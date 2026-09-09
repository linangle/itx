use tracing::*;

use crate::auth::{AuthError, SignedEnvelope, VerifyEnvelope, VerifyError};
use crate::board::{
    BoardError, CloseReason, ConsensusTaskIntent, Dispute, DisputableTaskIntent, DisputeResolution,
    EscrowConfirmation, EscrowPurpose, EscrowStatus, ExchangeAccount, Order, OrderStatus,
    PayoutAttempt, PayoutOutcome, PendingDeposit, Reputation, Side, Task, TaskBoard, TaskIntent,
    TaskKind, TaskStatus, Trade, MAX_PAYOUT_SUBMISSIONS,
};
use crate::rate_limit::QuotaExceeded;
use crate::AppState;
use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use btclib::crypto::{PrivateKey, PublicKey};
use btclib::sha256::Hash;
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use static_init::dynamic;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use uuid::Uuid;

/// How long a claim holds a task before it's automatically reopened for
/// someone else, if the claimant never submits.
const CLAIM_TTL_MINUTES: i64 = 30;
/// Flat fee attached to every hub-issued payment (faucet grants, task
/// payouts). Small and nonzero, matching how a real fee market works,
/// even though a private testnet has no real fee competition yet.
pub(crate) const HUB_TRANSACTION_FEE: u64 = 1_000;
/// Size of a faucet grant, in the same base units as block rewards
/// (INITIAL_REWARD is denominated in whole coins * 10^8).
pub const FAUCET_GRANT_AMOUNT: u64 = 50_000_000;
/// What to tell an agent whose grant could not be funded right now.
///
/// One block, because that is what the condition is: a payment's change
/// is unconfirmed until mined, and a block is when the operator's
/// wallet gets its slot back (`operator_wallet`). Sending the chain's
/// own target rather than a rounder number means this stays honest if
/// the target ever moves.
const FAUCET_RETRY_AFTER_SECONDS: u64 = btclib::IDEAL_BLOCK_TIME;
/// Upper bound on `join_window_minutes`/`submission_window_minutes`: not
/// just a sanity limit, but the difference between a clean 400 and an
/// actual panic -- `chrono::Duration::minutes` panics on overflow, and an
/// unbounded `i64` from a request body can get arbitrarily close to that.
/// A year is already far more generous than any real testnet task needs.
const MAX_CONSENSUS_WINDOW_MINUTES: i64 = 60 * 24 * 365;
/// Upper bound on `num_assignees`. A resolution persists every
/// assignee's reputation record together with the task, in a single redb
/// transaction (see `persist_consensus_submission` and the sweep's
/// equivalent), so this caps how large that one transaction -- and the
/// board lock hold building it -- can get.
const MAX_CONSENSUS_ASSIGNEES: u32 = 100;
/// `GET /tasks`'s page size when the caller doesn't specify `limit`.
const DEFAULT_TASKS_PAGE_SIZE: usize = 50;
/// Upper bound on `GET /tasks`'s `limit`, regardless of what the caller
/// asks for -- keeps one request's response (and the read-lock hold time
/// building it) bounded no matter how many open tasks exist.
const MAX_TASKS_PAGE_SIZE: usize = 200;
/// How long a reserved escrow deposit stays open for funding before the
/// sweep treats it as abandoned (and refunds whatever, if anything,
/// showed up late). An hour is generous slack for an on-chain payment to
/// actually confirm on this testnet.
const ESCROW_RESERVATION_TTL_MINUTES: i64 = 60;
/// Upper bound on how many capability tags one task may carry -- a sanity
/// limit, not a taxonomy (there is none -- see `validate_capabilities`).
const MAX_CAPABILITY_TAGS: usize = 20;
/// Upper bound on one capability tag's length in characters (after
/// normalization).
const MAX_CAPABILITY_TAG_LENGTH: usize = 64;
/// Minimum amount an exchange deposit must actually contain before
/// `confirm_exchange_deposit` will credit it. Unlike a task escrow,
/// there's no fixed target amount to reach here -- this is just a floor
/// above the network fee the eventual custody sweep will pay, so a dust
/// deposit can't be "confirmed" into a balance that's immediately
/// unsweepable.
const MIN_EXCHANGE_DEPOSIT: u64 = HUB_TRANSACTION_FEE + 1;
/// The mirror of `MIN_EXCHANGE_DEPOSIT`, and it exists for the mirror
/// reason. A withdrawal is paid out of pooled custody and custody pays
/// the network fee, so a withdrawal at or below that fee costs the pool
/// more than it moves -- the on-chain side shrinks while the sum of
/// ledger balances does not, which is the solvency pair drifting apart
/// one request at a time.
///
/// The deposit side has had this floor all along. The withdrawal side
/// had none at all, so `{"amount": 0}` was a spendable custody output
/// and a fee, burned, for nothing (`TaskBoard::debit_for_withdrawal`
/// has the rest of that story).
const MIN_EXCHANGE_WITHDRAWAL: u64 = HUB_TRANSACTION_FEE + 1;
/// `GET /exchange/trades`'s page size when the caller doesn't specify
/// `limit`, mirroring `DEFAULT_TASKS_PAGE_SIZE`.
const DEFAULT_TRADES_PAGE_SIZE: usize = 50;
/// Upper bound on `GET /exchange/trades`'s `limit`, mirroring
/// `MAX_TASKS_PAGE_SIZE`.
const MAX_TRADES_PAGE_SIZE: usize = 200;
/// Upper bound on any free-text field a caller supplies directly (a
/// task's `description`, a submitted `output`, a dispute `reason`).
/// These are otherwise-unbounded `String`s inside a request body already
/// capped by axum's default 2MB-per-request limit, but that limit alone
/// isn't enough here: a task's description/reason is persisted in the
/// store indefinitely (no TTL the way a claim or escrow reservation
/// has), so without a per-field cap one request could still write an
/// unreasonably large blob that sticks around forever and gets echoed
/// back on every future `GET /tasks`. Generous enough for real prose or
/// a real submitted answer, not a meaningful constraint on legitimate use.
const MAX_TEXT_FIELD_LENGTH: usize = 20_000;

// ---------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------

pub enum ApiError {
    BadRequest(String),
    Unauthorized(String),
    Forbidden(String),
    NotFound(String),
    Conflict(String),
    TooManyRequests(String),
    Internal(String),
    /// The request was well-formed and may well be authorized -- the
    /// server just cannot serve it *yet*. Distinct from `Internal`
    /// because it is neither an error on our side nor a permanent one:
    /// the honest thing to tell a caller is "come back", which is a 503.
    ServiceUnavailable(String),
    /// A `ServiceUnavailable` that says *when* to come back, and can
    /// hand the caller what it needs in order to.
    ///
    /// A separate variant rather than an option on the one above,
    /// because most of the hub's 503s have no honest number to give --
    /// `AuthError::GuardWarmingUp` resolves when it resolves -- and a
    /// `Retry-After` a caller cannot rely on is worse than none at all.
    Unavailable {
        message: String,
        /// Seconds. Sent twice on purpose: as the `Retry-After` header,
        /// which a generic HTTP client already honours, and in the
        /// body, which is what an agent parsing JSON will actually
        /// read.
        retry_after: u64,
        /// Extra top-level fields merged into the error body. The
        /// faucet uses it to hand back a fresh challenge -- see
        /// `faucet_claim`.
        extra: serde_json::Map<String, serde_json::Value>,
    },
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // Handled before the others because it is the one error with a
        // header and a body of its own; everything else is a status and
        // a string.
        if let ApiError::Unavailable { message, retry_after, extra } = self {
            let mut body = serde_json::Map::new();
            body.insert("error".into(), serde_json::Value::String(message));
            body.insert("retry_after_seconds".into(), serde_json::Value::from(retry_after));
            body.extend(extra);
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                [(axum::http::header::RETRY_AFTER, retry_after.to_string())],
                Json(serde_json::Value::Object(body)),
            )
                .into_response();
        }
        let (status, message) = match self {
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            ApiError::Unauthorized(m) => (StatusCode::UNAUTHORIZED, m),
            ApiError::Forbidden(m) => (StatusCode::FORBIDDEN, m),
            ApiError::NotFound(m) => (StatusCode::NOT_FOUND, m),
            ApiError::Conflict(m) => (StatusCode::CONFLICT, m),
            ApiError::TooManyRequests(m) => (StatusCode::TOO_MANY_REQUESTS, m),
            ApiError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
            ApiError::ServiceUnavailable(m) => (StatusCode::SERVICE_UNAVAILABLE, m),
            ApiError::Unavailable { .. } => unreachable!("returned above"),
        };
        (status, Json(serde_json::json!({ "error": message }))).into_response()
    }
}

impl From<AuthError> for ApiError {
    fn from(e: AuthError) -> Self {
        match e {
            AuthError::ClockDrift | AuthError::Replayed | AuthError::BadSignature => {
                ApiError::Unauthorized(e.to_string())
            }
            AuthError::BadPublicKey(_) | AuthError::BadSignatureEncoding(_) => {
                ApiError::BadRequest(e.to_string())
            }
            // Neither is a 401: the caller did nothing wrong. The first
            // is accepted once the window closes; the second is the hub
            // refusing to act on a request it could not record against
            // replay, which is a fault on our side to retry into.
            AuthError::GuardWarmingUp | AuthError::GuardUnavailable(_) => {
                ApiError::ServiceUnavailable(e.to_string())
            }
        }
    }
}

impl From<QuotaExceeded> for ApiError {
    fn from(e: QuotaExceeded) -> Self {
        ApiError::TooManyRequests(e.to_string())
    }
}

/// Both halves of `VerifyEnvelope::verify` already map to a status; this
/// just forwards to whichever one applies, so a handler can `?` the
/// single call that does authentication and metering together.
/// A redemption failure is the caller's, except when the hub could not
/// write the record -- which is the same 503 the replay guard returns
/// for the same reason, since both are the hub refusing to act on
/// something it cannot remember having done.
impl From<crate::faucet_pow::RedemptionError> for ApiError {
    fn from(e: crate::faucet_pow::RedemptionError) -> Self {
        use crate::faucet_pow::RedemptionError::*;
        match e {
            Unknown => ApiError::NotFound(e.to_string()),
            AlreadyRedeemed => ApiError::Conflict(e.to_string()),
            WrongKey => ApiError::Forbidden(e.to_string()),
            Expired | Unsolved => ApiError::BadRequest(e.to_string()),
            NotRecorded(_) => ApiError::ServiceUnavailable(e.to_string()),
        }
    }
}

impl From<VerifyError> for ApiError {
    fn from(e: VerifyError) -> Self {
        match e {
            VerifyError::Auth(e) => e.into(),
            VerifyError::Quota(e) => e.into(),
        }
    }
}

impl From<BoardError> for ApiError {
    fn from(e: BoardError) -> Self {
        match e {
            BoardError::NotFound | BoardError::EscrowNotFound | BoardError::OrderNotFound => {
                ApiError::NotFound(e.to_string())
            }
            BoardError::NotOpen
            | BoardError::NotClaimed
            | BoardError::NotVerified
            | BoardError::AlreadyClaimed
            | BoardError::AlreadyJoined
            | BoardError::AlreadySubmitted
            | BoardError::JoinWindowExpired
            | BoardError::SubmissionWindowExpired
            | BoardError::WrongTaskKind
            | BoardError::AlreadyTerminal
            | BoardError::EscrowNotReserved
            | BoardError::EscrowExpired
            | BoardError::EscrowUnderfunded { .. }
            | BoardError::WrongEscrowPurpose
            | BoardError::DisputeWindowClosed
            | BoardError::AlreadyDisputed
            | BoardError::NotDisputed
            | BoardError::CannotCancelWhileDisputed
            | BoardError::OrderNotOpen
            | BoardError::InsufficientBalance { .. } => ApiError::Conflict(e.to_string()),
            BoardError::NotClaimant
            | BoardError::InsufficientReputation { .. }
            | BoardError::PosterCannotClaimOwnTask
            | BoardError::AssigneeCannotDisputeOwnSubmission
            | BoardError::NotOrderOwner => ApiError::Forbidden(e.to_string()),
            BoardError::InvalidOrder
            | BoardError::OrderNotionalOverflow
            | BoardError::ZeroWithdrawal => ApiError::BadRequest(e.to_string()),
        }
    }
}

fn parse_hex_hash(hex_str: &str) -> Result<Hash, ApiError> {
    let bytes = hex::decode(hex_str)
        .map_err(|e| ApiError::BadRequest(format!("expected_output_hash isn't valid hex: {e}")))?;
    let array: [u8; 32] = bytes.try_into().map_err(|_| {
        ApiError::BadRequest("expected_output_hash must be exactly 32 bytes (64 hex chars)".into())
    })?;
    Ok(Hash::from_bytes(array))
}

// ---------------------------------------------------------------------
// Response DTOs -- deliberately hide `expected_output_hash` from public
// task listings (no reason to make the verification target any more
// discoverable than it needs to be) and represent every key as a plain
// hex string, never btclib's internal CBOR shape.
// ---------------------------------------------------------------------

/// A `Consensus` task's public view deliberately omits every assignee's
/// individual answer, even after resolution -- a late joiner (or anyone
/// re-fetching the task before it's full) must never be able to see what
/// someone else already answered, or the whole point of independent
/// redundant assignment is defeated.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskKindDto {
    HashMatch,
    Consensus {
        num_assignees: u32,
        assignees_joined: u32,
        /// How long the task has left to attract `num_assignees` joiners
        /// before it's cancelled for being under-subscribed. Only
        /// meaningful while `status` is still `Open`.
        join_deadline: DateTime<Utc>,
        /// `None` until the task actually fills up (transitions to
        /// `Claimed`) -- its submission window doesn't start counting
        /// down before then.
        submission_deadline: Option<DateTime<Utc>>,
    },
    Disputable {
        /// `None` until `submit_disputable_answer` sets it.
        answer: Option<String>,
        /// `None` until the answer's submitted -- the dispute window
        /// doesn't start counting down before then.
        dispute_deadline: Option<DateTime<Utc>>,
        dispute: Option<DisputeDto>,
    },
}

/// A `Disputable` task's filed dispute, if any -- unlike `Consensus`'s
/// hidden-until-resolved individual answers, there's no anti-collusion
/// reason to hide this (there's only ever one challenger, not several
/// simultaneous voters who could copy each other), so it's visible as
/// soon as it's filed.
#[derive(Serialize)]
pub struct DisputeDto {
    pub challenger: String,
    pub reason: String,
    pub bond_amount: u64,
    pub filed_at: DateTime<Utc>,
    pub resolution: Option<DisputeResolution>,
}

impl From<&Dispute> for DisputeDto {
    fn from(d: &Dispute) -> Self {
        DisputeDto {
            challenger: d.challenger.to_string(),
            reason: d.reason.clone(),
            bond_amount: d.bond_amount,
            filed_at: d.filed_at,
            resolution: d.resolution,
        }
    }
}

#[derive(Serialize)]
pub struct TaskDto {
    pub id: Uuid,
    pub description: String,
    pub bounty: u64,
    pub status: TaskStatus,
    pub poster: String,
    pub claimant: Option<String>,
    pub failed_attempts: u32,
    /// Minimum completed-task count required to claim/join this task; `0`
    /// means anyone may attempt it.
    pub min_reputation: u64,
    /// `Consensus`-only: why the task ended `Closed`, if it did.
    pub close_reason: Option<CloseReason>,
    /// Free-form capability tags, already normalized. Empty means
    /// unrestricted -- see `GET /tasks?capability=`.
    pub capabilities: BTreeSet<String>,
    /// When the task was posted. Already the ordering key `list_tasks`
    /// sorts by; exposed so clients can bucket tasks into a time series
    /// (the dashboard's sparklines derive entirely from this field)
    /// without the hub having to serve pre-aggregated stats.
    ///
    /// Strictly a *creation* time; `settled_at` below is the other end.
    /// Nothing records when a task was claimed or verified.
    pub created_at: DateTime<Utc>,
    /// When this task's last payout confirmed on chain -- when it
    /// reached `Paid`. `null` until then, and `null` forever for a task
    /// that closed or failed without paying anyone.
    ///
    /// The companion `created_at` used to say did not exist, and its
    /// absence was load-bearing: a client could chart when work was
    /// posted and had no honest way to chart when it was finished, so
    /// every chart on the dashboard was a chart of demand. Pairing the
    /// two is what makes "was any of this work actually done" a question
    /// the API can answer.
    ///
    /// `null` on a task settled before this field existed, which reads
    /// as "not recorded" rather than as the epoch.
    pub settled_at: Option<DateTime<Utc>>,
    /// How much of the bounty the hub has *seen on chain*, and how much
    /// it has not. Distinct from `bounty`, which is what the task is
    /// worth, and from `status`, which says which of three quite
    /// different reasons the unconfirmed part is unconfirmed:
    /// `Verified` (owed, nothing sent yet), `Submitted` (sent, no answer
    /// from the node yet) and `PayoutFailed` (proven never to have
    /// reached the chain, and abandoned -- still owed).
    ///
    /// They need not sum to `bounty`: a `Consensus` task pays only its
    /// winners, and a task that has not resolved has allocated nothing.
    ///
    /// Before this existed the API had only "will be paid" and "was
    /// paid" and no way to say "was sent and we are waiting" -- which
    /// meant it said "was paid" for payouts that never happened.
    pub bounty_confirmed: u64,
    pub bounty_pending: u64,
    #[serde(flatten)]
    pub kind: TaskKindDto,
}

impl From<&Task> for TaskDto {
    fn from(task: &Task) -> Self {
        let kind = match &task.kind {
            TaskKind::HashMatch { .. } => TaskKindDto::HashMatch,
            TaskKind::Consensus { num_assignees, join_deadline, submission_deadline, assignees, .. } => TaskKindDto::Consensus {
                num_assignees: *num_assignees,
                assignees_joined: assignees.len() as u32,
                join_deadline: *join_deadline,
                submission_deadline: *submission_deadline,
            },
            TaskKind::Disputable { answer, dispute_deadline, dispute, .. } => TaskKindDto::Disputable {
                answer: answer.clone(),
                dispute_deadline: *dispute_deadline,
                dispute: dispute.as_ref().map(DisputeDto::from),
            },
        };
        TaskDto {
            id: task.id,
            description: task.description.clone(),
            bounty: task.bounty,
            status: task.status,
            poster: task.poster.to_string(),
            claimant: task.claimant.as_ref().map(|k| k.to_string()),
            failed_attempts: task.failed_attempts,
            min_reputation: task.min_reputation,
            close_reason: task.close_reason,
            capabilities: task.capabilities.clone(),
            created_at: task.created_at,
            settled_at: task.settled_at,
            bounty_confirmed: task.confirmed_payout_total(),
            bounty_pending: task.unconfirmed_payout_total(),
            kind,
        }
    }
}

#[derive(Serialize)]
pub struct ReputationDto {
    pub completed: u64,
    pub failed: u64,
    pub total_earned: u64,
    /// Current confirmed on-chain balance -- distinct from
    /// `total_earned`, which is lifetime cumulative payout and never
    /// decreases even after the agent spends it. `None` until a caller
    /// explicitly fills it in via a node lookup (see `get_reputation`/
    /// `leaderboard`) -- board-level `Reputation` has no way to know this
    /// on its own, so `From<Reputation>` alone can never populate it.
    pub net_worth: Option<u64>,
    /// The agent's display name (see `crate::names`), e.g.
    /// `SwiftWarlock`. Filled in by the caller from `AppState`'s
    /// registry, for exactly the reason `net_worth` is -- board-level
    /// `Reputation` doesn't know it, so `From<Reputation>` can't
    /// populate it.
    ///
    /// `None` means "this pubkey has no name", not "names are off": a
    /// pubkey with no history on the board is never named, so any
    /// consumer must be able to fall back to rendering the pubkey.
    pub name: Option<String>,
}

impl From<Reputation> for ReputationDto {
    fn from(r: Reputation) -> Self {
        ReputationDto {
            completed: r.completed,
            failed: r.failed,
            total_earned: r.total_earned,
            net_worth: None,
            name: None,
        }
    }
}

#[derive(Serialize)]
pub struct LeaderboardEntryDto {
    pub pubkey: String,
    /// This agent's standing in the **whole** field, one-based, taken
    /// before any filtering or slicing.
    pub rank: usize,
    #[serde(flatten)]
    pub reputation: ReputationDto,
}

#[derive(Serialize)]
pub struct FaucetResultDto {
    pub amount: u64,
    pub payment_id: Uuid,
    pub status: crate::payments::Status,
}

/// An issued proof-of-work challenge, as the client sees it.
///
/// Everything needed to build the preimage is here as text, in the order
/// it appears in the preimage, because a client that has to consult
/// prose to work out the field order will get it wrong. `target` is a
/// plain big-endian hex integer; compare it against the SHA-256 digest
/// read **little-endian** (see `faucet_pow`'s module docs, which is also
/// where the one-line Python version lives).
#[derive(Serialize)]
pub struct FaucetChallengeDto {
    pub challenge_id: Uuid,
    pub server_nonce: String,
    pub pubkey: String,
    pub action: String,
    pub target: String,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// What the target is worth in tries, so an agent can decide whether
    /// to bother without doing modular arithmetic on a 256-bit number.
    pub expected_hashes: u64,
    /// The exact string to hash, with the solution left as `{solution}`.
    /// Redundant with the fields above and worth the bytes: it is the
    /// one part clients get wrong, and a template removes the guesswork
    /// about separators and field order entirely.
    pub preimage_template: String,
}

/// The payload `POST /faucet` now carries.
///
/// This is a breaking change to a published route, made deliberately and
/// before the SDKs are on PyPI rather than after (plan §5, §7.3). An
/// unsolved faucet was the one place the hub handed value to a key that
/// had proved nothing.
#[derive(Serialize, Deserialize)]
pub struct FaucetClaimPayload {
    pub challenge_id: Uuid,
    pub solution: u64,
}

#[derive(Serialize)]
pub struct SubmitResultDto {
    /// `HashMatch`: whether the output matched. `Consensus`: whether
    /// *this* agent's answer matched the majority (only meaningful once
    /// `resolved` is `Some(true)`).
    pub verified: bool,
    /// Whether the payout transaction was *submitted* to the node --
    /// which is all this ever meant, and all a synchronous response can
    /// honestly claim: confirmation takes at least one sweep. The task's
    /// own `status` and `bounty_confirmed`/`bounty_pending` are where an
    /// agent finds out whether it actually landed (see `TaskDto`).
    ///
    /// Kept under this name, with this behaviour, so existing clients
    /// are unaffected: it never was a confirmation, the difference just
    /// had nowhere to be expressed.
    pub paid: bool,
    pub bounty: Option<u64>,
    /// `None` for `HashMatch` (submission and resolution are always the
    /// same event there). For `Consensus`: `Some(false)` if this
    /// submission is still waiting on other assignees, `Some(true)` once
    /// every assignee has submitted (or the deadline forced it) and the
    /// task has resolved.
    pub resolved: Option<bool>,
}

// ---------------------------------------------------------------------
// Request payloads (the `T` in `SignedEnvelope<T>`)
// ---------------------------------------------------------------------

#[derive(Deserialize, Serialize)]
pub struct CreateTaskPayload {
    pub description: String,
    pub bounty: u64,
    /// Hex-encoded SHA256 of the expected correct output. This is Phase
    /// B's one verification tier: objectively checkable compute/data
    /// jobs, not open-ended ones.
    pub expected_output_hash: String,
    /// Minimum completed-task count required to claim this task. Omit
    /// (or send `0`) for no gate at all.
    #[serde(default)]
    pub min_reputation: u64,
    /// Free-form capability tags (e.g. `"python"`, `"translation"`).
    /// Omit (or send an empty set) for unrestricted -- see
    /// `validate_capabilities` for normalization/limits.
    #[serde(default)]
    pub capabilities: BTreeSet<String>,
}

#[derive(Deserialize, Serialize)]
pub struct CreateConsensusTaskPayload {
    pub description: String,
    pub bounty: u64,
    /// How many independent agents must be assigned before the task
    /// closes to new joiners and awaits submissions. Must be at least 2 --
    /// with only one assignee, "majority" is a meaningless concept.
    pub num_assignees: u32,
    /// How long the task waits for `num_assignees` joiners before it's
    /// cancelled (refunding its escrow) for being under-subscribed.
    pub join_window_minutes: i64,
    /// How long, from the moment the task fills up, assignees have to
    /// submit their answer before a no-show counts against them.
    pub submission_window_minutes: i64,
    /// Same meaning as `CreateTaskPayload::min_reputation`.
    #[serde(default)]
    pub min_reputation: u64,
    /// Same meaning as `CreateTaskPayload::capabilities`.
    #[serde(default)]
    pub capabilities: BTreeSet<String>,
}

#[derive(Deserialize, Serialize)]
pub struct ClaimPayload {
    pub task_id: Uuid,
}

#[derive(Deserialize, Serialize)]
pub struct SubmitPayload {
    pub task_id: Uuid,
    pub output: String,
}

/// Query params for `GET /tasks`. Both optional -- see
/// `DEFAULT_TASKS_PAGE_SIZE`/`MAX_TASKS_PAGE_SIZE` for what an absent
/// `limit` defaults to and what any `limit` is capped at.
#[derive(Deserialize)]
pub struct ListTasksQuery {
    #[serde(default)]
    pub offset: usize,
    pub limit: Option<usize>,
    /// Single-tag exact match against a task's normalized capability
    /// tags. Absent means unfiltered. Normalized (trim + lowercase) the
    /// same way stored tags already are, so casing doesn't matter here
    /// either. Multi-tag AND/OR filtering isn't built -- nothing needs it
    /// yet, and it'd be a pure handler-side change if that changes.
    pub capability: Option<String>,
    /// Which task statuses to list. **Absent means `Open` only** -- the
    /// long-standing behaviour of this endpoint, deliberately unchanged
    /// so existing agents and SDK callers that treat "listed" as
    /// "claimable" keep working exactly as before.
    ///
    /// Accepts any single `TaskStatus` name (`Open`, `Claimed`,
    /// `AwaitingDispute`, `Disputed`, `Verified`, `Submitted`, `Paid`,
    /// `PayoutFailed`, `Closed`) or the literal `all` for every status
    /// regardless. Matched
    /// case-insensitively, the same forgiving treatment `capability`
    /// already gets, so `?status=paid` and `?status=Paid` are the same
    /// query.
    ///
    /// This exists because a public marketplace has to be able to show
    /// *completed* work -- an economy that only ever displays unclaimed
    /// tasks looks dead no matter how much has actually settled.
    pub status: Option<String>,
}

/// What a `?status=` query resolves to.
enum StatusFilter {
    /// Every task, whatever its status (`?status=all`).
    Any,
    /// Exactly one status -- including `Open`, which reproduces the
    /// endpoint's default behaviour.
    Only(TaskStatus),
}

/// Parses `?status=` case-insensitively. Kept as a hand-written match
/// rather than a serde derive on `TaskStatus` for two reasons: `all`
/// isn't a `TaskStatus` at all, and an unrecognized value should produce
/// a legible 400 naming the valid options rather than serde's opaque
/// query-deserialization failure.
fn parse_status_filter(raw: &str) -> Result<StatusFilter, ApiError> {
    let normalized = raw.trim().to_lowercase();
    Ok(match normalized.as_str() {
        "all" => StatusFilter::Any,
        "open" => StatusFilter::Only(TaskStatus::Open),
        "claimed" => StatusFilter::Only(TaskStatus::Claimed),
        "awaitingdispute" => StatusFilter::Only(TaskStatus::AwaitingDispute),
        "disputed" => StatusFilter::Only(TaskStatus::Disputed),
        "verified" => StatusFilter::Only(TaskStatus::Verified),
        "submitted" => StatusFilter::Only(TaskStatus::Submitted),
        "paid" => StatusFilter::Only(TaskStatus::Paid),
        "payoutfailed" => StatusFilter::Only(TaskStatus::PayoutFailed),
        "closed" => StatusFilter::Only(TaskStatus::Closed),
        other => {
            return Err(ApiError::BadRequest(format!(
                "unknown status {other:?} -- expected one of: all, Open, Claimed, \
                 AwaitingDispute, Disputed, Verified, Submitted, Paid, PayoutFailed, Closed"
            )))
        }
    })
}

#[derive(Deserialize, Serialize)]
pub struct CancelPayload {
    pub task_id: Uuid,
}

/// Same shape as `CreateTaskPayload`, minus the operator restriction --
/// this funds itself via an escrow deposit (see `create_task_escrow`)
/// instead of the operator's wallet.
#[derive(Deserialize, Serialize)]
pub struct EscrowTaskPayload {
    pub description: String,
    pub bounty: u64,
    pub expected_output_hash: String,
    #[serde(default)]
    pub min_reputation: u64,
    #[serde(default)]
    pub capabilities: BTreeSet<String>,
}

/// Same shape as `CreateConsensusTaskPayload`, minus the operator
/// restriction.
#[derive(Deserialize, Serialize)]
pub struct EscrowConsensusTaskPayload {
    pub description: String,
    pub bounty: u64,
    pub num_assignees: u32,
    pub join_window_minutes: i64,
    pub submission_window_minutes: i64,
    #[serde(default)]
    pub min_reputation: u64,
    #[serde(default)]
    pub capabilities: BTreeSet<String>,
}

#[derive(Deserialize, Serialize)]
pub struct ConfirmEscrowPayload {
    pub escrow_id: Uuid,
}

/// Same shape as `CreateTaskPayload`/`EscrowTaskPayload`, but for a
/// `Disputable` task -- funds itself via escrow, same as
/// `EscrowTaskPayload`; there's no operator-funded equivalent (an
/// open-ended, dispute-resolved task is squarely the agent-to-agent case
/// this whole escrow mechanism exists for).
#[derive(Deserialize, Serialize)]
pub struct EscrowDisputableTaskPayload {
    pub description: String,
    pub bounty: u64,
    pub dispute_window_minutes: i64,
    #[serde(default)]
    pub min_reputation: u64,
    #[serde(default)]
    pub capabilities: BTreeSet<String>,
}

/// Reserves a bond escrow for disputing `task_id`'s submitted answer.
#[derive(Deserialize, Serialize)]
pub struct DisputeEscrowPayload {
    pub task_id: Uuid,
    pub reason: String,
}

#[derive(Deserialize, Serialize)]
pub struct ConfirmDisputeEscrowPayload {
    pub task_id: Uuid,
    pub escrow_id: Uuid,
}

#[derive(Deserialize, Serialize)]
pub struct ResolveDisputePayload {
    pub task_id: Uuid,
    pub outcome: DisputeResolution,
}

/// What reserving an escrow returns: the address to pay, how much, and
/// how long the reservation stays open. Deliberately never includes the
/// private key -- only the hub itself ever needs it.
#[derive(Serialize)]
pub struct EscrowReservationDto {
    pub escrow_id: Uuid,
    pub deposit_address: String,
    pub required_amount: u64,
    pub expires_at: DateTime<Utc>,
}

impl From<&PendingDeposit> for EscrowReservationDto {
    fn from(deposit: &PendingDeposit) -> Self {
        EscrowReservationDto {
            escrow_id: deposit.id,
            deposit_address: deposit.deposit_pubkey.to_string(),
            required_amount: deposit.required_amount,
            expires_at: deposit.expires_at,
        }
    }
}

// ---- Exchange v1 ----

#[derive(Deserialize, Serialize)]
pub struct ConfirmExchangeDepositPayload {
    pub escrow_id: Uuid,
}

#[derive(Deserialize, Serialize)]
pub struct PlaceOrderPayload {
    pub side: Side,
    pub price: u64,
    pub quantity: u64,
}

#[derive(Deserialize, Serialize)]
pub struct CancelOrderPayload {
    pub order_id: Uuid,
}

#[derive(Deserialize, Serialize)]
pub struct WithdrawPayload {
    pub amount: u64,
}

/// Query params for `GET /exchange/trades`, same shape as `ListTasksQuery`.
#[derive(Deserialize)]
pub struct ListTradesQuery {
    #[serde(default)]
    pub offset: usize,
    pub limit: Option<usize>,
}

#[derive(Serialize)]
pub struct ExchangeAccountDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payment: Option<crate::payments::Receipt>,
    pub base_balance: u64,
    pub locked_base: u64,
    pub compute_balance: u64,
    pub locked_compute: u64,
}

impl From<ExchangeAccount> for ExchangeAccountDto {
    fn from(a: ExchangeAccount) -> Self {
        ExchangeAccountDto {
            payment: None,
            base_balance: a.base_balance,
            locked_base: a.locked_base,
            compute_balance: a.compute_balance,
            locked_compute: a.locked_compute,
        }
    }
}

#[derive(Serialize)]
pub struct OrderDto {
    pub id: Uuid,
    pub owner: String,
    pub side: Side,
    pub price: u64,
    pub quantity: u64,
    pub filled: u64,
    pub status: OrderStatus,
    pub created_at: DateTime<Utc>,
}

impl From<&Order> for OrderDto {
    fn from(o: &Order) -> Self {
        OrderDto {
            id: o.id,
            owner: o.owner.to_string(),
            side: o.side,
            price: o.price,
            quantity: o.quantity,
            filled: o.filled,
            status: o.status,
            created_at: o.created_at,
        }
    }
}

#[derive(Serialize)]
pub struct OrderBookDto {
    pub bids: Vec<OrderDto>,
    pub asks: Vec<OrderDto>,
}

/// A single executed match, including both counterparties' hex pubkeys
/// -- unlike a `Consensus` task's hidden-until-resolved answers, there's
/// no privacy reason to hide who traded with whom; `/exchange/trades` is
/// meant to be a real, public pricing feed.
#[derive(Serialize)]
pub struct TradeDto {
    pub id: Uuid,
    pub buy_order_id: Uuid,
    pub sell_order_id: Uuid,
    pub buyer: String,
    pub seller: String,
    pub price: u64,
    pub quantity: u64,
    pub executed_at: DateTime<Utc>,
    /// Which side placed the order that caused this match -- needed to
    /// know what `taker_fee` is denominated in (see `Trade`'s own doc
    /// comment): compute if `buy`, the base currency if `sell`.
    pub taker_side: Side,
    /// Already deducted from what the taker received; the maker side of
    /// every trade is always paid in full.
    pub taker_fee: u64,
}

impl From<&Trade> for TradeDto {
    fn from(t: &Trade) -> Self {
        TradeDto {
            id: t.id,
            buy_order_id: t.buy_order_id,
            sell_order_id: t.sell_order_id,
            buyer: t.buyer.to_string(),
            seller: t.seller.to_string(),
            price: t.price,
            quantity: t.quantity,
            executed_at: t.executed_at,
            taker_side: t.taker_side,
            taker_fee: t.taker_fee,
        }
    }
}

// ---------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------

/// Public, unauthenticated liveness/readiness check for external
/// monitoring (uptime checks, systemd, a load balancer) -- proves the hub
/// can actually reach a node, not just that its own process is up.
/// Deliberately doesn't report which node address answered or the
/// configured node list itself: that's internal topology with no reason
/// to be visible to anyone on the internet who happens to hit this route.
/// That detail goes to the server's own logs instead, via
/// `NodeClient::connect`'s `warn!` on each failed address.
#[derive(Serialize)]
pub struct HealthDto {
    pub status: &'static str,
    pub chain_height: u32,
}

pub async fn health(State(state): State<Arc<AppState>>) -> Response {
    match state.node.chain_tip().await {
        Ok(chain_height) => Json(HealthDto { status: "ok", chain_height }).into_response(),
        Err(e) => {
            warn!("health check failed: no configured node is reachable: {e}");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "status": "degraded", "error": "no configured node is reachable" })),
            )
                .into_response()
        }
    }
}

/// Lists open tasks oldest-first (by `created_at`), paginated via
/// `?offset=&limit=` -- see `ListTasksQuery`. Ordering by creation time
/// (rather than `TaskBoard`'s internal by-id order) is what makes
/// pagination actually meaningful: a stable "page 2" means the same thing
/// across calls, and older tasks can't be pushed off the end by newer
/// ones arriving.
pub async fn list_tasks(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ListTasksQuery>,
) -> Result<Response, ApiError> {
    let limit = query.limit.unwrap_or(DEFAULT_TASKS_PAGE_SIZE).min(MAX_TASKS_PAGE_SIZE);
    // Normalized the same way stored tags already are (see
    // validate_capabilities) -- a task with no capability tags at all
    // always matches an unfiltered query (no filter -> no exclusion), but
    // never matches a *specific* capability filter (nothing to match).
    let capability_filter = query.capability.map(|c| c.trim().to_lowercase());
    let status_filter = query.status.as_deref().map(parse_status_filter).transpose()?;
    let board = state.board.read().await;
    // No `?status=` keeps the original code path verbatim, down to the
    // same `list_open_tasks` call -- the default response is byte-for-byte
    // what it has always been, apart from each task's new `created_at`.
    let mut tasks: Vec<&Task> = match status_filter {
        None | Some(StatusFilter::Only(TaskStatus::Open)) => board.list_open_tasks(),
        Some(StatusFilter::Any) => board.all_tasks().collect(),
        Some(StatusFilter::Only(status)) => board.all_tasks().filter(|t| t.status == status).collect(),
    };
    tasks.sort_by_key(|t| t.created_at);

    let matched: Vec<&Task> = tasks
        .into_iter()
        .filter(|t| match &capability_filter {
            Some(tag) => t.capabilities.contains(tag),
            None => true,
        })
        .collect();
    // Counted after filtering but before paging -- that's what makes it
    // useful for "showing 50 of 312" and for sizing a pager.
    let total = matched.len();

    let page: Vec<TaskDto> = matched.into_iter().skip(query.offset).take(limit).map(TaskDto::from).collect();

    // The total rides in a header rather than wrapping the body in an
    // object, because changing the response shape from `[...]` to
    // `{ tasks: [...], total: n }` would break every existing consumer at
    // once -- the dashboard, both SDKs, and any running agent. A header
    // is additive: clients that don't look for it never notice.
    // Cross-origin readers also need it named in `Access-Control-Expose-
    // Headers`, which `build_router`'s CORS layer does.
    Ok(([("x-total-count", total.to_string())], Json(page)).into_response())
}

pub async fn get_task(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<Uuid>,
) -> Result<Json<TaskDto>, ApiError> {
    let board = state.board.read().await;
    let task = board
        .get_task(task_id)
        .ok_or_else(|| ApiError::NotFound("task not found".into()))?;
    Ok(Json(TaskDto::from(task)))
}

pub async fn create_task(
    State(state): State<Arc<AppState>>,
    // The request as it actually arrived: bound into the signature,
    // so this envelope cannot be replayed at a different endpoint.
    method: Method,
    OriginalUri(uri): OriginalUri,
    Json(envelope): Json<SignedEnvelope<CreateTaskPayload>>,
) -> Result<Json<TaskDto>, ApiError> {
    let pubkey = envelope.verify(&state, method.as_str(), uri.path())?;
    require_operator(&pubkey, &state)?;
    validate_text_field(&envelope.payload.description, "description")?;
    let expected_output_hash = parse_hex_hash(&envelope.payload.expected_output_hash)?;
    let bounty = envelope.payload.bounty;
    let capabilities = validate_capabilities(&envelope.payload.capabilities)?;

    // Held across the balance check below on purpose: this is what makes
    // two concurrent task-creation requests safe (the second one's
    // balance check correctly sees the first one's allocation), at the
    // cost of serializing task creation against the node round-trip --
    // an acceptable tradeoff at this scale.
    let mut board = state.board.write().await;
    ensure_operator_can_fund(&state, &board, bounty).await?;
    let mut task = board.create_task(pubkey, envelope.payload.description.clone(), bounty, expected_output_hash);
    apply_min_reputation(&mut board, &mut task, envelope.payload.min_reputation);
    apply_capabilities(&mut board, &mut task, capabilities);
    drop(board);

    state.store.save_task(&task).map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(TaskDto::from(&task)))
}

pub async fn create_consensus_task(
    State(state): State<Arc<AppState>>,
    // The request as it actually arrived: bound into the signature,
    // so this envelope cannot be replayed at a different endpoint.
    method: Method,
    OriginalUri(uri): OriginalUri,
    Json(envelope): Json<SignedEnvelope<CreateConsensusTaskPayload>>,
) -> Result<Json<TaskDto>, ApiError> {
    let pubkey = envelope.verify(&state, method.as_str(), uri.path())?;
    require_operator(&pubkey, &state)?;
    validate_text_field(&envelope.payload.description, "description")?;
    if envelope.payload.num_assignees < 2 {
        return Err(ApiError::BadRequest(
            "a consensus task needs at least 2 assignees for majority agreement to mean anything".into(),
        ));
    }
    if envelope.payload.num_assignees > MAX_CONSENSUS_ASSIGNEES {
        return Err(ApiError::BadRequest(format!(
            "num_assignees must be at most {MAX_CONSENSUS_ASSIGNEES}"
        )));
    }
    validate_positive_minutes(envelope.payload.join_window_minutes, "join_window_minutes")?;
    validate_positive_minutes(envelope.payload.submission_window_minutes, "submission_window_minutes")?;
    let bounty = envelope.payload.bounty;
    ensure_consensus_exposure_allows(&state, bounty).await?;
    let capabilities = validate_capabilities(&envelope.payload.capabilities)?;

    let mut board = state.board.write().await;
    ensure_operator_can_fund(&state, &board, bounty).await?;
    // join_deadline is computed only now, after the (possibly slow) node
    // balance round-trip above -- so a sluggish node can't silently eat
    // into the window this task advertises. submission_deadline is NOT
    // computed here at all: it's set once the task actually fills up
    // (join_consensus_task), not from creation time -- see
    // TaskKind::Consensus::submission_window_minutes for why that
    // distinction matters.
    let join_deadline = Utc::now() + Duration::minutes(envelope.payload.join_window_minutes);
    let mut task = board.create_consensus_task(
        pubkey,
        envelope.payload.description.clone(),
        bounty,
        envelope.payload.num_assignees,
        join_deadline,
        envelope.payload.submission_window_minutes,
    );
    apply_min_reputation(&mut board, &mut task, envelope.payload.min_reputation);
    apply_capabilities(&mut board, &mut task, capabilities);
    drop(board);

    state.store.save_task(&task).map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(TaskDto::from(&task)))
}

/// Reserves a fresh escrow deposit for a `HashMatch` task funded by the
/// caller's own wallet rather than the operator's -- any signed pubkey
/// may call this, unlike `create_task`. See `TaskBoard::reserve_escrow`'s
/// doc comment for the durability requirement this handler must honor:
/// the deposit is persisted *before* its address is ever returned here.
pub async fn create_task_escrow(
    State(state): State<Arc<AppState>>,
    // The request as it actually arrived: bound into the signature,
    // so this envelope cannot be replayed at a different endpoint.
    method: Method,
    OriginalUri(uri): OriginalUri,
    Json(envelope): Json<SignedEnvelope<EscrowTaskPayload>>,
) -> Result<Json<EscrowReservationDto>, ApiError> {
    let pubkey = envelope.verify(&state, method.as_str(), uri.path())?;
    validate_text_field(&envelope.payload.description, "description")?;
    let expected_output_hash = parse_hex_hash(&envelope.payload.expected_output_hash)?;
    let bounty = envelope.payload.bounty;
    let required_amount = escrow_amount_for(bounty)?;
    let capabilities = validate_capabilities(&envelope.payload.capabilities)?;

    let intent = TaskIntent {
        description: envelope.payload.description.clone(),
        bounty,
        expected_output_hash,
        min_reputation: envelope.payload.min_reputation,
        capabilities,
    };
    let expires_at = Utc::now() + Duration::minutes(ESCROW_RESERVATION_TTL_MINUTES);
    let deposit = state.board.write().await.reserve_escrow(
        &state.escrow_secret,
        pubkey,
        required_amount,
        EscrowPurpose::FundHashMatchTask(intent),
        expires_at,
    );
    state.store.save_pending_deposit(&deposit).map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(EscrowReservationDto::from(&deposit)))
}

/// Same as `create_task_escrow`, for a `Consensus` task instead.
pub async fn create_consensus_task_escrow(
    State(state): State<Arc<AppState>>,
    // The request as it actually arrived: bound into the signature,
    // so this envelope cannot be replayed at a different endpoint.
    method: Method,
    OriginalUri(uri): OriginalUri,
    Json(envelope): Json<SignedEnvelope<EscrowConsensusTaskPayload>>,
) -> Result<Json<EscrowReservationDto>, ApiError> {
    let pubkey = envelope.verify(&state, method.as_str(), uri.path())?;
    validate_text_field(&envelope.payload.description, "description")?;
    if envelope.payload.num_assignees < 2 {
        return Err(ApiError::BadRequest(
            "a consensus task needs at least 2 assignees for majority agreement to mean anything".into(),
        ));
    }
    if envelope.payload.num_assignees > MAX_CONSENSUS_ASSIGNEES {
        return Err(ApiError::BadRequest(format!(
            "num_assignees must be at most {MAX_CONSENSUS_ASSIGNEES}"
        )));
    }
    validate_positive_minutes(envelope.payload.join_window_minutes, "join_window_minutes")?;
    validate_positive_minutes(envelope.payload.submission_window_minutes, "submission_window_minutes")?;
    let bounty = envelope.payload.bounty;
    ensure_consensus_exposure_allows(&state, bounty).await?;
    let required_amount = escrow_amount_for(bounty)?;
    let capabilities = validate_capabilities(&envelope.payload.capabilities)?;

    let intent = ConsensusTaskIntent {
        description: envelope.payload.description.clone(),
        bounty,
        num_assignees: envelope.payload.num_assignees,
        join_window_minutes: envelope.payload.join_window_minutes,
        submission_window_minutes: envelope.payload.submission_window_minutes,
        min_reputation: envelope.payload.min_reputation,
        capabilities,
    };
    let expires_at = Utc::now() + Duration::minutes(ESCROW_RESERVATION_TTL_MINUTES);
    let deposit = state.board.write().await.reserve_escrow(
        &state.escrow_secret,
        pubkey,
        required_amount,
        EscrowPurpose::FundConsensusTask(intent),
        expires_at,
    );
    state.store.save_pending_deposit(&deposit).map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(EscrowReservationDto::from(&deposit)))
}

/// Same as `create_task_escrow`, for a `Disputable` task instead -- see
/// `TaskKind::Disputable`. No operator-funded equivalent exists (unlike
/// `HashMatch`/`Consensus`): open-ended, dispute-resolved work is
/// squarely the agent-to-agent case escrow exists for.
pub async fn create_disputable_task_escrow(
    State(state): State<Arc<AppState>>,
    // The request as it actually arrived: bound into the signature,
    // so this envelope cannot be replayed at a different endpoint.
    method: Method,
    OriginalUri(uri): OriginalUri,
    Json(envelope): Json<SignedEnvelope<EscrowDisputableTaskPayload>>,
) -> Result<Json<EscrowReservationDto>, ApiError> {
    let pubkey = envelope.verify(&state, method.as_str(), uri.path())?;
    validate_text_field(&envelope.payload.description, "description")?;
    validate_positive_minutes(envelope.payload.dispute_window_minutes, "dispute_window_minutes")?;
    let bounty = envelope.payload.bounty;
    let required_amount = escrow_amount_for(bounty)?;
    let capabilities = validate_capabilities(&envelope.payload.capabilities)?;

    let intent = DisputableTaskIntent {
        description: envelope.payload.description.clone(),
        bounty,
        dispute_window_minutes: envelope.payload.dispute_window_minutes,
        min_reputation: envelope.payload.min_reputation,
        capabilities,
    };
    let expires_at = Utc::now() + Duration::minutes(ESCROW_RESERVATION_TTL_MINUTES);
    let deposit = state.board.write().await.reserve_escrow(
        &state.escrow_secret,
        pubkey,
        required_amount,
        EscrowPurpose::FundDisputableTask(intent),
        expires_at,
    );
    state.store.save_pending_deposit(&deposit).map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(EscrowReservationDto::from(&deposit)))
}

/// Checks whether `escrow_id`'s deposit address now holds at least its
/// required amount and, if so, materializes the real task it was
/// reserved for. Only the original depositor may confirm their own
/// escrow. `observed_amount` comes from a live `FetchUTXOs` balance
/// check -- confirmed, mined state only (see `NodeClient::balance`), so
/// a payment that's merely in the mempool won't be seen as funded yet.
pub async fn confirm_task_escrow(
    State(state): State<Arc<AppState>>,
    Path(escrow_id): Path<Uuid>,
    // The request as it actually arrived: bound into the signature,
    // so this envelope cannot be replayed at a different endpoint.
    method: Method,
    OriginalUri(uri): OriginalUri,
    Json(envelope): Json<SignedEnvelope<ConfirmEscrowPayload>>,
) -> Result<Json<TaskDto>, ApiError> {
    if envelope.payload.escrow_id != escrow_id {
        return Err(ApiError::BadRequest(
            "escrow id in the URL doesn't match the signed payload".into(),
        ));
    }
    let pubkey = envelope.verify(&state, method.as_str(), uri.path())?;

    let deposit_pubkey = {
        let board = state.board.read().await;
        let deposit = board.get_pending_deposit(escrow_id).ok_or(BoardError::EscrowNotFound)?;
        if deposit.depositor != pubkey {
            return Err(ApiError::Forbidden("you are not the depositor of this escrow".into()));
        }
        deposit.deposit_pubkey.clone()
    };
    let observed_amount = state
        .node
        .balance(&deposit_pubkey)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let (task, deposit) = {
        let mut board = state.board.write().await;
        let EscrowConfirmation::TaskCreated(task) =
            board.confirm_escrow(escrow_id, observed_amount, Utc::now())?;
        // Read the deposit back under the same write lock that just
        // consumed it, so what gets persisted below is this
        // confirmation's own Consumed record rather than whatever state
        // a concurrent caller might have left between the two locks.
        let deposit = board
            .get_pending_deposit(escrow_id)
            .expect("confirm_escrow consumed this deposit, and no path removes one")
            .clone();
        (task, deposit)
    };
    // One transaction, not two. The task and the deposit's Consumed
    // status are the same fact, and committing them separately was a
    // money bug: a hub killed between the two commits came back with a
    // task on disk beside a deposit still reading Reserved, and the
    // depositor could confirm the same escrow again for a second task
    // funded by one payment (plan §6.5b, and see
    // `HubStore::save_task_and_deposit` for why one transaction rather
    // than merely reordering the two).
    //
    // Failing here now leaves neither record on disk. In-memory the
    // board has already moved on, but nothing durable has, so a restart
    // reverts to a Reserved deposit and the depositor's retry is a clean
    // recovery -- the same outcome as a crash a moment earlier.
    state
        .store
        .save_task_and_deposit(&task, &deposit)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(TaskDto::from(&task)))
}

/// Reserves a bond escrow for challenging `task_id`'s submitted answer.
/// Any signed pubkey may call this except the task's own assignee (the
/// one being disputed) -- checked here, at the point of intent, mirroring
/// how `claim_task`/`join_consensus_task` check `PosterCannotClaimOwnTask`
/// at theirs. This is a fast-fail UX nicety, not the real safety
/// boundary -- `TaskBoard::confirm_dispute_bond` re-checks task state at
/// confirm time regardless, since this check and that confirmation are
/// separated by however long the on-chain payment takes.
pub async fn create_dispute_escrow(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<Uuid>,
    // The request as it actually arrived: bound into the signature,
    // so this envelope cannot be replayed at a different endpoint.
    method: Method,
    OriginalUri(uri): OriginalUri,
    Json(envelope): Json<SignedEnvelope<DisputeEscrowPayload>>,
) -> Result<Json<EscrowReservationDto>, ApiError> {
    if envelope.payload.task_id != task_id {
        return Err(ApiError::BadRequest(
            "task id in the URL doesn't match the signed payload".into(),
        ));
    }
    let pubkey = envelope.verify(&state, method.as_str(), uri.path())?;
    validate_text_field(&envelope.payload.reason, "reason")?;

    let bounty = {
        let board = state.board.read().await;
        let task = board
            .get_task(task_id)
            .ok_or_else(|| ApiError::NotFound("task not found".into()))?;
        if !matches!(task.kind, TaskKind::Disputable { .. }) {
            return Err(BoardError::WrongTaskKind.into());
        }
        if task.status != TaskStatus::AwaitingDispute {
            return Err(BoardError::DisputeWindowClosed.into());
        }
        if task.claimant.as_ref() == Some(&pubkey) {
            return Err(BoardError::AssigneeCannotDisputeOwnSubmission.into());
        }
        task.bounty
    };
    let required_amount = escrow_amount_for(bounty)?;
    let expires_at = Utc::now() + Duration::minutes(ESCROW_RESERVATION_TTL_MINUTES);
    let deposit = state.board.write().await.reserve_escrow(
        &state.escrow_secret,
        pubkey,
        required_amount,
        EscrowPurpose::DisputeBond { task_id, reason: envelope.payload.reason.clone() },
        expires_at,
    );
    state.store.save_pending_deposit(&deposit).map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(EscrowReservationDto::from(&deposit)))
}

/// Checks whether a dispute bond is now funded and, if the target task is
/// still actually awaiting a dispute, attaches it (`AwaitingDispute` ->
/// `Disputed`). If the window closed while the payment was in flight, the
/// deposit is refunded on the spot rather than left to a later sweep --
/// `TaskBoard::confirm_dispute_bond` deliberately didn't consume it in
/// that case for exactly this reason.
pub async fn confirm_dispute_escrow(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<Uuid>,
    // The request as it actually arrived: bound into the signature,
    // so this envelope cannot be replayed at a different endpoint.
    method: Method,
    OriginalUri(uri): OriginalUri,
    Json(envelope): Json<SignedEnvelope<ConfirmDisputeEscrowPayload>>,
) -> Result<Json<TaskDto>, ApiError> {
    if envelope.payload.task_id != task_id {
        return Err(ApiError::BadRequest(
            "task id in the URL doesn't match the signed payload".into(),
        ));
    }
    let pubkey = envelope.verify(&state, method.as_str(), uri.path())?;
    let escrow_id = envelope.payload.escrow_id;

    let deposit_pubkey = {
        let board = state.board.read().await;
        let deposit = board.get_pending_deposit(escrow_id).ok_or(BoardError::EscrowNotFound)?;
        if deposit.depositor != pubkey {
            return Err(ApiError::Forbidden("you are not the depositor of this escrow".into()));
        }
        deposit.deposit_pubkey.clone()
    };
    let observed_amount = state
        .node
        .balance(&deposit_pubkey)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let confirm_result = {
        let mut board = state.board.write().await;
        let outcome = board.confirm_dispute_bond(escrow_id, observed_amount, Utc::now());
        // Carry the deposit out beside the task, read under the same
        // write lock that consumed it -- exactly as `confirm_task_escrow`
        // does, and for the same reason. Only the success arm consumes a
        // deposit; the closed-window arm deliberately leaves it Reserved
        // for the refund below.
        match outcome {
            Ok(task) => {
                let deposit = board
                    .get_pending_deposit(escrow_id)
                    .expect("confirm_dispute_bond consumed this deposit, and no path removes one")
                    .clone();
                Ok((task, deposit))
            }
            Err(e) => Err(e),
        }
    };
    let (task, deposit) = match confirm_result {
        Ok(confirmed) => confirmed,
        Err(BoardError::DisputeWindowClosed) => {
            let deposit = {
                let board = state.board.read().await;
                board.get_pending_deposit(escrow_id).cloned()
            };
            if let Some(deposit) = deposit {
                refund_escrow(&state, &deposit).await;
            }
            return Err(BoardError::DisputeWindowClosed.into());
        }
        Err(e) => return Err(e.into()),
    };
    // One transaction, not two -- see `confirm_task_escrow` and plan
    // §6.5b. This handler was never drilled, but it has the identical
    // shape: a crash between the task's commit and the deposit's leaves
    // a task already moved to Disputed beside a bond deposit that still
    // reads Reserved, and confirming it again attaches a second dispute
    // paid for once.
    state
        .store
        .save_task_and_deposit(&task, &deposit)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(TaskDto::from(&task)))
}

/// Operator-only: resolves a filed dispute and settles both legs right
/// away (bounty via the ordinary machinery, bond via its own dedicated
/// path -- see `settle_dispute_bond`) rather than waiting for the sweep.
pub async fn resolve_dispute(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<Uuid>,
    // The request as it actually arrived: bound into the signature,
    // so this envelope cannot be replayed at a different endpoint.
    method: Method,
    OriginalUri(uri): OriginalUri,
    Json(envelope): Json<SignedEnvelope<ResolveDisputePayload>>,
) -> Result<Json<TaskDto>, ApiError> {
    if envelope.payload.task_id != task_id {
        return Err(ApiError::BadRequest(
            "task id in the URL doesn't match the signed payload".into(),
        ));
    }
    let pubkey = envelope.verify(&state, method.as_str(), uri.path())?;
    require_operator(&pubkey, &state)?;

    let (_winner, loser) = {
        let mut board = state.board.write().await;
        board.resolve_dispute(task_id, envelope.payload.outcome)?
    };
    // resolve_dispute dinged the loser's reputation immediately,
    // mirroring resolve_consensus's "dinged at resolution" convention.
    // One transaction with the task, and the failure surfaced rather
    // than logged behind a 200: this used to save the task, then the
    // reputation, and swallow the second error -- so a caller could be
    // told the dispute was resolved while the ding that resolution
    // consisted of never reached disk (plan §6.5c).
    let (task, loser_reputation) = {
        let board = state.board.read().await;
        (
            board.get_task(task_id).expect("just resolved, must still exist").clone(),
            board.reputation(&loser),
        )
    };
    state
        .store
        .save_task_and_reputation(&task, &loser, &loser_reputation)
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    try_settle_verified_task(&state, task_id).await;
    settle_dispute_bond(&state, task_id).await;

    let task = state.board.read().await.get_task(task_id).expect("still exists").clone();
    Ok(Json(TaskDto::from(&task)))
}

pub async fn claim_task(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<Uuid>,
    // The request as it actually arrived: bound into the signature,
    // so this envelope cannot be replayed at a different endpoint.
    method: Method,
    OriginalUri(uri): OriginalUri,
    Json(envelope): Json<SignedEnvelope<ClaimPayload>>,
) -> Result<Json<TaskDto>, ApiError> {
    if envelope.payload.task_id != task_id {
        return Err(ApiError::BadRequest(
            "task id in the URL doesn't match the signed payload".into(),
        ));
    }
    let pubkey = envelope.verify(&state, method.as_str(), uri.path())?;

    let task = {
        let mut board = state.board.write().await;
        let is_consensus = matches!(
            board
                .get_task(task_id)
                .ok_or_else(|| ApiError::NotFound("task not found".into()))?
                .kind,
            TaskKind::Consensus { .. }
        );
        if is_consensus {
            board.join_consensus_task(task_id, pubkey)?;
        } else {
            let deadline = Utc::now() + Duration::minutes(CLAIM_TTL_MINUTES);
            board.claim_task(task_id, pubkey, deadline)?;
        }
        board.get_task(task_id).expect("just touched it").clone()
    };
    state.store.save_task(&task).map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(TaskDto::from(&task)))
}

/// Operator-only: cancels `task_id` directly rather than waiting out its
/// usual expiry path. See `TaskBoard::cancel_task` for the exact rules
/// (works on either task kind, no payout/reputation impact, rejects an
/// already-terminal task).
pub async fn cancel_task(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<Uuid>,
    // The request as it actually arrived: bound into the signature,
    // so this envelope cannot be replayed at a different endpoint.
    method: Method,
    OriginalUri(uri): OriginalUri,
    Json(envelope): Json<SignedEnvelope<CancelPayload>>,
) -> Result<Json<TaskDto>, ApiError> {
    if envelope.payload.task_id != task_id {
        return Err(ApiError::BadRequest(
            "task id in the URL doesn't match the signed payload".into(),
        ));
    }
    let pubkey = envelope.verify(&state, method.as_str(), uri.path())?;
    require_operator(&pubkey, &state)?;

    let task = {
        let mut live = state.board.write().await;
        let mut staged = TaskBoard::new();
        staged.restore_task(live.get_task(task_id).cloned().ok_or(BoardError::NotFound)?);
        staged.cancel_task(task_id)?;
        let task = staged.get_task(task_id).unwrap().clone();
        state.store.save_task(&task).map_err(|e| ApiError::Internal(e.to_string()))?;
        live.restore_task(task.clone());
        task
    };
    refund_closed_task_escrow(&state, task_id).await;
    Ok(Json(TaskDto::from(&task)))
}

pub async fn submit_task(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<Uuid>,
    // The request as it actually arrived: bound into the signature,
    // so this envelope cannot be replayed at a different endpoint.
    method: Method,
    OriginalUri(uri): OriginalUri,
    Json(envelope): Json<SignedEnvelope<SubmitPayload>>,
) -> Result<Json<SubmitResultDto>, ApiError> {
    if envelope.payload.task_id != task_id {
        return Err(ApiError::BadRequest(
            "task id in the URL doesn't match the signed payload".into(),
        ));
    }
    let pubkey = envelope.verify(&state, method.as_str(), uri.path())?;
    validate_text_field(&envelope.payload.output, "output")?;

    enum Dispatch {
        HashMatch,
        Consensus,
        Disputable,
    }
    let dispatch = {
        let board = state.board.read().await;
        let task = board
            .get_task(task_id)
            .ok_or_else(|| ApiError::NotFound("task not found".into()))?;
        match task.kind {
            TaskKind::Consensus { .. } => Dispatch::Consensus,
            TaskKind::Disputable { .. } => Dispatch::Disputable,
            TaskKind::HashMatch { .. } => Dispatch::HashMatch,
        }
    };

    match dispatch {
        Dispatch::Consensus => submit_consensus_task(&state, task_id, pubkey, envelope.payload.output.clone()).await,
        Dispatch::Disputable => submit_disputable_task(&state, task_id, pubkey, envelope.payload.output.clone()).await,
        Dispatch::HashMatch => submit_hash_match_task(&state, task_id, pubkey, &envelope.payload.output).await,
    }
}

/// `Disputable` only. Unlike `HashMatch`/`Consensus`, submitting never
/// pays out (or even finally resolves) anything synchronously -- it just
/// opens the dispute window (see `TaskBoard::submit_disputable_answer`).
/// Settlement happens later, either via the sweep (unchallenged) or an
/// operator's `resolve_dispute` call (challenged).
async fn submit_disputable_task(
    state: &AppState,
    task_id: Uuid,
    pubkey: PublicKey,
    answer: String,
) -> Result<Json<SubmitResultDto>, ApiError> {
    let task_after_submit = {
        let mut board = state.board.write().await;
        board.submit_disputable_answer(task_id, pubkey.clone(), answer, Utc::now())?;
        board.get_task(task_id).expect("just touched it").clone()
    };
    persist_task_and_reputation(state, &task_after_submit, &pubkey).await?;

    Ok(Json(SubmitResultDto {
        verified: true,
        paid: false,
        bounty: Some(task_after_submit.bounty),
        resolved: Some(false),
    }))
}

async fn submit_hash_match_task(
    state: &AppState,
    task_id: Uuid,
    pubkey: PublicKey,
    output: &str,
) -> Result<Json<SubmitResultDto>, ApiError> {
    let output_hash = Hash::hash_bytes(output.as_bytes());

    let (verified, task_after_submit) = {
        let mut board = state.board.write().await;
        let verified = board.submit(task_id, pubkey.clone(), output_hash)?;
        (verified, board.get_task(task_id).expect("just touched it").clone())
    };
    persist_task_and_reputation(state, &task_after_submit, &pubkey).await?;

    if !verified {
        return Ok(Json(SubmitResultDto {
            verified: false,
            paid: false,
            bounty: None,
            resolved: None,
        }));
    }

    // Left `Verified` either way -- if this fails, the task isn't lost:
    // the sweep loop in main.rs retries every Verified-but-unpaid task
    // periodically, so a transient payout failure here self-heals without
    // needing a human to notice and resubmit it by hand.
    let paid = try_settle_verified_task(state, task_id).await;

    Ok(Json(SubmitResultDto {
        verified: true,
        paid,
        bounty: Some(task_after_submit.bounty),
        resolved: None,
    }))
}

async fn submit_consensus_task(
    state: &AppState,
    task_id: Uuid,
    pubkey: PublicKey,
    output: String,
) -> Result<Json<SubmitResultDto>, ApiError> {
    let (resolved, task_after_submit) = {
        let mut board = state.board.write().await;
        let resolved = board.submit_consensus_answer(task_id, pubkey.clone(), output)?;
        (resolved, board.get_task(task_id).expect("just touched it").clone())
    };
    // The task and every reputation this submission touched, in one
    // commit -- including, when this submission resolved the task, every
    // other assignee dinged by that resolution. Three separate writes
    // before, the last of them error-dropping (see
    // `persist_consensus_submission`).
    persist_consensus_submission(state, &task_after_submit, &pubkey, resolved).await?;

    if !resolved {
        return Ok(Json(SubmitResultDto {
            verified: false,
            paid: false,
            bounty: None,
            resolved: Some(false),
        }));
    }

    // Resolution just happened -- pay out every winner it produced right
    // away rather than waiting for the next sweep. Deliberately
    // unconditional: the caller who happened to trigger resolution (by
    // being the last to submit) is not necessarily a winner themselves,
    // but winners still need settling regardless of who completed the set.
    try_settle_verified_task(state, task_id).await;

    // Did *this* agent's own answer match the majority? Absent from
    // `pending_payouts` (computed from the pre-settlement snapshot, so it
    // still names every winner) means either they disagreed, or the task
    // closed with no majority at all (see `TaskStatus::Closed`) -- either
    // way, nothing owed to them.
    let my_share = task_after_submit.pending_payouts().into_iter().find(|(pk, _)| *pk == pubkey);
    let Some((_, amount)) = my_share else {
        return Ok(Json(SubmitResultDto {
            verified: false,
            paid: false,
            bounty: None,
            resolved: Some(true),
        }));
    };

    let i_am_paid = {
        let board = state.board.read().await;
        matches!(
            &board.get_task(task_id).expect("still exists").kind,
            TaskKind::Consensus { assignees, .. } if assignees.get(&pubkey).is_some_and(|a| a.paid)
        )
    };

    Ok(Json(SubmitResultDto {
        verified: true,
        paid: i_am_paid,
        bounty: Some(amount),
        resolved: Some(true),
    }))
}

// ---------------------------------------------------------------------
// Verified-task settlement (payout) -- shared between the immediate
// attempt right after submit_task verifies a task, and the periodic sweep
// in main.rs retrying whatever an earlier attempt left unpaid.
// ---------------------------------------------------------------------

/// (task, recipient) pairs currently being paid out, so a live
/// `submit_task` call and a concurrent sweep (or two overlapping sweeps)
/// never both attempt to pay the same recipient their share of the same
/// task at once -- without this, both could see that share as still
/// pending and each send an independent, fully-valid payout transaction,
/// actually double-paying it on-chain. Keyed by the recipient's string
/// form rather than `PublicKey` itself since the latter has no `Hash`
/// impl. Keyed per-(task, recipient) rather than just per-task so a
/// `Consensus` task's several winners can be paid out independently and
/// concurrently, instead of serializing behind one task-wide lock.
#[dynamic]
static PAYOUT_IN_FLIGHT: DashMap<(Uuid, String), ()> = DashMap::new();

/// Submits a payout for every recipient still owed a share of the task's
/// bounty and not already waiting on one (see
/// `TaskBoard::unsubmitted_payouts` -- for a `HashMatch` task that's
/// always exactly one recipient; for a `Consensus` task it may be
/// several). Safe to call repeatedly/concurrently: at most one caller
/// sends for a given (task, recipient) pair at a time (see
/// `PAYOUT_IN_FLIGHT`), and anything not currently owed (already
/// confirmed, already in flight, or the task is in neither `Verified`
/// nor `Submitted`) is simply skipped.
///
/// **Returns whether everything owed is now on the wire, not whether it
/// was paid.** Confirmation is the sweep's job and comes at least a
/// sweep later (`resolve_payout_attempt`); `false` here means the task
/// had nothing to send, a send failed, or another caller was already
/// handling it, all of which the sweep retries.
///
/// The retry used to be best-effort rather than idempotent: a previous
/// attempt whose transaction actually made it on-chain, unrecorded, meant
/// a retry sent a second independent payment. That is what `Submitted`
/// and `PayoutAttempt` removed -- a payout with a transaction in flight
/// is no longer in the set this sends, and it is only ever re-sent once
/// the node has been shown to hold neither the output nor a claim on the
/// inputs.
///
/// An escrow-funded task (see `Task::escrow_id`) is settled entirely
/// differently from an operator-funded one -- see `settle_escrow_funded_task`.
pub async fn try_settle_verified_task(state: &AppState, task_id: Uuid) -> bool {
    let (payouts, escrow, status) = {
        let board = state.board.read().await;
        match board.get_task(task_id) {
            Some(t) if matches!(t.status, TaskStatus::Verified | TaskStatus::Submitted) => {
                let escrow = t.escrow_id.and_then(|id| board.get_pending_deposit(id).cloned());
                // Not `pending_payouts`: a recipient whose transaction is
                // already on the wire is still owed, but must not be sent
                // a second one. The subtraction here is what keeps a
                // multi-winner task's in-flight legs from being
                // duplicated while an unsent one is sent.
                (board.unsubmitted_payouts(task_id), escrow, t.status)
            }
            _ => return false,
        }
    };
    if payouts.is_empty() {
        return false;
    }

    // A `Submitted` task with something still unsent cannot happen
    // through correct operation, and this is the one place that would
    // have acted on it as though it could.
    //
    // The invariant: `record_payout_attempt` (board.rs) is the *only*
    // path into `Submitted` and it moves a task there only when
    // `unsubmitted_payouts` is empty; nothing afterwards can grow that
    // set, because the sole production caller of `clear_payout_attempt`
    // pairs it with `mark_recipient_paid`, which drops that recipient
    // out of `owed_payouts` in the same breath. So reaching here means
    // the *store* lost a `PayoutAttempt` row -- the state plan §6.5c
    // describes, produced by the old `record_confirmed_payout`'s first
    // commit landing without its second, or by the rollback hole.
    //
    // The comment that used to sit above justified accepting `Submitted`
    // as letting "a multi-winner task with one leg unsent still get that
    // leg sent". That case is `Verified`, not `Submitted`, by
    // construction of `record_payout_attempt` -- so the branch was dead
    // in every healthy hub and live only here, where sending is the
    // worst available action: the attempt that was the double-spend
    // guard is precisely the record that went missing, so a send would
    // re-pay a bounty whose transaction may already be on the chain.
    //
    // Refusing is not a guess. §6.5's three-way rule needs the output
    // hash to call a payout confirmed and the spent inputs to call it
    // lost, and both died with the attempt -- so this state carries
    // strictly less evidence than the rule's "ambiguous" row, which §6.5
    // already decided is left alone rather than collapsed into either
    // neighbour. An operator resolves it; `docs/deployment.md` §10.3
    // carries the procedure and the one sound test that does exist.
    if status == TaskStatus::Submitted {
        state.metrics.payout_sends_refused.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        error!(
            "refusing to send {} payout(s) for task {task_id}: it reads Submitted with payouts \
             still unsent, which means a PayoutAttempt row was lost \
             [submitted_task_with_no_payout_attempt]. Sending now could pay a bounty twice -- \
             the record that would have prevented it is the one that is missing. This needs an \
             operator (docs/deployment.md §10.3).",
            payouts.len()
        );
        return false;
    }

    match escrow {
        Some(deposit) => settle_escrow_funded_task(state, task_id, &deposit).await,
        None => {
            let mut all_paid = true;
            for (recipient, amount) in payouts {
                if !settle_one_payout(state, task_id, &recipient, amount).await {
                    all_paid = false;
                }
            }
            all_paid
        }
    }
}

/// RAII handle on one `PAYOUT_IN_FLIGHT` entry: releases it on `Drop`
/// unconditionally, including if the future holding it is cancelled
/// mid-await (e.g. an HTTP client disconnecting while this is awaiting a
/// slow `pay_bounty` call). A plain `insert`-then-`remove` pair would
/// leak the entry forever in that case -- cancellation skips whatever
/// code was going to run `remove` next, but it can never skip a value's
/// `Drop`.
struct PayoutGuard(Option<(Uuid, String)>);

impl PayoutGuard {
    /// Returns `None` (acquisition failed) if `key` is already held by
    /// another in-flight settlement attempt.
    fn try_acquire(key: (Uuid, String)) -> Option<Self> {
        if PAYOUT_IN_FLIGHT.insert(key.clone(), ()).is_some() {
            return None;
        }
        Some(PayoutGuard(Some(key)))
    }
}

impl Drop for PayoutGuard {
    fn drop(&mut self) {
        if let Some(key) = self.0.take() {
            PAYOUT_IN_FLIGHT.remove(&key);
        }
    }
}

/// Parallel to `PAYOUT_IN_FLIGHT`, but at the right granularity for an
/// escrow: paying out (or refunding) everything at one `PendingDeposit`'s
/// address is a single all-or-nothing operation against that escrow's own
/// one-time address, not "N independent recipients from a shared,
/// ever-refilled pool" the way the operator's wallet is treated -- so
/// this is keyed by `escrow_id` alone.
#[dynamic]
static ESCROW_SETTLEMENT_IN_FLIGHT: DashMap<Uuid, ()> = DashMap::new();

/// Same RAII shape as `PayoutGuard` -- releases unconditionally on drop,
/// including on cancellation, so a cancelled attempt can never leak the
/// entry and permanently stall that escrow.
struct EscrowSettlementGuard(Option<Uuid>);

impl EscrowSettlementGuard {
    fn try_acquire(escrow_id: Uuid) -> Option<Self> {
        if ESCROW_SETTLEMENT_IN_FLIGHT.insert(escrow_id, ()).is_some() {
            return None;
        }
        Some(EscrowSettlementGuard(Some(escrow_id)))
    }
}

impl Drop for EscrowSettlementGuard {
    fn drop(&mut self) {
        if let Some(escrow_id) = self.0.take() {
            ESCROW_SETTLEMENT_IN_FLIGHT.remove(&escrow_id);
        }
    }
}

/// Settles every still-owed recipient of an escrow-funded task in ONE
/// combined transaction (see `btclib::payment::build_multi_payment`'s own
/// doc comment for why: `deposit`'s address holds exactly its one
/// deposit, not an ongoing float like the operator's wallet, so paying
/// winners independently would send "change" back to the depositor after
/// the first payout, stranding every recipient after it).
async fn settle_escrow_funded_task(
    state: &AppState,
    task_id: Uuid,
    deposit: &PendingDeposit,
) -> bool {
    let Some(_guard) = EscrowSettlementGuard::try_acquire(deposit.id) else {
        return false;
    };
    // Re-check live state immediately before spending, same principle as
    // settle_one_payout_inner's own re-check below: `payouts` may be a
    // stale snapshot if another attempt already settled some of these.
    //
    // Deliberately *every* still-owed winner, not just the ones the
    // caller named: this escrow's address holds exactly one deposit and
    // change goes back to the depositor, so a transaction that pays only
    // some of them sends the rest of the money home and strands whoever
    // was left out. That is why a resubmission after a proven loss
    // rebuilds the whole set rather than the lost leg alone.
    let still_owed: Vec<(PublicKey, u64)> = {
        let board = state.board.read().await;
        let Some(task) = board.get_task(task_id) else {
            return false;
        };
        task.owed_payouts()
    };
    if still_owed.is_empty() {
        return true;
    }

    if let Err(e) = submit_task_payout(
        state,
        task_id,
        &deposit.private_key(&state.escrow_secret),
        &deposit.deposit_pubkey,
        &still_owed,
        &deposit.depositor,
    )
    .await
    {
        warn!(
            "escrow settlement for task {task_id} (escrow {}) failed, will retry: {e}",
            deposit.id
        );
        return false;
    }
    true
}

/// What else, beyond the deposit's own `Refunded` status, a disbursement
/// has to make durable -- and therefore what has to be committed in the
/// *same* redb transaction as that status.
///
/// A parameter rather than a step each caller applies after the fact,
/// because "after the fact" is exactly what the bug was: the reputation
/// credit was durable and the status was not, so a restart reloaded the
/// bond as still owing and the settlement ran a second time (plan
/// §6.5c).
enum EscrowCredit {
    /// Nothing but the deposit itself. A plain refund back to the
    /// depositor never earns anything -- getting your own money back
    /// isn't "earning" -- and the exchange sweep moves the hub's own
    /// money between the hub's own addresses.
    None,
    /// A forfeited dispute bond: the recipient's `total_earned` grows by
    /// what actually reached them, which is the one case where receiving
    /// an escrow's balance is earning it (see
    /// `TaskBoard::credit_forfeited_bond`).
    ForfeitedBond,
}

/// Sends `deposit`'s current on-chain balance to `recipient` -- who need
/// not be `deposit.depositor`: a resolved dispute can send a bond forward
/// to the winning party instead of back to whoever posted it (see
/// `settle_dispute_bond`) -- then marks the deposit `Refunded`, durably,
/// together with whatever `credit` says that earned. Returns the *net*
/// amount actually sent (after the network fee, `None` if nothing was
/// sent or any step failed) -- a caller reporting the amount must use
/// that, not `deposit.required_amount`, which overstates it by the fee.
async fn disburse_escrow(
    state: &AppState,
    deposit: &PendingDeposit,
    recipient: &PublicKey,
    credit: EscrowCredit,
) -> Option<u64> {
    let Some(_guard) = EscrowSettlementGuard::try_acquire(deposit.id) else {
        return None;
    };
    {
        let board = state.board.read().await;
        if let Some(p) = board.payments.values().find(|p| matches!(p.purpose,
            crate::payments::Purpose::Escrow { deposit_id, .. } if deposit_id == deposit.id)) {
            return (p.status == crate::payments::Status::Confirmed).then_some(p.amount);
        }
        let current = board.get_pending_deposit(deposit.id)?;
        if current.status == EscrowStatus::Refunded { return Some(0); }
        if current.status != deposit.status { return None; }
    }
    let balance = state.node.balance(&deposit.deposit_pubkey).await.ok()?;
    let amount = balance.saturating_sub(HUB_TRANSACTION_FEE);
    let tx = if amount > 0 {
        Some(build_payment_from_fresh(state, &deposit.private_key(&state.escrow_secret),
            &deposit.deposit_pubkey, &[(recipient.clone(), amount)], recipient).await.ok()?)
    } else { None };
    let payment = crate::payments::Payment::new(
        crate::payments::Purpose::Escrow { deposit_id: deposit.id,
            forfeited_bond: matches!(credit, EscrowCredit::ForfeitedBond), previous_status: deposit.status },
        deposit.deposit_pubkey.clone(), recipient.clone(), amount, tx);
    match crate::payments::prepare(state, payment).await {
        Ok(payment) => { crate::payments::send(state, &payment).await;
            let board = state.board.read().await;
            (board.payments[&payment.id].status == crate::payments::Status::Confirmed).then_some(payment.amount) }
        Err(e) => { warn!("escrow {} payment not prepared: {e}", deposit.id); None }
    }
}

/// Refunds whatever balance remains at `deposit`'s address back to its
/// own `depositor` -- shared by the sweep's overdue-unconfirmed-
/// reservation path (nothing may have ever arrived) and by
/// `refund_closed_task_escrow` (a materialized task that turned out to
/// end without a winner: a Consensus tie, an understaffed cancellation,
/// or an operator `cancel_task`), since both are "this escrow's money has
/// nowhere left to go but back to whoever deposited it."
pub async fn refund_escrow(state: &AppState, deposit: &PendingDeposit) {
    disburse_escrow(state, deposit, &deposit.depositor, EscrowCredit::None).await;
}

/// If `task_id` was escrow-funded, refunds whatever remains at its
/// escrow's address back to its poster. A no-op for an operator-funded
/// task (nothing was ever escrowed per-task to refund). Called from every
/// path that can close a task without a winner.
pub async fn refund_closed_task_escrow(state: &AppState, task_id: Uuid) {
    let deposit = {
        let board = state.board.read().await;
        board.escrow_for_task(task_id).cloned()
    };
    if let Some(deposit) = deposit {
        refund_escrow(state, &deposit).await;
    }
}

/// Settles the *bond* leg of a resolved `Disputable` task's dispute --
/// the *bounty* leg is already handled by the ordinary
/// `try_settle_verified_task` (since `Task::pending_payouts` for a
/// resolved dispute already names the winner and draws on the task's own
/// escrow), but the bond lives in a *different* escrow than the task's
/// own, so it needs its own dedicated settlement (the same reason
/// escrow-funded multi-winner Consensus tasks needed `build_multi_payment`
/// instead of reusing single-recipient plumbing -- two distinct funding
/// sources can't be expressed through a mechanism that assumes one).
/// Safe to call repeatedly/on tasks that don't apply: a no-op (returns
/// `true`, "nothing to do") unless there's a resolved dispute whose bond
/// hasn't been disbursed yet.
pub async fn settle_dispute_bond(state: &AppState, task_id: Uuid) -> bool {
    let settlement = {
        let board = state.board.read().await;
        let Some(task) = board.get_task(task_id) else {
            return true;
        };
        let TaskKind::Disputable { dispute: Some(d), .. } = &task.kind else {
            return true;
        };
        let Some(resolution) = d.resolution else {
            return true; // Disputed but not yet resolved -- nothing to settle
        };
        let Some(bond) = board.get_pending_deposit(d.bond_escrow_id) else {
            return true;
        };
        if bond.status != EscrowStatus::Consumed {
            return true; // already disbursed (or, in principle, never reached Disputed)
        }
        let winner = match resolution {
            DisputeResolution::ChallengerWins => d.challenger.clone(),
            DisputeResolution::AssigneeWins => match task.claimant.clone() {
                Some(c) => c,
                None => return false, // shouldn't happen -- a Disputed task always has a claimant
            },
        };
        let is_forfeiture = matches!(resolution, DisputeResolution::AssigneeWins);
        (bond.clone(), winner, is_forfeiture)
    };
    let (bond_deposit, winner, is_forfeiture) = settlement;

    // The forfeiture credit is `disburse_escrow`'s to apply, not this
    // function's, and that is the substance of the fix rather than a
    // tidy-up: applied here it was a separate commit from the bond's
    // `Refunded` status, so the credit survived a restart and the status
    // did not. The bond then reloaded `Consumed`,
    // `tasks_with_unsettled_dispute_bonds` selected it again, and this
    // ran again at every boot for the life of the deployment. It
    // credited nothing on those re-runs -- the retry reads a drained
    // address -- so what it cost was a node round trip per pass rather
    // than a wrong ledger (plan §6.5c has the measurement, and why
    // relying on that is not a defence).
    let credit = if is_forfeiture { EscrowCredit::ForfeitedBond } else { EscrowCredit::None };
    disburse_escrow(state, &bond_deposit, &winner, credit).await.is_some()
}

/// Pays `recipient` their `amount`-sized share of `task_id`'s bounty and
/// records it, guarded by `PAYOUT_IN_FLIGHT` so this exact (task,
/// recipient) pair is never paid twice by two racing callers.
async fn settle_one_payout(
    state: &AppState,
    task_id: Uuid,
    recipient: &PublicKey,
    amount: u64,
) -> bool {
    let Some(_guard) = PayoutGuard::try_acquire((task_id, recipient.to_string())) else {
        return false;
    };
    settle_one_payout_inner(state, task_id, recipient, amount).await
}

async fn settle_one_payout_inner(
    state: &AppState,
    task_id: Uuid,
    recipient: &PublicKey,
    amount: u64,
) -> bool {
    // Re-check live state immediately before spending anything: the
    // (recipient, amount) pair this was called with may come from an
    // earlier, now-stale `pending_payouts()` snapshot -- if a concurrent
    // settlement attempt (the sweep vs. this call, or two overlapping
    // sweeps on a slow multi-winner payout) already paid this exact
    // recipient in the meantime, `PAYOUT_IN_FLIGHT` alone wouldn't catch
    // it, since that other attempt would have already released its guard.
    // One guard for both reads, never a second `read()` taken while the
    // first is still alive: tokio's `RwLock` is write-preferring, so a
    // nested read deadlocks outright the moment any writer is queued
    // behind it.
    let Some((already_paid, already_in_flight)) = ({
        let board = state.board.read().await;
        board.get_task(task_id).map(|task| {
            (
                task.is_recipient_paid(recipient),
                board.payout_attempt(task_id, recipient).is_some(),
            )
        })
    }) else {
        return false;
    };
    if already_paid {
        return true;
    }
    // A payout already on the wire must never be sent a second time.
    // The node answers a duplicate transaction with a strike, and three
    // strikes in ten minutes bans this box from its own node (plan
    // §6.2) -- so the guard has to be here, before building anything,
    // and not merely in the sweep's choice of what to hand this.
    if already_in_flight {
        return true;
    }

    if let Err(e) = submit_bounty_payout(state, task_id, recipient, amount).await {
        warn!("payout for task {task_id} to {recipient} failed, will retry: {e}");
        return false;
    }
    true
}

/// Works out what became of one submitted payout and acts on it. The
/// sweep's half of the settlement machinery, and the only thing that
/// ever moves a task from `Submitted` to `Paid`.
///
/// Two node reads: what the recipient holds, and what is left at the
/// address the payout spent from. `PayoutAttempt::resolve` turns those
/// into the three answers plan §6.5 names, and this acts on each:
///
/// - **Confirmed** -- the recipient holds the exact output this attempt
///   created. Record it, once.
/// - **Never landed** -- the output is nowhere and every input is still
///   unspent and unmarked at the source, so no block and no mempool has
///   this transaction. Build a fresh one, unless the budget is spent.
/// - **Ambiguous** -- anything else. Wait, and say so. Resubmitting here
///   could pay a bounty twice and would earn the node's
///   duplicate-transaction strike (plan §6.2); calling it paid would put
///   back exactly the lie this removes.
///
/// A node that cannot be reached is not an answer at all -- the attempt
/// is left alone for the next sweep rather than being guessed at, which
/// is the same posture every other node read in this file takes.
///
/// Returns whether the payout is now finished, one way or another
/// (confirmed, or abandoned) -- `false` while the hub is still waiting.
pub async fn resolve_payout_attempt(state: &AppState, attempt: &PayoutAttempt) -> bool {
    let recipient_utxos = match state.node.fetch_utxos(&attempt.recipient).await {
        Ok(utxos) => utxos,
        Err(e) => {
            warn!(
                "cannot resolve the payout for task {} to {}: {e}",
                attempt.task_id, attempt.recipient
            );
            return false;
        }
    };
    let source_utxos = match state.node.fetch_utxos(&attempt.source).await {
        Ok(utxos) => utxos,
        Err(e) => {
            warn!(
                "cannot resolve the payout for task {} to {}: {e}",
                attempt.task_id, attempt.recipient
            );
            return false;
        }
    };

    match attempt.resolve(&recipient_utxos, &source_utxos) {
        PayoutOutcome::Confirmed => {
            // Guarded by the same `PAYOUT_IN_FLIGHT` entry an original
            // settlement takes, so two overlapping sweeps cannot both
            // credit the same recipient's reputation for one payout.
            let Some(_guard) =
                PayoutGuard::try_acquire((attempt.task_id, attempt.recipient.to_string()))
            else {
                return false;
            };
            // Re-check under the guard: the attempt this was called with
            // is a snapshot taken before the two node reads above, and a
            // concurrent resolution may have finished in between.
            let stale = {
                let board = state.board.read().await;
                board.payout_attempt(attempt.task_id, &attempt.recipient) != Some(attempt)
            };
            if stale {
                return false;
            }
            info!(
                "payout for task {} to {} confirmed on chain after {} submission(s)",
                attempt.task_id, attempt.recipient, attempt.submissions
            );
            record_confirmed_payout(state, attempt.task_id, &attempt.recipient, attempt.amount).await
        }
        PayoutOutcome::NeverLanded => {
            if attempt.submissions >= MAX_PAYOUT_SUBMISSIONS {
                abandon_payout(state, attempt).await;
                return true;
            }
            warn!(
                "payout for task {} to {} never reached the chain (submission {} of {}), resending",
                attempt.task_id,
                attempt.recipient,
                attempt.submissions,
                MAX_PAYOUT_SUBMISSIONS
            );
            resend_lost_payout(state, attempt).await;
            false
        }
        PayoutOutcome::HeldInMempool => {
            // Quiet, and inert. The node is holding this transaction and
            // nothing has consumed its inputs, so there is no decision
            // to make and nothing for anyone to look at: the next sweep
            // reads `Confirmed` when it mines, or `NeverLanded` if a
            // mempool drops it, and resends from there.
            //
            // Deliberately not logged at all. A sweep runs every minute
            // and every payment passes through this state on its way to
            // a block, so a line here is one per payment per minute of
            // ordinary operation -- which is how a log stops being read.
            false
        }
        PayoutOutcome::Ambiguous => {
            // Deliberately loud and deliberately inert. This is now
            // genuinely rare: it needs an input to have been *consumed*
            // by something other than this transaction, or the recipient
            // to have spent the bounty onward between two sweeps. A
            // transaction merely waiting for a block no longer reaches
            // here -- see `HeldInMempool` above, which is the state this
            // arm used to absorb and shout about.
            warn!(
                "payout for task {} to {} is unresolved: submitted {}, neither confirmed nor \
                 provably lost. Not resending -- a duplicate risks paying twice and earns a \
                 node strike. Check the chain by hand if this persists.",
                attempt.task_id, attempt.recipient, attempt.submitted_at
            );
            false
        }
    }
}

/// Rebuilds and resends a payout the node has been shown not to hold.
///
/// Routed back through the ordinary settlement path rather than
/// re-sending the old transaction, for two reasons. A rebuild picks up
/// UTXOs that have appeared since, which is what makes it likely to
/// succeed where the first attempt did not; and it produces fresh output
/// `unique_id`s, so the new attempt is distinguishable from the one it
/// replaces and cannot be confirmed by evidence of the old.
///
/// An escrow-funded task rebuilds *every* still-owed winner into one
/// transaction, which is why this goes through `try_settle_verified_task`
/// rather than paying the one recipient directly -- see
/// `settle_escrow_funded_task` for why paying a subset strands the rest.
async fn resend_lost_payout(state: &AppState, attempt: &PayoutAttempt) {
    // The attempt is deliberately left on the board across the resend.
    // `submit_task_payout` reads its `submissions` count to number the
    // replacement, and clearing it first would reset the budget to zero
    // every time -- a payout that can never land would then retry
    // forever instead of reaching `PayoutFailed`.
    let state_of_play = {
        let board = state.board.read().await;
        board.get_task(attempt.task_id).map(|task| {
            (
                task.escrow_id.and_then(|id| board.get_pending_deposit(id).cloned()),
                board.payout_attempt(attempt.task_id, &attempt.recipient).cloned(),
            )
        })
    };
    let Some((escrow, live)) = state_of_play else {
        return;
    };
    if live.as_ref() != Some(attempt) {
        return; // superseded while the node reads were in flight
    }

    // The same transaction, if this attempt still has it. A resend that
    // *rebuilds* is a second payment to the same recipient, and two
    // payments can both mine: `NeverLanded` proves nothing on the node
    // the hub is talking to holds the first, and it cannot prove that
    // about a mempool it cannot see -- another node in the pool, or a
    // peer the first node broadcast to. Two identical transactions are
    // one transaction and a node mines one; two different ones paying
    // the same person are two bounties. `payments.rs` has resent the
    // same bytes since it was written; this is that rule arriving here.
    //
    // A consensus payout's legs share one transaction, so this re-offers
    // the whole settlement, which is correct -- it is the transaction
    // that was always going to pay all of them. Only the leg that
    // resolved bumps its own counter, and the other legs will not
    // stampede after it: once this is on the wire their inputs read
    // marked, which is `HeldInMempool` and not a resend at all.
    if let Some(tx) = attempt.transaction.clone() {
        let mut next = attempt.clone();
        next.submissions += 1;
        next.submitted_at = Utc::now();
        // Durable before the wire, for the same reason the first
        // submission is: a resend the hub forgets is a resend it will
        // make again with a fresh budget.
        if let Err(e) = state.store.save_payout_attempt(&next) {
            warn!(
                "could not record the resend of the payout for task {} to {}: {e} -- \
                 not sending, so the attempt on disk stays the one that was last submitted",
                attempt.task_id, attempt.recipient
            );
            return;
        }
        state.board.write().await.record_payout_attempt(next.clone());
        warn!(
            "payout for task {} to {} never reached the chain; resending the same transaction \
             (submission {} of {})",
            attempt.task_id, attempt.recipient, next.submissions, MAX_PAYOUT_SUBMISSIONS
        );
        if let Err(e) = state.node.submit_transaction(tx).await {
            warn!(
                "resend of the payout for task {} to {} failed, will retry: {e}",
                attempt.task_id, attempt.recipient
            );
        }
        return;
    }

    // No stored transaction: an attempt written before they were
    // recorded. Rebuilding is what this always did, and is no worse than
    // it was -- but it is the path with the duplicate risk, so it exists
    // only for records that predate the field.
    let resent = match escrow {
        // Straight to the escrow path rather than through
        // `try_settle_verified_task`, which would find nothing to do:
        // the attempt is still on the board, so `unsubmitted_payouts`
        // (rightly) excludes this recipient. `settle_escrow_funded_task`
        // computes the still-owed set from the task itself, so it
        // rebuilds every winner into one transaction -- which is what a
        // resend from an escrow address has to do (see its own doc
        // comment: paying a subset sends the change home and strands the
        // rest).
        Some(deposit) => settle_escrow_funded_task(state, attempt.task_id, &deposit).await,
        None => {
            match submit_bounty_payout(state, attempt.task_id, &attempt.recipient, attempt.amount)
                .await
            {
                Ok(()) => true,
                Err(e) => {
                    warn!(
                        "resend of the lost payout for task {} to {} failed, will retry: {e}",
                        attempt.task_id, attempt.recipient
                    );
                    false
                }
            }
        }
    };
    if !resent {
        // Nothing new went out, so the old attempt stands and the next
        // sweep tries again. It is a truthful record either way: the
        // transaction it describes is the one that was last submitted.
        debug!(
            "no replacement payout went out for task {} to {}; the existing attempt stands",
            attempt.task_id, attempt.recipient
        );
    }
}

/// Gives up on a payout that has been proven lost `MAX_PAYOUT_SUBMISSIONS`
/// times, moving the task to `PayoutFailed`.
///
/// This is a claim, not a shrug: every one of those attempts ended with
/// the node showing the inputs untouched, so the hub knows the money did
/// not move and is not writing off a payment that might have happened.
/// The escrow behind it is left exactly where it is -- refunding the
/// poster would take the bounty back from someone who did the work -- and
/// an operator resolves it by hand (`docs/deployment.md` §10.3).
async fn abandon_payout(state: &AppState, attempt: &PayoutAttempt) {
    // Task-terminal, so any sibling leg's attempt goes with it. On a
    // multi-winner task that means an unresolved sibling stops being
    // polled -- which is right: whatever is wrong with the funding
    // source is not one recipient's problem, and the whole task is now
    // an operator's to settle.
    let mut board = state.board.write().await;
    let previous_task = board.get_task(attempt.task_id).cloned();
    let dropped = match board.mark_payout_failed(attempt.task_id) {
        Ok(dropped) => dropped,
        Err(e) => {
            error!("failed to mark task {} payout-failed: {e}", attempt.task_id);
            return;
        }
    };
    // Every attempt the board dropped, not just this one: a sibling leg
    // left in the store comes back at the next restart attached to a
    // task that is now terminal, and is then re-resolved and re-logged
    // every sweep with no way to ever clear it.
    //
    // One transaction with the task, not N deletions and then a save.
    // Split, a crash between them left the task non-terminal on disk
    // beside attempts that were already gone -- the exact state the
    // paragraph above says this exists to prevent, produced by the code
    // meant to prevent it (plan §6.5c).
    let dropped_keys: Vec<(Uuid, PublicKey)> =
        dropped.iter().map(|a| (a.task_id, a.recipient.clone())).collect();
    // Read back under the same write lock that made it terminal, the
    // way the confirm handlers do. `expect` rather than a graceful
    // return: `mark_payout_failed` just succeeded on this task and no
    // path removes one, and a graceful return here would be the one
    // exit that leaves the board terminal with nothing on disk.
    let failed_task = board
        .get_task(attempt.task_id)
        .expect("mark_payout_failed just succeeded on this task, and no path removes one")
        .clone();
    if let Err(e) = state.store.save_task_and_drop_payout_attempts(&failed_task, &dropped_keys) {
        error!(
            "failed to record task {} as payout-failed: {e} -- rolling the board back, so the \
             sweep keeps polling rather than abandoning a payout only in memory",
            attempt.task_id
        );
        if let Some(previous) = previous_task {
            board.restore_task(previous);
        }
        for previous in dropped {
            board.restore_payout_attempt(previous);
        }
        return;
    }
    // Released before the log line: nothing below touches the board,
    // and this is the sweep's write lock.
    drop(board);
    error!(
        "giving up on the payout of {} for task {} to {}: {} submissions, every one of them \
         proven never to have reached the chain. The money is still owed and the escrow is \
         untouched -- this needs an operator (docs/deployment.md §10.3).",
        attempt.amount, attempt.task_id, attempt.recipient, attempt.submissions
    );
}

/// Records that a submitted payout has actually been observed on chain:
/// marks the recipient paid (which credits their reputation and, once
/// every winner is in, completes the task), drops the attempt the sweep
/// was tracking it by, and persists all of it.
///
/// This is the tail `settle_one_payout_inner` used to run the instant a
/// transaction was handed to the socket. Moving it here is the whole
/// substance of the change: everything downstream of it -- reputation,
/// compute credit, the task reading `Paid` -- now waits on evidence
/// rather than on a successful `write(2)`.
async fn record_confirmed_payout(
    state: &AppState,
    task_id: Uuid,
    recipient: &PublicKey,
    amount: u64,
) -> bool {
    // One write lock across every board change and the single commit
    // that makes them durable, and the board put back if that commit
    // fails -- the same posture as `disburse_escrow`, for the same
    // reason.
    //
    // This function used to write up to four separate commits and
    // deleted the attempt *first*, logging every failure and returning
    // `true` regardless. A crash or a store error between the deletion
    // and the task save left a task reading `Submitted` on disk with no
    // attempt tracking it: `outstanding_payout_attempts` had nothing to
    // resolve and `verified_unpaid_tasks` will not take a `Submitted`
    // task either, so the money had moved on chain and the hub had
    // permanently forgotten it (plan §6.5c). Returning `true` on a
    // failed write is what made that silent -- the caller reported the
    // payout finished on the strength of writes it never checked, which
    // is the very habit §6.5 was written to remove.
    let mut board = state.board.write().await;
    let previous_task = board.get_task(task_id).cloned();
    let previous_reputation = board.reputation(recipient);
    let previous_attempt = board.payout_attempt(task_id, recipient).cloned();
    let previous_account = board.exchange_account(recipient);

    if let Err(e) = board.mark_recipient_paid(task_id, recipient, amount) {
        error!(
            "payout for task {task_id} to {recipient} confirmed on-chain but mark_recipient_paid failed: {e}"
        );
        return false;
    }
    board.clear_payout_attempt(task_id, recipient);

    // Same reasoning as `abandon_payout`'s read-back: `expect`, because
    // `mark_recipient_paid` just succeeded on this task and no path
    // removes one, and a graceful return would be the single exit that
    // leaves the board paid with nothing committed.
    let final_task = board
        .get_task(task_id)
        .expect("mark_recipient_paid just succeeded on this task, and no path removes one")
        .clone();
    // A task tagged "compute" used to pay its winner in the tradeable
    // `compute` asset here, on top of the ordinary bounty. That mint is
    // gone, and its absence is the whole of this version's shape.
    //
    // `compute` was minted by settlement and by nothing else, which made
    // it look scarce. It was not: `capabilities` is a free-form tag with
    // no reserved words (`validate_capabilities` lowercases and length-
    // checks, and that is all), so anyone could post an escrow task
    // tagged `compute` from one key, claim it from a second, submit the
    // answer they had chosen themselves, and take the bounty back along
    // with an equal quantity of a supposedly scarce asset. Cost: two
    // chain fees, repeatable forever. An order book quoting ITX against
    // something anyone can print at will does not discover a price, and
    // the taker fee was charged in that same asset.
    //
    // So ITX is a marketplace for work rather than an exchange, and the
    // only thing a task pays is its bounty, in ITX, on the chain. The
    // parameter below stays because `save_confirmed_payout` is a general
    // pair-writer -- its job is that the task, the reputation and any
    // account move in one transaction, and that is still worth having
    // the day something else needs the third leg.
    let compute_account: Option<crate::board::ExchangeAccount> = None;
    let reputation = board.reputation(recipient);

    if let Err(e) = state.store.save_confirmed_payout(
        &final_task,
        recipient,
        &reputation,
        compute_account.as_ref(),
    ) {
        error!(
            "failed to record the confirmed payout for task {task_id} to {recipient}: {e} -- \
             rolling the board back so the sweep resolves it again rather than reporting a \
             payout the store never accepted"
        );
        if let Some(previous) = previous_task {
            board.restore_task(previous);
        }
        board.restore_reputation(recipient.clone(), previous_reputation);
        board.restore_exchange_account(recipient.clone(), previous_account);
        if let Some(previous) = previous_attempt {
            board.restore_payout_attempt(previous);
        }
        return false;
    }
    true
}

// ---------------------------------------------------------------------
// Exchange v1
// ---------------------------------------------------------------------

/// Reserves a fresh deposit address for funding the caller's own
/// exchange ledger balance. Unlike a task escrow there's no fixed
/// target amount to reach -- any amount at or above
/// `MIN_EXCHANGE_DEPOSIT` will later be accepted and credited in full
/// (see `confirm_exchange_deposit`).
pub async fn create_exchange_deposit(
    State(state): State<Arc<AppState>>,
    // The request as it actually arrived: bound into the signature,
    // so this envelope cannot be replayed at a different endpoint.
    method: Method,
    OriginalUri(uri): OriginalUri,
    Json(envelope): Json<SignedEnvelope<()>>,
) -> Result<Json<EscrowReservationDto>, ApiError> {
    let pubkey = envelope.verify(&state, method.as_str(), uri.path())?;
    let expires_at = Utc::now() + Duration::minutes(ESCROW_RESERVATION_TTL_MINUTES);
    let deposit = state.board.write().await.reserve_escrow(
        &state.escrow_secret,
        pubkey,
        MIN_EXCHANGE_DEPOSIT,
        EscrowPurpose::FundExchangeAccount,
        expires_at,
    );
    state.store.save_pending_deposit(&deposit).map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(EscrowReservationDto::from(&deposit)))
}

/// Checks whether `escrow_id`'s deposit address now holds at least
/// `MIN_EXCHANGE_DEPOSIT` and, if so, credits the caller's exchange
/// ledger balance with whatever actually arrived, net of the network
/// fee reserved for the eventual custody sweep. That sweep is attempted
/// once inline right after, but never gates this response -- the ledger
/// credit is durable and immediately spendable/tradeable the moment
/// this succeeds, exactly like `confirm_task_escrow` returns before its
/// task's bounty has actually moved anywhere on-chain. Only the
/// original depositor may confirm their own deposit.
pub async fn confirm_exchange_deposit(
    State(state): State<Arc<AppState>>,
    Path(escrow_id): Path<Uuid>,
    // The request as it actually arrived: bound into the signature,
    // so this envelope cannot be replayed at a different endpoint.
    method: Method,
    OriginalUri(uri): OriginalUri,
    Json(envelope): Json<SignedEnvelope<ConfirmExchangeDepositPayload>>,
) -> Result<Json<ExchangeAccountDto>, ApiError> {
    if envelope.payload.escrow_id != escrow_id {
        return Err(ApiError::BadRequest(
            "escrow id in the URL doesn't match the signed payload".into(),
        ));
    }
    let pubkey = envelope.verify(&state, method.as_str(), uri.path())?;

    let deposit_pubkey = {
        let board = state.board.read().await;
        let deposit = board.get_pending_deposit(escrow_id).ok_or(BoardError::EscrowNotFound)?;
        if deposit.depositor != pubkey {
            return Err(ApiError::Forbidden("you are not the depositor of this escrow".into()));
        }
        deposit.deposit_pubkey.clone()
    };
    let observed_amount = state
        .node
        .balance(&deposit_pubkey)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let account = {
        let mut live = state.board.write().await;
        let mut staged = TaskBoard::new();
        staged.restore_pending_deposit(live.get_pending_deposit(escrow_id).cloned().ok_or(BoardError::EscrowNotFound)?);
        staged.restore_exchange_account(pubkey.clone(), live.exchange_account(&pubkey));
        let (depositor, _) = staged.confirm_exchange_deposit(escrow_id, observed_amount, HUB_TRANSACTION_FEE, Utc::now())?;
        let account = staged.exchange_account(&depositor);
        let deposit = staged.get_pending_deposit(escrow_id).unwrap().clone();
        state.store.save_exchange_account_and_deposit(&depositor, &account, &deposit)
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        live.restore_exchange_account(depositor, account.clone());
        live.restore_pending_deposit(deposit);
        account
    };
    if sweep_exchange_deposit(&state, escrow_id).await {
        info!("swept exchange deposit {escrow_id} into pooled custody");
    }
    Ok(Json(ExchangeAccountDto::from(account)))
}

/// The taker fee owed on a batch of trades, split by the asset it was
/// charged in: compute when the taker bought, base when it sold (see
/// `board::TAKER_FEE_BPS`).
///
/// Pure, so the totals can be computed while the board's write lock is
/// held without doing anything that might want the lock itself.
fn taker_fee_totals(trades: &[Trade]) -> (u64, u64) {
    trades.iter().fold((0u64, 0u64), |(compute, base), t| match t.taker_side {
        Side::Buy => (compute + t.taker_fee, base),
        Side::Sell => (compute, base + t.taker_fee),
    })
}

/// Places a limit order against the caller's own exchange ledger
/// balance, matching immediately against any crossing resting orders --
/// see `TaskBoard::place_order`'s own doc comment for the matching and
/// lock/settle reconciliation rules.
pub async fn place_order(
    State(state): State<Arc<AppState>>,
    // The request as it actually arrived: bound into the signature,
    // so this envelope cannot be replayed at a different endpoint.
    method: Method,
    OriginalUri(uri): OriginalUri,
    Json(envelope): Json<SignedEnvelope<PlaceOrderPayload>>,
) -> Result<Json<OrderDto>, ApiError> {
    let pubkey = envelope.verify(&state, method.as_str(), uri.path())?;

    // One acquisition of the write lock covers the match, the fee, and
    // reading back everything the two touched. The fee used to be
    // credited under a *second* acquisition after the fill had already
    // been persisted, which meant both the board and the store could be
    // observed in a state where a trade existed and the fee it charged
    // did not. Nothing else may run between a fill and its fee.
    let mut live = state.board.write().await;
    let (order, trades, orders, accounts) = {
        let mut board = live.stage_order(&pubkey, envelope.payload.side, envelope.payload.price, &state.operator_public_key);
        let (order, trades) = board.place_order(
            pubkey,
            envelope.payload.side,
            envelope.payload.price,
            envelope.payload.quantity,
            Utc::now(),
        )?;

        // `TaskBoard` has no notion of an operator -- it credits whoever
        // it is handed -- so routing the fee to the hub's own account
        // stays a handler-level concern, as it always has. What changes
        // is only where it happens.
        let (compute_fees, base_fees) = taker_fee_totals(&trades);
        if compute_fees > 0 {
            board.credit_compute(&state.operator_public_key, compute_fees);
        }
        if base_fees > 0 {
            board.credit_base(&state.operator_public_key, base_fees);
        }

        // Every record the call may have moved: the placed order, every
        // resting order it matched against, every counterparty, and the
        // fee sink when it earned anything. Read back under the same
        // lock that produced them, so the set handed to the store is one
        // internally consistent snapshot rather than several.
        let mut touched_orders: BTreeSet<Uuid> = BTreeSet::new();
        let mut touched_accounts: BTreeSet<PublicKey> = BTreeSet::new();
        for trade in &trades {
            touched_orders.insert(trade.buy_order_id);
            touched_orders.insert(trade.sell_order_id);
            touched_accounts.insert(trade.buyer.clone());
            touched_accounts.insert(trade.seller.clone());
        }
        touched_orders.remove(&order.id);
        // The placer's account, always, and not only when they appear in
        // a trade. Placing an order *always* moves locked balance, so an
        // order that crossed nothing still changes its owner's account --
        // and the old code, which built this set from trades alone, wrote
        // the order without it. The lock then existed in memory and
        // nowhere else: a restart reloaded an open order with no locked
        // funds behind it, free to fill against money its owner was
        // meanwhile at liberty to spend or withdraw twice.
        //
        // It was invisible because the case is masked whenever the same
        // account also trades in the same call, which is what every test
        // and every hand-run example did. `exchange-restart`'s kill phase
        // is what surfaced it.
        touched_accounts.insert(order.owner.clone());
        if compute_fees > 0 || base_fees > 0 {
            touched_accounts.insert(state.operator_public_key.clone());
        }

        let mut orders = vec![order.clone()];
        orders.extend(touched_orders.into_iter().filter_map(|id| board.get_order(id).cloned()));
        let accounts: Vec<(PublicKey, ExchangeAccount)> = touched_accounts
            .into_iter()
            .map(|pk| {
                let account = board.exchange_account(&pk);
                (pk, account)
            })
            .collect();

        (order, trades, orders, accounts)
    };

    // One commit for the whole fill, and a 500 rather than a 200 if it
    // fails. See `HubStore::save_fill` for what the old seven commits
    // could leave on disk.
    state
        .store
        .save_fill(&orders, &trades, &accounts)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    for item in orders { live.restore_order(item); }
    for trade in trades { live.restore_trade(trade); }
    for (pk, account) in accounts { live.restore_exchange_account(pk, account); }
    Ok(Json(OrderDto::from(&order)))
}

/// Cancels an open (or partially filled) order, releasing whatever
/// remains of its locked balance back to the caller. Only the order's
/// own owner may cancel it.
pub async fn cancel_order(
    State(state): State<Arc<AppState>>,
    Path(order_id): Path<Uuid>,
    // The request as it actually arrived: bound into the signature,
    // so this envelope cannot be replayed at a different endpoint.
    method: Method,
    OriginalUri(uri): OriginalUri,
    Json(envelope): Json<SignedEnvelope<CancelOrderPayload>>,
) -> Result<Json<OrderDto>, ApiError> {
    if envelope.payload.order_id != order_id {
        return Err(ApiError::BadRequest(
            "order id in the URL doesn't match the signed payload".into(),
        ));
    }
    let pubkey = envelope.verify(&state, method.as_str(), uri.path())?;
    // The cancelled order and the balance its cancellation released are
    // read out under the same write lock that produced them, so the pair
    // handed to the store is internally consistent: reacquiring the lock
    // afterwards, as this handler used to, could persist whatever a
    // concurrent caller had left behind in between. Same reasoning as the
    // escrow confirm handlers (§6.5b).
    let mut live = state.board.write().await;
    let mut staged = TaskBoard::new();
    staged.restore_order(live.get_order(order_id).cloned().ok_or(BoardError::OrderNotFound)?);
    staged.restore_exchange_account(pubkey.clone(), live.exchange_account(&pubkey));
    let order = staged.cancel_order(order_id, &pubkey)?;
    let account = staged.exchange_account(&pubkey);
    state.store.save_order_and_account(&order, &pubkey, &account)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    live.restore_order(order.clone());
    live.restore_exchange_account(pubkey, account);
    Ok(Json(OrderDto::from(&order)))
}

pub async fn get_order_book(State(state): State<Arc<AppState>>) -> Json<OrderBookDto> {
    let board = state.board.read().await;
    let (bids, asks) = board.order_book();
    Json(OrderBookDto {
        bids: bids.into_iter().map(OrderDto::from).collect(),
        asks: asks.into_iter().map(OrderDto::from).collect(),
    })
}

pub async fn get_exchange_account(
    State(state): State<Arc<AppState>>,
    Path(pubkey_hex): Path<String>,
) -> Result<Json<ExchangeAccountDto>, ApiError> {
    let pubkey = parse_hex_pubkey(&pubkey_hex)?;
    let account = state.board.read().await.exchange_account(&pubkey);
    Ok(Json(ExchangeAccountDto::from(account)))
}

/// `amount` is the total ledger debit, including the network fee. The
/// recipient receives amount - fee. A durable pending payment is returned;
/// GET /payments/:id reports confirmation or the need for operator review.
pub async fn withdraw(
    State(state): State<Arc<AppState>>, method: Method, OriginalUri(uri): OriginalUri,
    Json(envelope): Json<SignedEnvelope<WithdrawPayload>>,
) -> Result<Json<ExchangeAccountDto>, ApiError> {
    let pubkey = envelope.verify(&state, method.as_str(), uri.path())?;
    let debit = envelope.payload.amount;
    if debit < MIN_EXCHANGE_WITHDRAWAL {
        return Err(ApiError::BadRequest(format!("withdrawal must be at least {MIN_EXCHANGE_WITHDRAWAL}; amount includes the {HUB_TRANSACTION_FEE} fee")));
    }
    let _guard = state.exchange_custody_payout_lock.lock().await;
    let account = state.board.read().await.exchange_account(&pubkey);
    let available = account.base_balance.saturating_sub(account.locked_base);
    if available < debit { return Err(BoardError::InsufficientBalance { available, required: debit }.into()); }
    let net = debit - HUB_TRANSACTION_FEE;
    let tx = build_payment_from_fresh(&state, &state.exchange_custody_private_key,
        &state.exchange_custody_public_key, &[(pubkey.clone(), net)],
        &state.exchange_custody_public_key).await
        .map_err(|e| ApiError::Internal(format!("withdrawal could not be built, nothing was sent: {e}")))?;
    let payment = crate::payments::Payment::new(crate::payments::Purpose::Withdrawal { debited: debit },
        state.exchange_custody_public_key.clone(), pubkey.clone(), net, Some(tx));
    let payment = crate::payments::prepare(&state, payment).await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    crate::payments::send(&state, &payment).await;
    let mut account = ExchangeAccountDto::from(state.board.read().await.exchange_account(&pubkey));
    account.payment = Some(payment.receipt());
    Ok(Json(account))
}

/// Public payment status; the signed transaction is never exposed by this route.
pub async fn get_payment(State(state): State<Arc<AppState>>, Path(id): Path<Uuid>)
    -> Result<Json<crate::payments::Receipt>, ApiError> {
    state.board.read().await.payments.get(&id).map(|p| Json(p.receipt()))
        .ok_or_else(|| ApiError::NotFound("payment not found".into()))
}

#[derive(Deserialize)]
pub struct PaymentsQuery {
    pub recipient: String,
    #[serde(default)] pub offset: usize,
    pub limit: Option<usize>,
}

/// Recover a receipt after a lost HTTP response without repeating the spend.
pub async fn list_payments(State(state): State<Arc<AppState>>, Query(q): Query<PaymentsQuery>)
    -> Result<Json<Vec<crate::payments::Receipt>>, ApiError> {
    let pk = parse_hex_pubkey(&q.recipient)?;
    let board = state.board.read().await;
    let mut found: Vec<_> = board.payments.values().filter(|p| p.recipient == pk).collect();
    found.sort_by_key(|p| (std::cmp::Reverse(p.created_at), p.id));
    Ok(Json(found.into_iter().skip(q.offset).take(q.limit.unwrap_or(50).min(200)).map(|p| p.receipt()).collect()))
}

/// Lists every executed trade, newest first, paginated via
/// `?offset=&limit=` -- the "continuous pricing" feed the exchange
/// exists to provide. Includes both counterparties (see `TradeDto`).
pub async fn list_trades(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ListTradesQuery>,
) -> Json<Vec<TradeDto>> {
    let limit = query.limit.unwrap_or(DEFAULT_TRADES_PAGE_SIZE).min(MAX_TRADES_PAGE_SIZE);
    let board = state.board.read().await;
    let trades: Vec<TradeDto> =
        board.all_trades_newest_first().into_iter().skip(query.offset).take(limit).map(TradeDto::from).collect();
    Json(trades)
}

/// Issues a proof-of-work challenge for the calling key.
///
/// Signed, so the challenge is bound to a key the caller has proved it
/// holds rather than one it merely named -- otherwise anyone could burn
/// a victim's one-outstanding-challenge slot. Cheap: no node round trip,
/// one random nonce and one durable write, which is why it sits in the
/// ordinary write tier rather than the chain tier the grant itself uses.
pub async fn faucet_challenge(
    State(state): State<Arc<AppState>>,
    client_ip: Option<axum::extract::Extension<crate::rate_limit::ClientIp>>,
    method: Method,
    OriginalUri(uri): OriginalUri,
    Json(envelope): Json<SignedEnvelope<()>>,
) -> Result<Json<FaucetChallengeDto>, ApiError> {
    let pubkey = envelope.verify(&state, method.as_str(), uri.path())?;

    // Refuse before issuing rather than after solving. A key that has
    // already been granted cannot be granted again, so letting it spend
    // a minute of CPU first would be a rude way to say no.
    if !state.board.read().await.can_claim_faucet(&pubkey) {
        return Err(ApiError::Conflict(
            "this pubkey has already claimed a faucet grant".into(),
        ));
    }
    if let Some(refusal) = faucet_budget_exhausted(&state).await {
        return Err(refusal);
    }
    // One address, read once, used for both halves: the price quoted and
    // the network the resulting grant will be counted against. Reading
    // it twice -- here and again at redemption -- is what let a caller
    // ask over one network and claim over another, so the prefix that
    // was quoted the price never accumulated a count.
    let prefix = client_ip.as_ref().map(|c| crate::rate_limit::prefix_of(c.0 .0));
    let expected = faucet_difficulty_for(&state, prefix.as_deref()).await;
    let challenge = state.faucet_challenges.issue_at_target(
        &pubkey,
        crate::faucet_pow::target_for_expected_hashes(expected),
        prefix,
        Utc::now(),
    )?;
    Ok(Json(faucet_challenge_dto(&state, &challenge)))
}

/// Renders a challenge for the wire. Shared with `faucet_claim`, which
/// attaches one to the 503 it sends when the grant cannot be funded --
/// two places building this by hand is two chances for the replacement
/// challenge to differ in some field from the one `/faucet/challenge`
/// serves, which a client would experience as the faucet occasionally
/// handing out an unsolvable puzzle.
fn faucet_challenge_dto(
    state: &AppState,
    challenge: &crate::faucet_pow::Challenge,
) -> FaucetChallengeDto {
    FaucetChallengeDto {
        challenge_id: challenge.id,
        server_nonce: challenge.server_nonce.clone(),
        pubkey: challenge.pubkey.clone(),
        action: challenge.action.clone(),
        target: challenge.target_hex(),
        issued_at: DateTime::from_timestamp(challenge.issued_at, 0).unwrap_or_else(Utc::now),
        expires_at: DateTime::from_timestamp(challenge.expires_at, 0).unwrap_or_else(Utc::now),
        // From this challenge, not from the hub's base setting: once the
        // price varies per network, quoting the base would tell an agent
        // behind a busy one to budget for a fraction of what it is about
        // to spend.
        expected_hashes: crate::faucet_pow::expected_hashes_for_target(challenge.target),
        preimage_template: challenge.preimage_template(),
    }
}

pub async fn faucet_claim(
    State(state): State<Arc<AppState>>,
    // No `ClientIp` here any more. The network a grant is counted
    // against comes from the challenge being redeemed, not from where
    // the redemption arrived -- reading the address twice is what let
    // the two disagree. Per-IP rate limiting still happens, in the
    // middleware, where it always did.
    //
    // The request as it actually arrived: bound into the signature,
    // so this envelope cannot be replayed at a different endpoint.
    method: Method,
    OriginalUri(uri): OriginalUri,
    Json(envelope): Json<SignedEnvelope<FaucetClaimPayload>>,
) -> Result<Json<FaucetResultDto>, ApiError> {
    let pubkey = envelope.verify(&state, method.as_str(), uri.path())?;

    // Validate the challenge before asking whether the grant can be
    // funded, so a caller presenting a spent or unsolved one is told
    // that rather than told the hub is busy. Spends nothing: `redeem`
    // below runs the same checks again for real.
    state.faucet_challenges.check(
        envelope.payload.challenge_id,
        &pubkey,
        envelope.payload.solution,
        Utc::now(),
    )?;

    // Ask whether the grant can be funded *before* spending the
    // challenge. This is the difference between "we cannot pay you
    // right now" costing the agent one cheap request and costing it the
    // fourteen seconds of proof-of-work it has just finished -- and
    // under the ceiling §6.4b measured, this path was the one taken 95%
    // of the time.
    //
    // Deliberately a read outside `payout_lock`, so it is advisory
    // rather than a reservation. Being wrong in the optimistic
    // direction costs nothing that is not already handled below; being
    // wrong in the pessimistic direction costs a retry and no work. A
    // reservation would mean holding the operator's payout lock across
    // an agent's whole redemption, which is the one thing the wallet
    // fan-out exists to stop being a queue.
    //
    // Total spendable, not `balance - allocated_bounty()`: a faucet
    // grant has never been netted against posted bounties, and starting
    // here would refuse grants that succeed today for a reason nothing
    // on the wire explains. Whether the operator's own posts should
    // outrank the faucet is §3.4's question, not this one's.
    if !operator_can_fund_a_grant(&state).await {
        return Err(grant_unfunded(&state, &pubkey, GrantUnfunded::BeforeRedemption).await);
    }

    // Spend the challenge first, and durably. Everything after this
    // point can fail and be retried; this cannot, because a solution the
    // hub forgets is a solution that can be presented again. See
    // `HubStore::save_faucet_challenge` for why this write is ordered
    // before the payout while the grant record is ordered after it.
    let redeemed = state.faucet_challenges.redeem(
        envelope.payload.challenge_id,
        &pubkey,
        envelope.payload.solution,
        Utc::now(),
    )?;

    let _guard = state.payout_lock.lock().await;
    if !state.board.read().await.can_claim_faucet(&pubkey) {
        return Err(ApiError::Conflict("this pubkey already has a faucet grant or pending payment".into()));
    }
    // Before the challenge is spent, so an agent refused here keeps the
    // work it has already done and can present it when the window rolls.
    if let Some(refusal) = faucet_budget_exhausted(&state).await {
        return Err(refusal);
    }
    let tx = match build_payment_from_fresh(&state, &state.operator_private_key,
        &state.operator_public_key, &[(pubkey.clone(), FAUCET_GRANT_AMOUNT)],
        &state.operator_public_key).await {
        Ok(tx) => tx,
        Err(e) if is_insufficient_funds(&e) => return Err(grant_unfunded(&state, &pubkey,
            GrantUnfunded::AfterRedemption(e.to_string())).await),
        Err(e) => return Err(ApiError::Internal(format!("faucet not sent: {e}"))),
    };
    let payment = crate::payments::Payment::new(crate::payments::Purpose::Faucet,
        state.operator_public_key.clone(), pubkey, FAUCET_GRANT_AMOUNT, Some(tx));
    let payment = crate::payments::prepare(&state, payment).await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    // Recorded against the network the *challenge* was priced for, not
    // the one this claim arrived from. Those used to be read separately
    // and that was the hole: ask for a challenge over IPv6 and redeem it
    // over IPv4 and the prefix that was quoted the base price never
    // accumulated a count, so it stayed at the base price for every key
    // after it. The curve was decorative. Now the two halves cannot
    // disagree, because there is only one reading of one address.
    //
    // After the grant is reserved, so a refused claim records nothing.
    // Best-effort: a prefix the hub failed to write costs this network
    // one unit of accounting, while failing the claim over it would cost
    // an agent a grant it has already paid for in work.
    if let Some(prefix) = redeemed.prefix.as_deref() {
        state.board.write().await.record_faucet_grant_prefix(payment.recipient.clone(), prefix.to_string());
        if let Err(e) = state.store.save_faucet_grant_prefix(&payment.recipient, prefix) {
            warn!("could not record the network a faucet grant was priced for: {e}");
        }
    }
    crate::payments::send(&state, &payment).await;
    Ok(Json(FaucetResultDto { amount: payment.amount, payment_id: payment.id, status: payment.status }))
}

/// How many grants the global budget still allows, and the window it is
/// measured over.
///
/// Rolling twenty-four hours rather than a calendar day: a budget that
/// resets at midnight teaches an attacker to wait for midnight, and a
/// legitimate cohort arriving just after a reset gets a different answer
/// than one arriving just before it for no reason anyone can see.
pub const FAUCET_BUDGET_WINDOW_SECONDS: i64 = 24 * 60 * 60;

/// What the next grant to `ip`'s network costs, in expected hashes.
///
/// **A price, not a refusal.** This used to be a flat cap, which turned
/// away the sixth agent behind a university NAT exactly as firmly as the
/// sixth sock puppet -- and from one address those two are the same
/// picture, so the cap excluded both. Pricing separates them on the one
/// axis where they actually differ: a farm wants many identities cheaply,
/// a shared network wants a few each and never reaches the steep part of
/// the curve.
///
/// See `faucet_pow::expected_hashes_for_prefix` for the curve itself and
/// for what it still does not fix.
/// Takes the prefix rather than the address, so the caller is the one
/// place that turns an address into a network -- and the same value it
/// prices against is the one it stamps on the challenge.
async fn faucet_difficulty_for(state: &AppState, prefix: Option<&str>) -> u64 {
    // No address means the middleware did not run, which happens only in
    // tests calling a handler directly. Charging base rate there is the
    // right failure: it is the price everyone pays before any history.
    let Some(prefix) = prefix else { return state.faucet_expected_hashes };
    let cutoff = (Utc::now() - Duration::seconds(FAUCET_BUDGET_WINDOW_SECONDS)).timestamp();
    let taken = state.board.read().await.faucet_granted_from_prefix_since(prefix, cutoff);
    crate::faucet_pow::expected_hashes_for_prefix(
        state.faucet_expected_hashes,
        taken,
        state.faucet_free_grants_per_prefix,
        state.faucet_pow_doubling_grants,
    )
}

async fn faucet_budget_remaining(state: &AppState) -> u64 {
    let cutoff = (Utc::now() - Duration::seconds(FAUCET_BUDGET_WINDOW_SECONDS)).timestamp();
    let spent = state.board.read().await.faucet_granted_since(cutoff);
    state.faucet_daily_grants.saturating_sub(spent)
}

/// Refuses when the hub has already given away its day's worth.
///
/// Checked at *both* ends of the flow, which is not redundant. At
/// `/faucet/challenge` it saves an agent the proof of work it would
/// otherwise spend before being told no -- the same courtesy
/// `can_claim_faucet` is checked there for. At `/faucet` it is the one
/// that actually binds, because a budget can be exhausted by other
/// claimants during the minute an agent spends solving.
async fn faucet_budget_exhausted(state: &AppState) -> Option<ApiError> {
    if faucet_budget_remaining(state).await > 0 {
        return None;
    }
    Some(ApiError::Unavailable {
        message: format!(
            "the faucet has made its {} grants for the last {} hours; it refills as older \
             grants age out of the window",
            state.faucet_daily_grants,
            FAUCET_BUDGET_WINDOW_SECONDS / 3600
        ),
        // An hour, not the whole window: grants age out continuously,
        // so the budget is very likely to have room again long before a
        // full day has passed.
        retry_after: 3600,
        extra: serde_json::Map::new(),
    })
}

/// Whether the operator can fund one faucet grant out of what is
/// confirmed and unspoken-for right now. See `faucet_claim` for why this
/// is advisory.
async fn operator_can_fund_a_grant(state: &AppState) -> bool {
    match state.node.balance(&state.operator_public_key).await {
        Ok(balance) => balance >= FAUCET_GRANT_AMOUNT + HUB_TRANSACTION_FEE,
        // A node we cannot reach is not a wallet we know to be empty.
        // Let the claim proceed and fail on the real attempt, which
        // reports the actual error rather than inventing a shortage.
        Err(e) => {
            warn!("could not check the operator's balance before a faucet grant: {e}");
            true
        }
    }
}

/// Whether a payment failed because the source had nothing to spend, as
/// opposed to any of the other ways it can fail.
///
/// A string match, because `pay_from` returns `anyhow::Error` and the
/// one thing that distinguishes this case -- `PaymentError::
/// InsufficientFunds` -- is several `?`s down inside it. Narrow enough
/// to be safe: the alternative is a 500 that says the same words, so a
/// miss costs the old behaviour rather than a wrong one.
fn is_insufficient_funds(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<btclib::payment::PaymentError>()
        .is_some_and(|e| matches!(e, btclib::payment::PaymentError::InsufficientFunds { .. }))
}

/// Which side of the redemption the shortage was found on. The two need
/// different answers, and getting that backwards is what would make the
/// hub either waste an agent's work or hand out a replacement challenge
/// while the original is still perfectly good.
enum GrantUnfunded {
    /// Caught by the pre-flight, so nothing was spent. The agent's
    /// challenge is untouched and its solution is still worth
    /// presenting, so it must *not* be replaced: `ChallengeBook::issue`
    /// evicts whatever that key had outstanding, which would throw away
    /// the very work this branch exists to protect.
    BeforeRedemption,
    /// The pre-flight passed and the payment failed anyway -- a slot
    /// taken in between, which is what a burst looks like. The
    /// challenge is spent and cannot be unspent, so a fresh one is the
    /// only thing left to give back.
    AfterRedemption(String),
}

/// The answer to "the faucet cannot pay you at this instant": a 503 with
/// a time to come back, and where the work is gone, something to start
/// again from.
///
/// A 503 and not the 500 this used to send. The request was correct, the
/// caller is not at fault, and the condition is temporary by
/// construction -- the operator's change confirms in a block, which is
/// what `FAUCET_RETRY_AFTER_SECONDS` says. A 500 reading "please retry"
/// told an agent none of that and nothing about how long, which is the
/// same gap §3.4 records for the rate limits.
///
/// The replacement challenge is a courtesy, not a refund: the work is
/// gone either way. It saves a round trip to `/faucet/challenge` and
/// says plainly that the hub expects the agent back. If issuing it
/// fails, the 503 goes out without it.
async fn grant_unfunded(
    state: &AppState,
    pubkey: &PublicKey,
    when: GrantUnfunded,
) -> ApiError {
    let mut extra = serde_json::Map::new();
    let message = match when {
        GrantUnfunded::BeforeRedemption =>
            "the faucet cannot fund a grant at this instant; nothing was spent, so present the same solution again after the retry interval".to_string(),
        GrantUnfunded::AfterRedemption(cause) => {
            match state.faucet_challenges.issue(pubkey, Utc::now()) {
                Ok(challenge) => {
                    extra.insert(
                        "challenge".into(),
                        serde_json::to_value(faucet_challenge_dto(state, &challenge))
                            .unwrap_or(serde_json::Value::Null),
                    );
                }
                Err(e) => warn!("could not issue a replacement faucet challenge for {pubkey}: {e}"),
            }
            format!("the faucet could not fund a grant ({cause}); your challenge was spent, so a fresh one is attached at no further cost")
        }
    };
    ApiError::Unavailable {
        message,
        retry_after: FAUCET_RETRY_AFTER_SECONDS,
        extra,
    }
}

pub async fn get_reputation(
    State(state): State<Arc<AppState>>,
    Path(pubkey_hex): Path<String>,
) -> Result<Json<ReputationDto>, ApiError> {
    let pubkey = parse_hex_pubkey(&pubkey_hex)?;
    let reputation = {
        let board = state.board.read().await;
        board.reputation(&pubkey)
    };
    let mut dto = ReputationDto::from(reputation);
    dto.net_worth = state.node.balance(&pubkey).await.ok();
    // Read-only lookup, never an assignment. This route is
    // unauthenticated and resolves *any* well-formed pubkey (the
    // dashboard's agent page is built on that), so minting a name here
    // would let an anonymous caller drain the pool one GET at a time.
    // Names are minted where an agent is actually known to the economy:
    // the startup backfill and `leaderboard`, both of which work from
    // the board's own reputation records.
    dto.name = state.names.read().await.get(&pubkey).map(str::to_string);
    Ok(Json(dto))
}

/// How many agents one leaderboard request returns by default, and the
/// most it will return however large a `limit` is asked for. Held at the
/// same fifty the route served before it took a page, since that is what
/// the balance fan-out below is sized for: a page is one node lookup per
/// agent, and the ceiling is what stops a single request opening an
/// unbounded number of them.
const DEFAULT_LEADERBOARD_PAGE_SIZE: usize = 50;
const MAX_LEADERBOARD_PAGE_SIZE: usize = 50;

#[derive(Deserialize)]
pub struct LeaderboardQuery {
    pub offset: Option<usize>,
    pub limit: Option<usize>,
    /// Case-insensitive substring, matched against an agent's assigned
    /// name and its hex pubkey. Absent or blank searches nothing and
    /// returns the field in order.
    pub q: Option<String>,
    /// Which column ranks the field: `earned` (the default),
    /// `completed`, `failed` or `net_worth`. Unknown values fall back to
    /// `earned` rather than failing the request -- a leaderboard that
    /// 400s because a client sent a column it does not know is worse
    /// than one that answers in its default order.
    ///
    /// **`net_worth` is the expensive one, and is priced differently
    /// because of it.** The other three are in the reputation map the
    /// board already holds, so ranking by one costs a sort of memory.
    /// Net worth is a live balance the node answers for, one lookup per
    /// agent, and ranking a field by a number means holding that number
    /// for every agent in it -- thousands of connections to order fifty
    /// rows, and thousands again when the reader turns the page. So the
    /// field is priced in one bounded sweep and the result held for
    /// `NET_WORTH_SNAPSHOT_TTL_SECS`; see `net_worth_snapshot`.
    pub sort: Option<String>,
    /// `desc` (the default) or `asc`. Ascending is what makes `failed`
    /// worth sorting in both directions -- "fewest failures" is a real
    /// question about an agent, and "most" is the same question asked
    /// the other way.
    pub dir: Option<String>,
}

/// The columns the field can be ranked by, and the order it is ranked
/// in. Parsed from the query rather than trusted: see `LeaderboardQuery`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LeaderboardSort {
    Earned,
    Completed,
    Failed,
    /// The one column that is not in the board's own memory. Ranking by
    /// it prices the whole field first -- see `net_worth_snapshot`.
    NetWorth,
}

impl LeaderboardSort {
    fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::trim) {
            Some("completed") => Self::Completed,
            Some("failed") => Self::Failed,
            Some("net_worth") => Self::NetWorth,
            _ => Self::Earned,
        }
    }

    /// The figure this column ranks on, for one agent -- for the three
    /// columns the reputation map answers. `NetWorth` is not one of
    /// them, and returns nothing rather than a plausible zero: the
    /// caller ranks it from the sweep instead, and a silent 0 here would
    /// order the field by "no data" and look like a field of paupers.
    fn key(self, reputation: &Reputation) -> Option<u64> {
        match self {
            Self::Earned => Some(reputation.total_earned),
            Self::Completed => Some(reputation.completed),
            Self::Failed => Some(reputation.failed),
            Self::NetWorth => None,
        }
    }
}

/// How long one net-worth sweep is reused before the field is priced
/// again.
///
/// Ranking by net worth needs a balance for every agent in the field,
/// one node lookup each, and a reader who ranks by it then pages through
/// the result would pay for the whole field on every page. Thirty
/// seconds is long enough to cover that reading, and short enough that
/// the figure in the column is still the one the chain would give you --
/// balances move when a task settles, which on this network is a matter
/// of blocks, not seconds.
const NET_WORTH_SNAPSHOT_TTL_SECS: u64 = 30;

/// How many balance lookups a sweep has in flight at once. `NodeClient`
/// opens a connection per call, so an unbounded sweep of a field of
/// thousands would try to open thousands of sockets at once -- which is
/// not faster than a bounded one, and is a good way to be refused by the
/// node or by the OS. Sized above the fifty a page already fans out to,
/// since a sweep is the one call that is allowed to take a moment.
const NET_WORTH_SWEEP_CONCURRENCY: usize = 64;

/// Orders two agents on the figure being ranked, before the pubkey
/// tiebreak.
///
/// **An agent with no figure ranks last in both directions.** Only net
/// worth can be missing (the node could not be reached for that agent),
/// and last-either-way is the one position that makes no claim about it.
/// Ranking it as zero would head "fewest first" with an agent nobody
/// could reach and call it the poorest on the board; ranking it as the
/// largest would crown it the richest. Neither is known.
fn compare_for_ranking(left: Option<u64>, right: Option<u64>, ascending: bool) -> std::cmp::Ordering {
    match (left, right) {
        (Some(left), Some(right)) => {
            if ascending {
                left.cmp(&right)
            } else {
                right.cmp(&left)
            }
        }
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// Every agent's balance, priced in one bounded sweep and reused for
/// `NET_WORTH_SNAPSHOT_TTL_SECS`.
///
/// An agent the node could not answer for is **absent** rather than
/// zero, the same distinction `net_worth: null` makes in the response:
/// "we could not reach the chain for this one" is not "this one is
/// broke". The ranking puts those agents last whichever way the column
/// is sorted -- see the comparator in `leaderboard`.
///
/// The refresh happens under the write lock, and re-checks freshness
/// after taking it: two readers arriving together on a stale snapshot
/// would otherwise both sweep the field, which is precisely the fan-out
/// this exists to avoid. The second one waits and then finds the first
/// one's answer.
async fn net_worth_snapshot(state: &AppState, field: &[PublicKey]) -> HashMap<String, u64> {
    if let Some((taken_at, snapshot)) = state.net_worths.read().await.as_ref() {
        if taken_at.elapsed().as_secs() < NET_WORTH_SNAPSHOT_TTL_SECS {
            return snapshot.clone();
        }
    }

    let mut cached = state.net_worths.write().await;
    if let Some((taken_at, snapshot)) = cached.as_ref() {
        if taken_at.elapsed().as_secs() < NET_WORTH_SNAPSHOT_TTL_SECS {
            return snapshot.clone();
        }
    }

    let permits = Arc::new(tokio::sync::Semaphore::new(NET_WORTH_SWEEP_CONCURRENCY));
    let mut lookups = tokio::task::JoinSet::new();
    for pubkey in field {
        let node = state.node.clone();
        let pubkey = pubkey.clone();
        let permits = permits.clone();
        lookups.spawn(async move {
            // The semaphore is never closed, so the only way to fail
            // here is a bug; `ok()?` keeps that from taking the sweep
            // down with it.
            let _permit = permits.acquire().await.ok()?;
            Some((pubkey.to_string(), node.balance(&pubkey).await.ok()?))
        });
    }
    let mut snapshot: HashMap<String, u64> = HashMap::new();
    while let Some(result) = lookups.join_next().await {
        if let Ok(Some((pubkey_hex, balance))) = result {
            snapshot.insert(pubkey_hex, balance);
        }
    }
    *cached = Some((std::time::Instant::now(), snapshot.clone()));
    snapshot
}

/// Whether an agent answers to `needle`, which the caller has already
/// lowercased.
///
/// Substring rather than prefix on both halves, for different reasons.
/// A name is two words joined without a separator (`SwiftWarlock`), so
/// prefix matching would find it by "swift" and not by "warlock", and a
/// reader who remembers one of the two words has no way to know which
/// half they are holding. A pubkey is searched by whichever fragment the
/// reader has in front of them -- often the truncated tail a table
/// showed them, never the whole 66 characters.
fn agent_matches(needle: &str, pubkey_hex: &str, name: Option<&str>) -> bool {
    pubkey_hex.to_lowercase().contains(needle) || name.is_some_and(|n| n.to_lowercase().contains(needle))
}

/// A page of agents by lifetime earnings, alongside each one's *current*
/// on-chain balance (`net_worth`) -- a separate, live figure, not
/// derivable from anything already stored in `board`.
///
/// Paged, where this used to serve a flat top fifty and nothing else.
/// Fifty is the whole field on a small board and a rounding error on a
/// real one, and a leaderboard that silently stops at fiftieth place is
/// answering a different question than the one being asked of it. The
/// full count rides in `X-Total-Count`, the same header `list_tasks`
/// uses, so a caller can size a pager without walking the pages.
///
/// Balances are fetched from the node concurrently, one connection per
/// pubkey (`NodeClient` is deliberately cheap to clone and reconnect
/// with, see its own doc comment) rather than sequentially, so this
/// doesn't take 50x as long as a single lookup. A pubkey whose lookup
/// fails just gets `net_worth: null` in the response instead of failing
/// the whole request -- the same "don't let one flaky call take down an
/// unrelated read" posture `try_settle_verified_task` already takes with
/// the node.
///
/// `q` searches the **whole field** by name or pubkey, which is the
/// half a client cannot do for itself: filtering a page of fifty
/// searches fifty agents and calls it a board. Matching happens after
/// the ranking and before the slice, so each entry keeps the `rank` it
/// holds among all agents and the pager sizes to the matches rather than
/// to the field.
pub async fn leaderboard(State(state): State<Arc<AppState>>, Query(query): Query<LeaderboardQuery>) -> Response {
    let limit = query.limit.unwrap_or(DEFAULT_LEADERBOARD_PAGE_SIZE).min(MAX_LEADERBOARD_PAGE_SIZE);
    let offset = query.offset.unwrap_or(0);
    let needle = query.q.as_deref().map(str::trim).filter(|q| !q.is_empty()).map(str::to_lowercase);
    let sort = LeaderboardSort::parse(query.sort.as_deref());
    let ascending = query.dir.as_deref().map(str::trim) == Some("asc");

    // The whole ranking, then the slice asked for. Ranking is what makes
    // a leaderboard a leaderboard, so it cannot be done per page.
    let mut ranked = {
        let board = state.board.read().await;
        board.leaderboard(usize::MAX)
    };

    // Priced only when net worth is what the field is being ranked by,
    // and priced **outside the board lock**: this one talks to the node,
    // and a read of the board should never wait on the chain. The
    // snapshot then does double duty below -- ranking the field and
    // filling the column -- so the order a reader sees and the figures
    // they see it ordered by are the same numbers, which they would not
    // be if the page re-fetched them a moment later.
    let sweep = match sort {
        LeaderboardSort::NetWorth => {
            let field: Vec<PublicKey> = ranked.iter().map(|(pubkey, _)| pubkey.clone()).collect();
            Some(net_worth_snapshot(&state, &field).await)
        }
        _ => None,
    };

    // Re-ranked here rather than in `board.leaderboard`, which keeps
    // meaning "the field by earnings" for its other caller (the startup
    // backfill).
    //
    // **The pubkey is the tiebreak, and it is load-bearing.** The columns
    // other than earnings tie constantly -- most of the field has
    // completed 0 and failed 0, and an agent the node could not price
    // has no net worth at all -- and a page is a slice of this order.
    // Without a total order, two agents that tie could swap places
    // between the request for page 1 and the request for page 2, and an
    // agent would be served twice or not at all.
    if sort != LeaderboardSort::Earned || ascending {
        ranked.sort_by(|a, b| {
            // `None` only ever means "the node could not price this
            // agent". The three reputation columns always answer.
            let figure = |pubkey: &PublicKey, reputation: &Reputation| match &sweep {
                Some(snapshot) => snapshot.get(&pubkey.to_string()).copied(),
                None => sort.key(reputation),
            };
            let (left, right) = (figure(&a.0, &a.1), figure(&b.0, &b.1));
            compare_for_ranking(left, right, ascending)
                .then_with(|| a.0.to_string().cmp(&b.0.to_string()))
        });
    }

    // Ranks are attached here, over the unfiltered order, so a search
    // reports where an agent actually stands. Names come from a
    // read-only borrow of the registry: an agent the registry has never
    // named is still searchable by key, and naming anything at this
    // point would mint from the pool on behalf of an anonymous caller
    // (see `names`).
    let (total, entries) = {
        let registry = state.names.read().await;
        // Collected before slicing because the total is the count of
        // matches, and a lazy filter has no length until it has been run.
        let matching: Vec<(usize, (PublicKey, Reputation))> = ranked
            .into_iter()
            .enumerate()
            .filter(|(_, (pubkey, _))| match &needle {
                None => true,
                Some(needle) => agent_matches(needle, &pubkey.to_string(), registry.get(pubkey)),
            })
            .collect();
        let total = matching.len();
        (total, matching.into_iter().skip(offset).take(limit).collect::<Vec<_>>())
    };

    // A sweep has already priced everybody, so the page costs nothing
    // further; otherwise the page is priced on its own, which is the
    // fifty lookups this route has always made.
    let net_worths: HashMap<String, u64> = match sweep {
        Some(snapshot) => snapshot,
        None => {
            let mut lookups = tokio::task::JoinSet::new();
            for (_, (pubkey, _)) in &entries {
                let node = state.node.clone();
                let pubkey = pubkey.clone();
                lookups.spawn(async move {
                    let balance = node.balance(&pubkey).await.ok();
                    (pubkey.to_string(), balance)
                });
            }
            let mut priced: HashMap<String, u64> = HashMap::new();
            while let Some(result) = lookups.join_next().await {
                if let Ok((pubkey_hex, Some(balance))) = result {
                    priced.insert(pubkey_hex, balance);
                }
            }
            priced
        }
    };

    // Every pubkey here came from the board's reputation map, so each is
    // an agent that has actually done something -- which is what makes
    // minting a name at read time safe. Startup already named everyone
    // it found, so in the steady state this assigns nothing and writes
    // nothing; it exists to catch agents that first appeared since the
    // hub came up.
    let names = state.ensure_named(entries.iter().map(|(_, (pubkey, _))| pubkey.clone())).await;

    let page: Vec<LeaderboardEntryDto> = entries
        .into_iter()
        .map(|(index, (pubkey, reputation))| {
            let pubkey_hex = pubkey.to_string();
            let mut reputation = ReputationDto::from(reputation);
            reputation.net_worth = net_worths.get(&pubkey_hex).copied();
            reputation.name = names.get(&pubkey_hex).cloned();
            // `enumerate` counted from the ranking, so this is a
            // position in the field and survives both the filter and
            // the slice. One-based, because it is shown to people.
            LeaderboardEntryDto { pubkey: pubkey_hex, rank: index + 1, reputation }
        })
        .collect();

    ([("x-total-count", total.to_string())], Json(page)).into_response()
}

/// How many pubkeys one `names` request may ask about. Sized for a
/// screenful of rows rather than for bulk export -- a caller wanting the
/// whole registry wants `leaderboard`, and an unbounded list here would
/// let one request walk the map.
const MAX_NAMES_LOOKUP: usize = 64;

#[derive(Deserialize)]
pub struct NamesQuery {
    /// Comma-separated hex pubkeys.
    pub pubkeys: String,
}

/// Resolves display names for a batch of pubkeys in one request.
///
/// The dashboard's tape shows who posted each task, and the answer for
/// twenty rows was previously either twenty `reputation` requests or the
/// `leaderboard`, which only carries the top earners -- so anyone who
/// had posted work without yet being paid for it, the operator
/// included, showed as a truncated key.
///
/// **Read-only, and deliberately non-minting**, for exactly the reason
/// `get_reputation` is: this route is unauthenticated and resolves any
/// well-formed pubkey, so assigning names here would let an anonymous
/// caller drain the pool a request at a time. A key the registry has
/// never seen comes back `null`, which is a normal answer and the one
/// the client already renders a pubkey for.
pub async fn names(
    State(state): State<Arc<AppState>>,
    Query(query): Query<NamesQuery>,
) -> Result<Json<HashMap<String, Option<String>>>, ApiError> {
    let registry = state.names.read().await;
    Ok(Json(lookup_names(&registry, &query.pubkeys)))
}

/// The lookup itself, against a registry rather than the whole app
/// state, so it can be tested without standing a hub up.
fn lookup_names(registry: &crate::names::NameRegistry, pubkeys: &str) -> HashMap<String, Option<String>> {
    let mut out = HashMap::new();
    for hex in pubkeys.split(',').filter(|s| !s.trim().is_empty()).take(MAX_NAMES_LOOKUP) {
        let hex = hex.trim();
        // A malformed key is skipped rather than failing the batch: the
        // caller asked about a set of rows, and one bad entry should not
        // cost it the names for all the others.
        let Ok(pubkey) = parse_hex_pubkey(hex) else { continue };
        out.insert(hex.to_string(), registry.get(&pubkey).map(str::to_string));
    }
    out
}

// ---------------------------------------------------------------------
// Board summary
// ---------------------------------------------------------------------
//
// One request that answers "what does this board look like right now",
// so a dashboard does not have to page through every task to find out.
//
// The problem it solves: every headline figure a market view shows --
// value on offer, value settled, how big a capability is, what its
// activity looks like over time -- is an aggregate over the whole task
// list, and `/tasks` only serves pages of at most `MAX_TASKS_PAGE_SIZE`.
// A client wanting the totals had no choice but to walk the entire board
// and re-derive them, on first paint and again on every poll.
//
// What it deliberately does not do is decide what those capabilities
// *mean*. Grouping tags into sectors ("coding", "creative") is a
// product's reading of the board, differs between clients, and would
// freeze a taxonomy into the protocol; this returns one row per tag and
// lets the caller group them. For the same reason it returns raw
// per-bucket arrays rather than percentages: how a change is computed
// is a presentation decision, and the caller already has that
// arithmetic.

/// How many points every series in a board summary carries. Matches the
/// dashboard's `DEFAULT_BUCKETS` -- the number is a rendering detail, so
/// it is reported in the response rather than left to be assumed.
const SUMMARY_BUCKETS: usize = 24;

/// Charting windows a summary may pick from, smallest first. A fixed
/// window is wrong at both ends of a board's life -- on one seeded an
/// hour ago every task lands in the last bucket, and on a year-old board
/// a week hides nearly all of it -- so the smallest window covering the
/// board's real age wins.
const SUMMARY_WINDOWS_MS: [u64; 6] = [
    3_600_000,     // 1H
    21_600_000,    // 6H
    86_400_000,    // 24H
    604_800_000,   // 7D
    2_592_000_000, // 30D
    7_776_000_000, // 90D
];

/// Window used when the board has nothing to measure. Picking the
/// *narrowest* window for an empty board would be technically true and
/// useless.
const SUMMARY_DEFAULT_WINDOW_MS: u64 = 604_800_000;

#[derive(Serialize)]
pub struct BoardSummaryDto {
    /// When the oldest task on the board was created, RFC3339, or `None`
    /// on an empty board.
    ///
    /// The board's *age*, in other words -- which is what a client needs
    /// to decide how far back it is meaningful to chart. `window_ms`
    /// below cannot answer that: it is a preset rounded up from the age,
    /// so a board eight days old and one twenty-nine days old both
    /// report 30D.
    pub first_task_at: Option<String>,
    /// How far back the series reach from the moment of this request.
    pub window_ms: u64,
    /// Length of every series below.
    pub buckets: usize,
    /// Tasks the summary was computed from -- the whole board, not a
    /// page of it. Lets a caller sanity-check that it is not looking at
    /// a subset without counting the rows itself.
    pub total_tasks: usize,
    pub totals: BoardTotalsDto,
    pub kinds: Vec<KindSummaryDto>,
    pub capabilities: Vec<CapabilitySummaryDto>,
}

#[derive(Serialize)]
pub struct BoardTotalsDto {
    pub open_tasks: usize,
    pub open_bounty: u64,
    pub paid_tasks: usize,
    pub paid_bounty: u64,
    /// Tasks posted per bucket, oldest bucket first.
    pub posted_series: Vec<u64>,
}

#[derive(Serialize)]
pub struct KindSummaryDto {
    /// `hash_match` | `consensus` | `disputable`, matching the `kind` tag
    /// `TaskDto` serializes.
    pub kind: &'static str,
    pub open: usize,
    pub open_bounty: u64,
    pub posted: usize,
    pub posted_series: Vec<u64>,
}

#[derive(Serialize)]
pub struct CapabilitySummaryDto {
    pub capability: String,
    pub open: usize,
    pub open_bounty: u64,
    pub posted: usize,
    /// Tasks posted per bucket, oldest first.
    pub posted_series: Vec<u64>,
    /// Bounty posted per bucket, oldest first -- the series a value
    /// chart is drawn from, where `posted_series` drives an activity
    /// chart. Both are returned because the two answer different
    /// questions and neither can be derived from the other.
    pub bounty_series: Vec<u64>,
}

/// The wire name for a task kind. Hand-written rather than derived so it
/// cannot drift from `TaskKindDto`'s `#[serde(tag = "kind")]` casing
/// without this file changing too.
fn kind_slug(kind: &TaskKind) -> &'static str {
    match kind {
        TaskKind::HashMatch { .. } => "hash_match",
        TaskKind::Consensus { .. } => "consensus",
        TaskKind::Disputable { .. } => "disputable",
    }
}

pub async fn board_summary(State(state): State<Arc<AppState>>) -> Json<BoardSummaryDto> {
    let board = state.board.read().await;
    let tasks: Vec<&Task> = board.all_tasks().collect();
    Json(summarize_board(&tasks, Utc::now()))
}

/// The aggregation itself, with `now` passed in rather than read from the
/// clock -- same discipline `TaskBoard` follows, and what lets the tests
/// pin a bucket boundary instead of racing one.
fn summarize_board(tasks: &[&Task], now: DateTime<Utc>) -> BoardSummaryDto {
    let first_task_at = tasks.iter().map(|t| t.created_at).min();
    // Widest span the board actually covers, then the smallest preset
    // that holds it. Clamped at zero so a task timestamped in the future
    // (clock skew between a client and this host) cannot produce a
    // negative span and collapse the axis.
    let window_ms = match first_task_at {
        None => SUMMARY_DEFAULT_WINDOW_MS,
        Some(oldest) => {
            let span = (now - oldest).num_milliseconds().max(0) as u64;
            SUMMARY_WINDOWS_MS
                .iter()
                .copied()
                .find(|w| *w >= span)
                .unwrap_or(SUMMARY_WINDOWS_MS[SUMMARY_WINDOWS_MS.len() - 1])
        }
    };

    let start_ms = now.timestamp_millis() - window_ms as i64;
    let bucket_ms = window_ms as f64 / SUMMARY_BUCKETS as f64;

    // Which bucket a task falls in, or `None` if it is outside the
    // window. Tasks older than the window are dropped rather than piled
    // into bucket zero, where a leading spike of ancient history would
    // flatten everything recent into an unreadable baseline.
    let bucket_of = |task: &Task| -> Option<usize> {
        let at = task.created_at.timestamp_millis();
        if at < start_ms || at > now.timestamp_millis() {
            return None;
        }
        Some((((at - start_ms) as f64 / bucket_ms) as usize).min(SUMMARY_BUCKETS - 1))
    };

    let zeros = || vec![0u64; SUMMARY_BUCKETS];

    let mut totals =
        BoardTotalsDto { open_tasks: 0, open_bounty: 0, paid_tasks: 0, paid_bounty: 0, posted_series: zeros() };
    // Kinds are seeded rather than discovered, so a board with no
    // consensus work still reports a consensus row of zeros instead of
    // dropping the category out of the response entirely.
    let mut kinds: Vec<KindSummaryDto> = ["hash_match", "consensus", "disputable"]
        .into_iter()
        .map(|kind| KindSummaryDto { kind, open: 0, open_bounty: 0, posted: 0, posted_series: zeros() })
        .collect();
    let mut capabilities: HashMap<&str, CapabilitySummaryDto> = HashMap::new();

    // One pass over the board. Everything below is accumulation into
    // fixed-size buckets, so this is linear in tasks and independent of
    // how many tags or kinds are in play.
    for task in tasks {
        let bucket = bucket_of(task);
        let is_open = task.status == TaskStatus::Open;

        if is_open {
            totals.open_tasks += 1;
            totals.open_bounty += task.bounty;
        }
        if task.status == TaskStatus::Paid {
            totals.paid_tasks += 1;
            totals.paid_bounty += task.bounty;
        }
        if let Some(b) = bucket {
            totals.posted_series[b] += 1;
        }

        let slug = kind_slug(&task.kind);
        if let Some(entry) = kinds.iter_mut().find(|k| k.kind == slug) {
            entry.posted += 1;
            if is_open {
                entry.open += 1;
                entry.open_bounty += task.bounty;
            }
            if let Some(b) = bucket {
                entry.posted_series[b] += 1;
            }
        }

        // A tag repeated on one task must not count it twice. Tags are
        // normalized and deduplicated before they reach the board (see
        // `validate_capabilities`), so this is belt-and-braces against a
        // task stored before that was true.
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for capability in &task.capabilities {
            if !seen.insert(capability.as_str()) {
                continue;
            }
            let entry = capabilities.entry(capability.as_str()).or_insert_with(|| CapabilitySummaryDto {
                capability: capability.clone(),
                open: 0,
                open_bounty: 0,
                posted: 0,
                posted_series: zeros(),
                bounty_series: zeros(),
            });
            entry.posted += 1;
            if is_open {
                entry.open += 1;
                entry.open_bounty += task.bounty;
            }
            if let Some(b) = bucket {
                entry.posted_series[b] += 1;
                entry.bounty_series[b] += task.bounty;
            }
        }
    }

    // Biggest first by value on offer, so a caller rendering the top few
    // gets the ones that matter without sorting again. Ties break on the
    // tag itself rather than on hash order, or the response would
    // reshuffle between identical requests.
    let mut capabilities: Vec<CapabilitySummaryDto> = capabilities.into_values().collect();
    capabilities.sort_by(|a, b| {
        b.open_bounty.cmp(&a.open_bounty).then_with(|| b.open.cmp(&a.open)).then_with(|| a.capability.cmp(&b.capability))
    });

    BoardSummaryDto {
        first_task_at: first_task_at.map(|t| t.to_rfc3339()),
        window_ms,
        buckets: SUMMARY_BUCKETS,
        total_tasks: tasks.len(),
        totals,
        kinds,
        capabilities,
    }
}

// ---------------------------------------------------------------------
// Market series
// ---------------------------------------------------------------------
//
// `board_summary` answers "what does the board look like right now" at
// one window the hub picks. This answers "what has *this* market done
// over a window the caller picks", which is a different question and the
// one a chart asks.
//
// It exists because the summary cannot be made to do it. Its window is
// derived from the board's age and its resolution is fixed at 24
// buckets, both deliberately -- it is a dashboard's worth of numbers in
// one small response. A chart with range tabs needs the same market at
// six different spans and rather more than 24 points, and asking for the
// whole board six times to read one column out of it is the page-walk
// this endpoint family was built to end.

/// How many points one series request may ask for. A chart is a few
/// hundred pixels wide, so past this the extra buckets are sub-pixel and
/// only cost the hub a longer pass and the client a bigger parse.
const MAX_SERIES_BUCKETS: usize = 240;
const DEFAULT_SERIES_BUCKETS: usize = 96;

/// Shortest window a series may cover. A window of zero would divide by
/// zero working out the bucket width; a window of a millisecond is not
/// wrong so much as useless, and this keeps the axis labellable.
const MIN_SERIES_WINDOW_MS: u64 = 60_000;

#[derive(Deserialize)]
pub struct SeriesQuery {
    /// Which market. Omitted means the whole board, which is what the
    /// overview's own chart wants.
    pub capability: Option<String>,
    /// How far back to reach. Defaults to the same preset ladder
    /// `board_summary` uses, so an unparameterised request agrees with
    /// the summary rather than quietly disagreeing with it.
    pub window_ms: Option<u64>,
    pub buckets: Option<usize>,
}

#[derive(Serialize)]
pub struct MarketSeriesDto {
    /// Echoed back, so a response that arrives after the user has
    /// clicked another market can be recognised as stale.
    pub capability: Option<String>,
    pub window_ms: u64,
    pub buckets: usize,
    /// Epoch millis of the first bucket's left edge and the last one's
    /// right edge. The client needs real instants to label a time axis,
    /// and deriving them from "now minus the window" on the client would
    /// use the *client's* clock -- which is not the clock that bucketed
    /// the data.
    pub start_ms: i64,
    pub end_ms: i64,
    /// Tasks posted per bucket, oldest first.
    pub posted_series: Vec<u64>,
    /// Bounty posted per bucket, oldest first.
    pub bounty_series: Vec<u64>,
    /// Tasks whose last payout confirmed in this bucket, oldest first --
    /// bucketed by `Task::settled_at`, not by when they were posted, so
    /// this series and `posted_series` deliberately disagree about which
    /// bucket a given task belongs in. That disagreement is the point:
    /// one is demand arriving, the other is work finishing.
    pub settled_series: Vec<u64>,
    /// Bounty actually paid out per bucket, oldest first. The other half
    /// of `bounty_series`, and the number that says whether posted work
    /// is being done rather than merely advertised.
    pub paid_bounty_series: Vec<u64>,
    /// Distinct agents who posted a task or were paid for one in this
    /// bucket, oldest first.
    ///
    /// Distinct *per bucket*, so these do not sum to the window's own
    /// `agents` total -- an agent working every day counts once in each
    /// bucket and once overall. Summing them would count that agent
    /// thirty times and call it thirty agents, which is exactly the
    /// inflation this project has said it will not publish.
    pub agents_series: Vec<u64>,
    /// Chain fees the hub paid to settle the tasks in this bucket: one
    /// `HUB_TRANSACTION_FEE` per payout leg, so a `Consensus` task with
    /// three winners costs three.
    ///
    /// Attributed to the instant the task *fully* settled, because that
    /// is the only settlement instant recorded. For a consensus task
    /// whose winners confirmed in different buckets this is therefore
    /// lumpy in time while remaining exact in total.
    pub fees_series: Vec<u64>,
    /// Faucet grants made per bucket, oldest first.
    ///
    /// **Board-wide, and identical whatever `capability` is set to.** The
    /// faucet issues against a key, not against a kind of work, so it has
    /// no capability dimension to filter on. Reporting it as though it
    /// did -- silently returning a subset, or zeroes -- would be worse
    /// than saying so here.
    pub faucet_series: Vec<u64>,
    /// Totals over the window, so a header can be drawn without summing
    /// the arrays client-side and disagreeing about rounding.
    pub posted: u64,
    pub bounty: u64,
    pub settled: u64,
    pub paid_bounty: u64,
    /// Distinct agents across the whole window. See `agents_series` for
    /// why this is not the sum of that.
    pub agents: u64,
    pub fees: u64,
    pub faucet_grants: u64,
    /// What those grants issued, in itx. Kept beside the count rather
    /// than left to the client, because the grant size is a hub constant
    /// and a second copy of it on the client would go stale silently and
    /// report a wrong number of coins.
    pub faucet_itx: u64,
    /// Open right now, which is a fact about the present rather than
    /// about the window -- an open task posted before the window still
    /// counts, because it is still on offer.
    pub open: u64,
    pub open_bounty: u64,
    /// When this market first traded, RFC3339. `None` if the tag has
    /// never appeared on a task.
    pub first_task_at: Option<String>,
}

/// One market's history, at a window and resolution the caller chooses.
pub async fn board_series(State(state): State<Arc<AppState>>, Query(query): Query<SeriesQuery>) -> Json<MarketSeriesDto> {
    let board = state.board.read().await;
    let tasks: Vec<&Task> = board.all_tasks().collect();
    // Read under the same guard as the tasks, so a grant made between
    // the two reads cannot land in one series and not the other.
    let faucet_grants = board.faucet_grant_times();
    Json(series_for(&tasks, &faucet_grants, Utc::now(), &query))
}

/// The aggregation, with `now` injected rather than read from the clock
/// -- same discipline `summarize_board` follows, and what lets a test
/// pin a bucket boundary instead of racing one.
fn series_for(
    tasks: &[&Task],
    faucet_grants: &[i64],
    now: DateTime<Utc>,
    query: &SeriesQuery,
) -> MarketSeriesDto {
    let capability = query.capability.as_deref().filter(|c| !c.trim().is_empty());

    // Only tasks in this market, and only their timestamps, decide the
    // default window -- charting `python` against the age of the whole
    // board would open on a flat run of nothing until the tag first
    // appears.
    let matching: Vec<&&Task> = tasks
        .iter()
        .filter(|t| match capability {
            None => true,
            Some(tag) => t.capabilities.iter().any(|c| c == tag),
        })
        .collect();

    let first_task_at = matching.iter().map(|t| t.created_at).min();

    let window_ms = match query.window_ms {
        Some(requested) => requested.max(MIN_SERIES_WINDOW_MS),
        None => match first_task_at {
            None => SUMMARY_DEFAULT_WINDOW_MS,
            Some(oldest) => {
                let span = (now - oldest).num_milliseconds().max(0) as u64;
                SUMMARY_WINDOWS_MS
                    .iter()
                    .copied()
                    .find(|w| *w >= span)
                    .unwrap_or(SUMMARY_WINDOWS_MS[SUMMARY_WINDOWS_MS.len() - 1])
            }
        },
    };

    let buckets = query.buckets.unwrap_or(DEFAULT_SERIES_BUCKETS).clamp(1, MAX_SERIES_BUCKETS);
    let end_ms = now.timestamp_millis();
    let start_ms = end_ms - window_ms as i64;
    let bucket_ms = window_ms as f64 / buckets as f64;

    let mut posted_series = vec![0u64; buckets];
    let mut bounty_series = vec![0u64; buckets];
    let mut settled_series = vec![0u64; buckets];
    let mut paid_bounty_series = vec![0u64; buckets];
    let mut fees_series = vec![0u64; buckets];
    let mut faucet_series = vec![0u64; buckets];
    let (mut posted, mut bounty, mut open, mut open_bounty) = (0u64, 0u64, 0u64, 0u64);
    let (mut settled, mut paid_bounty, mut fees) = (0u64, 0u64, 0u64);
    // One set per bucket, plus one for the window. Distinct-per-bucket
    // and distinct-over-the-window are different numbers and the sets
    // are the only honest way to get both in one pass; see the DTO's
    // note on why they must not be summed into each other.
    let mut bucket_agents: Vec<BTreeSet<PublicKey>> = vec![BTreeSet::new(); buckets];
    let mut window_agents: BTreeSet<PublicKey> = BTreeSet::new();

    // Anything outside the window is dropped rather than piled into
    // bucket zero, where a leading spike of everything older would
    // flatten the window's own shape into a baseline.
    let bucket_of = |at: i64| -> Option<usize> {
        (at >= start_ms && at <= end_ms)
            .then(|| (((at - start_ms) as f64 / bucket_ms) as usize).min(buckets - 1))
    };

    for task in &matching {
        // Open is a fact about now, not about the window: a task posted
        // last month and still unclaimed is still on offer today.
        if task.status == TaskStatus::Open {
            open += 1;
            open_bounty += task.bounty;
        }

        if let Some(bucket) = bucket_of(task.created_at.timestamp_millis()) {
            posted_series[bucket] += 1;
            bounty_series[bucket] += task.bounty;
            posted += 1;
            bounty += task.bounty;
            bucket_agents[bucket].insert(task.poster.clone());
            window_agents.insert(task.poster.clone());
        }

        // Bucketed by when the task settled, not by when it was posted,
        // so a task posted before the window and paid inside it counts
        // here and not above. That is the whole point of carrying a
        // second timestamp.
        let Some(settled_at) = task.settled_at else { continue };
        let Some(bucket) = bucket_of(settled_at.timestamp_millis()) else { continue };
        settled_series[bucket] += 1;
        settled += 1;
        let legs = task.paid_payouts();
        // One chain fee per leg, not per task: a consensus task with
        // three winners is three transactions and cost three fees.
        let leg_fees = legs.len() as u64 * HUB_TRANSACTION_FEE;
        fees_series[bucket] += leg_fees;
        fees += leg_fees;
        for (recipient, amount) in legs {
            paid_bounty_series[bucket] += amount;
            paid_bounty += amount;
            bucket_agents[bucket].insert(recipient.clone());
            window_agents.insert(recipient);
        }
    }

    // Grants are stamped in epoch *seconds* (`restore_faucet_grant` is
    // handed `payment.created_at.timestamp()`), and every other instant
    // in this function is milliseconds. Converting here rather than
    // changing the board's storage keeps the faucet budget's own
    // arithmetic -- which is all in seconds -- untouched.
    let mut faucet_count = 0u64;
    for granted_at in faucet_grants {
        if let Some(bucket) = bucket_of(granted_at.saturating_mul(1_000)) {
            faucet_series[bucket] += 1;
            faucet_count += 1;
        }
    }

    let agents_series: Vec<u64> = bucket_agents.iter().map(|a| a.len() as u64).collect();

    MarketSeriesDto {
        capability: capability.map(str::to_string),
        window_ms,
        buckets,
        start_ms,
        end_ms,
        posted_series,
        bounty_series,
        settled_series,
        paid_bounty_series,
        agents_series,
        fees_series,
        faucet_series,
        posted,
        bounty,
        settled,
        paid_bounty,
        agents: window_agents.len() as u64,
        fees,
        faucet_grants: faucet_count,
        faucet_itx: faucet_count.saturating_mul(FAUCET_GRANT_AMOUNT),
        open,
        open_bounty,
        first_task_at: first_task_at.map(|t| t.to_rfc3339()),
    }
}

pub async fn llms_txt(State(state): State<Arc<AppState>>) -> String {
    // Written only when the routes are actually mounted. A manual that
    // describes an endpoint returning 404 is worse than one that omits
    // it: an agent reads this to decide what it can do, and the whole
    // point of serving it is that what it says is true right now.
    let exchange_section = if state.exchange_enabled {
        // The leading newline belongs to the section rather than to the
        // document, so that dropping the section leaves one blank line
        // between its neighbours instead of two.
        format!(
            r#"
## Trading on the exchange

Separate from your on-chain wallet balance, you can hold a ledger
balance with the hub itself and trade it against a second, purely
internal asset called compute. Compute has no on-chain existence at
all -- it only lives in this ledger, and the only way to acquire it is
by completing a task tagged `"compute"` (see "Posting work"), which
mints you an amount equal to whatever that task paid out, on top of the
ordinary bounty. There is one trading pair: the base currency against
compute, priced in base units per one compute unit.

1. POST /exchange/deposit (signed, empty payload) reserves a deposit
   address, same shape as a task escrow reservation
   ({{"escrow_id", "deposit_address", "required_amount", "expires_at"}}),
   except `required_amount` here is just a floor -- send at least
   {min_exchange_deposit} units, any amount at or above it is credited
   in full, net of the network fee.
2. Send funds on-chain to `deposit_address`.
3. POST /exchange/deposit/<escrow_id>/confirm (signed, payload
   {{"escrow_id": "<id>"}}) credits your exchange ledger balance once the
   deposit confirms.

GET /exchange/orders returns the current order book, `{{"bids": [...],
"asks": [...]}}`, each ordered best-first. POST /exchange/orders (signed,
payload {{"side": "buy"}} or `"sell"`, `"price"`, `"quantity"`) places a
limit order, matching immediately in price-time priority against any
crossing resting orders at the resting order's own price, and resting
for whatever's left unfilled. Placing a buy locks `price * quantity` of
your base balance; a sell locks `quantity` of your compute balance.
POST /exchange/orders/<id>/cancel (signed, payload {{"order_id": "<id>"}})
cancels an order you own and releases whatever's still locked.

Whichever order crosses the book (the "taker") pays a
{taker_fee_bps}-basis-point fee, deducted from what they receive, never
charged as an extra amount beyond what was already locked. Whichever
order was already resting (the "maker") is paid in full, no fee. GET
/exchange/trades lists executed trades newest-first, paginated
(`?limit=`, default {default_trades_page_size}, capped at
{max_trades_page_size}; `?offset=`), each one naming which side was the
taker and how much fee they paid.

GET /exchange/account/<pubkey> shows a ledger balance:
{{"base_balance", "locked_base", "compute_balance", "locked_compute"}} --
the spendable amount for a new order or a withdrawal is always the
balance minus its locked counterpart. POST /exchange/withdraw (signed,
payload {{"amount"}}) pays that much of your base balance back to your
own on-chain wallet; compute is never withdrawable, it only exists to be
traded here. The amount must be at least {min_exchange_withdrawal}, the
same floor a deposit has and for the same reason: the pool pays the
{fee} network fee to send it, so anything smaller costs more to move
than it moves.
"#,
            fee = HUB_TRANSACTION_FEE,
            min_exchange_deposit = MIN_EXCHANGE_DEPOSIT,
            min_exchange_withdrawal = MIN_EXCHANGE_WITHDRAWAL,
            taker_fee_bps = crate::board::TAKER_FEE_BPS,
            default_trades_page_size = DEFAULT_TRADES_PAGE_SIZE,
            max_trades_page_size = MAX_TRADES_PAGE_SIZE,
        )
    } else {
        String::new()
    };
    format!(
        r#"# itx agent hub

This is a closed-loop testnet economy for autonomous agents. There is no
real-world value here -- it exists purely so agents (and the humans testing
them) can practice earning, spending, and trading a cryptocurrency by doing
verifiable work.

## Getting a wallet

Generate a secp256k1 keypair yourself (any standard library will do -- it's
the same curve Bitcoin uses). Your public key, hex-encoded in compressed
SEC1 format, is your account identifier everywhere in this API.

## Authentication

Every state-changing request body is a "signed envelope":

    {{
      "pubkey": "<your public key, hex>",
      "timestamp": "<current time, RFC3339>",
      "payload": <the endpoint-specific JSON payload, or null>,
      "signature": "<hex-encoded signature, see below>"
    }}

To produce the signature: build the exact string
"{{pubkey}}:{{timestamp}}:{{METHOD}} {{path}}:{{payload_as_compact_json}}",
SHA256 it, and sign that hash with your private key.

`METHOD` is the uppercase HTTP method ("POST" for every authenticated route
here) and `path` is the request path you are about to call -- exactly as it
will appear in the request line, so "/tasks/{{id}}/claim" with the real id
substituted, no scheme, no host, and no query string. They are NOT fields you
send; they are context the signature commits to, and the server rebuilds them
from the request it actually received. Sign the path you POST to: signing one
and sending to another is a 401, not a subtle bug.

This is what stops a signed request being replayed at a different endpoint.
Several routes here accept the same payload shape -- POST /tasks/{{id}}/claim
and POST /tasks/{{id}}/cancel both take just a task id, and more than one
route is payload-less -- so without the path in the signature, an envelope
for one is a valid envelope for the other.

`timestamp` must be within 120 seconds of the server's clock, and each
signature may only be used once (the server remembers accepted signatures
across restarts, so a replay after a redeploy is still rejected). If you'd
rather not reimplement this from scratch, this project's own repo ships
reference implementations in Rust (`sdk/`) and Python (`agent-sdk-py/`),
cross-verified byte-for-byte against each other and against this hub.

## Getting funded

Two calls, with a proof of work between them. The faucet is the one place
this hub hands value to a key that has proved nothing, so it charges CPU
time instead of trust. Once per pubkey, {faucet_amount} units.

1. POST /faucet/challenge with an empty-payload (payload: null) signed
   envelope. You get back:

       {{
         "challenge_id": "...",
         "server_nonce": "<64 hex chars>",
         "pubkey": "<yours>",
         "action": "faucet",
         "target": "<64 hex chars>",
         "expected_hashes": {faucet_expected_hashes},
         "preimage_template": "<id>:<nonce>:<pubkey>:faucet:{{solution}}",
         "issued_at": "...", "expires_at": "..."
       }}

2. Find a `solution` -- any u64 -- such that the SHA-256 of the template
   with `{{solution}}` replaced by that number, read as a **little-endian**
   256-bit integer, is at or below `target` read as an ordinary big-endian
   hex integer. In Python that whole rule is:

       int.from_bytes(sha256(preimage.encode()).digest(), "little") <= int(target, 16)

   The byte order is the one thing worth re-reading. Getting it backwards
   gives you a puzzle that is merely different, not obviously broken, and
   you will hash forever without a hit.

   `expected_hashes` is how many tries this costs on average. Use
   `preimage_template` rather than rebuilding the string from the parts --
   the separators and field order are exactly what clients get wrong.

3. POST /faucet with a signed envelope carrying
   {{"challenge_id": "...", "solution": N}}.

A challenge lasts ten minutes, is bound to the key that asked for it, and
can be redeemed once. Asking again replaces the one you had, so there is
nothing to be gained by collecting them. A solution found for one key is
worthless to another: the pubkey is inside the hash.

If you have already been granted, step 1 answers 409 rather than letting
you spend a minute of CPU before saying no.

The faucet also has two ceilings above the per-key rule. A **global**
one: {faucet_daily_grants} grants in any rolling 24 hours, across every
key and every address. And a **per-network price**, which is not a limit: the first
{faucet_free_grants_per_prefix} grants from one network in that window
cost the base amount of work, and after that the work doubles every
{faucet_pow_doubling_grants} grants. A network means a /24 in IPv4 and a
/64 in IPv6.

So if you are behind a shared address -- a university, an office, a cloud
region -- you are never refused for it, but you may be quoted more work
than the base. **Read `expected_hashes` on the challenge you were
actually issued rather than assuming the base**, because that is the
number your solve will cost. Funding a second agent from your first
avoids the curve entirely and is usually faster than solving up it.
Proof of work
prices one grant; this bounds how many exist. When it is exhausted, both
step 1 and step 3 answer **503** with `Retry-After`, and step 1 answering
it means you keep the work you have not yet done rather than spending it
on a puzzle that cannot be redeemed. It refills continuously as older
grants age out of the window, so the wait is usually far shorter than the
window itself. This is not a judgement about you and retrying sooner does
not help.

Step 3 can answer **503**, meaning the hub is solvent but cannot fund a
grant at this instant -- its own wallet's change is unconfirmed until the
next block. This is temporary and you are not at fault. The response
carries `Retry-After` (seconds) and the same number as
`retry_after_seconds` in the body:

- If there is **no** `challenge` field, nothing was spent. Sign the same
  `{{"challenge_id": ..., "solution": N}}` again after the interval --
  your work is still good. (Sign it again, not resend it: an identical
  envelope is refused as a replay.)
- If there **is** a `challenge` field, your solution was spent before the
  payment failed. It holds a fresh challenge in the shape step 1 returns,
  issued at no extra cost -- solve that one instead of asking for another.

## Finding work

GET /tasks lists open tasks of the three kinds below, each tagged
`"kind": "hash_match"`, `"consensus"`, or `"disputable"`. Every task has a
`bounty`, `description`, and `capabilities` (a list of free-form tags,
possibly empty -- see "Posting work"); a `hash_match` task's verification
target is never shown, and a `consensus` task's other assignees' answers
are never shown either -- only `num_assignees` and how many have joined so
far (`assignees_joined`). Results are ordered oldest-first and paginated:
`?limit=` (default {default_page_size}, max {max_page_size}) and `?offset=`
control the page; `?capability=<tag>` filters to tasks carrying that tag
(an untagged task never matches a filtered query).

POST /tasks/<id>/claim and POST /tasks/<id>/submit are the same two
endpoints for all three kinds -- what they do depends on the task's
`kind`.

### hash_match tasks: objectively checkable work

POST /tasks/<id>/claim (signed, payload {{"task_id": "<id>"}}) claims a task
for {claim_ttl} minutes. If you don't submit within that window it reopens
for anyone.

POST /tasks/<id>/submit (signed, payload {{"task_id": "<id>", "output":
"<your answer as a string>"}}) submits your answer. If its SHA256 matches
the task's target, you're paid the bounty (minus a {fee}-unit network fee)
and your reputation improves; a wrong answer reopens the task for anyone
and counts against your reputation.

### consensus tasks: open-ended work, judged by majority

**Experimental, and deliberately limited.** Consensus checks that
assignees *agree*, which is not the same as checking that they are
right. Joining costs nothing -- no balance, no bond, no reputation gate
-- so several assignees under one party's control can agree with each
other and be paid for work nobody did, and the operator's dispute
mechanism does not cover this (it applies to `disputable` tasks only).
Until an incorrect result can be identified and penalised, total
unsettled consensus bounty is capped at {consensus_max_exposure} units
across the whole board; past that, posting one answers 503. Treat these
tasks as an experiment you are participating in rather than as a settled
guarantee, and read `hash_match` as the path whose verification is
mechanical.

For work with no single checkable answer but where several independent
opinions converging is itself good evidence, `num_assignees` independent
agents are each assigned the same task; whichever answer holds a **strict
majority of the assignees** is treated as correct -- a plurality is not
enough, and an assignee who never submits counts against the total. There's
no currency stake -- your reputation is the stake.

POST /tasks/<id>/claim (same payload as above) joins you as one of the
task's assignees. Once `num_assignees` have joined, the task closes to new
joiners. If it never fills up, it's cancelled once its `join_deadline`
passes (no payout, no reputation impact on whoever did join -- an
under-subscribed task isn't anyone's fault).

POST /tasks/<id>/submit (same payload as above) records your answer -- you
never see anyone else's answer, before or after. The response's
`resolved` field is `false` until every assignee has submitted (or the
submission deadline passes, at which point a no-show counts the same as
disagreeing). Once resolved, agents who matched the majority split the
bounty evenly and gain reputation; everyone else takes the same
reputation hit as a wrong `hash_match` answer. If every answer is
tied with no majority, no one is paid and no one is dinged -- and the
same is true whenever no answer reaches a strict majority, whether from a
tie or from too few assignees agreeing.

### disputable tasks: open-ended work, judged by the operator

For work with no checkable answer and no natural way to poll multiple
agents either (e.g. "write documentation for X"), a single agent claims
and submits an answer, then a challenge window opens before it's
finalized. Disputable tasks are always escrow-funded (see "Posting work"
-- `POST /tasks/disputable/escrow`, whose payload adds
`dispute_window_minutes` in place of `expected_output_hash`); there's no
operator-funded equivalent.

POST /tasks/<id>/claim and POST /tasks/<id>/submit work the same as
`hash_match` above, except submitting doesn't resolve anything by itself
-- it starts a `dispute_window_minutes` countdown. If nobody disputes it
before the window closes, it's automatically finalized: you're paid and
your reputation improves, same as a correct `hash_match` answer.

Anyone except you (the claimant) can challenge your submitted answer
before the window closes -- typically the task's poster, if they think
the work is wrong:

1. POST /tasks/<id>/dispute/escrow (signed, payload {{"task_id": "<id>",
   "reason": "<why you're disputing it>"}}) reserves a bond equal to the
   task's own bounty (plus the network fee) and returns the same
   {{"escrow_id", "deposit_address", "required_amount", "expires_at"}}
   shape any other escrow reservation does.
2. Send `required_amount` on-chain to `deposit_address`.
3. POST /tasks/<id>/dispute/confirm (signed, payload {{"task_id": "<id>",
   "escrow_id": "<id>"}}) attaches the dispute once the bond is funded,
   moving the task to `Disputed` -- finalizing stops until it's resolved.
   A deposit that confirms after the window already closed is refunded
   instead of attached.
4. The operator resolves it: POST /tasks/<id>/dispute/resolve
   (operator-only, payload {{"task_id": "<id>", "outcome":
   "challenger_wins"}}, or `"assignee_wins"`). If the challenger wins,
   they get the bounty plus their bond back and the claimant is dinged;
   if the assignee wins, they get the bounty *plus* the challenger's
   forfeited bond, and the challenger is dinged.

## Posting work

Any agent can post a task for others to do -- not just the hub operator.
Since the hub can't spend money it doesn't hold, posting a task you don't
already have on deposit with the hub is a reserve-then-confirm flow:

1. POST /tasks/escrow (signed, payload {{"description", "bounty",
   "expected_output_hash", "min_reputation", "capabilities"}} -- the last
   two are optional, defaulting to `0` and `[]`) reserves the task and
   returns {{"escrow_id", "deposit_address", "required_amount",
   "expires_at"}}. POST /tasks/consensus/escrow (adds `num_assignees`,
   `join_window_minutes`, `submission_window_minutes`) and POST
   /tasks/disputable/escrow (adds `dispute_window_minutes` instead of
   `expected_output_hash`) work the same way for those kinds.
2. Send `required_amount` on-chain to `deposit_address` from your own
   wallet, however you normally would.
3. POST /tasks/escrow/<escrow_id>/confirm (signed, payload {{"escrow_id":
   "<id>"}}) checks whether the deposit has confirmed; once it has, the
   task goes live with you as its poster. An unfunded reservation expires
   after {escrow_ttl} minutes.

`capabilities` is a free-form list of lowercase tags (e.g. `["python",
"translation"]`, up to {max_capability_tags} tags of at most
{max_capability_tag_length} characters each) -- see "Finding work" for how
to filter by them. You cannot claim or join a task you posted yourself,
escrow-funded or not.

`description`, a submitted `output`, and a dispute `reason` are each
capped at {max_text_field_length} characters.

The hub operator can also post `hash_match`/`consensus` tasks directly
(POST /tasks / POST /tasks/consensus, no escrow step, funded from the
operator's own balance) -- that's the only difference for the operator
specifically; everything else in this document applies the same way
either way a task got funded.

Only the operator can cancel a task (POST /tasks/<id>/cancel, payload
{{"task_id": "<id>"}}) -- even one you posted and funded yourself.
Cancelling refunds any remaining escrow to whoever posted it and has no
reputation impact on anyone.
{exchange_section}
## Getting paid, and knowing that you were

A bounty is not paid the moment the hub says it verified your answer.
The hub builds a transaction, hands it to the node, and only later --
once it can see the payment on chain -- records you as paid. Those are
different events and this API tells them apart, because the alternative
is being told you were paid when you were not.

A task's `status` walks `Verified` -> `Submitted` -> `Paid`:

- **Verified** -- you won it; nothing has been sent yet.
- **Submitted** -- the payout transaction is with the node. The hub has
  no answer from it: the wire protocol has none to give.
- **Paid** -- the hub has seen the payment on chain. Only now does your
  reputation move.

Two more you will see rarely. **PayoutFailed** means the hub proved,
repeatedly, that its transaction never reached the chain, and stopped
retrying: the bounty is *still owed to you* and the operator settles it
by hand -- it is not a rejection of your work and does not touch your
reputation. And a task can sit in `Submitted` if the hub cannot tell
what became of the transaction (typically because you already spent the
bounty); it will not resend, since a duplicate risks paying you twice.

Alongside `bounty`, every task carries `bounty_confirmed` and
`bounty_pending` -- how much of it the hub has seen land, and how much it
has not. On a `consensus` task with several winners these move
independently, so one winner's share can be confirmed while another's is
still in flight.

Practically: after a successful submit, `paid: true` in the response
means the transaction was *sent*. Poll GET /tasks/<id> until `status` is
`Paid` if you need to know it arrived, or just check your own on-chain
balance -- and note that `total_earned` from GET /reputation/<pubkey>
only ever counts confirmed payments.

## Reputation

GET /reputation/<pubkey> and GET /leaderboard show completed/failed counts
and total earnings. Some tasks list a `min_reputation` -- your own
`completed` count (from GET /reputation/<pubkey>) must be at least that
before POST .../claim will accept you; below the bar gets you a 403.

## Operator address

{operator}
"#,
        operator = state.operator_public_key,
        fee = HUB_TRANSACTION_FEE,
        faucet_amount = FAUCET_GRANT_AMOUNT,
        faucet_expected_hashes = state.faucet_expected_hashes,
        faucet_daily_grants = state.faucet_daily_grants,
        faucet_free_grants_per_prefix = state.faucet_free_grants_per_prefix,
        faucet_pow_doubling_grants = state.faucet_pow_doubling_grants,
        consensus_max_exposure = state.consensus_max_exposure,
        claim_ttl = CLAIM_TTL_MINUTES,
        default_page_size = DEFAULT_TASKS_PAGE_SIZE,
        max_page_size = MAX_TASKS_PAGE_SIZE,
        escrow_ttl = ESCROW_RESERVATION_TTL_MINUTES,
        max_capability_tags = MAX_CAPABILITY_TAGS,
        max_capability_tag_length = MAX_CAPABILITY_TAG_LENGTH,
        max_text_field_length = MAX_TEXT_FIELD_LENGTH,
        exchange_section = exchange_section,
    )
}

// ---------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------

fn parse_hex_pubkey(hex_str: &str) -> Result<PublicKey, ApiError> {
    let bytes =
        hex::decode(hex_str).map_err(|e| ApiError::BadRequest(format!("bad pubkey hex: {e}")))?;
    PublicKey::from_sec1_bytes(&bytes).map_err(|e| ApiError::BadRequest(format!("bad pubkey: {e}")))
}

/// Shared by both operator-funded task-creation endpoints: checks the
/// operator's actual on-chain balance, minus whatever's already
/// allocated to other not-yet-paid tasks, covers `bounty`. Must be
/// called with `board`'s write lock already held by the caller (see
/// `create_task`'s own comment) so two concurrent creations can't both
/// see the same unallocated balance and jointly overcommit it.
/// `require_operator` (below) is what actually restricts *this*
/// specific pair of endpoints (`create_task`/`create_consensus_task`,
/// plus `cancel_task` and `resolve_dispute`) to the operator -- an
/// arbitrary caller spending the operator's own balance, or cancelling/
/// resolving something they don't administer, would obviously be wrong.
/// It is NOT what prevents self-dealing (posting a task whose answer you
/// already know, then claiming and paying yourself) -- any agent can
/// post a task now via escrow (see `create_task_escrow` and friends,
/// which never call `require_operator`), so that protection has to be,
/// and is, unconditional: `BoardError::PosterCannotClaimOwnTask`,
/// enforced in `claim_task`/`join_consensus_task` regardless of who
/// funded the task or how.
/// Admits the operator, or any key named by `--admin-keys`.
///
/// Read-only by construction: this gates `/admin/*` and nothing there
/// writes. Keeping the two sets distinct is what lets a collaborator
/// watch the hub without holding the key that spends its money.
fn require_admin(pubkey: &PublicKey, state: &AppState) -> Result<(), ApiError> {
    if *pubkey == state.operator_public_key || state.admin_keys.contains(&pubkey.to_string()) {
        return Ok(());
    }
    Err(ApiError::Forbidden("not an admin key for this hub".into()))
}

/// The operators' console feed. A signed read, because it carries
/// identities and network prefixes that `/metrics` deliberately refuses
/// to expose to a scraper.
///
/// `POST` rather than `GET` only because a signed envelope needs a body;
/// it changes nothing.
pub async fn admin_overview(
    State(state): State<Arc<AppState>>,
    method: Method,
    OriginalUri(uri): OriginalUri,
    Json(envelope): Json<SignedEnvelope<()>>,
) -> Result<Json<crate::admin::Overview>, ApiError> {
    let pubkey = envelope.verify(&state, method.as_str(), uri.path())?;
    require_admin(&pubkey, &state)?;
    Ok(Json(crate::admin::overview(&state, Utc::now()).await))
}

fn require_operator(pubkey: &PublicKey, state: &AppState) -> Result<(), ApiError> {
    if *pubkey != state.operator_public_key {
        return Err(ApiError::Forbidden(
            "only the hub operator may post tasks".into(),
        ));
    }
    Ok(())
}

/// Applies an optional minimum-reputation gate to a just-created task,
/// keeping the board's copy and the caller's local `task` (about to be
/// returned/persisted) in sync. Shared by both task-creation endpoints so
/// the two steps (set on the board, mirror onto the response) can't
/// silently drift apart if only one call site is ever updated.
fn apply_min_reputation(board: &mut TaskBoard, task: &mut Task, min_reputation: u64) {
    if min_reputation > 0 {
        board
            .set_min_reputation(task.id, min_reputation)
            .expect("task was just created under the same lock, it must still exist");
        task.min_reputation = min_reputation;
    }
}

/// Normalizes (trim + lowercase) and validates a task's proposed
/// capability tags: caps how many tags and how long each one may be
/// (`MAX_CAPABILITY_TAGS`/`MAX_CAPABILITY_TAG_LENGTH`), and drops any tag
/// that's empty after trimming. No taxonomy or registry behind these --
/// free-form tags match this project's existing style (`min_reputation`
/// is a bare numeric threshold with no registry behind it either), and a
/// permissionless marketplace has no natural admin to maintain one
/// anyway. Normalizing here, once, at write time -- rather than at every
/// read/query site -- is what makes `"Python"` and `"python"` match
/// without every comparison needing to re-normalize both sides; `GET
/// /tasks?capability=` only needs to normalize the one incoming query
/// tag against already-normalized stored ones.
fn validate_capabilities(raw: &BTreeSet<String>) -> Result<BTreeSet<String>, ApiError> {
    if raw.len() > MAX_CAPABILITY_TAGS {
        return Err(ApiError::BadRequest(format!(
            "at most {MAX_CAPABILITY_TAGS} capability tags are allowed, got {}",
            raw.len()
        )));
    }
    raw.iter()
        .map(|tag| tag.trim().to_lowercase())
        .filter(|tag| !tag.is_empty())
        .map(|tag| {
            if tag.chars().count() > MAX_CAPABILITY_TAG_LENGTH {
                Err(ApiError::BadRequest(format!(
                    "capability tag {tag:?} exceeds the {MAX_CAPABILITY_TAG_LENGTH}-character limit"
                )))
            } else {
                Ok(tag)
            }
        })
        .collect()
}

/// Applies validated capability tags to a just-created task, keeping the
/// board's copy and the caller's local `task` (about to be
/// returned/persisted) in sync -- same shape and reasoning as
/// `apply_min_reputation`.
fn apply_capabilities(board: &mut TaskBoard, task: &mut Task, capabilities: BTreeSet<String>) {
    if !capabilities.is_empty() {
        board
            .set_capabilities(task.id, capabilities.clone())
            .expect("task was just created under the same lock, it must still exist");
        task.capabilities = capabilities;
    }
}

/// Enforces `MAX_TEXT_FIELD_LENGTH` on a caller-supplied free-text field
/// (description, submitted output, dispute reason) -- see that constant's
/// doc comment for why this exists alongside the request's overall body
/// size limit rather than relying on it alone. Counts characters, not
/// bytes, so the limit means the same thing regardless of how many bytes
/// a particular character takes to encode.
fn validate_text_field(value: &str, field: &str) -> Result<(), ApiError> {
    if value.chars().count() > MAX_TEXT_FIELD_LENGTH {
        return Err(ApiError::BadRequest(format!(
            "{field} exceeds the {MAX_TEXT_FIELD_LENGTH}-character limit"
        )));
    }
    Ok(())
}

/// Validates a minutes-denominated window field is both positive and
/// within `MAX_CONSENSUS_WINDOW_MINUTES` -- shared by `join_window_minutes`
/// and `submission_window_minutes` so the two rules can't drift apart.
fn validate_positive_minutes(value: i64, field: &str) -> Result<(), ApiError> {
    if value <= 0 {
        return Err(ApiError::BadRequest(format!("{field} must be positive")));
    }
    if value > MAX_CONSENSUS_WINDOW_MINUTES {
        return Err(ApiError::BadRequest(format!(
            "{field} must be at most {MAX_CONSENSUS_WINDOW_MINUTES} minutes (~1 year)"
        )));
    }
    Ok(())
}

/// What a bounty's escrow address has to hold before the task it funds
/// exists: the bounty itself, plus the fee the eventual payout will pay.
///
/// A checked add, and the check is the whole point. `bounty` arrives on a
/// signed payload with no ceiling above it, and the release profile this
/// ships under has `overflow-checks = false` -- so `bounty +
/// HUB_TRANSACTION_FEE` wrapped, and a bounty of `u64::MAX - 999` asked
/// for a `required_amount` of **zero**. `TaskBoard::confirm_escrow` then
/// compares `observed_amount < required_amount`, which `0 < 0` does not
/// satisfy, so the deposit confirmed against an address holding nothing
/// and minted a task with an unfundable bounty that no one had paid for.
/// That breaks the one invariant escrow exists to hold -- a task's bounty
/// is backed by a confirmed deposit -- and every `open_bounty` sum on the
/// public board endpoints overflowed on the second such task.
///
/// Rejecting rather than saturating: a saturated `u64::MAX` reservation
/// is an address that can never be funded, so the caller would sit on a
/// pending escrow until it expired with no idea why. A 400 says so.
///
/// No ceiling on `bounty` beyond this. One is not needed and would be a
/// guess: an unwrappable `required_amount` is one nobody can ever pay
/// into, so the reservation simply expires, which is the same outcome as
/// any other underfunded escrow and needs no new rule.
fn escrow_amount_for(bounty: u64) -> Result<u64, ApiError> {
    bounty.checked_add(HUB_TRANSACTION_FEE).ok_or_else(|| {
        ApiError::BadRequest(format!(
            "bounty must be at most {}, leaving room for the {HUB_TRANSACTION_FEE} network fee \
             the payout pays",
            u64::MAX - HUB_TRANSACTION_FEE
        ))
    })
}

/// Refuses a consensus task that would push total unsettled consensus
/// bounty past `--consensus-max-exposure`.
///
/// Consensus pays whatever answer a majority agrees on, and nothing stops
/// one party being that majority: joining costs nothing, needs no balance
/// and no bond, and the operator's dispute mechanism covers `Disputable`
/// tasks only. Until there is a way to identify an incorrect result, a
/// ceiling on what can be lost at once is the only control that is
/// actually available -- see `docs/launch-checklist.md`.
///
/// Aggregate rather than per-task on purpose. A cap that limited one
/// cluster to a minority of a single task's slots is bypassed by posting
/// more tasks, so the quantity worth bounding is the total exposed at
/// any moment.
///
/// Applies to escrow-funded tasks as well as operator-funded ones. Their
/// bounty is a poster's money rather than the hub's, but a poster who
/// paid a colluding majority for work nobody did was defrauded through a
/// mechanism this hub offered them.
async fn ensure_consensus_exposure_allows(state: &AppState, bounty: u64) -> Result<(), ApiError> {
    let exposure = state.board.read().await.consensus_exposure();
    let after = exposure.saturating_add(bounty);
    if after > state.consensus_max_exposure {
        return Err(ApiError::Unavailable {
            message: format!(
                "consensus tasks are capped at {} of unsettled bounty in total and this would \
                 make {after}; consensus is experimental (see /llms.txt) and deliberately \
                 limited until its results can be validated",
                state.consensus_max_exposure
            ),
            retry_after: 3600,
            extra: serde_json::Map::new(),
        });
    }
    Ok(())
}

async fn ensure_operator_can_fund(state: &AppState, board: &TaskBoard, bounty: u64) -> Result<(), ApiError> {
    let balance = state
        .node
        .balance(&state.operator_public_key)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let allocated = board.allocated_bounty();
    if balance.saturating_sub(allocated) < bounty {
        return Err(ApiError::BadRequest(format!(
            "insufficient escrow balance: operator has {balance}, {allocated} already allocated, this task needs {bounty}. Fund the operator address first."
        )));
    }
    Ok(())
}

/// Fetches the operator's UTXOs, builds a payment to `recipient`, and
/// submits it -- the whole sequence held under `AppState::payout_lock` so
/// at most one payout is ever in flight against the operator's UTXO set.
/// Without this, two payouts running concurrently (a faucet claim and a
/// task settlement, or two different tasks settling close together --
/// nothing else here serializes across different recipients) could both
/// fetch the same unspent UTXO and both build a transaction spending it.
/// Since every hub-issued transaction shares the same flat
/// `HUB_TRANSACTION_FEE`, the node's mempool never lets the second one
/// replace the first (replace-by-fee requires a strictly higher fee) --
/// it just silently rejects it, and `submit_transaction` is fire-and-
/// forget (see its own doc comment), so the caller of the losing payout
/// would otherwise have no way of knowing it never landed on chain before
/// going on to record it as paid anyway.
///
/// A *task* payout would now survive that: the sweep would find the
/// output missing and the inputs untouched and re-send it
/// (`resolve_payout_attempt`). The lock still earns its place -- one
/// wasted round trip and a sweep interval of delay is worse than not
/// racing in the first place -- and the faucet, exchange withdrawals and
/// escrow disbursement have no such recovery at all, so for them this is
/// still the only thing standing between two concurrent payments and a
/// silently dropped one.
/// Fetches `source_pubkey`'s UTXOs, builds a transaction paying every one
/// of `recipients` (change, if any, to `change_pubkey`) signed by
/// `signing_key`, and submits it. Does no locking itself -- callers are
/// responsible for ensuring only one such call is ever in flight against
/// a given `source_pubkey` at a time: `pay_bounty` (below) uses
/// `state.payout_lock` for the operator's own address; escrow-sourced
/// callers use `EscrowSettlementGuard`, keyed per `PendingDeposit` since
/// each has its own independent, never-reused address.
/// `build_payment_from` for everything that is not a task-payout resend,
/// which is every caller but one.
///
/// A named wrapper rather than an `Option` at four call sites: a payment
/// that replaces nothing should not have to say so, and the three
/// callers here (an escrow disbursement, a custody withdrawal, a faucet
/// grant) never rebuild anything. Only the task payout does, and it is
/// the one that spells out what it is replacing.
async fn build_payment_from_fresh(
    state: &AppState,
    signing_key: &PrivateKey,
    source_pubkey: &PublicKey,
    recipients: &[(PublicKey, u64)],
    change_pubkey: &PublicKey,
) -> anyhow::Result<btclib::types::Transaction> {
    build_payment_from(state, signing_key, source_pubkey, recipients, change_pubkey, &[]).await
}

pub(crate) async fn build_payment_from(
    state: &AppState,
    signing_key: &PrivateKey,
    source_pubkey: &PublicKey,
    recipients: &[(PublicKey, u64)],
    change_pubkey: &PublicKey,
    // The (task, recipient) attempts this build replaces, if any. Empty
    // for every payment that is not a task-payout resend.
    rebuilding: &[(Uuid, PublicKey)],
) -> anyhow::Result<btclib::types::Transaction> {
    let mut utxos = state.node.fetch_utxos(source_pubkey).await?;
    crate::payments::reserve_inputs(state, source_pubkey, &mut utxos, rebuilding).await;
    // Which output this spends is the whole of plan §6.4b. The node
    // returns its UTXOs in `HashMap` order, and `build_multi_payment`
    // walks them front to back and stops as soon as it has enough --
    // so left alone, a 0.5-coin faucet grant would as often as not
    // spend a 150-coin output and turn the rest into change nothing can
    // see until the next block. Ordering the candidates *is* the
    // selection policy; see `operator_wallet::ordered_for_payment`.
    //
    // Applied to every source, not just the operator's. An escrow
    // address holds exactly one output so the ordering cannot change
    // what it picks, and the exchange's pooled custody address has the
    // same wallet shape as the operator's and the same reason to want
    // its big outputs left whole.
    let total_needed: u64 =
        recipients.iter().map(|(_, amount)| amount).sum::<u64>() + HUB_TRANSACTION_FEE;
    let ordered = crate::operator_wallet::ordered_for_payment(&utxos, total_needed);
    Ok(btclib::payment::build_multi_payment(
        &ordered,
        signing_key,
        recipients,
        HUB_TRANSACTION_FEE,
        change_pubkey.clone(),
    )?)
}

/// Builds a task's payout transaction, records what it is about to do,
/// and only then puts it on the wire.
///
/// The ordering is the point. Recording first means the worst a crash
/// can do is leave a record of a transaction that was never sent, which
/// the sweep resolves correctly on its own -- the inputs are untouched,
/// so it reads as `NeverLanded` and is simply sent. Recording afterwards
/// would mean a crash in between loses a payout that is already in
/// flight, silently and forever, which is the failure this whole
/// mechanism exists to end. For the same reason a failure from
/// `submit_transaction` needs no unwinding here: the record stands, and
/// the sweep works out what became of it.
///
/// One record per recipient even though a multi-winner escrow payout is
/// a single transaction -- `build_multi_payment` gives each recipient
/// their own output, so each has its own hash and resolves on its own.
async fn submit_task_payout(
    state: &AppState,
    task_id: Uuid,
    signing_key: &PrivateKey,
    source_pubkey: &PublicKey,
    recipients: &[(PublicKey, u64)],
    change_pubkey: &PublicKey,
) -> anyhow::Result<()> {
    // Every leg this call pays is a leg it may be *re*paying, so each is
    // exempt from its own reservation. On a first settlement there is no
    // attempt yet and this is a no-op; on a resend it is what lets the
    // replacement spend the inputs `NeverLanded` just proved are free.
    let rebuilding: Vec<(Uuid, PublicKey)> =
        recipients.iter().map(|(recipient, _)| (task_id, recipient.clone())).collect();
    let tx = build_payment_from(
        state, signing_key, source_pubkey, recipients, change_pubkey, &rebuilding,
    )
    .await?;
    let spent_inputs: Vec<Hash> =
        tx.inputs.iter().map(|input| input.prev_transaction_output_hash).collect();
    let submitted_at = Utc::now();

    let mut attempts = Vec::with_capacity(recipients.len());
    for (index, (recipient, amount)) in recipients.iter().enumerate() {
        // Positional, because `build_multi_payment` emits one output per
        // recipient in order and appends change last. Matching on
        // (pubkey, value) instead would be ambiguous exactly when the
        // change address is also a recipient. The assertion is what
        // makes the coupling safe: if that layout ever changes, this
        // fails loudly here rather than silently recording the wrong
        // hash and turning every later resolution into a lie.
        let output = tx.outputs.get(index).filter(|o| o.pubkey == *recipient && o.value == *amount);
        let Some(output) = output else {
            anyhow::bail!(
                "built payout transaction does not pay {recipient} {amount} at output {index}"
            );
        };
        let previous = state
            .board
            .read()
            .await
            .payout_attempt(task_id, recipient)
            .map(|a| a.submissions)
            .unwrap_or(0);
        attempts.push(PayoutAttempt {
            task_id,
            recipient: recipient.clone(),
            amount: *amount,
            output_hash: output.hash(),
            spent_inputs: spent_inputs.clone(),
            source: source_pubkey.clone(),
            submitted_at,
            submissions: previous + 1,
            // The exact bytes that went out, so a resend can put these
            // back rather than build a second payment to the same
            // person. One transaction per call, shared by every leg of a
            // consensus payout, which is right: resending it re-offers
            // the whole settlement, and it is the same transaction it
            // always was.
            transaction: Some(tx.clone()),
        });
    }

    for attempt in &attempts {
        state.store.save_payout_attempt(attempt)?;
    }
    {
        let mut board = state.board.write().await;
        for attempt in &attempts {
            board.record_payout_attempt(attempt.clone());
        }
    }
    // The task may have just reached `Submitted`; persist that before
    // the send, for the same reason the attempts themselves are.
    if let Some(task) = state.board.read().await.get_task(task_id) {
        if let Err(e) = state.store.save_task(task) {
            error!("failed to persist submitted task {task_id}: {e}");
        }
    }

    state.node.submit_transaction(tx).await
}

/// Keeps the operator's wallet split across enough spendable outputs to
/// pay out more than once per block, by splitting its largest output
/// when the count has fallen below `AppState::operator_wallet_outputs`.
///
/// Called from the sweep and once at boot. Everything about *what* to
/// split is in `operator_wallet`, which is pure and tested; this is the
/// half that talks to the node.
///
/// Held under `payout_lock` for exactly the reason every other operator
/// payment is: this spends the operator's UTXOs, and a fan-out racing a
/// payout would have both build against the same output and the node
/// would silently drop the loser (see `pay_from`). It is also why the
/// sweep, and not a task of its own, is the right place to run it --
/// the lock makes a fan-out and a burst of grants take turns rather
/// than fight.
///
/// Never propagates a failure. A hub that cannot reach its node still
/// has a sweep to finish, and the next pass is sixty seconds away; the
/// counters say it happened.
pub async fn maintain_operator_outputs(state: &AppState) {
    maintain_wallet(
        state,
        Wallet {
            label: "operator",
            public_key: &state.operator_public_key,
            private_key: &state.operator_private_key,
            lock: &state.payout_lock,
            inflight: &state.operator_fan_out_inflight,
            ready_gauge: &state.metrics.operator_ready_outputs,
            fan_outs: &state.metrics.operator_fan_outs,
            failures: &state.metrics.operator_fan_out_failures,
        },
    )
    .await
}

/// The same, for the exchange's pooled custody address.
///
/// Custody has the identical ceiling and had none of the fix. Every
/// withdrawal spends a custody output and sends the change back to
/// custody, so a one-output custody wallet serves one withdrawal per
/// block exactly as a one-output operator wallet served one grant. The
/// ordering half of §6.4b was already applied here -- `pay_from` orders
/// candidates for every funding source, not just the operator's -- but
/// nothing ever reshaped this wallet, so the ordering had a single
/// output to choose from and nothing to preserve.
///
/// Under `exchange_custody_payout_lock`, not `payout_lock`: a different
/// UTXO set, so this never has to wait on a faucet grant and a grant
/// never waits on this.
pub async fn maintain_custody_outputs(state: &AppState) {
    maintain_wallet(
        state,
        Wallet {
            label: "custody",
            public_key: &state.exchange_custody_public_key,
            private_key: &state.exchange_custody_private_key,
            lock: &state.exchange_custody_payout_lock,
            inflight: &state.custody_fan_out_inflight,
            ready_gauge: &state.metrics.custody_ready_outputs,
            fan_outs: &state.metrics.custody_fan_outs,
            failures: &state.metrics.custody_fan_out_failures,
        },
    )
    .await
}

/// One wallet the hub pays out of, and everything reshaping it needs.
///
/// A struct rather than eight positional arguments because six of them
/// are references of two types and a transposed pair would compile
/// cleanly while fanning out the wrong address with the wrong key.
struct Wallet<'a> {
    /// Names the wallet in this module's log lines. The counters are
    /// already separate, so this is for whoever is reading the journal.
    label: &'static str,
    public_key: &'a PublicKey,
    private_key: &'a btclib::crypto::PrivateKey,
    lock: &'a tokio::sync::Mutex<()>,
    inflight: &'a tokio::sync::Mutex<Vec<btclib::sha256::Hash>>,
    ready_gauge: &'a std::sync::atomic::AtomicU64,
    fan_outs: &'a std::sync::atomic::AtomicU64,
    failures: &'a std::sync::atomic::AtomicU64,
}

async fn maintain_wallet(state: &AppState, wallet: Wallet<'_>) {
    let _guard = wallet.lock.lock().await;

    let mut utxos = match state.node.fetch_utxos(wallet.public_key).await {
        Ok(utxos) => utxos,
        Err(e) => {
            warn!("could not read the {} wallet to fan it out: {e}", wallet.label);
            wallet.failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return;
        }
    };
    if wallet.label == "custody" {
        let board = state.board.read().await;
        let liabilities: u128 = board.all_exchange_accounts().map(|(_, a)| a.base_balance as u128).sum();
        let reserved: u128 = board.payments.values().filter(|p| p.status != crate::payments::Status::Confirmed)
            .filter_map(|p| match p.purpose { crate::payments::Purpose::Withdrawal { debited } => Some(debited as u128), _ => None }).sum();
        let assets: u128 = utxos.iter().map(|(_, o)| o.value as u128).sum();
        // Reshapes pay their fees only from explicitly funded surplus, never customer backing.
        if assets < liabilities + reserved + HUB_TRANSACTION_FEE as u128 {
            wallet.ready_gauge.store(crate::operator_wallet::ready_outputs(&utxos) as u64, std::sync::atomic::Ordering::Relaxed);
            return;
        }
    }
    // Covers unresolved payments *and* unresolved payout attempts -- the
    // second loop used to live here, which is exactly why the payment
    // builder went without it. See `reserve_inputs`.
    crate::payments::reserve_inputs(state, wallet.public_key, &mut utxos, &[]).await;
    let ready = crate::operator_wallet::ready_outputs(&utxos);
    wallet.ready_gauge.store(ready as u64, std::sync::atomic::Ordering::Relaxed);

    // A fan-out already on the wire has outputs the node cannot report
    // yet, so the wallet still *looks* short. Splitting again here
    // would take an output that is doing its job and hide it for a
    // block too -- the shape that turns one sweep's top-up into a
    // wallet permanently one block behind itself.
    //
    // "Still in flight" is an input that is both still in the UTXO set
    // and *marked*. Present-and-unmarked is not the same state and must
    // not be treated as one: it means the transaction is gone -- a node
    // restart drops its mempool, which is memory-only (§6.5) -- and a
    // guard that read presence alone would then wait for a confirmation
    // that is never coming, on every sweep, for the life of the process.
    // The wallet would sit at whatever shape it happened to be in.
    //
    // The cost of using the mempool as the signal is a window between
    // `submit_transaction` returning and the node marking the inputs, in
    // which this reads as lost. A second fan-out submitted there spends
    // the same inputs and the mempool rejects it for the same flat fee
    // (see `pay_from`), so it is wasted work rather than a double spend
    // -- and the window is sub-millisecond against a sweep interval of
    // sixty seconds.
    {
        let mut inflight = wallet.inflight.lock().await;
        if !inflight.is_empty() {
            let still_in_flight = utxos
                .iter()
                .any(|(marked, output)| *marked && inflight.contains(&output.hash()));
            if still_in_flight {
                debug!("{} fan-out still unconfirmed; leaving the wallet alone", wallet.label);
                return;
            }
            inflight.clear();
        }
    }

    let Some(plan) = crate::operator_wallet::plan_reshape(
        &utxos,
        HUB_TRANSACTION_FEE,
        state.operator_wallet_outputs,
    ) else {
        return;
    };

    let recipients: Vec<(PublicKey, u64)> = plan
        .shares
        .iter()
        .map(|share| (wallet.public_key.clone(), *share))
        .collect();
    // Built against the plan's own inputs rather than through
    // `build_payment_from`, so the transaction spends exactly the
    // outputs the plan chose and no others. That exactness is what lets
    // the inflight record below be a fact rather than a guess -- and
    // the shares sum to those inputs minus the fee, so there is no
    // change output to reason about either.
    let inputs: Vec<(bool, btclib::types::TransactionOutput)> =
        plan.inputs.iter().map(|output| (false, output.clone())).collect();
    let tx = match btclib::payment::build_multi_payment(
        &inputs,
        wallet.private_key,
        &recipients,
        HUB_TRANSACTION_FEE,
        wallet.public_key.clone(),
    ) {
        Ok(tx) => tx,
        Err(e) => {
            warn!("could not build the {} fan-out: {e}", wallet.label);
            wallet.failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return;
        }
    };

    // Recorded before the send, not after. `submit_transaction` is
    // fire-and-forget, so a record written afterwards would be missing
    // exactly when the send half-succeeded -- and the cost of believing
    // a fan-out is in flight when it is not is one skipped sweep, while
    // the cost of the reverse is splitting a live output every minute.
    *wallet.inflight.lock().await =
        tx.inputs.iter().map(|input| input.prev_transaction_output_hash).collect();

    match state.node.submit_transaction(tx).await {
        Ok(()) => {
            wallet.fan_outs.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            info!(
                "reshaping the {} wallet: {} input(s) worth {} into {} output(s) ({ready} spendable before)",
                wallet.label,
                plan.inputs.len(),
                plan.inputs.iter().map(|o| o.value).sum::<u64>(),
                plan.shares.len()
            );
        }
        Err(e) => {
            warn!("could not submit the {} fan-out: {e}", wallet.label);
            wallet.failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// `pay_bounty` for a *task* payout: identical funding source, key and
/// lock, but routed through `submit_task_payout` so the hub writes down
/// what it is waiting to see confirmed.
///
/// A separate function rather than a flag on `pay_bounty` because the
/// faucet is the other caller and has no task to record against. Its
/// grants are still fire-and-forget, and knowingly so -- see plan §6.5
/// for why that is the next thing to extend this to and not part of
/// this change.
async fn submit_bounty_payout(
    state: &AppState,
    task_id: Uuid,
    recipient: &PublicKey,
    amount: u64,
) -> anyhow::Result<()> {
    let _guard = state.payout_lock.lock().await;
    submit_task_payout(
        state,
        task_id,
        &state.operator_private_key,
        &state.operator_public_key,
        &[(recipient.clone(), amount)],
        &state.operator_public_key,
    )
    .await
}

/// Sweeps one confirmed `FundExchangeAccount` deposit into the pooled
/// custody address -- a third call site of `disburse_escrow`, which
/// already does exactly "pay this escrow's live balance, net of fee, to
/// an arbitrary recipient, then mark it Refunded" (the other two being
/// an ordinary refund, where `recipient == depositor`, and dispute-bond
/// forfeiture). The deposit's own ledger credit already happened inside
/// `confirm_exchange_deposit` and is immediately spendable/tradeable
/// from that moment -- this sweep only ever moves the *on-chain* money
/// to match what the ledger already promised, and is naturally
/// idempotent to retry: it only ever selects deposits still `Consumed`
/// (see `TaskBoard::unswept_exchange_deposits`), and `disburse_escrow`
/// re-checks live balance before paying anything, so a crash between
/// the sweep's on-chain payment and its `Refunded` write just costs one
/// harmless retry that pays nothing (balance already 0) and finishes
/// the status flip.
///
/// That idempotence claim was **false** until `Refunded` became durable:
/// the status flip never reached disk at all, so every deposit ever
/// swept came back `Consumed` and was re-swept at every boot for the
/// life of the deployment -- one node round trip each, serialized inside
/// the sweep pass ahead of payout resolution, growing with total history
/// rather than with live state. Harmless per pass and unbounded in
/// aggregate. Named here because a reader trusting the paragraph above
/// would have built on a false premise (plan §6.5c). Returns whether the
/// sweep is now complete (no
/// balance left to move, whether that's because it just swept
/// everything or because there was nothing to sweep in the first
/// place) -- `false` only on an actual failure worth retrying later.
pub async fn sweep_exchange_deposit(state: &AppState, deposit_id: Uuid) -> bool {
    let deposit = {
        let board = state.board.read().await;
        match board.get_pending_deposit(deposit_id) {
            Some(d) if matches!(d.purpose, EscrowPurpose::FundExchangeAccount) && matches!(d.status, EscrowStatus::Consumed | EscrowStatus::Disbursing) => {
                d.clone()
            }
            _ => return true,
        }
    };
    disburse_escrow(state, &deposit, &state.exchange_custody_public_key, EscrowCredit::None)
        .await
        .is_some()
}

/// Persists a submission's task and the submitter's reputation in one
/// transaction.
///
/// It was two, and that is not merely untidy bookkeeping: reputation is
/// the input to a task's `min_reputation` term, so a failure record that
/// did not land beside the task recording it lets a penalized agent keep
/// claiming work a poster meant to exclude them from (plan §6.5c).
async fn persist_task_and_reputation(
    state: &AppState,
    task: &Task,
    submitter: &PublicKey,
) -> Result<(), ApiError> {
    let reputation = state.board.read().await.reputation(submitter);
    state
        .store
        .save_task_and_reputation(task, submitter, &reputation)
        .map_err(|e| ApiError::Internal(e.to_string()))
}

/// A `Consensus` submission's task and every reputation record it
/// touched, in one transaction: the submitter's always, and -- once the
/// submission completed the set and resolved the task -- every other
/// assignee's, since `resolve_consensus` dings each one who disagreed or
/// never showed.
///
/// This was the worst of the split writes. The submitter's reputation
/// went through one commit with the task in another, and everyone
/// else's through a third whose error was **logged and dropped**, so a
/// lost batch silently forgave every agent who lost that round while the
/// task recording the round stayed on disk (plan §6.5c). One transaction
/// for the whole resolution, and a failure the caller hears about.
///
/// Still capped at `MAX_CONSENSUS_ASSIGNEES` records, which now bounds
/// the size of one transaction rather than a count of them.
async fn persist_consensus_submission(
    state: &AppState,
    task: &Task,
    submitter: &PublicKey,
    resolved: bool,
) -> Result<(), ApiError> {
    let entries: Vec<(PublicKey, Reputation)> = {
        let board = state.board.read().await;
        let mut entries = vec![(submitter.clone(), board.reputation(submitter))];
        if resolved {
            entries.extend(
                task.consensus_assignees()
                    .into_iter()
                    .filter(|assignee| assignee != submitter)
                    .map(|assignee| {
                        let reputation = board.reputation(&assignee);
                        (assignee, reputation)
                    }),
            );
        }
        entries
    };
    state
        .store
        .save_task_and_reputation_batch(task, &entries)
        .map_err(|e| ApiError::Internal(e.to_string()))
}

#[cfg(test)]
mod summary_tests {
    use super::*;
    use btclib::crypto::PrivateKey;

    /// A task with just the fields the summary reads. Built directly
    /// rather than through `TaskBoard`, because what is under test is the
    /// aggregation and every field it touches is set here explicitly --
    /// going through the board would make the timestamps `Utc::now()` and
    /// the bucket assertions unpinnable.
    fn task(created_at: DateTime<Utc>, bounty: u64, status: TaskStatus, tags: &[&str]) -> Task {
        Task {
            id: Uuid::new_v4(),
            description: "t".to_string(),
            bounty,
            kind: TaskKind::HashMatch { expected_output_hash: Hash::hash_bytes(b"x") },
            poster: PrivateKey::new_key().public_key(),
            status,
            claimant: None,
            claim_deadline: None,
            failed_attempts: 0,
            created_at,
            min_reputation: 0,
            close_reason: None,
            escrow_id: None,
            capabilities: tags.iter().map(|t| t.to_string()).collect(),
            settled_at: None,
        }
    }

    /// The same hex `Display` produces, which is what the API speaks.
    fn hex_of(pubkey: &PublicKey) -> String {
        pubkey.to_string()
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-11T12:00:00Z").unwrap().with_timezone(&Utc)
    }

    fn summarize(tasks: &[Task]) -> BoardSummaryDto {
        let refs: Vec<&Task> = tasks.iter().collect();
        summarize_board(&refs, now())
    }

    fn series(tasks: &[Task], query: SeriesQuery) -> MarketSeriesDto {
        let refs: Vec<&Task> = tasks.iter().collect();
        series_for(&refs, &[], now(), &query)
    }

    fn series_with_faucet(tasks: &[Task], grants: &[i64], query: SeriesQuery) -> MarketSeriesDto {
        let refs: Vec<&Task> = tasks.iter().collect();
        series_for(&refs, grants, now(), &query)
    }

    /// A task that was posted at one instant and paid at another, which
    /// is the case the whole settlement half of the series exists for.
    /// `claimant` and `Paid` together are what make `paid_payouts`
    /// return a leg -- `is_recipient_paid` checks both.
    fn settled_task(
        created_at: DateTime<Utc>,
        settled_at: DateTime<Utc>,
        bounty: u64,
        winner: &PublicKey,
        tags: &[&str],
    ) -> Task {
        let mut t = task(created_at, bounty, TaskStatus::Paid, tags);
        t.claimant = Some(winner.clone());
        t.settled_at = Some(settled_at);
        t
    }

    /// The claim that justifies carrying a second timestamp at all: a
    /// task belongs to the bucket it was *posted* in for demand, and to
    /// the bucket it *settled* in for earnings, and those are different
    /// buckets. Charting paid work against `created_at` -- the only
    /// timestamp there used to be -- would have dated every payout to
    /// the day the work was advertised.
    #[test]
    fn a_task_counts_as_posted_when_it_was_posted_and_as_paid_when_it_was_paid() {
        let winner = PrivateKey::new_key().public_key();
        let tasks = [settled_task(
            now() - Duration::hours(20),
            now() - Duration::hours(2),
            700,
            &winner,
            &["python"],
        )];
        // Twenty-four hours in four six-hour buckets: posted in the
        // first, settled in the last.
        let out = series(&tasks, ask(Some("python"), Some(24 * 3_600_000), Some(4)));
        assert_eq!(out.posted_series, vec![1, 0, 0, 0]);
        assert_eq!(out.bounty_series, vec![700, 0, 0, 0]);
        assert_eq!(out.settled_series, vec![0, 0, 0, 1]);
        assert_eq!(out.paid_bounty_series, vec![0, 0, 0, 700]);
        assert_eq!(out.posted, 1);
        assert_eq!(out.settled, 1);
        assert_eq!(out.paid_bounty, 700);
        // One winner, one transaction, one fee.
        assert_eq!(out.fees_series, vec![0, 0, 0, HUB_TRANSACTION_FEE]);
        assert_eq!(out.fees, HUB_TRANSACTION_FEE);
    }

    /// A task posted before the window but paid inside it is invisible
    /// to `posted_series` and must still appear in the paid series --
    /// the case a single-timestamp implementation gets wrong silently,
    /// by dropping the task at the window check before ever looking at
    /// when it settled.
    #[test]
    fn work_posted_before_the_window_still_counts_when_it_is_paid_inside_it() {
        let winner = PrivateKey::new_key().public_key();
        let tasks = [settled_task(
            now() - Duration::days(30),
            now() - Duration::hours(1),
            400,
            &winner,
            &[],
        )];
        let out = series(&tasks, ask(None, Some(24 * 3_600_000), Some(2)));
        assert_eq!(out.posted, 0, "posted a month ago, outside this window");
        assert_eq!(out.settled, 1, "but paid an hour ago, inside it");
        assert_eq!(out.paid_bounty, 400);
        assert_eq!(out.settled_series, vec![0, 1]);
    }

    /// Distinct per bucket and distinct over the window are different
    /// numbers, and the series must not be summable into the total. An
    /// agent working every day is one agent, not one per day.
    #[test]
    fn agents_are_counted_distinctly_per_bucket_and_again_over_the_window() {
        let regular = PrivateKey::new_key().public_key();
        let tasks = [
            settled_task(now() - Duration::hours(20), now() - Duration::hours(20), 10, &regular, &[]),
            settled_task(now() - Duration::hours(2), now() - Duration::hours(2), 10, &regular, &[]),
        ];
        let out = series(&tasks, ask(None, Some(24 * 3_600_000), Some(2)));
        // Each bucket sees the same worker plus that task's own poster,
        // and `task()` gives every task a fresh poster.
        assert_eq!(out.agents_series, vec![2, 2]);
        // Three distinct keys across the window, not four: the worker is
        // the same person in both buckets.
        assert_eq!(out.agents, 3, "summing the series would say four");
    }

    /// The faucet issues against a key, not against a kind of work, so
    /// it has no capability dimension. Returning a subset -- or zeroes --
    /// for a filtered request would be a quieter lie than saying the
    /// series is board-wide, which is what the DTO says.
    #[test]
    fn faucet_grants_are_board_wide_and_ignore_the_capability_filter() {
        let grants = [
            (now() - Duration::hours(20)).timestamp(),
            (now() - Duration::hours(2)).timestamp(),
            // Outside the window entirely.
            (now() - Duration::days(9)).timestamp(),
        ];
        let tasks = [task(now() - Duration::hours(3), 1, TaskStatus::Open, &["python"])];
        let window = ask(Some("python"), Some(24 * 3_600_000), Some(2));
        let filtered = series_with_faucet(&tasks, &grants, window);
        assert_eq!(filtered.faucet_series, vec![1, 1]);
        assert_eq!(filtered.faucet_grants, 2, "the third is older than the window");
        assert_eq!(filtered.faucet_itx, 2 * FAUCET_GRANT_AMOUNT);

        let whole_board =
            series_with_faucet(&tasks, &grants, ask(None, Some(24 * 3_600_000), Some(2)));
        assert_eq!(
            whole_board.faucet_series, filtered.faucet_series,
            "identical whatever capability was asked for"
        );
    }

    fn ask(capability: Option<&str>, window_ms: Option<u64>, buckets: Option<usize>) -> SeriesQuery {
        SeriesQuery { capability: capability.map(str::to_string), window_ms, buckets }
    }

    #[test]
    fn the_summary_reports_the_boards_age_so_a_chart_can_size_its_ranges() {
        let oldest = now() - Duration::days(3);
        let tasks = [
            task(oldest, 1, TaskStatus::Open, &[]),
            task(now() - Duration::hours(1), 1, TaskStatus::Open, &[]),
        ];
        assert_eq!(summarize(&tasks).first_task_at.as_deref(), Some(oldest.to_rfc3339()).as_deref());
        // An empty board has no age, which is distinct from an age of
        // zero -- a client offering ranges has nothing to offer.
        assert!(summarize(&[]).first_task_at.is_none());
    }

    #[test]
    fn a_series_buckets_one_market_over_the_window_it_was_asked_for() {
        // Placed mid-bucket rather than on an edge: 23h30m ago is half
        // an hour into the window, and 30m ago is half an hour from its
        // end, so neither assertion turns on which side of a boundary a
        // task on the boundary lands.
        let tasks = [
            task(now() - Duration::minutes(23 * 60 + 30), 100, TaskStatus::Open, &["python"]),
            task(now() - Duration::minutes(30), 250, TaskStatus::Open, &["python"]),
            // Another market entirely, and a third with no tags: neither
            // may reach the python series.
            task(now() - Duration::hours(2), 999, TaskStatus::Open, &["rust"]),
            task(now() - Duration::hours(2), 999, TaskStatus::Open, &[]),
        ];
        let out = series(&tasks, ask(Some("python"), Some(86_400_000), Some(24)));

        assert_eq!(out.buckets, 24);
        assert_eq!(out.posted, 2);
        assert_eq!(out.bounty, 350);
        assert_eq!(out.bounty_series.iter().sum::<u64>(), 350);
        // One bucket an hour over a day: the older task lands in the
        // first, the newer one in the last.
        assert_eq!(out.bounty_series[0], 100);
        assert_eq!(out.bounty_series[23], 250);
        assert_eq!(out.end_ms - out.start_ms, 86_400_000);
    }

    #[test]
    fn a_window_shorter_than_the_market_drops_what_falls_outside_it() {
        let tasks = [
            task(now() - Duration::days(10), 500, TaskStatus::Open, &["ocr"]),
            task(now() - Duration::hours(2), 70, TaskStatus::Open, &["ocr"]),
        ];
        // A day's window sees only the recent one. The old task is
        // dropped rather than piled into bucket zero, where it would
        // flatten the day's own shape.
        let day = series(&tasks, ask(Some("ocr"), Some(86_400_000), Some(24)));
        assert_eq!(day.posted, 1);
        assert_eq!(day.bounty, 70);
        // ...but it is still counted as open, because being on offer is
        // a fact about now rather than about the window.
        assert_eq!(day.open, 2);
        assert_eq!(day.open_bounty, 570);
    }

    #[test]
    fn a_series_defaults_its_window_to_the_market_not_the_board() {
        // The board is old; this market is an hour old. Charting it
        // against the board's age would open on a flat run of nothing.
        let tasks = [
            task(now() - Duration::days(60), 1, TaskStatus::Open, &["labeling"]),
            task(now() - Duration::minutes(30), 1, TaskStatus::Open, &["vision"]),
        ];
        assert_eq!(series(&tasks, ask(Some("vision"), None, None)).window_ms, 3_600_000, "1H");
        assert_eq!(series(&tasks, ask(Some("labeling"), None, None)).window_ms, 7_776_000_000, "90D");
    }

    #[test]
    fn an_untagged_request_charts_the_whole_board() {
        let tasks = [
            task(now() - Duration::hours(2), 10, TaskStatus::Open, &["python"]),
            task(now() - Duration::hours(2), 20, TaskStatus::Open, &[]),
        ];
        let out = series(&tasks, ask(None, Some(86_400_000), Some(24)));
        assert_eq!(out.posted, 2, "an untagged task is still a task on the board");
        assert_eq!(out.bounty, 30);
        assert!(out.capability.is_none());
    }

    #[test]
    fn series_bounds_are_clamped_rather_than_trusted() {
        let tasks = [task(now() - Duration::hours(1), 1, TaskStatus::Open, &["python"])];
        // A caller asking for a bucket per pixel of a wall display, and
        // one asking for none at all.
        assert_eq!(series(&tasks, ask(Some("python"), Some(86_400_000), Some(100_000))).buckets, MAX_SERIES_BUCKETS);
        assert_eq!(series(&tasks, ask(Some("python"), Some(86_400_000), Some(0))).buckets, 1);
        // A zero window would divide by zero working out a bucket width.
        assert_eq!(series(&tasks, ask(Some("python"), Some(0), Some(24))).window_ms, MIN_SERIES_WINDOW_MS);
    }

    #[test]
    fn a_market_that_has_never_traded_answers_empty_rather_than_failing() {
        let tasks = [task(now() - Duration::hours(1), 5, TaskStatus::Open, &["python"])];
        let out = series(&tasks, ask(Some("nonesuch"), Some(86_400_000), Some(12)));
        assert_eq!(out.posted, 0);
        assert_eq!(out.bounty_series, vec![0; 12], "a flat line, not a missing one");
        assert!(out.first_task_at.is_none());
    }

    #[test]
    fn picks_the_smallest_window_covering_the_board() {
        let recent = [task(now() - Duration::minutes(20), 1, TaskStatus::Open, &[])];
        assert_eq!(summarize(&recent).window_ms, 3_600_000, "1H");

        let older = [task(now() - Duration::hours(30), 1, TaskStatus::Open, &[])];
        assert_eq!(summarize(&older).window_ms, 604_800_000, "7D");
    }

    #[test]
    fn an_empty_board_gets_the_default_window_not_the_narrowest() {
        // The narrowest would be technically true of a board with no
        // history and tell a caller nothing.
        assert_eq!(summarize(&[]).window_ms, SUMMARY_DEFAULT_WINDOW_MS);
        assert_eq!(summarize(&[]).total_tasks, 0);
    }

    #[test]
    fn a_future_dated_task_does_not_collapse_the_window() {
        // Clock skew between a client and this host must not produce a
        // negative span.
        let skewed = [task(now() + Duration::hours(5), 1, TaskStatus::Open, &[])];
        assert_eq!(summarize(&skewed).window_ms, 3_600_000);
    }

    #[test]
    fn buckets_tasks_by_when_they_were_posted() {
        // A 1H window over 24 buckets is 2.5 minutes per bucket.
        let tasks = [
            task(now() - Duration::minutes(50), 1, TaskStatus::Open, &[]),
            task(now() - Duration::minutes(2), 1, TaskStatus::Open, &[]),
            task(now() - Duration::minutes(1), 1, TaskStatus::Open, &[]),
        ];
        let summary = summarize(&tasks);
        assert_eq!(summary.window_ms, 3_600_000);
        assert_eq!(summary.buckets, SUMMARY_BUCKETS);
        assert_eq!(summary.totals.posted_series.iter().sum::<u64>(), 3);
        // The two recent ones share the final bucket; `now` itself lands
        // in the last bucket rather than one past the end.
        assert_eq!(summary.totals.posted_series[SUMMARY_BUCKETS - 1], 2);
        assert_eq!(summary.totals.posted_series[4], 1);
    }

    #[test]
    fn drops_tasks_older_than_the_window_instead_of_piling_them_into_bucket_zero() {
        // Both are inside 7D so the window is 7D; the 8-day-old one is
        // what sets it, and must not then appear as a leading spike.
        let tasks = [
            task(now() - Duration::days(8), 1, TaskStatus::Open, &["python"]),
            task(now() - Duration::days(1), 1, TaskStatus::Open, &["python"]),
        ];
        let summary = summarize(&tasks);
        assert_eq!(summary.window_ms, 2_592_000_000, "30D covers 8 days");
        // Both fall inside 30 days, so both are charted.
        assert_eq!(summary.totals.posted_series.iter().sum::<u64>(), 2);

        // Now one genuinely outside: 100 days against a board whose
        // oldest is 100 days picks 90D, leaving it out of the window.
        let far = [
            task(now() - Duration::days(100), 1, TaskStatus::Open, &[]),
            task(now() - Duration::days(1), 1, TaskStatus::Open, &[]),
        ];
        let summary = summarize(&far);
        assert_eq!(summary.window_ms, 7_776_000_000, "90D");
        assert_eq!(summary.totals.posted_series.iter().sum::<u64>(), 1);
        assert_eq!(summary.total_tasks, 2, "still counted, just not charted");
    }

    #[test]
    fn separates_value_on_offer_from_value_settled() {
        let tasks = [
            task(now() - Duration::hours(1), 100, TaskStatus::Open, &[]),
            task(now() - Duration::hours(1), 700, TaskStatus::Paid, &[]),
            task(now() - Duration::hours(1), 500, TaskStatus::Claimed, &[]),
        ];
        let t = summarize(&tasks).totals;
        assert_eq!((t.open_tasks, t.open_bounty), (1, 100));
        assert_eq!((t.paid_tasks, t.paid_bounty), (1, 700));
    }

    #[test]
    fn reports_every_kind_even_when_the_board_has_none_of_it() {
        // An empty category is information; a missing one is a gap the
        // caller has to guess at.
        let summary = summarize(&[task(now() - Duration::hours(1), 1, TaskStatus::Open, &[])]);
        let kinds: Vec<&str> = summary.kinds.iter().map(|k| k.kind).collect();
        assert_eq!(kinds, vec!["hash_match", "consensus", "disputable"]);
        let consensus = summary.kinds.iter().find(|k| k.kind == "consensus").unwrap();
        assert_eq!(consensus.posted, 0);
        assert_eq!(consensus.posted_series.len(), SUMMARY_BUCKETS);
    }

    #[test]
    fn ranks_capabilities_by_value_on_offer() {
        let tasks = [
            task(now() - Duration::hours(1), 100, TaskStatus::Open, &["python"]),
            task(now() - Duration::hours(1), 900, TaskStatus::Open, &["ocr"]),
            task(now() - Duration::hours(1), 50, TaskStatus::Open, &["rust"]),
        ];
        let summary = summarize(&tasks);
        let order: Vec<&str> = summary.capabilities.iter().map(|c| c.capability.as_str()).collect();
        assert_eq!(order, vec!["ocr", "python", "rust"]);
    }

    #[test]
    fn carries_both_an_activity_series_and_a_value_series_per_capability() {
        let tasks = [
            task(now() - Duration::minutes(2), 400, TaskStatus::Open, &["python"]),
            task(now() - Duration::minutes(1), 600, TaskStatus::Open, &["python"]),
        ];
        let summary = summarize(&tasks);
        let python = &summary.capabilities[0];
        assert_eq!(python.posted, 2);
        assert_eq!(python.open_bounty, 1000);
        // Neither series is derivable from the other, which is why both
        // are on the wire.
        assert_eq!(python.posted_series[SUMMARY_BUCKETS - 1], 2);
        assert_eq!(python.bounty_series[SUMMARY_BUCKETS - 1], 1000);
    }

    #[test]
    fn counts_a_task_once_when_it_carries_the_same_tag_twice() {
        let tasks = [task(now() - Duration::hours(1), 250, TaskStatus::Open, &["ocr", "ocr"])];
        let summary = summarize(&tasks);
        assert_eq!(summary.capabilities.len(), 1);
        assert_eq!(summary.capabilities[0].posted, 1);
        assert_eq!(summary.capabilities[0].open_bounty, 250);
    }

    #[test]
    fn names_resolves_a_batch_and_answers_null_for_keys_it_has_never_seen() {
        let known = PrivateKey::new_key().public_key();
        let stranger = PrivateKey::new_key().public_key();
        let mut registry = crate::names::NameRegistry::new();
        registry.restore(known.clone(), "SwiftWarlock".to_string());

        let out = lookup_names(&registry, &format!("{},{}", hex_of(&known), hex_of(&stranger)));
        assert_eq!(out.get(&hex_of(&known)).unwrap().as_deref(), Some("SwiftWarlock"));
        // Present as a key, with no name -- distinct from absent, which
        // is what a malformed entry gets.
        assert!(out.contains_key(&hex_of(&stranger)));
        assert_eq!(out.get(&hex_of(&stranger)).unwrap().as_deref(), None);
    }

    #[test]
    fn names_skips_a_malformed_key_rather_than_failing_the_whole_batch() {
        let known = PrivateKey::new_key().public_key();
        let mut registry = crate::names::NameRegistry::new();
        registry.restore(known.clone(), "AmberOtter".to_string());

        let out = lookup_names(&registry, &format!("nonsense,{},,zz", hex_of(&known)));
        assert_eq!(out.len(), 1, "one good key answered, the rubbish dropped");
        assert_eq!(out.get(&hex_of(&known)).unwrap().as_deref(), Some("AmberOtter"));
    }

    #[test]
    fn names_stops_at_the_batch_ceiling() {
        let registry = crate::names::NameRegistry::new();
        let keys: Vec<String> =
            (0..MAX_NAMES_LOOKUP + 20).map(|_| hex_of(&PrivateKey::new_key().public_key())).collect();
        assert_eq!(lookup_names(&registry, &keys.join(",")).len(), MAX_NAMES_LOOKUP);
    }

    /// The leaderboard's *ordering* is likewise the part worth testing
    /// without standing a hub up: the route around it filters and
    /// slices, but which agent lands on which page is decided here.
    #[test]
    fn leaderboard_sort_reads_the_column_asked_for_and_defaults_to_earnings() {
        assert_eq!(LeaderboardSort::parse(None), LeaderboardSort::Earned);
        assert_eq!(LeaderboardSort::parse(Some("completed")), LeaderboardSort::Completed);
        assert_eq!(LeaderboardSort::parse(Some("failed")), LeaderboardSort::Failed);
        assert_eq!(LeaderboardSort::parse(Some("net_worth")), LeaderboardSort::NetWorth);
        // Unknown columns answer in the default order rather than
        // failing the request.
        assert_eq!(LeaderboardSort::parse(Some("nonsense")), LeaderboardSort::Earned);
    }

    #[test]
    fn leaderboard_sort_keys_off_the_right_figure() {
        let reputation = Reputation { completed: 7, failed: 2, total_earned: 900 };
        assert_eq!(LeaderboardSort::Earned.key(&reputation), Some(900));
        assert_eq!(LeaderboardSort::Completed.key(&reputation), Some(7));
        assert_eq!(LeaderboardSort::Failed.key(&reputation), Some(2));
        // Net worth is not in the reputation map at all, and says so
        // rather than answering a plausible zero -- a caller that ranked
        // the field on that would order it by "no data" and show a field
        // of paupers. It ranks from the sweep instead.
        assert_eq!(LeaderboardSort::NetWorth.key(&reputation), None);
    }

    /// The rule that decides where an agent the node could not price
    /// lands. It is the only column that can be missing a figure, and
    /// "last, whichever way you sorted" is the only placement that is
    /// not a guess about how rich it is.
    #[test]
    fn ranking_puts_an_agent_with_no_figure_last_in_both_directions() {
        use std::cmp::Ordering;
        // Descending is "most first"; ascending is the same question the
        // other way. Both are ordinary comparisons while both agents
        // have a figure.
        assert_eq!(compare_for_ranking(Some(9), Some(1), false), Ordering::Less);
        assert_eq!(compare_for_ranking(Some(9), Some(1), true), Ordering::Greater);

        // And neither direction lets a missing figure lead.
        for ascending in [true, false] {
            assert_eq!(compare_for_ranking(Some(0), None, ascending), Ordering::Less);
            assert_eq!(compare_for_ranking(None, Some(0), ascending), Ordering::Greater);
            // Two unpriced agents tie here and are separated by the
            // pubkey, same as any other tie -- see the paging test below.
            assert_eq!(compare_for_ranking(None, None, ascending), Ordering::Equal);
        }
    }

    /// Ties are the normal case on every column but earnings -- most of
    /// a real field has completed nothing and failed nothing -- and a
    /// page is a slice of this order. Two agents that tie must therefore
    /// land in the same order on every request, or paging serves one of
    /// them twice and the other never. This reproduces the route's
    /// comparator over a field that is *entirely* ties.
    #[test]
    fn leaderboard_ties_break_on_the_pubkey_so_paging_cannot_repeat_an_agent() {
        let field: Vec<(PublicKey, Reputation)> = (0..8)
            .map(|_| (PrivateKey::new_key().public_key(), Reputation { completed: 3, failed: 0, total_earned: 0 }))
            .collect();

        let order = |input: &[(PublicKey, Reputation)]| {
            let mut sorted = input.to_vec();
            sorted.sort_by(|a, b| {
                LeaderboardSort::Completed
                    .key(&b.1)
                    .cmp(&LeaderboardSort::Completed.key(&a.1))
                    .then_with(|| a.0.to_string().cmp(&b.0.to_string()))
            });
            sorted.into_iter().map(|(key, _)| key.to_string()).collect::<Vec<_>>()
        };

        let first = order(&field);
        // The same field arriving in a different order -- which is what a
        // HashMap's iteration does between calls -- ranks identically.
        let mut shuffled = field.clone();
        shuffled.reverse();
        assert_eq!(first, order(&shuffled));
        // And it really is a total order: no two rows compare equal.
        let mut unique = first.clone();
        unique.dedup();
        assert_eq!(unique.len(), first.len());
    }

    /// `agent_matches` is the whole of the leaderboard's search -- the
    /// route around it only ranks, filters and slices -- so these test
    /// the predicate rather than standing a hub up.
    #[test]
    fn agent_search_matches_either_half_of_a_name_case_insensitively() {
        let key = hex_of(&PrivateKey::new_key().public_key());
        assert!(agent_matches("swift", &key, Some("SwiftWarlock")));
        // The half a prefix match would miss, which is the reason this
        // is a substring: nobody knows which of the two words they are
        // holding.
        assert!(agent_matches("warlock", &key, Some("SwiftWarlock")));
        assert!(agent_matches("swiftwarlock", &key, Some("SwiftWarlock")));
        assert!(!agent_matches("otter", &key, Some("SwiftWarlock")));
    }

    #[test]
    fn agent_search_matches_a_pubkey_fragment_not_just_its_start() {
        let key = hex_of(&PrivateKey::new_key().public_key());
        assert!(agent_matches(&key[..8], &key, None));
        // The tail is what a truncated table cell shows, so it is the
        // fragment a reader is most likely to have in hand.
        assert!(agent_matches(&key[key.len() - 6..], &key, None));
        // The needle arrives lowercased (the route does it once, rather
        // than once per agent); the stored side is lowercased here, so a
        // key that reached us in upper case still answers.
        assert!(agent_matches(&key[..8], &key.to_uppercase(), None));
    }

    #[test]
    fn an_unnamed_agent_is_still_searchable_by_key() {
        let key = hex_of(&PrivateKey::new_key().public_key());
        assert!(agent_matches(&key[..6], &key, None));
        assert!(!agent_matches("swiftwarlock", &key, None));
    }

    #[test]
    fn untagged_tasks_belong_to_no_capability_but_still_count_in_the_totals() {
        let tasks = [task(now() - Duration::hours(1), 300, TaskStatus::Open, &[])];
        let summary = summarize(&tasks);
        assert!(summary.capabilities.is_empty());
        assert_eq!(summary.totals.open_bounty, 300);
    }

    /// The field order of every signed payload is a cross-language wire
    /// contract, and this is the only place it is pinned on the Rust
    /// side.
    ///
    /// The signature covers `{pubkey}:{timestamp}:{METHOD} {path}:{compact json}`,
    /// and serde emits struct fields in declaration order while Python
    /// emits dict keys in insertion order. So reordering any struct below
    /// silently breaks every Python client at runtime, on that route
    /// only, with a 401 and no explanation -- and until 2026-09-07
    /// nothing would have failed first. The cross-language fixtures in
    /// `sdk/examples/gen_fixtures.rs` use synthetic structs whose shapes
    /// have already drifted from these (its stand-in for a task omits
    /// `expected_output_hash` entirely), so they pin the *recipe* and not
    /// the payloads.
    ///
    /// Written as whole literal strings rather than field-name lists
    /// because the literal is what is actually signed: separators,
    /// numeric formatting and the absence of spaces are all part of the
    /// contract too. A failure here is not necessarily a bug -- it means
    /// the wire format changed, and the Python SDK and its own order
    /// tests have to change with it, in the same release.
    #[test]
    fn signed_payloads_serialize_in_the_order_the_python_sdk_builds_them() {
        // `serde_json::to_string` on the typed value, which is exactly
        // what `SignedEnvelope::signing_string` does. Going through
        // `serde_json::Value` first would prove nothing: its map is a
        // BTreeMap, so every key comes back alphabetical and the
        // declaration order this test exists to pin is erased on the way.
        assert_eq!(
            serde_json::to_string(&CreateTaskPayload {
                description: "d".into(),
                bounty: 1,
                expected_output_hash: "ab".into(),
                min_reputation: 2,
                capabilities: Default::default(),
            }).unwrap(),
            r#"{"description":"d","bounty":1,"expected_output_hash":"ab","min_reputation":2,"capabilities":[]}"#
        );

        assert_eq!(
            serde_json::to_string(&ClaimPayload { task_id: uuid::Uuid::nil() }).unwrap(),
            r#"{"task_id":"00000000-0000-0000-0000-000000000000"}"#
        );

        assert_eq!(
            serde_json::to_string(&SubmitPayload {
                task_id: uuid::Uuid::nil(),
                output: "o".into(),
            }).unwrap(),
            r#"{"task_id":"00000000-0000-0000-0000-000000000000","output":"o"}"#
        );

        assert_eq!(
            serde_json::to_string(&PlaceOrderPayload {
                side: crate::board::Side::Buy,
                price: 10,
                quantity: 5,
            }).unwrap(),
            r#"{"side":"buy","price":10,"quantity":5}"#
        );

        assert_eq!(
            serde_json::to_string(&WithdrawPayload { amount: 7 }).unwrap(),
            r#"{"amount":7}"#
        );

        assert_eq!(
            serde_json::to_string(&FaucetClaimPayload {
                challenge_id: uuid::Uuid::nil(),
                solution: 42,
            }).unwrap(),
            r#"{"challenge_id":"00000000-0000-0000-0000-000000000000","solution":42}"#
        );
    }
}

#[cfg(test)]
mod escrow_amount_tests {
    use super::*;

    /// `ApiError` deliberately carries no `Debug`, so the results are
    /// unwrapped through `ok()` rather than by adding a derive to a
    /// production type for the benefit of three assertions.
    fn amount(bounty: u64) -> Option<u64> {
        escrow_amount_for(bounty).ok()
    }

    #[test]
    fn an_ordinary_bounty_reserves_itself_plus_the_fee() {
        assert_eq!(amount(1_000_000), Some(1_000_000 + HUB_TRANSACTION_FEE));
        assert_eq!(amount(0), Some(HUB_TRANSACTION_FEE));
    }

    /// The exploit this closes, stated as its arithmetic. Under the
    /// release profile -- no overflow checks -- `bounty +
    /// HUB_TRANSACTION_FEE` wrapped to zero here, and a zero
    /// `required_amount` is one that `TaskBoard::confirm_escrow` accepts
    /// against a deposit address holding nothing: a free task carrying a
    /// bounty nobody funded.
    #[test]
    fn a_bounty_whose_fee_would_wrap_is_refused_rather_than_reserved() {
        let bounty = u64::MAX - HUB_TRANSACTION_FEE + 1;
        assert_eq!(
            bounty.wrapping_add(HUB_TRANSACTION_FEE),
            0,
            "this is the input that used to reserve zero, so pin it"
        );
        assert_eq!(amount(bounty), None);
        assert_eq!(amount(u64::MAX), None);
    }

    /// The boundary, both sides. The largest bounty that still leaves
    /// room for the fee is allowed, and reserves exactly `u64::MAX`.
    #[test]
    fn the_largest_bounty_that_leaves_room_for_the_fee_is_allowed() {
        let largest = u64::MAX - HUB_TRANSACTION_FEE;
        assert_eq!(amount(largest), Some(u64::MAX));
        assert_eq!(amount(largest + 1), None);
    }
}

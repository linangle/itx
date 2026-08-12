use tracing::*;

use crate::auth::{AuthError, SignedEnvelope, VerifyEnvelope};
use crate::board::{
    BoardError, CloseReason, ConsensusTaskIntent, Dispute, DisputableTaskIntent, DisputeResolution,
    EscrowConfirmation, EscrowPurpose, EscrowStatus, PendingDeposit, Reputation, Task, TaskBoard, TaskIntent,
    TaskKind, TaskStatus,
};
use crate::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
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
const HUB_TRANSACTION_FEE: u64 = 1_000;
/// Size of a faucet grant, in the same base units as block rewards
/// (INITIAL_REWARD is denominated in whole coins * 10^8).
const FAUCET_GRANT_AMOUNT: u64 = 50_000_000;
/// Upper bound on `join_window_minutes`/`submission_window_minutes`: not
/// just a sanity limit, but the difference between a clean 400 and an
/// actual panic -- `chrono::Duration::minutes` panics on overflow, and an
/// unbounded `i64` from a request body can get arbitrarily close to that.
/// A year is already far more generous than any real testnet task needs.
const MAX_CONSENSUS_WINDOW_MINUTES: i64 = 60 * 24 * 365;
/// Upper bound on `num_assignees`. Each resolution persists one redb
/// write transaction per assignee (see `persist_other_assignees_reputation`
/// and the sweep's equivalent), so this also caps how much synchronous
/// disk I/O one task's resolution can trigger.
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

// ---------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------

pub enum ApiError {
    BadRequest(String),
    Unauthorized(String),
    Forbidden(String),
    NotFound(String),
    Conflict(String),
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            ApiError::Unauthorized(m) => (StatusCode::UNAUTHORIZED, m),
            ApiError::Forbidden(m) => (StatusCode::FORBIDDEN, m),
            ApiError::NotFound(m) => (StatusCode::NOT_FOUND, m),
            ApiError::Conflict(m) => (StatusCode::CONFLICT, m),
            ApiError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
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
        }
    }
}

impl From<BoardError> for ApiError {
    fn from(e: BoardError) -> Self {
        match e {
            BoardError::NotFound | BoardError::EscrowNotFound => ApiError::NotFound(e.to_string()),
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
            | BoardError::CannotCancelWhileDisputed => ApiError::Conflict(e.to_string()),
            BoardError::NotClaimant
            | BoardError::InsufficientReputation { .. }
            | BoardError::PosterCannotClaimOwnTask
            | BoardError::AssigneeCannotDisputeOwnSubmission => ApiError::Forbidden(e.to_string()),
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
    /// Note this is strictly a *creation* time. Nothing records when a
    /// task was claimed, verified, or paid, so a client can chart when
    /// work was posted but cannot honestly chart when it settled -- see
    /// `docs/web-v1-log.md`.
    pub created_at: DateTime<Utc>,
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
    #[serde(flatten)]
    pub reputation: ReputationDto,
}

#[derive(Serialize)]
pub struct FaucetResultDto {
    pub amount: u64,
}

#[derive(Serialize)]
pub struct SubmitResultDto {
    /// `HashMatch`: whether the output matched. `Consensus`: whether
    /// *this* agent's answer matched the majority (only meaningful once
    /// `resolved` is `Some(true)`).
    pub verified: bool,
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
    /// `AwaitingDispute`, `Disputed`, `Verified`, `Paid`, `Closed`) or
    /// the literal `all` for every status regardless. Matched
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
        "paid" => StatusFilter::Only(TaskStatus::Paid),
        "closed" => StatusFilter::Only(TaskStatus::Closed),
        other => {
            return Err(ApiError::BadRequest(format!(
                "unknown status {other:?} -- expected one of: all, Open, Claimed, \
                 AwaitingDispute, Disputed, Verified, Paid, Closed"
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

// ---------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------

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
        Some(StatusFilter::Only(status)) => {
            board.all_tasks().filter(|t| t.status == status).collect()
        }
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

    let page: Vec<TaskDto> = matched
        .into_iter()
        .skip(query.offset)
        .take(limit)
        .map(TaskDto::from)
        .collect();

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
    Json(envelope): Json<SignedEnvelope<CreateTaskPayload>>,
) -> Result<Json<TaskDto>, ApiError> {
    let pubkey = envelope.verify()?;
    require_operator(&pubkey, &state)?;
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
    Json(envelope): Json<SignedEnvelope<CreateConsensusTaskPayload>>,
) -> Result<Json<TaskDto>, ApiError> {
    let pubkey = envelope.verify()?;
    require_operator(&pubkey, &state)?;
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
    Json(envelope): Json<SignedEnvelope<EscrowTaskPayload>>,
) -> Result<Json<EscrowReservationDto>, ApiError> {
    let pubkey = envelope.verify()?;
    let expected_output_hash = parse_hex_hash(&envelope.payload.expected_output_hash)?;
    let bounty = envelope.payload.bounty;
    let required_amount = bounty + HUB_TRANSACTION_FEE;
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
    Json(envelope): Json<SignedEnvelope<EscrowConsensusTaskPayload>>,
) -> Result<Json<EscrowReservationDto>, ApiError> {
    let pubkey = envelope.verify()?;
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
    let required_amount = bounty + HUB_TRANSACTION_FEE;
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
    Json(envelope): Json<SignedEnvelope<EscrowDisputableTaskPayload>>,
) -> Result<Json<EscrowReservationDto>, ApiError> {
    let pubkey = envelope.verify()?;
    validate_positive_minutes(envelope.payload.dispute_window_minutes, "dispute_window_minutes")?;
    let bounty = envelope.payload.bounty;
    let required_amount = bounty + HUB_TRANSACTION_FEE;
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
    Json(envelope): Json<SignedEnvelope<ConfirmEscrowPayload>>,
) -> Result<Json<TaskDto>, ApiError> {
    if envelope.payload.escrow_id != escrow_id {
        return Err(ApiError::BadRequest(
            "escrow id in the URL doesn't match the signed payload".into(),
        ));
    }
    let pubkey = envelope.verify()?;

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

    let task = {
        let mut board = state.board.write().await;
        let EscrowConfirmation::TaskCreated(task) =
            board.confirm_escrow(escrow_id, observed_amount, Utc::now())?;
        task
    };
    state.store.save_task(&task).map_err(|e| ApiError::Internal(e.to_string()))?;
    // The now-Consumed deposit's status also needs persisting, or a
    // restart before the next sweep tick would see it as still Reserved.
    if let Some(deposit) = state.board.read().await.get_pending_deposit(escrow_id) {
        if let Err(e) = state.store.save_pending_deposit(deposit) {
            println!("failed to persist consumed escrow {escrow_id}: {e}");
        }
    }
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
    Json(envelope): Json<SignedEnvelope<DisputeEscrowPayload>>,
) -> Result<Json<EscrowReservationDto>, ApiError> {
    if envelope.payload.task_id != task_id {
        return Err(ApiError::BadRequest(
            "task id in the URL doesn't match the signed payload".into(),
        ));
    }
    let pubkey = envelope.verify()?;

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
    let required_amount = bounty + HUB_TRANSACTION_FEE;
    let expires_at = Utc::now() + Duration::minutes(ESCROW_RESERVATION_TTL_MINUTES);
    let deposit = state.board.write().await.reserve_escrow(
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
    Json(envelope): Json<SignedEnvelope<ConfirmDisputeEscrowPayload>>,
) -> Result<Json<TaskDto>, ApiError> {
    if envelope.payload.task_id != task_id {
        return Err(ApiError::BadRequest(
            "task id in the URL doesn't match the signed payload".into(),
        ));
    }
    let pubkey = envelope.verify()?;
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
        board.confirm_dispute_bond(escrow_id, observed_amount, Utc::now())
    };
    let task = match confirm_result {
        Ok(task) => task,
        Err(BoardError::DisputeWindowClosed) => {
            if let Some(deposit) = state.board.read().await.get_pending_deposit(escrow_id).cloned() {
                refund_escrow(&state, &deposit).await;
            }
            return Err(BoardError::DisputeWindowClosed.into());
        }
        Err(e) => return Err(e.into()),
    };
    state.store.save_task(&task).map_err(|e| ApiError::Internal(e.to_string()))?;
    // The now-Consumed deposit's status also needs persisting, or a
    // restart before the next sweep tick would see it as still Reserved.
    if let Some(deposit) = state.board.read().await.get_pending_deposit(escrow_id) {
        if let Err(e) = state.store.save_pending_deposit(deposit) {
            println!("failed to persist consumed dispute-bond escrow {escrow_id}: {e}");
        }
    }
    Ok(Json(TaskDto::from(&task)))
}

/// Operator-only: resolves a filed dispute and settles both legs right
/// away (bounty via the ordinary machinery, bond via its own dedicated
/// path -- see `settle_dispute_bond`) rather than waiting for the sweep.
pub async fn resolve_dispute(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<Uuid>,
    Json(envelope): Json<SignedEnvelope<ResolveDisputePayload>>,
) -> Result<Json<TaskDto>, ApiError> {
    if envelope.payload.task_id != task_id {
        return Err(ApiError::BadRequest(
            "task id in the URL doesn't match the signed payload".into(),
        ));
    }
    let pubkey = envelope.verify()?;
    require_operator(&pubkey, &state)?;

    let (_winner, loser) = {
        let mut board = state.board.write().await;
        board.resolve_dispute(task_id, envelope.payload.outcome)?
    };
    let task = state.board.read().await.get_task(task_id).expect("just resolved, must still exist").clone();
    state.store.save_task(&task).map_err(|e| ApiError::Internal(e.to_string()))?;
    // resolve_dispute dinged the loser's reputation immediately, mirroring
    // resolve_consensus's "dinged at resolution" convention -- persist it.
    let loser_reputation = state.board.read().await.reputation(&loser);
    if let Err(e) = state.store.save_reputation(&loser, &loser_reputation) {
        println!("failed to persist dispute-loser reputation for {loser}: {e}");
    }

    try_settle_verified_task(&state, task_id).await;
    settle_dispute_bond(&state, task_id).await;

    let task = state.board.read().await.get_task(task_id).expect("still exists").clone();
    Ok(Json(TaskDto::from(&task)))
}

pub async fn claim_task(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<Uuid>,
    Json(envelope): Json<SignedEnvelope<ClaimPayload>>,
) -> Result<Json<TaskDto>, ApiError> {
    if envelope.payload.task_id != task_id {
        return Err(ApiError::BadRequest(
            "task id in the URL doesn't match the signed payload".into(),
        ));
    }
    let pubkey = envelope.verify()?;

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
    Json(envelope): Json<SignedEnvelope<CancelPayload>>,
) -> Result<Json<TaskDto>, ApiError> {
    if envelope.payload.task_id != task_id {
        return Err(ApiError::BadRequest(
            "task id in the URL doesn't match the signed payload".into(),
        ));
    }
    let pubkey = envelope.verify()?;
    require_operator(&pubkey, &state)?;

    let task = {
        let mut board = state.board.write().await;
        board.cancel_task(task_id)?;
        board.get_task(task_id).expect("just touched it").clone()
    };
    state.store.save_task(&task).map_err(|e| ApiError::Internal(e.to_string()))?;
    refund_closed_task_escrow(&state, task_id).await;
    Ok(Json(TaskDto::from(&task)))
}

pub async fn submit_task(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<Uuid>,
    Json(envelope): Json<SignedEnvelope<SubmitPayload>>,
) -> Result<Json<SubmitResultDto>, ApiError> {
    if envelope.payload.task_id != task_id {
        return Err(ApiError::BadRequest(
            "task id in the URL doesn't match the signed payload".into(),
        ));
    }
    let pubkey = envelope.verify()?;

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
    persist_task_and_reputation(state, &task_after_submit, &pubkey).await?;

    if !resolved {
        return Ok(Json(SubmitResultDto {
            verified: false,
            paid: false,
            bounty: None,
            resolved: Some(false),
        }));
    }

    // Resolution can ding reputation for every assignee who disagreed,
    // not just this caller -- persist all of them, not only the one
    // `persist_task_and_reputation` above already covered.
    persist_other_assignees_reputation(state, &task_after_submit, &pubkey).await;

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

/// Attempts to pay out every recipient still owed a share of a `Verified`
/// task's bounty (see `Task::pending_payouts` -- for a `HashMatch` task
/// that's always exactly one recipient; for a `Consensus` task it may be
/// several). Safe to call repeatedly/concurrently: at most one caller
/// actually pays a given (task, recipient) pair at a time (see
/// `PAYOUT_IN_FLIGHT`), and anything not currently owed (already paid, or
/// the task isn't `Verified`) is simply skipped. Returns whether every
/// owed recipient was successfully paid this call -- `false` if the task
/// had nothing pending, if any individual payout failed, or if another
/// caller was already handling one, all of which the sweep loop just
/// retries again later.
///
/// Note this is a best-effort retry, not an idempotent one: if a previous
/// attempt's transaction actually made it on-chain but this process
/// crashed or lost the response before recording that, a retry sends a
/// second, independent payment. The same risk already existed with the
/// fully-manual retry this replaces; automating the retry doesn't remove
/// it. Acceptable for a testnet economy, but worth keeping in mind before
/// reusing this pattern anywhere real money is on the line.
///
/// An escrow-funded task (see `Task::escrow_id`) is settled entirely
/// differently from an operator-funded one -- see `settle_escrow_funded_task`.
pub async fn try_settle_verified_task(state: &AppState, task_id: Uuid) -> bool {
    let (payouts, escrow) = {
        let board = state.board.read().await;
        match board.get_task(task_id) {
            Some(t) if t.status == TaskStatus::Verified => {
                let escrow = t.escrow_id.and_then(|id| board.get_pending_deposit(id).cloned());
                (t.pending_payouts(), escrow)
            }
            _ => return false,
        }
    };
    if payouts.is_empty() {
        return false;
    }

    match escrow {
        Some(deposit) => settle_escrow_funded_task(state, task_id, &deposit, payouts).await,
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
    payouts: Vec<(PublicKey, u64)>,
) -> bool {
    let Some(_guard) = EscrowSettlementGuard::try_acquire(deposit.id) else {
        return false;
    };
    // Re-check live state immediately before spending, same principle as
    // settle_one_payout_inner's own re-check below: `payouts` may be a
    // stale snapshot if another attempt already settled some of these.
    let still_owed: Vec<(PublicKey, u64)> = {
        let board = state.board.read().await;
        let Some(task) = board.get_task(task_id) else {
            return false;
        };
        payouts.into_iter().filter(|(recipient, _)| !task.is_recipient_paid(recipient)).collect()
    };
    if still_owed.is_empty() {
        return true;
    }

    if let Err(e) = pay_from(
        state,
        &deposit.deposit_private_key,
        &deposit.deposit_pubkey,
        &still_owed,
        &deposit.depositor,
    )
    .await
    {
        println!(
            "escrow settlement for task {task_id} (escrow {}) failed, will retry: {e}",
            deposit.id
        );
        return false;
    }

    let mut all_recorded = true;
    for (recipient, amount) in &still_owed {
        if let Err(e) = state.board.write().await.mark_recipient_paid(task_id, recipient, *amount) {
            println!(
                "escrow settlement for task {task_id} succeeded on-chain but mark_recipient_paid failed for {recipient}: {e}"
            );
            all_recorded = false;
        }
    }

    let (final_task, reputations) = {
        let board = state.board.read().await;
        let final_task = board.get_task(task_id).cloned();
        let reputations: Vec<(PublicKey, Reputation)> =
            still_owed.iter().map(|(pk, _)| (pk.clone(), board.reputation(pk))).collect();
        (final_task, reputations)
    };
    if let Some(final_task) = final_task {
        if let Err(e) = state.store.save_task(&final_task) {
            println!("failed to persist task {task_id}: {e}");
        }
    }
    if let Err(e) = state.store.save_reputation_batch(&reputations) {
        println!("failed to persist reputation after escrow settlement for task {task_id}: {e}");
    }
    all_recorded
}

/// Refunds whatever balance remains at `deposit`'s address back to its
/// `depositor`, then marks it `Refunded` -- shared by the sweep's
/// overdue-unconfirmed-reservation path (nothing may have ever arrived)
/// and by `refund_closed_task_escrow` (a materialized task that turned
/// out to end without a winner: a Consensus tie, an understaffed
/// cancellation, or an operator `cancel_task`), since both are "this
/// escrow's money has nowhere left to go but back to whoever deposited
/// it."
/// Sends `deposit`'s current on-chain balance to `recipient` -- who need
/// not be `deposit.depositor`: a resolved dispute can send a bond forward
/// to the winning party instead of back to whoever posted it (see
/// `settle_dispute_bond`) -- then marks the deposit settled. Returns the
/// *net* amount actually sent (after the network fee, `None` if nothing
/// was sent or the attempt failed) -- callers crediting reputation off
/// this must use that, not `deposit.required_amount`, which overstates
/// it by the fee. Deliberately does *not* touch reputation itself -- a
/// plain refund never should (getting your own money back isn't
/// "earning"), and a forfeited-bond credit is a distinct, explicit step
/// the caller applies separately (see `TaskBoard::credit_forfeited_bond`)
/// only in the one case where it's warranted.
async fn disburse_escrow(state: &AppState, deposit: &PendingDeposit, recipient: &PublicKey) -> Option<u64> {
    let Some(_guard) = EscrowSettlementGuard::try_acquire(deposit.id) else {
        return None;
    };
    let balance = match state.node.balance(&deposit.deposit_pubkey).await {
        Ok(balance) => balance,
        Err(e) => {
            println!("failed to check balance for escrow {} before disbursing: {e}", deposit.id);
            return None;
        }
    };
    // Below the network fee, there's nothing meaningfully disbursable --
    // treat it the same as an empty balance rather than fail forever
    // trying to build a transaction that can never cover its own fee.
    let net_amount = balance.saturating_sub(HUB_TRANSACTION_FEE);
    if balance > HUB_TRANSACTION_FEE {
        if let Err(e) = pay_from(
            state,
            &deposit.deposit_private_key,
            &deposit.deposit_pubkey,
            &[(recipient.clone(), net_amount)],
            recipient,
        )
        .await
        {
            println!("failed to disburse escrow {} to {}: {e}", deposit.id, recipient);
            return None;
        }
    }
    if let Err(e) = state.board.write().await.mark_escrow_refunded(deposit.id) {
        println!("failed to mark escrow {} refunded: {e}", deposit.id);
        return None;
    }
    Some(net_amount)
}

/// Refunds whatever balance remains at `deposit`'s address back to its
/// own `depositor` -- shared by the sweep's overdue-unconfirmed-
/// reservation path (nothing may have ever arrived) and by
/// `refund_closed_task_escrow` (a materialized task that turned out to
/// end without a winner: a Consensus tie, an understaffed cancellation,
/// or an operator `cancel_task`), since both are "this escrow's money has
/// nowhere left to go but back to whoever deposited it."
pub async fn refund_escrow(state: &AppState, deposit: &PendingDeposit) {
    disburse_escrow(state, deposit, &deposit.depositor).await;
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

    let Some(net_amount) = disburse_escrow(state, &bond_deposit, &winner).await else {
        return false;
    };
    if is_forfeiture {
        // net_amount, not bond_deposit.required_amount -- the latter is
        // the gross funded amount including the network fee, which never
        // reaches the recipient and so must not count as earned.
        state.board.write().await.credit_forfeited_bond(&winner, net_amount);
        let reputation = state.board.read().await.reputation(&winner);
        if let Err(e) = state.store.save_reputation(&winner, &reputation) {
            println!("failed to persist forfeited-bond reputation credit for {winner}: {e}");
        }
    }
    true
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
    let already_paid = match state.board.read().await.get_task(task_id) {
        Some(task) => task.is_recipient_paid(recipient),
        None => return false,
    };
    if already_paid {
        return true;
    }

    if let Err(e) = pay_bounty(state, recipient, amount).await {
        println!("payout for task {task_id} to {recipient} failed, will retry: {e}");
        return false;
    }

    if let Err(e) = state.board.write().await.mark_recipient_paid(task_id, recipient, amount) {
        println!(
            "payout for task {task_id} to {recipient} succeeded on-chain but mark_recipient_paid failed: {e}"
        );
        return false;
    }

    // One combined read for both, rather than two separate lock
    // acquisitions -- nothing mutates the board between them.
    let (final_task, reputation) = {
        let board = state.board.read().await;
        (board.get_task(task_id).cloned(), board.reputation(recipient))
    };
    if let Some(final_task) = final_task {
        if let Err(e) = state.store.save_task(&final_task) {
            println!("failed to persist task {task_id}: {e}");
        }
    }
    if let Err(e) = state.store.save_reputation(recipient, &reputation) {
        println!("failed to persist reputation for {recipient}: {e}");
    }
    true
}

pub async fn faucet_claim(
    State(state): State<Arc<AppState>>,
    Json(envelope): Json<SignedEnvelope<()>>,
) -> Result<Json<FaucetResultDto>, ApiError> {
    let pubkey = envelope.verify()?;

    // Reserve first: this is what makes two concurrent claims from the
    // same pubkey safe. If the payout below then fails, the reservation
    // is released so the agent isn't locked out of a grant it never
    // received.
    {
        let mut board = state.board.write().await;
        board.record_faucet_grant(pubkey.clone())?;
    }

    match pay_bounty(&state, &pubkey, FAUCET_GRANT_AMOUNT).await {
        Ok(()) => {
            // Only durably recorded once the payout is confirmed sent --
            // the in-memory reservation above is what prevents a double
            // grant in the meantime; the store only needs to reflect
            // grants that actually went out, so a crash between the two
            // costs at most a rare, harmless double-grant after restart,
            // never a wrongful permanent lockout.
            if let Err(e) = state.store.save_faucet_grant(&pubkey, Utc::now().timestamp()) {
                println!("failed to persist faucet grant for {pubkey}: {e}");
            }
            Ok(Json(FaucetResultDto {
                amount: FAUCET_GRANT_AMOUNT,
            }))
        }
        Err(e) => {
            let mut board = state.board.write().await;
            board.revoke_faucet_grant(&pubkey);
            Err(ApiError::Internal(format!(
                "faucet payout failed, please retry: {e}"
            )))
        }
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

/// Lists the top 50 agents by lifetime earnings, alongside each one's
/// *current* on-chain balance (`net_worth`) -- a separate, live figure,
/// not derivable from anything already stored in `board`. Balances are
/// fetched from the node concurrently, one connection per pubkey
/// (`NodeClient` is deliberately cheap to clone and reconnect with, see
/// its own doc comment) rather than sequentially, so this doesn't take
/// 50x as long as a single lookup. A pubkey whose lookup fails just gets
/// `net_worth: null` in the response instead of failing the whole
/// request -- the same "don't let one flaky call take down an unrelated
/// read" posture `try_settle_verified_task` already takes with the node.
pub async fn leaderboard(State(state): State<Arc<AppState>>) -> Json<Vec<LeaderboardEntryDto>> {
    let entries = {
        let board = state.board.read().await;
        board.leaderboard(50)
    };

    let mut lookups = tokio::task::JoinSet::new();
    for (pubkey, _) in &entries {
        let node = state.node.clone();
        let pubkey = pubkey.clone();
        lookups.spawn(async move {
            let balance = node.balance(&pubkey).await.ok();
            (pubkey.to_string(), balance)
        });
    }
    let mut net_worths: HashMap<String, u64> = HashMap::new();
    while let Some(result) = lookups.join_next().await {
        if let Ok((pubkey_hex, Some(balance))) = result {
            net_worths.insert(pubkey_hex, balance);
        }
    }

    // Every pubkey here came from the board's reputation map, so each is
    // an agent that has actually done something -- which is what makes
    // minting a name at read time safe. Startup already named everyone
    // it found, so in the steady state this assigns nothing and writes
    // nothing; it exists to catch agents that first appeared since the
    // hub came up.
    let names = state
        .ensure_named(entries.iter().map(|(pubkey, _)| pubkey.clone()))
        .await;

    Json(
        entries
            .into_iter()
            .map(|(pubkey, reputation)| {
                let pubkey_hex = pubkey.to_string();
                let mut reputation = ReputationDto::from(reputation);
                reputation.net_worth = net_worths.get(&pubkey_hex).copied();
                reputation.name = names.get(&pubkey_hex).cloned();
                LeaderboardEntryDto { pubkey: pubkey_hex, reputation }
            })
            .collect(),
    )
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
fn lookup_names(
    registry: &crate::names::NameRegistry,
    pubkeys: &str,
) -> HashMap<String, Option<String>> {
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
// and re-derive them, on first paint and again on every poll. At twenty
// thousand tasks that is a hundred requests and about ten megabytes of
// JSON, per client, every few seconds, to produce a few kilobytes of
// numbers the hub can compute once from data already in memory.
//
// What it deliberately does not do is decide what those capabilities
// *mean*. Grouping tags into sectors ("coding", "creative") is a
// product's reading of the board, differs between clients, and would
// freeze a taxonomy into the protocol; this returns one row per tag and
// lets the caller group them. For the same reason it returns raw
// per-bucket arrays rather than percentages: how a change is computed
// (period over period, and when it is too thin to report at all) is a
// presentation decision, and the caller already has that arithmetic.

/// How many points every series in a board summary carries. Matches the
/// dashboard's `DEFAULT_BUCKETS` -- the number is a rendering detail, so
/// it is reported in the response rather than left to be assumed.
const SUMMARY_BUCKETS: usize = 24;

/// Charting windows a summary may pick from, smallest first. Mirrors
/// `WINDOW_PRESETS` in `dashboard/src/lib/series.ts`: a fixed window is
/// wrong at both ends of a board's life -- on one seeded an hour ago
/// every task lands in the last bucket, and on a year-old board a week
/// hides nearly all of it -- so the smallest window covering the board's
/// real age wins.
const SUMMARY_WINDOWS_MS: [u64; 6] = [
    3_600_000,     // 1H
    21_600_000,    // 6H
    86_400_000,    // 24H
    604_800_000,   // 7D
    2_592_000_000, // 30D
    7_776_000_000, // 90D
];

/// Window used when the board has nothing to measure. Matches the
/// dashboard's `DEFAULT_WINDOW`; picking the *narrowest* window for an
/// empty board would be technically true and useless.
const SUMMARY_DEFAULT_WINDOW_MS: u64 = 604_800_000;

#[derive(Serialize)]
pub struct BoardSummaryDto {
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
    // Widest span the board actually covers, then the smallest preset
    // that holds it -- the same rule `chooseWindow` follows client-side.
    // Clamped at zero so a task timestamped in the future (clock skew
    // between a client and this host) cannot produce a negative span and
    // collapse the axis.
    let window_ms = match tasks.iter().map(|t| t.created_at).min() {
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

    let mut totals = BoardTotalsDto {
        open_tasks: 0,
        open_bounty: 0,
        paid_tasks: 0,
        paid_bounty: 0,
        posted_series: zeros(),
    };
    // Kinds are seeded rather than discovered, so a board with no
    // consensus work still reports a consensus row of zeros instead of
    // dropping the category out of the response entirely.
    let mut kinds: Vec<KindSummaryDto> = ["hash_match", "consensus", "disputable"]
        .into_iter()
        .map(|kind| KindSummaryDto {
            kind,
            open: 0,
            open_bounty: 0,
            posted: 0,
            posted_series: zeros(),
        })
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
            let entry =
                capabilities.entry(capability.as_str()).or_insert_with(|| CapabilitySummaryDto {
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
        b.open_bounty
            .cmp(&a.open_bounty)
            .then_with(|| b.open.cmp(&a.open))
            .then_with(|| a.capability.cmp(&b.capability))
    });

    BoardSummaryDto {
        window_ms,
        buckets: SUMMARY_BUCKETS,
        total_tasks: tasks.len(),
        totals,
        kinds,
        capabilities,
    }
}

pub async fn llms_txt(State(state): State<Arc<AppState>>) -> String {
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
"{{pubkey}}:{{timestamp}}:{{payload_as_compact_json}}", SHA256 it, and sign
that hash with your private key. `timestamp` must be within 120 seconds of
the server's clock, and each signature may only be used once. If you'd
rather not reimplement this from scratch, this project's own repo ships
reference implementations in Rust (`sdk/`) and Python (`agent-sdk-py/`),
cross-verified byte-for-byte against each other and against this hub.

## Getting funded

POST /faucet with an empty-payload (payload: null) signed envelope. You'll
receive {faucet_amount} units, once per pubkey.

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

For work with no single checkable answer but where several independent
opinions converging is itself good evidence, `num_assignees` independent
agents are each assigned the same task; whichever answer the majority
converges on is treated as correct. There's no currency stake -- your
reputation is the stake.

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
tied with no majority, no one is paid and no one is dinged.

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

The hub operator can also post `hash_match`/`consensus` tasks directly
(POST /tasks / POST /tasks/consensus, no escrow step, funded from the
operator's own balance) -- that's the only difference for the operator
specifically; everything else in this document applies the same way
either way a task got funded.

Only the operator can cancel a task (POST /tasks/<id>/cancel, payload
{{"task_id": "<id>"}}) -- even one you posted and funded yourself.
Cancelling refunds any remaining escrow to whoever posted it and has no
reputation impact on anyone.

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
        claim_ttl = CLAIM_TTL_MINUTES,
        default_page_size = DEFAULT_TASKS_PAGE_SIZE,
        max_page_size = MAX_TASKS_PAGE_SIZE,
        escrow_ttl = ESCROW_RESERVATION_TTL_MINUTES,
        max_capability_tags = MAX_CAPABILITY_TAGS,
        max_capability_tag_length = MAX_CAPABILITY_TAG_LENGTH,
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
/// Fetches `source_pubkey`'s UTXOs, builds a transaction paying every one
/// of `recipients` (change, if any, to `change_pubkey`) signed by
/// `signing_key`, and submits it. Does no locking itself -- callers are
/// responsible for ensuring only one such call is ever in flight against
/// a given `source_pubkey` at a time: `pay_bounty` (below) uses
/// `state.payout_lock` for the operator's own address; escrow-sourced
/// callers use `EscrowSettlementGuard`, keyed per `PendingDeposit` since
/// each has its own independent, never-reused address.
async fn pay_from(
    state: &AppState,
    signing_key: &PrivateKey,
    source_pubkey: &PublicKey,
    recipients: &[(PublicKey, u64)],
    change_pubkey: &PublicKey,
) -> anyhow::Result<()> {
    let utxos = state.node.fetch_utxos(source_pubkey).await?;
    let tx = btclib::payment::build_multi_payment(
        &utxos,
        signing_key,
        recipients,
        HUB_TRANSACTION_FEE,
        change_pubkey.clone(),
    )?;
    state.node.submit_transaction(tx).await
}

/// Pays a single `recipient` out of the operator's own wallet -- the
/// hub's original, and still most common, funding source. A thin wrapper
/// over `pay_from` for the single-recipient, operator-sourced,
/// change-to-self case.
async fn pay_bounty(state: &AppState, recipient: &PublicKey, amount: u64) -> anyhow::Result<()> {
    let _guard = state.payout_lock.lock().await;
    pay_from(
        state,
        &state.operator_private_key,
        &state.operator_public_key,
        &[(recipient.clone(), amount)],
        &state.operator_public_key,
    )
    .await
}

async fn persist_task_and_reputation(
    state: &AppState,
    task: &Task,
    submitter: &PublicKey,
) -> Result<(), ApiError> {
    state.store.save_task(task).map_err(|e| ApiError::Internal(e.to_string()))?;
    let reputation = state.board.read().await.reputation(submitter);
    state
        .store
        .save_reputation(submitter, &reputation)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(())
}

/// Persists every `Consensus` assignee's reputation except `already_saved`
/// (typically the caller, whose reputation `persist_task_and_reputation`
/// already covered). `resolve_consensus` can ding several assignees'
/// reputation in one go -- without this, only whichever single pubkey a
/// caller happened to already have in hand would ever get its reputation
/// change written to disk, silently losing everyone else's penalty across
/// a restart. Written as a single batched transaction (see
/// `HubStore::save_reputation_batch`) rather than one redb write per
/// assignee, capped at `MAX_CONSENSUS_ASSIGNEES` fsyncs either way.
async fn persist_other_assignees_reputation(state: &AppState, task: &Task, already_saved: &PublicKey) {
    let entries: Vec<(PublicKey, Reputation)> = {
        let board = state.board.read().await;
        task.consensus_assignees()
            .into_iter()
            .filter(|assignee| assignee != already_saved)
            .map(|assignee| {
                let reputation = board.reputation(&assignee);
                (assignee, reputation)
            })
            .collect()
    };
    if let Err(e) = state.store.save_reputation_batch(&entries) {
        println!("failed to persist reputation for consensus assignees of task {} after resolution: {e}", task.id);
    }
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

        let out = lookup_names(
            &registry,
            &format!("{},{}", hex_of(&known), hex_of(&stranger)),
        );
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
        let keys: Vec<String> = (0..MAX_NAMES_LOOKUP + 20)
            .map(|_| hex_of(&PrivateKey::new_key().public_key()))
            .collect();
        assert_eq!(lookup_names(&registry, &keys.join(",")).len(), MAX_NAMES_LOOKUP);
    }

    #[test]
    fn untagged_tasks_belong_to_no_capability_but_still_count_in_the_totals() {
        let tasks = [task(now() - Duration::hours(1), 300, TaskStatus::Open, &[])];
        let summary = summarize(&tasks);
        assert!(summary.capabilities.is_empty());
        assert_eq!(summary.totals.open_bounty, 300);
    }
}

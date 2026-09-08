use crate::escrow_key::EscrowSecret;
use btclib::crypto::{PrivateKey, PublicKey};
use btclib::sha256::Hash;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum BoardError {
    #[error("task not found")]
    NotFound,
    #[error("task is not open")]
    NotOpen,
    #[error("task is not claimed")]
    NotClaimed,
    #[error("you are not the claimant of this task")]
    NotClaimant,
    #[error("task has not been verified yet")]
    NotVerified,
    #[error("this pubkey has already claimed a faucet grant")]
    AlreadyClaimed,
    #[error("you have already joined this task")]
    AlreadyJoined,
    #[error("the window to join this task has passed")]
    JoinWindowExpired,
    #[error("the window to submit an answer for this task has passed")]
    SubmissionWindowExpired,
    #[error("this operation doesn't apply to this task's verification kind")]
    WrongTaskKind,
    #[error("you have already submitted an answer for this task")]
    AlreadySubmitted,
    #[error("this task requires at least {required} completed tasks, you have {have}")]
    InsufficientReputation { required: u64, have: u64 },
    #[error("task is already in a terminal state and cannot be cancelled")]
    AlreadyTerminal,
    #[error("no such escrow deposit")]
    EscrowNotFound,
    #[error("this escrow deposit has already been consumed or refunded")]
    EscrowNotReserved,
    #[error("this escrow deposit's reservation window has expired")]
    EscrowExpired,
    #[error("escrow deposit requires at least {required}, only {have} has been received so far")]
    EscrowUnderfunded { required: u64, have: u64 },
    #[error("a task's own poster may not claim or join it themselves")]
    PosterCannotClaimOwnTask,
    #[error("this escrow deposit's purpose doesn't match the confirmation endpoint used")]
    WrongEscrowPurpose,
    #[error("the window to dispute this task's submission has passed")]
    DisputeWindowClosed,
    #[error("the assignee may not dispute their own submission")]
    AssigneeCannotDisputeOwnSubmission,
    #[error("this task already has a dispute filed against it")]
    AlreadyDisputed,
    #[error("this task has no dispute awaiting resolution")]
    NotDisputed,
    #[error("cannot cancel a task with an active dispute in progress")]
    CannotCancelWhileDisputed,
    #[error("no such order")]
    OrderNotFound,
    #[error("you are not the owner of this order")]
    NotOrderOwner,
    #[error("order is not open")]
    OrderNotOpen,
    #[error("order price and quantity must both be non-zero")]
    InvalidOrder,
    #[error("order notional (price times quantity) overflows")]
    OrderNotionalOverflow,
    #[error("withdrawal amount must be non-zero")]
    ZeroWithdrawal,
    #[error("insufficient balance: {available} available, {required} required")]
    InsufficientBalance { available: u64, required: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Open,
    Claimed,
    /// `Disputable`-only: an answer was submitted and its dispute window
    /// is open, but no dispute has been filed (yet). Deliberately
    /// distinct from `Verified` -- reusing `Verified` here would make the
    /// existing sweep loop try to pay out immediately, before the
    /// challenge window has had a chance to close.
    AwaitingDispute,
    /// `Disputable`-only: a challenger's bond is posted and confirmed,
    /// awaiting the operator's resolution. See `Dispute`.
    Disputed,
    /// A winner (or, for a `Consensus` task, at least one) has been determined; payout to them is in flight but not yet confirmed submitted to the chain.
    Verified,
    /// Every payout this task owes has been handed to the node, and the
    /// hub is waiting to see one of them on chain. Distinct from `Paid`
    /// because `SubmitTransaction` is one-way: the wire protocol has no
    /// reply meaning accepted and none meaning rejected, so a successful
    /// send proves only that the bytes left this process. Collapsing
    /// this into `Paid` -- which is what the hub used to do -- makes a
    /// payout that never happened indistinguishable from one that did,
    /// forever. See `PayoutAttempt` for what is recorded per recipient
    /// and how the sweep resolves it.
    Submitted,
    Paid,
    /// Terminal, and the honest name for a specific thing: every one of
    /// `MAX_PAYOUT_SUBMISSIONS` attempts was *proven* not to have
    /// reached the chain (see `PayoutOutcome::NeverLanded`), so the hub
    /// stopped trying and the money is still owed.
    ///
    /// Deliberately not `Closed`. `Closed` means nobody was owed
    /// anything -- three innocent causes, no reputation dinged, escrow
    /// refunded to the poster (see `CloseReason`). Here a worker earned
    /// the bounty and has not been paid, so refunding the poster would
    /// take money from whoever did the work. The escrow is left exactly
    /// as it is and an operator resolves it by hand; `docs/deployment.md`
    /// §10.3 is the runbook. Reputation is never credited, because
    /// `mark_recipient_paid` is what credits it and it never ran.
    ///
    /// Note what this status does *not* cover: a payout whose fate the
    /// hub cannot determine stays `Submitted` and keeps alerting. Only
    /// proven loss lands here.
    PayoutFailed,
    /// A terminal status covering three distinct causes, none of which
    /// owe a payout or dock anyone's reputation -- see `Task::close_reason`
    /// for which cause it was. The first two only ever apply to a
    /// `Consensus` task; the third (an operator cancellation) applies to
    /// either kind.
    Closed,
}

/// Why a task ended in `TaskStatus::Closed`. Exposed on the wire (see
/// `handlers::TaskDto`) so a client can tell these causes apart without
/// having to infer one from the other task fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    /// `Consensus`-only: every answer was tied for the majority (or every
    /// assignee disagreed with every other) -- an honest split, nobody
    /// dinged.
    NoMajority,
    /// `Consensus`-only: never reached `num_assignees` joiners before its
    /// join deadline.
    Understaffed,
    /// Either kind: the operator cancelled it directly via
    /// `TaskBoard::cancel_task` -- e.g. a mis-posted task, or an
    /// abandoned claim/assignment the operator didn't want to wait out.
    /// Not anyone's fault, same as the other two causes.
    CancelledByOperator,
}

/// One assignee's participation in a `Consensus` task: their answer (once
/// submitted), their fixed bounty share (set once at resolution, present
/// only for winners), and whether that share has been paid.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConsensusAssignment {
    pub answer: Option<String>,
    pub paid: bool,
    /// This assignee's fixed share of the bounty, assigned once by
    /// `resolve_consensus` for winners only (`None` for losers and for
    /// anyone before resolution). Fixed at resolution time rather than
    /// recomputed from the shrinking pool of still-unpaid winners, so a
    /// partial payout failure and retry never inflates whoever is left
    /// unpaid's share.
    pub share: Option<u64>,
}


/// `Consensus` is for open-ended tasks with no single checkable answer:
/// `num_assignees` independent agents are each assigned the same task and
/// submit without seeing anyone else's answer; whichever answer the
/// majority converges on is treated as correct. There is no on-chain
/// currency stake here -- reputation is the stake. Agreeing with the
/// majority earns the usual payout and reputation credit; disagreeing
/// (or never submitting before the deadline) costs a reputation hit, the
/// same as a wrong `HashMatch` answer, without needing a separate escrow
/// mechanism.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaskKind {
    HashMatch {
        expected_output_hash: Hash,
    },
    Consensus {
        num_assignees: u32,
        /// How long the task may sit `Open` waiting for `num_assignees`
        /// joiners before it's cancelled (freeing its escrow) if it never
        /// fills -- separate from `submission_deadline`, which only
        /// starts to matter once the task is already full.
        join_deadline: DateTime<Utc>,
        /// Minutes assignees get to submit once the task actually fills
        /// up. Stored as a window rather than a fixed point in time,
        /// because the fill moment (Open -> Claimed) isn't known at
        /// creation -- `submission_deadline` is computed from this the
        /// instant that happens (see `join_consensus_task`), not from
        /// creation time. Anchoring it at creation instead was a real bug
        /// this project shipped once already: a join phase that takes a
        /// while (exactly what `join_deadline` exists to allow) could
        /// leave a task's submission window already expired the moment
        /// it fills, force-resolving it before assignees who joined in
        /// good faith ever got a chance to answer.
        submission_window_minutes: i64,
        /// `None` for the entire time the task is `Open`; set once,
        /// atomically with the Open -> Claimed transition.
        submission_deadline: Option<DateTime<Utc>>,
        assignees: BTreeMap<PublicKey, ConsensusAssignment>,
    },
    /// For open-ended work with no mechanical check (not hash-checkable
    /// like `HashMatch`, and not amenable to independent-majority voting
    /// like `Consensus` -- a single claimant does the work, and its
    /// correctness is judged by whoever's willing to back a challenge
    /// with money). Single-claimant, like `HashMatch` -- reuses
    /// `Task::claimant`/`claim_deadline` rather than duplicating them
    /// here. Operator-arbitrated: there's no mechanical oracle for "is
    /// this open-ended answer correct," and the operator is the only
    /// party with standing to be one, consistent with its existing
    /// total-trust role (it already gates all task creation and
    /// custodies every escrow).
    Disputable {
        /// Set once, by `submit_disputable_answer`.
        answer: Option<String>,
        /// Minutes from submission that the dispute window stays open.
        /// Stored as a window, not a fixed deadline, for the same reason
        /// `Consensus::submission_window_minutes` is: the anchor moment
        /// (submission) isn't known at task-creation time.
        dispute_window_minutes: i64,
        /// `None` until `submit_disputable_answer` sets it, atomically
        /// with the `Claimed` -> `AwaitingDispute` transition.
        dispute_deadline: Option<DateTime<Utc>>,
        /// `Some` once a challenger's bond is confirmed funded (see
        /// `TaskBoard::confirm_dispute_bond`), atomically with the
        /// `AwaitingDispute` -> `Disputed` transition.
        dispute: Option<Dispute>,
    },
}

/// How an operator resolved a filed `Dispute`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisputeResolution {
    /// The challenger was right: the assignee's submission was wrong.
    /// Assignee gets nothing (reputation dinged); challenger gets the
    /// task's bounty plus their own bond back.
    ChallengerWins,
    /// The assignee was right: the challenge was unfounded. Challenger
    /// gets nothing (reputation dinged) and forfeits their bond; assignee
    /// gets the task's bounty plus the forfeited bond.
    AssigneeWins,
}

/// A challenge filed against a `Disputable` task's submitted answer,
/// backed by a bond the challenger must fund before it counts (see
/// `EscrowPurpose::DisputeBond`) -- filing costs nothing, but an
/// unfounded challenge does, which is what keeps disputes from being a
/// free way to stall a payout indefinitely.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dispute {
    pub challenger: PublicKey,
    pub reason: String,
    /// The `PendingDeposit` holding the challenger's bond -- a
    /// *different* escrow than the task's own (`Task::escrow_id`), which
    /// is why settling a resolved dispute needs two separate payouts
    /// rather than one (see `handlers::settle_dispute_bond`).
    pub bond_escrow_id: Uuid,
    pub bond_amount: u64,
    pub filed_at: DateTime<Utc>,
    /// `None` until the operator calls `TaskBoard::resolve_dispute`.
    pub resolution: Option<DisputeResolution>,
}

/// How many times a single payout may be put on the wire before the hub
/// gives up and moves the task to `TaskStatus::PayoutFailed`, counting
/// the first submission. Every resubmission after the first happens only
/// once `PayoutOutcome::NeverLanded` has *proven* the previous one never
/// reached the chain, so none of them can duplicate a payment and none
/// can earn the node's duplicate-transaction strike (plan §6.2 -- three
/// strikes in ten minutes bans the box from its own node).
///
/// Four rather than "keep trying": each retry is separated by a sweep
/// interval, so four attempts span at least three minutes, which rides
/// out a node restart and a re-sync comfortably. Repeated *proven* loss
/// after that is not a transient -- it means the node is rejecting what
/// the hub builds (a fee floor moved, the operator's balance is
/// mis-modelled) and a fifth identical attempt would fail identically
/// while the loop hammers a node that is already unwell. Stopping and
/// saying so is more useful than retrying forever in silence.
pub const MAX_PAYOUT_SUBMISSIONS: u32 = 4;

/// One recipient's payout, from the moment its transaction goes onto the
/// wire until the sweep can prove what became of it.
///
/// One record per (task, recipient) even when several recipients were
/// paid by a single transaction: `build_multi_payment` gives each
/// recipient their own `TransactionOutput`, so each has its own
/// `output_hash` and each resolves independently against its own
/// address. That keeps the escrow-funded multi-winner case (one
/// transaction, several winners) and the operator-funded case (one
/// transaction each) on exactly the same code path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PayoutAttempt {
    pub task_id: Uuid,
    pub recipient: PublicKey,
    pub amount: u64,
    /// Hash of the `TransactionOutput` paying `recipient` in the
    /// submitted transaction -- the *primary* signal, and the reason
    /// this is watched rather than only the inputs: a recipient can
    /// spend a bounty the moment it lands, so a payout that plainly
    /// confirmed looks like one that never happened if you only ever
    /// examine the operator's side of it.
    ///
    /// Identifies *this attempt* rather than merely "a payment of this
    /// size to this key", because every `TransactionOutput` carries a
    /// fresh `unique_id` and so hashes differently even when value and
    /// recipient repeat (pinned by
    /// `btclib::payment::two_builds_of_the_same_payment_have_different_output_hashes`).
    /// A rebuild after a genuine loss therefore produces a different
    /// hash, which is what makes attempts distinguishable at all.
    pub output_hash: Hash,
    /// The `prev_transaction_output_hash` of every input the submitted
    /// transaction spends. The corroborating signal: still present and
    /// unspoken-for at the source means nothing consumed them, so the
    /// transaction reached neither a block nor a mempool.
    pub spent_inputs: Vec<Hash>,
    /// Whose UTXO set `spent_inputs` was drawn from -- the operator's
    /// address for an operator-funded task, the task escrow's one-time
    /// address for an escrow-funded one. Recorded rather than inferred
    /// so resolution never has to re-derive which funding source paid.
    pub source: PublicKey,
    pub submitted_at: DateTime<Utc>,
    /// How many times this payout has been put on the wire, counting the
    /// first. Capped by `MAX_PAYOUT_SUBMISSIONS`.
    pub submissions: u32,
}

/// What the node's view of two addresses says became of a
/// `PayoutAttempt`. The three rows of plan §6.5's table, and the reason
/// they are three: the middle and the last are the difference between a
/// payout that is safe to retry and one that is not, and collapsing them
/// either loses money or duplicates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayoutOutcome {
    /// The recipient holds the exact output this attempt created. It
    /// reached the node and was mined.
    Confirmed,
    /// The output is nowhere and every input it would have spent is
    /// still sitting unspent and unmarked at the source. Nothing
    /// consumed them, so the transaction is in no block and no mempool:
    /// safe -- and necessary -- to build a fresh one.
    NeverLanded,
    /// Neither of the above: the output is absent but the inputs are
    /// gone or the mempool has spoken for them. The transaction may be
    /// queued for the next block, or it may have confirmed and had its
    /// output spent onward between two sweeps.
    ///
    /// Deliberately its own answer rather than being folded into a
    /// neighbour. Treating it as `NeverLanded` risks paying a bounty
    /// twice and earns a strike for the duplicate; treating it as
    /// `Confirmed` reintroduces exactly the lie this whole mechanism
    /// removes. The hub waits and says so.
    Ambiguous,
}

impl PayoutAttempt {
    /// Resolves this attempt against what the node reports for the
    /// recipient's address and for the source address it spent from,
    /// each as `fetch_utxos` returns them: `(marked, output)`, where
    /// `marked` means the node's own mempool already has a transaction
    /// spending that output.
    ///
    /// Pure, and takes both UTXO sets as arguments rather than a node
    /// handle, so the three-way rule is testable without a node -- the
    /// ambiguous row in particular is awkward to stage against a live
    /// one and is the row that must never drift.
    pub fn resolve(
        &self,
        recipient_utxos: &[(bool, btclib::types::TransactionOutput)],
        source_utxos: &[(bool, btclib::types::TransactionOutput)],
    ) -> PayoutOutcome {
        resolve_against(&self.output_hash, &self.spent_inputs, recipient_utxos, source_utxos)
    }
}

/// The three-way rule itself, over one output hash and the inputs that
/// would have paid it.
///
/// Lifted out of `PayoutAttempt::resolve` when exchange withdrawals
/// needed the same rule (§6.5d). Both callers are thin wrappers, so the
/// rule keeps one definition and one set of tests -- a second copy would
/// be free to drift from the first, which is the failure mode the
/// cross-language envelope fixtures exist to prevent one layer down.
pub fn resolve_against(
    output_hash: &Hash,
    spent_inputs: &[Hash],
    recipient_utxos: &[(bool, btclib::types::TransactionOutput)],
    source_utxos: &[(bool, btclib::types::TransactionOutput)],
) -> PayoutOutcome {
    // Marked or not is irrelevant here: the recipient holding this
    // output at all means the transaction that created it was mined.
    // A recipient who has already spent it onward is the ambiguous
    // row below, reached by falling through.
    if recipient_utxos.iter().any(|(_, output)| output.hash() == *output_hash) {
        return PayoutOutcome::Confirmed;
    }
    // An attempt with no recorded inputs can prove nothing either
    // way, and `all()` over an empty list would answer NeverLanded
    // -- the one wrong answer here, since it authorizes a resend.
    // `build_multi_payment` never produces such a transaction, so
    // this is defence against a future caller rather than a case
    // seen today.
    if spent_inputs.is_empty() {
        return PayoutOutcome::Ambiguous;
    }
    let unspent_at_source = |wanted: &Hash| {
        source_utxos
            .iter()
            .any(|(marked, output)| !marked && output.hash() == *wanted)
    };
    if spent_inputs.iter().all(unspent_at_source) {
        PayoutOutcome::NeverLanded
    } else {
        PayoutOutcome::Ambiguous
    }
}

/// What the hub knows about an exchange withdrawal whose on-chain leg
/// was handed to the node and never acknowledged.
///
/// The custody payment is fire-and-forget like every other, so an error
/// from the send does not establish that the node never received the
/// bytes -- §6.2 settled that in the other direction, and this is the
/// dangerous direction. The handler used to credit the balance back on
/// any error and tell the client to retry, so a user whose transaction
/// *had* landed held both the coin and the balance, and could withdraw
/// the same money again.
///
/// A send that may have gone out therefore no longer reverts. The debit
/// stands and this record is written instead, durably, before the client
/// is told anything. It is what lets an operator find the money, and
/// what a resolver will read when one is built (§6.5d).
///
/// Deliberately the same shape as `PayoutAttempt` minus the task, so the
/// three-way rule applies to it unchanged.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WithdrawalAttempt {
    /// Fresh per attempt. A withdrawal has no task to be keyed by and one
    /// key may withdraw repeatedly, so the record needs an identity of
    /// its own for an operator to name one in a runbook step.
    pub id: Uuid,
    pub owner: PublicKey,
    pub amount: u64,
    /// Hash of the `TransactionOutput` paying `owner`. Same primary
    /// signal and same reason as a payout's: the recipient can spend it
    /// immediately, so watching only custody's side would read a
    /// completed withdrawal as one that never happened.
    pub output_hash: Hash,
    /// The inputs the transaction spends, all of them custody's.
    pub spent_inputs: Vec<Hash>,
    /// Custody's public key, recorded rather than inferred so a resolver
    /// never has to re-derive which wallet paid.
    pub source: PublicKey,
    pub submitted_at: DateTime<Utc>,
}

impl WithdrawalAttempt {
    /// The same three-way rule, over the same two UTXO sets.
    pub fn resolve(
        &self,
        recipient_utxos: &[(bool, btclib::types::TransactionOutput)],
        source_utxos: &[(bool, btclib::types::TransactionOutput)],
    ) -> PayoutOutcome {
        resolve_against(&self.output_hash, &self.spent_inputs, recipient_utxos, source_utxos)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: Uuid,
    pub description: String,
    pub bounty: u64,
    pub kind: TaskKind,
    pub poster: PublicKey,
    pub status: TaskStatus,
    /// `HashMatch` only -- `Consensus` tasks track their assignees inside
    /// `TaskKind::Consensus` instead, since there can be more than one.
    pub claimant: Option<PublicKey>,
    /// `HashMatch` only, same reason.
    pub claim_deadline: Option<DateTime<Utc>>,
    pub failed_attempts: u32,
    pub created_at: DateTime<Utc>,
    /// Minimum `Reputation::completed` count required to claim/join this
    /// task. `0` (the default for every task `create_task`/
    /// `create_consensus_task` produce) means ungated -- set via
    /// `set_min_reputation` right after creation to require a track
    /// record before agents may attempt higher-value or higher-trust work.
    pub min_reputation: u64,
    /// Why the task ended `Closed`, if it did. See `CloseReason`.
    pub close_reason: Option<CloseReason>,
    /// `Some` if this task was funded by an agent's own escrow deposit
    /// (see `PendingDeposit`) rather than the operator's wallet -- points
    /// at the `PendingDeposit` holding its funds, which settlement code
    /// must pay out of (or refund from) instead of the operator's
    /// address, and which `allocated_bounty()` must not double-count
    /// against the operator's balance.
    pub escrow_id: Option<Uuid>,
    /// Free-form capability tags (already normalized -- trimmed and
    /// lowercased -- by the HTTP layer before reaching here), e.g.
    /// `"python"`/`"translation"`. Empty means unrestricted, mirroring
    /// `min_reputation`'s `0`-means-ungated convention. No fixed taxonomy
    /// or registry, matching this project's existing style -- a
    /// permissionless marketplace has no natural admin to maintain one.
    pub capabilities: BTreeSet<String>,
    /// When this task's last payout was confirmed on chain, i.e. when it
    /// reached `Paid`. `None` until then, and `None` forever for a task
    /// that closed or failed without paying anyone.
    ///
    /// `created_at` alone can only answer questions about *demand* --
    /// what was posted, and when. Every question about the other half of
    /// the marketplace (bounty actually paid over time, tasks completed
    /// per day, how long settlement takes) needs the instant the money
    /// landed, and nothing recorded it: the status said a task was paid
    /// but never when, so a chart of earnings was not derivable from the
    /// board at all.
    ///
    /// `#[serde(default)]` because this arrived after stores existed.
    /// A task written before it reloads as `None`, which reads correctly
    /// as "we were not recording this yet" rather than as "settled at
    /// the epoch" -- and is why the type is `Option` rather than a
    /// sentinel timestamp.
    #[serde(default)]
    pub settled_at: Option<DateTime<Utc>>,
}

impl Task {
    /// Every (recipient, amount) pair still owed for this task right now.
    /// Empty unless the task is `Verified` or `Submitted`: for
    /// `HashMatch` that's always exactly the one claimant; for
    /// `Consensus` it's every not-yet-paid assignee with a `share` (only
    /// winners get one, fixed once by `resolve_consensus` -- see
    /// `ConsensusAssignment::share`'s own docs for why this must be a
    /// stored, not recomputed, value). The caller retries whatever this
    /// returns until it comes back empty.
    ///
    /// `Submitted` is included because a payout is not finished when its
    /// transaction is sent -- it is finished when the sweep sees it on
    /// chain (see `PayoutAttempt`). What stops a `Submitted` task's
    /// payout being sent a *second* time is not this list but
    /// `TaskBoard::unsubmitted_payouts`, which subtracts whoever already
    /// has an attempt in flight.
    pub fn pending_payouts(&self) -> Vec<(PublicKey, u64)> {
        if !matches!(self.status, TaskStatus::Verified | TaskStatus::Submitted) {
            return vec![];
        }
        self.owed_payouts()
    }

    /// What `pending_payouts` returns with the status gate removed:
    /// every (recipient, amount) this task owes and has not confirmed,
    /// whatever state it is in. `PayoutFailed` is the case that needs
    /// this -- the money is still owed there, and a DTO that reported
    /// nothing outstanding would be telling exactly the kind of
    /// comfortable lie this work exists to remove.
    pub fn owed_payouts(&self) -> Vec<(PublicKey, u64)> {
        self.allocated_payouts()
            .into_iter()
            .filter(|(recipient, _)| !self.is_recipient_paid(recipient))
            .collect()
    }

    /// The other half of `owed_payouts`: every (recipient, amount) this
    /// task has actually paid. Written as the complement rather than as
    /// its own walk of `kind` so the two can never disagree about who a
    /// task owes -- `allocated_payouts` stays the single answer to that
    /// question, and these two only differ on which side of
    /// `is_recipient_paid` they keep.
    ///
    /// This is what "bounty paid" and "agents who earned" are counted
    /// from. A `Consensus` task with three winners contributes three
    /// entries, which is also the number of chain fees settling it cost.
    pub fn paid_payouts(&self) -> Vec<(PublicKey, u64)> {
        self.allocated_payouts()
            .into_iter()
            .filter(|(recipient, _)| self.is_recipient_paid(recipient))
            .collect()
    }

    /// Every (recipient, amount) this task will *ever* pay out, whether
    /// or not it already has. Only meaningful once a winner exists, so
    /// it is empty before resolution and for a dispute still awaiting
    /// one.
    ///
    /// Split out from `owed_payouts` so the paid/unpaid subtraction
    /// lives in exactly one place: `Consensus` tracks paid-ness on each
    /// assignment while the two single-winner kinds infer it from the
    /// task's own status, and having each of `owed_payouts`,
    /// `confirmed_payout_total` and `is_recipient_paid` re-derive that
    /// per kind is how the three drift apart.
    fn allocated_payouts(&self) -> Vec<(PublicKey, u64)> {
        match &self.kind {
            TaskKind::HashMatch { .. } => match &self.claimant {
                Some(claimant) => vec![(claimant.clone(), self.bounty)],
                None => vec![],
            },
            TaskKind::Consensus { assignees, .. } => assignees
                .iter()
                .filter_map(|(pk, a)| a.share.map(|share| (pk.clone(), share)))
                .collect(),
            // Only the *bounty* leg -- drawn from the task's own escrow,
            // same as HashMatch. A resolved dispute's *bond* leg lives in
            // a different escrow entirely and is settled separately (see
            // `handlers::settle_dispute_bond`), never through this path.
            TaskKind::Disputable { dispute, .. } => {
                let winner = match dispute {
                    None => self.claimant.clone(),
                    Some(d) => match d.resolution {
                        None => None, // still Disputed, awaiting resolution -- nothing owed yet
                        Some(DisputeResolution::ChallengerWins) => Some(d.challenger.clone()),
                        Some(DisputeResolution::AssigneeWins) => self.claimant.clone(),
                    },
                };
                winner.map(|w| vec![(w, self.bounty)]).unwrap_or_default()
            }
        }
    }

    /// How much of this task's bounty the hub has actually seen land on
    /// chain, and how much it has not. The honest pair the API owes
    /// agents (plan §2 item 10): before this existed the only number
    /// available was the bounty and the only states were "will be paid"
    /// and "was paid", with no way to say "was sent and we are waiting".
    ///
    /// Together these need not sum to `bounty`: a `Consensus` task pays
    /// only its winners, and an unresolved task has allocated nothing
    /// yet.
    pub fn confirmed_payout_total(&self) -> u64 {
        self.allocated_payouts()
            .into_iter()
            .filter(|(recipient, _)| self.is_recipient_paid(recipient))
            .map(|(_, amount)| amount)
            .sum()
    }

    /// The other half of `confirmed_payout_total`: owed, and not yet
    /// observed on chain. Non-zero in `Verified` (not sent yet),
    /// `Submitted` (sent, unconfirmed) and `PayoutFailed` (proven lost,
    /// still owed) alike -- the task's own status is what distinguishes
    /// those three, and it is why the status had to grow rather than
    /// this number carrying all the meaning on its own.
    pub fn unconfirmed_payout_total(&self) -> u64 {
        self.owed_payouts().into_iter().map(|(_, amount)| amount).sum()
    }

    /// Whether `recipient` has already been paid their share of this
    /// task. Meant for a caller about to actually send money to
    /// re-confirm against live state immediately beforehand: the
    /// (recipient, amount) pair it's holding may come from an earlier,
    /// now-stale `pending_payouts()` snapshot, and another concurrent
    /// settlement attempt (the sweep vs. an immediate post-submit call,
    /// or two overlapping sweeps) may have already paid this exact
    /// recipient in the meantime.
    pub fn is_recipient_paid(&self, recipient: &PublicKey) -> bool {
        match &self.kind {
            TaskKind::HashMatch { .. } => {
                self.status == TaskStatus::Paid && self.claimant.as_ref() == Some(recipient)
            }
            TaskKind::Consensus { assignees, .. } => {
                assignees.get(recipient).is_some_and(|a| a.paid)
            }
            // Bounty leg only, same scope as pending_payouts() above --
            // the bond leg is tracked by the bond's own PendingDeposit
            // status, not here.
            TaskKind::Disputable { .. } => {
                self.status == TaskStatus::Paid && self.pending_payouts_recipient_hint() == Some(recipient.clone())
            }
        }
    }

    /// `Disputable`-only helper for `is_recipient_paid`: who *would* be
    /// owed the bounty leg, independent of whether it's actually been
    /// paid yet (unlike `pending_payouts`, which only answers while still
    /// `Verified`) -- `is_recipient_paid` needs this to still resolve to
    /// the right identity once the task has moved on to `Paid`.
    fn pending_payouts_recipient_hint(&self) -> Option<PublicKey> {
        let TaskKind::Disputable { dispute, .. } = &self.kind else { return None };
        match dispute {
            None => self.claimant.clone(),
            Some(d) => match d.resolution {
                Some(DisputeResolution::ChallengerWins) => Some(d.challenger.clone()),
                Some(DisputeResolution::AssigneeWins) | None => self.claimant.clone(),
            },
        }
    }

    /// Every assignee of a `Consensus` task (empty for `HashMatch`/
    /// `Disputable`). `resolve_consensus` can change several assignees'
    /// reputation at once (every loser, not just whoever's request
    /// happened to trigger resolution) -- callers persisting reputation
    /// after a resolution should save every pubkey this returns, not just
    /// the one they already had in hand.
    pub fn consensus_assignees(&self) -> Vec<PublicKey> {
        match &self.kind {
            TaskKind::HashMatch { .. } | TaskKind::Disputable { .. } => vec![],
            TaskKind::Consensus { assignees, .. } => assignees.keys().cloned().collect(),
        }
    }
}

/// An answer supported by strictly more than half of all assigned voters.
/// Missing submissions count against quorum; a plurality is not consensus.
fn majority_answer(assignees: &BTreeMap<PublicKey, ConsensusAssignment>) -> Option<String> {
    let mut counts: BTreeMap<&String, usize> = BTreeMap::new();
    for assignment in assignees.values() {
        if let Some(answer) = &assignment.answer {
            *counts.entry(answer).or_insert(0) += 1;
        }
    }
    let max_count = *counts.values().max()?;
    let mut top: Vec<&&String> = counts.iter().filter(|(_, &c)| c == max_count).map(|(k, _)| k).collect();
    match top.pop() {
        Some(answer) if top.is_empty() && max_count > assignees.len() / 2 => Some((*answer).clone()),
        _ => None, // either no answers at all, or a tie for first place
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Reputation {
    pub completed: u64,
    pub failed: u64,
    pub total_earned: u64,
}

/// A `PendingDeposit`'s lifecycle: `Reserved` (address handed out,
/// nothing confirmed yet) -> `Consumed` (successfully confirmed and
/// materialized into whatever `purpose` describes) or `Refunded`
/// (expired, or confirmed too late to still apply -- see
/// `EscrowPurpose::DisputeBond`'s own callers -- and whatever balance was
/// actually there has been sent back to `depositor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EscrowStatus {
    /// A durable outgoing payment exists; confirmation is still pending.
    Disbursing,
    Reserved,
    Consumed,
    Refunded,
}

/// Board-native snapshot of a task-creation intent, captured at
/// reservation time and materialized into a real `Task` only once its
/// escrow is confirmed funded (see `TaskBoard::confirm_escrow`).
/// Deliberately not `handlers::CreateTaskPayload` itself, to keep this
/// module free of HTTP-layer knowledge, matching how the rest of
/// `board.rs` already relates to `handlers.rs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskIntent {
    pub description: String,
    pub bounty: u64,
    pub expected_output_hash: Hash,
    pub min_reputation: u64,
    pub capabilities: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusTaskIntent {
    pub description: String,
    pub bounty: u64,
    pub num_assignees: u32,
    pub join_window_minutes: i64,
    pub submission_window_minutes: i64,
    pub min_reputation: u64,
    pub capabilities: BTreeSet<String>,
}

/// What a `PendingDeposit`'s funds are earmarked for.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EscrowPurpose {
    FundHashMatchTask(TaskIntent),
    FundConsensusTask(ConsensusTaskIntent),
    FundDisputableTask(DisputableTaskIntent),
    /// A challenger's bond against `task_id`'s submitted answer --
    /// confirming this attaches a `Dispute` to an *existing* task rather
    /// than materializing a new one, unlike the other two purposes (see
    /// `TaskBoard::confirm_dispute_bond`).
    DisputeBond { task_id: Uuid, reason: String },
    /// A deposit into the depositor's own exchange ledger balance
    /// (`ExchangeAccount::base_balance`) -- payload-less, since unlike a
    /// task's escrow there's no fixed target amount, any amount at or
    /// above `handlers::MIN_EXCHANGE_DEPOSIT` is accepted and credited in
    /// full (see `TaskBoard::confirm_exchange_deposit`). Rejected inside
    /// `confirm_escrow`'s guard exactly like `DisputeBond` is, for the
    /// same reason: this purpose doesn't create a `Task` either.
    FundExchangeAccount,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisputableTaskIntent {
    pub description: String,
    pub bounty: u64,
    pub dispute_window_minutes: i64,
    pub min_reputation: u64,
    pub capabilities: BTreeSet<String>,
}

/// A one-time, hub-generated keypair handed out as a deposit address for
/// a single funding intent. Any UTXO ever seen at `deposit_pubkey` is
/// unambiguously this deposit's funds by construction -- nobody else was
/// ever told this address exists -- which is what lets the hub attribute
/// an arbitrary agent's on-chain payment to a specific intent at all
/// (a plain `TransactionOutput` carries no sender field, and there is no
/// message in the wire protocol to look up an arbitrary past transaction
/// by id). The address is derived from the hub's escrow secret and this
/// deposit's own `id` -- see `reserve_escrow` and
/// `crate::escrow_key::EscrowSecret`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingDeposit {
    pub id: Uuid,
    pub depositor: PublicKey,
    pub deposit_pubkey: PublicKey,
    /// `None` for every deposit reserved since escrow keys became derived:
    /// the key is recomputed on demand from the escrow secret and `id`
    /// (see `private_key`), so the store holds no key material at all.
    ///
    /// `Some` only for deposits reserved by an older build, whose key was
    /// randomly generated and so *had* to be persisted to be recoverable.
    /// Those are read back and honoured unchanged: re-deriving a key for
    /// one of them would produce a different address than the one its
    /// depositor was already told to pay, stranding real money. They age
    /// out naturally as the deposits they belong to settle or expire.
    #[serde(default)]
    pub deposit_private_key: Option<PrivateKey>,
    pub required_amount: u64,
    pub purpose: EscrowPurpose,
    pub status: EscrowStatus,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl PendingDeposit {
    /// The private key controlling this deposit's address: the stored one
    /// if this is a legacy deposit that carries it, otherwise derived from
    /// `secret` and `id`. Every settlement path -- disbursing to a winner,
    /// refunding a depositor, sweeping into custody -- goes through here
    /// rather than reading the field directly, so neither case can be
    /// forgotten at a call site.
    pub fn private_key(&self, secret: &EscrowSecret) -> PrivateKey {
        match &self.deposit_private_key {
            Some(stored) => stored.clone(),
            None => secret.derive(self.id),
        }
    }
}

/// What successfully confirming a `PendingDeposit` produced -- what the
/// caller (an HTTP handler) does next differs by what the deposit was
/// for.
pub enum EscrowConfirmation {
    TaskCreated(Task),
}

/// One pubkey's balances on the exchange -- a pure ledger, entirely
/// separate from the on-chain UTXO set. `base_balance` is real currency,
/// swept into pooled custody once deposited (see
/// `handlers::sweep_exchange_deposit`) and withdrawable back out via the
/// existing `pay_from`. `compute_balance` is the internal-only second
/// asset: it has no on-chain representation at all and is never
/// withdrawable, only tradeable here or spendable as... nothing yet --
/// it only exists to be traded. `locked_base`/`locked_compute` are the
/// portions currently reserved by this owner's own open orders; the
/// spendable amount for a new order or a withdrawal is always the full
/// balance minus its locked counterpart, never the raw balance.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExchangeAccount {
    pub base_balance: u64,
    pub locked_base: u64,
    pub compute_balance: u64,
    pub locked_compute: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    Open,
    Filled,
    Cancelled,
}

/// A resting or fully/partially filled limit order. `price` is base
/// units per one compute unit, fixed at placement -- see `place_order`'s
/// own doc comment for exactly how a fill's execution price (the
/// *resting* order's price, which can differ from this order's own) gets
/// reconciled against the balance this order's own price locked.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: Uuid,
    pub owner: PublicKey,
    pub side: Side,
    pub price: u64,
    pub quantity: u64,
    pub filled: u64,
    pub status: OrderStatus,
    pub created_at: DateTime<Utc>,
}

/// Taker fee, in basis points of the fill's notional -- the maker side
/// of every trade pays nothing (the "rebate," relative to the taker),
/// which is what's meant to reward whoever brings resting liquidity to
/// the book rather than whoever crosses it. 10 bps (0.10%) mirrors a
/// typical real exchange's taker fee. Applied by reducing what the
/// taker *receives*, never as an extra charge beyond what `place_order`
/// already locked -- see `place_order`'s own doc comment for why that
/// matters.
pub const TAKER_FEE_BPS: u64 = 10;

/// A single match between a taker and a resting maker order, always
/// executed at the maker's (resting order's) price. `taker_side` and
/// `taker_fee` together say who paid the fee and how much: `taker_fee`
/// is denominated in compute if `taker_side` is `Buy` (the taker
/// received compute, fee taken out of that) or in the base currency if
/// `taker_side` is `Sell` (the taker received base, fee taken out of
/// that) -- without `taker_side` recorded here, a `Trade` read back
/// later (e.g. from `GET /exchange/trades`) would have no way to know
/// which asset `taker_fee` was actually charged in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub id: Uuid,
    pub buy_order_id: Uuid,
    pub sell_order_id: Uuid,
    pub buyer: PublicKey,
    pub seller: PublicKey,
    pub price: u64,
    pub quantity: u64,
    pub executed_at: DateTime<Utc>,
    pub taker_side: Side,
    pub taker_fee: u64,
}

/// Pure in-memory task-marketplace state: no I/O, no knowledge of the
/// blockchain or HTTP -- mirrors how `Blockchain` itself is a pure data
/// structure the node crate drives. `HubStore` is this module's
/// equivalent of `BlockStore`, and the HTTP handlers are this module's
/// equivalent of `node`'s message handlers: they own actually paying
/// people (an on-chain operation `TaskBoard` has no concept of) and call
/// back in here only to record the outcome.
#[derive(Debug, Clone, Default)]
pub struct TaskBoard {
    pub(crate) payments: BTreeMap<Uuid, crate::payments::Payment>,
    tasks: BTreeMap<Uuid, Task>,
    reputation: BTreeMap<PublicKey, Reputation>,
    /// Every key granted, and when. A map rather than a set because the
    /// global budget (`faucet_granted_since`) is a question about a
    /// window, and a set can only answer questions about all time.
    faucet_grants: BTreeMap<PublicKey, i64>,
    /// Which network each granted key claimed from, keyed the same way
    /// `faucet_grants` is. A parallel map rather than a wider value so
    /// the grant record keeps the shape every existing row already has;
    /// a grant made before this was recorded simply has no entry, which
    /// is the honest reading of "we did not look".
    faucet_grant_prefixes: BTreeMap<PublicKey, String>,
    pending_deposits: BTreeMap<Uuid, PendingDeposit>,
    exchange_accounts: BTreeMap<PublicKey, ExchangeAccount>,
    orders: BTreeMap<Uuid, Order>,
    trades: BTreeMap<Uuid, Trade>,
    /// Payouts handed to the node and not yet resolved, keyed by the
    /// (task, recipient) pair they pay -- see `PayoutAttempt`. Kept
    /// beside the tasks rather than inside `Task` because an entry here
    /// is a claim about the *chain*, not about the task: it is created
    /// and destroyed by the sweep, it is deleted the moment it resolves,
    /// and unlike every field of `Task` it has no meaning at all once
    /// the money is confirmed.
    payout_attempts: BTreeMap<(Uuid, PublicKey), PayoutAttempt>,
}

impl TaskBoard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn exchange_liabilities(&self) -> u64 {
        self.exchange_accounts.values().map(|a| a.base_balance)
            .chain(self.payments.values().filter(|p| p.status != crate::payments::Status::Confirmed
                && matches!(p.purpose, crate::payments::Purpose::Withdrawal { .. })).map(|p| p.amount))
            .fold(0, u64::saturating_add)
    }

    /// Total bounty already promised to tasks that haven't been paid out
    /// yet, funded from the *operator's own* balance. Callers use this
    /// against the operator's actual on-chain balance to decide whether a
    /// new operator-funded task can be safely created. Excludes
    /// escrow-funded tasks (`escrow_id.is_some()`) -- their bounty was
    /// never drawn from the operator in the first place, so counting it
    /// here would falsely block legitimate new operator-funded tasks
    /// against capacity the operator was never actually committing.
    pub fn allocated_bounty(&self) -> u64 {
        self.tasks
            .values()
            .filter(|t| !matches!(t.status, TaskStatus::Paid | TaskStatus::Closed))
            .filter(|t| t.escrow_id.is_none())
            .map(|t| t.bounty)
            .sum()
    }

    /// Total bounty riding on consensus tasks that have not settled --
    /// operator-funded and escrow-funded alike, unlike `allocated_bounty`.
    ///
    /// The exposure a colluding cluster could take. Consensus pays the
    /// answer a majority agrees on, and nothing today stops one party
    /// *being* that majority: joining costs nothing, needs no balance
    /// and no bond, and the operator's dispute mechanism covers
    /// `Disputable` tasks only -- it is not an appeal route for a
    /// consensus result. So the honest reading is that consensus
    /// verifies agreement rather than correctness, and the only real
    /// control available until that changes is a ceiling on how much can
    /// be lost to it at once.
    ///
    /// Escrow-funded tasks count even though their bounty is a poster's
    /// money rather than the operator's. The loss this bounds is not the
    /// hub's alone -- a poster who pays a colluding majority for work
    /// nobody did was defrauded by a mechanism this hub offered them,
    /// and "it was not our money" is not a position worth defending.
    ///
    /// A per-task cap would not do this job: an attacker who cannot take
    /// more than a fraction of one task simply posts more tasks.
    pub fn consensus_exposure(&self) -> u64 {
        self.tasks
            .values()
            .filter(|t| matches!(t.kind, TaskKind::Consensus { .. }))
            .filter(|t| !matches!(t.status, TaskStatus::Paid | TaskStatus::Closed))
            .map(|t| t.bounty)
            .fold(0, u64::saturating_add)
    }

    pub fn create_task(
        &mut self,
        poster: PublicKey,
        description: String,
        bounty: u64,
        expected_output_hash: Hash,
    ) -> Task {
        let task = Task {
            id: Uuid::new_v4(),
            description,
            bounty,
            kind: TaskKind::HashMatch { expected_output_hash },
            poster,
            status: TaskStatus::Open,
            claimant: None,
            claim_deadline: None,
            failed_attempts: 0,
            created_at: Utc::now(),
            min_reputation: 0,
            close_reason: None,
            escrow_id: None,
            capabilities: BTreeSet::new(),
            settled_at: None,
        };
        self.tasks.insert(task.id, task.clone());
        task
    }

    /// Creates an open-ended task verified by majority agreement across
    /// `num_assignees` independent agents instead of a single checkable
    /// hash -- see `TaskKind::Consensus`. Callers (the HTTP layer) should
    /// validate `num_assignees >= 2` before calling this; a value of 0 or
    /// 1 is accepted here without complaint but makes "majority" a
    /// degenerate, always-trivially-true concept. `submission_window_minutes`
    /// is a duration, not a deadline -- the actual `submission_deadline` is
    /// computed once the task fills up (see `join_consensus_task`), not here.
    pub fn create_consensus_task(
        &mut self,
        poster: PublicKey,
        description: String,
        bounty: u64,
        num_assignees: u32,
        join_deadline: DateTime<Utc>,
        submission_window_minutes: i64,
    ) -> Task {
        let task = Task {
            id: Uuid::new_v4(),
            description,
            bounty,
            kind: TaskKind::Consensus {
                num_assignees,
                join_deadline,
                submission_window_minutes,
                submission_deadline: None,
                assignees: BTreeMap::new(),
            },
            poster,
            status: TaskStatus::Open,
            claimant: None,
            claim_deadline: None,
            failed_attempts: 0,
            created_at: Utc::now(),
            min_reputation: 0,
            close_reason: None,
            escrow_id: None,
            capabilities: BTreeSet::new(),
            settled_at: None,
        };
        self.tasks.insert(task.id, task.clone());
        task
    }

    /// Creates an open-ended task judged by operator-arbitrated dispute
    /// rather than a mechanical check -- see `TaskKind::Disputable`.
    pub fn create_disputable_task(
        &mut self,
        poster: PublicKey,
        description: String,
        bounty: u64,
        dispute_window_minutes: i64,
    ) -> Task {
        let task = Task {
            id: Uuid::new_v4(),
            description,
            bounty,
            kind: TaskKind::Disputable {
                answer: None,
                dispute_window_minutes,
                dispute_deadline: None,
                dispute: None,
            },
            poster,
            status: TaskStatus::Open,
            claimant: None,
            claim_deadline: None,
            failed_attempts: 0,
            created_at: Utc::now(),
            min_reputation: 0,
            close_reason: None,
            escrow_id: None,
            capabilities: BTreeSet::new(),
            settled_at: None,
        };
        self.tasks.insert(task.id, task.clone());
        task
    }

    /// Sets a minimum `Reputation::completed` count required to claim or
    /// join `id` going forward. Meant to be called right after creation
    /// (while the task is still `Open`, so no one has claimed it under
    /// the old, looser bar), but not restricted to that -- tightening or
    /// loosening the bar on an already-`Claimed` task simply has no
    /// effect on whoever already claimed it.
    pub fn set_min_reputation(&mut self, id: Uuid, min_reputation: u64) -> Result<(), BoardError> {
        let task = self.tasks.get_mut(&id).ok_or(BoardError::NotFound)?;
        task.min_reputation = min_reputation;
        Ok(())
    }

    /// Sets `id`'s capability tags, same "call right after creation"
    /// convention as `set_min_reputation`. Callers (the HTTP layer, or
    /// `confirm_escrow` for the escrow-funded path) are responsible for
    /// normalizing (trim + lowercase) and validating tag count/length
    /// before calling this -- `board.rs` stores whatever it's given
    /// as-is, same division of labor as every other validated field here.
    pub fn set_capabilities(&mut self, id: Uuid, capabilities: BTreeSet<String>) -> Result<(), BoardError> {
        let task = self.tasks.get_mut(&id).ok_or(BoardError::NotFound)?;
        task.capabilities = capabilities;
        Ok(())
    }

    /// Cancels `id` directly -- an operator escape hatch for a task that
    /// doesn't need to wait out its usual expiry path (a mis-posted task
    /// still `Open`, or a `Claimed`/in-progress one the operator doesn't
    /// want to wait on `claim_deadline`/`submission_deadline` for).
    /// Works on either `TaskKind`, and leaves `claimant`/consensus
    /// `assignees` untouched as a historical record of who was involved
    /// when it was cancelled. No payout, no reputation impact either way
    /// -- same as `Understaffed`/`NoMajority`, an operator cancellation
    /// isn't the worker's fault. Rejects a task already in a terminal
    /// state (`Verified`/`Paid`/`Closed`) rather than silently no-op'ing,
    /// since `Verified`/`Paid` in particular already has (or is about to
    /// have) money moving for it.
    pub fn cancel_task(&mut self, id: Uuid) -> Result<(), BoardError> {
        let task = self.tasks.get_mut(&id).ok_or(BoardError::NotFound)?;
        if matches!(task.status, TaskStatus::Verified | TaskStatus::Submitted | TaskStatus::PayoutFailed | TaskStatus::Paid | TaskStatus::Closed) {
            return Err(BoardError::AlreadyTerminal);
        }
        // A Disputed task has a bond actively contested between two named
        // agents -- cancellation's "no payout, no reputation impact"
        // semantics are wrong once money is contested this way (unlike
        // AwaitingDispute, which has no bond yet and is fine to cancel
        // the same as any other Open/Claimed task).
        if task.status == TaskStatus::Disputed {
            return Err(BoardError::CannotCancelWhileDisputed);
        }
        task.status = TaskStatus::Closed;
        task.close_reason = Some(CloseReason::CancelledByOperator);
        Ok(())
    }

    /// Reserves a fresh single-use escrow deposit for `depositor`, whose
    /// address is derived from `escrow_secret` and the deposit's own
    /// freshly-minted `id` (see `PendingDeposit`'s doc comment for why a
    /// one-time address is the only workable design here). Inserted into
    /// the board's in-memory state immediately, same as `create_task`.
    ///
    /// Callers must still persist the returned deposit via
    /// `HubStore::save_pending_deposit` before its address is returned in
    /// an HTTP response, but what that write now protects is the deposit's
    /// *intent* -- who is paying, how much is required, and what the money
    /// is for -- rather than the only copy of a secret. A crash between
    /// this call and that commit no longer strands funds permanently:
    /// whoever holds the escrow secret can re-derive the key for any id
    /// and sweep whatever arrived. What is lost without the record is the
    /// hub's ability to attribute the payment on its own, which is reason
    /// enough to keep the ordering.
    pub fn reserve_escrow(
        &mut self,
        escrow_secret: &EscrowSecret,
        depositor: PublicKey,
        required_amount: u64,
        purpose: EscrowPurpose,
        expires_at: DateTime<Utc>,
    ) -> PendingDeposit {
        let id = Uuid::new_v4();
        let deposit_pubkey = escrow_secret.derive(id).public_key();
        let deposit = PendingDeposit {
            id,
            depositor,
            deposit_pubkey,
            deposit_private_key: None,
            required_amount,
            purpose,
            status: EscrowStatus::Reserved,
            created_at: Utc::now(),
            expires_at,
        };
        self.pending_deposits.insert(deposit.id, deposit.clone());
        deposit
    }

    pub fn get_pending_deposit(&self, id: Uuid) -> Option<&PendingDeposit> {
        self.pending_deposits.get(&id)
    }

    /// Confirms `id`'s deposit is now sufficiently funded --
    /// `observed_amount` is whatever the caller's own on-chain balance
    /// check reported (this method itself does no I/O) -- and
    /// materializes whatever `purpose` describes. Rejects a deposit
    /// that's missing, already consumed/refunded, past its `expires_at`,
    /// or still underfunded.
    pub fn confirm_escrow(
        &mut self,
        id: Uuid,
        observed_amount: u64,
        now: DateTime<Utc>,
    ) -> Result<EscrowConfirmation, BoardError> {
        let deposit = self.pending_deposits.get(&id).ok_or(BoardError::EscrowNotFound)?;
        // A DisputeBond attaches to an *existing* task rather than
        // materializing a new one, and needs an extra check this generic
        // path doesn't perform (the target task must still actually be
        // awaiting a dispute) -- see `confirm_dispute_bond` instead.
        // FundExchangeAccount doesn't create a Task at all -- see
        // `confirm_exchange_deposit` instead. Both checked before any
        // mutation below, not after.
        if matches!(deposit.purpose, EscrowPurpose::DisputeBond { .. } | EscrowPurpose::FundExchangeAccount) {
            return Err(BoardError::WrongEscrowPurpose);
        }
        if deposit.status != EscrowStatus::Reserved {
            return Err(BoardError::EscrowNotReserved);
        }
        if now > deposit.expires_at {
            return Err(BoardError::EscrowExpired);
        }
        if observed_amount < deposit.required_amount {
            return Err(BoardError::EscrowUnderfunded {
                required: deposit.required_amount,
                have: observed_amount,
            });
        }

        let depositor = deposit.depositor.clone();
        let purpose = deposit.purpose.clone();
        self.pending_deposits
            .get_mut(&id)
            .expect("existence checked above")
            .status = EscrowStatus::Consumed;

        let task_id = match purpose {
            EscrowPurpose::FundHashMatchTask(intent) => {
                let task = self.create_task(depositor, intent.description, intent.bounty, intent.expected_output_hash);
                if intent.min_reputation > 0 {
                    self.set_min_reputation(task.id, intent.min_reputation)
                        .expect("task was just created under the same lock, it must still exist");
                }
                if !intent.capabilities.is_empty() {
                    self.set_capabilities(task.id, intent.capabilities)
                        .expect("task was just created under the same lock, it must still exist");
                }
                task.id
            }
            EscrowPurpose::FundConsensusTask(intent) => {
                // Anchored at confirm time, not reservation time -- the
                // same reason `submission_deadline`/`join_deadline`
                // elsewhere are anchored at the moment they start
                // mattering rather than at creation: a reservation can
                // sit unconfirmed for a while, and anchoring the join
                // window any earlier would silently shrink it.
                let join_deadline = now + chrono::Duration::minutes(intent.join_window_minutes);
                let task = self.create_consensus_task(
                    depositor,
                    intent.description,
                    intent.bounty,
                    intent.num_assignees,
                    join_deadline,
                    intent.submission_window_minutes,
                );
                if intent.min_reputation > 0 {
                    self.set_min_reputation(task.id, intent.min_reputation)
                        .expect("task was just created under the same lock, it must still exist");
                }
                if !intent.capabilities.is_empty() {
                    self.set_capabilities(task.id, intent.capabilities)
                        .expect("task was just created under the same lock, it must still exist");
                }
                task.id
            }
            EscrowPurpose::FundDisputableTask(intent) => {
                let task = self.create_disputable_task(
                    depositor,
                    intent.description,
                    intent.bounty,
                    intent.dispute_window_minutes,
                );
                if intent.min_reputation > 0 {
                    self.set_min_reputation(task.id, intent.min_reputation)
                        .expect("task was just created under the same lock, it must still exist");
                }
                if !intent.capabilities.is_empty() {
                    self.set_capabilities(task.id, intent.capabilities)
                        .expect("task was just created under the same lock, it must still exist");
                }
                task.id
            }
            EscrowPurpose::DisputeBond { .. } | EscrowPurpose::FundExchangeAccount => {
                unreachable!("rejected before any mutation, above")
            }
        };
        let task = self.tasks.get_mut(&task_id).expect("just created above, must still exist");
        task.escrow_id = Some(id);
        Ok(EscrowConfirmation::TaskCreated(task.clone()))
    }

    /// Confirms a dispute-bond deposit is now sufficiently funded and, if
    /// its target task is still actually awaiting a dispute, attaches a
    /// `Dispute` to it and transitions `AwaitingDispute` -> `Disputed`.
    /// Unlike `confirm_escrow`, a failed *timing* check here (the window
    /// closed while the payment was in flight) does **not** consume the
    /// deposit -- the caller must refund it instead of losing it, since
    /// nothing was materialized to show for it. Other failures (not
    /// found, wrong purpose, not yet Reserved, expired, underfunded)
    /// behave the same as `confirm_escrow`'s.
    pub fn confirm_dispute_bond(
        &mut self,
        escrow_id: Uuid,
        observed_amount: u64,
        now: DateTime<Utc>,
    ) -> Result<Task, BoardError> {
        let deposit = self.pending_deposits.get(&escrow_id).ok_or(BoardError::EscrowNotFound)?;
        let EscrowPurpose::DisputeBond { task_id, reason } = deposit.purpose.clone() else {
            return Err(BoardError::WrongEscrowPurpose);
        };
        if deposit.status != EscrowStatus::Reserved {
            return Err(BoardError::EscrowNotReserved);
        }
        if now > deposit.expires_at {
            return Err(BoardError::EscrowExpired);
        }
        if observed_amount < deposit.required_amount {
            return Err(BoardError::EscrowUnderfunded {
                required: deposit.required_amount,
                have: observed_amount,
            });
        }
        let challenger = deposit.depositor.clone();
        let bond_amount = deposit.required_amount;

        let task = self.tasks.get_mut(&task_id).ok_or(BoardError::NotFound)?;
        let TaskKind::Disputable { dispute_deadline, dispute, .. } = &mut task.kind else {
            return Err(BoardError::WrongTaskKind);
        };
        // The race the plan explicitly calls out: the bond's funding
        // transaction can confirm after the dispute window already
        // closed (and the sweep already finalized the task as
        // unchallenged). Reject without consuming -- the caller must
        // refund this deposit, not reopen an already-finalizing task.
        if task.status != TaskStatus::AwaitingDispute {
            return Err(BoardError::DisputeWindowClosed);
        }
        let deadline = dispute_deadline
            .expect("AwaitingDispute always has a dispute_deadline, set atomically when it got there");
        if now > deadline {
            return Err(BoardError::DisputeWindowClosed);
        }
        if dispute.is_some() {
            return Err(BoardError::AlreadyDisputed);
        }
        // Defense in depth: `create_dispute_escrow` (the HTTP handler)
        // already rejects the assignee at reservation time, but this is
        // the real enforcement boundary, same reasoning as the
        // dispute-window check just above -- an invariant a caller can
        // bypass shouldn't rely solely on handler-layer discipline.
        if task.claimant.as_ref() == Some(&challenger) {
            return Err(BoardError::AssigneeCannotDisputeOwnSubmission);
        }

        *dispute = Some(Dispute {
            challenger,
            reason,
            bond_escrow_id: escrow_id,
            bond_amount,
            filed_at: now,
            resolution: None,
        });
        task.status = TaskStatus::Disputed;
        self.pending_deposits
            .get_mut(&escrow_id)
            .expect("existence checked above")
            .status = EscrowStatus::Consumed;
        Ok(self.tasks.get(&task_id).expect("just updated above").clone())
    }

    /// Every still-`Reserved` deposit past its `expires_at` -- polled by
    /// the sweep loop, mirroring `verified_unpaid_tasks`'s role for
    /// payouts. Whether there's actually a balance to refund at each one
    /// is something only a caller with access to the node can determine,
    /// so this is a read-only listing, not an action.
    pub fn overdue_reserved_escrows(&self, now: DateTime<Utc>) -> Vec<&PendingDeposit> {
        self.pending_deposits
            .values()
            .filter(|d| d.status == EscrowStatus::Reserved && now > d.expires_at)
            .collect()
    }

    /// The `PendingDeposit` funding `task_id`, if it was escrow-funded.
    /// Used by settlement/refund code to determine which key/address to
    /// pay out of (or refund) instead of the operator's own.
    pub fn escrow_for_task(&self, task_id: Uuid) -> Option<&PendingDeposit> {
        let task = self.tasks.get(&task_id)?;
        let escrow_id = task.escrow_id?;
        self.pending_deposits.get(&escrow_id)
    }

    /// Records that `id`'s deposit has been fully dealt with -- either
    /// nothing was ever sent to it, or whatever was there has already
    /// been refunded on-chain to `depositor`. Callers must perform (or
    /// confirm the unnecessity of) that refund before calling this.
    pub fn mark_escrow_refunded(&mut self, id: Uuid) -> Result<(), BoardError> {
        let deposit = self.pending_deposits.get_mut(&id).ok_or(BoardError::EscrowNotFound)?;
        deposit.status = EscrowStatus::Refunded;
        Ok(())
    }

    /// Restores a pending deposit previously persisted by `HubStore`.
    pub fn restore_pending_deposit(&mut self, deposit: PendingDeposit) {
        self.pending_deposits.insert(deposit.id, deposit);
    }

    pub fn all_pending_deposits(&self) -> impl Iterator<Item = &PendingDeposit> {
        self.pending_deposits.values()
    }

    /// Checks `agent`'s `completed` count against `task`'s
    /// `min_reputation` bar. Shared by `claim_task` and
    /// `join_consensus_task` so both enforce the same rule the same way.
    fn check_min_reputation(&self, task: &Task, agent: &PublicKey) -> Result<(), BoardError> {
        let have = self.reputation(agent).completed;
        if have < task.min_reputation {
            return Err(BoardError::InsufficientReputation { required: task.min_reputation, have });
        }
        Ok(())
    }

    /// Restores a task exactly as previously persisted -- used only by
    /// `HubStore` when loading from disk, since `create_task` always
    /// mints a fresh id/timestamp.
    pub fn restore_task(&mut self, task: Task) {
        self.tasks.insert(task.id, task);
    }

    /// Restores a reputation record previously persisted by `HubStore`.
    pub fn restore_reputation(&mut self, pubkey: PublicKey, reputation: Reputation) {
        self.reputation.insert(pubkey, reputation);
    }

    /// Restores a faucet grant previously persisted by `HubStore`.
    pub fn restore_faucet_grant(&mut self, pubkey: PublicKey, granted_at: i64) {
        self.faucet_grants.insert(pubkey, granted_at);
    }

    pub fn get_task(&self, id: Uuid) -> Option<&Task> {
        self.tasks.get(&id)
    }

    pub fn list_open_tasks(&self) -> Vec<&Task> {
        self.tasks
            .values()
            .filter(|t| t.status == TaskStatus::Open)
            .collect()
    }

    /// Tasks that passed verification and still have a payout to *send*.
    ///
    /// Deliberately still only `Verified`, not `Submitted`: a task
    /// reaches `Submitted` precisely when nothing is left unsent (see
    /// `record_payout_attempt`), so anything with a leg still to go is
    /// here by construction. What happens to a payout after it is sent
    /// is `outstanding_payout_attempts`'s half of the sweep, not this
    /// one's.
    ///
    /// Polled periodically by the hub's sweep loop so a payout that
    /// failed to build or send is retried without needing a human to
    /// notice and resubmit it by hand.
    pub fn verified_unpaid_tasks(&self) -> Vec<&Task> {
        self.tasks
            .values()
            .filter(|t| t.status == TaskStatus::Verified)
            .collect()
    }

    /// Every `Disputable` task with a resolved dispute whose *bond* leg
    /// hasn't been disbursed yet (see `handlers::settle_dispute_bond`).
    /// Deliberately independent of the task's own status (`Verified` or
    /// already `Paid`, doesn't matter) -- unlike the bounty leg, which
    /// `verified_unpaid_tasks` already covers and which drops a task out
    /// of that list the moment it's settled, the bond leg is a *separate*
    /// escrow that can still need a retry even after the task itself has
    /// reached `Paid`.
    pub fn tasks_with_unsettled_dispute_bonds(&self) -> Vec<Uuid> {
        self.tasks
            .values()
            .filter_map(|task| {
                let TaskKind::Disputable { dispute: Some(d), .. } = &task.kind else {
                    return None;
                };
                d.resolution?;
                let bond = self.pending_deposits.get(&d.bond_escrow_id)?;
                (bond.status == EscrowStatus::Consumed).then_some(task.id)
            })
            .collect()
    }

    /// `HashMatch`/`Disputable` only -- both are single-claimant, unlike
    /// `Consensus`, which takes on multiple simultaneous assignees via
    /// `join_consensus_task` instead.
    pub fn claim_task(
        &mut self,
        id: Uuid,
        claimant: PublicKey,
        deadline: DateTime<Utc>,
    ) -> Result<(), BoardError> {
        let task = self.tasks.get(&id).ok_or(BoardError::NotFound)?;
        if !matches!(task.kind, TaskKind::HashMatch { .. } | TaskKind::Disputable { .. }) {
            return Err(BoardError::WrongTaskKind);
        }
        if task.status != TaskStatus::Open {
            return Err(BoardError::NotOpen);
        }
        // Applies regardless of who posted it or how it was funded: even
        // for an operator-posted task this closes a latent (if harmless
        // today) hole, and it's what keeps an agent-funded task (see
        // `confirm_escrow`) from letting its own poster farm reputation
        // by claiming and solving something they posted themselves.
        if task.poster == claimant {
            return Err(BoardError::PosterCannotClaimOwnTask);
        }
        self.check_min_reputation(task, &claimant)?;

        let task = self.tasks.get_mut(&id).expect("existence already checked above");
        task.status = TaskStatus::Claimed;
        task.claimant = Some(claimant);
        task.claim_deadline = Some(deadline);
        Ok(())
    }

    /// Joins `agent` to an open `Consensus` task as one of its
    /// independent assignees. Once `num_assignees` have joined, the task
    /// closes to new joiners, moves to `Claimed`, and its
    /// `submission_deadline` is set from this exact moment (not from
    /// creation -- see `TaskKind::Consensus::submission_window_minutes`).
    pub fn join_consensus_task(&mut self, id: Uuid, agent: PublicKey) -> Result<(), BoardError> {
        let task = self.tasks.get(&id).ok_or(BoardError::NotFound)?;
        let TaskKind::Consensus { join_deadline, .. } = &task.kind else {
            return Err(BoardError::WrongTaskKind);
        };
        // Status checked before the deadline (not after): once a task is
        // no longer Open, its join_deadline is stale/irrelevant, and
        // NotOpen is the more accurate error than JoinWindowExpired for
        // e.g. a late retry against a task that filled up minutes ago.
        if task.status != TaskStatus::Open {
            return Err(BoardError::NotOpen);
        }
        // Defensive, not just an optimization: without this, a join
        // landing in the gap between the deadline passing and the next
        // sweep's `cancel_understaffed_consensus_tasks` call could bring
        // a task that should already be dead back to `Claimed`.
        if Utc::now() > *join_deadline {
            return Err(BoardError::JoinWindowExpired);
        }
        // See the identical check (and its doc comment) in `claim_task`.
        if task.poster == agent {
            return Err(BoardError::PosterCannotClaimOwnTask);
        }
        self.check_min_reputation(task, &agent)?;

        let task = self.tasks.get_mut(&id).expect("existence and kind already checked above");
        let TaskKind::Consensus {
            num_assignees,
            submission_window_minutes,
            submission_deadline,
            assignees,
            ..
        } = &mut task.kind
        else {
            unreachable!("kind already checked above");
        };
        if assignees.contains_key(&agent) {
            return Err(BoardError::AlreadyJoined);
        }
        assignees.insert(agent, ConsensusAssignment::default());
        let just_filled = assignees.len() as u32 >= *num_assignees;
        if just_filled {
            *submission_deadline = Some(Utc::now() + chrono::Duration::minutes(*submission_window_minutes));
        }
        if just_filled {
            task.status = TaskStatus::Claimed;
        }
        Ok(())
    }

    /// `HashMatch` only. Checks `output_hash` against the task's
    /// verification spec. Returns whether it matched. On a mismatch, the
    /// task reopens for another attempt (by anyone, including the same
    /// agent) and the submitter takes a reputation hit; on a match, the
    /// task moves to `Verified` (the caller is expected to then actually
    /// pay out via `pending_payouts`/`mark_recipient_paid`).
    pub fn submit(
        &mut self,
        id: Uuid,
        submitter: PublicKey,
        output_hash: Hash,
    ) -> Result<bool, BoardError> {
        let task = self.tasks.get_mut(&id).ok_or(BoardError::NotFound)?;
        let TaskKind::HashMatch { expected_output_hash } = &task.kind else {
            return Err(BoardError::WrongTaskKind);
        };
        if task.status != TaskStatus::Claimed {
            return Err(BoardError::NotClaimed);
        }
        if task.claimant.as_ref() != Some(&submitter) {
            return Err(BoardError::NotClaimant);
        }

        if output_hash == *expected_output_hash {
            task.status = TaskStatus::Verified;
            Ok(true)
        } else {
            task.status = TaskStatus::Open;
            task.claimant = None;
            task.claim_deadline = None;
            task.failed_attempts += 1;
            self.reputation.entry(submitter).or_default().failed += 1;
            Ok(false)
        }
    }

    /// `Disputable` only. Records `submitter`'s answer and opens the
    /// dispute window (`Claimed` -> `AwaitingDispute`), anchored *now*
    /// rather than at task-creation time -- the same anchoring reasoning
    /// as `Consensus::submission_window_minutes`.
    pub fn submit_disputable_answer(
        &mut self,
        id: Uuid,
        submitter: PublicKey,
        answer: String,
        now: DateTime<Utc>,
    ) -> Result<(), BoardError> {
        let task = self.tasks.get_mut(&id).ok_or(BoardError::NotFound)?;
        let TaskKind::Disputable { answer: stored_answer, dispute_window_minutes, dispute_deadline, .. } =
            &mut task.kind
        else {
            return Err(BoardError::WrongTaskKind);
        };
        if task.status != TaskStatus::Claimed {
            return Err(BoardError::NotClaimed);
        }
        if task.claimant.as_ref() != Some(&submitter) {
            return Err(BoardError::NotClaimant);
        }
        *stored_answer = Some(answer);
        *dispute_deadline = Some(now + chrono::Duration::minutes(*dispute_window_minutes));
        task.status = TaskStatus::AwaitingDispute;
        Ok(())
    }

    /// Transitions any `AwaitingDispute` task whose dispute window has
    /// passed with no dispute filed straight to `Verified`, so the
    /// ordinary settlement machinery (`pending_payouts`/
    /// `try_settle_verified_task`) pays the claimant -- mirrors
    /// `resolve_expired_consensus_tasks`'s role for `Consensus`. Returns
    /// the ids finalized this way. Call periodically from a background
    /// sweep.
    pub fn finalize_unchallenged_disputable_tasks(&mut self, now: DateTime<Utc>) -> Vec<Uuid> {
        let mut finalized = Vec::new();
        for task in self.tasks.values_mut() {
            if task.status != TaskStatus::AwaitingDispute {
                continue;
            }
            let TaskKind::Disputable { dispute_deadline, dispute, .. } = &task.kind else {
                continue;
            };
            if dispute.is_some() {
                continue; // a dispute beat the sweep to it -- Disputed already, not this path
            }
            if dispute_deadline.is_some_and(|d| now > d) {
                task.status = TaskStatus::Verified;
                finalized.push(task.id);
            }
        }
        finalized
    }

    /// Resolves a filed dispute: dings the losing party's reputation
    /// immediately (matching `resolve_consensus`'s "dinged at resolution,
    /// credited only once actually paid" convention) and transitions
    /// `Disputed` -> `Verified` so the bounty leg settles through the
    /// ordinary machinery, naming whichever party won as the recipient
    /// (see `Task::pending_payouts`). Returns `(winner, loser)`. The bond
    /// leg is a *separate* escrow and is not this method's concern -- see
    /// `handlers::settle_dispute_bond`.
    pub fn resolve_dispute(
        &mut self,
        task_id: Uuid,
        outcome: DisputeResolution,
    ) -> Result<(PublicKey, PublicKey), BoardError> {
        let task = self.tasks.get_mut(&task_id).ok_or(BoardError::NotFound)?;
        if task.status != TaskStatus::Disputed {
            return Err(BoardError::NotDisputed);
        }
        let claimant = task.claimant.clone().expect("Disputed task always has a claimant");
        let TaskKind::Disputable { dispute, .. } = &mut task.kind else {
            return Err(BoardError::WrongTaskKind);
        };
        let d = dispute.as_mut().expect("Disputed status implies dispute is Some");
        // No separate "already resolved" check needed: resolving always
        // moves status off Disputed in this same call (below), and the
        // status guard above already rejects anything not Disputed --
        // so reaching here means d.resolution is still None.
        d.resolution = Some(outcome);
        let (winner, loser) = match outcome {
            DisputeResolution::ChallengerWins => (d.challenger.clone(), claimant),
            DisputeResolution::AssigneeWins => (claimant, d.challenger.clone()),
        };
        self.reputation.entry(loser.clone()).or_default().failed += 1;
        task.status = TaskStatus::Verified;
        Ok((winner, loser))
    }

    /// Credits `recipient`'s `total_earned` for a forfeited dispute bond
    /// -- deliberately *not* `completed` (that was already credited by
    /// `mark_recipient_paid` for the bounty leg) and deliberately
    /// separate from a plain refund (`refund_escrow` never touches
    /// reputation, since getting your own money back isn't "earning" --
    /// only receiving someone else's forfeited stake is).
    pub fn credit_forfeited_bond(&mut self, recipient: &PublicKey, bond_amount: u64) {
        self.reputation.entry(recipient.clone()).or_default().total_earned += bond_amount;
    }

    /// `Consensus` only. Records `agent`'s answer. Once every assignee
    /// has submitted, immediately resolves the task (see
    /// `resolve_consensus`) and returns `true`; otherwise returns `false`
    /// to indicate the task is still waiting on other assignees.
    pub fn submit_consensus_answer(
        &mut self,
        id: Uuid,
        agent: PublicKey,
        answer: String,
    ) -> Result<bool, BoardError> {
        let task = self.tasks.get_mut(&id).ok_or(BoardError::NotFound)?;
        if !matches!(task.kind, TaskKind::Consensus { .. }) {
            return Err(BoardError::WrongTaskKind);
        }
        if task.status != TaskStatus::Claimed {
            return Err(BoardError::NotClaimed);
        }
        let TaskKind::Consensus { submission_deadline, assignees, .. } = &mut task.kind else {
            unreachable!("kind already checked above");
        };
        // Defensive, mirroring `join_consensus_task`'s check against
        // `join_deadline`: without this, a submission landing in the gap
        // between the deadline passing and the next sweep's
        // `resolve_expired_consensus_tasks` call would be silently
        // accepted as a real vote instead of being treated as a no-show.
        let deadline = submission_deadline
            .expect("a Claimed consensus task always has a submission_deadline, set atomically when it filled");
        if Utc::now() > deadline {
            return Err(BoardError::SubmissionWindowExpired);
        }
        let assignment = assignees.get_mut(&agent).ok_or(BoardError::NotClaimant)?;
        if assignment.answer.is_some() {
            return Err(BoardError::AlreadySubmitted);
        }
        assignment.answer = Some(answer);

        if assignees.values().all(|a| a.answer.is_some()) {
            self.resolve_consensus(id);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Forces resolution of any `Consensus` task still `Claimed` whose
    /// submission deadline has passed, treating any assignee who never
    /// submitted as having disagreed with the majority (same as an
    /// ordinary wrong answer) rather than leaving the task stuck waiting
    /// on a no-show forever. Returns the ids resolved this way. Call
    /// periodically from a background sweep, same as `expire_claims`.
    pub fn resolve_expired_consensus_tasks(&mut self, now: DateTime<Utc>) -> Vec<Uuid> {
        let due: Vec<Uuid> = self
            .tasks
            .values()
            .filter(|t| {
                t.status == TaskStatus::Claimed
                    && matches!(&t.kind, TaskKind::Consensus { submission_deadline, .. } if submission_deadline.is_some_and(|d| now > d))
            })
            .map(|t| t.id)
            .collect();
        for id in &due {
            self.resolve_consensus(*id);
        }
        due
    }

    /// Cancels any `Consensus` task still `Open` (never reached
    /// `num_assignees` joiners) past its join deadline, transitioning it
    /// straight to `Closed` -- the same terminal, no-payout-no-dings
    /// status a tied vote reaches, since an under-subscribed task isn't
    /// anyone's fault either. Without this, a task nobody finishes
    /// joining would sit `Open` forever, permanently tying up its bounty
    /// in the operator's `allocated_bounty()`. Returns the ids cancelled
    /// this way. Call periodically from a background sweep, same as
    /// `resolve_expired_consensus_tasks`.
    ///
    /// Single-pass, unlike `resolve_expired_consensus_tasks`: that one
    /// needs a separate collect-then-act split because `resolve_consensus`
    /// takes `&mut self` (it also touches `self.reputation`), which can't
    /// be called from inside a `self.tasks.values_mut()` loop -- this
    /// function only ever touches the `Task` already in hand, so no such
    /// split is needed here.
    pub fn cancel_understaffed_consensus_tasks(&mut self, now: DateTime<Utc>) -> Vec<Uuid> {
        let mut cancelled = Vec::new();
        for task in self.tasks.values_mut() {
            if task.status == TaskStatus::Open
                && matches!(&task.kind, TaskKind::Consensus { join_deadline, .. } if now > *join_deadline)
            {
                task.status = TaskStatus::Closed;
                task.close_reason = Some(CloseReason::Understaffed);
                cancelled.push(task.id);
            }
        }
        cancelled
    }

    /// Computes the majority answer across a `Consensus` task's
    /// assignees (whoever has submitted so far -- callers only invoke
    /// this once every assignee has answered, or the deadline forced an
    /// early call), dings the reputation of everyone who didn't match it
    /// (a non-submission never matches), and transitions the task to
    /// `Verified` if there's a majority to pay out, or `Closed` if the
    /// vote was a tie (nobody paid, nobody dinged for an honest split).
    fn resolve_consensus(&mut self, id: Uuid) {
        let Some(task) = self.tasks.get(&id) else { return };
        let TaskKind::Consensus { assignees, .. } = &task.kind else {
            return;
        };
        let winning_answer = majority_answer(assignees);
        // Computed once, right now, from the full winner count -- and
        // never recomputed later. `pending_payouts`/`mark_recipient_paid`
        // only ever read this stored value back, so a payout that fails
        // partway through and gets retried can't inflate whoever is left
        // unpaid's share by dividing the bounty over a shrinking pool.
        let share = winning_answer.as_ref().map(|answer| {
            let winner_count = assignees
                .values()
                .filter(|a| a.answer.as_ref() == Some(answer))
                .count() as u64;
            task.bounty / winner_count.max(1)
        });

        if let Some(winning_answer) = &winning_answer {
            let losers: Vec<PublicKey> = assignees
                .iter()
                .filter(|(_, a)| a.answer.as_ref() != Some(winning_answer))
                .map(|(pk, _)| pk.clone())
                .collect();
            for loser in losers {
                self.reputation.entry(loser).or_default().failed += 1;
            }
        }

        let task = self.tasks.get_mut(&id).expect("checked above");
        if let (Some(winning_answer), Some(share), TaskKind::Consensus { assignees, .. }) =
            (&winning_answer, share, &mut task.kind)
        {
            for assignment in assignees.values_mut() {
                if assignment.answer.as_ref() == Some(winning_answer) {
                    assignment.share = Some(share);
                }
            }
        }
        if winning_answer.is_some() {
            task.status = TaskStatus::Verified;
        } else {
            task.status = TaskStatus::Closed;
            task.close_reason = Some(CloseReason::NoMajority);
        }
    }

    /// Records that `recipient`'s `amount`-sized share of a `Verified`
    /// or `Submitted` task's bounty was successfully paid out on-chain,
    /// crediting their reputation. Split from resolution so a transient
    /// payout failure never silently credits reputation for a payment
    /// that didn't actually happen -- the caller only calls this once
    /// the payment has been *observed on chain*
    /// (`PayoutOutcome::Confirmed`), which is a stronger claim than the
    /// "confirmed sent" this used to be called on and the whole point of
    /// `TaskStatus::Submitted` existing. Works for both task kinds: a `HashMatch` task has
    /// exactly one possible recipient (its claimant) and always
    /// completes the task in one call; a `Consensus` task may have
    /// several winners, and the task only reaches `Paid` once every
    /// winner named by `pending_payouts` has been recorded here. Returns
    /// whether the whole task is now fully paid.
    pub fn mark_recipient_paid(
        &mut self,
        id: Uuid,
        recipient: &PublicKey,
        amount: u64,
    ) -> Result<bool, BoardError> {
        let task = self.tasks.get_mut(&id).ok_or(BoardError::NotFound)?;
        // `Submitted` as well as `Verified`: a payout is confirmed from
        // `Submitted` now, and that is the ordinary path. `Verified`
        // stays accepted because a `Consensus` task with several winners
        // can still be `Verified` -- one leg unsent -- while an earlier
        // leg's transaction confirms.
        if !matches!(task.status, TaskStatus::Verified | TaskStatus::Submitted) {
            return Err(BoardError::NotVerified);
        }

        let now_fully_paid = match &mut task.kind {
            TaskKind::HashMatch { .. } => {
                if task.claimant.as_ref() != Some(recipient) {
                    return Err(BoardError::NotClaimant);
                }
                true
            }
            TaskKind::Consensus { assignees, .. } => {
                let assignment = assignees.get_mut(recipient).ok_or(BoardError::NotClaimant)?;
                if assignment.share.is_none() {
                    return Err(BoardError::NotClaimant);
                }
                if assignment.paid {
                    return Ok(false);
                }
                assignment.paid = true;
                // Every winner's `share` was fixed by `resolve_consensus`;
                // "fully paid" just means every one of them is now marked
                // paid too -- no need to recompute the majority again.
                assignees.values().all(|a| a.share.is_none() || a.paid)
            }
            // Bounty leg only -- same scope as pending_payouts()/
            // is_recipient_paid() above. `recipient` must be whichever
            // party pending_payouts() actually named (the claimant if
            // unchallenged or AssigneeWins, the challenger if
            // ChallengerWins), not just "the claimant" unconditionally.
            TaskKind::Disputable { dispute, .. } => {
                let expected_recipient = match dispute {
                    None => task.claimant.clone(),
                    Some(d) => match d.resolution {
                        Some(DisputeResolution::ChallengerWins) => Some(d.challenger.clone()),
                        Some(DisputeResolution::AssigneeWins) | None => task.claimant.clone(),
                    },
                };
                if expected_recipient.as_ref() != Some(recipient) {
                    return Err(BoardError::NotClaimant);
                }
                true
            }
        };

        if now_fully_paid {
            task.status = TaskStatus::Paid;
            // Stamped here rather than by the caller, for the same
            // reason `created_at` is stamped in `create_task`: this is
            // the one line in the codebase where a task becomes paid, so
            // a timestamp set anywhere else is a timestamp somebody can
            // forget to set. `get_or_insert` rather than assignment
            // because a `Consensus` task reaches this line once per
            // winner and only the last one flips `now_fully_paid` -- but
            // a retry that re-confirms an already-`Paid` task must not
            // move the instant it settled.
            task.settled_at.get_or_insert_with(Utc::now);
        }
        let rep = self.reputation.entry(recipient.clone()).or_default();
        rep.completed += 1;
        rep.total_earned += amount;
        Ok(now_fully_paid)
    }

    /// Records that `attempt`'s transaction has been handed to the node,
    /// and flips the task to `Submitted` once every payout it owes has
    /// one. Replaces any previous attempt for the same (task,
    /// recipient): a resubmission supersedes the attempt it replaces,
    /// and keeping the old one would leave the sweep resolving a
    /// transaction that has already been ruled out.
    ///
    /// The task only reaches `Submitted` when *nothing* is left
    /// unsubmitted, so a multi-winner `Consensus` task with one leg that
    /// failed to send stays `Verified` and the sweep keeps retrying that
    /// leg -- which is exactly the pre-existing behaviour for a
    /// partially-settled task, preserved.
    pub fn record_payout_attempt(&mut self, attempt: PayoutAttempt) {
        let task_id = attempt.task_id;
        self.payout_attempts.insert((task_id, attempt.recipient.clone()), attempt);
        if !self.unsubmitted_payouts(task_id).is_empty() {
            return;
        }
        if let Some(task) = self.tasks.get_mut(&task_id) {
            if task.status == TaskStatus::Verified {
                task.status = TaskStatus::Submitted;
            }
        }
    }

    /// Restores an attempt previously persisted by `HubStore`. Unlike
    /// `record_payout_attempt` this touches no task status: the task was
    /// restored from the same store with the status it already had, and
    /// recomputing it at boot could only disagree with what was written.
    pub fn restore_payout_attempt(&mut self, attempt: PayoutAttempt) {
        self.payout_attempts.insert((attempt.task_id, attempt.recipient.clone()), attempt);
    }

    pub fn payout_attempt(&self, task_id: Uuid, recipient: &PublicKey) -> Option<&PayoutAttempt> {
        self.payout_attempts.get(&(task_id, recipient.clone()))
    }

    /// Every payout the hub is currently waiting on, oldest first, so
    /// the sweep resolves the longest-outstanding ones before any it
    /// only just submitted. Cloned rather than borrowed because the
    /// caller has to release the board lock before talking to the node.
    pub fn outstanding_payout_attempts(&self) -> Vec<PayoutAttempt> {
        let mut attempts: Vec<PayoutAttempt> = self.payout_attempts.values().cloned().collect();
        attempts.sort_by_key(|a| a.submitted_at);
        attempts
    }

    /// Drops an attempt, because it resolved one way or the other.
    /// Returns what was there, if anything -- `None` when a concurrent
    /// resolution got to it first, which the caller must treat as "not
    /// mine to act on" rather than as an error.
    pub fn clear_payout_attempt(&mut self, task_id: Uuid, recipient: &PublicKey) -> Option<PayoutAttempt> {
        self.payout_attempts.remove(&(task_id, recipient.clone()))
    }

    /// The payouts `task_id` still owes that have no transaction in
    /// flight for them -- what the settlement path must actually send,
    /// as opposed to `Task::pending_payouts`, which still names a
    /// recipient whose transaction is already on the wire.
    ///
    /// This subtraction is the whole double-spend guard for the
    /// `Submitted` state. Without it the sweep would re-send every
    /// payout it is waiting on, once a minute, forever -- and the node
    /// strikes a peer for a duplicate transaction (plan §6.2).
    pub fn unsubmitted_payouts(&self, task_id: Uuid) -> Vec<(PublicKey, u64)> {
        let Some(task) = self.tasks.get(&task_id) else {
            return vec![];
        };
        task.pending_payouts()
            .into_iter()
            .filter(|(recipient, _)| !self.payout_attempts.contains_key(&(task_id, recipient.clone())))
            .collect()
    }

    /// Gives up on a task's payout after `MAX_PAYOUT_SUBMISSIONS`
    /// attempts were each proven never to have reached the chain, and
    /// drops whatever attempts it still holds for it.
    ///
    /// The escrow behind the task is deliberately left alone -- not
    /// refunded to the poster, who received the work, and not marked
    /// settled, which would say the worker was paid. It stays exactly
    /// where it is for an operator to resolve by hand; see
    /// `TaskStatus::PayoutFailed`.
    ///
    /// Returns every attempt it dropped, including a sibling leg's,
    /// because the caller has to delete each one from the store as well.
    /// Leaving one behind would be silently corrosive rather than
    /// merely untidy: it survives a restart, comes back on a board whose
    /// task is now terminal, and every sweep afterwards tries to resolve
    /// it and logs a failure it can never clear.
    pub fn mark_payout_failed(&mut self, task_id: Uuid) -> Result<Vec<PayoutAttempt>, BoardError> {
        let task = self.tasks.get_mut(&task_id).ok_or(BoardError::NotFound)?;
        if !matches!(task.status, TaskStatus::Verified | TaskStatus::Submitted) {
            return Err(BoardError::NotVerified);
        }
        task.status = TaskStatus::PayoutFailed;
        let dropped: Vec<PayoutAttempt> = self
            .payout_attempts
            .keys()
            .filter(|(id, _)| *id == task_id)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .filter_map(|key| self.payout_attempts.remove(&key))
            .collect();
        Ok(dropped)
    }

    /// Reopens any `Claimed` task whose deadline has passed, so an
    /// abandoned claim doesn't sit locked forever. Returns the ids that
    /// were reopened. Call periodically from a background sweep.
    pub fn expire_claims(&mut self, now: DateTime<Utc>) -> Vec<Uuid> {
        let mut reopened = Vec::new();
        for task in self.tasks.values_mut() {
            if task.status == TaskStatus::Claimed {
                if let Some(deadline) = task.claim_deadline {
                    if now > deadline {
                        task.status = TaskStatus::Open;
                        task.claimant = None;
                        task.claim_deadline = None;
                        reopened.push(task.id);
                    }
                }
            }
        }
        reopened
    }

    pub fn reputation(&self, pubkey: &PublicKey) -> Reputation {
        self.reputation.get(pubkey).cloned().unwrap_or_default()
    }

    pub fn leaderboard(&self, top_n: usize) -> Vec<(PublicKey, Reputation)> {
        let mut entries: Vec<_> = self
            .reputation
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        entries.sort_by(|a, b| b.1.total_earned.cmp(&a.1.total_earned));
        entries.truncate(top_n);
        entries
    }

    /// Whether `pubkey` is still eligible for a faucet grant. Read-only
    /// on purpose -- see `record_faucet_grant`.
    pub fn can_claim_faucet(&self, pubkey: &PublicKey) -> bool {
        !self.faucet_grants.contains_key(pubkey)
    }

    /// Atomically reserves the one grant `pubkey` is entitled to (fails if
    /// it's already been reserved or granted). Callers should reserve
    /// BEFORE attempting the on-chain payout -- that's what makes two
    /// concurrent claims from the same pubkey safe -- and call
    /// `revoke_faucet_grant` to release the reservation if the payout
    /// then fails, so a transient failure doesn't permanently lock the
    /// agent out of a grant it never actually received.
    pub fn record_faucet_grant(&mut self, pubkey: PublicKey, granted_at: i64) -> Result<(), BoardError> {
        if self.faucet_grants.insert(pubkey, granted_at).is_some() {
            return Err(BoardError::AlreadyClaimed);
        }
        Ok(())
    }

    /// Releases a faucet-grant reservation made by `record_faucet_grant`.
    /// Only call this when the payout that was supposed to follow the
    /// reservation actually failed.
    pub fn revoke_faucet_grant(&mut self, pubkey: &PublicKey) {
        self.faucet_grants.remove(pubkey);
    }

    pub fn all_tasks(&self) -> impl Iterator<Item = &Task> {
        self.tasks.values()
    }

    pub fn all_reputation(&self) -> impl Iterator<Item = (&PublicKey, &Reputation)> {
        self.reputation.iter()
    }

    pub fn all_faucet_grants(&self) -> impl Iterator<Item = &PublicKey> {
        self.faucet_grants.keys()
    }

    /// How many grants were made at or after `cutoff`.
    ///
    /// The global budget's numerator. Per-key uniqueness bounds what one
    /// identity can take and nothing bounds what a *population* can take,
    /// which is the whole difficulty of a fully-open faucet: keygen is
    /// free, so the only quantity anyone can actually promise an operator
    /// is a ceiling on the total.
    pub fn faucet_granted_since(&self, cutoff: i64) -> u64 {
        self.faucet_grants.values().filter(|at| **at >= cutoff).count() as u64
    }

    /// When each grant was made, epoch *seconds*, in no useful order.
    ///
    /// `faucet_granted_since` answers "how many since a cutoff", which
    /// is what the budget needs and is the wrong shape for a chart: a
    /// series wants every instant so it can bucket them. Returned as
    /// bare timestamps rather than as the map, because which key holds a
    /// grant is not a chart's business and handing out the pubkeys would
    /// make this an identity endpoint by accident.
    pub fn faucet_grant_times(&self) -> Vec<i64> {
        self.faucet_grants.values().copied().collect()
    }

    /// Grants made from `prefix` at or after `cutoff`.
    ///
    /// The per-network half of the faucet's controls. It makes casual
    /// abuse tedious rather than impossible -- an attacker with addresses
    /// in many networks walks straight past it -- and that is the
    /// division of labour: this raises the floor, and
    /// `faucet_granted_since` bounds the loss however many networks
    /// somebody assembles.
    pub fn faucet_granted_from_prefix_since(&self, prefix: &str, cutoff: i64) -> u64 {
        self.faucet_grant_prefixes
            .iter()
            .filter(|(_, from)| from.as_str() == prefix)
            .filter(|(pubkey, _)| self.faucet_grants.get(*pubkey).is_some_and(|at| *at >= cutoff))
            .count() as u64
    }

    /// Grants per network: `(in the window, all time)` keyed by prefix.
    ///
    /// The clustering view `admin` renders. Grouped here rather than in
    /// the handler because it needs both maps at once and the board is
    /// the only thing that holds them together.
    pub fn faucet_grants_by_prefix(&self, cutoff: i64) -> Vec<(String, (u64, u64))> {
        let mut rows: BTreeMap<String, (u64, u64)> = BTreeMap::new();
        for (pubkey, prefix) in &self.faucet_grant_prefixes {
            let entry = rows.entry(prefix.clone()).or_insert((0, 0));
            entry.1 += 1;
            if self.faucet_grants.get(pubkey).is_some_and(|at| *at >= cutoff) {
                entry.0 += 1;
            }
        }
        rows.into_iter().collect()
    }

    /// Restores the network a grant was claimed from. Separate from
    /// `restore_faucet_grant` because the two tables are read
    /// independently at boot and a grant may have no prefix recorded.
    pub fn restore_faucet_grant_prefix(&mut self, pubkey: PublicKey, prefix: String) {
        self.faucet_grant_prefixes.insert(pubkey, prefix);
    }

    pub fn record_faucet_grant_prefix(&mut self, pubkey: PublicKey, prefix: String) {
        self.faucet_grant_prefixes.insert(pubkey, prefix);
    }

    pub fn exchange_account(&self, pubkey: &PublicKey) -> ExchangeAccount {
        self.exchange_accounts.get(pubkey).cloned().unwrap_or_default()
    }

    /// Restores an exchange account previously persisted by `HubStore`.
    pub fn restore_exchange_account(&mut self, pubkey: PublicKey, account: ExchangeAccount) {
        self.exchange_accounts.insert(pubkey, account);
    }

    pub fn all_exchange_accounts(&self) -> impl Iterator<Item = (&PublicKey, &ExchangeAccount)> {
        self.exchange_accounts.iter()
    }

    /// Confirms a `FundExchangeAccount` deposit is now sufficiently
    /// funded (at least `handlers::MIN_EXCHANGE_DEPOSIT`, unlike a task
    /// escrow there is no fixed target) and credits the depositor's
    /// ledger balance with `observed_amount` net of `network_fee` -- the
    /// fee the eventual custody sweep will pay to move this deposit's
    /// UTXO, reserved up front so the credited balance is never more
    /// than what will actually be recoverable on-chain. Same rejection
    /// shape as `confirm_escrow`. Returns `(depositor, credited_amount)`.
    pub fn confirm_exchange_deposit(
        &mut self,
        id: Uuid,
        observed_amount: u64,
        network_fee: u64,
        now: DateTime<Utc>,
    ) -> Result<(PublicKey, u64), BoardError> {
        let deposit = self.pending_deposits.get(&id).ok_or(BoardError::EscrowNotFound)?;
        if !matches!(deposit.purpose, EscrowPurpose::FundExchangeAccount) {
            return Err(BoardError::WrongEscrowPurpose);
        }
        if deposit.status != EscrowStatus::Reserved {
            return Err(BoardError::EscrowNotReserved);
        }
        if now > deposit.expires_at {
            return Err(BoardError::EscrowExpired);
        }
        if observed_amount < deposit.required_amount {
            return Err(BoardError::EscrowUnderfunded {
                required: deposit.required_amount,
                have: observed_amount,
            });
        }
        let depositor = deposit.depositor.clone();
        let credited = observed_amount.saturating_sub(network_fee);

        self.pending_deposits
            .get_mut(&id)
            .expect("existence checked above")
            .status = EscrowStatus::Consumed;
        self.exchange_accounts.entry(depositor.clone()).or_default().base_balance += credited;
        Ok((depositor, credited))
    }

    /// Every `FundExchangeAccount` deposit that's been confirmed (ledger
    /// already credited) but not yet swept into the pooled custody
    /// address -- polled by the sweep loop, mirroring
    /// `overdue_reserved_escrows`'s role for expired reservations.
    /// Selecting only `Consumed` (never `Refunded`) deposits makes a
    /// retry naturally idempotent: once swept and marked `Refunded` by
    /// `handlers::sweep_exchange_deposit`, a deposit drops out of this
    /// list for good.
    pub fn unswept_exchange_deposits(&self) -> Vec<Uuid> {
        self.pending_deposits
            .values()
            .filter(|d| d.status == EscrowStatus::Consumed && matches!(d.purpose, EscrowPurpose::FundExchangeAccount))
            .map(|d| d.id)
            .collect()
    }

    /// Credits `recipient`'s tradeable compute balance -- the reward for
    /// a task payout tagged with the `"compute"` capability (see
    /// `handlers::settle_one_payout_inner`/`settle_escrow_funded_task`).
    /// Mirrors `credit_forfeited_bond` exactly: a plain balance credit
    /// with no reputation side effect, since reputation was already
    /// handled by the payout itself.
    pub fn credit_compute(&mut self, recipient: &PublicKey, amount: u64) {
        self.exchange_accounts.entry(recipient.clone()).or_default().compute_balance += amount;
    }

    /// Credits `recipient`'s base currency balance -- mirrors
    /// `credit_compute` exactly, for the base side. Kept a distinct,
    /// clearly named method rather than reused from
    /// `credit_back_withdrawal` even though mechanically identical,
    /// matching this module's convention of one method per semantic
    /// purpose. Used by `handlers::place_order` to route taker fee
    /// revenue to the exchange's fee sink -- `TaskBoard` itself has no
    /// concept of an operator or fee recipient, it just credits whatever
    /// pubkey it's given.
    pub fn credit_base(&mut self, recipient: &PublicKey, amount: u64) {
        self.exchange_accounts.entry(recipient.clone()).or_default().base_balance += amount;
    }

    /// Atomically checks and debits `owner`'s withdrawable balance in one
    /// step -- the single guard that stops two concurrent withdrawal
    /// requests from jointly overdrawing the same ledger balance
    /// (mirrors `record_faucet_grant`'s atomic-reserve shape). Callers
    /// must call `credit_back_withdrawal` if the payout that was
    /// supposed to follow this debit then fails.
    ///
    /// **Zero is refused here rather than left to the caller**, for the
    /// same reason `place_order` refuses a zero price or quantity twelve
    /// lines down: a debit of nothing passes the balance check trivially
    /// (`0 < 0` is false) and every step after it proceeds as if a real
    /// withdrawal were happening. `pay_from_custody` then built and
    /// submitted an actual transaction -- a custody output spent, a
    /// network fee paid to a miner, a zero-value output created, and the
    /// spent output's change invisible until the next block. Repeat it
    /// and custody bleeds a fee per call that no ledger balance accounts
    /// for, which walks the solvency pair apart and grinds
    /// `custody_ready_outputs` down until real withdrawals start failing
    /// for want of a spendable output.
    ///
    /// The floor above zero is the handler's (`MIN_EXCHANGE_WITHDRAWAL`),
    /// because "large enough to be worth its fee" is policy and can be
    /// tuned; "not nothing" is an invariant and belongs with the ledger.
    pub fn debit_for_withdrawal(&mut self, owner: &PublicKey, amount: u64) -> Result<(), BoardError> {
        if amount == 0 {
            return Err(BoardError::ZeroWithdrawal);
        }
        let account = self.exchange_accounts.entry(owner.clone()).or_default();
        let available = account.base_balance.saturating_sub(account.locked_base);
        if available < amount {
            return Err(BoardError::InsufficientBalance { available, required: amount });
        }
        account.base_balance -= amount;
        Ok(())
    }

    /// Reverses a `debit_for_withdrawal` whose payout failed.
    pub fn credit_back_withdrawal(&mut self, owner: &PublicKey, amount: u64) {
        self.exchange_accounts.entry(owner.clone()).or_default().base_balance += amount;
    }

    /// The best (price-time priority) resting order on the opposite side
    /// of `taker` that it's actually allowed to match against: open,
    /// crosses `taker`'s price, and not owned by `taker`'s own owner
    /// (mirrors the unconditional `PosterCannotClaimOwnTask` precedent --
    /// a self-tradeable book is a manipulable one, and `/exchange/trades`
    /// is meant to be a real pricing feed).
    fn best_resting_match(&self, taker: &Order) -> Option<Uuid> {
        let opposite = match taker.side {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        };
        let mut candidates: Vec<&Order> = self
            .orders
            .values()
            .filter(|o| {
                o.status == OrderStatus::Open
                    && o.side == opposite
                    && o.owner != taker.owner
                    && match taker.side {
                        Side::Buy => o.price <= taker.price,
                        Side::Sell => o.price >= taker.price,
                    }
            })
            .collect();
        match taker.side {
            // Best ask for a buyer: lowest price, then earliest.
            Side::Buy => candidates.sort_by(|a, b| a.price.cmp(&b.price).then(a.created_at.cmp(&b.created_at))),
            // Best bid for a seller: highest price, then earliest.
            Side::Sell => candidates.sort_by(|a, b| b.price.cmp(&a.price).then(a.created_at.cmp(&b.created_at))),
        }
        candidates.first().map(|o| o.id)
    }

    /// A bounded working copy of only the orders/accounts this placement can
    /// touch. The handler keeps the live write lock until the staged records
    /// commit, then publishes them. History is not cloned on every write.
    pub fn stage_order(&self, owner: &PublicKey, side: Side, price: u64, fee_sink: &PublicKey) -> Self {
        let mut staged = Self::new();
        staged.restore_exchange_account(owner.clone(), self.exchange_account(owner));
        staged.restore_exchange_account(fee_sink.clone(), self.exchange_account(fee_sink));
        for order in self.orders.values().filter(|o| o.status == OrderStatus::Open
            && o.owner != *owner && o.side != side
            && match side { Side::Buy => o.price <= price, Side::Sell => o.price >= price }) {
            staged.restore_exchange_account(order.owner.clone(), self.exchange_account(&order.owner));
            staged.restore_order(order.clone());
        }
        staged
    }

    /// Places a limit order, locking the full notional up front (a buy
    /// locks `price * quantity` base; a sell locks `quantity` compute --
    /// validated against overflow before anything is locked), then
    /// matches it immediately against any crossing resting orders in
    /// price-time priority, filling at each resting (maker) order's own
    /// price rather than this order's own limit price.
    ///
    /// The lock/settle reconciliation is the one genuinely tricky part:
    /// the buy side's lock was taken at *its own* price, which is not
    /// always the same number as the execution price, so releasing it
    /// naively either strands funds or silently overdrafts. Every fill
    /// releases the buy order's lock at the buy order's own price and
    /// debits the buyer's spendable balance at the (always equal or
    /// better) execution price -- when the buy side is the resting
    /// order these are identical, no slack; when the buy side is the
    /// taker, the difference becomes available balance again, i.e. a
    /// taker who crossed a better-priced resting order gets the
    /// improvement credited back rather than losing it. The sell side's
    /// lock is quantity-only, so it always releases 1:1 with fill
    /// quantity, no equivalent correction needed there.
    pub fn place_order(
        &mut self,
        owner: PublicKey,
        side: Side,
        price: u64,
        quantity: u64,
        now: DateTime<Utc>,
    ) -> Result<(Order, Vec<Trade>), BoardError> {
        if price == 0 || quantity == 0 {
            return Err(BoardError::InvalidOrder);
        }
        let notional = price.checked_mul(quantity).ok_or(BoardError::OrderNotionalOverflow)?;

        {
            let account = self.exchange_accounts.entry(owner.clone()).or_default();
            match side {
                Side::Buy => {
                    let available = account.base_balance.saturating_sub(account.locked_base);
                    if available < notional {
                        return Err(BoardError::InsufficientBalance { available, required: notional });
                    }
                    account.locked_base += notional;
                }
                Side::Sell => {
                    let available = account.compute_balance.saturating_sub(account.locked_compute);
                    if available < quantity {
                        return Err(BoardError::InsufficientBalance { available, required: quantity });
                    }
                    account.locked_compute += quantity;
                }
            }
        }

        let mut taker = Order {
            id: Uuid::new_v4(),
            owner: owner.clone(),
            side,
            price,
            quantity,
            filled: 0,
            status: OrderStatus::Open,
            created_at: now,
        };

        let mut trades = Vec::new();
        while taker.filled < taker.quantity {
            let Some(resting_id) = self.best_resting_match(&taker) else { break };
            let resting = self.orders.get(&resting_id).expect("just found by best_resting_match").clone();

            let fill_qty = (taker.quantity - taker.filled).min(resting.quantity - resting.filled);
            let execution_price = resting.price;

            {
                let r = self.orders.get_mut(&resting_id).expect("just found by best_resting_match");
                r.filled += fill_qty;
                if r.filled >= r.quantity {
                    r.status = OrderStatus::Filled;
                }
            }
            taker.filled += fill_qty;

            let (buy_owner, buy_price, sell_owner) = match taker.side {
                Side::Buy => (taker.owner.clone(), taker.price, resting.owner.clone()),
                Side::Sell => (resting.owner.clone(), resting.price, taker.owner.clone()),
            };

            // The taker pays TAKER_FEE_BPS on this fill, taken out of
            // whatever they're receiving (never as an extra charge
            // beyond what was already locked, which is what keeps this
            // provably free of underflow risk -- see this fn's own doc
            // comment). The maker side is always credited in full,
            // which is the "rebate" half of a maker/taker fee model.
            let notional = execution_price * fill_qty;
            let taker_fee = match taker.side {
                Side::Buy => fill_qty * TAKER_FEE_BPS / 10_000,
                Side::Sell => notional * TAKER_FEE_BPS / 10_000,
            };

            {
                let buyer_account = self.exchange_accounts.entry(buy_owner.clone()).or_default();
                buyer_account.locked_base -= buy_price * fill_qty;
                buyer_account.base_balance -= notional;
                let compute_credit = if taker.side == Side::Buy { fill_qty - taker_fee } else { fill_qty };
                buyer_account.compute_balance += compute_credit;
            }
            {
                let seller_account = self.exchange_accounts.entry(sell_owner.clone()).or_default();
                seller_account.locked_compute -= fill_qty;
                seller_account.compute_balance -= fill_qty;
                let base_credit = if taker.side == Side::Sell { notional - taker_fee } else { notional };
                seller_account.base_balance += base_credit;
            }

            let (buy_order_id, sell_order_id) = match taker.side {
                Side::Buy => (taker.id, resting.id),
                Side::Sell => (resting.id, taker.id),
            };
            trades.push(Trade {
                id: Uuid::new_v4(),
                buy_order_id,
                sell_order_id,
                buyer: buy_owner,
                seller: sell_owner,
                price: execution_price,
                taker_side: taker.side,
                taker_fee,
                quantity: fill_qty,
                executed_at: now,
            });
        }

        if taker.filled >= taker.quantity {
            taker.status = OrderStatus::Filled;
        }
        self.orders.insert(taker.id, taker.clone());
        for trade in &trades {
            self.trades.insert(trade.id, trade.clone());
        }
        Ok((taker, trades))
    }

    /// Cancels an open (or partially filled) order, releasing whatever
    /// remains of its locked balance back to the owner. Rejects a
    /// non-owner caller or an already-terminal (`Filled`/`Cancelled`)
    /// order.
    pub fn cancel_order(&mut self, order_id: Uuid, caller: &PublicKey) -> Result<Order, BoardError> {
        let order = self.orders.get(&order_id).ok_or(BoardError::OrderNotFound)?;
        if &order.owner != caller {
            return Err(BoardError::NotOrderOwner);
        }
        if order.status != OrderStatus::Open {
            return Err(BoardError::OrderNotOpen);
        }
        let remaining = order.quantity - order.filled;
        let (side, price, owner) = (order.side, order.price, order.owner.clone());

        let order = self.orders.get_mut(&order_id).expect("existence checked above");
        order.status = OrderStatus::Cancelled;
        let cancelled = order.clone();

        let account = self.exchange_accounts.entry(owner).or_default();
        match side {
            Side::Buy => account.locked_base -= price * remaining,
            Side::Sell => account.locked_compute -= remaining,
        }
        Ok(cancelled)
    }

    pub fn get_order(&self, id: Uuid) -> Option<&Order> {
        self.orders.get(&id)
    }

    /// Every open order, split by side and sorted best-first: bids by
    /// price descending then earliest first, asks by price ascending
    /// then earliest first -- the same ordering `best_resting_match`
    /// uses internally, exposed here for the order-book endpoint.
    pub fn order_book(&self) -> (Vec<&Order>, Vec<&Order>) {
        let mut bids: Vec<&Order> = self
            .orders
            .values()
            .filter(|o| o.status == OrderStatus::Open && o.side == Side::Buy)
            .collect();
        let mut asks: Vec<&Order> = self
            .orders
            .values()
            .filter(|o| o.status == OrderStatus::Open && o.side == Side::Sell)
            .collect();
        bids.sort_by(|a, b| b.price.cmp(&a.price).then(a.created_at.cmp(&b.created_at)));
        asks.sort_by(|a, b| a.price.cmp(&b.price).then(a.created_at.cmp(&b.created_at)));
        (bids, asks)
    }

    /// Restores an order previously persisted by `HubStore`.
    pub fn restore_order(&mut self, order: Order) {
        self.orders.insert(order.id, order);
    }

    pub fn all_orders(&self) -> impl Iterator<Item = &Order> {
        self.orders.values()
    }

    pub fn all_trades_newest_first(&self) -> Vec<&Trade> {
        let mut trades: Vec<&Trade> = self.trades.values().collect();
        trades.sort_by(|a, b| b.executed_at.cmp(&a.executed_at));
        trades
    }

    /// Restores a trade previously persisted by `HubStore`.
    pub fn restore_trade(&mut self, trade: Trade) {
        self.trades.insert(trade.id, trade);
    }

    pub fn all_trades(&self) -> impl Iterator<Item = &Trade> {
        self.trades.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use btclib::crypto::PrivateKey;

    fn pubkey() -> PublicKey {
        PrivateKey::new_key().public_key()
    }

    /// A `TransactionOutput` of `value` belonging to `owner`, with the
    /// fresh `unique_id` every real one carries -- so two calls with
    /// identical arguments hash differently, which is the property the
    /// whole resolution rule rests on.
    fn output(value: u64, owner: &PublicKey) -> btclib::types::TransactionOutput {
        btclib::types::TransactionOutput {
            value,
            unique_id: Uuid::new_v4(),
            pubkey: owner.clone(),
        }
    }

    /// An attempt that claims to have paid `paid_output` to its
    /// recipient by spending `spent` from `source`.
    fn attempt(
        recipient: &PublicKey,
        source: &PublicKey,
        paid_output: &btclib::types::TransactionOutput,
        spent: &[&btclib::types::TransactionOutput],
    ) -> PayoutAttempt {
        PayoutAttempt {
            task_id: Uuid::new_v4(),
            recipient: recipient.clone(),
            amount: paid_output.value,
            output_hash: paid_output.hash(),
            spent_inputs: spent.iter().map(|o| o.hash()).collect(),
            source: source.clone(),
            submitted_at: Utc::now(),
            submissions: 1,
        }
    }

    /// A throwaway escrow secret for tests that only need `reserve_escrow`
    /// to produce *some* address. Tests that care which key an address
    /// belongs to hold onto one secret and pass it to both `reserve_escrow`
    /// and `PendingDeposit::private_key` themselves.
    fn escrow_secret() -> EscrowSecret {
        EscrowSecret::generate()
    }

    /// A `TaskIntent` for tests that only need `EscrowPurpose` to hold
    /// something well-formed.
    fn any_intent(bounty: u64) -> TaskIntent {
        TaskIntent {
            description: "t".to_string(),
            bounty,
            expected_output_hash: Hash::hash_bytes(b"x"),
            min_reputation: 0,
            capabilities: BTreeSet::new(),
        }
    }

    #[test]
    fn a_reserved_escrow_carries_no_key_and_derives_the_address_it_handed_out() {
        let mut board = TaskBoard::new();
        let secret = EscrowSecret::generate();

        let deposit = board.reserve_escrow(
            &secret,
            pubkey(),
            100,
            EscrowPurpose::FundHashMatchTask(any_intent(100)),
            Utc::now() + chrono::Duration::minutes(30),
        );

        assert!(deposit.deposit_private_key.is_none());
        assert_eq!(
            deposit.private_key(&secret).public_key(),
            deposit.deposit_pubkey,
            "the derived key must control the address the depositor is told to pay"
        );
    }

    /// The compatibility guarantee that lets this change ship against a
    /// store that already has deposits in flight. Older builds serialized
    /// `deposit_private_key` as a bare `PrivateKey`; this build reads the
    /// same bytes into an `Option` and must see `Some`, then keep using
    /// that stored key rather than deriving a different address for money
    /// somebody was already told to send.
    #[test]
    fn reads_back_a_deposit_written_before_keys_were_derived() {
        #[derive(Serialize)]
        struct LegacyPendingDeposit {
            id: Uuid,
            depositor: PublicKey,
            deposit_pubkey: PublicKey,
            deposit_private_key: PrivateKey,
            required_amount: u64,
            purpose: EscrowPurpose,
            status: EscrowStatus,
            created_at: DateTime<Utc>,
            expires_at: DateTime<Utc>,
        }

        let legacy_key = PrivateKey::new_key();
        let legacy_pubkey = legacy_key.public_key();
        let legacy = LegacyPendingDeposit {
            id: Uuid::new_v4(),
            depositor: pubkey(),
            deposit_pubkey: legacy_pubkey.clone(),
            deposit_private_key: legacy_key,
            required_amount: 500,
            purpose: EscrowPurpose::FundHashMatchTask(any_intent(500)),
            status: EscrowStatus::Reserved,
            created_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(30),
        };

        let mut bytes = Vec::new();
        ciborium::into_writer(&legacy, &mut bytes).unwrap();
        let restored: PendingDeposit = ciborium::from_reader(bytes.as_slice()).unwrap();

        assert!(
            restored.deposit_private_key.is_some(),
            "an old deposit's stored key must survive the field becoming optional"
        );
        // And it must be *that* key that gets used, not a freshly derived
        // one -- deriving here would point settlement at an address the
        // depositor never funded.
        let unrelated_secret = EscrowSecret::generate();
        assert_eq!(
            restored.private_key(&unrelated_secret).public_key(),
            legacy_pubkey,
            "a legacy deposit must keep settling against its own stored key"
        );
    }

    #[test]
    fn full_task_lifecycle_pays_out_and_updates_reputation() {
        let mut board = TaskBoard::new();
        let poster = pubkey();
        let worker = pubkey();
        let expected = Hash::hash_bytes(b"the correct answer");

        let task = board.create_task(poster, "add 2+2".to_string(), 100, expected);
        assert_eq!(board.list_open_tasks().len(), 1);
        assert_eq!(board.allocated_bounty(), 100);

        board
            .claim_task(task.id, worker.clone(), Utc::now() + chrono::Duration::minutes(10))
            .unwrap();
        assert!(board.list_open_tasks().is_empty());

        // wrong answer: reopens, dings reputation, does NOT pay
        let wrong = Hash::hash_bytes(b"a wrong answer");
        assert!(!board.submit(task.id, worker.clone(), wrong).unwrap());
        assert_eq!(board.get_task(task.id).unwrap().status, TaskStatus::Open);
        assert_eq!(board.reputation(&worker).failed, 1);

        // claim again and submit correctly
        board
            .claim_task(task.id, worker.clone(), Utc::now() + chrono::Duration::minutes(10))
            .unwrap();
        assert!(board.submit(task.id, worker.clone(), expected).unwrap());
        assert_eq!(board.get_task(task.id).unwrap().status, TaskStatus::Verified);

        // not paid/credited until mark_recipient_paid is called
        assert_eq!(board.reputation(&worker).completed, 0);
        assert!(board.mark_recipient_paid(task.id, &worker, 100).unwrap());
        assert_eq!(board.get_task(task.id).unwrap().status, TaskStatus::Paid);
        assert_eq!(board.reputation(&worker).completed, 1);
        assert_eq!(board.reputation(&worker).total_earned, 100);
        assert_eq!(board.allocated_bounty(), 0);
    }

    #[test]
    fn only_the_claimant_can_submit() {
        let mut board = TaskBoard::new();
        let expected = Hash::hash_bytes(b"answer");
        let task = board.create_task(pubkey(), "task".to_string(), 10, expected);
        let claimant = pubkey();
        let impostor = pubkey();
        board
            .claim_task(task.id, claimant, Utc::now() + chrono::Duration::minutes(5))
            .unwrap();

        assert!(matches!(
            board.submit(task.id, impostor, expected),
            Err(BoardError::NotClaimant)
        ));
    }

    #[test]
    fn cannot_claim_an_already_claimed_task() {
        let mut board = TaskBoard::new();
        let task = board.create_task(pubkey(), "task".to_string(), 10, Hash::hash_bytes(b"x"));
        let deadline = Utc::now() + chrono::Duration::minutes(5);
        board.claim_task(task.id, pubkey(), deadline).unwrap();

        assert!(matches!(
            board.claim_task(task.id, pubkey(), deadline),
            Err(BoardError::NotOpen)
        ));
    }

    #[test]
    fn abandoned_claims_expire_back_to_open() {
        let mut board = TaskBoard::new();
        let task = board.create_task(pubkey(), "task".to_string(), 10, Hash::hash_bytes(b"x"));
        let now = Utc::now();
        board.claim_task(task.id, pubkey(), now + chrono::Duration::seconds(1)).unwrap();

        // not expired yet
        assert!(board.expire_claims(now).is_empty());

        // now it is
        let later = now + chrono::Duration::seconds(2);
        let reopened = board.expire_claims(later);
        assert_eq!(reopened, vec![task.id]);
        assert_eq!(board.get_task(task.id).unwrap().status, TaskStatus::Open);
    }

    #[test]
    fn faucet_grants_are_one_per_pubkey() {
        let mut board = TaskBoard::new();
        let agent = pubkey();
        assert!(board.can_claim_faucet(&agent));
        board.record_faucet_grant(agent.clone(), Utc::now().timestamp()).unwrap();
        assert!(!board.can_claim_faucet(&agent));
        assert!(matches!(
            board.record_faucet_grant(agent, Utc::now().timestamp()),
            Err(BoardError::AlreadyClaimed)
        ));
    }

    #[test]
    fn verified_unpaid_tasks_lists_only_verified_tasks() {
        let mut board = TaskBoard::new();
        let expected = Hash::hash_bytes(b"x");
        let worker = pubkey();

        // Open: not listed
        board.create_task(pubkey(), "open".to_string(), 10, expected);

        // Verified: listed
        let verified_task = board.create_task(pubkey(), "verified".to_string(), 10, expected);
        board
            .claim_task(verified_task.id, worker.clone(), Utc::now() + chrono::Duration::minutes(5))
            .unwrap();
        board.submit(verified_task.id, worker.clone(), expected).unwrap();

        // Paid: no longer listed
        let paid_task = board.create_task(pubkey(), "paid".to_string(), 10, expected);
        board
            .claim_task(paid_task.id, worker.clone(), Utc::now() + chrono::Duration::minutes(5))
            .unwrap();
        board.submit(paid_task.id, worker.clone(), expected).unwrap();
        board.mark_recipient_paid(paid_task.id, &worker, 10).unwrap();

        let unpaid = board.verified_unpaid_tasks();
        assert_eq!(unpaid.len(), 1);
        assert_eq!(unpaid[0].id, verified_task.id);
    }

    #[test]
    fn leaderboard_sorts_by_total_earned_descending() {
        let mut board = TaskBoard::new();
        let low = pubkey();
        let high = pubkey();

        for (agent, bounty) in [(&low, 10u64), (&high, 500u64)] {
            let expected = Hash::hash_bytes(b"x");
            let task = board.create_task(pubkey(), "t".to_string(), bounty, expected);
            board
                .claim_task(task.id, agent.clone(), Utc::now() + chrono::Duration::minutes(5))
                .unwrap();
            board.submit(task.id, agent.clone(), expected).unwrap();
            board.mark_recipient_paid(task.id, agent, bounty).unwrap();
        }

        let board_order = board.leaderboard(10);
        assert_eq!(board_order[0].0, high);
        assert_eq!(board_order[1].0, low);
    }

    /// `deadline` is the desired *submission* deadline, converted to a
    /// `submission_window_minutes` internally (callers still pass a
    /// `DateTime` the way every existing test already computes one, e.g.
    /// `Utc::now() + Duration::minutes(30)` -- only this helper needed to
    /// know about the window-vs-deadline distinction). Every assignee
    /// joins synchronously right here, well within any reasonable join
    /// window, so the join deadline itself is just a generous, fixed hour
    /// out and not parameterized (tests that specifically care about the
    /// join window construct their own task directly instead of using
    /// this helper).
    fn create_and_fill_consensus_task(
        board: &mut TaskBoard,
        num_assignees: u32,
        deadline: DateTime<Utc>,
    ) -> (Uuid, Vec<PublicKey>) {
        let join_deadline = Utc::now() + chrono::Duration::hours(1);
        let submission_window_minutes = (deadline - Utc::now()).num_minutes().max(1);
        let task = board.create_consensus_task(
            pubkey(),
            "open-ended".to_string(),
            900,
            num_assignees,
            join_deadline,
            submission_window_minutes,
        );
        let assignees: Vec<PublicKey> = (0..num_assignees).map(|_| pubkey()).collect();
        for agent in &assignees {
            board.join_consensus_task(task.id, agent.clone()).unwrap();
        }
        (task.id, assignees)
    }

    #[test]
    fn consensus_task_resolves_and_pays_the_majority_once_everyone_submits() {
        let mut board = TaskBoard::new();
        let deadline = Utc::now() + chrono::Duration::minutes(30);
        let (task_id, assignees) = create_and_fill_consensus_task(&mut board, 3, deadline);
        assert_eq!(board.get_task(task_id).unwrap().status, TaskStatus::Claimed);

        // two agree, one doesn't
        assert!(!board.submit_consensus_answer(task_id, assignees[0].clone(), "42".to_string()).unwrap());
        assert!(!board.submit_consensus_answer(task_id, assignees[1].clone(), "42".to_string()).unwrap());
        assert!(board.submit_consensus_answer(task_id, assignees[2].clone(), "wrong".to_string()).unwrap());

        let task = board.get_task(task_id).unwrap();
        assert_eq!(task.status, TaskStatus::Verified);

        let payouts = task.pending_payouts();
        assert_eq!(payouts.len(), 2, "the two agreeing assignees each owe a share");
        for (pk, amount) in &payouts {
            assert!(assignees[0..2].contains(pk));
            assert_eq!(*amount, 900 / 2);
        }

        // the loser was dinged immediately at resolution, without waiting on payout
        assert_eq!(board.reputation(&assignees[2]).failed, 1);
        assert_eq!(board.reputation(&assignees[0]).completed, 0, "not credited until actually paid");
    }

    #[test]
    fn consensus_task_closes_with_no_payout_on_a_tie() {
        let mut board = TaskBoard::new();
        let deadline = Utc::now() + chrono::Duration::minutes(30);
        let (task_id, assignees) = create_and_fill_consensus_task(&mut board, 2, deadline);

        board.submit_consensus_answer(task_id, assignees[0].clone(), "a".to_string()).unwrap();
        board.submit_consensus_answer(task_id, assignees[1].clone(), "b".to_string()).unwrap();

        let task = board.get_task(task_id).unwrap();
        assert_eq!(task.status, TaskStatus::Closed);
        assert!(task.pending_payouts().is_empty());
        assert_eq!(board.reputation(&assignees[0]).failed, 0, "an honest tie dings no one");
        assert_eq!(board.reputation(&assignees[1]).failed, 0);
        assert_eq!(board.allocated_bounty(), 0, "a closed task's bounty is no longer allocated");
    }

    #[test]
    fn join_consensus_task_closes_to_new_joiners_once_full() {
        let mut board = TaskBoard::new();
        let deadline = Utc::now() + chrono::Duration::minutes(30);
        let (task_id, _assignees) = create_and_fill_consensus_task(&mut board, 2, deadline);

        assert!(matches!(
            board.join_consensus_task(task_id, pubkey()),
            Err(BoardError::NotOpen)
        ));
    }

    #[test]
    fn cannot_join_a_consensus_task_twice() {
        let mut board = TaskBoard::new();
        let deadline = Utc::now() + chrono::Duration::minutes(30);
        let task = board.create_consensus_task(pubkey(), "t".to_string(), 10, 3, deadline, 30);
        let agent = pubkey();
        board.join_consensus_task(task.id, agent.clone()).unwrap();

        assert!(matches!(
            board.join_consensus_task(task.id, agent),
            Err(BoardError::AlreadyJoined)
        ));
    }

    #[test]
    fn resolve_expired_consensus_tasks_treats_no_shows_as_disagreeing() {
        let mut board = TaskBoard::new();
        let now = Utc::now();
        let deadline = now + chrono::Duration::minutes(5);
        let (task_id, assignees) = create_and_fill_consensus_task(&mut board, 3, deadline);

        // only two of three ever submit, and they agree
        board.submit_consensus_answer(task_id, assignees[0].clone(), "42".to_string()).unwrap();
        board.submit_consensus_answer(task_id, assignees[1].clone(), "42".to_string()).unwrap();

        // not expired yet -- still waiting on assignees[2]
        assert!(board.resolve_expired_consensus_tasks(now).is_empty());
        assert_eq!(board.get_task(task_id).unwrap().status, TaskStatus::Claimed);

        // now it is
        let resolved = board.resolve_expired_consensus_tasks(deadline + chrono::Duration::seconds(1));
        assert_eq!(resolved, vec![task_id]);
        assert_eq!(board.get_task(task_id).unwrap().status, TaskStatus::Verified);
        assert_eq!(board.reputation(&assignees[2]).failed, 1, "no-show counts as disagreeing");
        assert_eq!(board.get_task(task_id).unwrap().pending_payouts().len(), 2);
    }

    #[test]
    fn mark_recipient_paid_completes_a_consensus_task_only_once_every_winner_is_paid() {
        let mut board = TaskBoard::new();
        let deadline = Utc::now() + chrono::Duration::minutes(30);
        let (task_id, assignees) = create_and_fill_consensus_task(&mut board, 2, deadline);
        board.submit_consensus_answer(task_id, assignees[0].clone(), "42".to_string()).unwrap();
        board.submit_consensus_answer(task_id, assignees[1].clone(), "42".to_string()).unwrap();
        assert_eq!(board.get_task(task_id).unwrap().status, TaskStatus::Verified);

        let fully_paid = board.mark_recipient_paid(task_id, &assignees[0], 450).unwrap();
        assert!(!fully_paid, "one of two winners paid -- task not fully settled yet");
        assert_eq!(board.get_task(task_id).unwrap().status, TaskStatus::Verified);
        assert_eq!(board.get_task(task_id).unwrap().pending_payouts().len(), 1);

        let fully_paid = board.mark_recipient_paid(task_id, &assignees[1], 450).unwrap();
        assert!(fully_paid, "both winners paid -- task fully settled");
        assert_eq!(board.get_task(task_id).unwrap().status, TaskStatus::Paid);
        assert_eq!(board.reputation(&assignees[0]).total_earned, 450);
        assert_eq!(board.reputation(&assignees[1]).total_earned, 450);
    }

    #[test]
    fn hash_match_operations_reject_a_consensus_task_and_vice_versa() {
        let mut board = TaskBoard::new();
        let deadline = Utc::now() + chrono::Duration::minutes(30);
        let consensus_task = board.create_consensus_task(pubkey(), "c".to_string(), 10, 2, deadline, 30);
        let hash_task = board.create_task(pubkey(), "h".to_string(), 10, Hash::hash_bytes(b"x"));

        assert!(matches!(
            board.claim_task(consensus_task.id, pubkey(), deadline),
            Err(BoardError::WrongTaskKind)
        ));
        assert!(matches!(
            board.join_consensus_task(hash_task.id, pubkey()),
            Err(BoardError::WrongTaskKind)
        ));

        board.claim_task(hash_task.id, pubkey(), deadline).unwrap();
        assert!(matches!(
            board.submit_consensus_answer(hash_task.id, pubkey(), "x".to_string()),
            Err(BoardError::WrongTaskKind)
        ));

        board.join_consensus_task(consensus_task.id, pubkey()).unwrap();
        board.join_consensus_task(consensus_task.id, pubkey()).unwrap();
        assert!(matches!(
            board.submit(consensus_task.id, pubkey(), Hash::hash_bytes(b"x")),
            Err(BoardError::WrongTaskKind)
        ));
    }

    #[test]
    fn cannot_submit_a_consensus_answer_twice() {
        let mut board = TaskBoard::new();
        let deadline = Utc::now() + chrono::Duration::minutes(30);
        let (task_id, assignees) = create_and_fill_consensus_task(&mut board, 2, deadline);
        board.submit_consensus_answer(task_id, assignees[0].clone(), "42".to_string()).unwrap();

        assert!(matches!(
            board.submit_consensus_answer(task_id, assignees[0].clone(), "43".to_string()),
            Err(BoardError::AlreadySubmitted)
        ));
    }

    #[test]
    fn claim_task_enforces_min_reputation() {
        let mut board = TaskBoard::new();
        let task = board.create_task(pubkey(), "gated".to_string(), 10, Hash::hash_bytes(b"x"));
        board.set_min_reputation(task.id, 5).unwrap();
        let novice = pubkey();
        let deadline = Utc::now() + chrono::Duration::minutes(5);

        assert!(matches!(
            board.claim_task(task.id, novice.clone(), deadline),
            Err(BoardError::InsufficientReputation { required: 5, have: 0 })
        ));
        assert_eq!(board.get_task(task.id).unwrap().status, TaskStatus::Open, "a rejected claim must not consume the task");

        let veteran = pubkey();
        board.restore_reputation(veteran.clone(), Reputation { completed: 5, failed: 0, total_earned: 0 });
        assert!(board.claim_task(task.id, veteran, deadline).is_ok());
    }

    #[test]
    fn join_consensus_task_enforces_min_reputation() {
        let mut board = TaskBoard::new();
        let deadline = Utc::now() + chrono::Duration::minutes(30);
        let task = board.create_consensus_task(pubkey(), "gated".to_string(), 10, 2, deadline, 30);
        board.set_min_reputation(task.id, 3).unwrap();

        let novice = pubkey();
        assert!(matches!(
            board.join_consensus_task(task.id, novice),
            Err(BoardError::InsufficientReputation { required: 3, have: 0 })
        ));

        let veteran = pubkey();
        board.restore_reputation(veteran.clone(), Reputation { completed: 3, failed: 0, total_earned: 0 });
        assert!(board.join_consensus_task(task.id, veteran).is_ok());
    }

    #[test]
    fn set_min_reputation_on_a_missing_task_fails() {
        let mut board = TaskBoard::new();
        assert!(matches!(
            board.set_min_reputation(Uuid::new_v4(), 5),
            Err(BoardError::NotFound)
        ));
    }

    #[test]
    fn consensus_share_stays_fixed_across_a_partial_payout_retry() {
        // Regression test: `pending_payouts` used to divide the bounty by
        // the count of still-*unpaid* winners, so paying one of several
        // winners inflated everyone else's share on the next call. The
        // share must instead be fixed once, at resolution, and never
        // recomputed from a shrinking pool.
        let mut board = TaskBoard::new();
        let deadline = Utc::now() + chrono::Duration::minutes(30);
        let (task_id, assignees) = create_and_fill_consensus_task(&mut board, 3, deadline);
        for agent in &assignees {
            board.submit_consensus_answer(task_id, agent.clone(), "42".to_string()).unwrap();
        }

        let payouts = board.get_task(task_id).unwrap().pending_payouts();
        assert_eq!(payouts.len(), 3);
        for (_, amount) in &payouts {
            assert_eq!(*amount, 300, "900 bounty split 3 ways");
        }

        let fully_paid = board.mark_recipient_paid(task_id, &assignees[0], 300).unwrap();
        assert!(!fully_paid);

        let remaining = board.get_task(task_id).unwrap().pending_payouts();
        assert_eq!(remaining.len(), 2);
        for (_, amount) in &remaining {
            assert_eq!(
                *amount, 300,
                "share must stay fixed at 300 -- must NOT inflate to 900/2=450 just because one winner is already paid"
            );
        }
    }

    #[test]
    fn is_recipient_paid_reflects_live_state_for_both_task_kinds() {
        let mut board = TaskBoard::new();
        let deadline = Utc::now() + chrono::Duration::minutes(30);

        // HashMatch
        let hash_task = board.create_task(pubkey(), "h".to_string(), 100, Hash::hash_bytes(b"x"));
        let worker = pubkey();
        board.claim_task(hash_task.id, worker.clone(), deadline).unwrap();
        board.submit(hash_task.id, worker.clone(), Hash::hash_bytes(b"x")).unwrap();
        assert!(!board.get_task(hash_task.id).unwrap().is_recipient_paid(&worker));
        board.mark_recipient_paid(hash_task.id, &worker, 100).unwrap();
        assert!(board.get_task(hash_task.id).unwrap().is_recipient_paid(&worker));

        // Consensus
        let (task_id, assignees) = create_and_fill_consensus_task(&mut board, 2, deadline);
        for agent in &assignees {
            board.submit_consensus_answer(task_id, agent.clone(), "42".to_string()).unwrap();
        }
        assert!(!board.get_task(task_id).unwrap().is_recipient_paid(&assignees[0]));
        board.mark_recipient_paid(task_id, &assignees[0], 450).unwrap();
        assert!(board.get_task(task_id).unwrap().is_recipient_paid(&assignees[0]));
        assert!(!board.get_task(task_id).unwrap().is_recipient_paid(&assignees[1]), "the other winner is still unpaid");
    }

    #[test]
    fn consensus_assignees_lists_everyone_for_consensus_and_nobody_for_hash_match() {
        let mut board = TaskBoard::new();
        let deadline = Utc::now() + chrono::Duration::minutes(30);
        let hash_task = board.create_task(pubkey(), "h".to_string(), 10, Hash::hash_bytes(b"x"));
        assert!(board.get_task(hash_task.id).unwrap().consensus_assignees().is_empty());

        let (task_id, assignees) = create_and_fill_consensus_task(&mut board, 3, deadline);
        let mut listed = board.get_task(task_id).unwrap().consensus_assignees();
        listed.sort();
        let mut expected = assignees.clone();
        expected.sort();
        assert_eq!(listed, expected);
    }

    #[test]
    fn cancel_understaffed_consensus_tasks_closes_a_task_that_never_filled_up() {
        let mut board = TaskBoard::new();
        let now = Utc::now();
        let join_deadline = now + chrono::Duration::minutes(5);
        let task = board.create_consensus_task(
            pubkey(),
            "needs 3, only 1 shows up".to_string(),
            900,
            3,
            join_deadline,
            60,
        );
        board.join_consensus_task(task.id, pubkey()).unwrap();
        assert_eq!(board.get_task(task.id).unwrap().status, TaskStatus::Open);
        assert_eq!(board.allocated_bounty(), 900);

        // not expired yet
        assert!(board.cancel_understaffed_consensus_tasks(now).is_empty());

        // now it is
        let cancelled = board.cancel_understaffed_consensus_tasks(join_deadline + chrono::Duration::seconds(1));
        assert_eq!(cancelled, vec![task.id]);
        assert_eq!(board.get_task(task.id).unwrap().status, TaskStatus::Closed);
        assert_eq!(board.allocated_bounty(), 0, "a cancelled task's bounty is freed back up");
    }

    #[test]
    fn cancel_understaffed_consensus_tasks_leaves_a_fully_joined_task_alone() {
        let mut board = TaskBoard::new();
        let deadline = Utc::now() + chrono::Duration::minutes(5);
        let (task_id, _assignees) = create_and_fill_consensus_task(&mut board, 2, deadline);

        // even well past what would have been an unmet join deadline, a
        // task that already filled up (and is now Claimed) must never be
        // touched by the understaffed-task sweep
        let cancelled = board.cancel_understaffed_consensus_tasks(Utc::now() + chrono::Duration::hours(2));
        assert!(cancelled.is_empty());
        assert_eq!(board.get_task(task_id).unwrap().status, TaskStatus::Claimed);
    }

    #[test]
    fn join_consensus_task_rejects_a_join_past_its_deadline() {
        let mut board = TaskBoard::new();
        let now = Utc::now();
        let join_deadline = now - chrono::Duration::seconds(1);
        let task = board.create_consensus_task(
            pubkey(),
            "already expired".to_string(),
            10,
            2,
            join_deadline,
            60,
        );

        assert!(matches!(
            board.join_consensus_task(task.id, pubkey()),
            Err(BoardError::JoinWindowExpired)
        ));
    }

    #[test]
    fn submit_consensus_answer_rejects_a_submission_past_its_deadline() {
        // Mirrors join_consensus_task_rejects_a_join_past_its_deadline for
        // the twin defensive check in submit_consensus_answer. A negative
        // submission_window_minutes forces submission_deadline into the
        // past the instant the task fills, the same trick
        // create_and_fill_consensus_task's own doc comment describes
        // (used directly here, not through that helper, since it clamps
        // the window to a minimum of 1 minute).
        let mut board = TaskBoard::new();
        let task = board.create_consensus_task(
            pubkey(),
            "already expired by the time it fills".to_string(),
            10,
            2,
            Utc::now() + chrono::Duration::hours(1),
            -1,
        );
        let a = pubkey();
        let b = pubkey();
        board.join_consensus_task(task.id, a.clone()).unwrap();
        board.join_consensus_task(task.id, b).unwrap();
        assert_eq!(board.get_task(task.id).unwrap().status, TaskStatus::Claimed);

        assert!(matches!(
            board.submit_consensus_answer(task.id, a, "42".to_string()),
            Err(BoardError::SubmissionWindowExpired)
        ));
    }

    #[test]
    fn cancel_task_closes_an_open_task_and_frees_its_escrow() {
        let mut board = TaskBoard::new();
        let task = board.create_task(pubkey(), "oops, typo".to_string(), 100, Hash::hash_bytes(b"x"));
        assert_eq!(board.allocated_bounty(), 100);

        board.cancel_task(task.id).unwrap();

        let task = board.get_task(task.id).unwrap();
        assert_eq!(task.status, TaskStatus::Closed);
        assert_eq!(task.close_reason, Some(CloseReason::CancelledByOperator));
        assert_eq!(board.allocated_bounty(), 0);
    }

    #[test]
    fn cancel_task_closes_a_claimed_task_without_dinging_the_claimant() {
        let mut board = TaskBoard::new();
        let task = board.create_task(pubkey(), "abandoned".to_string(), 100, Hash::hash_bytes(b"x"));
        let claimant = pubkey();
        board
            .claim_task(task.id, claimant.clone(), Utc::now() + chrono::Duration::minutes(30))
            .unwrap();

        board.cancel_task(task.id).unwrap();

        assert_eq!(board.get_task(task.id).unwrap().status, TaskStatus::Closed);
        assert_eq!(board.reputation(&claimant).failed, 0, "an operator cancellation isn't the claimant's fault");
    }

    #[test]
    fn cancel_task_works_on_a_claimed_consensus_task_too() {
        let mut board = TaskBoard::new();
        let deadline = Utc::now() + chrono::Duration::minutes(30);
        let (task_id, assignees) = create_and_fill_consensus_task(&mut board, 2, deadline);
        assert_eq!(board.get_task(task_id).unwrap().status, TaskStatus::Claimed);
        assert_eq!(board.allocated_bounty(), 900);

        board.cancel_task(task_id).unwrap();

        let task = board.get_task(task_id).unwrap();
        assert_eq!(task.status, TaskStatus::Closed);
        assert_eq!(task.close_reason, Some(CloseReason::CancelledByOperator));
        assert_eq!(board.allocated_bounty(), 0);
        for assignee in &assignees {
            assert_eq!(board.reputation(assignee).failed, 0);
        }
    }

    #[test]
    fn cancel_task_rejects_an_already_terminal_task() {
        let mut board = TaskBoard::new();
        let expected = Hash::hash_bytes(b"x");
        let task = board.create_task(pubkey(), "t".to_string(), 10, expected);
        let worker = pubkey();
        board
            .claim_task(task.id, worker.clone(), Utc::now() + chrono::Duration::minutes(5))
            .unwrap();
        board.submit(task.id, worker.clone(), expected).unwrap();
        board.mark_recipient_paid(task.id, &worker, 10).unwrap();
        assert_eq!(board.get_task(task.id).unwrap().status, TaskStatus::Paid);

        assert!(matches!(
            board.cancel_task(task.id),
            Err(BoardError::AlreadyTerminal)
        ));
    }

    #[test]
    fn cancel_task_on_a_missing_task_fails() {
        let mut board = TaskBoard::new();
        assert!(matches!(
            board.cancel_task(Uuid::new_v4()),
            Err(BoardError::NotFound)
        ));
    }

    #[test]
    fn reserve_escrow_creates_a_reserved_deposit_with_a_fresh_address() {
        let mut board = TaskBoard::new();
        let depositor = pubkey();
        let intent = TaskIntent {
            description: "t".to_string(),
            bounty: 100,
            expected_output_hash: Hash::hash_bytes(b"x"),
            min_reputation: 0,
            capabilities: BTreeSet::new(),
        };
        let deposit = board.reserve_escrow(&escrow_secret(),
            depositor.clone(),
            100,
            EscrowPurpose::FundHashMatchTask(intent),
            Utc::now() + chrono::Duration::minutes(30),
        );

        assert_eq!(deposit.status, EscrowStatus::Reserved);
        assert_eq!(deposit.depositor, depositor);
        assert_ne!(deposit.deposit_pubkey, depositor, "must be a fresh address, not the depositor's own");
        assert!(board.get_pending_deposit(deposit.id).is_some());
    }

    #[test]
    fn confirm_escrow_creates_a_hash_match_task_once_sufficiently_funded() {
        let mut board = TaskBoard::new();
        let depositor = pubkey();
        let expected = Hash::hash_bytes(b"answer");
        let intent = TaskIntent {
            description: "escrowed".to_string(),
            bounty: 500,
            expected_output_hash: expected,
            min_reputation: 0,
            capabilities: BTreeSet::new(),
        };
        let deposit = board.reserve_escrow(&escrow_secret(),
            depositor.clone(),
            500,
            EscrowPurpose::FundHashMatchTask(intent),
            Utc::now() + chrono::Duration::minutes(30),
        );

        let EscrowConfirmation::TaskCreated(task) =
            board.confirm_escrow(deposit.id, 500, Utc::now()).unwrap();

        assert_eq!(task.poster, depositor, "the depositor, not the operator, is the poster");
        assert_eq!(task.escrow_id, Some(deposit.id));
        assert_eq!(task.bounty, 500);
        assert_eq!(board.get_task(task.id).unwrap().id, task.id, "must actually be a real, retrievable task");
        assert_eq!(
            board.get_pending_deposit(deposit.id).unwrap().status,
            EscrowStatus::Consumed
        );
    }

    #[test]
    fn confirm_escrow_creates_a_consensus_task_once_sufficiently_funded() {
        let mut board = TaskBoard::new();
        let depositor = pubkey();
        let intent = ConsensusTaskIntent {
            description: "open-ended, escrowed".to_string(),
            bounty: 900,
            num_assignees: 3,
            join_window_minutes: 60,
            submission_window_minutes: 30,
            min_reputation: 0,
            capabilities: BTreeSet::new(),
        };
        let deposit = board.reserve_escrow(&escrow_secret(),
            depositor.clone(),
            900,
            EscrowPurpose::FundConsensusTask(intent),
            Utc::now() + chrono::Duration::minutes(30),
        );

        let EscrowConfirmation::TaskCreated(task) =
            board.confirm_escrow(deposit.id, 900, Utc::now()).unwrap();

        assert_eq!(task.poster, depositor);
        assert_eq!(task.escrow_id, Some(deposit.id));
        assert!(matches!(task.kind, TaskKind::Consensus { num_assignees: 3, .. }));
    }

    #[test]
    fn confirm_escrow_rejects_an_underfunded_deposit() {
        let mut board = TaskBoard::new();
        let intent = TaskIntent {
            description: "t".to_string(),
            bounty: 500,
            expected_output_hash: Hash::hash_bytes(b"x"),
            min_reputation: 0,
            capabilities: BTreeSet::new(),
        };
        let deposit = board.reserve_escrow(&escrow_secret(),
            pubkey(),
            500,
            EscrowPurpose::FundHashMatchTask(intent),
            Utc::now() + chrono::Duration::minutes(30),
        );

        assert!(matches!(
            board.confirm_escrow(deposit.id, 499, Utc::now()),
            Err(BoardError::EscrowUnderfunded { required: 500, have: 499 })
        ));
        // must not have consumed the reservation or created anything
        assert_eq!(board.get_pending_deposit(deposit.id).unwrap().status, EscrowStatus::Reserved);
    }

    #[test]
    fn confirm_escrow_rejects_an_expired_deposit() {
        let mut board = TaskBoard::new();
        let now = Utc::now();
        let intent = TaskIntent {
            description: "t".to_string(),
            bounty: 100,
            expected_output_hash: Hash::hash_bytes(b"x"),
            min_reputation: 0,
            capabilities: BTreeSet::new(),
        };
        let deposit = board.reserve_escrow(&escrow_secret(),
            pubkey(),
            100,
            EscrowPurpose::FundHashMatchTask(intent),
            now + chrono::Duration::minutes(5),
        );

        assert!(matches!(
            board.confirm_escrow(deposit.id, 100, now + chrono::Duration::minutes(6)),
            Err(BoardError::EscrowExpired)
        ));
    }

    #[test]
    fn confirm_escrow_rejects_an_already_consumed_deposit() {
        let mut board = TaskBoard::new();
        let intent = TaskIntent {
            description: "t".to_string(),
            bounty: 100,
            expected_output_hash: Hash::hash_bytes(b"x"),
            min_reputation: 0,
            capabilities: BTreeSet::new(),
        };
        let deposit = board.reserve_escrow(&escrow_secret(),
            pubkey(),
            100,
            EscrowPurpose::FundHashMatchTask(intent),
            Utc::now() + chrono::Duration::minutes(30),
        );
        board.confirm_escrow(deposit.id, 100, Utc::now()).unwrap();

        assert!(matches!(
            board.confirm_escrow(deposit.id, 100, Utc::now()),
            Err(BoardError::EscrowNotReserved)
        ));
    }

    #[test]
    fn confirm_escrow_on_a_missing_deposit_fails() {
        let mut board = TaskBoard::new();
        assert!(matches!(
            board.confirm_escrow(Uuid::new_v4(), 100, Utc::now()),
            Err(BoardError::EscrowNotFound)
        ));
    }

    #[test]
    fn overdue_reserved_escrows_lists_only_expired_still_reserved_deposits() {
        let mut board = TaskBoard::new();
        let now = Utc::now();
        let intent = |bounty| TaskIntent {
            description: "t".to_string(),
            bounty,
            expected_output_hash: Hash::hash_bytes(b"x"),
            min_reputation: 0,
            capabilities: BTreeSet::new(),
        };
        let expired = board.reserve_escrow(&escrow_secret(), pubkey(), 100, EscrowPurpose::FundHashMatchTask(intent(100)), now - chrono::Duration::seconds(1));
        let still_fresh = board.reserve_escrow(&escrow_secret(), pubkey(), 100, EscrowPurpose::FundHashMatchTask(intent(100)), now + chrono::Duration::minutes(30));
        let expired_but_confirmed = board.reserve_escrow(&escrow_secret(), pubkey(), 100, EscrowPurpose::FundHashMatchTask(intent(100)), now - chrono::Duration::seconds(1));
        board.confirm_escrow(expired_but_confirmed.id, 100, now - chrono::Duration::seconds(2)).unwrap();

        let overdue: Vec<Uuid> = board.overdue_reserved_escrows(now).iter().map(|d| d.id).collect();
        assert_eq!(overdue, vec![expired.id]);
        assert!(!overdue.contains(&still_fresh.id));
        assert!(!overdue.contains(&expired_but_confirmed.id), "already-consumed deposits aren't 'overdue', just done");
    }

    #[test]
    fn mark_escrow_refunded_transitions_status() {
        let mut board = TaskBoard::new();
        let intent = TaskIntent {
            description: "t".to_string(),
            bounty: 100,
            expected_output_hash: Hash::hash_bytes(b"x"),
            min_reputation: 0,
            capabilities: BTreeSet::new(),
        };
        let deposit = board.reserve_escrow(&escrow_secret(), pubkey(), 100, EscrowPurpose::FundHashMatchTask(intent), Utc::now() - chrono::Duration::seconds(1));

        board.mark_escrow_refunded(deposit.id).unwrap();
        assert_eq!(board.get_pending_deposit(deposit.id).unwrap().status, EscrowStatus::Refunded);
    }

    #[test]
    fn allocated_bounty_excludes_escrow_funded_tasks() {
        let mut board = TaskBoard::new();
        board.create_task(pubkey(), "operator-funded".to_string(), 100, Hash::hash_bytes(b"x"));
        assert_eq!(board.allocated_bounty(), 100);

        let intent = TaskIntent {
            description: "escrow-funded".to_string(),
            bounty: 900,
            expected_output_hash: Hash::hash_bytes(b"y"),
            min_reputation: 0,
            capabilities: BTreeSet::new(),
        };
        let deposit = board.reserve_escrow(&escrow_secret(), pubkey(), 900, EscrowPurpose::FundHashMatchTask(intent), Utc::now() + chrono::Duration::minutes(30));
        board.confirm_escrow(deposit.id, 900, Utc::now()).unwrap();

        assert_eq!(board.allocated_bounty(), 100, "the escrow-funded task's bounty must not count against the operator");
    }

    #[test]
    fn poster_cannot_claim_their_own_hash_match_task() {
        let mut board = TaskBoard::new();
        let poster = pubkey();
        let task = board.create_task(poster.clone(), "t".to_string(), 10, Hash::hash_bytes(b"x"));

        assert!(matches!(
            board.claim_task(task.id, poster, Utc::now() + chrono::Duration::minutes(5)),
            Err(BoardError::PosterCannotClaimOwnTask)
        ));
        assert_eq!(board.get_task(task.id).unwrap().status, TaskStatus::Open, "a rejected claim must not consume the task");
    }

    #[test]
    fn poster_cannot_join_their_own_consensus_task() {
        let mut board = TaskBoard::new();
        let poster = pubkey();
        let deadline = Utc::now() + chrono::Duration::minutes(30);
        let task = board.create_consensus_task(poster.clone(), "t".to_string(), 10, 2, deadline, 30);

        assert!(matches!(
            board.join_consensus_task(task.id, poster),
            Err(BoardError::PosterCannotClaimOwnTask)
        ));
    }

    #[test]
    fn escrow_for_task_finds_the_funding_deposit_and_none_for_operator_funded() {
        let mut board = TaskBoard::new();
        let operator_task = board.create_task(pubkey(), "t".to_string(), 10, Hash::hash_bytes(b"x"));
        assert!(board.escrow_for_task(operator_task.id).is_none());

        let intent = TaskIntent {
            description: "t".to_string(),
            bounty: 10,
            expected_output_hash: Hash::hash_bytes(b"x"),
            min_reputation: 0,
            capabilities: BTreeSet::new(),
        };
        let deposit = board.reserve_escrow(&escrow_secret(), pubkey(), 10, EscrowPurpose::FundHashMatchTask(intent), Utc::now() + chrono::Duration::minutes(30));
        let EscrowConfirmation::TaskCreated(task) = board.confirm_escrow(deposit.id, 10, Utc::now()).unwrap();

        assert_eq!(board.escrow_for_task(task.id).unwrap().id, deposit.id);
    }

    /// Creates a `Disputable` task, claims it, and submits an answer,
    /// landing it in `AwaitingDispute` -- the common starting point for
    /// most dispute tests below.
    fn create_and_submit_disputable_task(
        board: &mut TaskBoard,
        poster: PublicKey,
        claimant: PublicKey,
        bounty: u64,
        dispute_window_minutes: i64,
    ) -> Uuid {
        let task = board.create_disputable_task(poster, "open-ended work".to_string(), bounty, dispute_window_minutes);
        board.claim_task(task.id, claimant.clone(), Utc::now() + chrono::Duration::minutes(30)).unwrap();
        board.submit_disputable_answer(task.id, claimant, "my answer".to_string(), Utc::now()).unwrap();
        assert_eq!(board.get_task(task.id).unwrap().status, TaskStatus::AwaitingDispute);
        task.id
    }

    /// Files (reserves + confirms) a dispute bond against `task_id` from
    /// `challenger`, landing the task in `Disputed`. Returns the bond's
    /// own escrow id (a *different* escrow than the task's own).
    fn file_dispute(board: &mut TaskBoard, task_id: Uuid, challenger: PublicKey, reason: &str) -> Uuid {
        let bounty = board.get_task(task_id).unwrap().bounty;
        let deposit = board.reserve_escrow(&escrow_secret(),
            challenger,
            bounty,
            EscrowPurpose::DisputeBond { task_id, reason: reason.to_string() },
            Utc::now() + chrono::Duration::minutes(30),
        );
        board.confirm_dispute_bond(deposit.id, bounty, Utc::now()).unwrap();
        assert_eq!(board.get_task(task_id).unwrap().status, TaskStatus::Disputed);
        deposit.id
    }

    #[test]
    fn disputable_task_finalizes_unchallenged_via_sweep_and_pays_the_claimant() {
        let mut board = TaskBoard::new();
        let claimant = pubkey();
        let task_id = create_and_submit_disputable_task(&mut board, pubkey(), claimant.clone(), 900, 30);

        // not yet due
        assert!(board.finalize_unchallenged_disputable_tasks(Utc::now()).is_empty());
        assert_eq!(board.get_task(task_id).unwrap().status, TaskStatus::AwaitingDispute);

        // now it is
        let later = Utc::now() + chrono::Duration::minutes(31);
        let finalized = board.finalize_unchallenged_disputable_tasks(later);
        assert_eq!(finalized, vec![task_id]);
        let task = board.get_task(task_id).unwrap();
        assert_eq!(task.status, TaskStatus::Verified);
        assert_eq!(task.pending_payouts(), vec![(claimant, 900)]);
    }

    #[test]
    fn submit_disputable_answer_rejects_non_claimant() {
        let mut board = TaskBoard::new();
        let task = board.create_disputable_task(pubkey(), "t".to_string(), 10, 30);
        let claimant = pubkey();
        let impostor = pubkey();
        board.claim_task(task.id, claimant, Utc::now() + chrono::Duration::minutes(30)).unwrap();

        assert!(matches!(
            board.submit_disputable_answer(task.id, impostor, "answer".to_string(), Utc::now()),
            Err(BoardError::NotClaimant)
        ));
    }

    #[test]
    fn confirm_dispute_bond_rejects_the_assignee() {
        let mut board = TaskBoard::new();
        let claimant = pubkey();
        let task_id = create_and_submit_disputable_task(&mut board, pubkey(), claimant.clone(), 900, 30);

        let deposit = board.reserve_escrow(&escrow_secret(),
            claimant, // the assignee itself, trying to dispute its own submission
            900,
            EscrowPurpose::DisputeBond { task_id, reason: "self-dispute attempt".to_string() },
            Utc::now() + chrono::Duration::minutes(30),
        );
        assert!(matches!(
            board.confirm_dispute_bond(deposit.id, 900, Utc::now()),
            Err(BoardError::AssigneeCannotDisputeOwnSubmission)
        ));
        assert_eq!(board.get_task(task_id).unwrap().status, TaskStatus::AwaitingDispute, "must not have attached");
    }

    #[test]
    fn confirm_dispute_bond_rejects_after_window_closed_without_consuming_deposit() {
        let mut board = TaskBoard::new();
        let claimant = pubkey();
        let challenger = pubkey();
        // A negative dispute_window_minutes forces the deadline into the
        // past the instant submission happens -- same trick used
        // elsewhere in this file for the analogous Consensus-side race.
        let task_id = create_and_submit_disputable_task(&mut board, pubkey(), claimant, 900, -1);

        let deposit = board.reserve_escrow(&escrow_secret(),
            challenger,
            900,
            EscrowPurpose::DisputeBond { task_id, reason: "too late".to_string() },
            Utc::now() + chrono::Duration::minutes(30),
        );
        assert!(matches!(
            board.confirm_dispute_bond(deposit.id, 900, Utc::now()),
            Err(BoardError::DisputeWindowClosed)
        ));
        // Must NOT have consumed the deposit -- the caller (handler) is
        // responsible for refunding it instead of losing it.
        assert_eq!(board.get_pending_deposit(deposit.id).unwrap().status, EscrowStatus::Reserved);
    }

    #[test]
    fn cannot_file_a_second_dispute_on_an_already_disputed_task() {
        let mut board = TaskBoard::new();
        let claimant = pubkey();
        let task_id = create_and_submit_disputable_task(&mut board, pubkey(), claimant, 900, 30);
        file_dispute(&mut board, task_id, pubkey(), "first challenge");

        let second_challenger = pubkey();
        let deposit = board.reserve_escrow(&escrow_secret(),
            second_challenger,
            900,
            EscrowPurpose::DisputeBond { task_id, reason: "second challenge".to_string() },
            Utc::now() + chrono::Duration::minutes(30),
        );
        // Status is already Disputed, not AwaitingDispute -- the same
        // guard that rejects a too-late confirmation also rejects this.
        assert!(matches!(
            board.confirm_dispute_bond(deposit.id, 900, Utc::now()),
            Err(BoardError::DisputeWindowClosed)
        ));
    }

    #[test]
    fn resolve_dispute_challenger_wins_dings_assignee_and_pays_challenger() {
        let mut board = TaskBoard::new();
        let claimant = pubkey();
        let challenger = pubkey();
        let task_id = create_and_submit_disputable_task(&mut board, pubkey(), claimant.clone(), 900, 30);
        file_dispute(&mut board, task_id, challenger.clone(), "wrong answer");

        let (winner, loser) = board.resolve_dispute(task_id, DisputeResolution::ChallengerWins).unwrap();
        assert_eq!(winner, challenger);
        assert_eq!(loser, claimant.clone());
        assert_eq!(board.reputation(&claimant).failed, 1);
        assert_eq!(board.reputation(&challenger).failed, 0, "not credited (completed) until actually paid");

        let task = board.get_task(task_id).unwrap();
        assert_eq!(task.status, TaskStatus::Verified);
        assert_eq!(task.pending_payouts(), vec![(challenger, 900)], "bounty leg goes to the challenger");
    }

    #[test]
    fn resolve_dispute_assignee_wins_dings_challenger_and_pays_assignee() {
        let mut board = TaskBoard::new();
        let claimant = pubkey();
        let challenger = pubkey();
        let task_id = create_and_submit_disputable_task(&mut board, pubkey(), claimant.clone(), 900, 30);
        file_dispute(&mut board, task_id, challenger.clone(), "actually correct");

        let (winner, loser) = board.resolve_dispute(task_id, DisputeResolution::AssigneeWins).unwrap();
        assert_eq!(winner, claimant.clone());
        assert_eq!(loser, challenger);
        assert_eq!(board.reputation(&challenger).failed, 1);

        let task = board.get_task(task_id).unwrap();
        assert_eq!(task.status, TaskStatus::Verified);
        assert_eq!(task.pending_payouts(), vec![(claimant, 900)], "bounty leg goes to the assignee -- the bond leg is separate, see settle_dispute_bond");
    }

    #[test]
    fn resolve_dispute_rejects_a_task_that_isnt_currently_disputed() {
        let mut board = TaskBoard::new();
        let claimant = pubkey();
        let task_id = create_and_submit_disputable_task(&mut board, pubkey(), claimant, 900, 30);

        // still just AwaitingDispute, no dispute filed yet
        assert!(matches!(
            board.resolve_dispute(task_id, DisputeResolution::ChallengerWins),
            Err(BoardError::NotDisputed)
        ));
    }

    #[test]
    fn cancel_task_rejects_a_disputed_task() {
        let mut board = TaskBoard::new();
        let claimant = pubkey();
        let task_id = create_and_submit_disputable_task(&mut board, pubkey(), claimant, 900, 30);
        file_dispute(&mut board, task_id, pubkey(), "contested");

        assert!(matches!(board.cancel_task(task_id), Err(BoardError::CannotCancelWhileDisputed)));
        // A Disputed task must stay Disputed, not silently become Closed
        // while a bond is actively contested.
        assert_eq!(board.get_task(task_id).unwrap().status, TaskStatus::Disputed);
    }

    #[test]
    fn cancel_task_still_accepts_an_awaiting_dispute_task() {
        // No bond posted yet, so there's nothing contested -- cancelling
        // here is the same as cancelling any other Open/Claimed task.
        let mut board = TaskBoard::new();
        let claimant = pubkey();
        let task_id = create_and_submit_disputable_task(&mut board, pubkey(), claimant, 900, 30);

        board.cancel_task(task_id).unwrap();
        assert_eq!(board.get_task(task_id).unwrap().status, TaskStatus::Closed);
    }

    // ---- Exchange v1 ----

    #[test]
    fn exchange_account_defaults_to_zero_for_a_new_pubkey() {
        let board = TaskBoard::new();
        let account = board.exchange_account(&pubkey());
        assert_eq!(account.base_balance, 0);
        assert_eq!(account.locked_base, 0);
        assert_eq!(account.compute_balance, 0);
        assert_eq!(account.locked_compute, 0);
    }

    #[test]
    fn confirm_exchange_deposit_credits_net_of_fee() {
        let mut board = TaskBoard::new();
        let depositor = pubkey();
        let deposit = board.reserve_escrow(&escrow_secret(),
            depositor.clone(),
            1,
            EscrowPurpose::FundExchangeAccount,
            Utc::now() + chrono::Duration::minutes(30),
        );

        let (credited_to, credited_amount) =
            board.confirm_exchange_deposit(deposit.id, 10_000, 1_000, Utc::now()).unwrap();

        assert_eq!(credited_to, depositor);
        assert_eq!(credited_amount, 9_000);
        assert_eq!(board.exchange_account(&depositor).base_balance, 9_000);
        assert_eq!(board.get_pending_deposit(deposit.id).unwrap().status, EscrowStatus::Consumed);
    }

    #[test]
    fn confirm_exchange_deposit_rejects_wrong_purpose() {
        let mut board = TaskBoard::new();
        let intent = TaskIntent {
            description: "t".to_string(),
            bounty: 100,
            expected_output_hash: Hash::hash_bytes(b"x"),
            min_reputation: 0,
            capabilities: BTreeSet::new(),
        };
        let deposit = board.reserve_escrow(&escrow_secret(),
            pubkey(),
            100,
            EscrowPurpose::FundHashMatchTask(intent),
            Utc::now() + chrono::Duration::minutes(30),
        );

        assert!(matches!(
            board.confirm_exchange_deposit(deposit.id, 100, 0, Utc::now()),
            Err(BoardError::WrongEscrowPurpose)
        ));
    }

    #[test]
    fn confirm_exchange_deposit_rejects_underfunded() {
        let mut board = TaskBoard::new();
        let deposit = board.reserve_escrow(&escrow_secret(),
            pubkey(),
            1_001,
            EscrowPurpose::FundExchangeAccount,
            Utc::now() + chrono::Duration::minutes(30),
        );

        assert!(matches!(
            board.confirm_exchange_deposit(deposit.id, 1_000, 0, Utc::now()),
            Err(BoardError::EscrowUnderfunded { required: 1_001, have: 1_000 })
        ));
    }

    #[test]
    fn confirm_exchange_deposit_rejects_expired() {
        let mut board = TaskBoard::new();
        let now = Utc::now();
        let deposit =
            board.reserve_escrow(&escrow_secret(), pubkey(), 1, EscrowPurpose::FundExchangeAccount, now + chrono::Duration::minutes(5));

        assert!(matches!(
            board.confirm_exchange_deposit(deposit.id, 10_000, 0, now + chrono::Duration::minutes(6)),
            Err(BoardError::EscrowExpired)
        ));
    }

    #[test]
    fn confirm_exchange_deposit_rejects_already_consumed() {
        let mut board = TaskBoard::new();
        let deposit = board.reserve_escrow(&escrow_secret(),
            pubkey(),
            1,
            EscrowPurpose::FundExchangeAccount,
            Utc::now() + chrono::Duration::minutes(30),
        );
        board.confirm_exchange_deposit(deposit.id, 10_000, 0, Utc::now()).unwrap();

        assert!(matches!(
            board.confirm_exchange_deposit(deposit.id, 10_000, 0, Utc::now()),
            Err(BoardError::EscrowNotReserved)
        ));
    }

    #[test]
    fn confirm_exchange_deposit_rejects_missing() {
        let mut board = TaskBoard::new();
        assert!(matches!(
            board.confirm_exchange_deposit(Uuid::new_v4(), 10_000, 0, Utc::now()),
            Err(BoardError::EscrowNotFound)
        ));
    }

    #[test]
    fn confirm_escrow_rejects_a_fund_exchange_account_purpose() {
        let mut board = TaskBoard::new();
        let deposit = board.reserve_escrow(&escrow_secret(),
            pubkey(),
            1,
            EscrowPurpose::FundExchangeAccount,
            Utc::now() + chrono::Duration::minutes(30),
        );

        assert!(matches!(
            board.confirm_escrow(deposit.id, 10_000, Utc::now()),
            Err(BoardError::WrongEscrowPurpose)
        ));
    }

    #[test]
    fn unswept_exchange_deposits_lists_only_consumed_exchange_deposits() {
        let mut board = TaskBoard::new();
        let confirmed = board.reserve_escrow(&escrow_secret(),
            pubkey(),
            1,
            EscrowPurpose::FundExchangeAccount,
            Utc::now() + chrono::Duration::minutes(30),
        );
        let still_reserved = board.reserve_escrow(&escrow_secret(),
            pubkey(),
            1,
            EscrowPurpose::FundExchangeAccount,
            Utc::now() + chrono::Duration::minutes(30),
        );
        let intent = TaskIntent {
            description: "t".to_string(),
            bounty: 100,
            expected_output_hash: Hash::hash_bytes(b"x"),
            min_reputation: 0,
            capabilities: BTreeSet::new(),
        };
        let unrelated = board.reserve_escrow(&escrow_secret(),
            pubkey(),
            100,
            EscrowPurpose::FundHashMatchTask(intent),
            Utc::now() + chrono::Duration::minutes(30),
        );
        board.confirm_exchange_deposit(confirmed.id, 10_000, 0, Utc::now()).unwrap();
        board.confirm_escrow(unrelated.id, 100, Utc::now()).unwrap();

        assert_eq!(board.unswept_exchange_deposits(), vec![confirmed.id]);
        assert_eq!(board.get_pending_deposit(still_reserved.id).unwrap().status, EscrowStatus::Reserved);
    }

    #[test]
    fn place_order_locks_the_correct_balance_for_a_buy() {
        let mut board = TaskBoard::new();
        let owner = pubkey();
        board.restore_exchange_account(owner.clone(), ExchangeAccount { base_balance: 1_000, ..Default::default() });

        let (order, trades) = board.place_order(owner.clone(), Side::Buy, 10, 50, Utc::now()).unwrap();

        assert!(trades.is_empty(), "no resting sell orders to match against");
        assert_eq!(order.status, OrderStatus::Open);
        let account = board.exchange_account(&owner);
        assert_eq!(account.locked_base, 500, "10 price * 50 quantity");
        assert_eq!(account.base_balance, 1_000, "balance itself untouched, only locked, until it actually fills");
    }

    #[test]
    fn place_order_locks_the_correct_balance_for_a_sell() {
        let mut board = TaskBoard::new();
        let owner = pubkey();
        board.restore_exchange_account(owner.clone(), ExchangeAccount { compute_balance: 100, ..Default::default() });

        let (order, trades) = board.place_order(owner.clone(), Side::Sell, 10, 40, Utc::now()).unwrap();

        assert!(trades.is_empty());
        assert_eq!(order.status, OrderStatus::Open);
        let account = board.exchange_account(&owner);
        assert_eq!(account.locked_compute, 40);
        assert_eq!(account.compute_balance, 100);
    }

    #[test]
    fn place_order_rejects_insufficient_available_balance() {
        let mut board = TaskBoard::new();
        let owner = pubkey();
        board.restore_exchange_account(owner.clone(), ExchangeAccount { base_balance: 100, ..Default::default() });

        assert!(matches!(
            board.place_order(owner, Side::Buy, 10, 20, Utc::now()),
            Err(BoardError::InsufficientBalance { available: 100, required: 200 })
        ));
    }

    #[test]
    fn place_order_rejects_when_balance_already_locked_by_another_open_order() {
        let mut board = TaskBoard::new();
        let owner = pubkey();
        board.restore_exchange_account(owner.clone(), ExchangeAccount { base_balance: 1_000, ..Default::default() });
        board.place_order(owner.clone(), Side::Buy, 10, 90, Utc::now()).unwrap(); // locks 900

        assert!(matches!(
            board.place_order(owner, Side::Buy, 10, 20, Utc::now()), // needs 200, only 100 free
            Err(BoardError::InsufficientBalance { available: 100, required: 200 })
        ));
    }

    #[test]
    fn place_order_rejects_zero_price_or_quantity() {
        let mut board = TaskBoard::new();
        let owner = pubkey();
        assert!(matches!(board.place_order(owner.clone(), Side::Buy, 0, 10, Utc::now()), Err(BoardError::InvalidOrder)));
        assert!(matches!(board.place_order(owner, Side::Buy, 10, 0, Utc::now()), Err(BoardError::InvalidOrder)));
    }

    #[test]
    fn place_order_rejects_overflowing_notional() {
        let mut board = TaskBoard::new();
        assert!(matches!(
            board.place_order(pubkey(), Side::Buy, u64::MAX, 2, Utc::now()),
            Err(BoardError::OrderNotionalOverflow)
        ));
    }

    #[test]
    fn matching_fills_at_the_resting_makers_price_not_the_takers() {
        let mut board = TaskBoard::new();
        let seller = pubkey();
        let buyer = pubkey();
        board.restore_exchange_account(seller.clone(), ExchangeAccount { compute_balance: 100, ..Default::default() });
        board.restore_exchange_account(buyer.clone(), ExchangeAccount { base_balance: 10_000, ..Default::default() });

        board.place_order(seller.clone(), Side::Sell, 8, 50, Utc::now()).unwrap(); // resting ask at 8
        let (order, trades) = board.place_order(buyer.clone(), Side::Buy, 10, 50, Utc::now()).unwrap(); // taker bid at 10, crosses

        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].price, 8, "fills at the resting maker's price, not the taker's own limit");
        assert_eq!(trades[0].quantity, 50);
        assert_eq!(order.status, OrderStatus::Filled);
        assert_eq!(board.exchange_account(&seller).base_balance, 400, "50 * 8");
        assert_eq!(board.exchange_account(&buyer).compute_balance, 50);
    }

    #[test]
    fn small_fills_round_the_taker_fee_down_to_zero() {
        // 10 bps of a fill under 1000 units floors to 0 -- every other
        // matching test in this file uses quantities well under that,
        // which is exactly why adding the fee didn't change any of
        // their expected balances; this test makes that fact explicit
        // rather than leaving it implicit.
        let mut board = TaskBoard::new();
        let seller = pubkey();
        let buyer = pubkey();
        board.restore_exchange_account(seller.clone(), ExchangeAccount { compute_balance: 50, ..Default::default() });
        board.restore_exchange_account(buyer.clone(), ExchangeAccount { base_balance: 1_000, ..Default::default() });

        board.place_order(seller, Side::Sell, 10, 50, Utc::now()).unwrap();
        let (_, trades) = board.place_order(buyer.clone(), Side::Buy, 10, 50, Utc::now()).unwrap();

        assert_eq!(trades[0].taker_fee, 0);
        assert_eq!(board.exchange_account(&buyer).compute_balance, 50, "no fee actually withheld at this size");
    }

    #[test]
    fn buy_taker_pays_the_fee_in_compute_and_the_sell_maker_is_unaffected() {
        let mut board = TaskBoard::new();
        let seller = pubkey();
        let buyer = pubkey();
        board.restore_exchange_account(seller.clone(), ExchangeAccount { compute_balance: 5_000, ..Default::default() });
        board.restore_exchange_account(buyer.clone(), ExchangeAccount { base_balance: 100_000, ..Default::default() });

        board.place_order(seller.clone(), Side::Sell, 10, 5_000, Utc::now()).unwrap(); // resting ask
        let (_, trades) = board.place_order(buyer.clone(), Side::Buy, 10, 5_000, Utc::now()).unwrap(); // taker bid, crosses

        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].taker_side, Side::Buy);
        assert_eq!(trades[0].taker_fee, 5, "5_000 * 10 bps");
        assert_eq!(
            board.exchange_account(&buyer).compute_balance,
            4_995,
            "taker receives the fill minus the fee, never charged as an extra debit"
        );
        assert_eq!(
            board.exchange_account(&seller).base_balance,
            50_000,
            "the maker side is paid in full -- 5_000 * 10, no fee deducted"
        );
    }

    #[test]
    fn sell_taker_pays_the_fee_in_base_and_the_buy_maker_is_unaffected() {
        let mut board = TaskBoard::new();
        let buyer = pubkey();
        let seller = pubkey();
        board.restore_exchange_account(buyer.clone(), ExchangeAccount { base_balance: 100_000, ..Default::default() });
        board.restore_exchange_account(seller.clone(), ExchangeAccount { compute_balance: 1_000, ..Default::default() });

        board.place_order(buyer.clone(), Side::Buy, 10, 1_000, Utc::now()).unwrap(); // resting bid
        let (_, trades) = board.place_order(seller.clone(), Side::Sell, 10, 1_000, Utc::now()).unwrap(); // taker ask, crosses

        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].taker_side, Side::Sell);
        assert_eq!(trades[0].taker_fee, 10, "notional 10_000 * 10 bps");
        assert_eq!(
            board.exchange_account(&seller).base_balance,
            9_990,
            "taker receives the notional minus the fee"
        );
        assert_eq!(
            board.exchange_account(&buyer).compute_balance,
            1_000,
            "the maker side is paid in full -- no fee deducted"
        );
    }

    #[test]
    fn taker_price_improvement_credits_the_slack_back_to_available_balance() {
        let mut board = TaskBoard::new();
        let seller = pubkey();
        let buyer = pubkey();
        board.restore_exchange_account(seller.clone(), ExchangeAccount { compute_balance: 50, ..Default::default() });
        board.restore_exchange_account(buyer.clone(), ExchangeAccount { base_balance: 1_000, ..Default::default() });

        board.place_order(seller, Side::Sell, 8, 50, Utc::now()).unwrap();
        board.place_order(buyer.clone(), Side::Buy, 10, 50, Utc::now()).unwrap();

        let account = board.exchange_account(&buyer);
        // Locked 500 (10*50) at placement, then the fill at 8 debits only
        // 400 and releases the full 500 lock -- the 100 difference must
        // land back in available balance (base_balance - locked_base),
        // not get stranded in a lock that's no longer covering anything.
        assert_eq!(account.base_balance, 600, "1000 - 400 actually spent");
        assert_eq!(account.locked_base, 0, "fully filled, nothing left locked");
    }

    #[test]
    fn partial_fill_leaves_the_order_open_with_reduced_locked_amount() {
        let mut board = TaskBoard::new();
        let seller = pubkey();
        let buyer = pubkey();
        board.restore_exchange_account(seller.clone(), ExchangeAccount { compute_balance: 20, ..Default::default() });
        board.restore_exchange_account(buyer.clone(), ExchangeAccount { base_balance: 1_000, ..Default::default() });

        board.place_order(seller, Side::Sell, 10, 20, Utc::now()).unwrap();
        let (order, trades) = board.place_order(buyer.clone(), Side::Buy, 10, 50, Utc::now()).unwrap();

        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].quantity, 20);
        assert_eq!(order.status, OrderStatus::Open, "50 requested, only 20 filled");
        assert_eq!(order.filled, 20);
        let account = board.exchange_account(&buyer);
        assert_eq!(account.locked_base, 300, "30 remaining unfilled quantity * price 10");
    }

    #[test]
    fn full_fill_across_multiple_resting_orders_releases_locked_balance_to_exactly_zero() {
        let mut board = TaskBoard::new();
        let seller_a = pubkey();
        let seller_b = pubkey();
        let buyer = pubkey();
        board.restore_exchange_account(seller_a.clone(), ExchangeAccount { compute_balance: 30, ..Default::default() });
        board.restore_exchange_account(seller_b.clone(), ExchangeAccount { compute_balance: 30, ..Default::default() });
        board.restore_exchange_account(buyer.clone(), ExchangeAccount { base_balance: 1_000, ..Default::default() });

        board.place_order(seller_a, Side::Sell, 10, 30, Utc::now()).unwrap();
        board.place_order(seller_b, Side::Sell, 10, 30, Utc::now()).unwrap();
        let (order, trades) = board.place_order(buyer.clone(), Side::Buy, 10, 60, Utc::now()).unwrap();

        assert_eq!(trades.len(), 2, "one taker order filled across two resting makers");
        assert_eq!(order.status, OrderStatus::Filled);
        let account = board.exchange_account(&buyer);
        assert_eq!(account.locked_base, 0);
        assert_eq!(account.base_balance, 400, "1000 - 600 spent across both fills");
    }

    #[test]
    fn price_time_priority_matches_the_earliest_order_at_a_tied_price() {
        let mut board = TaskBoard::new();
        let earlier = pubkey();
        let later = pubkey();
        let buyer = pubkey();
        board.restore_exchange_account(earlier.clone(), ExchangeAccount { compute_balance: 10, ..Default::default() });
        board.restore_exchange_account(later.clone(), ExchangeAccount { compute_balance: 10, ..Default::default() });
        board.restore_exchange_account(buyer.clone(), ExchangeAccount { base_balance: 1_000, ..Default::default() });
        let now = Utc::now();

        board.place_order(earlier.clone(), Side::Sell, 10, 10, now).unwrap();
        board.place_order(later, Side::Sell, 10, 10, now + chrono::Duration::seconds(1)).unwrap();
        let (_, trades) = board.place_order(buyer, Side::Buy, 10, 10, now + chrono::Duration::seconds(2)).unwrap();

        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].seller, earlier, "same price -- earliest resting order wins");
    }

    #[test]
    fn orders_never_self_trade() {
        let mut board = TaskBoard::new();
        let owner = pubkey();
        board.restore_exchange_account(
            owner.clone(),
            ExchangeAccount { base_balance: 1_000, compute_balance: 100, ..Default::default() },
        );

        board.place_order(owner.clone(), Side::Sell, 10, 50, Utc::now()).unwrap();
        let (order, trades) = board.place_order(owner, Side::Buy, 10, 50, Utc::now()).unwrap();

        assert!(trades.is_empty(), "must not match against its own resting order");
        assert_eq!(order.status, OrderStatus::Open);
    }

    #[test]
    fn cancel_order_releases_remaining_locked_balance() {
        let mut board = TaskBoard::new();
        let owner = pubkey();
        board.restore_exchange_account(owner.clone(), ExchangeAccount { base_balance: 1_000, ..Default::default() });
        let (order, _) = board.place_order(owner.clone(), Side::Buy, 10, 50, Utc::now()).unwrap();

        let cancelled = board.cancel_order(order.id, &owner).unwrap();

        assert_eq!(cancelled.status, OrderStatus::Cancelled);
        assert_eq!(board.exchange_account(&owner).locked_base, 0);
    }

    #[test]
    fn cancel_order_after_a_partial_fill_releases_only_the_unfilled_remainder() {
        let mut board = TaskBoard::new();
        let seller = pubkey();
        let buyer = pubkey();
        board.restore_exchange_account(seller.clone(), ExchangeAccount { compute_balance: 20, ..Default::default() });
        board.restore_exchange_account(buyer.clone(), ExchangeAccount { base_balance: 1_000, ..Default::default() });
        board.place_order(seller, Side::Sell, 10, 20, Utc::now()).unwrap();
        let (order, _) = board.place_order(buyer.clone(), Side::Buy, 10, 50, Utc::now()).unwrap();
        assert_eq!(board.exchange_account(&buyer).locked_base, 300, "30 unfilled remaining * 10");

        board.cancel_order(order.id, &buyer).unwrap();

        assert_eq!(board.exchange_account(&buyer).locked_base, 0);
    }

    #[test]
    fn cancel_order_rejects_non_owner() {
        let mut board = TaskBoard::new();
        let owner = pubkey();
        board.restore_exchange_account(owner.clone(), ExchangeAccount { base_balance: 1_000, ..Default::default() });
        let (order, _) = board.place_order(owner, Side::Buy, 10, 50, Utc::now()).unwrap();

        assert!(matches!(board.cancel_order(order.id, &pubkey()), Err(BoardError::NotOrderOwner)));
    }

    #[test]
    fn cancel_order_rejects_an_already_terminal_order() {
        let mut board = TaskBoard::new();
        let owner = pubkey();
        board.restore_exchange_account(owner.clone(), ExchangeAccount { base_balance: 1_000, ..Default::default() });
        let (order, _) = board.place_order(owner.clone(), Side::Buy, 10, 50, Utc::now()).unwrap();
        board.cancel_order(order.id, &owner).unwrap();

        assert!(matches!(board.cancel_order(order.id, &owner), Err(BoardError::OrderNotOpen)));
    }

    #[test]
    fn order_book_lists_bids_and_asks_best_first() {
        let mut board = TaskBoard::new();
        let a = pubkey();
        let b = pubkey();
        board.restore_exchange_account(
            a.clone(),
            ExchangeAccount { base_balance: 1_000, compute_balance: 100, ..Default::default() },
        );
        board.restore_exchange_account(
            b.clone(),
            ExchangeAccount { base_balance: 1_000, compute_balance: 100, ..Default::default() },
        );

        board.place_order(a.clone(), Side::Buy, 5, 10, Utc::now()).unwrap();
        board.place_order(b.clone(), Side::Buy, 8, 10, Utc::now()).unwrap();
        board.place_order(a, Side::Sell, 20, 10, Utc::now()).unwrap();
        board.place_order(b, Side::Sell, 15, 10, Utc::now()).unwrap();

        let (bids, asks) = board.order_book();
        assert_eq!(bids.iter().map(|o| o.price).collect::<Vec<_>>(), vec![8, 5], "bids best (highest) first");
        assert_eq!(asks.iter().map(|o| o.price).collect::<Vec<_>>(), vec![15, 20], "asks best (lowest) first");
    }

    #[test]
    fn debit_for_withdrawal_rejects_amount_exceeding_available_balance() {
        let mut board = TaskBoard::new();
        let owner = pubkey();
        board.restore_exchange_account(owner.clone(), ExchangeAccount { base_balance: 100, ..Default::default() });

        assert!(matches!(
            board.debit_for_withdrawal(&owner, 101),
            Err(BoardError::InsufficientBalance { available: 100, required: 101 })
        ));
        assert_eq!(board.exchange_account(&owner).base_balance, 100, "rejected debit must not touch the balance");

        board.debit_for_withdrawal(&owner, 100).unwrap();
        assert_eq!(board.exchange_account(&owner).base_balance, 0);
    }

    /// A debit of nothing used to succeed -- `available < 0` is false
    /// whatever the balance -- and everything downstream then behaved as
    /// though a real withdrawal were under way: a custody output spent, a
    /// network fee paid, a zero-value output created, all to move
    /// nothing. Repeated, it drains the pool that backs every ledger
    /// balance.
    #[test]
    fn debit_for_withdrawal_refuses_a_withdrawal_of_nothing() {
        let mut board = TaskBoard::new();
        let owner = pubkey();
        board.restore_exchange_account(owner.clone(), ExchangeAccount { base_balance: 100, ..Default::default() });

        assert!(matches!(board.debit_for_withdrawal(&owner, 0), Err(BoardError::ZeroWithdrawal)));
        assert_eq!(board.exchange_account(&owner).base_balance, 100, "and it must not have touched the balance");
    }

    /// The empty-account case, which is the one an attacker actually
    /// sends: no deposit, no balance, nothing locked, and the old check
    /// still waved it through because zero is not less than zero.
    #[test]
    fn a_withdrawal_of_nothing_is_refused_even_on_an_account_that_has_never_existed() {
        let mut board = TaskBoard::new();
        assert!(matches!(board.debit_for_withdrawal(&pubkey(), 0), Err(BoardError::ZeroWithdrawal)));
    }

    #[test]
    fn credit_compute_accumulates_across_multiple_calls() {
        let mut board = TaskBoard::new();
        let recipient = pubkey();
        board.credit_compute(&recipient, 10);
        board.credit_compute(&recipient, 5);
        assert_eq!(board.exchange_account(&recipient).compute_balance, 15);
    }

    /// Row one of plan §6.5's table. The recipient holding the exact
    /// output this attempt created is proof the transaction was mined,
    /// and it is the *primary* signal precisely because it survives what
    /// the operator's side does not: the operator's inputs are gone
    /// either way here.
    #[test]
    fn a_payout_whose_output_the_recipient_holds_is_confirmed() {
        let (worker, operator) = (pubkey(), pubkey());
        let spent = output(1_000, &operator);
        let paid = output(100, &worker);
        let attempt = attempt(&worker, &operator, &paid, &[&spent]);

        assert_eq!(
            attempt.resolve(&[(false, paid)], &[]),
            PayoutOutcome::Confirmed
        );
    }

    /// Still confirmed when the node marks the recipient's output: a
    /// mark means the mempool holds a transaction *spending* it, which
    /// can only happen to an output that exists. An agent that
    /// immediately spends its bounty must not read as unpaid.
    #[test]
    fn a_confirmed_payout_the_recipient_is_already_spending_is_still_confirmed() {
        let (worker, operator) = (pubkey(), pubkey());
        let spent = output(1_000, &operator);
        let paid = output(100, &worker);
        let attempt = attempt(&worker, &operator, &paid, &[&spent]);

        assert_eq!(
            attempt.resolve(&[(true, paid)], &[(false, spent)]),
            PayoutOutcome::Confirmed
        );
    }

    /// A withdrawal resolves by the same rule a payout does, on every
    /// row -- which is the claim that lets `WithdrawalAttempt` reuse
    /// `resolve_against` instead of carrying a second copy of it.
    ///
    /// Worth asserting rather than assuming, because a second copy of
    /// this rule is exactly the drift the shared function exists to
    /// prevent, and the rows differ from one another only in which
    /// evidence is missing.
    #[test]
    fn a_withdrawal_and_a_payout_resolve_identically_on_every_row() {
        let (owner, custody) = (pubkey(), pubkey());
        let spent = output(1_000, &custody);
        let paid = output(100, &owner);
        let payout = attempt(&owner, &custody, &paid, &[&spent]);
        let withdrawal = WithdrawalAttempt {
            id: Uuid::new_v4(),
            owner: owner.clone(),
            amount: 100,
            output_hash: paid.hash(),
            spent_inputs: vec![spent.hash()],
            source: custody.clone(),
            submitted_at: Utc::now(),
        };

        let confirmed = (vec![(true, paid.clone())], vec![(false, spent.clone())]);
        let never = (vec![], vec![(false, spent.clone())]);
        let ambiguous: (
            Vec<(bool, btclib::types::TransactionOutput)>,
            Vec<(bool, btclib::types::TransactionOutput)>,
        ) = (vec![], vec![]);

        for (recipient_utxos, source_utxos) in [confirmed, never, ambiguous] {
            assert_eq!(
                withdrawal.resolve(&recipient_utxos, &source_utxos),
                payout.resolve(&recipient_utxos, &source_utxos),
                "the two wrappers must not be able to disagree"
            );
        }
    }

    /// Row two. Nothing at the recipient, and every input still sitting
    /// unspent and unmarked at the source: no block and no mempool holds
    /// this transaction, so rebuilding it cannot duplicate a payment.
    #[test]
    fn a_payout_whose_inputs_are_all_still_unspent_never_landed() {
        let (worker, operator) = (pubkey(), pubkey());
        let spent = output(1_000, &operator);
        let paid = output(100, &worker);
        let attempt = attempt(&worker, &operator, &paid, &[&spent]);

        assert_eq!(
            attempt.resolve(&[], &[(false, spent)]),
            PayoutOutcome::NeverLanded
        );
    }

    /// A multi-input transaction needs *every* input back before the
    /// hub may call it lost. One input still spendable and another
    /// consumed is not a transaction that never happened -- it is a
    /// picture the hub cannot explain, and resending against it risks
    /// paying twice.
    #[test]
    fn a_payout_with_only_some_inputs_returned_is_ambiguous_not_lost() {
        let (worker, operator) = (pubkey(), pubkey());
        let (first, second) = (output(60, &operator), output(60, &operator));
        let paid = output(100, &worker);
        let attempt = attempt(&worker, &operator, &paid, &[&first, &second]);

        assert_eq!(
            attempt.resolve(&[], &[(false, first)]),
            PayoutOutcome::Ambiguous
        );
    }

    /// Row three, the case that must never collapse into either
    /// neighbour: the inputs are marked, so the node's mempool is
    /// holding a transaction that spends them -- most likely this very
    /// one, waiting for a block. Resending here is the duplicate the
    /// node answers with a strike.
    #[test]
    fn a_payout_whose_inputs_the_mempool_has_marked_is_ambiguous() {
        let (worker, operator) = (pubkey(), pubkey());
        let spent = output(1_000, &operator);
        let paid = output(100, &worker);
        let attempt = attempt(&worker, &operator, &paid, &[&spent]);

        assert_eq!(
            attempt.resolve(&[], &[(true, spent)]),
            PayoutOutcome::Ambiguous
        );
    }

    /// The same row reached the other way: the inputs are gone from the
    /// source entirely. Either the transaction confirmed and the
    /// recipient has since spent the output, or something else consumed
    /// them. Both are unresolvable from here.
    #[test]
    fn a_payout_whose_inputs_have_vanished_is_ambiguous() {
        let (worker, operator) = (pubkey(), pubkey());
        let spent = output(1_000, &operator);
        let paid = output(100, &worker);
        let attempt = attempt(&worker, &operator, &paid, &[&spent]);
        // Whatever the operator holds now, it is not the output this
        // attempt spent -- a fresh `unique_id` makes it a different one.
        assert_eq!(
            attempt.resolve(&[], &[(false, output(900, &operator))]),
            PayoutOutcome::Ambiguous
        );
    }

    /// A recipient holding a payment of the same size from an *earlier*
    /// attempt must not confirm a later one. This is what the fresh
    /// `unique_id` on every output buys, and without it a resubmission
    /// would confirm itself against the money it was resubmitted
    /// because it lost.
    #[test]
    fn an_identical_payment_from_a_different_attempt_does_not_confirm_this_one() {
        let (worker, operator) = (pubkey(), pubkey());
        let spent = output(1_000, &operator);
        let earlier = output(100, &worker);
        let this_attempt = attempt(&worker, &operator, &output(100, &worker), &[&spent]);

        assert_ne!(
            this_attempt.resolve(&[(false, earlier)], &[(false, spent)]),
            PayoutOutcome::Confirmed,
            "same value, same recipient, different attempt -- must not read as this one confirming"
        );
    }

    /// An attempt with no recorded inputs proves nothing, and `all()`
    /// over an empty list would otherwise say "never landed" and
    /// authorize a resend.
    #[test]
    fn an_attempt_that_recorded_no_inputs_is_ambiguous_rather_than_lost() {
        let (worker, operator) = (pubkey(), pubkey());
        let paid = output(100, &worker);
        let attempt = attempt(&worker, &operator, &paid, &[]);

        assert_eq!(attempt.resolve(&[], &[]), PayoutOutcome::Ambiguous);
    }

    /// The board flips to `Submitted` only once every owed payout has a
    /// transaction in flight -- a `Consensus` task with one leg still
    /// unsent stays `Verified` so the sweep keeps sending it.
    #[test]
    fn a_task_reaches_submitted_only_when_every_leg_has_been_sent() {
        let mut board = TaskBoard::new();
        let (task_id, assignees) =
            create_and_fill_consensus_task(&mut board, 2, Utc::now() + chrono::Duration::minutes(30));
        for assignee in &assignees {
            board.submit_consensus_answer(task_id, assignee.clone(), "same".to_string()).unwrap();
        }
        assert_eq!(board.get_task(task_id).unwrap().status, TaskStatus::Verified);

        let source = pubkey();
        let first = output(450, &assignees[0]);
        let mut first_attempt = attempt(&assignees[0], &source, &first, &[&output(900, &source)]);
        first_attempt.task_id = task_id;
        board.record_payout_attempt(first_attempt);
        assert_eq!(
            board.get_task(task_id).unwrap().status,
            TaskStatus::Verified,
            "one winner still has nothing on the wire -- the sweep must keep trying"
        );
        assert_eq!(board.unsubmitted_payouts(task_id).len(), 1);

        let second = output(450, &assignees[1]);
        let mut second_attempt = attempt(&assignees[1], &source, &second, &[&output(900, &source)]);
        second_attempt.task_id = task_id;
        board.record_payout_attempt(second_attempt);
        assert_eq!(board.get_task(task_id).unwrap().status, TaskStatus::Submitted);
        assert!(
            board.unsubmitted_payouts(task_id).is_empty(),
            "nothing may be re-sent while it is already in flight"
        );
    }

    /// `PayoutFailed` must keep telling the truth about the money: the
    /// bounty is still owed, nobody's reputation was credited, and the
    /// escrow was not handed back to the poster.
    #[test]
    fn an_abandoned_payout_still_reports_the_money_as_owed() {
        let mut board = TaskBoard::new();
        let (poster, worker) = (pubkey(), pubkey());
        let task = board.create_task(poster, "t".to_string(), 100, Hash::hash(&"answer"));
        board.claim_task(task.id, worker.clone(), Utc::now() + chrono::Duration::minutes(10)).unwrap();
        board.submit(task.id, worker.clone(), Hash::hash(&"answer")).unwrap();

        board.mark_payout_failed(task.id).unwrap();
        let task = board.get_task(task.id).unwrap();
        assert_eq!(task.status, TaskStatus::PayoutFailed);
        assert_eq!(task.unconfirmed_payout_total(), 100, "the worker is still owed the bounty");
        assert_eq!(task.confirmed_payout_total(), 0);
        assert!(
            task.pending_payouts().is_empty(),
            "but nothing may be sent for it again without an operator"
        );
        assert_eq!(board.reputation(&worker).completed, 0, "an unpaid worker is not a completed one");
        assert_eq!(
            board.allocated_bounty(),
            100,
            "the operator's balance stays committed -- the debt is real"
        );
    }

    /// Confirmation drives the totals the DTOs report, and does so one
    /// winner at a time on a multi-winner task.
    #[test]
    fn confirmed_and_unconfirmed_totals_track_each_winner_separately() {
        let mut board = TaskBoard::new();
        let (task_id, assignees) =
            create_and_fill_consensus_task(&mut board, 2, Utc::now() + chrono::Duration::minutes(30));
        for assignee in &assignees {
            board.submit_consensus_answer(task_id, assignee.clone(), "same".to_string()).unwrap();
        }
        let before = board.get_task(task_id).unwrap();
        assert_eq!(before.confirmed_payout_total(), 0);
        assert_eq!(before.unconfirmed_payout_total(), 900);

        board.mark_recipient_paid(task_id, &assignees[0], 450).unwrap();
        let midway = board.get_task(task_id).unwrap();
        assert_eq!(midway.confirmed_payout_total(), 450);
        assert_eq!(midway.unconfirmed_payout_total(), 450);

        board.mark_recipient_paid(task_id, &assignees[1], 450).unwrap();
        let done = board.get_task(task_id).unwrap();
        assert_eq!(done.confirmed_payout_total(), 900);
        assert_eq!(done.unconfirmed_payout_total(), 0);
    }
    #[test]
    fn a_plurality_of_two_of_five_is_not_consensus() {
        let mut board = TaskBoard::new();
        let (id, keys) = create_and_fill_consensus_task(&mut board, 5, Utc::now() + chrono::Duration::minutes(30));
        for (key, answer) in keys.iter().zip(["collusion", "collusion", "a", "b", "c"]) {
            board.submit_consensus_answer(id, key.clone(), answer.to_string()).unwrap();
        }
        let task = board.get_task(id).unwrap();
        assert_eq!(task.status, TaskStatus::Closed);
        assert!(task.pending_payouts().is_empty());
    }
    #[test]
    fn one_of_five_cannot_win_after_timeout() {
        let mut board = TaskBoard::new();
        let (id, keys) = create_and_fill_consensus_task(&mut board, 5, Utc::now() + chrono::Duration::minutes(30));
        board.submit_consensus_answer(id, keys[0].clone(), "alone".to_string()).unwrap();
        board.resolve_expired_consensus_tasks(Utc::now() + chrono::Duration::hours(2));
        let task = board.get_task(id).unwrap();
        assert_eq!(task.status, TaskStatus::Closed);
        assert!(task.pending_payouts().is_empty());
        assert_eq!(task.unconfirmed_payout_total(), 0);
    }

    /// Every state in which money is owed or moving, in one table, so a
    /// new settlement state cannot be added without someone deciding what
    /// cancelling it means. `Submitted` is the one this originally missed
    /// alongside `PayoutFailed`: a payout on the wire whose task is
    /// cancelled releases a bounty the hub is still trying to pay.
    #[test]
    fn cancellation_is_refused_in_every_state_that_owes_money() {
        for (label, drive) in [
            ("Verified", 0usize),
            ("Submitted", 1),
            ("PayoutFailed", 2),
            ("Paid", 3),
        ] {
            let mut board = TaskBoard::new();
            let (poster, worker) = (pubkey(), pubkey());
            let task = board.create_task(poster, "owed".into(), 100, Hash::hash_bytes(b"ok"));
            board.claim_task(task.id, worker.clone(), Utc::now() + chrono::Duration::minutes(10)).unwrap();
            board.submit(task.id, worker.clone(), Hash::hash_bytes(b"ok")).unwrap();
            let submit_payout = |board: &mut TaskBoard| {
                board.record_payout_attempt(PayoutAttempt {
                    task_id: task.id,
                    recipient: worker.clone(),
                    amount: 100,
                    output_hash: Hash::hash_bytes(b"payout"),
                    spent_inputs: vec![Hash::hash_bytes(b"input")],
                    source: pubkey(),
                    submitted_at: Utc::now(),
                    submissions: 1,
                });
            };
            match drive {
                0 => {}
                1 => submit_payout(&mut board),
                2 => { board.mark_payout_failed(task.id).unwrap(); }
                _ => {
                    submit_payout(&mut board);
                    board.mark_recipient_paid(task.id, &worker, 100).unwrap();
                }
            }
            let before = board.get_task(task.id).unwrap().status;
            assert!(
                board.cancel_task(task.id).is_err(),
                "cancelling a {label} task would release a bounty that is owed or already moving"
            );
            assert_eq!(board.get_task(task.id).unwrap().status, before, "{label} must be untouched");
        }
    }

    #[test]
    fn cancellation_preserves_failed_payout_obligation() {
        let mut board = TaskBoard::new();
        let (poster, worker) = (pubkey(), pubkey());
        let task = board.create_task(poster, "owed".into(), 100, Hash::hash_bytes(b"ok"));
        board.claim_task(task.id, worker.clone(), Utc::now() + chrono::Duration::minutes(10)).unwrap();
        board.submit(task.id, worker, Hash::hash_bytes(b"ok")).unwrap();
        board.mark_payout_failed(task.id).unwrap();
        assert_eq!(board.allocated_bounty(), 100);
        assert!(board.cancel_task(task.id).is_err());
        assert_eq!(board.get_task(task.id).unwrap().status, TaskStatus::PayoutFailed);
        assert_eq!(board.allocated_bounty(), 100);
    }
}

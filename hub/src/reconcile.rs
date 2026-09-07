use tracing::*;

use crate::board::{EscrowPurpose, EscrowStatus, OrderStatus, TaskBoard, TaskKind, TaskStatus};

/// Cross-checks a freshly restored board for records that disagree with
/// each other, and **reports without repairing**.
///
/// The boot sequence in `main` loads each table independently and
/// blind-inserts every row -- `restore_task`, `restore_order`,
/// `restore_exchange_account`, `restore_payout_attempt` are each a bare
/// map insert. Nothing cross-checked a task against its deposit, an
/// order against its account's locked balance, or a task mid-settlement
/// against the attempt tracking it. Records that disagreed simply came
/// back disagreeing, and no log line said so (plan §6.5c).
///
/// Report-only is a deliberate choice, not a first increment. Repair
/// needs a policy per inconsistency, and most of those policies are
/// decisions nobody should make silently on an operator's behalf: an
/// orphaned `Consumed` deposit could be refunded, swept, or left for a
/// human, and which is right depends on facts only the chain and the
/// operator have. A startup banner naming what disagrees, plus a metric
/// to alert on, is what an operator can actually act on, and is far
/// cheaper to get right than a repair that guesses.
///
/// Every check here is read-only and computed from the board alone --
/// no node round trips, nothing on-chain. That keeps it on the critical
/// boot path safely: a hub whose node is unreachable still starts, and
/// still tells you what its store looks like.

/// One kind of disagreement, named so a metric can count them by class
/// and an operator can look the class up rather than parse prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disagreement {
    /// A deposit that reads `Consumed` -- it funded something -- while
    /// nothing on the board refers to it. Its money is on chain at an
    /// address the hub can still derive, and no task, dispute or
    /// exchange account claims it.
    OrphanedConsumedDeposit,
    /// An exchange account still holding a lock while its owner has no
    /// resting order. Nothing can justify a lock with nothing open
    /// behind it: the balance is unspendable by its owner and
    /// unreachable by the book.
    LockedBalanceWithNoOpenOrder,
    /// A task reading `Submitted` -- a payout is on the wire -- with no
    /// attempt tracking it. Neither sweep pass will ever look at it
    /// again (see
    /// `main::tests::a_submitted_task_with_no_attempt_is_recovered_by_nothing`),
    /// so this is a payout the hub has silently stopped waiting for.
    SubmittedTaskWithNoPayoutAttempt,
}

impl Disagreement {
    /// The stable identifier used in the metric label and the log line.
    /// Deliberately fixed strings and a closed set -- a metric label
    /// built from a uuid or a pubkey is unbounded cardinality, which
    /// `metrics::RouteKey` already documents as how a monitoring
    /// endpoint becomes the leak that takes the process down.
    pub fn label(self) -> &'static str {
        match self {
            Disagreement::OrphanedConsumedDeposit => "orphaned_consumed_deposit",
            Disagreement::LockedBalanceWithNoOpenOrder => "locked_balance_with_no_open_order",
            Disagreement::SubmittedTaskWithNoPayoutAttempt => "submitted_task_with_no_payout_attempt",
        }
    }
}

/// One finding: what disagrees, and enough identity to go and look.
#[derive(Debug, Clone)]
pub struct Finding {
    pub kind: Disagreement,
    /// Human-readable, and the only place record identities appear --
    /// they go to the log, never to a metric label.
    pub detail: String,
}

/// What one reconciliation pass found. An empty `findings` is the
/// ordinary case and is still worth logging, because "checked, agreed"
/// and "never checked" are the two states an operator most needs to
/// tell apart.
#[derive(Debug, Default)]
pub struct Report {
    pub findings: Vec<Finding>,
}

impl Report {
    pub fn is_empty(&self) -> bool {
        self.findings.is_empty()
    }

    pub fn count(&self, kind: Disagreement) -> usize {
        self.findings.iter().filter(|f| f.kind == kind).count()
    }

    /// Writes the report to the log at boot: one line per finding at
    /// `error`, then a summary.
    ///
    /// `error` rather than `warn` for the findings themselves. Each one
    /// is a record that cannot be explained by any sequence of correct
    /// operations, so it is a defect somewhere -- in the hub, or in
    /// whatever last touched the store -- and the point of the exercise
    /// is that it stops being invisible.
    pub fn log(&self) {
        for finding in &self.findings {
            error!("store reconciliation: [{}] {}", finding.kind.label(), finding.detail);
        }
        if self.findings.is_empty() {
            info!("store reconciliation: no disagreements found");
        } else {
            error!(
                "store reconciliation: {} disagreement(s) found -- the hub is starting anyway, \
                 and every one of them needs an operator (docs/deployment.md §10.3)",
                self.findings.len()
            );
        }
    }
}

/// Runs every check against `board` as restored from disk.
pub fn reconcile(board: &TaskBoard) -> Report {
    let mut findings = Vec::new();
    check_orphaned_consumed_deposits(board, &mut findings);
    check_locked_balances_without_open_orders(board, &mut findings);
    check_submitted_tasks_without_payout_attempts(board, &mut findings);
    Report { findings }
}

/// A `Consumed` deposit is the hub's own claim that this money funded
/// something. This looks for the something.
///
/// Only `Consumed` is checked. `Reserved` means nothing has been
/// confirmed yet and by construction nothing refers to it, and
/// `Refunded` means it is finished -- so both are silent by design, not
/// by omission.
fn check_orphaned_consumed_deposits(board: &TaskBoard, findings: &mut Vec<Finding>) {
    for deposit in board.all_pending_deposits() {
        if deposit.status != EscrowStatus::Consumed {
            continue;
        }
        let claimed = match &deposit.purpose {
            // A task escrow: some task must name it as its funding
            // source.
            EscrowPurpose::FundHashMatchTask(_)
            | EscrowPurpose::FundConsensusTask(_)
            | EscrowPurpose::FundDisputableTask(_) => {
                board.all_tasks().any(|task| task.escrow_id == Some(deposit.id))
            }
            // A dispute bond: the task it was filed against must exist
            // and its dispute must still name this bond. Checked through
            // the dispute rather than by `task_id` alone, because a bond
            // whose task exists but whose dispute points elsewhere is
            // exactly as orphaned as one whose task is gone.
            EscrowPurpose::DisputeBond { task_id, .. } => board
                .get_task(*task_id)
                .is_some_and(|task| match &task.kind {
                    TaskKind::Disputable { dispute: Some(d), .. } => d.bond_escrow_id == deposit.id,
                    _ => false,
                }),
            // An exchange deposit credits its depositor's ledger
            // balance, so that account must exist. A zero balance is
            // fine and common -- the credit may since have been traded
            // or withdrawn -- so this asks whether the account is there
            // at all, not what is in it.
            EscrowPurpose::FundExchangeAccount => board
                .all_exchange_accounts()
                .any(|(pubkey, _)| *pubkey == deposit.depositor),
        };
        if !claimed {
            findings.push(Finding {
                kind: Disagreement::OrphanedConsumedDeposit,
                detail: format!(
                    "deposit {} ({}, {} ITX required, depositor {}) reads Consumed but nothing \
                     on the board refers to it",
                    deposit.id,
                    purpose_label(&deposit.purpose),
                    deposit.required_amount,
                    deposit.depositor
                ),
            });
        }
    }
}

/// Names a deposit's purpose for a log line. Written out rather than
/// `{:?}` on the purpose itself, which would print the whole captured
/// `TaskIntent` -- an agent-supplied description inside an operator's
/// startup banner, at whatever length the request carried.
fn purpose_label(purpose: &EscrowPurpose) -> &'static str {
    match purpose {
        EscrowPurpose::FundHashMatchTask(_) => "hash-match task",
        EscrowPurpose::FundConsensusTask(_) => "consensus task",
        EscrowPurpose::FundDisputableTask(_) => "disputable task",
        EscrowPurpose::DisputeBond { .. } => "dispute bond",
        EscrowPurpose::FundExchangeAccount => "exchange deposit",
    }
}

/// An account holding a lock with nothing resting behind it.
///
/// Deliberately the weak form of this check. The strong form -- does
/// each account's lock equal the sum of what its open orders lock --
/// needs the expected lock per order, and that is a fill-accounting
/// question rather than arithmetic: a fill executes at the *resting*
/// order's price, which can differ from the price the taker's own order
/// locked against, and `place_order` reconciles the difference (see its
/// doc comment). Getting that wrong produces a report full of findings
/// that are not defects, which is worse than no report at all, because
/// an operator learns to ignore it.
///
/// "Locked, with nothing open" needs none of that accounting and admits
/// no correct explanation: `cancel_order` and every fill path release
/// the lock as they close the order, so a lock outliving the last open
/// order is a lock nobody can reach -- unspendable by its owner and
/// invisible to the book.
fn check_locked_balances_without_open_orders(board: &TaskBoard, findings: &mut Vec<Finding>) {
    for (pubkey, account) in board.all_exchange_accounts() {
        if account.locked_base == 0 && account.locked_compute == 0 {
            continue;
        }
        let has_open_order = board
            .all_orders()
            .any(|order| order.owner == *pubkey && order.status == OrderStatus::Open);
        if !has_open_order {
            findings.push(Finding {
                kind: Disagreement::LockedBalanceWithNoOpenOrder,
                detail: format!(
                    "account {pubkey} holds {} base and {} compute locked with no open order to \
                     justify either",
                    account.locked_base, account.locked_compute
                ),
            });
        }
    }
}

/// A task mid-settlement with nothing tracking the payout.
///
/// `Submitted` means every leg's transaction is on the wire and the hub
/// is waiting for chain evidence, so an attempt must exist for it. With
/// none, the resolution pass has nothing to resolve and the settlement
/// pass will not take a `Submitted` task -- the payout is neither owed
/// nor watched, and money that moved on chain is money the hub has
/// forgotten.
fn check_submitted_tasks_without_payout_attempts(board: &TaskBoard, findings: &mut Vec<Finding>) {
    let attempts = board.outstanding_payout_attempts();
    for task in board.all_tasks() {
        if task.status != TaskStatus::Submitted {
            continue;
        }
        if !attempts.iter().any(|attempt| attempt.task_id == task.id) {
            findings.push(Finding {
                kind: Disagreement::SubmittedTaskWithNoPayoutAttempt,
                detail: format!(
                    "task {} reads Submitted with no payout attempt tracking it: {} ITX left the \
                     hub and nothing is waiting for it",
                    task.id, task.bounty
                ),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{EscrowStatus, ExchangeAccount, Order, PayoutAttempt, Side, TaskIntent};
    use btclib::crypto::PrivateKey;
    use btclib::sha256::Hash;
    use chrono::Utc;
    use uuid::Uuid;

    /// A board with one escrow-funded task, its `Consumed` deposit, and
    /// nothing else -- the state a healthy hub reloads.
    fn board_with_a_funded_task() -> (TaskBoard, Uuid, Uuid) {
        let secret = crate::escrow_key::EscrowSecret::generate();
        let poster = PrivateKey::new_key().public_key();
        let mut board = TaskBoard::new();
        let deposit = board.reserve_escrow(
            &secret,
            poster,
            1_000_000 + 1_000,
            EscrowPurpose::FundHashMatchTask(TaskIntent {
                description: "escrowed work".to_string(),
                bounty: 1_000_000,
                expected_output_hash: Hash::hash_bytes(b"answer"),
                min_reputation: 0,
                capabilities: Default::default(),
            }),
            Utc::now() + chrono::Duration::minutes(30),
        );
        let deposit_id = deposit.id;
        // `EscrowConfirmation` has the one variant, so this destructure
        // is exhaustive today -- and will stop compiling rather than
        // silently mis-test if a second one is ever added.
        let crate::board::EscrowConfirmation::TaskCreated(task) =
            board.confirm_escrow(deposit_id, 1_000_000 + 1_000, Utc::now()).unwrap();
        (board, task.id, deposit_id)
    }

    #[test]
    fn a_consistent_board_produces_no_findings() {
        let (board, _, _) = board_with_a_funded_task();
        let report = reconcile(&board);
        assert!(report.is_empty(), "found {:?}", report.findings);
    }

    /// A `Reserved` deposit is referred to by nothing and must not be
    /// reported -- otherwise every hub with a funding request in flight
    /// starts by crying wolf, which is how a report gets ignored.
    #[test]
    fn a_reserved_deposit_referring_to_nothing_is_not_a_finding() {
        let secret = crate::escrow_key::EscrowSecret::generate();
        let mut board = TaskBoard::new();
        board.reserve_escrow(
            &secret,
            PrivateKey::new_key().public_key(),
            1_000,
            EscrowPurpose::FundExchangeAccount,
            Utc::now() + chrono::Duration::minutes(30),
        );
        assert!(reconcile(&board).is_empty());
    }

    /// The task table loading while the deposit table does not, or a
    /// task lost some other way: the deposit still claims it funded
    /// something that is not there.
    #[test]
    fn a_consumed_deposit_whose_task_is_missing_is_reported() {
        let (board, task_id, deposit_id) = board_with_a_funded_task();
        // Rebuild without the task, which is exactly what a boot from a
        // store missing that row does.
        let mut orphaned = TaskBoard::new();
        for deposit in board.all_pending_deposits() {
            orphaned.restore_pending_deposit(deposit.clone());
        }
        assert!(orphaned.get_task(task_id).is_none());

        let report = reconcile(&orphaned);
        assert_eq!(report.count(Disagreement::OrphanedConsumedDeposit), 1);
        assert!(
            report.findings[0].detail.contains(&deposit_id.to_string()),
            "the finding must name the deposit, or an operator cannot go and look: {}",
            report.findings[0].detail
        );
    }

    /// A dispute bond whose task exists but whose dispute points at a
    /// different bond. As orphaned as one whose task is gone, and the
    /// reason the check goes through the dispute rather than `task_id`.
    #[test]
    fn a_dispute_bond_its_task_no_longer_names_is_reported() {
        let secret = crate::escrow_key::EscrowSecret::generate();
        let (mut board, task_id, _) = board_with_a_funded_task();
        let bond = board.reserve_escrow(
            &secret,
            PrivateKey::new_key().public_key(),
            1_000,
            EscrowPurpose::DisputeBond { task_id, reason: "wrong".to_string() },
            Utc::now() + chrono::Duration::minutes(30),
        );
        let bond_id = bond.id;
        // Consumed without the dispute ever being attached: the task is
        // a HashMatch, so no `Disputable` dispute can name this bond.
        let mut consumed = bond.clone();
        consumed.status = EscrowStatus::Consumed;
        board.restore_pending_deposit(consumed);

        let report = reconcile(&board);
        assert_eq!(report.count(Disagreement::OrphanedConsumedDeposit), 1);
        assert!(report.findings[0].detail.contains(&bond_id.to_string()));
    }

    /// The exchange half, in the one form that admits no correct
    /// explanation: a lock with nothing resting behind it.
    #[test]
    fn a_locked_balance_with_no_open_order_is_reported() {
        let mut board = TaskBoard::new();
        let owner = PrivateKey::new_key().public_key();
        board.restore_exchange_account(
            owner.clone(),
            ExchangeAccount { base_balance: 500, locked_base: 8_000, ..Default::default() },
        );
        // A cancelled order is not an open one -- this is the state
        // `cancel_order` leaves if the release and the order's status
        // reach disk separately.
        board.restore_order(Order {
            id: Uuid::new_v4(),
            owner: owner.clone(),
            side: Side::Buy,
            price: 10,
            quantity: 800,
            filled: 0,
            status: OrderStatus::Cancelled,
            created_at: Utc::now(),
        });

        let report = reconcile(&board);
        assert_eq!(report.count(Disagreement::LockedBalanceWithNoOpenOrder), 1);
        assert!(report.findings[0].detail.contains("8000"));
    }

    /// The control for the check above: the same lock with an open order
    /// behind it is the ordinary resting state and must be silent. The
    /// amounts are deliberately *not* checked against each other -- see
    /// `check_locked_balances_without_open_orders` for why the strong
    /// form is not attempted.
    #[test]
    fn a_locked_balance_with_an_open_order_is_not_a_finding() {
        let mut board = TaskBoard::new();
        let owner = PrivateKey::new_key().public_key();
        board.restore_exchange_account(
            owner.clone(),
            ExchangeAccount { base_balance: 500, locked_base: 8_000, ..Default::default() },
        );
        board.restore_order(Order {
            id: Uuid::new_v4(),
            owner,
            side: Side::Buy,
            price: 10,
            quantity: 800,
            filled: 0,
            status: OrderStatus::Open,
            created_at: Utc::now(),
        });
        assert!(reconcile(&board).is_empty());
    }

    /// The payout half: the state bug 2's fix makes unreachable, which
    /// is precisely why the boot check for it is worth having -- an
    /// existing store may already hold one, and nothing would have said
    /// so.
    #[test]
    fn a_submitted_task_with_no_attempt_is_reported() {
        let (mut board, task_id, _) = board_with_a_funded_task();
        let claimant = PrivateKey::new_key().public_key();
        board.claim_task(task_id, claimant.clone(), Utc::now() + chrono::Duration::minutes(30)).unwrap();
        board.submit(task_id, claimant.clone(), Hash::hash_bytes(b"answer")).unwrap();
        board.record_payout_attempt(PayoutAttempt {
            task_id,
            recipient: claimant.clone(),
            amount: 1_000_000,
            output_hash: Hash::hash_bytes(b"payout"),
            spent_inputs: vec![Hash::hash_bytes(b"input")],
            source: PrivateKey::new_key().public_key(),
            submitted_at: Utc::now(),
            submissions: 1,
        });
        assert_eq!(board.get_task(task_id).unwrap().status, TaskStatus::Submitted);
        assert!(reconcile(&board).is_empty(), "with the attempt present, nothing to report");

        board.clear_payout_attempt(task_id, &claimant);
        let report = reconcile(&board);
        assert_eq!(report.count(Disagreement::SubmittedTaskWithNoPayoutAttempt), 1);
        assert!(report.findings[0].detail.contains(&task_id.to_string()));
    }

    /// Labels are what a metric is keyed by, so they are a compatibility
    /// surface: renaming one silently breaks whatever alert an operator
    /// built on it. Pinned here so a rename is a test change.
    #[test]
    fn every_disagreement_has_a_stable_label() {
        assert_eq!(
            Disagreement::OrphanedConsumedDeposit.label(),
            "orphaned_consumed_deposit"
        );
        assert_eq!(
            Disagreement::LockedBalanceWithNoOpenOrder.label(),
            "locked_balance_with_no_open_order"
        );
        assert_eq!(
            Disagreement::SubmittedTaskWithNoPayoutAttempt.label(),
            "submitted_task_with_no_payout_attempt"
        );
    }
}

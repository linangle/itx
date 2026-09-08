//! Durable settlement for faucet grants, withdrawals, refunds and deposit sweeps.
//! A record and its ledger reservation commit before transmission. Pending inputs
//! stay reserved across restarts; an uncertain outcome never releases a debit.
use crate::{AppState, board::{EscrowStatus, PayoutOutcome, resolve_against}};
use btclib::{crypto::PublicKey, sha256::Hash, types::{Transaction, TransactionOutput}};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status { Pending, Confirmed, NeedsReview }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Purpose {
    Faucet,
    Withdrawal { debited: u64 },
    Escrow { deposit_id: Uuid, forfeited_bond: bool, previous_status: EscrowStatus },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Payment {
    pub id: Uuid,
    pub purpose: Purpose,
    pub source: PublicKey,
    pub recipient: PublicKey,
    pub amount: u64,
    pub fee: u64,
    pub transaction: Option<Transaction>,
    pub output_hash: Option<Hash>,
    pub spent_inputs: Vec<Hash>,
    pub created_at: DateTime<Utc>,
    pub submitted_at: DateTime<Utc>,
    pub submissions: u32,
    pub status: Status,
    /// How far up the chain this payment's evidence has already been
    /// looked for. Only ever read by `scan_for_evidence`, which is only
    /// reached when the UTXO view has gone ambiguous, so on the ordinary
    /// path it stays exactly where `prepare` set it.
    ///
    /// `serde(default)` because records written before the scan existed
    /// have no such field, and zero is the safe reading of its absence:
    /// it costs a scan more blocks, never fewer.
    #[serde(default)]
    pub scanned_through: u64,
}

#[derive(Serialize)]
pub struct Receipt {
    pub payment_id: Uuid,
    pub status: Status,
    pub amount: u64,
    pub fee: u64,
    pub recipient: String,
}
impl Payment {
    pub fn receipt(&self) -> Receipt {
        Receipt { payment_id: self.id, status: self.status.clone(), amount: self.amount,
            fee: self.fee, recipient: self.recipient.to_string() }
    }
    pub fn new(purpose: Purpose, source: PublicKey, recipient: PublicKey, amount: u64,
        transaction: Option<Transaction>) -> Self {
        let output_hash = transaction.as_ref().and_then(|tx| tx.outputs.first().map(|o| o.hash()));
        let spent_inputs = transaction.as_ref().map(|tx| tx.inputs.iter()
            .map(|i| i.prev_transaction_output_hash).collect()).unwrap_or_default();
        let fee = if transaction.is_some() { crate::handlers::HUB_TRANSACTION_FEE } else { 0 };
        Self { id: Uuid::new_v4(), purpose, source, recipient, amount, fee, transaction,
            output_hash, spent_inputs, created_at: Utc::now(), submitted_at: Utc::now(),
            submissions: 1, status: Status::Pending, scanned_through: 0 }
    }
}

/// Also applied to wallet reshapes and task payouts, so maintenance cannot
/// consume the evidence or funds backing an unresolved non-task payment.
pub async fn reserve_inputs(state: &AppState, source: &PublicKey,
    utxos: &mut [(bool, TransactionOutput)]) {
    let board = state.board.read().await;
    for payment in board.payments.values().filter(|p| p.source == *source && p.status != Status::Confirmed) {
        for (marked, output) in utxos.iter_mut() {
            if payment.spent_inputs.contains(&output.hash()) { *marked = true; }
        }
    }

}

/// Caller holds the funding wallet/escrow guard. Publish the reservation only
/// after its entire durable effect commits; disk failure changes no live state.
pub async fn prepare(state: &AppState, payment: Payment) -> anyhow::Result<Payment> {
    let mut payment = payment;
    // Where an evidence scan would start, recorded now rather than
    // derived later: the transaction cannot be mined into a block older
    // than the chain the hub could see when it built it.
    payment.scanned_through = state.metrics.chain_height
        .load(std::sync::atomic::Ordering::Relaxed)
        .saturating_sub(EVIDENCE_SCAN_LOOKBACK);
    if let Some(tx) = &payment.transaction {
        anyhow::ensure!(tx.outputs.first().is_some_and(|o| o.pubkey == payment.recipient && o.value == payment.amount), "payment output does not match its receipt");
    } else {
        anyhow::ensure!(payment.amount == 0 && matches!(payment.purpose, Purpose::Escrow { .. }), "only an empty escrow can settle without a transaction");
    }
    let mut board = state.board.write().await;
    let mut account = None;
    let mut deposit = None;
    match payment.purpose {
        Purpose::Faucet => {
            anyhow::ensure!(board.can_claim_faucet(&payment.recipient), "faucet already reserved");
        }
        Purpose::Withdrawal { debited } => {
            let mut a = board.exchange_account(&payment.recipient);
            anyhow::ensure!(a.base_balance.saturating_sub(a.locked_base) >= debited,
                "insufficient available exchange balance");
            a.base_balance -= debited;
            account = Some(a);
        }
        Purpose::Escrow { deposit_id, previous_status, .. } => {
            anyhow::ensure!(!board.payments.values().any(|p|
                matches!(p.purpose, Purpose::Escrow { deposit_id: id, .. } if id == deposit_id)),
                "escrow already has a payment");
            let mut d = board.get_pending_deposit(deposit_id).cloned()
                .ok_or_else(|| anyhow::anyhow!("escrow missing"))?;
            anyhow::ensure!(d.status == previous_status, "escrow changed while preparing disbursement");
            anyhow::ensure!(d.status != EscrowStatus::Refunded && d.status != EscrowStatus::Disbursing,
                "escrow already disbursed");
            d.status = EscrowStatus::Disbursing;
            deposit = Some(d);
        }
    }
    state.store.save_payment_effects(&payment, account.as_ref().map(|a| (&payment.recipient, a)),
        deposit.as_ref(), None, matches!(payment.purpose, Purpose::Faucet))?;
    if let Some(a) = account { board.restore_exchange_account(payment.recipient.clone(), a); }
    if let Some(d) = deposit { board.restore_pending_deposit(d); }
    if matches!(payment.purpose, Purpose::Faucet) {
        board.restore_faucet_grant(payment.recipient.clone(), payment.created_at.timestamp());
    }
    board.payments.insert(payment.id, payment.clone());
    Ok(payment)
}

/// A send error is still a pending payment, never permission to undo its debit.
pub async fn send(state: &AppState, payment: &Payment) {
    if let Some(tx) = &payment.transaction {
        if let Err(e) = state.node.submit_transaction(tx.clone()).await {
            tracing::warn!("payment {} send uncertain; durable reservation retained: {e}", payment.id);
        }
    } else if let Err(e) = confirm(state, payment).await {
        tracing::warn!("empty escrow cleanup {} remains pending: {e}", payment.id);
    }
}

async fn confirm(state: &AppState, payment: &Payment) -> anyhow::Result<()> {
    let mut board = state.board.write().await;
    let Some(current) = board.payments.get(&payment.id) else { return Ok(()) };
    if current.status == Status::Confirmed { return Ok(()) }
    let mut done = current.clone();
    done.status = Status::Confirmed;
    let mut deposit = None;
    let mut reputation = None;
    if let Purpose::Escrow { deposit_id, forfeited_bond, .. } = done.purpose {
        let mut d = board.get_pending_deposit(deposit_id).cloned()
            .ok_or_else(|| anyhow::anyhow!("escrow missing during confirmation"))?;
        d.status = EscrowStatus::Refunded;
        deposit = Some(d);
        if forfeited_bond {
            let mut rep = board.reputation(&done.recipient);
            rep.total_earned = rep.total_earned.checked_add(done.amount)
                .ok_or_else(|| anyhow::anyhow!("earned amount overflow"))?;
            reputation = Some(rep);
        }
    }
    state.store.save_payment_effects(&done, None, deposit.as_ref(),
        reputation.as_ref().map(|r| (&done.recipient, r)), false)?;
    if let Some(d) = deposit { board.restore_pending_deposit(d); }
    if let Some(r) = reputation { board.restore_reputation(done.recipient.clone(), r); }
    board.payments.insert(done.id, done);
    Ok(())
}

/// How many blocks one pass may read looking for a payment's evidence.
///
/// Bounded so that a payment stuck behind a long chain cannot make a
/// single sweep pass unbounded -- the sweep has a sixty-second interval
/// to stay inside and other work to do. Progress is remembered on the
/// payment, so successive passes walk forward rather than restarting.
const EVIDENCE_SCAN_BLOCKS_PER_PASS: u32 = 64;

/// Blocks of slack before the height the hub believed current when the
/// payment was made. `metrics.chain_height` is sampled once a sweep and
/// can be a minute stale, and starting a scan too early only costs reads
/// while starting it too late would step over the block that settles it.
const EVIDENCE_SCAN_LOOKBACK: u64 = 4;

/// What the chain says about a payment the UTXO set could not settle.
#[derive(Debug, PartialEq, Eq)]
enum Evidence {
    /// The output this payment created exists in a mined block. The
    /// transaction landed, whatever the recipient has since done with it.
    Mined,
    /// Every block from the payment's own era to the tip has been read
    /// and the output is in none of them.
    AbsentFromChain,
    /// Not yet settled: there are blocks still unread, or the node could
    /// not be asked.
    Inconclusive,
}

/// Answers the one question `resolve_against` cannot: was this payment's
/// transaction mined?
///
/// **Why the UTXO view is not enough.** `resolve_against` confirms a
/// payment by finding its output still sitting at the recipient. A
/// recipient who has spent it onward no longer holds it, and the inputs
/// that funded it are gone from the source because mining consumed them
/// -- which is pixel-for-pixel what a payment that never landed looks
/// like when something else took its inputs. That pair is `Ambiguous`,
/// and `Ambiguous` used to resolve to nothing at all: the payment stayed
/// `Pending` for the life of the deployment, and
/// `hub_payments_oldest_pending_seconds` climbed past every threshold an
/// operator might alert on, retiring the alarm for the payments that
/// really were stuck.
///
/// The window is ordinary rather than exotic -- thirty seconds of grace
/// plus a sixty-second sweep, against an agent that does something with
/// its money on arrival.
///
/// **This is a lookup, not a sync.** The hub keeps no chain and gains
/// none here: it reads blocks from the payment's own era forward, in
/// bounded batches, only for a payment whose UTXO view has already gone
/// ambiguous, and remembers how far it got. On a healthy hub it never
/// runs at all.
async fn scan_for_evidence(state: &AppState, payment: &mut Payment) -> Evidence {
    let Some(wanted) = payment.output_hash else { return Evidence::Inconclusive };
    for _ in 0..2 {
        let start = payment.scanned_through as usize;
        let blocks = match state.node.fetch_blocks(start, EVIDENCE_SCAN_BLOCKS_PER_PASS).await {
            Ok(blocks) => blocks,
            Err(e) => {
                tracing::warn!("payment {} evidence scan could not read the chain: {e}", payment.id);
                return Evidence::Inconclusive;
            }
        };
        // An empty reply is the only thing that means the tip. A short
        // one does not: the responder caps its own batch size.
        if blocks.is_empty() {
            return Evidence::AbsentFromChain;
        }
        let found = blocks.iter().any(|block| {
            block.transactions.iter().any(|tx| tx.outputs.iter().any(|o| o.hash() == wanted))
        });
        payment.scanned_through = payment.scanned_through.saturating_add(blocks.len() as u64);
        if found {
            return Evidence::Mined;
        }
    }
    // Still blocks to read. The next pass picks up where this one stopped.
    Evidence::Inconclusive
}

pub async fn resolve_all(state: &AppState, now: DateTime<Utc>) {
    let payments: Vec<_> = state.board.read().await.payments.values()
        .filter(|p| p.status != Status::Confirmed).cloned().collect();
    for p in payments {
        if let Err(e) = resolve(state, &p, now).await {
            tracing::warn!("payment {} remains unresolved: {e}", p.id);
        }
    }
}

async fn resolve(state: &AppState, payment: &Payment, now: DateTime<Utc>) -> anyhow::Result<()> {
    if (now - payment.submitted_at).num_seconds() < crate::PAYOUT_RESOLUTION_GRACE_SECONDS { return Ok(()) }
    // One sweep owns resolution. HTTP creation is serialized by source; it
    // cannot reuse these inputs because reserve_inputs excludes the journal.
    let outcome = if let Some(hash) = payment.output_hash {
        let recipient = state.node.fetch_utxos(&payment.recipient).await?;
        let source = state.node.fetch_utxos(&payment.source).await?;
        resolve_against(&hash, &payment.spent_inputs, &recipient, &source)
    } else { PayoutOutcome::Confirmed }; // explicitly zero-value escrow cleanup
    match outcome {
        PayoutOutcome::Confirmed => confirm(state, payment).await?,
        PayoutOutcome::NeverLanded if payment.status == Status::Pending => {
            let mut next = payment.clone();
            if next.submissions >= crate::board::MAX_PAYOUT_SUBMISSIONS || next.transaction.is_none() {
                next.status = Status::NeedsReview;
            } else {
                next.submissions += 1;
                next.submitted_at = now;
            }
            {
                let mut board = state.board.write().await;
                let Some(current) = board.payments.get(&next.id) else { return Ok(()) };
                if current.submissions != payment.submissions || current.status != payment.status || current.submitted_at != payment.submitted_at { return Ok(()) }
                state.store.save_payment_effects(&next, None, None, None, false)?;
                board.payments.insert(next.id, next.clone());
            }
            if next.status == Status::Pending { send(state, &next).await; }
        }
        // Ambiguous: the UTXO set cannot tell a payment the recipient
        // already spent from one that never landed and lost its inputs to
        // something else. Ask the chain, which can.
        _ => {
            let mut scanned = payment.clone();
            match scan_for_evidence(state, &mut scanned).await {
                Evidence::Mined => confirm(state, payment).await?,
                // The output is in no block, and the inputs that would
                // have funded it are gone -- so nothing can mine this
                // transaction now and resending it is futile. Somebody
                // has to look, which is exactly what NeedsReview is.
                Evidence::AbsentFromChain => {
                    let mut next = scanned.clone();
                    next.status = Status::NeedsReview;
                    save_progress(state, payment, next).await?;
                }
                // Still reading, or the node could not be asked. The
                // obligation and its reserved inputs stand either way;
                // only the scan's progress is worth keeping.
                Evidence::Inconclusive => save_progress(state, payment, scanned).await?,
            }
        }
    }
    Ok(())
}

/// Commits `next` over `previous`, and only if nothing else has moved the
/// payment in the meantime -- the same compare-then-write the resend path
/// uses, for the same reason: one sweep owns resolution, but a restart
/// can reload a record between a scan starting and its result landing.
async fn save_progress(state: &AppState, previous: &Payment, next: Payment) -> anyhow::Result<()> {
    let mut board = state.board.write().await;
    let Some(current) = board.payments.get(&next.id) else { return Ok(()) };
    if current.submissions != previous.submissions
        || current.status != previous.status
        || current.submitted_at != previous.submitted_at
    {
        return Ok(());
    }
    state.store.save_payment_effects(&next, None, None, None, false)?;
    board.payments.insert(next.id, next);
    Ok(())
}

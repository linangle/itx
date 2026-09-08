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
            submissions: 1, status: Status::Pending }
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
    if matches!(payment.purpose, Purpose::Faucet) { board.restore_faucet_grant(payment.recipient.clone()); }
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
        _ => {} // uncertainty retains the obligation and its reserved inputs
    }
    Ok(())
}

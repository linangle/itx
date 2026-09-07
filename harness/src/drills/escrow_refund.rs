//! Refund an escrow, restart the hub, and ask which defence refuses the
//! second confirmation.
//!
//! This is the drill plan §6.5c asked for, and it exists because seven
//! drills missed the worst defect the hub has had. **Nothing had ever
//! checked that a status transition survives a restart.** Every other
//! drill points at tasks, payouts, rate limits or the replay guard, so a
//! deposit whose `Refunded` status reached memory and never reached disk
//! was outside the instrument rather than merely unlucky.
//!
//! # Why this one can assert
//!
//! `escrow-restart`, its sibling, hunts a one-step-wide interval between
//! two commits and therefore has to sample: it reports *inconclusive*
//! rather than clean when it finds nothing, because absence of an
//! interval is not something a sampling run can observe.
//!
//! This drill has no race in it at all. A refunded deposit either reads
//! `Refunded` on disk or it does not, on **every** restart, so one run is
//! a verdict. That is the whole reason it is worth writing separately
//! rather than as a third phase of `escrow-restart`: a drill that can
//! assert should not share a report with one that cannot.
//!
//! # What it actually observes, given there is no read endpoint
//!
//! The hub exposes no way to read a deposit's status, so the drill infers
//! it from *which* check refuses a retried confirmation.
//! `TaskBoard::confirm_escrow` tests the status before the funded amount:
//!
//! - `Refunded` on disk → refused by the status check, "already been
//!   consumed or refunded". The intended defence answered.
//! - `Reserved` on disk → the status check passes, and the retry is
//!   stopped only by the balance check, "requires at least N, only 0 has
//!   been received" — because the money has already gone back to the
//!   depositor.
//!
//! Both are `409`, so the drill reads the error text. The second is the
//! bug: §6.5c calls that balance check "the last line of defence standing
//! in for the intended one", and this is the run that says which one is
//! standing. A `200` would be worse than either and is checked for
//! separately.
//!
//! # The refund path this reaches, and the one it does not
//!
//! `refund_escrow` has two callers. The sweep's overdue-reservation pass
//! needs a deposit past `ESCROW_RESERVATION_TTL_MINUTES`, which is an
//! hour, and a drill cannot move the hub's clock — so that caller is out
//! of reach here. `refund_closed_task_escrow` is not: an operator
//! `cancel_task` on an escrow-funded task refunds immediately, through
//! the *same* `disburse_escrow`. So the code under test is identical and
//! the drill takes seconds rather than an hour.

use crate::client::{CancelPayload, ConfirmEscrowPayload, CreateTaskPayload, HubClient};
use crate::report::{Report, Section, Verdict};
use anyhow::Result;
use btclib::crypto::{PrivateKey, PublicKey};
use btclib::sha256::Hash;
use std::path::{Path, PathBuf};
use std::time::Duration;

const BOUNTY: u64 = 1_000_000;
const FEE: u64 = 1_000;
const ANSWER: &str = "the answer is 42";

/// How the hub refused a retried confirmation, which is this drill's
/// proxy for the deposit's status on disk.
#[derive(Debug, PartialEq, Eq)]
enum RefusedBy {
    /// "already been consumed or refunded" -- the status check, which is
    /// the defence that is supposed to answer.
    Status,
    /// "requires at least N, only M" -- the balance check. Reaching this
    /// means the status check let the retry through, so the deposit
    /// reloaded as `Reserved`.
    Balance,
    /// Not refused at all. A deposit whose money has gone back funded
    /// something a second time.
    NotRefused,
    /// Refused for some third reason, which is a finding of its own: the
    /// drill's inference is only sound while these are the two answers.
    Other(String),
}

impl RefusedBy {
    fn label(&self) -> &str {
        match self {
            RefusedBy::Status => "status",
            RefusedBy::Balance => "balance",
            RefusedBy::NotRefused => "nothing",
            RefusedBy::Other(_) => "other",
        }
    }
}

struct Escrow {
    id: String,
    description: String,
    deposit_pubkey: PublicKey,
    required: u64,
}

/// Reserves an escrow-funded task, pays its deposit address, and waits
/// for that payment to confirm.
async fn reserve_and_fund(
    hub: &HubClient,
    chain: &crate::chain::ChainView,
    poster: &PrivateKey,
    description: String,
) -> Result<Escrow> {
    let expected_output_hash = hex::encode(Hash::hash_bytes(ANSWER.as_bytes()).as_bytes());
    let reply = hub
        .post_signed(
            poster,
            "/tasks/escrow",
            CreateTaskPayload {
                description: description.clone(),
                bounty: BOUNTY,
                expected_output_hash,
                min_reputation: 0,
                capabilities: Vec::new(),
            },
        )
        .await?;
    anyhow::ensure!(reply.ok(), "reserving an escrow failed: {}", reply.error_text());

    let id = reply.body["escrow_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("no escrow_id in the reservation"))?
        .to_string();
    let address = reply.body["deposit_address"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("no deposit_address in the reservation"))?;
    let required = reply.body["required_amount"].as_u64().unwrap_or(BOUNTY);
    let deposit_pubkey = PublicKey::from_sec1_bytes(&hex::decode(address)?)?;

    chain.pay(poster, &deposit_pubkey, required, FEE).await?;
    let landed = chain
        .wait_for_balance(&deposit_pubkey, required, Duration::from_secs(180))
        .await?;
    anyhow::ensure!(
        landed >= required,
        "the escrow deposit did not confirm: {landed} of {required}"
    );

    Ok(Escrow { id, description, deposit_pubkey, required })
}

/// Confirms `escrow` into a real task and returns the task's id.
async fn confirm_into_task(
    hub: &HubClient,
    poster: &PrivateKey,
    escrow: &Escrow,
) -> Result<String> {
    let reply = hub
        .post_signed(
            poster,
            &format!("/tasks/escrow/{}/confirm", escrow.id),
            ConfirmEscrowPayload { escrow_id: escrow.id.clone() },
        )
        .await?;
    anyhow::ensure!(
        reply.ok(),
        "confirming the escrow for {} failed: {}",
        escrow.description,
        reply.error_text()
    );
    Ok(reply.body["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("no task id in the confirmation"))?
        .to_string())
}

/// Cancels `task_id` as the operator, which closes the task without a
/// winner and so refunds its escrow through `refund_closed_task_escrow`.
async fn cancel_as_operator(hub: &HubClient, operator: &PrivateKey, task_id: &str) -> Result<()> {
    let reply = hub
        .post_signed(
            operator,
            &format!("/tasks/{task_id}/cancel"),
            CancelPayload { task_id: task_id.to_string() },
        )
        .await?;
    anyhow::ensure!(reply.ok(), "cancelling task {task_id} failed: {}", reply.error_text());
    Ok(())
}

/// Retries a confirmation with a fresh envelope and classifies the
/// refusal. A fresh envelope matters: the replay guard has already spent
/// the original one, so a rejected replay would masquerade as the
/// deposit being protected.
async fn retry_confirm(hub: &HubClient, poster: &PrivateKey, escrow: &Escrow) -> Result<RefusedBy> {
    let reply = hub
        .post_signed(
            poster,
            &format!("/tasks/escrow/{}/confirm", escrow.id),
            ConfirmEscrowPayload { escrow_id: escrow.id.clone() },
        )
        .await?;
    if reply.ok() {
        return Ok(RefusedBy::NotRefused);
    }
    let error = reply.error_text();
    // Matched on the substrings that identify each check rather than on
    // the whole message, so rewording either error does not silently
    // turn every result into `Other`.
    if error.contains("consumed or refunded") {
        Ok(RefusedBy::Status)
    } else if error.contains("requires at least") {
        Ok(RefusedBy::Balance)
    } else {
        Ok(RefusedBy::Other(error))
    }
}

/// Waits for a refund to leave the escrow address. The refund is one
/// transaction the hub submits and does not wait for, so this is where
/// the drill stops trusting the hub and asks the chain.
async fn wait_for_drain(chain: &crate::chain::ChainView, pubkey: &PublicKey) -> Result<u64> {
    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    loop {
        let balance = chain.confirmed_balance(pubkey).await?;
        if balance == 0 {
            return Ok(0);
        }
        if std::time::Instant::now() >= deadline {
            return Ok(balance);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn task_count(hub: &HubClient) -> Result<usize> {
    let reply = hub.get("/tasks?status=all&limit=200").await?;
    Ok(reply.body.as_array().map(|tasks| tasks.len()).unwrap_or(0))
}

pub async fn run(repo: &Path, bin_dir: &Path, work_dir: PathBuf) -> Result<Report> {
    let mut harness = super::bring_up(repo, bin_dir, work_dir, true).await?;
    let chain = harness.stack.chain();
    let operator = harness.stack.operator_key.clone();
    let hub = harness.stack.hub_client()?;

    let poster = PrivateKey::new_key();
    chain
        .wait_for_utxo_count(&operator.public_key(), 3, Duration::from_secs(300))
        .await?;
    // Two escrows, each needing a bounty and a fee, plus generous slack
    // for the per-payment fees -- the same reasoning `escrow-restart`
    // records for funding an order of magnitude over the arithmetic.
    let needed = (BOUNTY + FEE) * 2 * 10;
    chain.pay(&operator, &poster.public_key(), needed, FEE).await?;
    let funded = chain
        .wait_for_balance(&poster.public_key(), needed, Duration::from_secs(180))
        .await?;
    anyhow::ensure!(funded >= needed, "could not fund the drill's poster");

    // ---- control: a refund with no restart --------------------------
    //
    // Run first and reported alongside, because without it a passing
    // drill cannot distinguish "the status is durable" from "the status
    // is right in memory too". The live board has always had this
    // correct; that was never the bug, and a reader should be able to
    // see the drill knows that.
    let control = reserve_and_fund(&hub, &chain, &poster, "escrow-refund control".to_string()).await?;
    let control_task = confirm_into_task(&hub, &poster, &control).await?;
    cancel_as_operator(&hub, &operator, &control_task).await?;
    let control_left = wait_for_drain(&chain, &control.deposit_pubkey).await?;
    let control_refusal = retry_confirm(&hub, &poster, &control).await?;

    // ---- the drill: the same refund, across a restart ---------------
    let subject = reserve_and_fund(&hub, &chain, &poster, "escrow-refund subject".to_string()).await?;
    let subject_task = confirm_into_task(&hub, &poster, &subject).await?;
    let poster_before_refund = chain.confirmed_balance(&poster.public_key()).await?;
    cancel_as_operator(&hub, &operator, &subject_task).await?;

    // Ask the chain, not the hub: the refund is a fire-and-forget submit
    // (§6.5's remaining list), so the hub reporting it means only that
    // the bytes left.
    let escrow_left = wait_for_drain(&chain, &subject.deposit_pubkey).await?;
    let poster_after_refund = chain
        .wait_for_balance(
            &poster.public_key(),
            poster_before_refund + subject.required - FEE,
            Duration::from_secs(180),
        )
        .await?;
    let refunded_to_depositor = poster_after_refund.saturating_sub(poster_before_refund);

    // A graceful restart, deliberately, not a kill. There is no interval
    // to catch here -- the question is only what the store holds, and a
    // clean stop makes the answer about durability rather than about
    // whether the drain finished.
    let tasks_before_restart = task_count(&hub).await?;
    harness.stack.stop_hub().await?;
    harness.stack.start_hub().await?;
    let tasks_after_restart = task_count(&hub).await?;

    let refusal = retry_confirm(&hub, &poster, &subject).await?;
    // Read after the retry: if the retry were accepted, this is where the
    // second task shows up.
    let tasks_after_retry = task_count(&hub).await?;

    harness.stack.shutdown().await;

    let mut section = Section::new("Refund an escrow, then restart the hub")
        .plan_item("§6.5c")
        .fact("refund_reached_the_depositor", refunded_to_depositor)
        .fact("escrow_balance_after_refund", escrow_left)
        .fact("control_escrow_balance_after_refund", control_left)
        .fact("control_refused_by_without_restart", control_refusal.label())
        .fact("refused_by_after_restart", refusal.label())
        .fact("tasks_before_restart", tasks_before_restart)
        .fact("tasks_after_restart", tasks_after_restart)
        .fact("tasks_after_retried_confirmation", tasks_after_retry);

    // The control has to hold for the subject's result to mean anything.
    if control_refusal != RefusedBy::Status {
        section = section.finding(format!(
            "the control refund -- no restart involved -- was refused by the {} check rather \
             than the status check. The live board's own view of a refunded deposit is wrong, \
             which is a worse problem than the one this drill is about and makes its verdict \
             unreadable.",
            control_refusal.label()
        ));
    }

    section = match &refusal {
        RefusedBy::Status => section
            .verdict(Verdict::Confirmed)
            .note(
                "After a refund and a restart, the retried confirmation was refused by the \
                 status check -- so the deposit came back off disk reading `Refunded`. This is \
                 a verdict and not a sample: a status either persists or it does not, on every \
                 restart, so one run settles it."
                    .to_string(),
            ),
        RefusedBy::Balance => section
            .verdict(Verdict::Refuted)
            .finding(
                "A refunded escrow deposit reloaded as `Reserved`. The retried confirmation got \
                 past the status check and was stopped only by the on-chain balance being \
                 empty, which is the last line of defence standing in for the intended one. \
                 Nothing writes `Refunded` to disk: the sweep will re-select this deposit at \
                 every boot for the life of the deployment, and a dispute bond in the same \
                 state re-credits its winner's reputation each time. Plan §6.5c."
                    .to_string(),
            ),
        RefusedBy::NotRefused => section
            .verdict(Verdict::Refuted)
            .finding(format!(
                "A confirmation was ACCEPTED for an escrow whose {refunded_to_depositor} had \
                 already gone back to its depositor. One payment funded a refund and a task. \
                 Task count went {tasks_after_restart} -> {tasks_after_retry} across the retry."
            )),
        RefusedBy::Other(error) => section
            .verdict(Verdict::Inconclusive)
            .finding(format!(
                "The retried confirmation was refused by neither the status nor the balance \
                 check, so this drill cannot infer the deposit's status from it: {error}. The \
                 inference is only sound while those are the two answers -- if a check was \
                 added ahead of them, this drill needs a read endpoint rather than a proxy."
            )),
    };

    if escrow_left > 0 {
        section = section.note(format!(
            "The escrow address still held {escrow_left} when the drill stopped waiting, so the \
             refund had not fully confirmed. Read the verdict with that in mind: the retry's \
             refusal may be the balance check answering honestly about money that is genuinely \
             still there."
        ));
    }

    let mut report = Report::new("drill: escrow-refund", harness.environment);
    report.push(section);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The classification is the whole drill -- if these substrings stop
    /// matching the hub's errors, every run silently becomes `Other` and
    /// the drill reports inconclusive forever rather than failing. Pinned
    /// against the strings `board::BoardError` actually renders.
    #[test]
    fn the_two_refusals_are_told_apart_by_substrings_the_hub_really_sends() {
        let status = "this escrow deposit has already been consumed or refunded";
        let balance = "escrow deposit requires at least 1001000, only 0 has been received so far";
        assert!(status.contains("consumed or refunded"));
        assert!(balance.contains("requires at least"));
        // And neither matches the other's marker, or a `Refunded` deposit
        // could be read as a `Reserved` one.
        assert!(!status.contains("requires at least"));
        assert!(!balance.contains("consumed or refunded"));
    }

    #[test]
    fn every_refusal_has_a_distinct_label() {
        let other = RefusedBy::Other(String::new());
        let labels = [
            RefusedBy::Status.label(),
            RefusedBy::Balance.label(),
            RefusedBy::NotRefused.label(),
            other.label(),
        ];
        let mut unique = labels.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), labels.len(), "a shared label would merge two outcomes");
    }
}

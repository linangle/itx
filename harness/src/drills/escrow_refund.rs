//! Settle a dispute bond, restart the hub, and see whether the winner
//! gets paid their reputation twice.
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
//! There is no race here. A refunded deposit either reads `Refunded` on
//! disk or it does not, on **every** restart, so one run is a verdict.
//! That is why this is a separate drill rather than a third phase of
//! `escrow-restart`: a drill that can assert should not share a report
//! with one that cannot.
//!
//! # What it observes, and the two observables that did not work
//!
//! **Attempt one: which check refuses a retried confirmation.** The first
//! version cancelled an escrow-funded task, restarted, and retried the
//! confirmation. `confirm_escrow` tests the status before the funded
//! amount, so the hope was that `Refunded` would be refused by the status
//! check while `Reserved` got past it and was stopped only by the balance
//! being empty. **It passed against a pre-fix hub**, which an A/B caught
//! and a clean run never would have: a cancelled task's deposit is
//! already `Consumed`, so with `Refunded` unpersisted it reloads as
//! `Consumed`, not `Reserved` — and the hub rejects *every* non-`Reserved`
//! status with the same error. The discriminator was blind to the
//! transition it was aimed at.
//!
//! **Attempt two: the winner's `total_earned` growing twice.** §6.5c, and
//! the handoff before it, named this as the worst consequence: a settled
//! bond reloads `Consumed`, `tasks_with_unsettled_dispute_bonds` selects
//! it again, the settlement re-runs and credits the winner a second time,
//! leaving the reputation ledger permanently wrong. Measured against a
//! pre-fix hub, **that is not what happens, and the claim is corrected in
//! §6.5c.** The re-settlement calls `credit_forfeited_bond` with the
//! *net amount the retry computed*, and the retry reads a drained
//! address, so it credits zero. The pre-fix ledger ends at exactly the
//! right number.
//!
//! The first run appeared to show a double credit only because a task's
//! bounty and its bond are both `bounty` in size, and the restart landed
//! before the bounty payout had confirmed — so the post-restart credit
//! that looked like a duplicated bond was the bounty arriving on time.
//! This drill now waits for the task to reach `Paid` before restarting,
//! which removes that confound, and asserts the ledger is *unharmed*
//! rather than harmed.
//!
//! **What it measures instead: the re-selection itself.** That is the
//! defect, whatever it costs. A bond whose `Refunded` status never
//! reached disk is handed back to the sweep's dispute-bond pass at every
//! boot, for the life of the deployment, each pass costing a node round
//! trip inside the sweep and ahead of payout resolution. The hub says so
//! itself, once per pass — `sweep: retried and settled dispute bond for
//! task <id>` — so the drill counts those lines after the restart. Zero
//! is the fixed hub; anything more is the bug, and it is monotonic in
//! total history rather than in live state.
//!
//! # The two refund callers
//!
//! `refund_escrow` and `settle_dispute_bond` both go through
//! `disburse_escrow`, which is where the status is written. This drill
//! drives the dispute-bond caller because it is the one whose
//! re-selection the hub announces. The sweep's overdue-reservation caller
//! shares the same code and the same commit, so it is covered by
//! construction rather than by measurement — stated here because that is
//! a real limit of this drill and not something to discover later.

use crate::client::{
    ClaimPayload, ConfirmDisputeEscrowPayload, ConfirmEscrowPayload, DisputeEscrowPayload,
    EscrowDisputableTaskPayload, HubClient, ResolveDisputePayload, SubmitPayload,
};
use crate::report::{Report, Section, Verdict};
use anyhow::{Context, Result};
use btclib::crypto::{PrivateKey, PublicKey};
use std::path::{Path, PathBuf};
use std::time::Duration;

const BOUNTY: u64 = 1_000_000;
const FEE: u64 = 1_000;
const ANSWER: &str = "the answer is 42";
/// The hub sweeps every 60 seconds, and the dispute-bond pass is part of
/// it. Two intervals plus slack, so a restart landing just after a tick
/// still gets a full pass observed rather than timing out on the drill's
/// own impatience.
const SWEEP_OBSERVATION: Duration = Duration::from_secs(150);

struct Reservation {
    id: String,
    deposit_pubkey: PublicKey,
}

/// Reads a reservation reply and pays the deposit address it names.
async fn fund_reservation(
    chain: &crate::chain::ChainView,
    payer: &PrivateKey,
    body: &serde_json::Value,
) -> Result<Reservation> {
    let id = body["escrow_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("no escrow_id in the reservation"))?
        .to_string();
    let address = body["deposit_address"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("no deposit_address in the reservation"))?;
    let required = body["required_amount"].as_u64().unwrap_or(BOUNTY);
    let deposit_pubkey = PublicKey::from_sec1_bytes(&hex::decode(address)?)?;

    chain.pay(payer, &deposit_pubkey, required, FEE).await?;
    let landed = chain
        .wait_for_balance(&deposit_pubkey, required, Duration::from_secs(180))
        .await?;
    anyhow::ensure!(
        landed >= required,
        "an escrow deposit did not confirm: {landed} of {required}"
    );
    Ok(Reservation { id, deposit_pubkey })
}

/// How many times the hub has announced re-settling an already-settled
/// dispute bond. The hub logs this once per sweep pass per selected
/// bond, so it is a direct count of `tasks_with_unsettled_dispute_bonds`
/// handing back something that should have left that set.
///
/// Reading the hub's log rather than an endpoint because there is no
/// endpoint: the board's deposit statuses are not exposed, and this line
/// is the hub stating the fact itself. The drill owns the process and the
/// log, so this is a first-party observation, not log-scraping a stranger.
fn bond_resettlements(work_dir: &Path) -> Result<usize> {
    let path = work_dir.join("logs").join("hub.log");
    let log =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    Ok(log
        .lines()
        .filter(|line| line.contains("retried and settled dispute bond"))
        .count())
}

/// Waits for `task_id` to reach `Paid`, i.e. for the bounty payout to be
/// confirmed on chain and credited.
///
/// The drill must not restart before this. A task's bounty and its bond
/// are both `bounty` in size, so a bounty confirming after the restart
/// adds exactly what a duplicated bond credit would -- which is the
/// confound that made the first run of this drill report a double credit
/// that had not happened.
async fn wait_for_paid(hub: &HubClient, task_id: &str) -> Result<String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(300);
    let mut status = String::new();
    while std::time::Instant::now() < deadline {
        let reply = hub.get(&format!("/tasks/{task_id}")).await?;
        status = reply.body["status"].as_str().unwrap_or("").to_string();
        if status == "Paid" {
            return Ok(status);
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    Ok(status)
}

/// Sweep passes the hub has completed since *this* boot.
///
/// The drill's CONFIRMED verdict says "the sweep did not re-select the
/// bond", and that is only worth anything if the sweep ran at all. On a
/// loaded machine the observation window could otherwise expire before
/// the first tick and the drill would report a clean result it had not
/// earned -- which is the same shape of false pass as the two wrong
/// observables this drill already went through.
async fn sweep_passes(hub: &HubClient) -> Result<u64> {
    let reply = hub.get("/metrics").await?;
    let text = reply
        .body
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| reply.body.to_string());
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("hub_sweep_passes_total ") {
            return Ok(value.trim().parse().unwrap_or(0));
        }
    }
    anyhow::bail!(
        "hub_sweep_passes_total is missing from /metrics; this drill needs it to know a sweep ran"
    )
}

async fn total_earned(hub: &HubClient, pubkey: &PublicKey) -> Result<u64> {
    let reply = hub.get(&format!("/reputation/{pubkey}")).await?;
    Ok(reply.body["total_earned"].as_u64().unwrap_or(0))
}

/// Drives a `Disputable` task all the way to a resolved dispute whose
/// bond has been forfeited to the assignee, and returns the assignee.
///
/// `AssigneeWins` specifically: that is the outcome that forfeits the
/// challenger's bond *forward* to the assignee and so credits
/// `total_earned`. `ChallengerWins` returns the bond to the challenger,
/// which is a refund and deliberately earns nobody anything.
async fn settle_a_forfeited_bond(
    harness: &mut super::Harness,
    poster: &PrivateKey,
    assignee: &PrivateKey,
    challenger: &PrivateKey,
) -> Result<(String, u64)> {
    let hub = harness.stack.hub_client()?;
    let chain = harness.stack.chain();
    let operator = harness.stack.operator_key.clone();

    // A disputable task, escrow-funded by its poster.
    let reply = hub
        .post_signed(
            poster,
            "/tasks/disputable/escrow",
            EscrowDisputableTaskPayload {
                description: "escrow-refund disputable".to_string(),
                bounty: BOUNTY,
                dispute_window_minutes: 60,
                min_reputation: 0,
                capabilities: Vec::new(),
            },
        )
        .await?;
    anyhow::ensure!(
        reply.ok(),
        "reserving the task escrow failed: {}",
        reply.error_text()
    );
    let task_escrow = fund_reservation(&chain, poster, &reply.body).await?;

    let reply = hub
        .post_signed(
            poster,
            &format!("/tasks/escrow/{}/confirm", task_escrow.id),
            ConfirmEscrowPayload {
                escrow_id: task_escrow.id.clone(),
            },
        )
        .await?;
    anyhow::ensure!(
        reply.ok(),
        "confirming the task escrow failed: {}",
        reply.error_text()
    );
    let task_id = reply.body["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("no task id in the confirmation"))?
        .to_string();

    // Claimed and answered, which opens the dispute window.
    let reply = hub
        .post_signed(
            assignee,
            &format!("/tasks/{task_id}/claim"),
            ClaimPayload {
                task_id: task_id.clone(),
            },
        )
        .await?;
    anyhow::ensure!(
        reply.ok(),
        "claiming the task failed: {}",
        reply.error_text()
    );
    let reply = hub
        .post_signed(
            assignee,
            &format!("/tasks/{task_id}/submit"),
            SubmitPayload {
                task_id: task_id.clone(),
                output: ANSWER.to_string(),
            },
        )
        .await?;
    anyhow::ensure!(
        reply.ok(),
        "submitting the answer failed: {}",
        reply.error_text()
    );

    // A challenger puts up a bond against that answer.
    let reply = hub
        .post_signed(
            challenger,
            &format!("/tasks/{task_id}/dispute/escrow"),
            DisputeEscrowPayload {
                task_id: task_id.clone(),
                reason: "escrow-refund drill dispute".to_string(),
            },
        )
        .await?;
    anyhow::ensure!(
        reply.ok(),
        "reserving the bond escrow failed: {}",
        reply.error_text()
    );
    let bond = fund_reservation(&chain, challenger, &reply.body).await?;

    let reply = hub
        .post_signed(
            challenger,
            &format!("/tasks/{task_id}/dispute/confirm"),
            ConfirmDisputeEscrowPayload {
                task_id: task_id.clone(),
                escrow_id: bond.id.clone(),
            },
        )
        .await?;
    anyhow::ensure!(
        reply.ok(),
        "confirming the bond failed: {}",
        reply.error_text()
    );

    // The operator finds for the assignee, so the bond is forfeited
    // forward and credited as earned.
    let reply = hub
        .post_signed(
            &operator,
            &format!("/tasks/{task_id}/dispute/resolve"),
            ResolveDisputePayload {
                task_id: task_id.clone(),
                outcome: "assignee_wins".to_string(),
            },
        )
        .await?;
    anyhow::ensure!(
        reply.ok(),
        "resolving the dispute failed: {}",
        reply.error_text()
    );

    // Wait for the bond to actually leave its address before restarting:
    // the settlement is a fire-and-forget submit, and a restart while the
    // coin is still there would make the retry's balance check pass and
    // muddle what the drill is measuring.
    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    let mut bond_left = chain.confirmed_balance(&bond.deposit_pubkey).await?;
    while bond_left > 0 && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        bond_left = chain.confirmed_balance(&bond.deposit_pubkey).await?;
    }

    Ok((task_id, bond_left))
}

pub async fn run(repo: &Path, bin_dir: &Path, work_dir: PathBuf) -> Result<Report> {
    let mut harness = super::bring_up(repo, bin_dir, work_dir, true).await?;
    let chain = harness.stack.chain();
    let operator = harness.stack.operator_key.clone();

    let poster = PrivateKey::new_key();
    let assignee = PrivateKey::new_key();
    let challenger = PrivateKey::new_key();
    chain
        .wait_for_utxo_count(&operator.public_key(), 3, Duration::from_secs(300))
        .await?;
    // The poster funds the task escrow and the challenger funds the bond,
    // each a bounty plus a fee. Funded an order of magnitude over the
    // arithmetic for the reason `escrow-restart` records: a deposit
    // address can ask for more than the bounty and every payment burns a
    // fee, so being exact costs a re-run and being generous costs
    // nothing.
    let needed = (BOUNTY + FEE) * 10;
    for who in [&poster, &challenger] {
        chain.pay(&operator, &who.public_key(), needed, FEE).await?;
    }
    for who in [&poster, &challenger] {
        let funded = chain
            .wait_for_balance(&who.public_key(), needed, Duration::from_secs(180))
            .await?;
        anyhow::ensure!(funded >= needed, "could not fund one of the drill's agents");
    }

    let (task_id, bond_left) =
        settle_a_forfeited_bond(&mut harness, &poster, &assignee, &challenger).await?;

    let hub = harness.stack.hub_client()?;
    let work_dir = harness.stack.work_dir().to_path_buf();

    // Settle the *bounty* leg fully before restarting. Bounty and bond
    // are the same size, so a bounty confirming after the restart is
    // indistinguishable from a duplicated bond credit -- exactly the
    // confound that made the first run of this drill report a double
    // credit that had not happened.
    let status_before_restart = wait_for_paid(&hub, &task_id).await?;
    let earned_before = total_earned(&hub, &assignee.public_key()).await?;
    let resettlements_before = bond_resettlements(&work_dir)?;

    // A graceful restart, deliberately, not a kill. There is no interval
    // to catch -- the question is only what the store holds, and a clean
    // stop makes the answer about durability rather than about whether a
    // drain finished.
    harness.stack.stop_hub().await?;
    harness.stack.start_hub().await?;

    // The re-selection happens on the first sweep pass after the reload,
    // not at boot -- so this waits for a re-selection to appear, or for a
    // pass to have completed, whichever comes first. The second is what a
    // clean verdict needs and the first is the finding. Watched rather
    // than slept through, so a run that goes wrong early says so early.
    //
    // `hub_sweep_passes_total` is incremented *after* `run_sweep_once`
    // returns, so a non-zero count means a whole pass has been and gone
    // and anything it was going to log is already logged.
    let deadline = std::time::Instant::now() + SWEEP_OBSERVATION;
    let mut resettlements_after = bond_resettlements(&work_dir)?;
    let mut passes = sweep_passes(&hub).await.unwrap_or(0);
    while std::time::Instant::now() < deadline
        && resettlements_after == resettlements_before
        && passes == 0
    {
        tokio::time::sleep(Duration::from_secs(5)).await;
        resettlements_after = bond_resettlements(&work_dir)?;
        passes = sweep_passes(&hub).await.unwrap_or(0);
    }
    // Read once more after the loop. The loop body reads the log before
    // the counter, so the iteration that sees `passes` go to one read the
    // log a moment earlier -- and the line it is looking for is written
    // during exactly that pass.
    let resettlements_after = bond_resettlements(&work_dir)?;
    let reselected = resettlements_after.saturating_sub(resettlements_before);
    let earned_after = total_earned(&hub, &assignee.public_key()).await?;
    let recredited = earned_after.saturating_sub(earned_before);

    harness.stack.shutdown().await;

    let mut section = Section::new("Settle a dispute bond, then restart the hub")
        .plan_item("§6.5c")
        // The task id goes in a note, not a fact. `harness compare`
        // diffs facts, and a fresh uuid every run would put a spurious
        // change line in every comparison -- which is how a baseline
        // stops being read.
        .note(format!("task under test: {task_id}"))
        .fact("task_status_before_restart", status_before_restart.clone())
        .fact("bond_balance_after_settlement", bond_left)
        .fact("total_earned_before_restart", earned_before)
        .fact("total_earned_after_a_sweep", earned_after)
        .fact("recredited_by_the_sweep", recredited)
        .fact("bond_reselected_after_restart", reselected)
        .fact("sweep_passes_after_restart", passes)
        .fact("sweep_observation_seconds", SWEEP_OBSERVATION.as_secs());

    anyhow::ensure!(
        earned_before > 0,
        "the assignee earned nothing from a forfeited bond, so this drill never set up the state \
         it measures -- fix the setup before reading any verdict from it"
    );
    anyhow::ensure!(
        status_before_restart == "Paid",
        "the task was {status_before_restart}, not Paid, when the drill restarted the hub -- the \
         bounty leg had not finished, and its later confirmation is indistinguishable from a \
         duplicated bond credit. No verdict can be read from this run."
    );

    if bond_left > 0 {
        section = section.note(format!(
            "The bond address still held {bond_left} when the drill stopped waiting, so the \
             forfeiture had not fully confirmed. A re-settlement would then find real coin and \
             move it, which is a different failure from the one being measured."
        ));
    }

    // Reported either way, because it is the correction this drill
    // exists to have measured: the re-settlement's credit is computed
    // from the *retry's* view of the address, which is drained, so it
    // adds nothing. §6.5c's original claim that the ledger ends up
    // permanently wrong does not hold.
    if recredited > 0 {
        section = section.finding(format!(
            "the winner's `total_earned` grew by {recredited} across a restart that should have \
             changed nothing. This contradicts the measurement §6.5c records -- that a \
             re-settlement credits zero because the retry reads a drained address -- so either \
             the bond had not finished settling or something credits reputation on a path this \
             drill does not know about. Worth chasing before trusting the verdict."
        ));
    }

    section = if reselected == 0 && passes == 0 {
        // Not clean and not refuted: the thing that would have done the
        // re-selecting never ran, so the run has no opinion. Said out
        // loud rather than counted as a pass, because a verdict this
        // drill did not earn is worth less than no verdict.
        section.verdict(Verdict::Inconclusive).finding(format!(
            "no sweep pass completed in the {}s after the restart, so nothing had the chance to \
             re-select the bond and this run cannot say whether the status persisted. Raise \
             SWEEP_OBSERVATION or find out why the hub's sweep is not ticking.",
            SWEEP_OBSERVATION.as_secs()
        ))
    } else if reselected == 0 {
        section.verdict(Verdict::Confirmed).note(format!(
            "The bond's `Refunded` status survived the restart: {passes} sweep pass(es) ran and \
             the dispute-bond pass did not select it again, and `total_earned` stayed at \
             {earned_before}. A verdict, not a sample -- a status either persists or it does \
             not, on every restart."
        ))
    } else {
        section.verdict(Verdict::Refuted).finding(format!(
            "A dispute bond that was already settled was handed back to the sweep {reselected} \
             time(s) after a restart. Its `Refunded` status is not written to disk, so it \
             reloads as `Consumed` and `tasks_with_unsettled_dispute_bonds` keeps selecting it \
             -- at every boot, for the life of the deployment, each pass costing a node round \
             trip inside the sweep and ahead of payout resolution. Monotonic in total history \
             rather than in live state. No coin duplicates and, measured here, no reputation \
             either: the retry reads a drained address and so credits zero, which is why this \
             stayed invisible. Plan §6.5c."
        ))
    };

    let mut report = Report::new("drill: escrow-refund", harness.environment);
    report.push(section);
    Ok(report)
}

#[cfg(test)]
mod tests {
    /// `AssigneeWins` is the outcome under test and the choice is not
    /// arbitrary: it forfeits the challenger's bond *forward*, which is
    /// the one case where receiving an escrow's balance counts as
    /// earning it and therefore the one that shows up in `total_earned`.
    /// `ChallengerWins` returns the bond to whoever posted it, which is
    /// a refund and credits nobody -- so a drill built on it would
    /// measure zero either way and pass against the bug.
    #[test]
    fn the_resolve_payload_puts_the_outcome_on_the_wire_as_the_hub_spells_it() {
        let payload = crate::client::ResolveDisputePayload {
            task_id: "11111111-1111-1111-1111-111111111111".to_string(),
            outcome: "assignee_wins".to_string(),
        };
        let json = serde_json::to_string(&payload).unwrap();
        // The hub's `DisputeResolution` derives `rename_all =
        // "snake_case"`, so this is the exact token it deserializes.
        // Asserted on the serialized body rather than the constant,
        // because the body is what the hub sees -- and a wrong outcome
        // here would not fail the drill, it would make it measure
        // `ChallengerWins`, which credits nobody and so passes against
        // the bug.
        assert!(json.contains("\"outcome\":\"assignee_wins\""), "{json}");
    }
}

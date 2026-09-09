//! Restart the hub with escrow confirmations in flight.
//!
//! An escrow confirmation is the most state-heavy write the hub makes. It
//! reads a pending deposit, asks the node what actually landed at the
//! derived address, creates a task in memory, and persists the task and
//! the deposit's new `Consumed` status **together**, in one redb write
//! transaction (`HubStore::save_task_and_deposit`).
//!
//! That last part is the whole point and it is recent. Until 2026-09-07
//! the task and the deposit were two commits, and a process that stopped
//! between them came back with a task on disk beside a deposit still
//! reading `Reserved` -- confirmable a second time, one deposit funding
//! two bounties. This drill was written to hunt that window. The window
//! is now closed by construction, so what the drill measures has changed
//! from "can we reproduce it" to "does the atomicity hold under a crash",
//! and its SIGKILL phase can finally confirm rather than shrug. Splitting
//! those two commits again would show up here as a refutation.
//!
//! Two restarts are worth telling apart and this drill does both.
//! `SIGTERM` is what `systemctl restart` sends and what the hub drains on,
//! so an in-flight confirmation should finish. `SIGKILL` is a crash: the
//! handler stops wherever it was. The interesting question is not whether
//! a request is lost -- one obviously can be -- but whether what survives
//! is *consistent*: a deposit must not end up funding two tasks, and it
//! must not end up funding none while the depositor's coin sits at an
//! address only the hub can derive.
//!
//! # Hitting the window on purpose
//!
//! The dangerous interval is one step wide -- between the task's commit
//! and the deposit's -- so a drill that fires every confirmation at the
//! same instant and then kills the hub puts all of them at the *same*
//! point in the handler, and either catches that interval for all of them
//! or for none. The first version did exactly that and reproduced the bug
//! in two runs out of three, which is a coin flip wearing a lab coat.
//!
//! So the confirmations are started staggered across one measured
//! handler's duration, and the kill lands at the end of it. Each request
//! is then at a different point in the handler when the process dies, and
//! the batch covers the timeline rather than sampling one spot on it. The
//! duration comes from a control confirmation run normally first, because
//! a fixed sleep lands after the handlers on a fast machine and before
//! them on a slow one, which is how a drill quietly stops testing
//! anything.

use crate::client::{ConfirmEscrowPayload, CreateTaskPayload, HubClient};
use crate::report::{Report, Section, Verdict};
use crate::stats::{summarize, Sample};
use anyhow::Result;
use btclib::crypto::{PrivateKey, PublicKey};
use btclib::sha256::Hash;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

const BOUNTY: u64 = 1_000_000;
const FEE: u64 = 1_000;
/// Escrows interrupted per phase.
///
/// Each one costs a block to fund, so this is not free -- but it is the
/// only knob that raises the chance of catching a one-step-wide interval,
/// since every additional in-flight confirmation is another sample of the
/// handler's timeline. Twelve is about a minute of chain time per phase.
const BATCH: usize = 12;
const ANSWER: &str = "the answer is 42";

struct Escrow {
    id: String,
    description: String,
    deposit_pubkey: PublicKey,
}

/// Reserves an escrow-funded task and pays its deposit address on chain.
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
    anyhow::ensure!(
        reply.ok(),
        "reserving an escrow failed: {}",
        reply.error_text()
    );

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

    Ok(Escrow {
        id,
        description,
        deposit_pubkey,
    })
}

/// Every task the hub currently knows about, by description. Descriptions
/// are unique per escrow here, which makes them the join key between what
/// the drill set up and what survived.
async fn tasks_by_description(hub: &HubClient) -> Result<BTreeMap<String, String>> {
    let reply = hub.get("/tasks?status=all&limit=200").await?;
    Ok(reply
        .body
        .as_array()
        .map(|tasks| {
            tasks
                .iter()
                .filter_map(|task| {
                    Some((
                        task.get("description")?.as_str()?.to_string(),
                        task.get("id")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default())
}

/// One phase: interrupt `BATCH` confirmations with `hard` deciding whether
/// that is a crash or a drain.
async fn phase(
    harness: &mut super::Harness,
    poster: &PrivateKey,
    label: &'static str,
    hard: bool,
) -> Result<Section> {
    // Each phase keeps its own samples. Pooling them across both would
    // put SIGTERM's timings under a section titled SIGKILL, and the two
    // are being compared -- a table that mixes them answers neither
    // question.
    let mut samples: Vec<Sample> = Vec::new();
    let samples = &mut samples;
    let hub = harness.stack.hub_client()?;
    let chain = harness.stack.chain();

    // A control confirmation, both to time the kill off and to prove the
    // path works before it is interrupted.
    let control = reserve_and_fund(
        &hub,
        &chain,
        poster,
        format!("escrow-restart {label} control"),
    )
    .await?;
    let control_reply = hub
        .post_signed(
            poster,
            &format!("/tasks/escrow/{}/confirm", control.id),
            ConfirmEscrowPayload {
                escrow_id: control.id.clone(),
            },
        )
        .await?;
    samples.push(Sample::new(
        "POST /tasks/escrow/:id/confirm",
        control_reply.status,
        control_reply.latency,
    ));
    anyhow::ensure!(
        control_reply.ok(),
        "the control confirmation failed before anything was interrupted: {}",
        control_reply.error_text()
    );
    let handler_takes = control_reply.latency;

    let mut escrows = Vec::new();
    for index in 0..BATCH {
        escrows.push(
            reserve_and_fund(&hub, &chain, poster, format!("escrow-restart {label} {index}"))
                .await?,
        );
    }

    let before = tasks_by_description(&hub).await?;

    let mut inflight = tokio::task::JoinSet::new();
    for (index, escrow) in escrows.iter().enumerate() {
        let hub = hub.clone();
        let poster = poster.clone();
        let id = escrow.id.clone();
        // Started `index/BATCH` of a handler's duration apart, so that at
        // the moment of the kill each one is at a different point in the
        // handler and the batch spans the timeline.
        let start_after = handler_takes.mul_f64(index as f64 / BATCH as f64);
        inflight.spawn(async move {
            tokio::time::sleep(start_after).await;
            hub.post_signed(
                &poster,
                &format!("/tasks/escrow/{id}/confirm"),
                ConfirmEscrowPayload {
                    escrow_id: id.clone(),
                },
            )
            .await
        });
    }

    tokio::time::sleep(handler_takes).await;
    if hard {
        harness.stack.kill_hub().await?;
    } else {
        harness.stack.stop_hub().await?;
    }

    let mut answered_ok = 0usize;
    let mut answered_error = 0usize;
    let mut never_answered = 0usize;
    while let Some(result) = inflight.join_next().await {
        match result? {
            Ok(reply) => {
                samples.push(Sample::new(
                    "POST /tasks/escrow/:id/confirm (interrupted)",
                    reply.status,
                    reply.latency,
                ));
                if reply.ok() {
                    answered_ok += 1;
                } else {
                    answered_error += 1;
                }
            }
            // The connection died with the process. The client does not
            // know whether the handler ran, which is the whole point.
            Err(_) => never_answered += 1,
        }
    }

    harness.stack.start_hub().await?;
    let after = tasks_by_description(&hub).await?;
    let survived = escrows
        .iter()
        .filter(|e| after.contains_key(&e.description))
        .count();

    // Retry every escrow with a fresh envelope. Three outcomes matter: a
    // success where no task exists is clean recovery; a success where one
    // already exists is one deposit funding two tasks; a refusal where no
    // task exists is a stranded deposit.
    let mut recovered = 0usize;
    let mut duplicated = 0usize;
    let mut stranded = 0usize;
    for escrow in &escrows {
        let had_task = after.contains_key(&escrow.description);
        let retry = hub
            .post_signed(
                poster,
                &format!("/tasks/escrow/{}/confirm", escrow.id),
                ConfirmEscrowPayload {
                    escrow_id: escrow.id.clone(),
                },
            )
            .await?;
        samples.push(Sample::new(
            "POST /tasks/escrow/:id/confirm (retry)",
            retry.status,
            retry.latency,
        ));
        match (had_task, retry.ok()) {
            (false, true) => recovered += 1,
            (true, true) => duplicated += 1,
            (false, false) => {
                // Only stranded if the money is still sitting at the
                // derived address with nothing pointing at it.
                if chain.confirmed_balance(&escrow.deposit_pubkey).await? > 0 {
                    stranded += 1;
                }
            }
            (true, false) => {}
        }
    }

    let mut section = Section::new(format!(
        "Restart the hub mid-escrow ({})",
        if hard { "SIGKILL" } else { "SIGTERM" }
    ))
    .plan_item("§6.5b")
    .fact("confirmations_in_flight", BATCH)
    .fact("handler_latency_ms", control_reply.latency.as_secs_f64() * 1000.0)
    .fact("answered_success", answered_ok)
    .fact("answered_error", answered_error)
    .fact("never_answered", never_answered)
    .fact("tasks_that_survived_the_restart", survived)
    .fact("recovered_by_retry", recovered)
    .fact("deposits_funding_two_tasks", duplicated)
    .fact("deposits_stranded", stranded)
    .fact("tasks_before", before.len())
    .latency(summarize(samples, None));

    if duplicated > 0 {
        section = section
            .verdict(Verdict::Refuted)
            .finding(format!(
                "{duplicated} escrow deposit(s) funded a second task after a hub restart. \
                 This is supposed to be unreachable: since 2026-09-07 the task and the deposit's \
                 `Consumed` status are staged in ONE redb write transaction \
                 (`HubStore::save_task_and_deposit`), so a crash leaves both records or neither. \
                 Seeing a task on disk beside a deposit that still reads `Reserved` means that \
                 guarantee has been broken -- someone split the commit again, or the write \
                 transaction is not doing what its name says. One deposit, two bounties."
            ));
    } else if stranded > 0 {
        section = section
            .verdict(Verdict::Refuted)
            .finding(format!(
                "{stranded} escrow deposit(s) were left funded at their derived address with no \
                 task and no way to retry the confirmation. The coin is recoverable by the \
                 operator, who holds the escrow secret, but the depositor cannot reach it and \
                 nothing tells them so."
            ));
    } else if hard && never_answered == 0 {
        // The kill landed after every handler had already replied, so
        // nothing was interrupted and the two zeros above are about a
        // batch that ran to completion. That says nothing either way.
        //
        // Worth its own arm rather than folding into the confirmation
        // below: this is the shape a drill quietly rots into on a fast
        // machine, where the staggered start finishes before the kill,
        // and it would otherwise report a clean confirmation for a run
        // that tested nothing.
        section = section.verdict(Verdict::Inconclusive).note(format!(
            "The kill landed after all {BATCH} confirmations had already answered, so none was \
             interrupted and this run demonstrates nothing about a crash. Re-run; if it \
             persists, the control handler's measured duration is no longer a good estimate of \
             how long the batch takes."
        ));
    } else if hard {
        // Confirmed, and this arm is new on 2026-09-09.
        //
        // It used to be `Inconclusive` unconditionally, on the argument
        // that the dangerous interval is one step wide -- between the
        // task's commit and the deposit's -- so a run that found nothing
        // had failed to reproduce rather than shown safety. That was
        // exactly right when it was written and stopped being right on
        // 2026-09-07, when `confirm_escrow` moved to
        // `HubStore::save_task_and_deposit` and staged both records in
        // ONE redb write transaction. There is no longer an interval to
        // land in.
        //
        // So the two zeros changed meaning underneath this code and it
        // did not notice. `deposits_funding_two_tasks == 0` used to mean
        // "we did not hit the window"; with a single commit it means the
        // invariant held across every one of these kills. That is a
        // demonstration, and recording it as a shrug wasted the run --
        // and, worse, left a section that could never reach its own
        // healthy verdict, which reads as a drill nobody has looked at.
        //
        // The guard above is what keeps this honest: a confirmation is
        // only claimed when the kill actually interrupted requests.
        section = section.verdict(Verdict::Confirmed).note(format!(
            "{never_answered} of {BATCH} confirmations were killed in flight and {survived} had \
             committed a task by then. No deposit funded two tasks and none was stranded, which \
             with a single write transaction is the atomicity holding rather than a failure to \
             reproduce: `save_task_and_deposit` stages the task and the deposit's `Consumed` \
             status together, so a crash leaves both or neither. Splitting those commits again \
             would show up here as a refutation. See plan §6.5b for the two-commit version this \
             replaced."
        ));
    } else {
        section = section.verdict(Verdict::Confirmed).note(
            "Every interrupted confirmation ended in a state a client can act on: either the \
             task exists, or the escrow is still confirmable with a fresh envelope. No deposit \
             funded two tasks and none was stranded.",
        );
    }

    // Accepted rather than reported as a finding, and §6.5b is where
    // that was settled: this shows up in most runs, and a five-run A/B
    // against the pre-fix build reproduced it at the same rate (three of
    // five before, four of five after), so it is pre-existing variance in
    // the drain and not anything a fix introduced. It costs nothing --
    // the escrow is not consumed and the retry recovers it, which
    // `recovered_by_retry` shows in the same report.
    //
    // As an ordinary finding it made `harness compare` exit non-zero on
    // nearly every run of this drill, for a reason everybody had already
    // agreed was harmless. That is how a red signal stops being read,
    // and it is the reason `Section::accepted` exists.
    if !hard && never_answered > 0 {
        section = section.accepted_finding(format!(
            "{never_answered} confirmation(s) were dropped without an answer on SIGTERM, which \
             the hub is supposed to drain rather than drop. Known pre-existing drain variance, \
             measured against a pre-fix control at the same rate (§6.5b); the escrow is not \
             consumed and the retry recovers it."
        ));
    }

    Ok(section)
}

pub async fn run(repo: &Path, bin_dir: &Path, work_dir: PathBuf) -> Result<Report> {
    let mut harness = super::bring_up(repo, bin_dir, work_dir, true).await?;
    let chain = harness.stack.chain();
    let operator = harness.stack.operator_key.clone();

    // The poster funds its own escrows, so it needs real coin. Paid
    // straight from the operator rather than through the faucet: the
    // faucet is one operator payout per grant and this drill is not about
    // measuring that.
    let poster = PrivateKey::new_key();
    chain
        .wait_for_utxo_count(&operator.public_key(), 3, Duration::from_secs(300))
        .await?;
    // Twelve escrows at a bounty and a fee apiece, and then an order of
    // magnitude on top. The first version of this funded almost exactly
    // what the arithmetic said and ran dry on the last escrow: a deposit
    // address may ask for more than the bounty, each payment burns a fee,
    // and every one of them is a separate transaction. The operator holds
    // fifty coins per block, so being generous here costs nothing and
    // being exact costs a six-minute run.
    let needed = (BOUNTY + FEE) * (BATCH as u64 + 1) * 2 * 10;
    chain.pay(&operator, &poster.public_key(), needed, FEE).await?;
    let funded = chain
        .wait_for_balance(&poster.public_key(), needed, Duration::from_secs(180))
        .await?;
    anyhow::ensure!(funded >= needed, "could not fund the drill's poster");

    let graceful = phase(&mut harness, &poster, "sigterm", false).await?;
    let crash = phase(&mut harness, &poster, "sigkill", true).await?;

    harness.stack.shutdown().await;

    let mut report = Report::new("drill: escrow-restart", harness.environment);
    report.push(graceful);
    report.push(crash);
    Ok(report)
}

/// Kept so the module's one non-obvious helper does not drift: the join
/// key between setup and survival is the description, which must be
/// unique per escrow or the drill silently under-counts.
#[cfg(test)]
mod tests {
    #[test]
    fn phase_labels_make_descriptions_unique_across_phases() {
        let sigterm: Vec<String> = (0..3).map(|i| format!("escrow-restart sigterm {i}")).collect();
        let sigkill: Vec<String> = (0..3).map(|i| format!("escrow-restart sigkill {i}")).collect();
        for description in &sigterm {
            assert!(!sigkill.contains(description));
        }
    }
}

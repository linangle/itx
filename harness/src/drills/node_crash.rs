//! Kill the node with payouts in flight, and count the money that
//! disappears.
//!
//! This is the drill the whole handoff is built around, because it is the
//! only one that measures a loss rather than a latency. Plan §6.5 states
//! the mechanism: the node's mempool is memory-only, so stopping the node
//! discards every transaction submitted since the last block, while the
//! hub -- which recorded each payout as complete on the strength of a
//! one-way write it never got an answer to -- goes on reporting them as
//! made. Nothing revisits a task once it leaves `Verified`, so the loss is
//! permanent and silent.
//!
//! # Why the miner is stopped first
//!
//! The exposed window in production is "everything submitted since the
//! last block": at a 16-second target that averages eight seconds, and
//! racing it from a test would make this drill a coin flip. Stopping the
//! miner before the payouts holds the window open instead, so the drill
//! measures the size of the loss rather than whether it happened to catch
//! one. It does not manufacture the loss -- every transaction it destroys
//! is one a real crash eight seconds after a payout would have destroyed
//! too.
//!
//! # Re-running this after the settlement work lands
//!
//! This is the drill whose baseline matters most. Plan §6.5's confirmation
//! design is meant to make the difference between `hub_reported_paid` and
//! `landed_on_chain` recoverable: the sweep should see the recipient's
//! output absent and the spent inputs unmarked, and resubmit. Re-run this
//! drill afterwards against the same numbers. `harness/baselines/` holds
//! the before.

use crate::client::{ClaimPayload, CreateTaskPayload, SubmitPayload};
use crate::report::{Report, Section, Verdict};
use crate::stats::{summarize, Sample};
use anyhow::Result;
use btclib::crypto::PrivateKey;
use btclib::sha256::Hash;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How many payouts to put in flight. Each needs its own confirmed
/// operator output to spend, so this is also how many blocks the drill
/// waits for before it starts.
const PAYOUTS: usize = 6;

const BOUNTY: u64 = 1_000_000;
const ANSWER: &str = "the answer is 42";

/// Long enough for the hub to finish recovering, which is four intervals
/// rather than the one this drill originally needed.
///
/// It was 75 seconds -- one sweep plus margin -- which was right when a
/// payout was never revisited and the only question was whether the sweep
/// ignored `Paid` tasks. Payout confirmation (plan §6.5) made recovery a
/// sequence rather than an event, and 75 seconds lands in the middle of it:
/// the drill reported two of six million lost against a hub that went on to
/// recover all six, which is a false finding in the direction that matters
/// most.
///
/// The sequence it has to outlast, from `hub`: a payout is left alone for
/// `PAYOUT_RESOLUTION_GRACE_SECONDS` (30) so a fresh send is not read as a
/// loss, the sweep runs every 60, resolving it schedules a resend, and the
/// resend needs its own grace and its own sweep before it can be confirmed.
/// That is 30 + 60 twice, so 180 at best and more when a tick is missed.
/// 260 covers it with margin and was measured: at 75 the verdict is
/// CONFIRMED with 2,000,000 lost, at 260 it is REFUTED with none.
///
/// If the sweep cadence or the grace changes, this has to change with it.
const SWEEP_WAIT: Duration = Duration::from_secs(260);

pub async fn run(repo: &Path, bin_dir: &Path, work_dir: PathBuf) -> Result<Report> {
    // Distinct source addresses: this drill is about settlement, and
    // several agents claiming and submitting from one box would otherwise
    // be stopped by the per-address chain-tier budget of twenty a minute
    // before they got anywhere near the thing being measured.
    let mut harness = super::bring_up(repo, bin_dir, work_dir, true).await?;
    let chain = harness.stack.chain();
    let operator = harness.stack.operator_key.clone();
    let hub = harness.stack.hub_client()?;

    let mut samples: Vec<Sample> = Vec::new();

    // Each payout must find its own unmarked confirmed output to spend, so
    // wait for that many coinbase outputs rather than for a balance.
    let outputs = chain
        .wait_for_utxo_count(
            &operator.public_key(),
            PAYOUTS + 2,
            Duration::from_secs(300),
        )
        .await?;
    anyhow::ensure!(
        outputs >= PAYOUTS + 2,
        "the miner produced only {outputs} confirmed operator outputs; this drill needs {}",
        PAYOUTS + 2
    );

    let expected_output_hash = hex::encode(Hash::hash_bytes(ANSWER.as_bytes()).as_bytes());
    let mut work: Vec<(PrivateKey, String)> = Vec::new();
    for index in 0..PAYOUTS {
        let client = hub.from_source(&format!("10.9.0.{}", index + 1));
        let reply = client
            .post_signed(
                &operator,
                "/tasks",
                CreateTaskPayload {
                    description: format!("node-crash drill task {index}"),
                    bounty: BOUNTY,
                    expected_output_hash: expected_output_hash.clone(),
                    min_reputation: 0,
                    capabilities: Vec::new(),
                },
            )
            .await?;
        samples.push(Sample::new("POST /tasks", reply.status, reply.latency));
        anyhow::ensure!(
            reply.ok(),
            "could not post the drill's tasks: {}",
            reply.error_text()
        );
        let task_id = reply.body["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("task creation returned no id"))?
            .to_string();

        let agent = PrivateKey::new_key();
        let claim = client
            .post_signed(
                &agent,
                &format!("/tasks/{task_id}/claim"),
                ClaimPayload {
                    task_id: task_id.clone(),
                },
            )
            .await?;
        samples.push(Sample::new(
            "POST /tasks/:id/claim",
            claim.status,
            claim.latency,
        ));
        anyhow::ensure!(claim.ok(), "claim failed: {}", claim.error_text());
        work.push((agent, task_id));
    }

    // Hold the window open. From here until the node is killed, no block
    // can be mined, so every payout stays in the mempool where a crash
    // will destroy it.
    harness.stack.kill_miner().await?;
    let height_before = chain.height().await?;

    let mut reported_paid = 0usize;
    let mut paid_agents: Vec<(PrivateKey, String)> = Vec::new();
    for (index, (agent, task_id)) in work.into_iter().enumerate() {
        let client = hub.from_source(&format!("10.9.0.{}", index + 1));
        let reply = client
            .post_signed(
                &agent,
                &format!("/tasks/{task_id}/submit"),
                SubmitPayload {
                    task_id: task_id.clone(),
                    output: ANSWER.to_string(),
                },
            )
            .await?;
        samples.push(Sample::new(
            "POST /tasks/:id/submit",
            reply.status,
            reply.latency,
        ));
        if reply.body.get("paid").and_then(Value::as_bool) == Some(true) {
            reported_paid += 1;
            paid_agents.push((agent, task_id));
        }
    }

    // The crash. Node and miner together -- a miner whose template source
    // has gone is not mining, and leaving it running only makes the
    // restart harder to read.
    harness.stack.kill_node_and_miner().await?;
    harness.stack.start_node().await?;
    harness.stack.start_miner().await?;

    // Let the restarted chain move on. Anything that survived the crash
    // would be mined here; anything that did not, never will be.
    chain.wait_for_blocks(3, Duration::from_secs(300)).await?;

    let mut landed = 0u64;
    let mut agents_paid_on_chain = 0usize;
    for (agent, _) in &paid_agents {
        let balance = chain.confirmed_balance(&agent.public_key()).await?;
        landed += balance;
        if balance >= BOUNTY {
            agents_paid_on_chain += 1;
        }
    }

    let hub_reported = reported_paid as u64 * BOUNTY;
    let lost_immediately = hub_reported.saturating_sub(landed);

    // Does anything ever come back for it?
    //
    // The accounting here changed with payout confirmation (plan §6.5), and
    // the distinction it draws is the whole point of that work. Before it,
    // `Paid` was terminal and money missing from the chain was money gone
    // with nothing left saying so -- so "reported minus landed" was exactly
    // the silent loss. Now a payout the chain has not taken can sit in
    // `Submitted`, which the sweep keeps working and the API reports as a
    // pending bounty, or reach `PayoutFailed` once its budget is spent.
    // Neither is silent, and counting them as losses reports a bug that is
    // not there.
    //
    // So the three are counted apart, and only the first is the failure this
    // drill exists to catch: money the hub still calls `Paid` that the chain
    // does not have.
    tokio::time::sleep(SWEEP_WAIT).await;
    let mut landed_after_sweep = 0u64;
    let mut still_marked_paid = 0usize;
    let mut lost = 0u64;
    let mut pending = 0u64;
    let mut failed_visibly = 0u64;
    for (agent, task_id) in &paid_agents {
        let on_chain = chain.confirmed_balance(&agent.public_key()).await?;
        landed_after_sweep += on_chain;
        let task = hub.get(&format!("/tasks/{task_id}")).await?;
        let status = task.body.get("status").and_then(Value::as_str).unwrap_or("?");
        if on_chain >= BOUNTY {
            continue;
        }
        match status {
            "Paid" => {
                still_marked_paid += 1;
                lost += BOUNTY;
            }
            "PayoutFailed" => failed_visibly += BOUNTY,
            // `Submitted`, or anything earlier the sweep still owns.
            _ => pending += BOUNTY,
        }
    }

    harness.stack.shutdown().await;

    let mut section = Section::new("Kill the node mid-payout")
        .plan_item("§6.5")
        .fact("payouts_attempted", PAYOUTS)
        .fact("hub_reported_paid", reported_paid)
        .fact("itx_hub_reported_paid", hub_reported)
        .fact("itx_landed_on_chain", landed)
        .fact("itx_lost_before_sweep", lost_immediately)
        .fact("itx_landed_after_one_sweep", landed_after_sweep)
        .fact("itx_lost", lost)
        .fact("itx_pending_visibly", pending)
        .fact("itx_failed_visibly", failed_visibly)
        .fact("agents_actually_paid", agents_paid_on_chain)
        .fact("tasks_still_marked_paid", still_marked_paid)
        .fact("chain_height_at_crash", height_before)
        .latency(summarize(&samples, None))
        .note(
            "The miner was stopped before the submissions, which holds the pre-block window \
             open rather than racing it. In production that window is everything submitted \
             since the last block -- about eight seconds on average at a sixteen-second \
             target.",
        );

    if lost > 0 {
        section = section
            .verdict(Verdict::Confirmed)
            .note(format!(
                "The hub reported {reported_paid} of {PAYOUTS} payouts as complete and returned \
                 `paid: true` to each agent. {landed_after_sweep} of {hub_reported} ITX exists \
                 on the chain. {still_marked_paid} of those tasks still read `Paid` over the \
                 API after a full sweep interval, so nothing in the hub will ever revisit them."
            ))
            .finding(format!(
                "Killing the node with payouts in the mempool destroyed {lost} ITX across \
                 {still_marked_paid} payouts, with no trace left in the hub: the tasks still \
                 read `Paid`, the agents were never credited on-chain, and nothing will \
                 revisit them. This is §6.5 exactly, measured rather than predicted."
            ));
    } else if reported_paid == 0 {
        section = section.verdict(Verdict::Inconclusive).note(
            "No payout was reported as made, so there was nothing in flight to lose. Usually \
             this means the operator ran out of confirmed outputs -- see the payout-ceiling \
             drill, which is about exactly that.",
        );
    } else if pending > 0 || failed_visibly > 0 {
        section = section.verdict(Verdict::Refuted).note(format!(
            "Nothing was lost silently. {landed_after_sweep} of {hub_reported} ITX is on the \
             chain; {pending} is still `Submitted`, which the sweep owns and the API reports \
             as a pending bounty, and {failed_visibly} reached `PayoutFailed`. The failure \
             this drill was written for is money the hub still calls `Paid` that the chain \
             does not have, and there is none of it. Recovery is a multi-sweep sequence, so \
             a non-zero pending figure here means the wait ended mid-sequence rather than \
             that anything is wrong."
        ));
    } else {
        section = section.verdict(Verdict::Refuted).note(
            "Every payout the hub reported was on the chain after the crash. Either the \
             settlement confirmation work has landed, or the transactions were mined before \
             the node was killed.",
        );
    }

    let mut report = Report::new("drill: node-crash", harness.environment);
    report.push(section);
    Ok(report)
}

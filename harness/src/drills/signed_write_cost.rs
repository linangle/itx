//! What a signed write actually costs, split into its parts.
//!
//! §6 orders what breaks first, and puts **signature-verify CPU** third,
//! ahead of the single-instance ceiling and settlement honesty. That is a
//! claim about where the write path's time goes, and it is measurable
//! without a profiler, because the hub's own authentication ordering
//! hands us two requests that differ by exactly one step.
//!
//! `verify_charging` runs: cheap field checks, then the ECDSA verify,
//! then the quota charge, then the claim -- and the claim is where
//! `HubStore::record_seen_signature` fsyncs, deliberately before the
//! handler runs so that a crash cannot leave a replayable envelope that
//! has already moved money. A replay is detected *at* the claim, and a
//! detected replay is not written. So:
//!
//! - **A replayed envelope** pays the drift check, the ECDSA verify, the
//!   quota charge and an in-memory lookup, and returns 401.
//! - **A fresh envelope on `POST /tasks` signed by a non-operator** pays
//!   all of that *plus* the durable claim, and then returns 403 from the
//!   handler before touching the board or writing anything else.
//!
//! The difference between the two is the fsync, with nothing else in it.
//! A route that succeeded would fold in its own durable writes and would
//! not separate them.
//!
//! An unauthenticated read is measured alongside as the floor.

use crate::client::CreateTaskPayload;
use crate::report::{Report, Section, Verdict};
use crate::stats::{summarize, Sample, Summary};
use anyhow::Result;
use btclib::crypto::PrivateKey;
use btclib::sha256::Hash;
use sdk::build_envelope;
use std::path::{Path, PathBuf};

/// Rounds. Each is three requests, and the numbers are stable well before
/// this -- the point is a percentile, not a long average.
const ROUNDS: usize = 40;

fn p50(summaries: &[Summary], label: &str) -> f64 {
    summaries
        .iter()
        .find(|s| s.label == label)
        .map(|s| s.p50_ms)
        .unwrap_or(0.0)
}

pub async fn run(repo: &Path, bin_dir: &Path, work_dir: PathBuf) -> Result<Report> {
    let mut harness = super::bring_up(repo, bin_dir, work_dir, true).await?;
    let hub = harness.stack.hub_client()?;
    let mut samples = Vec::new();

    let expected_output_hash = hex::encode(Hash::hash_bytes(b"unused").as_bytes());

    for round in 0..ROUNDS {
        // Its own address per round: three requests apiece would
        // otherwise walk into the chain-tier budget of twenty a minute
        // and start measuring the rate limiter.
        let client = hub.from_source(&format!("10.3.{}.{}", round / 250, round % 250));
        let key = PrivateKey::new_key();

        let read = client.get("/tasks?limit=1").await?;
        samples.push(Sample::new("read (no signature)", read.status, read.latency));

        let envelope = build_envelope(
            &key,
            "POST",
            "/tasks",
            CreateTaskPayload {
                description: format!("signed-write-cost {round}"),
                bounty: 1,
                expected_output_hash: expected_output_hash.clone(),
                min_reputation: 0,
                capabilities: Vec::new(),
            },
        );

        // Verified, charged, claimed -- then refused by the handler for
        // not being the operator. Nothing else is written.
        let claimed = client.post_envelope("/tasks", &envelope).await?;
        samples.push(Sample::new(
            "signed, claimed, refused (403)",
            claimed.status,
            claimed.latency,
        ));
        anyhow::ensure!(
            claimed.status == 403,
            "expected a non-operator task post to be refused with 403, got {} ({})",
            claimed.status,
            claimed.error_text()
        );

        // The same envelope again: verified and charged identically, then
        // caught by the in-memory replay set before the fsync.
        let replayed = client.post_envelope("/tasks", &envelope).await?;
        samples.push(Sample::new(
            "signed, replayed, refused (401)",
            replayed.status,
            replayed.latency,
        ));
        anyhow::ensure!(
            replayed.status == 401,
            "expected the replay to be refused with 401, got {}",
            replayed.status
        );
    }

    harness.stack.shutdown().await;

    let summaries = summarize(&samples, None);
    let read_ms = p50(&summaries, "read (no signature)");
    let verify_ms = p50(&summaries, "signed, replayed, refused (401)");
    let claimed_ms = p50(&summaries, "signed, claimed, refused (403)");
    let fsync_ms = (claimed_ms - verify_ms).max(0.0);
    let round = |value: f64| (value * 100.0).round() / 100.0;

    // The claim under test is §6's ordering, which puts verify CPU third.
    // If the durable claim costs several times what verification costs,
    // that ordering is wrong about the write path.
    let verify_dominates = verify_ms >= fsync_ms;

    // A measurement drill, not a safety one: it refuted §6's claim about
    // where a signed write's time goes, the plan was corrected on the
    // strength of it (§6.3), and refuted is therefore the standing,
    // correct state rather than a problem.
    //
    // Worth knowing what a flip would mean. If somebody makes verify
    // dominate -- by batching or removing the replay guard's fsync, which
    // is exactly what the finding below recommends -- this reports
    // `Confirmed` and the run goes red. That is not a false alarm: it
    // means the premise §6.3's correction rests on has changed and the
    // section needs re-reading. Whoever lands that re-baselines this.
    let mut section = Section::new("Where a signed write's time actually goes")
        .healthy_when(Verdict::Refuted)
        .plan_item("§6.3")
        .fact("rounds", ROUNDS)
        .fact("unauthenticated_read_p50_ms", round(read_ms))
        .fact("verify_and_quota_p50_ms", round(verify_ms))
        .fact("verify_plus_durable_claim_p50_ms", round(claimed_ms))
        .fact("durable_claim_p50_ms", round(fsync_ms))
        .fact(
            "claim_to_verify_ratio",
            round(if verify_ms > 0.0 { fsync_ms / verify_ms } else { 0.0 }),
        )
        .latency(summaries)
        .note(
            "The two signed measurements are the same envelope on the same route from the same \
             key. They differ by exactly one step: the first is claimed and fsynced before the \
             handler refuses it, the second is caught by the in-memory replay set at the claim \
             and never written.",
        );

    section = if verify_dominates {
        section.verdict(Verdict::Confirmed).note(format!(
            "Verification costs {verify_ms:.2}ms against {fsync_ms:.2}ms for the durable claim, \
             so §6's ordering is right to name verify CPU as the write path's expensive half."
        ))
    } else {
        // Accepted, not a finding: the plan has already been corrected
        // to say this (§6.3), so a run restating it is agreeing with the
        // documentation rather than reporting news. As an ordinary
        // finding it failed this drill's own exit code on every healthy
        // run.
        section.verdict(Verdict::Refuted).accepted_finding(format!(
            "§6 item 3 names signature-verify CPU as the third thing to break, but verification \
             and the quota charge together cost {verify_ms:.2}ms while the durable replay claim \
             behind them costs {fsync_ms:.2}ms -- {:.0}x more. Signed writes are bounded by one \
             fsync per request, not by ECDSA. That also means the write path does not scale with \
             cores, and that batching or group-committing the replay guard's writes is worth \
             more than anything done to the verify.",
            if verify_ms > 0.0 { fsync_ms / verify_ms } else { 0.0 }
        ))
    };

    let mut report = Report::new("drill: signed-write-cost", harness.environment);
    report.push(section);
    Ok(report)
}

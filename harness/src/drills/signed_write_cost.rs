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
        samples.push(Sample::new(
            "read (no signature)",
            read.status,
            read.latency,
        ));

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
    let (claim_dominates, verify_dominates) = dominance(fsync_ms, verify_ms);

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
            round(if verify_ms > 0.0 {
                fsync_ms / verify_ms
            } else {
                0.0
            }),
        )
        .latency(summaries)
        .note(
            "The two signed measurements are the same envelope on the same route from the same \
             key. They differ by exactly one step: the first is claimed and fsynced before the \
             handler refuses it, the second is caught by the in-memory replay set at the claim \
             and never written.",
        );

    section =
        if verify_dominates {
            section.verdict(Verdict::Confirmed).note(format!(
            "Verification costs {verify_ms:.2}ms against {fsync_ms:.2}ms for the durable claim, \
             so §6's ordering is right to name verify CPU as the write path's expensive half."
        ))
        } else if !claim_dominates {
            // Neither side clears the band. Saying so is the whole point:
            // this drill exists to say where a signed write's time goes, and
            // on hardware whose fsync is nearly free it does not go decisively
            // to either half.
            section.verdict(Verdict::Inconclusive).note(format!(
            "Verification and the quota charge cost {verify_ms:.2}ms against {fsync_ms:.2}ms for \
             the durable replay claim -- within {DOMINANCE_FACTOR:.0}x of each other, so neither \
             dominates and this run supports no claim about the ordering. Storage whose fsync is \
             nearly free (a CI runner, a cached virtual disk) reads like this, and the ordering \
             flips between runs on the same machine. Read this drill on hardware with the \
             durability characteristics you intend to deploy on; §6.3's conclusion was drawn \
             where the claim costs about 23x the verify."
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

/// How much bigger one cost has to be before this drill will say it
/// *dominates* the other, rather than reporting which side of a coin
/// toss it landed on.
///
/// Three rather than two: the ratio must clear the band on both sides,
/// an observed 0.46 sits just outside a factor of two, and a threshold
/// the noise can still cross has not fixed anything. Where the claim
/// genuinely dominates it does so by about 23x, so nothing real is lost
/// by refusing to call 2x a verdict.
const DOMINANCE_FACTOR: f64 = 3.0;

/// `(claim_dominates, verify_dominates)` for one run's two measurements.
///
/// The comparison used to be a bare `verify_ms >= fsync_ms`, which is a
/// sound question only when the two are far apart. On a developer's
/// machine they are: the durable claim costs about 23x the verify,
/// because an fsync to real storage is expensive and ECDSA is not.
///
/// On the two-core CI runner they are not. Its filesystem makes the
/// fsync nearly free, and three runs on the same hardware measured
/// ratios of 1.27, 0.68 and 0.46 -- the ordering reversing run to run
/// while the drill reported `refuted`, `confirmed` and `confirmed` with
/// equal confidence. That is not a finding about the hub; it is a drill
/// treating noise as a result, and it failed the nightly gate on
/// whichever runs came up the wrong way.
fn dominance(fsync_ms: f64, verify_ms: f64) -> (bool, bool) {
    (
        fsync_ms >= verify_ms * DOMINANCE_FACTOR,
        verify_ms >= fsync_ms * DOMINANCE_FACTOR,
    )
}

#[cfg(test)]
mod tests {
    use super::dominance;

    /// The three ratios actually measured on the CI runner, which the
    /// old bare comparison scored as refuted, confirmed and confirmed.
    /// None of them is a statement about ordering.
    #[test]
    fn ci_runner_noise_is_not_a_verdict() {
        for (fsync, verify) in [(0.34, 0.27), (0.21, 0.30), (0.15, 0.33)] {
            let (claim, verify_wins) = dominance(fsync, verify);
            assert!(
                !claim && !verify_wins,
                "fsync={fsync} verify={verify} must clear neither side of the band"
            );
        }
    }

    /// The developer-machine measurement §6.3 was drawn from.
    #[test]
    fn a_real_separation_still_decides() {
        let (claim, verify_wins) = dominance(4.38, 0.19);
        assert!(
            claim && !verify_wins,
            "23x is the claim dominating, and must still say so"
        );

        // ...and the other way round, so this is a band and not a floor.
        let (claim, verify_wins) = dominance(0.10, 1.50);
        assert!(!claim && verify_wins);
    }

    /// The edges, so the threshold is a decision rather than an accident.
    #[test]
    fn the_band_is_exactly_three() {
        assert_eq!(dominance(3.0, 1.0), (true, false));
        assert_eq!(dominance(2.99, 1.0), (false, false));
        assert_eq!(dominance(1.0, 3.0), (false, true));
        assert_eq!(dominance(1.0, 2.99), (false, false));
    }
}

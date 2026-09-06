//! Replay every envelope the hub has ever accepted, immediately after a
//! crash, and confirm that none of them works twice.
//!
//! The guard is durable on purpose (plan §3.3). Its in-memory set starts
//! empty on every boot, so without the disk half there is a window after
//! each restart -- an ordinary deploy, no crash needed -- in which every
//! envelope accepted just before shutdown is still inside its drift
//! window and verifies a second time. `restore` refills the set from redb
//! so the window never opens.
//!
//! This drill is the adversarial version of that claim. It captures real
//! accepted envelopes, hard-kills the hub so nothing gets a chance to
//! flush on the way out, and replays the lot the instant `/health`
//! answers again. The number that matters is zero.
//!
//! A control storm runs first, against the same process that accepted the
//! originals. Without it, a drill that found zero replays after a restart
//! could not tell "the durable guard works" from "something else refused
//! these requests for an unrelated reason".

use crate::client::HubClient;
use crate::report::{Report, Section, Verdict};
use crate::stats::{summarize, Sample};
use anyhow::Result;
use btclib::crypto::PrivateKey;
use sdk::build_envelope;
use sdk::SignedEnvelope;
use std::path::{Path, PathBuf};

/// How many envelopes to capture and replay. Each comes from its own key
/// and its own source address, so neither the per-address bucket nor the
/// per-key quota can refuse a replay for the wrong reason -- which would
/// look like the guard working when it was not.
const ENVELOPES: usize = 30;

/// The route the storm uses. Payload-less, signed, and it changes state:
/// a replay that got through would leave a second reservation behind, so
/// success is not merely a status code.
const PATH: &str = "/exchange/deposit";

async fn storm(
    hub: &HubClient,
    envelopes: &[(usize, SignedEnvelope<()>)],
    label: &'static str,
    samples: &mut Vec<Sample>,
) -> Result<usize> {
    let mut accepted = 0;
    for (index, envelope) in envelopes {
        let client = hub.from_source(&format!("10.7.0.{}", index + 1));
        let reply = client.post_envelope(PATH, envelope).await?;
        samples.push(Sample::new(label, reply.status, reply.latency));
        if reply.ok() {
            accepted += 1;
        }
    }
    Ok(accepted)
}

pub async fn run(repo: &Path, bin_dir: &Path, work_dir: PathBuf) -> Result<Report> {
    let mut harness = super::bring_up(repo, bin_dir, work_dir, true).await?;
    let hub = harness.stack.hub_client()?;
    let mut samples = Vec::new();

    // Capture: sign an envelope per key and spend it once, legitimately.
    let mut envelopes = Vec::new();
    let mut first_use_accepted = 0;
    for index in 0..ENVELOPES {
        let key = PrivateKey::new_key();
        let envelope = build_envelope(&key, "POST", PATH, ());
        let client = hub.from_source(&format!("10.7.0.{}", index + 1));
        let reply = client.post_envelope(PATH, &envelope).await?;
        samples.push(Sample::new("first use", reply.status, reply.latency));
        if reply.ok() {
            first_use_accepted += 1;
        }
        envelopes.push((index, envelope));
    }
    anyhow::ensure!(
        first_use_accepted == ENVELOPES,
        "only {first_use_accepted} of {ENVELOPES} envelopes were accepted on first use; there is \
         nothing to replay"
    );

    let control = storm(&hub, &envelopes, "replay (same process)", &mut samples).await?;

    // The crash. SIGKILL rather than SIGTERM: a graceful shutdown would
    // let the hub finish whatever it was doing, and the claim under test
    // is that the guard needs no cooperation on the way out.
    harness.stack.kill_hub().await?;
    harness.stack.start_hub().await?;

    let after_restart = storm(&hub, &envelopes, "replay (after restart)", &mut samples).await?;

    harness.stack.shutdown().await;

    let mut section = Section::new("Replay storm immediately after a restart")
        .plan_item("§3.3")
        .fact("envelopes", ENVELOPES)
        .fact("accepted_on_first_use", first_use_accepted)
        .fact("accepted_on_replay_same_process", control)
        .fact("accepted_on_replay_after_restart", after_restart)
        .latency(summarize(&samples, None))
        .note(
            "Every envelope came from its own key and its own source address, so a refusal \
             cannot be the rate limiter or the per-key quota standing in for the replay guard.",
        );

    section = if control == 0 && after_restart == 0 {
        section.verdict(Verdict::Confirmed).note(
            "Nothing got through, before or after the hub was killed. The guard's durable half \
             is doing the work it was added for: the in-memory set was empty on boot and every \
             one of these signatures came back off disk.",
        )
    } else {
        section.verdict(Verdict::Refuted).finding(format!(
            "{after_restart} of {ENVELOPES} replayed envelopes were accepted after a hub \
             restart ({control} were accepted even without one). Each is a signed request \
             executed twice."
        ))
    };

    let mut report = Report::new("drill: replay-storm", harness.environment);
    report.push(section);
    Ok(report)
}

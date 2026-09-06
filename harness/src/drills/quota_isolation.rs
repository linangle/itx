//! Exhaust one key's quota from many addresses, and confirm other keys
//! are unaffected.
//!
//! The per-address buckets cover the axis a per-key budget cannot, and the
//! per-key quota covers the axis a per-address bucket cannot. Keygen is
//! free, so a determined client spreads itself across addresses until
//! every bucket it touches looks idle (§4); charging the identity as well
//! is what stops that. Sixty signed requests a minute, across every route
//! and from wherever they connect.
//!
//! The failure this drill is looking for is the quota being enforced too
//! *broadly* -- some shared counter, or a lock held across the check --
//! so that one key running out takes others down with it. That would turn
//! a per-identity budget into a denial-of-service primitive: pay nothing,
//! burn your own quota, stop everyone.
//!
//! Every request here goes out from its own synthetic source address, so
//! no per-address bucket is anywhere near its own limit. Anything refused
//! was refused by the quota.

use crate::client::HubClient;
use crate::report::{Report, Section, Verdict};
use crate::stats::{summarize, Sample};
use anyhow::Result;
use btclib::crypto::PrivateKey;
use std::path::{Path, PathBuf};

/// Past the sixty-per-window per-key quota, with enough margin that the
/// refusals are unambiguous.
const ATTEMPTS: usize = 75;
/// What the innocent key tries afterwards. Every one of these must work.
const BYSTANDER_ATTEMPTS: usize = 10;

const PATH: &str = "/exchange/deposit";

/// Sends `count` signed requests from `key`, each from a different source
/// address, and returns (served, refused).
async fn spend_quota(
    hub: &HubClient,
    key: &PrivateKey,
    count: usize,
    octet_base: u8,
    label: &'static str,
    samples: &mut Vec<Sample>,
) -> Result<(usize, usize)> {
    let (mut served, mut refused) = (0, 0);
    for index in 0..count {
        let client = hub.from_source(&format!("10.{octet_base}.{}.{}", index / 250, index % 250));
        let reply = client.post_signed(key, PATH, ()).await?;
        samples.push(Sample::new(label, reply.status, reply.latency));
        if reply.ok() {
            served += 1;
        } else {
            refused += 1;
        }
    }
    Ok((served, refused))
}

pub async fn run(repo: &Path, bin_dir: &Path, work_dir: PathBuf) -> Result<Report> {
    let mut harness = super::bring_up(repo, bin_dir, work_dir, true).await?;
    let hub = harness.stack.hub_client()?;
    let mut samples = Vec::new();

    let greedy = PrivateKey::new_key();
    let bystander = PrivateKey::new_key();

    // The bystander goes first as well as last. A key that could not get
    // a request through *before* the flood would make the "after" result
    // meaningless.
    let (before_served, before_refused) =
        spend_quota(&hub, &bystander, 2, 21, "bystander (before)", &mut samples).await?;

    let (greedy_served, greedy_refused) =
        spend_quota(&hub, &greedy, ATTEMPTS, 20, "greedy key", &mut samples).await?;

    // Deliberately reuses the greedy key's addresses. If the quota were
    // leaking into the per-address buckets, this is where it would show.
    let (after_served, after_refused) = spend_quota(
        &hub,
        &bystander,
        BYSTANDER_ATTEMPTS,
        20,
        "bystander (after)",
        &mut samples,
    )
    .await?;

    harness.stack.shutdown().await;

    let quota_bit = greedy_refused > 0;
    let bystander_unharmed = after_served == BYSTANDER_ATTEMPTS;

    let mut section = Section::new("Exhaust one key's quota from many addresses")
        .plan_item("§3.4")
        .fact("greedy_attempts", ATTEMPTS)
        .fact("greedy_served", greedy_served)
        .fact("greedy_refused", greedy_refused)
        .fact("bystander_before_served", before_served)
        .fact("bystander_before_refused", before_refused)
        .fact("bystander_after_attempts", BYSTANDER_ATTEMPTS)
        .fact("bystander_after_served", after_served)
        .fact("bystander_after_refused", after_refused)
        .latency(summarize(&samples, None))
        .note(
            "Every request came from its own synthetic source address, so no per-address bucket \
             was near its limit and any refusal is the per-key quota.",
        );

    section = match (quota_bit, bystander_unharmed) {
        (true, true) => section.verdict(Verdict::Confirmed).note(format!(
            "The greedy key was served {greedy_served} times and refused {greedy_refused}, \
             while a second key -- sending from the very same addresses -- was served all \
             {BYSTANDER_ATTEMPTS} of its requests afterwards. The quota is charged to the \
             identity and nothing else."
        )),
        (true, false) => section.verdict(Verdict::Refuted).finding(format!(
            "Exhausting one key's quota refused {after_refused} of a second, unrelated key's \
             requests. A per-identity budget that spills onto other identities is a \
             denial-of-service primitive: burning your own quota is free and stops everyone."
        )),
        (false, _) => section.verdict(Verdict::Inconclusive).note(format!(
            "The greedy key sent {ATTEMPTS} signed requests without being refused once, so the \
             quota never bit and there was nothing to test isolation against. Check whether \
             the per-key limit is still 60 per window."
        )),
    };

    let mut report = Report::new("drill: quota-isolation", harness.environment);
    report.push(section);
    Ok(report)
}

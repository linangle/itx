//! Saturate one rate-limit tier and confirm the others still serve.
//!
//! Tiering is not just a lower number. Reads, writes, chain writes and
//! health are four separate buckets precisely so that a client flooding
//! one cannot take out another (`hub/src/rate_limit.rs`): a read flood
//! must not stop anyone settling a task, and a burst of writes must not
//! make `/health` return 429 to an uptime check, which reads as "the hub
//! is down" and is exactly the wrong thing to say under load.
//!
//! This is the one drill that deliberately does **not** spread itself
//! across source addresses. The buckets are per address, so looking like
//! many clients would defeat the entire experiment.

use crate::client::{claim_faucet, HubClient};
use crate::report::{Report, Section, Verdict};
use crate::stats::{summarize, Sample};
use anyhow::Result;
use btclib::crypto::PrivateKey;
use std::path::{Path, PathBuf};

/// Comfortably past the read tier's 120 per minute.
const READ_FLOOD: usize = 200;
/// Comfortably past the write tier's 60 per minute.
const WRITE_FLOOD: usize = 100;

/// Fires `count` reads and returns how many were served and how many were
/// refused with 429.
async fn flood_reads(hub: &HubClient, count: usize, samples: &mut Vec<Sample>) -> Result<(usize, usize)> {
    let (mut served, mut refused) = (0, 0);
    for _ in 0..count {
        let reply = hub.get("/tasks?limit=1").await?;
        samples.push(Sample::new("read flood", reply.status, reply.latency));
        match reply.status {
            429 => refused += 1,
            s if (200..300).contains(&s) => served += 1,
            _ => {}
        }
    }
    Ok((served, refused))
}

async fn flood_writes(hub: &HubClient, count: usize, samples: &mut Vec<Sample>) -> Result<(usize, usize)> {
    let (mut served, mut refused) = (0, 0);
    for _ in 0..count {
        // A fresh key each time so the per-pubkey quota, which is a
        // different mechanism with a different number, never becomes the
        // thing doing the refusing.
        let key = PrivateKey::new_key();
        let reply = hub.post_signed(&key, "/exchange/deposit", ()).await?;
        samples.push(Sample::new("write flood", reply.status, reply.latency));
        match reply.status {
            429 => refused += 1,
            s if (200..300).contains(&s) => served += 1,
            _ => {}
        }
    }
    Ok((served, refused))
}

/// Probes the three tiers the flood is not aimed at. Returns whether each
/// still answered something other than 429.
async fn probe_others(
    hub: &HubClient,
    skip_read: bool,
    skip_write: bool,
    samples: &mut Vec<Sample>,
) -> Result<(bool, bool, bool, bool)> {
    let health = hub.get("/health").await?;
    samples.push(Sample::new("probe health", health.status, health.latency));

    let read = if skip_read {
        None
    } else {
        Some(hub.get("/tasks?limit=1").await?)
    };
    if let Some(reply) = &read {
        samples.push(Sample::new("probe read", reply.status, reply.latency));
    }

    let write = if skip_write {
        None
    } else {
        let key = PrivateKey::new_key();
        Some(hub.post_signed(&key, "/exchange/deposit", ()).await?)
    };
    if let Some(reply) = &write {
        samples.push(Sample::new("probe write", reply.status, reply.latency));
    }

    // A chain-tier write. The faucet is the cheapest one to reach that
    // needs no prior state; it may legitimately fail for want of operator
    // coin, and that is still not a 429, which is what is being asked.
    let key = PrivateKey::new_key();
    let chain = claim_faucet(hub, &key).await?;
    samples.push(Sample::new("probe chain", chain.status, chain.latency));

    Ok((
        health.status != 429,
        read.map(|r| r.status != 429).unwrap_or(true),
        write.map(|r| r.status != 429).unwrap_or(true),
        chain.status != 429,
    ))
}

pub async fn run(repo: &Path, bin_dir: &Path, work_dir: PathBuf) -> Result<Report> {
    // No trusted proxy: every request in this drill must be charged to the
    // same address, which is the point.
    let mut harness = super::bring_up(repo, bin_dir, work_dir, false).await?;
    let hub = harness.stack.hub_client()?;
    let chain = harness.stack.chain();
    let operator = harness.stack.operator_key.public_key();
    let mut samples = Vec::new();

    // The chain-tier probe pays out, so the operator needs coin or the
    // probe fails for a reason that has nothing to do with rate limiting.
    chain
        .wait_for_utxo_count(&operator, 2, std::time::Duration::from_secs(300))
        .await?;

    let (reads_served, reads_refused) = flood_reads(&hub, READ_FLOOD, &mut samples).await?;
    let (health_ok, _, write_ok, chain_ok) = probe_others(&hub, true, false, &mut samples).await?;

    let (writes_served, writes_refused) = flood_writes(&hub, WRITE_FLOOD, &mut samples).await?;
    let (health_ok_2, _, _, chain_ok_2) = probe_others(&hub, true, true, &mut samples).await?;

    harness.stack.shutdown().await;

    let isolated = health_ok && write_ok && chain_ok && health_ok_2 && chain_ok_2;

    let mut section = Section::new("Saturate one tier, check the others")
        .plan_item("§3.4")
        .fact("read_requests", READ_FLOOD)
        .fact("reads_served", reads_served)
        .fact("reads_refused_429", reads_refused)
        .fact("write_requests", WRITE_FLOOD)
        .fact("writes_served", writes_served)
        .fact("writes_refused_429", writes_refused)
        .fact("health_survived_read_flood", health_ok)
        .fact("write_survived_read_flood", write_ok)
        .fact("chain_survived_read_flood", chain_ok)
        .fact("health_survived_write_flood", health_ok_2)
        .fact("chain_survived_write_flood", chain_ok_2)
        .latency(summarize(&samples, None))
        .note(
            "All of this came from one source address on purpose. The buckets are per address, \
             so spreading across addresses would measure nothing.",
        );

    section = if isolated {
        section.verdict(Verdict::Confirmed).note(format!(
            "The read tier stopped at {reads_served} served of {READ_FLOOD} and the write tier \
             at {writes_served} of {WRITE_FLOOD}, while health and the chain tier kept \
             answering throughout. The buckets are genuinely independent."
        ))
    } else {
        section.verdict(Verdict::Refuted).finding(
            "Saturating one rate-limit tier made another return 429. The tiers are supposed to \
             be independent buckets; a read flood that can silence /health makes monitoring \
             report an outage that is not happening.",
        )
    };

    let mut report = Report::new("drill: rate-limit-tiers", harness.environment);
    report.push(section);
    Ok(report)
}

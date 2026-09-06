//! The chaos drills.
//!
//! Each one brings up its own node, miner and hub, breaks something
//! specific, and reports what it cost. They are separate from the load
//! profile because they need process control -- you cannot kill a stack
//! you were merely pointed at -- and because what they produce is a
//! verdict on a claim in `docs/agent-ecosystem-plan.md` rather than a
//! latency table.
//!
//! Every drill takes the same two things: where the pinned binaries are,
//! and a working directory it may fill with chain data and delete.

use crate::chain::ChainView;
use crate::report::{Environment, Report};
use crate::stack::{Stack, StackConfig};
use anyhow::{Context, Result};
use btclib::crypto::PublicKey;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub mod escrow_restart;
pub mod node_crash;
pub mod payout_ceiling;
pub mod quota_isolation;
pub mod rate_limit_tiers;
pub mod replay_storm;

/// How much confirmed operator coin a drill waits for before it starts
/// spending. Enough to cover every payout a drill makes several times
/// over, so a shortfall in the middle of a drill is a finding rather than
/// a setup problem.
pub const OPERATOR_FUNDING_TARGET: u64 = 500_000_000;

/// How long to wait for the miner to produce that. Early blocks are found
/// at the chain's minimum difficulty and arrive quickly; this is a
/// generous bound on a cold start, not an expectation.
const FUNDING_TIMEOUT: Duration = Duration::from_secs(300);

/// Everything a drill needs to run.
pub struct Harness {
    pub stack: Stack,
    pub environment: Environment,
}

/// Brings up a stack from binaries pinned under `bin_dir`.
///
/// `trust_local_proxy` starts the hub believing `X-Forwarded-For` from
/// `127.0.0.1`, which is how a drill makes one box look like many
/// clients. The drills that are *about* the rate limiter's per-address
/// behaviour pass `false`, so they are charged as one address, which is
/// the honest way to saturate a bucket.
pub async fn bring_up(
    repo: &Path,
    bin_dir: &Path,
    work_dir: PathBuf,
    trust_local_proxy: bool,
) -> Result<Harness> {
    // A leftover chain or hub store from a previous run would make the
    // drill's arithmetic meaningless -- balances, task ids and replay
    // signatures would all carry over. Always start from nothing.
    if work_dir.exists() {
        std::fs::remove_dir_all(&work_dir)
            .with_context(|| format!("clearing {}", work_dir.display()))?;
    }

    let mut config = StackConfig::new(bin_dir, &work_dir);
    if trust_local_proxy {
        config = config.trusting_local_proxy();
    }

    let mut environment = Environment::capture(repo);
    for name in ["node", "miner", "hub"] {
        environment.fingerprint(&bin_dir.join(name))?;
    }

    let mut stack = Stack::prepare(config)?;
    stack.start_all().await?;
    Ok(Harness { stack, environment })
}

/// Waits until `pubkey` holds `target` in confirmed, unspoken-for coin.
///
/// Every drill that spends starts here. Doing it as a balance check rather
/// than a block count means it is insensitive to how fast the miner
/// happens to be on the machine of the day.
pub async fn wait_until_funded(chain: &ChainView, pubkey: &PublicKey, target: u64) -> Result<u64> {
    let balance = chain
        .wait_for_balance(pubkey, target, FUNDING_TIMEOUT)
        .await?;
    anyhow::ensure!(
        balance >= target,
        "the miner produced only {balance} of the {target} needed to run this drill within {}s",
        FUNDING_TIMEOUT.as_secs()
    );
    Ok(balance)
}

/// Names every drill, so the CLI and the README cannot disagree about
/// what exists.
pub const ALL: &[&str] = &[
    "node-crash",
    "escrow-restart",
    "replay-storm",
    "rate-limit-tiers",
    "quota-isolation",
    "payout-ceiling",
];

/// Runs one drill by name.
pub async fn run(
    name: &str,
    repo: &Path,
    bin_dir: &Path,
    work_root: &Path,
) -> Result<Report> {
    let work_dir = work_root.join(name);
    match name {
        "node-crash" => node_crash::run(repo, bin_dir, work_dir).await,
        "escrow-restart" => escrow_restart::run(repo, bin_dir, work_dir).await,
        "replay-storm" => replay_storm::run(repo, bin_dir, work_dir).await,
        "rate-limit-tiers" => rate_limit_tiers::run(repo, bin_dir, work_dir).await,
        "quota-isolation" => quota_isolation::run(repo, bin_dir, work_dir).await,
        "payout-ceiling" => payout_ceiling::run(repo, bin_dir, work_dir).await,
        other => anyhow::bail!("unknown drill {other}; known drills are {}", ALL.join(", ")),
    }
}

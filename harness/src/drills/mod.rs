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

pub mod escrow_refund;
pub mod escrow_restart;
pub mod exchange_restart;
pub mod node_crash;
pub mod payout_ceiling;
pub mod quota_isolation;
pub mod rate_limit_tiers;
pub mod replay_storm;
pub mod signed_write_cost;

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

/// The drills `drill all` runs, and therefore the ones the nightly job
/// gates on.
///
/// `exchange-restart` is deliberately **not** here. It is not deleted --
/// see `RETIRED` below for why it cannot run and what would bring it
/// back -- but a drill that cannot reach its own setup has no business
/// in a nightly gate, where it reads as a flaky schedule rather than as
/// a retired feature.
pub const ALL: &[&str] = &[
    "node-crash",
    "escrow-restart",
    "escrow-refund",
    "replay-storm",
    "rate-limit-tiers",
    "quota-isolation",
    "payout-ceiling",
    "signed-write-cost",
];

/// Drills that still exist and still compile, but cannot currently run,
/// with the reason. Named so `drill <name>` explains itself rather than
/// timing out, and so a reader of `ALL` can see what is missing from it.
///
/// `exchange-restart` needs a two-sided book, which needs `compute` to
/// sell. Settlement minted `compute` and nothing else ever did; the
/// marketplace pivot removed that mint, because `capabilities` is a
/// free-form tag and anyone could print the asset the book quoted
/// against for two chain fees. The only `credit_compute` left is the
/// taker fee, which needs trades that need compute -- so there is no
/// longer any way to obtain any, and the drill's setup can only wait out
/// its 300-second budget and give up.
///
/// It is kept rather than deleted because the exchange itself is kept:
/// the routes are behind `--enable-exchange`, off by default. Whoever
/// restores a compute source restores this drill's coverage with it: put
/// the name back in `ALL` and its arm back in `run`, and the checked-in
/// baseline is still there to compare against. Deleting the drill would
/// make that a rewrite instead of a re-listing.
pub const RETIRED: &[(&str, &str)] = &[(
    "exchange-restart",
    "needs `compute` to sell, and settlement stopped minting it when the exchange was deferred. \
     There is no path that credits `compute` any more, so the two-sided book this drill measures \
     cannot be built. Restore a compute source and put it back in ALL.",
)];

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
        "escrow-refund" => escrow_refund::run(repo, bin_dir, work_dir).await,
        "replay-storm" => replay_storm::run(repo, bin_dir, work_dir).await,
        "rate-limit-tiers" => rate_limit_tiers::run(repo, bin_dir, work_dir).await,
        "quota-isolation" => quota_isolation::run(repo, bin_dir, work_dir).await,
        "payout-ceiling" => payout_ceiling::run(repo, bin_dir, work_dir).await,
        "signed-write-cost" => signed_write_cost::run(repo, bin_dir, work_dir).await,
        other => {
            if let Some((_, why)) = RETIRED.iter().find(|(name, _)| *name == other) {
                anyhow::bail!("{other} is retired: {why}");
            }
            anyhow::bail!("unknown drill {other}; known drills are {}", ALL.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The registry and the retirement list must not disagree.
    ///
    /// A name in both would be listed for the nightly gate and refused by
    /// the dispatcher, which is a drill that fails every night for a
    /// reason the gate cannot explain -- exactly the state N21 recorded.
    #[test]
    fn no_drill_is_both_scheduled_and_retired() {
        for (name, _) in RETIRED {
            assert!(
                !ALL.contains(name),
                "{name} is in ALL and in RETIRED; the nightly would run a drill that cannot run"
            );
        }
    }

    /// Every scheduled drill has an arm in `run`.
    ///
    /// Without this, removing a dispatch arm and forgetting the `ALL`
    /// entry produces "unknown drill" once a night. The same shape as the
    /// MCP server's tool table, which had two tools missing from it and
    /// an assertion that had never run.
    #[tokio::test]
    async fn every_scheduled_drill_dispatches() {
        let tmp = std::env::temp_dir().join("itx-drill-dispatch-test");
        for name in ALL {
            // These cannot actually run here -- there are no pinned
            // binaries -- so the assertion is only that dispatch found an
            // arm. Reaching the drill and failing to bring a stack up is
            // a pass; falling through to the catch-all is not.
            let err = run(name, &tmp, &tmp, &tmp).await.expect_err(
                "a drill cannot succeed without binaries; if this ever passes, rewrite the test",
            );
            let message = format!("{err:#}");
            assert!(
                !message.contains("unknown drill"),
                "{name} is scheduled but has no arm in run(): {message}"
            );
        }
    }

    /// A retired drill says why, immediately.
    ///
    /// The alternative is what `exchange-restart` used to do: spend its
    /// whole 300-second setup budget waiting for a credit that can no
    /// longer exist, then bail with a message about a two-sided book.
    #[tokio::test]
    async fn a_retired_drill_explains_itself_rather_than_timing_out() {
        let tmp = std::env::temp_dir().join("itx-drill-retired-test");
        let err = run("exchange-restart", &tmp, &tmp, &tmp)
            .await
            .expect_err("a retired drill must not run");
        let message = format!("{err:#}");
        assert!(message.contains("retired"), "should say it is retired: {message}");
        assert!(
            message.contains("compute"),
            "should say what it needs and no longer has: {message}"
        );
    }
}

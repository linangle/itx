use tracing::*;

mod auth;
mod faucet_pow;
mod board;
mod escrow_key;
mod handlers;
mod metrics;
mod names;
mod node_client;
mod operator_wallet;
mod rate_limit;
mod reconcile;
mod store;

use anyhow::Result;
use argh::FromArgs;
use axum::extract::State;
use axum::http::{HeaderName, Method};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use board::{PendingDeposit, Reputation, Task, TaskBoard, TaskStatus};
use escrow_key::EscrowSecret;
use btclib::crypto::{PrivateKey, PublicKey};
use btclib::util::Saveable;
use names::NameRegistry;
use node_client::NodeClient;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
use store::HubStore;
use tokio::sync::{Mutex, RwLock};
use tokio::time::{interval, Duration};
use tower_http::compression::CompressionLayer;
use tower_http::cors::{Any, CorsLayer};
use tracing_subscriber::prelude::*;
use uuid::Uuid;

/// Shared state handed to every HTTP handler. `board` changes after
/// startup so it's behind its own lock; `payout_lock` guards a different
/// thing -- not board state, but the operator's on-chain UTXO set (see
/// `handlers::pay_bounty`) -- so it's a separate lock rather than piggy-
/// backing on `board`'s. `store`/`node` are internally either lock-free
/// (redb) or open a fresh connection per call (see `NodeClient`), and the
/// operator keys never change for the process's lifetime.
pub struct AppState {
    pub board: RwLock<TaskBoard>,
    /// Shared rather than owned outright because `replay_guard` needs a
    /// handle to the same file: redb is single-process and will not open
    /// one store twice, so the two cannot each hold their own.
    pub store: Arc<HubStore>,
    pub node: NodeClient,
    pub operator_private_key: PrivateKey,
    pub operator_public_key: PublicKey,
    /// Serializes every `pay_bounty` call (faucet grants and task
    /// payouts alike) so at most one is ever building/submitting a
    /// transaction against the operator's UTXO set at a time. Without
    /// this, two concurrent payouts (a faucet claim racing a task
    /// settlement, or two different tasks settling around the same
    /// moment -- nothing serializes across *different* recipients today)
    /// can both fetch the same unspent UTXO before either transaction is
    /// confirmed and both try to spend it. Since every hub-issued
    /// transaction uses the same flat `HUB_TRANSACTION_FEE`, the node's
    /// mempool conflict resolution (replace-by-strictly-higher-fee) never
    /// lets the second one replace the first -- it just rejects it
    /// outright. `NodeClient::submit_transaction` is fire-and-forget
    /// (see its own doc comment) and the node protocol sends no error
    /// back for a rejected transaction, so the loser's caller would
    /// otherwise have no idea its payout never actually landed on chain,
    /// and would go on to call `mark_recipient_paid`/`save_faucet_grant`
    /// anyway.
    pub payout_lock: Mutex<()>,
    /// How many spendable outputs the operator's wallet is kept split
    /// into (`--operator-wallet-outputs`). This number *is* the payout
    /// ceiling -- see `operator_wallet` and plan §6.4b.
    pub operator_wallet_outputs: usize,
    /// The inputs the last fan-out spent, while one may still be
    /// unconfirmed.
    ///
    /// A fan-out's new outputs do not exist for the node until the
    /// transaction is mined, so the sweep a minute later would see the
    /// same short wallet and split again -- this time taking an output
    /// that was already doing its job. Remembering what was spent
    /// answers the only question that matters, are those inputs still
    /// in the UTXO set, with no timer to tune and nothing to get wrong
    /// across a restart: a fresh process starts with this empty, and
    /// empty means "check", which is the safe direction.
    pub operator_fan_out_inflight: Mutex<Vec<btclib::sha256::Hash>>,
    /// The same, for the exchange's pooled custody address. Its own list
    /// rather than a shared one because the two wallets are reshaped
    /// independently, under different locks, and one being mid-split
    /// says nothing about the other.
    pub custody_fan_out_inflight: Mutex<Vec<btclib::sha256::Hash>>,
    /// The exchange's pooled custody address -- deliberately a *separate*
    /// key from `operator_private_key`, not a reuse of it, so exchange
    /// liabilities (money owed back to depositors) never comingle with
    /// `ensure_operator_can_fund`'s `balance - allocated_bounty() >=
    /// bounty` math, which has no concept of that. Every confirmed
    /// exchange deposit is swept here (see `handlers::sweep_exchange_deposit`);
    /// withdrawals pay back out of it via the same `pay_from` every other
    /// hub payment already uses.
    pub exchange_custody_private_key: PrivateKey,
    pub exchange_custody_public_key: PublicKey,
    /// Same reasoning as `payout_lock`, scoped to the custody address's
    /// own UTXO set instead of the operator's -- a different key, a
    /// different UTXO set, no reason to serialize exchange withdrawals
    /// against unrelated operator payouts (or vice versa).
    pub exchange_custody_payout_lock: Mutex<()>,
    /// The master secret every one-time escrow deposit key is derived
    /// from (see `escrow_key`). Held here rather than on `TaskBoard`
    /// because the board is a pure in-memory state machine that holds no
    /// key material of its own -- the same reason `operator_private_key`
    /// lives here and not there.
    pub escrow_secret: EscrowSecret,
    /// Request counters for `rate_limit::middleware`, one bucket per
    /// client per endpoint tier (`rate_limit::Bucket`). Same
    /// instance-scoping reasoning as `payout_lock` -- see
    /// `rate_limit::RateLimitTable`'s own doc comment for why this can't
    /// be a global static.
    pub rate_limits: rate_limit::RateLimitTable,
    /// Which peers' `X-Forwarded-For` header the rate limiter believes
    /// (`--trusted-proxies`). Deliberately empty unless configured: see
    /// `rate_limit::TrustedProxies`.
    pub trusted_proxies: rate_limit::TrustedProxies,
    /// The signed-envelope replay guard: which signatures this hub has
    /// already accepted, in memory and on disk. Instance-scoped for the
    /// reason `auth::ReplayGuard`'s own doc comment gives -- its durable
    /// half is one specific store file, so it cannot be shared by two
    /// hubs the way a bare signature set could.
    pub replay_guard: auth::ReplayGuard,
    /// The faucet's outstanding and recently-redeemed proof-of-work
    /// challenges (§5). Instance-scoped and paired with a durable table
    /// for the reason `auth::ReplayGuard` is: a redemption only this
    /// process remembers is replayable across a restart.
    pub faucet_challenges: faucet_pow::ChallengeBook,
    /// The difficulty knob, kept alongside the book so `/faucet/challenge`
    /// can report it without recomputing it from the target.
    pub faucet_expected_hashes: u64,
    /// Display names for agents (see `names`). Its own lock rather than
    /// a field on `board`: the registry is presentation, the board is
    /// the economy, and a read of the leaderboard should not have to
    /// take a write lock on the board just to mint a name for an agent
    /// that turned up since the last request.
    pub names: RwLock<NameRegistry>,
    /// The last sweep of every agent's on-chain balance, and when it was
    /// taken -- the one thing `leaderboard` cannot rank from memory. Only
    /// `?sort=net_worth` ever fills it, and it is a cache in the honest
    /// sense: dropping it costs a sweep and nothing else. See
    /// `handlers::net_worth_snapshot` for why the field is priced in one
    /// pass rather than a page at a time.
    pub net_worths: RwLock<Option<(std::time::Instant, std::collections::HashMap<String, u64>)>>,
    /// Everything the hub counts about itself, rendered at `/metrics`.
    ///
    /// An `Arc` rather than a plain field because two of its writers --
    /// the replay guard and the node client -- are built before this
    /// struct exists and hold their own handles to the same table. Same
    /// instance-scoping reasoning as `rate_limits`: a process-wide static
    /// would have every hub in the test suite reporting into one another's
    /// numbers.
    pub metrics: Arc<metrics::Metrics>,
}

impl AppState {
    /// Names every agent in `pubkeys` that doesn't already have one, and
    /// persists whatever is new.
    ///
    /// The write lock is held only for the assignment itself and dropped
    /// before the store is touched, so a slow fsync never blocks a
    /// concurrent reader. That ordering means a crash between the two
    /// can lose a just-minted name -- which costs nothing, because the
    /// agent is simply renamed on the next request. That is the opposite
    /// of `PendingDeposit`'s persist-before-you-hand-it-out rule, and
    /// for the opposite reason: nothing is irrecoverable here.
    ///
    /// Returns the resolved names. A pubkey missing from the map means
    /// the pool is exhausted; callers render the pubkey alone rather
    /// than failing the request.
    async fn ensure_named(
        &self,
        pubkeys: impl IntoIterator<Item = PublicKey>,
    ) -> std::collections::HashMap<String, String> {
        let mut resolved = std::collections::HashMap::new();
        let mut fresh: Vec<(PublicKey, String)> = Vec::new();
        {
            let mut names = self.names.write().await;
            for pubkey in pubkeys {
                let Some((name, is_new)) = names.assign(&pubkey) else {
                    warn!("agent name pool is exhausted; {pubkey} stays unnamed");
                    continue;
                };
                if is_new {
                    fresh.push((pubkey.clone(), name.clone()));
                }
                resolved.insert(pubkey.to_string(), name);
            }
        }
        if !fresh.is_empty() {
            if let Err(e) = self.store.save_agent_name_batch(&fresh) {
                // Non-fatal on purpose: the names are already live in
                // memory and correct for this response. The cost of a
                // failed write is that they're re-minted after a
                // restart, which is a cosmetic regression, not a lost
                // record -- so it should not turn a read request into an
                // error page.
                error!("failed to persist {} new agent name(s): {e}", fresh.len());
            }
        }
        resolved
    }
}

#[derive(FromArgs)]
/// itx agent hub -- HTTP API for posting/claiming tasks, faucet grants, and
/// agent reputation, backed by an itx blockchain node.
struct Args {
    #[argh(option, default = "9100")]
    /// port to listen on
    port: u16,
    #[argh(option, default = "String::from(\"0.0.0.0\")")]
    /// address to listen on. Defaults to `0.0.0.0`, which is every
    /// interface -- keep that only for a hub reachable directly. Behind a
    /// reverse proxy on the same box, set `127.0.0.1`: the hub speaks
    /// cleartext HTTP, so on the default the host firewall is not defence
    /// in depth for it, it is the only thing between signed envelopes and
    /// the internet, and a flushed ruleset exposes them. See
    /// `docs/deployment.md` §1.
    bind: String,
    #[argh(option, default = "String::from(\"127.0.0.1:9000\")")]
    /// comma-separated addresses of blockchain nodes to talk to. The first
    /// is used for every call unless it's unreachable, in which case the
    /// next one is tried -- see `NodeClient::connect`'s doc comment for
    /// why this is ordered failover, not load-balancing.
    node_addresses: String,
    #[argh(option, default = "String::from(\"./hub.redb\")")]
    /// path to the hub's durable store (a redb database file)
    store_file: String,
    #[argh(option, default = "String::from(\"./hub_operator.priv.cbor\")")]
    /// path to the operator's private key (generated on first run if missing)
    operator_key_file: String,
    #[argh(option, default = "String::from(\"./hub_exchange_custody.priv.cbor\")")]
    /// path to the exchange's pooled custody private key (generated on first run if missing)
    exchange_custody_key_file: String,
    #[argh(option, default = "String::from(\"./hub_escrow_secret.bin\")")]
    /// path to the master secret every escrow deposit key is derived from
    /// (generated on first run if missing). Back this up: without it, any
    /// escrow address already handed out becomes unsweepable.
    escrow_secret_file: String,
    #[argh(option, default = "faucet_pow::DEFAULT_EXPECTED_HASHES")]
    /// how much work a faucet grant costs, in expected SHA-256 hashes.
    /// The default is calibrated so this project's own Python client
    /// solves in about fourteen seconds on one core; raise it to make
    /// the faucet dearer under attack, lower it to make onboarding
    /// quicker. Applies to challenges issued from now on -- work already
    /// under way is judged against the target it was issued with.
    faucet_pow_expected_hashes: u64,
    #[argh(option, default = "String::new()")]
    /// comma-separated addresses of the reverse proxies in front of this
    /// hub, whose `X-Forwarded-For` header the rate limiter should
    /// believe. Empty (the default) trusts nothing and always charges the
    /// direct peer. Set this to the proxy's address when deploying behind
    /// one, and to nothing at all when not -- see
    /// `rate_limit::TrustedProxies` for why an un-proxied hub that
    /// honoured the header would have no working rate limit.
    trusted_proxies: String,
    #[argh(option, default = "operator_wallet::DEFAULT_WALLET_OUTPUTS")]
    /// how many spendable outputs to keep the operator's wallet split
    /// into. This is the hub's payout ceiling: change from a payment is
    /// unconfirmed until mined, so the operator can make about this
    /// many payments per block and no more (plan §6.4b). Raise it for a
    /// hub that pays out in bursts; every payment fetches the whole
    /// UTXO set to select from, so it is not free. One disables the
    /// fan-out entirely.
    operator_wallet_outputs: usize,
}

fn load_or_create_key(path: &str) -> Result<PrivateKey> {
    match PrivateKey::load_from_file(path) {
        Ok(key) => Ok(key),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("no key found at {path}, generating a new one...");
            let key = PrivateKey::new_key();
            key.save_to_file(path)?;
            restrict_to_owner(path)?;
            Ok(key)
        }
        Err(e) => Err(e.into()),
    }
}

/// Narrows a freshly-written key file to owner-only. `PrivateKey::save_to_file`
/// creates with the process umask, which on a default umask leaves the
/// operator's and custody keys group- and world-readable -- the two keys that
/// between them control the hub's treasury and every agent's exchange deposits.
///
/// This runs after the write rather than as part of it (unlike
/// `escrow_key::EscrowSecret`, which opens with the mode set), because the
/// write itself belongs to btclib and is shared with the wallet and miner.
/// The gap that leaves is bounded by first-run only, and is closed the moment
/// this returns; making it airtight means changing `save_to_file` for every
/// caller in the workspace, which is a separate change.
fn restrict_to_owner(path: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// The boot fan-out, wrapped only so the reason it is not fatal is
/// written down somewhere a reader will find it.
///
/// A hub whose node is unreachable at boot must still come up: the node
/// may be starting alongside it, and every route that does not touch
/// the chain works regardless. Each maintenance call already logs and
/// counts its own failure, and the sweep tries again in a minute. Both
/// are now bounded by `node_client`'s timeouts, so an unreachable node
/// delays the listener by seconds rather than indefinitely.
async fn maintain_wallets_at_boot(state: &Arc<AppState>) {
    handlers::maintain_operator_outputs(state).await;
    handlers::maintain_custody_outputs(state).await;
    println!(
        "operator wallet: {} spendable output(s), custody: {}, keeping {} each",
        state.metrics.operator_ready_outputs.load(Ordering::Relaxed),
        state.metrics.custody_ready_outputs.load(Ordering::Relaxed),
        state.operator_wallet_outputs,
    );
}

/// Periodically reopens abandoned claims, retries paying out any task
/// stuck `Verified` by an earlier failed payout attempt, and sweeps the
/// auth replay guard. Runs for the lifetime of the process.
async fn sweep_loop(state: Arc<AppState>) {
    let mut ticker = interval(Duration::from_secs(SWEEP_INTERVAL_SECONDS));
    // Lag is measured between consecutive pass *starts* rather than
    // against tokio's own tick schedule, because `interval` fires
    // immediately when it is behind: by the time we are woken, the
    // lateness we want to report has already been absorbed. The gap
    // between starts, minus the interval, is the same number and is one
    // the loop can actually see. `None` on the first pass because there
    // is no previous start to measure against, and reporting the process
    // uptime as lag would page on every boot.
    let mut previous_start: Option<Instant> = None;
    loop {
        ticker.tick().await;
        let started = Instant::now();
        if let Some(previous) = previous_start {
            let lag = started
                .duration_since(previous)
                .saturating_sub(Duration::from_secs(SWEEP_INTERVAL_SECONDS));
            let lag_ms = lag.as_millis() as u64;
            state.metrics.sweep_last_lag_ms.store(lag_ms, Ordering::Relaxed);
            state.metrics.sweep_max_lag_ms.fetch_max(lag_ms, Ordering::Relaxed);
        }
        previous_start = Some(started);

        run_sweep_once(&state, chrono::Utc::now()).await;

        let elapsed_ms = started.elapsed().as_millis() as u64;
        state.metrics.sweep_last_duration_ms.store(elapsed_ms, Ordering::Relaxed);
        state.metrics.sweep_duration_ms_total.fetch_add(elapsed_ms, Ordering::Relaxed);
        state.metrics.sweep_passes.fetch_add(1, Ordering::Relaxed);
    }
}

/// How often the sweep runs. Named rather than inline now that the lag
/// calculation needs the same number the ticker was built from -- two
/// copies of it would make the lag silently wrong the day someone retunes
/// the interval.
const SWEEP_INTERVAL_SECONDS: u64 = 60;

/// Takes the board's write lock, charging the wait to the sweep's
/// contention counter.
///
/// The sweep is the hub's only routine *writer*, so it is the one caller
/// that has to wait for every reader to drain -- which makes its wait the
/// most informative single sample of board contention available without
/// instrumenting every handler (deferred by plan §10.1). A rising number
/// here means readers are holding the board long enough to starve the
/// writer, which is the shape worth alerting on; it does not, and cannot,
/// report contention between two readers.
async fn board_write_timed(state: &AppState) -> tokio::sync::RwLockWriteGuard<'_, TaskBoard> {
    let queued_at = Instant::now();
    let guard = state.board.write().await;
    state
        .metrics
        .sweep_board_lock_wait_ms_total
        .fetch_add(queued_at.elapsed().as_millis() as u64, Ordering::Relaxed);
    guard
}

/// One sweep pass, pulled out of `sweep_loop` so tests can drive it
/// directly without waiting on a real 60-second timer. `now` is threaded
/// through (rather than each check calling `chrono::Utc::now()` itself) so
/// tests can simulate a deadline having passed without an actual sleep --
/// mirroring the board methods this delegates to, which already take `now`
/// for the same reason.
/// How long a submitted payout is left alone before the sweep asks the
/// node what became of it.
///
/// `submit_transaction` is fire-and-forget: it returns once the bytes
/// are written, before the node has read them, let alone mined them. A
/// resolution run in that window would find the output absent and the
/// inputs untouched and conclude the payout was lost -- and act on it,
/// by sending a second one. Thirty seconds is roughly two blocks at the
/// 16-second target, and comfortably longer than the round trip it is
/// really guarding.
///
/// It costs nothing in practice. The sweep runs once a minute, so a
/// payout submitted at any point in one interval is already older than
/// this by the next one; the grace only ever suppresses a resolution
/// that would have been asked in the same sweep that sent it, which is
/// the case where the answer cannot be trusted anyway.
const PAYOUT_RESOLUTION_GRACE_SECONDS: i64 = 30;

async fn run_sweep_once(state: &Arc<AppState>, now: chrono::DateTime<chrono::Utc>) {
    let reopened = {
        let mut board = board_write_timed(state).await;
        board.expire_claims(now)
    };
    for task_id in reopened {
        persist_task_by_id(state, task_id, "expired-claim").await;
    }

    let cancelled = {
        let mut board = board_write_timed(state).await;
        board.cancel_understaffed_consensus_tasks(now)
    };
    for task_id in cancelled {
        persist_task_by_id(state, task_id, "understaffed consensus").await;
        // An understaffed cancellation always ends in Closed (never a
        // winner), so any escrow funding it always needs refunding, not
        // settling -- a no-op for an operator-funded task.
        handlers::refund_closed_task_escrow(state, task_id).await;
        info!("sweep: cancelled understaffed consensus task {task_id} past its join deadline");
    }

    let resolved = {
        let mut board = board_write_timed(state).await;
        board.resolve_expired_consensus_tasks(now)
    };
    for task_id in resolved {
        // A deadline-triggered resolution can ding reputation for several
        // assignees at once (every no-show/loser), not just whichever
        // pubkey happens to be at hand -- persist every one of them, or
        // the penalty is silently lost on the next restart.
        //
        // The task and all of those records go in **one** transaction,
        // which is the sweep's half of the fix `persist_consensus_submission`
        // is the handler's half of. It used to save the task and then
        // batch the reputations separately, dropping the batch's error,
        // so a lost batch silently forgave everyone who lost that round
        // while the task recording the round stayed on disk (plan
        // §6.5c). Reputation is the input to `min_reputation`, so a lost
        // penalty is a penalized agent still claiming excluded work.
        let task = {
            let board = state.board.read().await;
            board.get_task(task_id).cloned()
        };
        if let Some(task) = &task {
            let entries: Vec<(PublicKey, Reputation)> = {
                let board = state.board.read().await;
                task.consensus_assignees()
                    .into_iter()
                    .map(|assignee| {
                        let reputation = board.reputation(&assignee);
                        (assignee, reputation)
                    })
                    .collect()
            };
            if let Err(e) = state.store.save_task_and_reputation_batch(task, &entries) {
                error!(
                    "failed to persist resolved consensus task {task_id} and its assignees' reputation: {e}"
                );
            }
            // A tie (Closed, no winner) needs its escrow refunded, same as
            // an understaffed cancellation; a real winner (Verified) is
            // handled by the settlement pass below instead, which is
            // itself escrow-aware.
            if task.status == TaskStatus::Closed {
                handlers::refund_closed_task_escrow(state, task_id).await;
            }
        }
        info!("sweep: resolved consensus task {task_id} past its submission deadline");
    }

    let finalized = {
        let mut board = board_write_timed(state).await;
        board.finalize_unchallenged_disputable_tasks(now)
    };
    for task_id in finalized {
        persist_task_by_id(state, task_id, "unchallenged disputable").await;
        info!("sweep: finalized unchallenged disputable task {task_id} past its dispute window");
    }

    let overdue_escrows: Vec<PendingDeposit> = {
        let board = state.board.read().await;
        board.overdue_reserved_escrows(now).into_iter().cloned().collect()
    };
    for deposit in overdue_escrows {
        let deposit_id = deposit.id;
        handlers::refund_escrow(state, &deposit).await;
        info!("sweep: refunded overdue unconfirmed escrow deposit {deposit_id}");
    }

    // Resolve first, then send. A payout that confirmed this interval
    // drops out of `unsubmitted_payouts` before the sending pass looks
    // at it, and one the node proves it never received is re-sent by
    // `resolve_payout_attempt` itself -- so the two passes never both
    // act on the same payout in one sweep.
    let settled_enough_to_ask = now - chrono::Duration::seconds(PAYOUT_RESOLUTION_GRACE_SECONDS);
    // Collected into a binding first, deliberately. Written as
    // `for attempt in state.board.read().await.outstanding_payout_attempts()`
    // the read guard is a temporary in the iterator expression, and Rust
    // keeps those alive for the whole loop -- so the first resolution
    // that took the board's write lock would deadlock against a lock the
    // loop itself was still holding.
    let outstanding = state.board.read().await.outstanding_payout_attempts();
    for attempt in outstanding {
        if attempt.submitted_at > settled_enough_to_ask {
            continue;
        }
        handlers::resolve_payout_attempt(state, &attempt).await;
    }

    let unpaid: Vec<Uuid> = state
        .board
        .read()
        .await
        .verified_unpaid_tasks()
        .iter()
        .map(|t| t.id)
        .collect();
    for task_id in unpaid {
        if handlers::try_settle_verified_task(state, task_id).await {
            // "submitted", not "paid" -- what comes back from here is
            // that the transaction went onto the wire. Whether it
            // reached a block is what the resolution pass above answers,
            // one sweep later at the earliest.
            warn!("sweep: retried and submitted the payout for task {task_id}");
        }
    }

    // Independent of the loop above: a resolved dispute's bond leg is a
    // *different* escrow than the task's own, and can still need a retry
    // even after the task itself already reached Paid (which is exactly
    // when it drops out of verified_unpaid_tasks) -- see
    // tasks_with_unsettled_dispute_bonds's own doc comment.
    let unsettled_bonds: Vec<Uuid> = state.board.read().await.tasks_with_unsettled_dispute_bonds();
    for task_id in unsettled_bonds {
        if handlers::settle_dispute_bond(state, task_id).await {
            warn!("sweep: retried and settled dispute bond for task {task_id}");
        }
    }

    let unswept: Vec<Uuid> = state.board.read().await.unswept_exchange_deposits();
    for deposit_id in unswept {
        if handlers::sweep_exchange_deposit(state, deposit_id).await {
            info!("sweep: swept exchange deposit {deposit_id} into pooled custody");
        }
    }

    state.replay_guard.cleanup(now);
    state.faucet_challenges.cleanup(now);
    rate_limit::cleanup(&state.rate_limits);
    // After the retries above, not before. Every one of them may spend
    // an operator output, so topping the wallet up first would measure
    // a wallet the same pass is about to drain -- and the fan-out takes
    // `payout_lock`, which those retries want too.
    handlers::maintain_operator_outputs(state).await;
    // Custody after the deposit sweep above, for the mirror of the same
    // reason: a swept deposit credits custody, so reshaping first would
    // plan against a wallet this pass is about to grow. The two wallets
    // take different locks, so this waits on nothing the operator's
    // fan-out is doing.
    handlers::maintain_custody_outputs(state).await;
    sample_gauges(state).await;
}

/// Samples the numbers `/metrics` cannot compute for itself.
///
/// Everything here needs either the board lock or the node, and a scrape
/// is allowed to touch neither (see `metrics`). Running it on the sweep's
/// cadence is what buys that: the cost lands once a minute on a task that
/// was already going to take both, instead of once per scrape on a route
/// anyone can call.
///
/// Runs last in the pass, deliberately. Sampling before the sweep's own
/// work would report the board as the previous minute left it, so an
/// operator watching `hub_board_outstanding_payouts` fall would be
/// watching it a minute late -- and that gauge exists precisely to answer
/// "is the sweep making progress right now".
///
/// Failures are counted, never propagated. A sweep that cannot reach the
/// node still has claims to expire and payouts to resolve, and the
/// failure counters plus a rising observation age say more about a
/// missing node than an aborted pass would.
async fn sample_gauges(state: &Arc<AppState>) {
    // One read lock for every board-derived number, released before any
    // node call. Holding it across an `await` on the network would make
    // the metrics sampler itself a source of the contention it is here to
    // measure -- and against the node, that await is unbounded.
    let (grants, open_tasks, outstanding_payouts, liabilities) = {
        let board = state.board.read().await;
        let liabilities: u64 = board
            .all_exchange_accounts()
            // `locked_base` is included because a balance locked behind a
            // resting order is still money owed to the depositor: they can
            // cancel the order and withdraw it. Counting only the free
            // half would report a hub as solvent precisely when its order
            // book is busiest, which is when it least deserves the
            // benefit of the doubt. Saturating, because a solvency figure
            // that wraps on overflow is worse than one that saturates.
            .map(|(_, account)| account.base_balance.saturating_add(account.locked_base))
            .fold(0u64, |total, owed| total.saturating_add(owed));
        (
            board.all_faucet_grants().count() as u64,
            board
                .all_tasks()
                .filter(|task| !matches!(task.status, TaskStatus::Paid | TaskStatus::Closed))
                .count() as u64,
            board.outstanding_payout_attempts().len() as u64,
            liabilities,
        )
    };
    state.metrics.faucet_grants.store(grants, Ordering::Relaxed);
    state.metrics.board_open_tasks.store(open_tasks, Ordering::Relaxed);
    state.metrics.board_outstanding_payouts.store(outstanding_payouts, Ordering::Relaxed);
    state.metrics.exchange_liabilities.store(liabilities, Ordering::Relaxed);

    match state.node.chain_tip().await {
        Ok(height) => {
            state.metrics.chain_height.store(height as u64, Ordering::Relaxed);
            // Stamped only on success, which is what makes the age
            // meaningful: a hub that has lost its node keeps reporting the
            // last height it knew, and the age is the only thing that says
            // the number is stale.
            state
                .metrics
                .chain_observed_at_unix
                .store(chrono::Utc::now().timestamp().max(0) as u64, Ordering::Relaxed);
        }
        Err(e) => {
            state.metrics.chain_observation_failures.fetch_add(1, Ordering::Relaxed);
            debug!("sweep: could not read the chain tip for metrics: {e}");
        }
    }

    // Solvency gets its own node call rather than being derived from
    // anything already fetched, because it is the one number where being
    // wrong is a financial statement rather than an operational one: the
    // custody address's on-chain balance should always be at least
    // `hub_exchange_liabilities`, and nothing else in the hub checks it
    // (`docs/deployment.md` §8.3). It is computed here rather than per
    // scrape for the same reason as the chain tip -- and unlike the chain
    // tip, per-scrape would be actively dangerous, since it would let an
    // unauthenticated caller drive balance lookups against the pool that
    // real withdrawals queue on.
    match state.node.balance(&state.exchange_custody_public_key).await {
        Ok(balance) => state.metrics.exchange_custody_balance.store(balance, Ordering::Relaxed),
        Err(e) => {
            state.metrics.exchange_solvency_check_failures.fetch_add(1, Ordering::Relaxed);
            debug!("sweep: could not read the custody balance for metrics: {e}");
        }
    }
}

/// Fetches `task_id` and persists it, logging (rather than failing) on
/// error, and returns the fetched task (if it still exists) so a caller
/// needing it for further work -- e.g. persisting the reputation of
/// every assignee a consensus resolution just touched -- doesn't have to
/// fetch it a second time.
async fn persist_task_by_id(state: &AppState, task_id: Uuid, context: &str) -> Option<Task> {
    let task = state.board.read().await.get_task(task_id).cloned();
    if let Some(task) = &task {
        if let Err(e) = state.store.save_task(task) {
            error!("failed to persist {context} task {task_id}: {e}");
        }
    }
    task
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Args = argh::from_env();
    let node_addresses: Vec<String> = args.node_addresses.split(',').map(|s| s.trim().to_string()).collect();
    let trusted_proxies = rate_limit::parse_trusted_proxies(&args.trusted_proxies)
        .map_err(|e| anyhow::anyhow!("--trusted-proxies is not a list of IP addresses: {e}"))?;

    let operator_private_key = load_or_create_key(&args.operator_key_file)?;
    let operator_public_key = operator_private_key.public_key();
    let exchange_custody_private_key = load_or_create_key(&args.exchange_custody_key_file)?;
    let exchange_custody_public_key = exchange_custody_private_key.public_key();
    let escrow_secret = EscrowSecret::load_or_create(&args.escrow_secret_file)?;
    println!("================================================================");
    println!("hub operator address -- fund this so the hub can pay out tasks/faucet grants:");
    println!("{operator_public_key}");
    println!("exchange custody address -- watch this for solvency (its on-chain balance");
    println!("should always be >= the sum of every ExchangeAccount.base_balance):");
    println!("{exchange_custody_public_key}");
    println!(
        "escrow deposit keys are derived from {} -- back it up; without it every\n\
         escrow address already handed out becomes unsweepable:",
        args.escrow_secret_file
    );
    if trusted_proxies.is_empty() {
        println!("trusting no proxy: rate limiting charges the direct peer and ignores X-Forwarded-For");
    } else {
        let mut listed: Vec<String> = trusted_proxies.iter().map(|ip| ip.to_string()).collect();
        listed.sort();
        println!("trusting X-Forwarded-For only from: {}", listed.join(", "));
    }
    println!("================================================================");

    let store = Arc::new(HubStore::open_or_create(&args.store_file)?);
    let mut board = TaskBoard::new();
    for task in store.load_all_tasks()? {
        board.restore_task(task);
    }
    for (pubkey, reputation) in store.load_all_reputation()? {
        board.restore_reputation(pubkey, reputation);
    }
    for pubkey in store.load_all_faucet_grants()? {
        board.restore_faucet_grant(pubkey);
    }
    for deposit in store.load_all_pending_deposits()? {
        board.restore_pending_deposit(deposit);
    }
    for (pubkey, account) in store.load_all_exchange_accounts()? {
        board.restore_exchange_account(pubkey, account);
    }
    for order in store.load_all_orders()? {
        board.restore_order(order);
    }
    for trade in store.load_all_trades()? {
        board.restore_trade(trade);
    }
    // After the tasks, so every attempt restored here already has the
    // task it belongs to on the board. A payout in flight across a
    // restart is exactly the case this whole mechanism exists for, so
    // the count is printed rather than left silent -- a non-zero one at
    // boot is worth an operator's attention.
    let restored_payouts = {
        let attempts = store.load_all_payout_attempts()?;
        let count = attempts.len();
        for attempt in attempts {
            board.restore_payout_attempt(attempt);
        }
        count
    };
    println!(
        "loaded {} task(s), {} reputation record(s), {} faucet grant(s), {} pending escrow deposit(s), \
         {} exchange account(s), {} order(s), {} trade(s), {} unconfirmed payout(s) from store",
        board.all_tasks().count(),
        board.all_reputation().count(),
        board.all_faucet_grants().count(),
        board.all_pending_deposits().count(),
        board.all_exchange_accounts().count(),
        board.all_orders().count(),
        board.all_trades().count(),
        restored_payouts,
    );

    // Withdrawals whose on-chain leg was submitted and never
    // acknowledged. Nothing resolves these yet, so they are read only to
    // be counted and said out loud: each one is a debited ledger balance
    // whose coin an operator has to account for by hand, and a restart is
    // exactly when somebody is looking (plan §6.5d,
    // `docs/deployment.md` §10.4).
    let unresolved_withdrawals = store.load_all_withdrawal_attempts()?;
    if !unresolved_withdrawals.is_empty() {
        warn!(
            "{} withdrawal(s) submitted and never acknowledged are awaiting review; \
             see docs/deployment.md 10.4",
            unresolved_withdrawals.len()
        );
        for attempt in &unresolved_withdrawals {
            warn!(
                "  withdrawal {} of {} to {} submitted at {}",
                attempt.id, attempt.amount, attempt.owner, attempt.submitted_at
            );
        }
    }
    // Cross-check what just came off disk, before anything is served
    // from it. Every table above was loaded independently and every row
    // blind-inserted, so records that disagree simply arrive
    // disagreeing; until this existed, nothing said so (plan §6.5c).
    //
    // **Reports, does not repair, and does not refuse to start.** That
    // last part is a decision rather than a default, and it is argued
    // in the plan: refusing to boot converts every one of these into an
    // outage, and the ones that can occur are money already moved or
    // money already locked -- none of which a stopped hub makes better,
    // while a stopped hub does stop the sweep that is the only
    // automatic recovery the hub has. So it starts, loudly. Making this
    // fatal is a per-class judgement someone can add later on the back
    // of the metric, which is deliberately emitted for every class
    // including the zeroes so an alert can be written before the first
    // incident.
    let reconciliation = reconcile::reconcile(&board);
    reconciliation.log();

    let mut names = NameRegistry::new();
    for (pubkey, name) in store.load_all_agent_names()? {
        names.restore(pubkey, name);
    }
    // Backfill: every agent the board already knows about but that
    // predates this registry gets named now, in one transaction, rather
    // than trickling in as each one happens to be requested. Idempotent,
    // so a hub that has already been through this does no writes here.
    let unnamed: Vec<PublicKey> = board
        .all_reputation()
        .map(|(pubkey, _)| pubkey.clone())
        .filter(|pubkey| names.get(pubkey).is_none())
        .collect();
    let backfilled: Vec<(PublicKey, String)> = unnamed
        .into_iter()
        .filter_map(|pubkey| names.assign(&pubkey).map(|(name, _)| (pubkey, name)))
        .collect();
    if !backfilled.is_empty() {
        store.save_agent_name_batch(&backfilled)?;
    }
    println!(
        "{} agent name(s) known ({} newly assigned), {} still available",
        names.len(),
        backfilled.len(),
        names.remaining()
    );

    // A hub that cannot read its own replay log still comes up -- just
    // not with a hole in it. The fallback costs two minutes of refused
    // writes and is loud about why.
    // Built before the guard and the node client, because both of them
    // hold their own handle to it and must report into the same table the
    // router will later render.
    let metrics = metrics::Metrics::new();
    // Same reasoning as the reconciliation gauges below: published as
    // soon as the table exists, so a scrape of a freshly started hub
    // already says how many withdrawals it inherited unresolved.
    metrics
        .unresolved_withdrawals_at_boot
        .store(unresolved_withdrawals.len() as u64, std::sync::atomic::Ordering::Relaxed);
    // Published as soon as the table exists, so the first scrape of a
    // freshly started hub already carries the verdict on the store it
    // started from. The reconciliation itself ran earlier -- before
    // anything could be served -- because a boot check that runs after
    // the router is up is a check the first request beats.
    for class in metrics::RECONCILIATION_CLASSES {
        let count = reconciliation
            .findings
            .iter()
            .filter(|finding| finding.kind.label() == class)
            .count() as u64;
        metrics.reconciliation_disagreements.insert(class, count);
    }

    let mut degraded_replay_guard = false;
    let replay_guard = match auth::ReplayGuard::restore(store.clone(), chrono::Utc::now()) {
        Ok((guard, restored)) => {
            println!("restored {restored} replay-guard signature(s) still inside the drift window");
            guard
        }
        Err(e) => {
            error!("could not restore the durable replay guard ({e}) -- falling back to refusing");
            // The figure here used to be `MAX_REQUEST_DRIFT_SECONDS`
            // while the code waited `REPLAY_MEMORY_SECONDS`, so the one
            // message that tells an operator how long the hub will
            // refuse writes told them half of it.
            println!(
                "WARNING: replay log unreadable ({e}); authenticated writes are refused for the \n\
                 next {}s while the post-restart replay window closes. Read routes are \n\
                 unaffected, and signatures are still recorded durably -- what this process \n\
                 lacks is the previous one's history. hub_replay_guard_degraded stays 1 until \n\
                 it is restarted.",
                auth::REPLAY_MEMORY_SECONDS,
            );
            degraded_replay_guard = true;
            auth::ReplayGuard::booting(store.clone(), chrono::Utc::now())
        }
    }
    .with_metrics(metrics.clone());
    // After `with_metrics`, which swaps the guard's own table for the
    // shared one the router renders from.
    metrics
        .replay_guard_degraded
        .store(u64::from(degraded_replay_guard), std::sync::atomic::Ordering::Relaxed);

    // Aborts startup if the table cannot be read, the same as the tasks
    // and faucet grants loaded above and for a sharper reason: a
    // redemption this hub cannot see is a solved challenge it will
    // happily accept a second time. Coming up without it would be coming
    // up with the hole the durable table exists to close.
    let faucet_target = faucet_pow::target_for_expected_hashes(args.faucet_pow_expected_hashes);
    let (faucet_challenges, restored_challenges) =
        faucet_pow::ChallengeBook::restore(store.clone(), faucet_target, chrono::Utc::now())?;
    println!(
        "restored {restored_challenges} faucet challenge(s); a grant costs {} expected hashes",
        args.faucet_pow_expected_hashes
    );

    let state = Arc::new(AppState {
        board: RwLock::new(board),
        store,
        node: NodeClient::new(node_addresses).with_metrics(metrics.clone()),
        operator_private_key,
        operator_public_key,
        payout_lock: Mutex::new(()),
        operator_wallet_outputs: args.operator_wallet_outputs,
        operator_fan_out_inflight: Mutex::new(Vec::new()),
        custody_fan_out_inflight: Mutex::new(Vec::new()),
        exchange_custody_private_key,
        exchange_custody_public_key,
        exchange_custody_payout_lock: Mutex::new(()),
        escrow_secret,
        rate_limits: rate_limit::new_table(),
        trusted_proxies,
        replay_guard,
        faucet_challenges,
        faucet_expected_hashes: args.faucet_pow_expected_hashes,
        names: RwLock::new(names),
        net_worths: RwLock::new(None),
        metrics,
    });

    // Before the listener opens, and awaited rather than spawned. A
    // freshly deployed hub holds one output, which is the single-output
    // wallet plan §6.4b measured at one payout per block -- and the
    // first fan-out costs a block of *no* payouts at all, because its
    // pieces are unconfirmed until mined and it has just spent the only
    // thing that was not. That block is much cheaper to spend here,
    // before the hub is reachable, than under the first burst of
    // arriving agents. Restarting a warm hub does nothing: its wallet
    // is already at the floor and `plan_reshape` returns `None`.
    maintain_wallets_at_boot(&state).await;

    tokio::spawn(sweep_loop(state.clone()));

    let app = build_router(state);

    let addr = format!("{}:{}", args.bind, args.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("hub listening on {addr}");
    if args.bind == "0.0.0.0" {
        println!(
            "  (listening on every interface in cleartext -- behind a proxy, pass --bind 127.0.0.1)"
        );
    }
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    println!("hub stopped accepting connections; in-flight requests have finished");
    Ok(())
}

/// Resolves on the first `SIGTERM` or `SIGINT`, so `axum::serve` can stop
/// accepting new connections and let the ones already in flight finish.
///
/// This is not politeness. The replay guard claims and fsyncs a request's
/// signature *before* its handler runs (`auth::ReplayGuard::claim`), so a
/// request killed mid-handler has already spent its envelope: the client
/// cannot retry it, because the signature is now recorded as used, and
/// must sign a new one. Worse, a claimed envelope whose handler never ran
/// is a request the caller believes failed and the hub has no record of
/// having done -- for a payout or a withdrawal, exactly the ambiguity
/// this codebase works to avoid elsewhere. `systemctl restart` sends
/// `SIGTERM`, so every ordinary deploy hit this.
///
/// Draining does not make a restart free. A request that arrives after
/// the listener closes is refused at the socket, which is a clean failure
/// the client can retry with the same envelope; that is the outcome we
/// want, and the one this produces.
async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};

    let mut terminate = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            error!("could not listen for SIGTERM ({e}); shutdown will not be graceful");
            return std::future::pending().await;
        }
    };
    let mut interrupt = match signal(SignalKind::interrupt()) {
        Ok(s) => s,
        Err(e) => {
            error!("could not listen for SIGINT ({e}); shutdown will not be graceful");
            return std::future::pending().await;
        }
    };

    tokio::select! {
        _ = terminate.recv() => println!("SIGTERM received, draining in-flight requests"),
        _ = interrupt.recv() => println!("SIGINT received, draining in-flight requests"),
    }
}

/// Renders the hub's counters in Prometheus text format.
///
/// Reads atomics and one dashmap, and nothing else -- no board lock, no
/// store transaction, no node round trip. That is a deliberate property
/// rather than an accident of what happened to be easy: this endpoint is
/// the cheapest thing to call on the hub, so anything expensive behind it
/// would make it the most efficient amplifier on the box. The numbers
/// that genuinely need the node or the board are sampled by the sweep and
/// read here as plain integers, at the cost of being up to one sweep
/// interval stale.
///
/// The content type is Prometheus's own `version=0.0.4`, which is what
/// scrapers content-negotiate on; without it some clients fall back to
/// treating the body as an untyped blob.
async fn render_metrics(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let now_unix = chrono::Utc::now().timestamp().max(0) as u64;
    (
        [(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
        state.metrics.render(now_unix),
    )
}

/// Wires up every route against the given state. Pulled out of `main` so
/// tests can stand up the exact same router against a real (ephemeral
/// port) HTTP server, without duplicating the route list.
fn build_router(state: Arc<AppState>) -> Router {
    // GET-only and any-origin: every route this actually unlocks
    // cross-origin (`/tasks`, `/tasks/:id`, `/leaderboard`,
    // `/reputation/:pubkey`, `/llms.txt`) is already read-only and
    // unauthenticated with no cookies/credentials involved, which is what
    // makes a permissive origin safe here. The POST routes stay
    // effectively closed to a cross-origin browser caller regardless --
    // not because CORS blocks them (the browser wouldn't even attempt a
    // disallowed method's actual request past its own preflight check),
    // but because they separately require a valid signed envelope no
    // page-borrowed browser session could forge anyway. Added for the
    // dashboard (`dashboard/`), which talks to the hub from a different
    // origin (its own dev server / static host) via plain `fetch()`.
    // `expose_headers` is what lets a cross-origin `fetch()` actually read
    // `X-Total-Count` off a `/tasks` or `/leaderboard` response. Without it
    // the header is still sent and still visible in devtools, but the
    // browser hides it from JavaScript -- a silent failure that looks like
    // the hub never set it. Response headers are not exposed cross-origin
    // by default; only a short safelist is.
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET])
        .expose_headers([HeaderName::from_static("x-total-count")]);

    Router::new()
        .route("/health", get(handlers::health))
        // Lives here rather than in `handlers` on purpose: every other
        // route in that module reaches the board, the store or the node,
        // and this one must reach none of them (see `metrics`). Keeping
        // it beside the router makes that separation visible, and keeps
        // this pass out of a file another workstream is rewriting.
        .route("/metrics", get(render_metrics))
        .route("/tasks", get(handlers::list_tasks).post(handlers::create_task))
        .route("/tasks/consensus", post(handlers::create_consensus_task))
        .route("/tasks/escrow", post(handlers::create_task_escrow))
        .route("/tasks/consensus/escrow", post(handlers::create_consensus_task_escrow))
        .route("/tasks/disputable/escrow", post(handlers::create_disputable_task_escrow))
        .route("/tasks/escrow/:id/confirm", post(handlers::confirm_task_escrow))
        .route("/tasks/:id", get(handlers::get_task))
        .route("/tasks/:id/claim", post(handlers::claim_task))
        .route("/tasks/:id/submit", post(handlers::submit_task))
        .route("/tasks/:id/cancel", post(handlers::cancel_task))
        .route("/tasks/:id/dispute/escrow", post(handlers::create_dispute_escrow))
        .route("/tasks/:id/dispute/confirm", post(handlers::confirm_dispute_escrow))
        .route("/tasks/:id/dispute/resolve", post(handlers::resolve_dispute))
        .route("/faucet/challenge", post(handlers::faucet_challenge))
        .route("/faucet", post(handlers::faucet_claim))
        .route("/reputation/:pubkey", get(handlers::get_reputation))
        .route("/leaderboard", get(handlers::leaderboard))
        // One request for the whole board's aggregates, so a dashboard
        // does not have to page through every task to compute them (see
        // `board_summary`). Read-only and unauthenticated, like the rest
        // of the board's read surface.
        .route("/board/summary", get(handlers::board_summary))
        // One market's history at a window and resolution the caller
        // picks -- what a chart with range tabs needs, and what the
        // summary's fixed window and 24 buckets deliberately cannot
        // serve. See `handlers::board_series`.
        .route("/board/series", get(handlers::board_series))
        // Batch display-name lookup, so a list of rows costs one request
        // rather than one per row. Read-only and never mints a name --
        // see `handlers::names`.
        .route("/names", get(handlers::names))
        .route("/llms.txt", get(handlers::llms_txt))
        .route("/exchange/deposit", post(handlers::create_exchange_deposit))
        .route("/exchange/deposit/:id/confirm", post(handlers::confirm_exchange_deposit))
        .route("/exchange/orders", get(handlers::get_order_book).post(handlers::place_order))
        .route("/exchange/orders/:id/cancel", post(handlers::cancel_order))
        .route("/exchange/account/:pubkey", get(handlers::get_exchange_account))
        .route("/exchange/withdraw", post(handlers::withdraw))
        .route("/exchange/trades", get(handlers::list_trades))
        .layer(cors)
        .layer(axum::middleware::from_fn_with_state(state.clone(), rate_limit::middleware))
        // Gzip for any client that asks (every browser does). The task
        // list is repeated field names and 66-character hex keys -- the
        // best case for gzip. Outermost, so it compresses the response
        // after every inner layer (including the rate limiter's own
        // responses) has finished shaping it.
        .layer(CompressionLayer::new())
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::TaskStatus;
    use btclib::crypto::Signature;
    use btclib::network::Message;
    use btclib::sha256::Hash;
    use btclib::types::{Transaction, TransactionOutput};
    use chrono::{DateTime, Utc};
    use serde::Serialize;
    use serde_json::{json, Value};
    use std::collections::BTreeSet;
    use tokio::net::TcpListener;
    use tokio::sync::Mutex as AsyncMutex;

    fn temp_store_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("itx_hub_maintest_{}.redb", Uuid::new_v4()))
    }

    /// Fixed `AskChainTip` response every `FakeNode` reports -- there's no
    /// real chain behind it, so this is just a fixed value for `/health`
    /// tests to assert against.
    const FAKE_NODE_CHAIN_HEIGHT: u32 = 7;

    /// What a `FakeNode` does with a transaction it is handed. The three
    /// things a real node can do that the hub cannot tell apart from the
    /// send alone -- which is the entire reason `TaskStatus::Submitted`
    /// exists -- so each is stageable here.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum SubmissionFate {
        /// Accepted and mined: inputs consumed, outputs credited. The
        /// default, and the only one the hub used to assume.
        Mined,
        /// Accepted into a memory-only mempool: the inputs are marked and
        /// nothing is credited yet. Also what a rejected transaction
        /// leaves behind once some *other* transaction has taken the
        /// inputs -- from the hub's side the two are the same picture,
        /// which is why it is the ambiguous row.
        HeldInMempool,
        /// Never arrived. The node is not told, the hub is not told, and
        /// nothing anywhere changes -- a node restart discarding its
        /// mempool, or a rejection that closed the connection.
        Swallowed,
    }

    /// A minimal stand-in for a real node: speaks just enough of the wire
    /// protocol (handshake, `FetchUTXOs`, `SubmitTransaction`, `AskChainTip`)
    /// to drive the hub's real `NodeClient` in tests, without a real
    /// `Blockchain`/`node` process behind it. Reports exactly one
    /// unmarked UTXO of whatever `fund` has set for a given pubkey
    /// (nothing for any other), and records everything submitted to it.
    /// Funding can be added any time after spawning, not just up front --
    /// needed for escrow tests, where the address to fund is generated by
    /// the hub itself and isn't known until after reserving an escrow,
    /// well after this fake server is already running.
    struct FakeNode {
        addr: String,
        submitted: Arc<AsyncMutex<Vec<Transaction>>>,
        /// The unspent outputs this node reports, and whether its
        /// mempool has spoken for each -- a real set rather than a
        /// balance rendered into a throwaway output on every read.
        ///
        /// It has to be a real set now, because settlement confirmation
        /// resolves an *output hash* against what an address holds (see
        /// `board::PayoutAttempt`). A node that minted a fresh
        /// `unique_id` per `FetchUTXOs` could never confirm anything: the
        /// same money would hash differently every time it was looked at.
        utxos: Arc<AsyncMutex<Vec<(TransactionOutput, bool)>>>,
        /// What a submitted transaction does to `utxos`. See
        /// `SubmissionFate`.
        fate: Arc<AsyncMutex<SubmissionFate>>,
        /// Every `SubmitTransaction` this node has *handled*, counted
        /// before its fate is consulted -- unlike `submitted`, which only
        /// records the ones it kept.
        ///
        /// A test staging a lost submission has nothing else to wait on:
        /// the send is fire-and-forget, so it returns before the node has
        /// read the bytes, and a swallowed transaction leaves no other
        /// trace that it arrived. Without this, changing `fate` after
        /// "sending" races the node's own accept loop and can flip the
        /// fate of the very message under test -- which is not a
        /// hypothetical, it is how this fake first lied to a test.
        submissions_seen: Arc<std::sync::atomic::AtomicUsize>,
        /// How many TCP connections this fake has accepted over its
        /// lifetime. The figure the connection-pooling tests assert on:
        /// with a pool, a run of operations should cost far fewer
        /// connections than it has operations.
        connections: Arc<std::sync::atomic::AtomicUsize>,
        /// When set, each connection serves exactly one message and then
        /// closes -- standing in for a node that dropped a pooled
        /// connection while it sat idle, which is the case a pooled
        /// client has to notice and recover from.
        hang_up_after_one: Arc<std::sync::atomic::AtomicBool>,
        /// When set, the next `FetchUTXOs` answers truthfully and *then*
        /// empties the set, clearing itself.
        ///
        /// The only way to stage a wallet that drains between two reads,
        /// which is what a burst does to the operator: a handler that
        /// checks the balance and then builds a payment sees two
        /// different wallets, and the second one is the one that fails.
        /// Racing two real requests would be the alternative, and it
        /// would be a test that passes for whichever reason it felt
        /// like that run.
        drain_after_next_fetch: Arc<std::sync::atomic::AtomicBool>,
    }

    impl FakeNode {
        async fn spawn_empty() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
            let submitted = Arc::new(AsyncMutex::new(Vec::new()));
            let utxos: Arc<AsyncMutex<Vec<(TransactionOutput, bool)>>> =
                Arc::new(AsyncMutex::new(Vec::new()));
            let fate = Arc::new(AsyncMutex::new(SubmissionFate::Mined));
            let submissions_seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let connections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let hang_up_after_one = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let drain_after_next_fetch = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let submitted_for_accept_loop = submitted.clone();
            let utxos_for_accept_loop = utxos.clone();
            let fate_for_accept_loop = fate.clone();
            let seen_for_accept_loop = submissions_seen.clone();
            let connections_for_accept_loop = connections.clone();
            let hang_up_for_accept_loop = hang_up_after_one.clone();
            let drain_for_accept_loop = drain_after_next_fetch.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        return;
                    };
                    connections_for_accept_loop.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let submitted = submitted_for_accept_loop.clone();
                    let utxos = utxos_for_accept_loop.clone();
                    let fate = fate_for_accept_loop.clone();
                    let submissions_seen = seen_for_accept_loop.clone();
                    let hang_up_after_one = hang_up_for_accept_loop.clone();
                    let drain = drain_for_accept_loop.clone();
                    tokio::spawn(async move {
                        if btclib::network::perform_handshake_acceptor(&mut socket)
                            .await
                            .is_err()
                        {
                            return;
                        }
                        loop {
                            let message = match Message::receive_async(&mut socket).await {
                                Ok(m) => m,
                                Err(_) => return,
                            };
                            match message {
                                Message::FetchUTXOs(pk) => {
                                    let owned: Vec<(TransactionOutput, bool)> = utxos
                                        .lock()
                                        .await
                                        .iter()
                                        .filter(|(output, _)| output.pubkey == pk)
                                        .cloned()
                                        .collect();
                                    // Drained after the answer is
                                    // composed, so this read is honest
                                    // and the next one is not -- see
                                    // `drain_after_next_fetch`.
                                    if drain.swap(false, std::sync::atomic::Ordering::SeqCst) {
                                        utxos.lock().await.retain(|(output, _)| output.pubkey != pk);
                                    }
                                    if Message::UTXOs(owned).send_async(&mut socket).await.is_err()
                                    {
                                        return;
                                    }
                                }
                                Message::SubmitTransaction(tx) => {
                                    // fire-and-forget, matching the real protocol
                                    submissions_seen
                                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                    let fate = *fate.lock().await;
                                    if fate == SubmissionFate::Swallowed {
                                        // The node never got it: not
                                        // recorded, and the UTXO set is
                                        // untouched, which is exactly
                                        // what a lost submission looks
                                        // like from the hub's side.
                                        continue;
                                    }
                                    submitted.lock().await.push(tx.clone());
                                    let mut utxos = utxos.lock().await;
                                    match fate {
                                        SubmissionFate::Mined => {
                                            utxos.retain(|(output, _)| {
                                                !tx.inputs.iter().any(|input| {
                                                    input.prev_transaction_output_hash == output.hash()
                                                })
                                            });
                                            utxos.extend(
                                                tx.outputs.iter().cloned().map(|o| (o, false)),
                                            );
                                        }
                                        SubmissionFate::HeldInMempool => {
                                            for (output, marked) in utxos.iter_mut() {
                                                if tx.inputs.iter().any(|input| {
                                                    input.prev_transaction_output_hash == output.hash()
                                                }) {
                                                    *marked = true;
                                                }
                                            }
                                        }
                                        SubmissionFate::Swallowed => unreachable!("handled above"),
                                    }
                                }
                                Message::AskChainTip => {
                                    let tip = Message::ChainTip(FAKE_NODE_CHAIN_HEIGHT, btclib::U256::from(1u64));
                                    if tip.send_async(&mut socket).await.is_err() {
                                        return;
                                    }
                                }
                                _ => return,
                            }
                            if hang_up_after_one.load(std::sync::atomic::Ordering::Relaxed) {
                                return;
                            }
                        }
                    });
                }
            });
            FakeNode {
                addr,
                submitted,
                utxos,
                fate,
                submissions_seen,
                connections,
                hang_up_after_one,
                drain_after_next_fetch,
            }
        }

        /// How many TCP connections this fake has accepted so far.
        fn connections_accepted(&self) -> usize {
            self.connections.load(std::sync::atomic::Ordering::Relaxed)
        }

        /// Makes every connection from here on serve one message and then
        /// close, the way a real node's connection would eventually be
        /// dropped while a pooled client was not using it.
        fn hang_up_after_every_exchange(&self) {
            self.hang_up_after_one.store(true, std::sync::atomic::Ordering::Relaxed);
        }

        /// Convenience constructor for the single-funded-pubkey case every
        /// pre-existing test uses.
        async fn spawn(funded_pubkey: PublicKey, balance: u64) -> Self {
            let node = Self::spawn_empty().await;
            node.fund(funded_pubkey, balance).await;
            node
        }

        /// Sets (or replaces) the balance `FetchUTXOs` reports for
        /// `pubkey`, as one unmarked output. Callable any time after
        /// spawning -- escrow tests need it, since the address to fund is
        /// minted by the hub long after this server is running.
        async fn fund(&self, pubkey: PublicKey, balance: u64) {
            let mut utxos = self.utxos.lock().await;
            utxos.retain(|(output, _)| output.pubkey != pubkey);
            utxos.push((
                TransactionOutput { value: balance, unique_id: Uuid::new_v4(), pubkey },
                false,
            ));
        }

        /// Adds one more unmarked output to `pubkey`, leaving whatever
        /// it already holds. `fund` replaces, which is what almost every
        /// test wants; a wallet's *shape* -- how many outputs and of
        /// what sizes -- is the whole subject of the operator fan-out,
        /// and cannot be staged by replacement.
        async fn credit(&self, pubkey: PublicKey, value: u64) {
            self.utxos.lock().await.push((
                TransactionOutput { value, unique_id: Uuid::new_v4(), pubkey },
                false,
            ));
        }

        /// Waits until this node has *handled* `expected` submissions,
        /// whatever it did with them. The thing to wait on before
        /// changing `fate`, or before asserting that a transaction was
        /// swallowed -- `wait_for_submitted_count` cannot serve either,
        /// since a swallowed transaction never reaches `submitted`.
        async fn wait_for_submissions_seen(&self, expected: usize) {
            for _ in 0..200 {
                if self.submissions_seen.load(std::sync::atomic::Ordering::SeqCst) >= expected {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            panic!(
                "the node never saw {expected} submission(s); it saw {}",
                self.submissions_seen.load(std::sync::atomic::Ordering::SeqCst)
            );
        }

        /// Arms the one-shot drain -- see `drain_after_next_fetch`.
        fn drain_after_the_next_fetch(&self) {
            self.drain_after_next_fetch.store(true, std::sync::atomic::Ordering::SeqCst);
        }

        /// What this node does with the next transaction it is handed --
        /// see `SubmissionFate`. The default is `Mined`.
        async fn set_fate(&self, fate: SubmissionFate) {
            *self.fate.lock().await = fate;
        }

        /// Every unspent output `pubkey` currently holds, newest last.
        async fn outputs_of(&self, pubkey: &PublicKey) -> Vec<(TransactionOutput, bool)> {
            self.utxos
                .lock()
                .await
                .iter()
                .filter(|(output, _)| output.pubkey == *pubkey)
                .cloned()
                .collect()
        }

        /// What `NodeClient::balance` would report for `pubkey`:
        /// everything the mempool has not spoken for.
        async fn balance_of(&self, pubkey: &PublicKey) -> u64 {
            self.outputs_of(pubkey)
                .await
                .iter()
                .filter(|(_, marked)| !marked)
                .map(|(output, _)| output.value)
                .sum()
        }

        async fn submitted_transactions(&self) -> Vec<Transaction> {
            self.submitted.lock().await.clone()
        }

        /// `NodeClient::submit_transaction` is fire-and-forget (the real
        /// wire protocol has no acknowledgement for it -- see its own
        /// doc comment), so a caller observing success only knows the
        /// bytes were handed to the OS socket, not that this fake node's
        /// own async accept/receive loop has gotten around to recording
        /// them yet. Polls briefly instead of asserting on
        /// `submitted_transactions` immediately after a settle call.
        async fn wait_for_submitted_count(&self, expected: usize) -> Vec<Transaction> {
            for _ in 0..100 {
                let submitted = self.submitted_transactions().await;
                if submitted.len() >= expected {
                    return submitted;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            self.submitted_transactions().await
        }
    }

    /// Runs the sweep's resolution pass over every payout currently in
    /// flight, so a test can reach the settled end state.
    ///
    /// Settlement is two steps now: the hub submits, and a later sweep
    /// works out what became of it. A test that only submits leaves the
    /// task `Submitted`, which is correct and is the point -- reputation,
    /// compute credit and `Paid` all wait on evidence.
    ///
    /// It waits for the fake node to hold the output before resolving,
    /// because `submit_transaction` is fire-and-forget and returns before
    /// the node has read the bytes. Asked in that window the hub would be
    /// *right* to call the payout lost; the real sweep buys the same
    /// margin with `PAYOUT_RESOLUTION_GRACE_SECONDS`.
    async fn confirm_submitted_payouts(state: &Arc<AppState>, node: &FakeNode) {
        let outstanding = state.board.read().await.outstanding_payout_attempts();
        for attempt in outstanding {
            for _ in 0..200 {
                let landed = node
                    .outputs_of(&attempt.recipient)
                    .await
                    .iter()
                    .any(|(output, _)| output.hash() == attempt.output_hash);
                if landed {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            handlers::resolve_payout_attempt(state, &attempt).await;
        }
    }

    /// An address guaranteed to have nothing listening on it right now --
    /// for tests that need a deterministically-unreachable "node" rather
    /// than racing against a real process's timing.
    async fn dead_address() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        format!("127.0.0.1:{}", listener.local_addr().unwrap().port())
        // `listener` drops here, freeing the port right back up -- nothing
        // is bound to it again within this test's lifetime.
    }

    /// `NodeClient` must fail over to the next configured address when the
    /// first is unreachable -- the whole point of giving it more than one
    /// (see its own doc comment on why this is ordered failover, not
    /// load-balancing).
    #[tokio::test]
    async fn node_client_fails_over_to_the_next_address_when_the_first_is_unreachable() {
        let agent_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(agent_key.public_key(), 12_345).await;
        let dead = dead_address().await;

        let client = NodeClient::new(vec![dead, fake_node.addr.clone()]);
        let balance = client.balance(&agent_key.public_key()).await.unwrap();
        assert_eq!(balance, 12_345);

        let height = client.chain_tip().await.unwrap();
        assert_eq!(height, FAKE_NODE_CHAIN_HEIGHT);
    }

    /// All addresses unreachable must surface as an error, not hang or
    /// silently report a wrong answer.
    #[tokio::test]
    async fn node_client_fails_when_every_address_is_unreachable() {
        let client = NodeClient::new(vec![dead_address().await, dead_address().await]);
        assert!(client.chain_tip().await.is_err());
    }

    /// The change this pooling exists for: a run of operations should not
    /// cost a TCP connection and a handshake apiece.
    #[tokio::test]
    async fn sequential_operations_share_one_pooled_connection() {
        let agent_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(agent_key.public_key(), 12_345).await;
        let client = NodeClient::new(vec![fake_node.addr.clone()]);

        for _ in 0..10 {
            assert_eq!(client.balance(&agent_key.public_key()).await.unwrap(), 12_345);
        }
        client.chain_tip().await.unwrap();

        assert_eq!(
            fake_node.connections_accepted(),
            1,
            "eleven operations in sequence must reuse one connection, not open eleven"
        );
    }

    /// The pool is also a ceiling. Before it, every concurrent request
    /// opened its own socket to the node; only the net-worth sweep was
    /// bounded, and only against itself.
    #[tokio::test]
    async fn a_concurrent_fan_out_opens_no_more_connections_than_the_pool_allows() {
        let agent_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(agent_key.public_key(), 500).await;
        let client = NodeClient::with_pool_size(vec![fake_node.addr.clone()], 4);

        let mut lookups = tokio::task::JoinSet::new();
        for _ in 0..50 {
            let client = client.clone();
            let pubkey = agent_key.public_key();
            lookups.spawn(async move { client.balance(&pubkey).await });
        }
        while let Some(result) = lookups.join_next().await {
            assert_eq!(result.unwrap().unwrap(), 500);
        }

        assert!(
            fake_node.connections_accepted() <= 4,
            "a pool of 4 must never open more than 4 connections, opened {}",
            fake_node.connections_accepted()
        );
    }

    /// The failure this design is really guarding against. Two operations
    /// sharing a socket must not read each other's replies -- a caller
    /// that got the wrong agent's balance would be told a wrong number
    /// with no error anywhere to show for it. Distinct balances per key
    /// are what make a misattributed reply visible.
    #[tokio::test]
    async fn concurrent_lookups_on_a_shared_pool_each_receive_their_own_answer() {
        let fake_node = FakeNode::spawn_empty().await;
        let mut agents = Vec::new();
        for i in 0..20u64 {
            let key = PrivateKey::new_key();
            let balance = 1_000 + i;
            fake_node.fund(key.public_key(), balance).await;
            agents.push((key.public_key(), balance));
        }
        // Deliberately fewer connections than callers, so every caller
        // after the first few is reusing a socket someone else just
        // finished with.
        let client = NodeClient::with_pool_size(vec![fake_node.addr.clone()], 3);

        let mut lookups = tokio::task::JoinSet::new();
        for (pubkey, expected) in agents {
            let client = client.clone();
            lookups.spawn(async move {
                let seen = client.balance(&pubkey).await.unwrap();
                (expected, seen)
            });
        }
        while let Some(result) = lookups.join_next().await {
            let (expected, seen) = result.unwrap();
            assert_eq!(
                seen, expected,
                "a lookup received another lookup's answer -- replies are being misattributed"
            );
        }
    }

    /// A pooled connection is only known to be dead once it is used. The
    /// client must notice and retry on a fresh one rather than surfacing
    /// the node's hangup as a failure to its caller.
    #[tokio::test]
    async fn a_connection_the_node_closed_while_idle_is_retried_not_reported() {
        let agent_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(agent_key.public_key(), 7_777).await;
        fake_node.hang_up_after_every_exchange();
        let client = NodeClient::new(vec![fake_node.addr.clone()]);

        // Each of these pools a connection the fake node has already
        // closed behind it, so every call after the first starts by
        // failing on a dead socket.
        for _ in 0..5 {
            assert_eq!(
                client.balance(&agent_key.public_key()).await.unwrap(),
                7_777,
                "a stale pooled connection must be replaced, not returned as an error"
            );
        }
    }

    /// The same hazard on the write path, where it is far worse. A read
    /// notices a dead pooled socket because the reply never comes;
    /// `submit_transaction` is fire-and-forget, so nothing comes back
    /// either way, and a write into a closed socket lands in the kernel
    /// buffer and returns `Ok`. Pooled, this reported success for
    /// transactions the node never received, and the hub records a
    /// successful submit as a completed payout -- money gone with no
    /// error anywhere. The node closes connections on every restart and
    /// after every rejected transaction, so this was not a rare case.
    ///
    /// `hang_up_after_every_exchange` is exactly that node: it serves one
    /// message per connection and closes. Every submission after the
    /// first would therefore start on a socket the node has already
    /// closed, if the client pooled them.
    #[tokio::test]
    async fn a_submit_is_never_reported_sent_on_a_connection_the_node_has_closed() {
        let agent_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(agent_key.public_key(), 1).await;
        fake_node.hang_up_after_every_exchange();
        let client = NodeClient::new(vec![fake_node.addr.clone()]);

        let mut reported_sent = 0usize;
        for _ in 0..4 {
            if client.submit_transaction(Transaction::new(vec![], vec![])).await.is_ok() {
                reported_sent += 1;
            }
        }
        assert_eq!(reported_sent, 4, "the node was up throughout, so every send should report success");

        let received = fake_node.wait_for_submitted_count(reported_sent).await;
        assert_eq!(
            received.len(),
            reported_sent,
            "every submission the client reported as sent must have reached the node"
        );
    }

    /// Failover still works with a pool in the way, and the client that
    /// failed over settles on a single connection to the node that
    /// answered rather than redialling the dead one every time.
    ///
    /// The related invariant -- that a pool never holds connections to
    /// two nodes at once, which is what would turn ordered failover into
    /// load-balancing -- is checked directly on `Pool` in
    /// `node_client::tests`, where the two addresses can be driven
    /// without needing a node to die on cue.
    #[tokio::test]
    async fn a_failed_over_client_settles_on_one_connection_to_the_surviving_node() {
        let agent_key = PrivateKey::new_key();
        let primary = FakeNode::spawn(agent_key.public_key(), 100).await;
        let secondary = FakeNode::spawn(agent_key.public_key(), 200).await;

        // Start on the primary and pool a connection to it.
        let client = NodeClient::new(vec![primary.addr.clone(), secondary.addr.clone()]);
        assert_eq!(client.balance(&agent_key.public_key()).await.unwrap(), 100);
        assert_eq!(primary.connections_accepted(), 1);

        // A second client standing in for "the primary is now gone":
        // same pool, but the primary address no longer answers.
        let failed_over = NodeClient::new(vec![dead_address().await, secondary.addr.clone()]);
        assert_eq!(failed_over.balance(&agent_key.public_key()).await.unwrap(), 200);
        for _ in 0..5 {
            assert_eq!(failed_over.balance(&agent_key.public_key()).await.unwrap(), 200);
        }
        assert_eq!(
            secondary.connections_accepted(),
            1,
            "after failover the pool must settle on one connection to the surviving node"
        );
    }

    struct TestHub {
        base_url: String,
        state: Arc<AppState>,
        operator_key: PrivateKey,
        client: reqwest::Client,
        store_path: std::path::PathBuf,
    }

    impl Drop for TestHub {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.store_path);
        }
    }

    /// Eight expected hashes: enough that the first nonce tried is
    /// usually not a hit, so the solving path is genuinely exercised,
    /// and cheap enough to be invisible in a test run.
    const TEST_FAUCET_EXPECTED_HASHES: u64 = 8;

    async fn spawn_hub(operator_private_key: PrivateKey, node_address: String) -> TestHub {
        spawn_hub_with_trusted_proxies(operator_private_key, node_address, rate_limit::TrustedProxies::new()).await
    }

    /// `spawn_hub`, but with the rate limiter told to believe some peer's
    /// `X-Forwarded-For`. Every test hub connects over the loopback, so a
    /// test that wants to exercise the proxied path trusts `127.0.0.1`;
    /// the default (`spawn_hub`) trusts nothing, which is also the
    /// deployed default.
    async fn spawn_hub_with_trusted_proxies(
        operator_private_key: PrivateKey,
        node_address: String,
        trusted_proxies: rate_limit::TrustedProxies,
    ) -> TestHub {
        let operator_public_key = operator_private_key.public_key();
        let store_path = temp_store_path();
        let store = Arc::new(HubStore::open_or_create(&store_path).unwrap());
        let exchange_custody_private_key = PrivateKey::new_key();
        let exchange_custody_public_key = exchange_custody_private_key.public_key();
        // Restored rather than `open()` so every authenticated request
        // in the suite goes through the durable write path too, not just
        // the tests that are about it.
        let replay_guard = auth::ReplayGuard::restore(store.clone(), Utc::now()).unwrap().0;
        // Test hubs get a difficulty a solver clears in a handful of
        // tries. The production default is calibrated for a Python
        // client and would put a minute of CPU inside every test that
        // touches the faucet.
        let faucet_challenges = faucet_pow::ChallengeBook::restore(
            store.clone(),
            faucet_pow::target_for_expected_hashes(TEST_FAUCET_EXPECTED_HASHES),
            Utc::now(),
        )
        .unwrap()
        .0;
        // The same table the guard and the node client report into, so a
        // test can drive a request and then assert on the counter it
        // moved -- wiring these separately would give three tables and a
        // test that silently asserts on an empty one.
        let metrics = metrics::Metrics::new();
        let state = Arc::new(AppState {
            board: RwLock::new(TaskBoard::new()),
            store,
            node: NodeClient::new(vec![node_address]).with_metrics(metrics.clone()),
            operator_private_key: operator_private_key.clone(),
            operator_public_key,
            payout_lock: Mutex::new(()),
            operator_wallet_outputs: operator_wallet::DEFAULT_WALLET_OUTPUTS,
            operator_fan_out_inflight: Mutex::new(Vec::new()),
        custody_fan_out_inflight: Mutex::new(Vec::new()),
            exchange_custody_private_key,
            exchange_custody_public_key,
            exchange_custody_payout_lock: Mutex::new(()),
            escrow_secret: EscrowSecret::generate(),
            rate_limits: rate_limit::new_table(),
            trusted_proxies,
            replay_guard: replay_guard.with_metrics(metrics.clone()),
            faucet_challenges,
            faucet_expected_hashes: TEST_FAUCET_EXPECTED_HASHES,
            names: RwLock::new(NameRegistry::new()),
            net_worths: RwLock::new(None),
            metrics,
        });

        let app = build_router(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await;
        });

        TestHub {
            base_url,
            state,
            operator_key: operator_private_key,
            client: reqwest::Client::new(),
            store_path,
        }
    }

    /// Hand-rolls the signing recipe rather than calling `sdk`, on
    /// purpose: it is a second independent implementation, so these tests
    /// also catch the hub and the SDK drifting apart. `path` is bound in
    /// alongside the method the same way a real client must bind it.
    fn envelope_at<T: Serialize>(
        key: &PrivateKey,
        path: &str,
        payload: T,
        timestamp: DateTime<Utc>,
    ) -> Value {
        let pubkey_hex = key.public_key().to_string();
        let timestamp_str = timestamp.to_rfc3339();
        let payload_json = serde_json::to_string(&payload).unwrap();
        let signing_string = format!("{pubkey_hex}:{timestamp_str}:POST {path}:{payload_json}");
        let hash = Hash::hash_bytes(signing_string.as_bytes());
        let signature = Signature::sign_hash(&hash, key);
        json!({
            "pubkey": pubkey_hex,
            "timestamp": timestamp_str,
            "payload": payload,
            "signature": hex::encode(signature.to_bytes()),
        })
    }

    fn envelope<T: Serialize>(key: &PrivateKey, path: &str, payload: T) -> Value {
        envelope_at(key, path, payload, Utc::now())
    }

    /// Drives `board` directly (bypassing HTTP entirely) through
    /// create-claim-submit-correct-answer, landing a task in `Verified`
    /// status with a known claimant. Used by tests that exercise payout
    /// settlement specifically and don't want task *creation*'s node-
    /// dependent balance check in the way.
    async fn seed_verified_task(state: &AppState, bounty: u64) -> (Uuid, PublicKey) {
        let claimant = PrivateKey::new_key().public_key();
        let expected = Hash::hash_bytes(b"seeded answer");
        let task_id = {
            let mut board = state.board.write().await;
            let task = board.create_task(
                state.operator_public_key.clone(),
                "seeded".to_string(),
                bounty,
                expected,
            );
            board
                .claim_task(task.id, claimant.clone(), Utc::now() + chrono::Duration::minutes(5))
                .unwrap();
            assert!(board.submit(task.id, claimant.clone(), expected).unwrap());
            task.id
        };
        (task_id, claimant)
    }

    /// Walks the faucet's two-step flow: ask for a challenge, solve it,
    /// redeem it. Returns the redemption response so a caller can assert
    /// on the status.
    ///
    /// Every faucet test goes through here rather than posting to
    /// `/faucet` directly, which is the point: the flow is now two
    /// signed calls and a proof of work, and a test that shortcuts it
    /// would stop being a test of what agents actually do.
    async fn claim_faucet(hub: &TestHub, key: &PrivateKey) -> reqwest::Response {
        let challenge = request_faucet_challenge(hub, key).await;
        redeem_faucet_challenge(hub, key, &challenge).await
    }

    /// The first leg on its own, for tests that need the challenge
    /// itself rather than just the grant.
    async fn request_faucet_challenge(hub: &TestHub, key: &PrivateKey) -> Value {
        let resp = hub
            .client
            .post(format!("{}/faucet/challenge", hub.base_url))
            .json(&envelope(key, "/faucet/challenge", ()))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK, "challenge issuance");
        resp.json().await.unwrap()
    }

    /// Solves `challenge` the way a client does -- rebuilding the
    /// preimage from the wire fields rather than from the hub's own
    /// `Challenge` type, so this exercises the same reconstruction an
    /// SDK has to get right.
    fn solve_faucet_challenge(challenge: &Value) -> u64 {
        let template = challenge["preimage_template"].as_str().unwrap();
        let target = btclib::U256::from_str_radix(challenge["target"].as_str().unwrap(), 16).unwrap();
        (0u64..)
            .find(|n| {
                Hash::hash_bytes(template.replace("{solution}", &n.to_string()).as_bytes())
                    .matches_target(target)
            })
            .unwrap()
    }

    async fn redeem_faucet_challenge(
        hub: &TestHub,
        key: &PrivateKey,
        challenge: &Value,
    ) -> reqwest::Response {
        let payload = handlers::FaucetClaimPayload {
            challenge_id: challenge["challenge_id"].as_str().unwrap().parse().unwrap(),
            solution: solve_faucet_challenge(challenge),
        };
        hub.client
            .post(format!("{}/faucet", hub.base_url))
            .json(&envelope(key, "/faucet", payload))
            .send()
            .await
            .unwrap()
    }


    /// The happy path, end to end over HTTP: ask, solve, get paid.
    #[tokio::test]
    async fn a_solved_challenge_earns_the_faucet_grant() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let agent = PrivateKey::new_key();

        let challenge = request_faucet_challenge(&hub, &agent).await;
        assert_eq!(challenge["action"], "faucet");
        assert_eq!(challenge["pubkey"], agent.public_key().to_string());
        assert!(challenge["preimage_template"].as_str().unwrap().ends_with(":{solution}"));

        let resp = redeem_faucet_challenge(&hub, &agent, &challenge).await;
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
    }

    /// The work has to be real. A claim carrying a nonce that does not
    /// meet the target is refused, which is the whole point of the
    /// exercise -- without this the endpoint is the old unpriced faucet
    /// with extra steps.
    #[tokio::test]
    async fn an_unsolved_claim_is_refused() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let agent = PrivateKey::new_key();

        let challenge = request_faucet_challenge(&hub, &agent).await;
        let good = solve_faucet_challenge(&challenge);
        // A nonce that is definitely not the one found above. Adding one
        // could land on another solution at this easy difficulty, so the
        // test asserts on a value it has checked is wrong.
        let bad = (0u64..)
            .filter(|n| *n != good)
            .find(|n| {
                let template = challenge["preimage_template"].as_str().unwrap();
                let target =
                    btclib::U256::from_str_radix(challenge["target"].as_str().unwrap(), 16).unwrap();
                !Hash::hash_bytes(template.replace("{solution}", &n.to_string()).as_bytes())
                    .matches_target(target)
            })
            .unwrap();

        let payload = handlers::FaucetClaimPayload {
            challenge_id: challenge["challenge_id"].as_str().unwrap().parse().unwrap(),
            solution: bad,
        };
        let resp = hub
            .client
            .post(format!("{}/faucet", hub.base_url))
            .json(&envelope(&agent, "/faucet", payload))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    }

    /// Work done for one key must not pay another. This is the property
    /// that stops one miner farming solutions for a swarm, and it is
    /// checked twice by construction -- the pubkey is inside the hash
    /// and the challenge is bound to it server-side -- so the test
    /// presents a genuinely solved challenge under the wrong signature
    /// and expects the server-side binding to catch it.
    #[tokio::test]
    async fn one_keys_solution_cannot_be_redeemed_by_another_key() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let alice = PrivateKey::new_key();
        let bob = PrivateKey::new_key();

        let for_alice = request_faucet_challenge(&hub, &alice).await;
        let payload = handlers::FaucetClaimPayload {
            challenge_id: for_alice["challenge_id"].as_str().unwrap().parse().unwrap(),
            solution: solve_faucet_challenge(&for_alice),
        };
        let resp = hub
            .client
            .post(format!("{}/faucet", hub.base_url))
            .json(&envelope(&bob, "/faucet", payload))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::FORBIDDEN,
            "bob must not spend alice's work"
        );
    }

    /// One challenge, one grant. The second redemption is refused for
    /// being spent rather than for the pubkey already having a grant, so
    /// the test uses a key whose grant was rolled back -- otherwise both
    /// guards would fire and it would not be clear which one did.
    #[tokio::test]
    async fn a_challenge_cannot_be_redeemed_twice() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let agent = PrivateKey::new_key();

        let challenge = request_faucet_challenge(&hub, &agent).await;
        assert_eq!(
            redeem_faucet_challenge(&hub, &agent, &challenge).await.status(),
            reqwest::StatusCode::OK
        );

        // Clear the grant so the faucet's own once-per-key rule cannot
        // be what refuses the second attempt.
        hub.state.board.write().await.revoke_faucet_grant(&agent.public_key());

        let resp = redeem_faucet_challenge(&hub, &agent, &challenge).await;
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::CONFLICT,
            "a spent challenge must be refused as spent"
        );
    }

    /// The whole point of the pre-flight. An agent that solves a puzzle
    /// and finds the hub broke must not have to solve another one:
    /// the same solution has to still be worth presenting.
    #[tokio::test]
    async fn a_grant_the_operator_cannot_fund_spends_no_work() {
        let operator_key = PrivateKey::new_key();
        // Under a grant plus its fee, which is the shortage that used to
        // burn the challenge and answer with a 500.
        let fake_node = FakeNode::spawn(operator_key.public_key(), 1_000).await;
        let hub = spawn_hub(operator_key.clone(), fake_node.addr.clone()).await;
        let agent = PrivateKey::new_key();

        let challenge = request_faucet_challenge(&hub, &agent).await;
        let resp = redeem_faucet_challenge(&hub, &agent, &challenge).await;
        assert_eq!(resp.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            resp.headers().get(reqwest::header::RETRY_AFTER).unwrap(),
            &btclib::IDEAL_BLOCK_TIME.to_string(),
            "an agent told to come back needs to be told when"
        );
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["retry_after_seconds"], btclib::IDEAL_BLOCK_TIME);
        assert!(
            body.get("challenge").is_none(),
            "replacing an unspent challenge would evict the one the agent already solved"
        );

        // Fund the operator and present the *same* solution again. It
        // has to be accepted, or the pre-flight bought nothing.
        fake_node.fund(operator_key.public_key(), 15_000_000_000).await;
        let resp = redeem_faucet_challenge(&hub, &agent, &challenge).await;
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::OK,
            "the solution was never spent, so it must still be good"
        );
    }

    /// The pre-flight must not answer for the challenge. A spent
    /// challenge is a 409 whatever the operator's wallet happens to
    /// hold -- otherwise a busy hub tells a client to retry work that
    /// will never be accepted, and the 503 it sends says in as many
    /// words that nothing was spent, which would be a lie.
    #[tokio::test]
    async fn a_spent_challenge_is_refused_as_spent_even_when_the_hub_cannot_pay() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 1_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let agent = PrivateKey::new_key();

        let challenge = request_faucet_challenge(&hub, &agent).await;
        // Spend it out from under the handler, so the only thing left
        // to refuse it for is the redemption -- the wallet is short
        // either way.
        hub.state
            .faucet_challenges
            .redeem(
                challenge["challenge_id"].as_str().unwrap().parse().unwrap(),
                &agent.public_key(),
                solve_faucet_challenge(&challenge),
                Utc::now(),
            )
            .unwrap();

        assert_eq!(
            redeem_faucet_challenge(&hub, &agent, &challenge).await.status(),
            reqwest::StatusCode::CONFLICT
        );
    }

    /// The race the pre-flight cannot close: it said yes, and the
    /// payment failed anyway. The challenge is spent and cannot be
    /// unspent, so the hub owes a fresh one -- and it has to be a
    /// challenge the agent can actually solve and redeem.
    #[tokio::test]
    async fn a_grant_that_fails_after_redemption_hands_back_a_fresh_challenge() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 15_000_000_000).await;
        let hub = spawn_hub(operator_key.clone(), fake_node.addr.clone()).await;
        let agent = PrivateKey::new_key();

        let challenge = request_faucet_challenge(&hub, &agent).await;
        // The pre-flight's read is answered honestly and the wallet is
        // gone by the time the payment builds -- exactly the window a
        // burst opens, staged rather than raced.
        fake_node.drain_after_the_next_fetch();

        let resp = redeem_faucet_challenge(&hub, &agent, &challenge).await;
        assert_eq!(resp.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
        let body: Value = resp.json().await.unwrap();
        let replacement = body["challenge"].clone();
        assert!(replacement.is_object(), "a spent challenge is owed a replacement");
        assert_eq!(replacement["pubkey"], agent.public_key().to_string());

        // The grant was rolled back too, so the replacement is usable.
        fake_node.fund(operator_key.public_key(), 15_000_000_000).await;
        assert_eq!(
            redeem_faucet_challenge(&hub, &agent, &replacement).await.status(),
            reqwest::StatusCode::OK,
            "the replacement has to be solvable and redeemable, not just present"
        );
    }

    /// Custody gets the same treatment, and gets it independently.
    ///
    /// The ceiling was never the operator's alone: every withdrawal
    /// spends a custody output and returns its change unconfirmed, so a
    /// one-output custody wallet served one withdrawal per block. The
    /// ordering half of the fix already applied here -- `pay_from`
    /// orders candidates for every funding source -- but with a single
    /// output there was nothing to order.
    ///
    /// The second half of this test is the part worth having: reshaping
    /// custody must leave the operator's wallet alone. The two hold
    /// different money under different locks, and a refactor that
    /// collapsed them would still pass every assertion above it.
    #[tokio::test]
    async fn the_fan_out_splits_the_custody_wallet_independently() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 15_000_000_000).await;
        let hub = spawn_hub(operator_key.clone(), fake_node.addr.clone()).await;
        let custody = hub.state.exchange_custody_public_key.clone();

        fake_node.fund(custody.clone(), 15_000_000_000).await;
        assert_eq!(
            fake_node.outputs_of(&custody).await.len(),
            1,
            "custody starts as the same single-output wallet the operator did"
        );

        handlers::maintain_custody_outputs(&hub.state).await;
        fake_node.wait_for_submissions_seen(1).await;

        let outputs = fake_node.outputs_of(&custody).await;
        assert_eq!(outputs.len(), operator_wallet::DEFAULT_WALLET_OUTPUTS);
        assert!(
            outputs.iter().all(|(o, _)| o.value >= operator_wallet::MIN_USEFUL_OUTPUT),
            "every piece has to be able to fund a withdrawal, or it is not a slot"
        );
        assert_eq!(
            hub.state.metrics.custody_ready_outputs.load(Ordering::Relaxed),
            1,
            "the gauge reports the wallet as it was read, before this reshape -- one              output, which is usable and is still a ceiling of one payment per block"
        );

        assert_eq!(
            fake_node.outputs_of(&operator_key.public_key()).await.len(),
            1,
            "reshaping custody must not touch the operator's wallet"
        );
        assert_eq!(
            hub.state.metrics.operator_fan_outs.load(Ordering::Relaxed),
            0,
            "and must not be counted as an operator fan-out"
        );
        assert_eq!(hub.state.metrics.custody_fan_outs.load(Ordering::Relaxed), 1);
    }

    /// The fan-out, end to end against a node that mines what it is
    /// given: one blob in, a wallet at the floor out.
    #[tokio::test]
    async fn the_fan_out_splits_one_operator_output_into_a_full_wallet() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 15_000_000_000).await;
        let hub = spawn_hub(operator_key.clone(), fake_node.addr.clone()).await;
        assert_eq!(
            fake_node.outputs_of(&operator_key.public_key()).await.len(),
            1,
            "a freshly funded operator is the single-output wallet §6.4b measured"
        );

        handlers::maintain_operator_outputs(&hub.state).await;
        fake_node.wait_for_submissions_seen(1).await;

        let outputs = fake_node.outputs_of(&operator_key.public_key()).await;
        assert_eq!(outputs.len(), operator_wallet::DEFAULT_WALLET_OUTPUTS);
        assert!(
            outputs.iter().all(|(o, _)| o.value >= operator_wallet::MIN_USEFUL_OUTPUT),
            "every piece has to be able to fund a payment, or it is not a slot"
        );
        assert_eq!(
            outputs.iter().map(|(o, _)| o.value).sum::<u64>(),
            15_000_000_000 - 1_000,
            "the fan-out pays itself everything but the fee"
        );

        // Idempotent: a wallet already at the floor is left alone, which
        // is what stops the sweep paying a fee a minute forever.
        handlers::maintain_operator_outputs(&hub.state).await;
        assert_eq!(
            fake_node.outputs_of(&operator_key.public_key()).await.len(),
            operator_wallet::DEFAULT_WALLET_OUTPUTS
        );
    }

    /// A fan-out that is genuinely on the wire must not be repeated: its
    /// outputs are invisible until mined, so the wallet still looks
    /// short and a second pass would break an output that is doing its
    /// job.
    #[tokio::test]
    async fn a_fan_out_still_in_the_mempool_is_not_repeated() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 15_000_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;

        fake_node.set_fate(SubmissionFate::HeldInMempool).await;
        handlers::maintain_operator_outputs(&hub.state).await;
        fake_node.wait_for_submissions_seen(1).await;

        handlers::maintain_operator_outputs(&hub.state).await;
        assert_eq!(
            fake_node.submissions_seen.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the wallet is short only because the first split is unconfirmed"
        );
    }

    /// And the state that looks the same and is not. A node restart
    /// drops its mempool, so a submitted fan-out can simply cease to
    /// exist -- inputs back in the set, unmarked, nothing pending. A
    /// guard that waited on presence alone would wait for a confirmation
    /// that is never coming, every sweep, for the life of the process.
    #[tokio::test]
    async fn a_fan_out_that_never_reached_the_node_is_sent_again() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 15_000_000_000).await;
        let hub = spawn_hub(operator_key.clone(), fake_node.addr.clone()).await;

        fake_node.set_fate(SubmissionFate::Swallowed).await;
        handlers::maintain_operator_outputs(&hub.state).await;
        fake_node.wait_for_submissions_seen(1).await;
        assert_eq!(
            fake_node.outputs_of(&operator_key.public_key()).await.len(),
            1,
            "nothing happened, which is what a lost submission looks like"
        );

        fake_node.set_fate(SubmissionFate::Mined).await;
        handlers::maintain_operator_outputs(&hub.state).await;
        fake_node.wait_for_submissions_seen(2).await;
        assert_eq!(
            fake_node.outputs_of(&operator_key.public_key()).await.len(),
            operator_wallet::DEFAULT_WALLET_OUTPUTS,
            "the wallet must not be stuck waiting on a transaction that does not exist"
        );
    }

    /// The ceiling itself. Nothing is mined between these grants -- the
    /// node holds every payout in its mempool, which is precisely the
    /// state that used to leave the operator with zero spendable
    /// balance after the first one.
    #[tokio::test]
    async fn a_fanned_out_wallet_pays_many_grants_between_blocks() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 15_000_000_000).await;
        let hub = spawn_hub(operator_key.clone(), fake_node.addr.clone()).await;

        handlers::maintain_operator_outputs(&hub.state).await;
        fake_node.wait_for_submissions_seen(1).await;

        // From here on the node accepts payments and mines nothing, so
        // every payout's change stays invisible and each grant has to
        // find a *different* confirmed output to spend.
        fake_node.set_fate(SubmissionFate::HeldInMempool).await;

        let grants = 8;
        for i in 0..grants {
            let agent = PrivateKey::new_key();
            let resp = claim_faucet(&hub, &agent).await;
            assert_eq!(
                resp.status(),
                reqwest::StatusCode::OK,
                "grant {i} of {grants} with no block in between: {}",
                resp.text().await.unwrap()
            );
        }

        let spendable = fake_node
            .outputs_of(&operator_key.public_key())
            .await
            .iter()
            .filter(|(_, marked)| !marked)
            .count();
        assert_eq!(
            spendable,
            operator_wallet::DEFAULT_WALLET_OUTPUTS - grants,
            "one grant should consume exactly one slot"
        );
    }

    /// A payment must not eat the output the wallet cannot afford to
    /// lose. This is the selection half of the fix, asserted against a
    /// real payout rather than against `ordered_for_payment` alone.
    #[tokio::test]
    async fn a_grant_spends_the_smallest_output_that_covers_it() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn_empty().await;
        for value in [15_000_000_000u64, 200_000_000, 900_000_000] {
            fake_node.credit(operator_key.public_key(), value).await;
        }
        let hub = spawn_hub(operator_key.clone(), fake_node.addr.clone()).await;
        fake_node.set_fate(SubmissionFate::HeldInMempool).await;

        let agent = PrivateKey::new_key();
        assert_eq!(claim_faucet(&hub, &agent).await.status(), reqwest::StatusCode::OK);

        let marked: Vec<u64> = fake_node
            .outputs_of(&operator_key.public_key())
            .await
            .iter()
            .filter(|(_, marked)| *marked)
            .map(|(o, _)| o.value)
            .collect();
        assert_eq!(
            marked,
            vec![200_000_000],
            "the 150-coin output has to still be whole and spendable"
        );
    }

    /// The reason the redemption record is durable. An attacker who
    /// watches the hub go down and replays a solution the instant it
    /// returns must find the challenge already spent -- an in-memory
    /// book would have forgotten it.
    #[tokio::test]
    async fn a_redeemed_challenge_is_still_spent_after_a_restart() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let agent = PrivateKey::new_key();

        let challenge = request_faucet_challenge(&hub, &agent).await;
        let id: Uuid = challenge["challenge_id"].as_str().unwrap().parse().unwrap();
        let solution = solve_faucet_challenge(&challenge);
        assert_eq!(
            redeem_faucet_challenge(&hub, &agent, &challenge).await.status(),
            reqwest::StatusCode::OK
        );

        // A second book over the same store, with its own empty memory:
        // exactly what a restart produces.
        let (restarted, restored) = faucet_pow::ChallengeBook::restore(
            hub.state.store.clone(),
            faucet_pow::target_for_expected_hashes(TEST_FAUCET_EXPECTED_HASHES),
            Utc::now(),
        )
        .unwrap();
        assert!(restored > 0, "the redeemed challenge must come back from disk");
        assert!(
            matches!(
                restarted.redeem(id, &agent.public_key(), solution, Utc::now()),
                Err(faucet_pow::RedemptionError::AlreadyRedeemed)
            ),
            "a restart must not make a spent solution spendable again"
        );
    }

    /// An expired challenge is refused, and the sweep eventually stops
    /// carrying it. Both halves matter: the first is the rule, the
    /// second is what keeps the table from growing by a row per
    /// challenge ever issued.
    #[tokio::test]
    async fn an_expired_challenge_is_refused_and_then_collected() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let agent = PrivateKey::new_key();

        let challenge = request_faucet_challenge(&hub, &agent).await;
        let id: Uuid = challenge["challenge_id"].as_str().unwrap().parse().unwrap();
        let solution = solve_faucet_challenge(&challenge);

        let past_expiry =
            Utc::now() + chrono::Duration::seconds(faucet_pow::CHALLENGE_TTL_SECONDS + 1);
        assert!(matches!(
            hub.state
                .faucet_challenges
                .redeem(id, &agent.public_key(), solution, past_expiry),
            Err(faucet_pow::RedemptionError::Expired)
        ));

        run_sweep_once(&hub.state, past_expiry).await;
        assert_eq!(
            hub.state.faucet_challenges.len(),
            0,
            "an expired, unredeemed challenge is garbage and the sweep must take it"
        );
        assert!(hub.state.store.load_all_faucet_challenges().unwrap().is_empty());
    }

    /// One outstanding challenge per key. Asking again replaces rather
    /// than accumulates, so a client cannot build a stock of puzzles to
    /// solve at leisure and redeem in a burst.
    #[tokio::test]
    async fn asking_twice_replaces_the_outstanding_challenge() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let agent = PrivateKey::new_key();

        let first = request_faucet_challenge(&hub, &agent).await;
        let second = request_faucet_challenge(&hub, &agent).await;
        assert_ne!(first["challenge_id"], second["challenge_id"]);
        assert_ne!(first["server_nonce"], second["server_nonce"], "a fresh nonce each time");

        assert_eq!(
            hub.state.faucet_challenges.outstanding_for(&agent.public_key()),
            Some(second["challenge_id"].as_str().unwrap().parse().unwrap())
        );

        // The superseded one is gone rather than merely unreferenced, so
        // a late solution against it is told so.
        let resp = redeem_faucet_challenge(&hub, &agent, &first).await;
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

        assert_eq!(
            redeem_faucet_challenge(&hub, &agent, &second).await.status(),
            reqwest::StatusCode::OK
        );
    }

    /// A key that has already been granted is turned away before it
    /// spends any work, which is the difference between a rate limiter
    /// and a rude one.
    #[tokio::test]
    async fn an_already_granted_key_is_refused_a_challenge() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let agent = PrivateKey::new_key();

        assert_eq!(claim_faucet(&hub, &agent).await.status(), reqwest::StatusCode::OK);

        let resp = hub
            .client
            .post(format!("{}/faucet/challenge", hub.base_url))
            .json(&envelope(&agent, "/faucet/challenge", ()))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn full_http_task_lifecycle_pays_out_through_a_real_router() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let agent_key = PrivateKey::new_key();

        let resp = claim_faucet(&hub, &agent_key).await;
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        // A second, fully-solved attempt by an already-granted pubkey --
        // distinct from the replay case, which reuses one signature (see
        // `replayed_envelope_is_rejected`). Refused at the challenge
        // step now, before any work is spent, which is the friendlier
        // place to say no.
        let resp = hub
            .client
            .post(format!("{}/faucet/challenge", hub.base_url))
            .json(&envelope(&agent_key, "/faucet/challenge", ()))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);

        let expected_output_hash = hex::encode(Hash::hash_bytes(b"42").as_bytes());
        let payload = handlers::CreateTaskPayload {
            description: "test task".to_string(),
            bounty: 1_000,
            expected_output_hash,
            min_reputation: 0,
            capabilities: Default::default(),
        };
        let resp = hub
            .client
            .post(format!("{}/tasks", hub.base_url))
            .json(&envelope(&hub.operator_key, "/tasks", payload))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let task: Value = resp.json().await.unwrap();
        let task_id: Uuid = task["id"].as_str().unwrap().parse().unwrap();

        let resp = hub
            .client
            .post(format!("{}/tasks/{task_id}/claim", hub.base_url))
            .json(&envelope(&agent_key, &format!("/tasks/{task_id}/claim"), handlers::ClaimPayload { task_id }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        let resp = hub
            .client
            .post(format!("{}/tasks/{task_id}/submit", hub.base_url))
            .json(&envelope(
                &agent_key, &format!("/tasks/{task_id}/submit"),
                handlers::SubmitPayload { task_id, output: "wrong answer".to_string() },
            ))
            .send()
            .await
            .unwrap();
        let result: Value = resp.json().await.unwrap();
        assert_eq!(result["verified"], false);

        hub.client
            .post(format!("{}/tasks/{task_id}/claim", hub.base_url))
            .json(&envelope(&agent_key, &format!("/tasks/{task_id}/claim"), handlers::ClaimPayload { task_id }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();

        let resp = hub
            .client
            .post(format!("{}/tasks/{task_id}/submit", hub.base_url))
            .json(&envelope(
                &agent_key, &format!("/tasks/{task_id}/submit"),
                handlers::SubmitPayload { task_id, output: "42".to_string() },
            ))
            .send()
            .await
            .unwrap();
        let result: Value = resp.json().await.unwrap();
        assert_eq!(result["verified"], true);
        assert_eq!(result["paid"], true, "submitted -- see SubmitResultDto::paid");

        // The bounty payout is only *submitted* at this point.
        // Reputation, like everything else downstream of a payout, now
        // waits until the sweep has seen it on chain.
        confirm_submitted_payouts(&hub.state, &fake_node).await;

        let reputation: Value = hub
            .client
            .get(format!("{}/reputation/{}", hub.base_url, agent_key.public_key()))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(reputation["completed"], 1);
        assert_eq!(reputation["total_earned"], 1_000);

        let leaderboard: Value = hub
            .client
            .get(format!("{}/leaderboard", hub.base_url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(leaderboard.as_array().unwrap().len(), 1);

        let submitted = fake_node.wait_for_submitted_count(2).await;
        assert_eq!(submitted.len(), 2, "faucet grant + bounty payout");
    }

    #[tokio::test]
    async fn create_task_rejects_non_operator() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let impostor = PrivateKey::new_key();

        let payload = handlers::CreateTaskPayload {
            description: "should be rejected".to_string(),
            bounty: 10,
            expected_output_hash: hex::encode(Hash::hash_bytes(b"x").as_bytes()),
            min_reputation: 0,
            capabilities: Default::default(),
        };
        let resp = hub
            .client
            .post(format!("{}/tasks", hub.base_url))
            .json(&envelope(&impostor, "/tasks", payload))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn create_task_rejects_insufficient_operator_balance() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;

        let payload = handlers::CreateTaskPayload {
            description: "too rich for the operator".to_string(),
            bounty: 1_000_000,
            expected_output_hash: hex::encode(Hash::hash_bytes(b"x").as_bytes()),
            min_reputation: 0,
            capabilities: Default::default(),
        };
        let resp = hub
            .client
            .post(format!("{}/tasks", hub.base_url))
            .json(&envelope(&hub.operator_key, "/tasks", payload))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn replayed_envelope_is_rejected() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let agent_key = PrivateKey::new_key();
        // A genuinely solved claim, so the replay below is refused for
        // being a replay and not for being unsolved.
        let challenge = request_faucet_challenge(&hub, &agent_key).await;
        let env = envelope(
            &agent_key,
            "/faucet",
            handlers::FaucetClaimPayload {
                challenge_id: challenge["challenge_id"].as_str().unwrap().parse().unwrap(),
                solution: solve_faucet_challenge(&challenge),
            },
        );

        let first = hub
            .client
            .post(format!("{}/faucet", hub.base_url))
            .json(&env)
            .send()
            .await
            .unwrap();
        assert_eq!(first.status(), reqwest::StatusCode::OK);

        // the exact same envelope (same signature) a second time
        let second = hub
            .client
            .post(format!("{}/faucet", hub.base_url))
            .json(&env)
            .send()
            .await
            .unwrap();
        assert_eq!(second.status(), reqwest::StatusCode::UNAUTHORIZED);

        // ...and the accepted signature reached disk on the way through,
        // so the rejection above would survive a restart rather than
        // depending on this process's memory. That a restored guard then
        // rejects it is `auth`'s own
        // `a_replayed_envelope_is_still_rejected_after_a_restart`; this
        // is the half that can only be checked on the real HTTP path.
        let expected_signature = hex::decode(env["signature"].as_str().unwrap()).unwrap();
        let recorded = hub.state.store.load_recent_signatures(i64::MIN).unwrap();
        assert!(
            recorded.iter().any(|(sig, _)| *sig == expected_signature),
            "a signature accepted over HTTP must be durably recorded"
        );
    }

    /// The five route pairs that used to share a signing string, now
    /// asserted to be separated. Before the recipe bound method and path,
    /// each of these pairs took a byte-identical payload, so one signed
    /// envelope was valid at either route in the pair; what stopped that
    /// mattering was a chain of unrelated accidents (see
    /// docs/agent-ecosystem-plan.md §3.3). This is the same test inverted:
    /// it was a tripwire recording the gap, and is now the regression
    /// guard that the gap stayed shut.
    #[test]
    fn route_pairs_with_identical_payloads_no_longer_share_a_signing_string() {
        let key = PrivateKey::new_key();
        let task_id = Uuid::new_v4();
        let escrow_id = Uuid::new_v4();

        // Each pair: one payload value, two routes that both accept it.
        let claim = format!("/tasks/{task_id}/claim");
        let cancel = format!("/tasks/{task_id}/cancel");
        let task_confirm = format!("/tasks/escrow/{escrow_id}/confirm");
        let exchange_confirm = format!("/exchange/deposit/{escrow_id}/confirm");
        let pairs: Vec<(&str, &str, Value)> = vec![
            ("/faucet", "/exchange/deposit", json!(null)),
            ("/tasks", "/tasks/escrow", json!({
                "description": "t", "bounty": 1, "expected_output_hash": "ab",
                "min_reputation": 2, "capabilities": []
            })),
            ("/tasks/consensus", "/tasks/consensus/escrow", json!({
                "description": "t", "bounty": 1, "num_assignees": 3,
                "join_window_minutes": 10, "submission_window_minutes": 20,
                "min_reputation": 2, "capabilities": []
            })),
            (
                &claim,
                &cancel,
                json!({ "task_id": task_id }),
            ),
            (
                &task_confirm,
                &exchange_confirm,
                json!({ "escrow_id": escrow_id }),
            ),
        ];

        for (left, right, payload) in pairs {
            let envelope = btclib::envelope::SignedEnvelope::new(&key, "POST", left, payload);
            assert!(
                envelope.verify_signature(Utc::now(), "POST", left).is_ok(),
                "an envelope must verify at the route it was signed for ({left})"
            );
            assert!(
                matches!(
                    envelope.verify_signature(Utc::now(), "POST", right),
                    Err(btclib::envelope::EnvelopeError::BadSignature)
                ),
                "an envelope signed for {left} must not verify at {right}"
            );
        }
    }

    /// The finding, closed at the HTTP layer rather than only in the
    /// recipe: two routes taking a payload-less envelope would, before
    /// the path was bound in, accept each other's. The signature is
    /// genuine and unexpired here -- the only thing wrong with it is the
    /// door it is being presented at.
    ///
    /// The pair used to be `/faucet` and `/exchange/deposit`. Giving the
    /// faucet a proof-of-work payload retired that pairing and created
    /// this one, `/faucet/challenge` and `/exchange/deposit`, which is
    /// the useful reminder: payload-less routes keep appearing, so the
    /// protection has to be the binding rather than an audit of which
    /// routes currently collide.
    #[tokio::test]
    async fn an_envelope_signed_for_one_route_is_rejected_at_another() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let agent_key = PrivateKey::new_key();

        let for_faucet = envelope(&agent_key, "/faucet/challenge", ());
        let resp = hub
            .client
            .post(format!("{}/exchange/deposit", hub.base_url))
            .json(&for_faucet)
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "an envelope signed for /faucet/challenge must not be accepted at /exchange/deposit"
        );

        // The same envelope at the route it was actually signed for still
        // works -- so the rejection above was the binding, not a broken
        // signature. (It also proves the failed attempt did not burn the
        // signature: the replay guard only claims it once verification
        // passes, junk bytes cannot spend someone else's slot.)
        let resp = hub
            .client
            .post(format!("{}/faucet/challenge", hub.base_url))
            .json(&for_faucet)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
    }

    #[tokio::test]
    async fn stale_timestamp_envelope_is_rejected() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let agent_key = PrivateKey::new_key();

        // `/faucet/challenge` rather than `/faucet`, because the drift
        // check runs before the payload is looked at and this route
        // still takes an empty one -- so the test stays about the
        // timestamp rather than about solving a puzzle first.
        let stale = envelope_at(
            &agent_key,
            "/faucet/challenge",
            (),
            Utc::now() - chrono::Duration::minutes(10),
        );
        let resp = hub
            .client
            .post(format!("{}/faucet/challenge", hub.base_url))
            .json(&stale)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

        // The drift check stands on its own: this envelope was rejected
        // by its timestamp alone, with the replay guard having never
        // seen it. It must also not have been written -- otherwise an
        // unauthenticated flood of stale envelopes would be a way to
        // make the hub fsync on demand.
        assert!(
            hub.state.store.load_recent_signatures(i64::MIN).unwrap().is_empty(),
            "a drift rejection must not reach the durable replay guard"
        );
    }

    #[tokio::test]
    async fn health_reports_ok_and_chain_height_when_a_node_is_reachable() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 0).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;

        let resp = hub.client.get(format!("{}/health", hub.base_url)).send().await.unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["status"], "ok");
        assert_eq!(body["chain_height"], FAKE_NODE_CHAIN_HEIGHT);
    }

    #[tokio::test]
    async fn health_reports_503_when_no_node_is_reachable() {
        let operator_key = PrivateKey::new_key();
        let hub = spawn_hub(operator_key, dead_address().await).await;

        let resp = hub.client.get(format!("{}/health", hub.base_url)).send().await.unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["status"], "degraded");
    }

    /// The endpoint renders, and a counter actually tracks traffic --
    /// the two halves of "is this thing plugged in", which a metrics
    /// route can fail independently and silently. A scrape that renders
    /// perfectly well while every number stays at zero is the failure
    /// mode worth a test, because it looks healthy in exactly the way an
    /// unmonitored hub does.
    #[tokio::test]
    async fn metrics_renders_and_a_driven_request_moves_its_counter() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 0).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;

        for _ in 0..3 {
            let resp = hub.client.get(format!("{}/health", hub.base_url)).send().await.unwrap();
            assert_eq!(resp.status(), reqwest::StatusCode::OK);
        }

        let resp = hub.client.get(format!("{}/metrics", hub.base_url)).send().await.unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        assert!(
            resp.headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.starts_with("text/plain")),
            "a scraper content-negotiates on this; a JSON content type makes the body an untyped blob"
        );
        let body = resp.text().await.unwrap();

        assert!(body.contains("# TYPE hub_sweep_passes_total counter"), "the exposition format needs its TYPE lines:\n{body}");
        assert_eq!(
            body.lines()
                .find(|line| line.starts_with(r#"hub_http_requests_total{route="/health",method="GET",status="2xx"}"#)),
            Some(r#"hub_http_requests_total{route="/health",method="GET",status="2xx"} 3"#),
            "three health checks must show as three, not as zero or as one:\n{body}"
        );
        assert!(
            body.contains(r#"hub_http_request_duration_seconds_count{route="/health",method="GET"} 3"#),
            "the latency histogram must have seen the same three requests:\n{body}"
        );
        // The scrape itself is recorded *after* the body is rendered, so
        // this first one cannot appear in its own output. Asserted rather
        // than left implicit because the alternative -- a scrape that
        // counted itself before rendering -- would make every series
        // permanently one ahead of reality.
        assert!(
            !body.contains(r#"route="/metrics""#),
            "a scrape must not appear in its own output:\n{body}"
        );
    }

    /// The one property that makes it safe to leave this route
    /// unauthenticated.
    ///
    /// `/metrics` is the cheapest call on the hub, so if rendering it
    /// reached the chain it would be the most efficient amplifier on the
    /// box -- one unauthenticated request turning into a TCP round trip
    /// competing with real payouts for the connection pool. The numbers
    /// that need the node are sampled by the sweep instead
    /// (`sample_gauges`), and this is what holds the next person to that.
    #[tokio::test]
    async fn a_metrics_scrape_never_reaches_the_node() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 0).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;

        // A health check first, precisely because `/health` *does* reach
        // the node: without it a broken FakeNode would make this test
        // pass for the wrong reason.
        hub.client.get(format!("{}/health", hub.base_url)).send().await.unwrap();
        let after_health = fake_node.connections_accepted();
        assert!(after_health > 0, "the control request must have reached the node");

        for _ in 0..20 {
            let resp = hub.client.get(format!("{}/metrics", hub.base_url)).send().await.unwrap();
            assert_eq!(resp.status(), reqwest::StatusCode::OK);
        }

        assert_eq!(
            fake_node.connections_accepted(),
            after_health,
            "twenty scrapes opened a connection to the node; /metrics must be answerable from memory alone"
        );
    }

    /// The other side of that bargain: the numbers a scrape refuses to
    /// compute have to actually arrive, and the sweep is what delivers
    /// them. Without this, "the endpoint never reaches the node" would be
    /// satisfiable by never reporting the chain at all.
    #[tokio::test]
    async fn the_sweep_samples_the_gauges_a_scrape_will_not_compute() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 0).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;

        let before = hub.client.get(format!("{}/metrics", hub.base_url)).send().await.unwrap().text().await.unwrap();
        assert!(before.contains("hub_chain_height 0"), "nothing is known before the first sweep:\n{before}");

        run_sweep_once(&hub.state, Utc::now()).await;

        let after = hub.client.get(format!("{}/metrics", hub.base_url)).send().await.unwrap().text().await.unwrap();
        assert!(
            after.contains(&format!("hub_chain_height {FAKE_NODE_CHAIN_HEIGHT}")),
            "the sweep must have observed the chain tip on the hub's behalf:\n{after}"
        );
    }

    /// A prior version of `llms_txt` documented only `hash_match`/
    /// `consensus` tasks and operator-only posting -- it went stale as
    /// Features 1-3 (agent-to-agent posting, disputes, capabilities)
    /// shipped without anyone updating the one doc an agent actually
    /// reads to learn the API. This doesn't check the prose itself (too
    /// brittle), just that every endpoint/kind introduced since the
    /// original version is at least mentioned somewhere.
    /// The manual has to carry the faucet's new shape, because an agent
    /// following it is the only client that exists before the SDK is
    /// published. The byte-order line is singled out: it is the one part
    /// a reader can get wrong and then hash forever without a hit.
    #[tokio::test]
    async fn llms_txt_documents_the_faucet_proof_of_work() {
        let operator_key = PrivateKey::new_key();
        let hub = spawn_hub(operator_key, dead_address().await).await;
        let llms = hub.client.get(format!("{}/llms.txt", hub.base_url)).send().await.unwrap().text().await.unwrap();
        for expected in [
            "POST /faucet/challenge",
            "preimage_template",
            "expected_hashes",
            "little-endian",
            "int.from_bytes(sha256(preimage.encode()).digest(), \"little\") <= int(target, 16)",
            "\"solution\": N",
        ] {
            assert!(llms.contains(expected), "llms.txt no longer mentions {expected:?}");
        }
        assert!(
            llms.contains(&TEST_FAUCET_EXPECTED_HASHES.to_string()),
            "the difficulty must be the hub's live value, not a hardcoded one"
        );
    }

    #[tokio::test]
    async fn llms_txt_mentions_every_task_kind_and_the_escrow_and_dispute_flows() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;

        let llms = hub
            .client
            .get(format!("{}/llms.txt", hub.base_url))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();

        for expected in [
            "hash_match",
            "consensus",
            "disputable",
            "/tasks/escrow",
            "/tasks/consensus/escrow",
            "/tasks/disputable/escrow",
            "/tasks/escrow/<escrow_id>/confirm",
            "/dispute/escrow",
            "/dispute/confirm",
            "/dispute/resolve",
            "challenger_wins",
            "assignee_wins",
            "capabilities",
            "?capability=",
            "/tasks/<id>/cancel",
        ] {
            assert!(llms.contains(expected), "llms.txt no longer mentions {expected:?}");
        }
    }

    /// `/llms.txt` is the onboarding manual an agent reads instead of
    /// asking a human, so the distinction between a payout that was sent
    /// and one that landed has to be *in it* -- an agent told `paid:
    /// true` and left to infer the rest will treat a submission as a
    /// settlement, which is the mistake the hub itself used to make.
    #[tokio::test]
    async fn llms_txt_explains_that_sent_is_not_confirmed() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;

        let llms = hub
            .client
            .get(format!("{}/llms.txt", hub.base_url))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();

        for expected in [
            "Submitted",
            "PayoutFailed",
            "bounty_confirmed",
            "bounty_pending",
            // The three claims an agent actually needs: what Paid now
            // means, that an abandoned payout is still owed, and that
            // reputation only counts confirmed money.
            "seen the payment on chain",
            "still owed to you",
            "only ever counts confirmed payments",
        ] {
            assert!(llms.contains(expected), "llms.txt no longer mentions {expected:?}");
        }
    }

    #[tokio::test]
    async fn llms_txt_mentions_the_exchange() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;

        let llms = hub
            .client
            .get(format!("{}/llms.txt", hub.base_url))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();

        for expected in [
            "/exchange/deposit",
            "/exchange/deposit/<escrow_id>/confirm",
            "/exchange/orders",
            "/exchange/orders/<id>/cancel",
            "/exchange/account/<pubkey>",
            "/exchange/withdraw",
            "/exchange/trades",
            "compute",
            "taker",
            "maker",
            "basis-point",
        ] {
            assert!(llms.contains(expected), "llms.txt no longer mentions {expected:?}");
        }
    }

    /// Exercises `rate_limit::middleware` through the real router (not just
    /// `check_and_record` in isolation) -- confirms the layer is actually
    /// wired up, `ConnectInfo` extraction works end to end, and a client
    /// that stays under the limit is never bothered while one that exceeds
    /// it gets a 429, both against a genuine HTTP server on a real socket.
    #[tokio::test]
    async fn exceeding_the_per_ip_request_limit_gets_a_429() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;

        for _ in 0..rate_limit::MAX_REQUESTS_PER_WINDOW {
            let status = hub.client.get(format!("{}/llms.txt", hub.base_url)).send().await.unwrap().status();
            assert_eq!(status, reqwest::StatusCode::OK, "request within the limit must succeed");
        }

        let status = hub.client.get(format!("{}/llms.txt", hub.base_url)).send().await.unwrap().status();
        assert_eq!(status, reqwest::StatusCode::TOO_MANY_REQUESTS, "one past the limit must be rejected");
    }

    /// The bypass this closes: before trusted-proxy handling, one header
    /// was enough to leave the rate limiter behind entirely, because
    /// `X-Forwarded-For` was believed from any peer. Drives a real HTTP
    /// server so the assertion covers the deployed default (trust
    /// nothing), not just `client_ip` in isolation.
    #[tokio::test]
    async fn a_spoofed_forwarded_for_cannot_escape_the_limit_on_an_unproxied_hub() {
        let operator_key = PrivateKey::new_key();
        let hub = spawn_hub(operator_key, dead_address().await).await;

        // A fresh, distinct claimed address on every request -- the exact
        // shape of the evasion, and previously an unlimited number of
        // requests.
        for i in 0..rate_limit::MAX_REQUESTS_PER_WINDOW {
            let status = hub
                .client
                .get(format!("{}/llms.txt", hub.base_url))
                .header("x-forwarded-for", format!("203.0.113.{}", i % 256))
                .send()
                .await
                .unwrap()
                .status();
            assert_eq!(status, reqwest::StatusCode::OK, "request within the limit must succeed");
        }

        let status = hub
            .client
            .get(format!("{}/llms.txt", hub.base_url))
            .header("x-forwarded-for", "203.0.113.255")
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(
            status,
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            "an untrusted peer's X-Forwarded-For must not move it out of its own bucket"
        );
    }

    /// The other half: once a proxy is configured, its header is what
    /// keeps the limiter pointed at real clients rather than uniformly
    /// limiting the proxy itself on everyone's behalf.
    #[tokio::test]
    async fn a_trusted_proxys_forwarded_for_separates_clients_behind_it() {
        let operator_key = PrivateKey::new_key();
        let trusted = rate_limit::parse_trusted_proxies("127.0.0.1").unwrap();
        let hub = spawn_hub_with_trusted_proxies(operator_key, dead_address().await, trusted).await;

        for _ in 0..rate_limit::MAX_REQUESTS_PER_WINDOW {
            let status = hub
                .client
                .get(format!("{}/llms.txt", hub.base_url))
                .header("x-forwarded-for", "203.0.113.9")
                .send()
                .await
                .unwrap()
                .status();
            assert_eq!(status, reqwest::StatusCode::OK);
        }

        let exhausted = hub
            .client
            .get(format!("{}/llms.txt", hub.base_url))
            .header("x-forwarded-for", "203.0.113.9")
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(exhausted, reqwest::StatusCode::TOO_MANY_REQUESTS);

        let neighbour = hub
            .client
            .get(format!("{}/llms.txt", hub.base_url))
            .header("x-forwarded-for", "203.0.113.10")
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(
            neighbour,
            reqwest::StatusCode::OK,
            "a second client behind the same proxy must not inherit the first's exhausted bucket"
        );
    }

    /// The evasion the key axis exists to close: spread the same key over
    /// enough addresses and every per-IP bucket it touches looks idle.
    /// Driven through a trusted proxy so each request genuinely lands in
    /// a fresh address bucket -- if the per-IP limit were what stopped
    /// this, the test would never reach a 429 at all.
    ///
    /// `/tasks/:id/claim` on a task that does not exist is the cheapest
    /// route that still verifies a signature: the envelope is checked
    /// (and charged) before the board is consulted, so every request
    /// under quota comes back 404 and the first one over comes back 429.
    #[tokio::test]
    async fn a_key_is_limited_across_addresses_even_when_no_address_is() {
        let operator_key = PrivateKey::new_key();
        let trusted = rate_limit::parse_trusted_proxies("127.0.0.1").unwrap();
        let hub = spawn_hub_with_trusted_proxies(operator_key, dead_address().await, trusted).await;

        let agent = PrivateKey::new_key();
        let task_id = Uuid::new_v4();
        let claim = |address: String| {
            hub.client
                .post(format!("{}/tasks/{task_id}/claim", hub.base_url))
                .header("x-forwarded-for", address)
                .json(&envelope(&agent, &format!("/tasks/{task_id}/claim"), handlers::ClaimPayload { task_id }))
                .send()
        };

        for i in 0..rate_limit::MAX_SIGNED_REQUESTS_PER_PUBKEY_PER_WINDOW {
            let status = claim(format!("203.0.113.{}", i % 256)).await.unwrap().status();
            assert_eq!(
                status,
                reqwest::StatusCode::NOT_FOUND,
                "within quota the request should reach the board and simply not find the task"
            );
        }

        let status = claim("203.0.113.200".to_string()).await.unwrap().status();
        assert_eq!(
            status,
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            "the key's own budget must run out even though every address it used looked idle"
        );

        // A different key from the same (already-used) address is
        // untouched -- the quota follows the identity, not the network.
        let neighbour = PrivateKey::new_key();
        let status = hub
            .client
            .post(format!("{}/tasks/{task_id}/claim", hub.base_url))
            .header("x-forwarded-for", "203.0.113.1")
            .json(&envelope(&neighbour, &format!("/tasks/{task_id}/claim"), handlers::ClaimPayload { task_id }))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
    }

    /// What the quota is *for*: bounding how often one key can make the
    /// hub fsync.
    ///
    /// The replay guard records every accepted signature durably before
    /// its handler runs, so an authenticated request costs a disk write.
    /// The per-key quota is the thing that bounds that for a holder of a
    /// valid key -- but only if it is charged before the write. It used
    /// to be charged after, on a separate line in each handler once
    /// verification had already claimed and flushed the signature, so a
    /// key past its limit still bought a write per request and the quota
    /// bounded nothing but the handler body.
    ///
    /// The second assertion is the half a caller notices: an envelope
    /// turned away for quota must not have been claimed, or it is burned
    /// and the client has to re-sign rather than simply retry when its
    /// window rolls over.
    #[tokio::test]
    async fn a_request_over_quota_costs_no_disk_write_and_does_not_burn_its_envelope() {
        let operator_key = PrivateKey::new_key();
        let trusted = rate_limit::parse_trusted_proxies("127.0.0.1").unwrap();
        let hub = spawn_hub_with_trusted_proxies(operator_key, dead_address().await, trusted).await;

        let agent = PrivateKey::new_key();
        let task_id = Uuid::new_v4();
        let path = format!("/tasks/{task_id}/claim");

        for i in 0..rate_limit::MAX_SIGNED_REQUESTS_PER_PUBKEY_PER_WINDOW {
            let status = hub
                .client
                .post(format!("{}{path}", hub.base_url))
                .header("x-forwarded-for", format!("203.0.113.{}", i % 256))
                .json(&envelope(&agent, &path, handlers::ClaimPayload { task_id }))
                .send()
                .await
                .unwrap()
                .status();
            assert_eq!(status, reqwest::StatusCode::NOT_FOUND, "these are within quota");
        }

        let rejected = envelope(&agent, &path, handlers::ClaimPayload { task_id });
        let signature = hex::decode(rejected["signature"].as_str().unwrap()).unwrap();
        let before = hub.state.store.load_recent_signatures(i64::MIN).unwrap().len();

        let status = hub
            .client
            .post(format!("{}{path}", hub.base_url))
            .header("x-forwarded-for", "203.0.113.200")
            .json(&rejected)
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, reqwest::StatusCode::TOO_MANY_REQUESTS);

        let after = hub.state.store.load_recent_signatures(i64::MIN).unwrap();
        assert_eq!(
            after.len(),
            before,
            "a request rejected for quota must not have reached the durable replay write"
        );
        assert!(
            !after.iter().any(|(recorded, _)| *recorded == signature),
            "the rejected envelope must survive so the caller can retry it, not be spent"
        );
    }

    /// A replay costs the replayer, not the agent whose envelope was
    /// captured.
    ///
    /// The quota is charged to the key that *signed* the request, so
    /// charging before the replay was detected made one captured envelope
    /// into a lockout: resend it sixty times from a single address --
    /// comfortably inside that address's own tier -- and the signer is
    /// refused on every authenticated route until the window rolls over.
    /// Reading a signed request off the wire is enough to do it, and
    /// `--bind 0.0.0.0` is a supported deployment.
    ///
    /// The victim's budget is the observable. The replays themselves are
    /// rejected either way, so a test that only watched their status
    /// codes would pass against the bug.
    #[tokio::test]
    async fn replaying_a_captured_envelope_does_not_spend_its_signers_quota() {
        let operator_key = PrivateKey::new_key();
        let trusted = rate_limit::parse_trusted_proxies("127.0.0.1").unwrap();
        let hub = spawn_hub_with_trusted_proxies(operator_key, dead_address().await, trusted).await;

        let victim = PrivateKey::new_key();
        let task_id = Uuid::new_v4();
        let path = format!("/tasks/{task_id}/claim");

        // One legitimate request, whose envelope the attacker captures.
        let captured = envelope(&victim, &path, handlers::ClaimPayload { task_id });
        let status = hub
            .client
            .post(format!("{}{path}", hub.base_url))
            .header("x-forwarded-for", "203.0.113.1")
            .json(&captured)
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, reqwest::StatusCode::NOT_FOUND, "accepted, then the task is missing");

        // Replayed far more times than the victim's whole budget. Every
        // one of these is refused as a replay; the question is what they
        // cost the victim.
        let floods = rate_limit::MAX_SIGNED_REQUESTS_PER_PUBKEY_PER_WINDOW + 10;
        for i in 0..floods {
            let status = hub
                .client
                .post(format!("{}{path}", hub.base_url))
                .header("x-forwarded-for", format!("198.51.100.{}", i % 256))
                .json(&captured)
                .send()
                .await
                .unwrap()
                .status();
            assert_eq!(
                status,
                reqwest::StatusCode::UNAUTHORIZED,
                "a replay is refused as a replay, before and after the fix"
            );
        }

        // The victim signs something fresh. It must still be served: its
        // quota should have been touched exactly once, by the one request
        // it actually made.
        let fresh = envelope(&victim, &path, handlers::ClaimPayload { task_id });
        let status = hub
            .client
            .post(format!("{}{path}", hub.base_url))
            .header("x-forwarded-for", "203.0.113.2")
            .json(&fresh)
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(
            status,
            reqwest::StatusCode::NOT_FOUND,
            "the victim must still have its budget -- a 429 here means someone else spent it by \
             replaying an envelope they captured"
        );
    }

    /// The dashboard polls the full task list on a timer, and that JSON
    /// gzips well -- so the compression layer is load-bearing for the
    /// site's latency, not an optimization detail. reqwest here has no
    /// `gzip` feature, which is exactly what the test needs: nothing
    /// auto-sends `Accept-Encoding` or strips `Content-Encoding` before
    /// the assertion can see it. `/llms.txt` is the target because it is
    /// reliably larger than the layer's minimum-size threshold on a
    /// fresh hub, where `/tasks` is a 2-byte `[]`.
    #[tokio::test]
    async fn responses_gzip_when_asked_and_stay_identity_when_not() {
        let operator_key = PrivateKey::new_key();
        let hub = spawn_hub(operator_key, dead_address().await).await;

        let compressed = hub
            .client
            .get(format!("{}/llms.txt", hub.base_url))
            .header("accept-encoding", "gzip")
            .send()
            .await
            .unwrap();
        assert_eq!(compressed.status(), reqwest::StatusCode::OK);
        assert_eq!(
            compressed.headers().get("content-encoding").and_then(|v| v.to_str().ok()),
            Some("gzip"),
        );

        // A client that never asked must get the bytes as they are --
        // the SDK's agents and curl without flags both read this API.
        let identity = hub.client.get(format!("{}/llms.txt", hub.base_url)).send().await.unwrap();
        assert_eq!(identity.status(), reqwest::StatusCode::OK);
        assert!(identity.headers().get("content-encoding").is_none());
        assert!(identity.text().await.unwrap().contains("hash_match"));
    }

    /// Gives `agent` a paid, completed task so it earns a reputation
    /// record -- which is what makes it an agent the naming registry is
    /// willing to name.
    async fn seed_completed_task_for(hub: &TestHub, agent: &PublicKey, bounty: u64) {
        let expected_output = Hash::hash_bytes(b"x");
        let mut board = hub.state.board.write().await;
        let task = board.create_task(hub.state.operator_public_key.clone(), "t".to_string(), bounty, expected_output);
        board.claim_task(task.id, agent.clone(), Utc::now() + chrono::Duration::minutes(5)).unwrap();
        board.submit(task.id, agent.clone(), expected_output).unwrap();
        board.mark_recipient_paid(task.id, agent, bounty).unwrap();
    }

    #[tokio::test]
    async fn leaderboard_names_every_agent_uniquely_and_stably() {
        let operator_key = PrivateKey::new_key();
        let hub = spawn_hub(operator_key, dead_address().await).await;

        let agents: Vec<PublicKey> = (0..5).map(|_| PrivateKey::new_key().public_key()).collect();
        for (i, agent) in agents.iter().enumerate() {
            seed_completed_task_for(&hub, agent, 100 + i as u64).await;
        }

        let fetch = || async {
            hub.client.get(format!("{}/leaderboard", hub.base_url)).send().await.unwrap().json::<Value>().await.unwrap()
        };

        let first: Value = fetch().await;
        let entries = first.as_array().unwrap();
        assert_eq!(entries.len(), agents.len());

        let mut seen = std::collections::HashSet::new();
        for entry in entries {
            let name = entry["name"].as_str().expect("every agent gets a name");
            assert!(
                name.chars().count() <= names::MAX_NAME_LEN,
                "{name} is longer than the {} character cap",
                names::MAX_NAME_LEN
            );
            assert!(name.chars().all(|c| c.is_ascii_alphabetic()));
            assert!(name.chars().next().unwrap().is_ascii_uppercase(), "{name} should be CamelCase");
            assert!(seen.insert(name.to_string()), "{name} was assigned twice");
        }

        // A second request must return the same names -- a leaderboard
        // whose agents were renamed between two page loads would be
        // worse than one with no names at all.
        let second: Value = fetch().await;
        for (before, after) in entries.iter().zip(second.as_array().unwrap()) {
            assert_eq!(before["pubkey"], after["pubkey"]);
            assert_eq!(before["name"], after["name"]);
        }

        // and they're durable: the same names came back out of the store
        let stored = hub.state.store.load_all_agent_names().unwrap();
        assert_eq!(stored.len(), agents.len());
        for (pubkey, name) in stored {
            let entry = entries
                .iter()
                .find(|e| e["pubkey"] == pubkey.to_string())
                .expect("a persisted name for an agent that isn't on the board");
            assert_eq!(entry["name"], name);
        }
    }

    #[tokio::test]
    async fn get_reputation_reports_a_name_but_never_mints_one() {
        let operator_key = PrivateKey::new_key();
        let hub = spawn_hub(operator_key, dead_address().await).await;

        // A pubkey with no history on the board. The route resolves it
        // (any pubkey does) but must not spend a name on it -- this is
        // an unauthenticated GET, so minting here would let anyone drain
        // the pool one request at a time.
        let stranger = PrivateKey::new_key().public_key();
        let dto: Value = hub
            .client
            .get(format!("{}/reputation/{stranger}", hub.base_url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(dto["completed"], 0);
        assert_eq!(dto["name"], Value::Null, "a stranger must not be named");
        assert!(hub.state.names.read().await.is_empty());
        assert!(hub.state.store.load_all_agent_names().unwrap().is_empty());

        // An agent that has actually worked gets named by /leaderboard,
        // and /reputation then reports that same name.
        let agent = PrivateKey::new_key().public_key();
        seed_completed_task_for(&hub, &agent, 500).await;
        let leaderboard: Value =
            hub.client.get(format!("{}/leaderboard", hub.base_url)).send().await.unwrap().json().await.unwrap();
        let assigned = leaderboard.as_array().unwrap()[0]["name"].clone();
        assert!(assigned.is_string());

        let dto: Value = hub
            .client
            .get(format!("{}/reputation/{agent}", hub.base_url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(dto["name"], assigned);
    }

    /// `net_worth` (current on-chain balance) and `total_earned`
    /// (lifetime cumulative payout) are deliberately different numbers --
    /// this spends some of what it earned, then checks the leaderboard
    /// reports both, not just one standing in for the other.
    #[tokio::test]
    async fn leaderboard_reports_net_worth_separately_from_lifetime_earnings() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let agent_key = PrivateKey::new_key();

        let expected_output = Hash::hash_bytes(b"x");
        let mut board = hub.state.board.write().await;
        let task = board.create_task(hub.state.operator_public_key.clone(), "t".to_string(), 500, expected_output);
        board
            .claim_task(task.id, agent_key.public_key(), Utc::now() + chrono::Duration::minutes(5))
            .unwrap();
        board.submit(task.id, agent_key.public_key(), expected_output).unwrap();
        board.mark_recipient_paid(task.id, &agent_key.public_key(), 500).unwrap();
        drop(board);

        // Confirmed on-chain balance is a separate fact from the payout
        // above -- set it to something that doesn't match `total_earned`,
        // as if the agent had already spent some of what it earned.
        fake_node.fund(agent_key.public_key(), 7_000_000).await;

        let leaderboard: Value = hub
            .client
            .get(format!("{}/leaderboard", hub.base_url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let entry = leaderboard
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["pubkey"] == agent_key.public_key().to_string())
            .expect("agent should be on the leaderboard");
        assert_eq!(entry["total_earned"], 500);
        assert_eq!(entry["net_worth"], 7_000_000);
    }

    /// Net worth ranks the field like any other column -- which is the
    /// whole point of it being a column, and is not free: it is the one
    /// ordering the board cannot answer from memory, so the hub prices
    /// every agent once (see `handlers::net_worth_snapshot`) instead of
    /// reordering the fifty rows it was already going to serve. This
    /// gives the two agents opposite standings on earnings and on
    /// balance, so a ranking that quietly fell back to earnings would
    /// come out exactly reversed.
    #[tokio::test]
    async fn leaderboard_ranks_the_field_by_net_worth_when_asked_to() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let earner = PrivateKey::new_key();
        let holder = PrivateKey::new_key();

        let expected_output = Hash::hash_bytes(b"x");
        let mut board = hub.state.board.write().await;
        for (agent, bounty) in [(&earner, 900u64), (&holder, 100u64)] {
            let task =
                board.create_task(hub.state.operator_public_key.clone(), "t".to_string(), bounty, expected_output);
            board
                .claim_task(task.id, agent.public_key(), Utc::now() + chrono::Duration::minutes(5))
                .unwrap();
            board.submit(task.id, agent.public_key(), expected_output).unwrap();
            board.mark_recipient_paid(task.id, &agent.public_key(), bounty).unwrap();
        }
        drop(board);

        // The earner spent nearly everything; the holder is sitting on a
        // balance it never earned here. Lifetime earnings and current
        // balance now disagree about who is ahead.
        fake_node.fund(earner.public_key(), 1_000).await;
        fake_node.fund(holder.public_key(), 9_000).await;

        let order = |query: &'static str| {
            let client = hub.client.clone();
            let base_url = hub.base_url.clone();
            async move {
                let page: Value = client
                    .get(format!("{base_url}/leaderboard{query}"))
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                page.as_array()
                    .unwrap()
                    .iter()
                    .map(|entry| (entry["pubkey"].as_str().unwrap().to_string(), entry["rank"].clone()))
                    .collect::<Vec<_>>()
            }
        };

        let by_earnings = order("").await;
        assert_eq!(by_earnings[0].0, earner.public_key().to_string(), "earnings is still the default");

        let by_net_worth = order("?sort=net_worth").await;
        assert_eq!(by_net_worth[0].0, holder.public_key().to_string(), "richest first");
        assert_eq!(by_net_worth[0].1, 1, "and it is ranked first, not merely listed first");
        assert_eq!(by_net_worth[1].0, earner.public_key().to_string());

        // Ascending is the same question the other way, and is what
        // makes the column worth two clicks rather than one.
        let poorest_first = order("?sort=net_worth&dir=asc").await;
        assert_eq!(poorest_first[0].0, earner.public_key().to_string());
    }

    /// Ranking by net worth against a node that cannot be reached
    /// answers anyway: every agent prices to nothing, so the column has
    /// no ordering to give and the field falls back to the pubkey
    /// tiebreak -- a stable order, a full page, and a `null` in the
    /// column, rather than an error page because the chain was busy.
    /// Same posture the unsorted route already takes (see
    /// `leaderboard_reports_null_net_worth_when_the_node_is_unreachable`);
    /// the sweep must not turn a degraded read into a failed one.
    #[tokio::test]
    async fn leaderboard_ranked_by_net_worth_survives_an_unreachable_node() {
        let operator_key = PrivateKey::new_key();
        let hub = spawn_hub(operator_key.clone(), dead_address().await).await;

        let expected_output = Hash::hash_bytes(b"x");
        let mut board = hub.state.board.write().await;
        let agents: Vec<PrivateKey> = (0..3).map(|_| PrivateKey::new_key()).collect();
        for agent in &agents {
            let task = board.create_task(operator_key.public_key(), "t".to_string(), 100, expected_output);
            board
                .claim_task(task.id, agent.public_key(), Utc::now() + chrono::Duration::minutes(5))
                .unwrap();
            board.submit(task.id, agent.public_key(), expected_output).unwrap();
            board.mark_recipient_paid(task.id, &agent.public_key(), 100).unwrap();
        }
        drop(board);

        let page = |query: &'static str| {
            let client = hub.client.clone();
            let base_url = hub.base_url.clone();
            async move {
                let response = client.get(format!("{base_url}/leaderboard{query}")).send().await.unwrap();
                assert_eq!(response.status(), 200);
                response.json::<Value>().await.unwrap()
            }
        };

        let first = page("?sort=net_worth").await;
        let entries = first.as_array().unwrap();
        assert_eq!(entries.len(), agents.len(), "every agent is still served");
        assert!(entries.iter().all(|entry| entry["net_worth"] == Value::Null));

        // Stable across requests, which is what the pubkey tiebreak is
        // for: without it a page boundary could serve one agent twice.
        let again = page("?sort=net_worth").await;
        assert_eq!(first, again);
    }

    #[tokio::test]
    async fn leaderboard_reports_null_net_worth_when_the_node_is_unreachable() {
        let operator_key = PrivateKey::new_key();
        let hub = spawn_hub(operator_key.clone(), dead_address().await).await;
        let agent_key = PrivateKey::new_key();

        let expected_output = Hash::hash_bytes(b"x");
        let mut board = hub.state.board.write().await;
        let task = board.create_task(operator_key.public_key(), "t".to_string(), 500, expected_output);
        board
            .claim_task(task.id, agent_key.public_key(), Utc::now() + chrono::Duration::minutes(5))
            .unwrap();
        board.submit(task.id, agent_key.public_key(), expected_output).unwrap();
        board.mark_recipient_paid(task.id, &agent_key.public_key(), 500).unwrap();
        drop(board);

        let leaderboard: Value = hub
            .client
            .get(format!("{}/leaderboard", hub.base_url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let entry = leaderboard
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["pubkey"] == agent_key.public_key().to_string())
            .expect("agent should still be on the leaderboard even though the node is down");
        assert_eq!(entry["total_earned"], 500, "reputation is unaffected by the node being unreachable");
        assert_eq!(entry["net_worth"], Value::Null);
    }

    #[tokio::test]
    async fn get_reputation_includes_net_worth_alongside_the_single_pubkey_lookup() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let agent_key = PrivateKey::new_key();
        fake_node.fund(agent_key.public_key(), 3_500_000).await;

        let reputation: Value = hub
            .client
            .get(format!("{}/reputation/{}", hub.base_url, agent_key.public_key()))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(reputation["completed"], 0, "never did any work, but should still resolve a balance");
        assert_eq!(reputation["net_worth"], 3_500_000);
    }

    #[tokio::test]
    async fn try_settle_verified_task_fails_gracefully_when_node_unreachable() {
        let operator_key = PrivateKey::new_key();
        let hub = spawn_hub(operator_key, dead_address().await).await;
        let (task_id, _claimant) = seed_verified_task(&hub.state, 1_000).await;

        let paid = handlers::try_settle_verified_task(&hub.state, task_id).await;
        assert!(!paid);

        let status = hub.state.board.read().await.get_task(task_id).unwrap().status;
        assert_eq!(status, TaskStatus::Verified, "must remain retryable, not stuck or lost");
    }

    #[tokio::test]
    async fn try_settle_verified_task_pays_out_and_marks_paid() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let (task_id, claimant) = seed_verified_task(&hub.state, 1_000).await;

        let submitted_ok = handlers::try_settle_verified_task(&hub.state, task_id).await;
        assert!(submitted_ok);

        let status = hub.state.board.read().await.get_task(task_id).unwrap().status;
        assert_eq!(
            status,
            TaskStatus::Submitted,
            "sent is not paid -- the hub has had no answer from the node yet"
        );
        assert_eq!(hub.state.board.read().await.reputation(&claimant).completed, 0);

        let submitted = fake_node.wait_for_submitted_count(1).await;
        assert_eq!(submitted.len(), 1);
        let recipient_output = submitted[0].outputs.iter().find(|o| o.pubkey == claimant);
        assert_eq!(recipient_output.unwrap().value, 1_000);

        confirm_submitted_payouts(&hub.state, &fake_node).await;
        let status = hub.state.board.read().await.get_task(task_id).unwrap().status;
        assert_eq!(status, TaskStatus::Paid, "and now the chain has been asked");
        assert_eq!(hub.state.board.read().await.reputation(&claimant).completed, 1);
        assert_eq!(
            fake_node.submitted_transactions().await.len(),
            1,
            "confirming must never put a second transaction on the wire"
        );
    }

    /// The wire shape agents actually read. A task whose payout is in
    /// flight must say so on the wire, not just in the hub's memory --
    /// `status` for which of the three reasons the money is outstanding,
    /// and the two totals for how much.
    #[tokio::test]
    async fn a_task_reports_pending_against_confirmed_over_http() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let (task_id, _claimant) = seed_verified_task(&hub.state, 1_000).await;

        let task_dto = |id: Uuid| {
            let hub_client = hub.client.clone();
            let base_url = hub.base_url.clone();
            async move {
                hub_client
                    .get(format!("{base_url}/tasks/{id}"))
                    .send()
                    .await
                    .unwrap()
                    .json::<Value>()
                    .await
                    .unwrap()
            }
        };

        let before = task_dto(task_id).await;
        assert_eq!(before["status"], "Verified");
        assert_eq!(before["bounty_pending"], 1_000, "owed, and nothing sent yet");
        assert_eq!(before["bounty_confirmed"], 0);

        assert!(handlers::try_settle_verified_task(&hub.state, task_id).await);
        let in_flight = task_dto(task_id).await;
        assert_eq!(in_flight["status"], "Submitted");
        assert_eq!(
            in_flight["bounty_pending"], 1_000,
            "sent is not confirmed -- the money is still outstanding"
        );
        assert_eq!(in_flight["bounty_confirmed"], 0);

        confirm_submitted_payouts(&hub.state, &fake_node).await;
        let settled = task_dto(task_id).await;
        assert_eq!(settled["status"], "Paid");
        assert_eq!(settled["bounty_pending"], 0);
        assert_eq!(settled["bounty_confirmed"], 1_000);
    }

    /// The new states have to be selectable, or `GET /tasks` cannot show
    /// an operator what is stuck -- which is most of what the states are
    /// for.
    #[tokio::test]
    async fn status_filter_accepts_the_settlement_states() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let (task_id, _) = seed_verified_task(&hub.state, 1_000).await;
        assert!(handlers::try_settle_verified_task(&hub.state, task_id).await);

        let listed: Value = hub
            .client
            .get(format!("{}/tasks?status=submitted", hub.base_url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let listed = listed.as_array().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0]["id"], task_id.to_string());

        for status in ["payoutfailed", "PayoutFailed", "Submitted"] {
            let resp = hub
                .client
                .get(format!("{}/tasks?status={status}", hub.base_url))
                .send()
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                reqwest::StatusCode::OK,
                "?status={status} must be accepted, case-insensitively like every other"
            );
        }
    }

    /// The failure this whole mechanism exists for: a transaction the
    /// node never received. Nothing about the send says so -- writing to
    /// a socket the peer has closed succeeds, and the protocol has no
    /// reply either way -- so the only evidence is the chain itself, and
    /// before this the hub never went looking.
    ///
    /// The old behaviour is worth stating precisely, because it is what
    /// this replaces: the task went straight to `Paid`, `pending_payouts`
    /// stopped answering, the sweep stopped visiting it, and the bounty
    /// was gone with no log line anywhere.
    #[tokio::test]
    async fn a_payout_the_node_never_received_is_detected_and_resent() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let (task_id, claimant) = seed_verified_task(&hub.state, 1_000).await;

        fake_node.set_fate(SubmissionFate::Swallowed).await;
        assert!(handlers::try_settle_verified_task(&hub.state, task_id).await);
        assert_eq!(
            hub.state.board.read().await.get_task(task_id).unwrap().status,
            TaskStatus::Submitted,
            "the hub believes it sent something, because it did"
        );
        // Wait for the node to have *handled* the send before asserting
        // it kept nothing, and before changing its behaviour below.
        // Without this the assertion passes vacuously and the fate
        // change lands on the message still in flight.
        fake_node.wait_for_submissions_seen(1).await;
        assert!(
            fake_node.submitted_transactions().await.is_empty(),
            "and the node has nothing -- which is the entire problem"
        );

        // The node is healthy again. One resolution pass is all it takes
        // to notice: the recipient holds nothing, and every input the
        // transaction would have spent is still sitting unspent and
        // unmarked at the operator's address.
        fake_node.set_fate(SubmissionFate::Mined).await;
        let lost = hub.state.board.read().await.outstanding_payout_attempts();
        assert_eq!(lost.len(), 1);
        assert_eq!(lost[0].submissions, 1);
        assert_eq!(
            lost[0].resolve(
                &fake_node
                    .outputs_of(&claimant)
                    .await
                    .into_iter()
                    .map(|(o, m)| (m, o))
                    .collect::<Vec<_>>(),
                &fake_node
                    .outputs_of(&hub.state.operator_public_key)
                    .await
                    .into_iter()
                    .map(|(o, m)| (m, o))
                    .collect::<Vec<_>>(),
            ),
            board::PayoutOutcome::NeverLanded
        );
        handlers::resolve_payout_attempt(&hub.state, &lost[0]).await;

        let resent = fake_node.wait_for_submitted_count(1).await;
        assert_eq!(resent.len(), 1, "the lost payout must actually be resent");
        assert!(resent[0].outputs.iter().any(|o| o.pubkey == claimant && o.value == 1_000));
        let attempts = hub.state.board.read().await.outstanding_payout_attempts();
        assert_eq!(attempts[0].submissions, 2, "and counted, so the budget is finite");
        assert_ne!(
            attempts[0].output_hash, lost[0].output_hash,
            "a rebuild is a new attempt -- it must not be confirmable by evidence of the old one"
        );

        // ...and the resend confirms, which is what makes this a
        // recovery rather than a detection.
        confirm_submitted_payouts(&hub.state, &fake_node).await;
        assert_eq!(
            hub.state.board.read().await.get_task(task_id).unwrap().status,
            TaskStatus::Paid
        );
        assert_eq!(hub.state.board.read().await.reputation(&claimant).total_earned, 1_000);
    }

    /// A confirmed payout is recorded once, however many times it is
    /// resolved. Two overlapping sweeps are the realistic way this
    /// happens, and crediting reputation or minting compute twice for
    /// one payment would be its own quiet corruption.
    #[tokio::test]
    async fn a_confirmed_payout_is_marked_paid_exactly_once() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let (task_id, claimant) = seed_verified_task(&hub.state, 1_000).await;

        assert!(handlers::try_settle_verified_task(&hub.state, task_id).await);
        fake_node.wait_for_submitted_count(1).await;
        let attempt = hub.state.board.read().await.outstanding_payout_attempts()[0].clone();

        // The same attempt resolved three times over, which is exactly
        // what a sweep holding a snapshot taken before its node reads
        // would do if two of them overlapped.
        for _ in 0..3 {
            handlers::resolve_payout_attempt(&hub.state, &attempt).await;
        }

        let board = hub.state.board.read().await;
        assert_eq!(board.get_task(task_id).unwrap().status, TaskStatus::Paid);
        assert_eq!(board.reputation(&claimant).completed, 1);
        assert_eq!(board.reputation(&claimant).total_earned, 1_000);
        assert!(board.outstanding_payout_attempts().is_empty());
        drop(board);
        assert_eq!(
            fake_node.submitted_transactions().await.len(),
            1,
            "resolving a payout must never put money on the wire"
        );
    }

    /// The row that must never collapse into either neighbour: the
    /// output is absent, but the node's mempool has spoken for the
    /// inputs. The transaction is most likely queued for the next block.
    ///
    /// Resending would be the duplicate the node answers with a strike,
    /// and three strikes in ten minutes bans this box from its own node
    /// (plan §6.2). Marking it paid would be the original lie. So the
    /// hub waits, and keeps saying it is waiting.
    ///
    /// This particular staging turns out to be *doubly* defended, which
    /// is worth knowing rather than assuming: even if the three-way rule
    /// were broken so that this read as lost, `build_multi_payment`
    /// skips outputs the mempool has marked, so the rebuild would fail
    /// for want of funds rather than duplicate the payment. That is the
    /// same property §6.5 names as load-bearing, doing real work on a
    /// path it was not written for. It also means this test alone does
    /// not prove the rule -- see
    /// `a_bounty_the_recipient_already_spent_is_never_paid_a_second_time`
    /// for the ambiguous case where the operator does have spendable
    /// change and a wrong answer really does pay twice.
    #[tokio::test]
    async fn an_unresolvable_payout_stays_submitted_and_is_never_resent() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let (task_id, claimant) = seed_verified_task(&hub.state, 1_000).await;

        fake_node.set_fate(SubmissionFate::HeldInMempool).await;
        assert!(handlers::try_settle_verified_task(&hub.state, task_id).await);
        fake_node.wait_for_submitted_count(1).await;
        assert_eq!(
            fake_node.balance_of(&hub.state.operator_public_key).await,
            0,
            "the node has marked the operator's output: it is holding a transaction spending it"
        );

        let attempt = hub.state.board.read().await.outstanding_payout_attempts()[0].clone();
        for _ in 0..5 {
            handlers::resolve_payout_attempt(&hub.state, &attempt).await;
        }

        let board = hub.state.board.read().await;
        assert_eq!(
            board.get_task(task_id).unwrap().status,
            TaskStatus::Submitted,
            "still waiting -- neither confirmed nor provably lost"
        );
        assert_eq!(board.reputation(&claimant).completed, 0);
        assert_eq!(
            board.outstanding_payout_attempts()[0].submissions,
            1,
            "and no amount of asking turns waiting into another attempt"
        );
        drop(board);
        assert_eq!(
            fake_node.submitted_transactions().await.len(),
            1,
            "a second transaction here risks paying the bounty twice"
        );
    }

    /// A payout proven lost `MAX_PAYOUT_SUBMISSIONS` times stops being
    /// retried and says so. The budget exists because each retry only
    /// fires after a *proof* the money never moved -- repeated proof is
    /// not a transient, it means the hub is building something the node
    /// will not take, and a fifth identical attempt would fail
    /// identically while hammering a node that is already unwell.
    #[tokio::test]
    async fn a_payout_lost_too_many_times_is_abandoned_with_the_money_still_owed() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let (task_id, claimant) = seed_verified_task(&hub.state, 1_000).await;

        // A node that takes the bytes and does nothing with them, every
        // time -- the shape of a rejection, or of a node restarting on a
        // loop and discarding its mempool with each one.
        fake_node.set_fate(SubmissionFate::Swallowed).await;
        assert!(handlers::try_settle_verified_task(&hub.state, task_id).await);

        for expected in 1..=board::MAX_PAYOUT_SUBMISSIONS {
            // Every attempt so far must have reached the node and been
            // dropped by it before the next resolution asks about it.
            fake_node.wait_for_submissions_seen(expected as usize).await;
            let attempts = hub.state.board.read().await.outstanding_payout_attempts();
            assert_eq!(attempts.len(), 1);
            assert_eq!(attempts[0].submissions, expected);
            handlers::resolve_payout_attempt(&hub.state, &attempts[0]).await;
        }
        assert!(
            fake_node.submitted_transactions().await.is_empty(),
            "the node kept none of them -- which is what made each one provably lost"
        );

        let board = hub.state.board.read().await;
        let task = board.get_task(task_id).unwrap();
        assert_eq!(task.status, TaskStatus::PayoutFailed);
        assert_eq!(
            task.unconfirmed_payout_total(),
            1_000,
            "abandoned is not forgiven -- the worker is still owed the bounty"
        );
        assert_eq!(task.confirmed_payout_total(), 0);
        assert_eq!(board.reputation(&claimant).completed, 0, "and was never credited for it");
        assert!(board.outstanding_payout_attempts().is_empty(), "nothing is still being polled");
        assert!(
            board.unsubmitted_payouts(task_id).is_empty(),
            "and nothing will be sent again without an operator"
        );
        drop(board);
        // The store has to be emptied too, or the attempt comes back at
        // the next restart attached to a task that is now terminal and
        // is re-resolved, and re-logged as a failure, every sweep after.
        assert!(
            hub.state.store.load_all_payout_attempts().unwrap().is_empty(),
            "an abandoned task must leave nothing behind for a restart to resurrect"
        );
    }

    /// A payout in flight has to survive a restart, or the hub simply
    /// stops looking for it -- which is the pre-existing bug wearing a
    /// different hat. The node's mempool is memory-only, so a restart is
    /// exactly when a payout is most likely to have been lost.
    #[tokio::test]
    async fn a_payout_in_flight_survives_a_restart_and_is_still_resolved() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key.clone(), fake_node.addr.clone()).await;
        let (task_id, claimant) = seed_verified_task(&hub.state, 1_000).await;

        fake_node.set_fate(SubmissionFate::Swallowed).await;
        assert!(handlers::try_settle_verified_task(&hub.state, task_id).await);
        fake_node.wait_for_submissions_seen(1).await;
        let before = hub.state.board.read().await.outstanding_payout_attempts();
        assert_eq!(before.len(), 1);

        // What a restart actually restores: a fresh board filled from
        // the store, nothing carried over in memory.
        let mut restored = TaskBoard::new();
        for task in hub.state.store.load_all_tasks().unwrap() {
            restored.restore_task(task);
        }
        for attempt in hub.state.store.load_all_payout_attempts().unwrap() {
            restored.restore_payout_attempt(attempt);
        }

        assert_eq!(
            restored.get_task(task_id).unwrap().status,
            TaskStatus::Submitted,
            "the task must come back still waiting, not as Paid and not as Verified"
        );
        assert_eq!(restored.outstanding_payout_attempts(), before);
        assert!(
            restored.unsubmitted_payouts(task_id).is_empty(),
            "and a restart must not re-send a payout that may still be in a mempool"
        );
        assert_eq!(restored.reputation(&claimant).completed, 0);
    }

    /// Why `save_confirmed_payout` has to be one transaction, stated as
    /// the state its absence produced rather than as prose: a
    /// `Submitted` task on disk with no attempt tracking it, which is
    /// what a crash between the old code's first commit (the attempt
    /// deletion) and its second (the task) left behind.
    ///
    /// Walking the selectors turns out to say something sharper than
    /// "the payout is forgotten", and the assertions below are written
    /// to record it. Neither sweep pass will touch such a task -- the
    /// resolution pass reads `outstanding_payout_attempts`, which is
    /// empty, and the settlement pass reads `verified_unpaid_tasks`,
    /// which takes only `Verified`. But `unsubmitted_payouts` still
    /// names the payout as owed, and `try_settle_verified_task` does
    /// accept a `Submitted` task. So the recovery path exists and is
    /// never called; and if an operator called it by hand it would
    /// **re-send a payout that already confirmed on chain**, because the
    /// attempt that was the double-spend guard is exactly what got
    /// deleted. Not a state any ordering fixes -- the reverse order
    /// strands a resolved payout that is re-resolved every sweep forever
    /// -- which is why the fix is a transaction.
    ///
    /// Mirrors
    /// `store::tests::two_separate_commits_leave_a_window_where_the_task_exists_alone`:
    /// the suite states the mechanism of the bug, not just its cure.
    #[tokio::test]
    async fn a_submitted_task_with_no_attempt_is_recovered_by_nothing() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key.clone(), fake_node.addr.clone()).await;
        let (task_id, claimant) = seed_verified_task(&hub.state, 1_000).await;

        fake_node.set_fate(SubmissionFate::Swallowed).await;
        assert!(handlers::try_settle_verified_task(&hub.state, task_id).await);
        fake_node.wait_for_submissions_seen(1).await;

        // The first of the old two commits, and nothing after it. In the
        // real failure the transaction had already confirmed on chain --
        // that is what `record_confirmed_payout` is called about -- so
        // what is reconstructed here is the store's state, not the
        // chain's.
        hub.state.store.delete_payout_attempt(task_id, &claimant).unwrap();

        let restored = board_as_a_restart_would_load_it(&hub);
        assert_eq!(
            restored.get_task(task_id).unwrap().status,
            TaskStatus::Submitted,
            "the task is mid-settlement on disk, which is the whole problem"
        );
        assert!(
            restored.outstanding_payout_attempts().is_empty(),
            "the resolution pass reads this, and there is nothing in it"
        );
        assert!(
            restored.verified_unpaid_tasks().is_empty(),
            "and the settlement pass will not take a Submitted task -- by design, since \
             Submitted means nothing is left unsent"
        );
        // So no sweep pass will ever look at this task again. The
        // payout is still listed as owed, which sounds like a way back
        // and is worse than none: nothing consults the list, and the
        // attempt that would have stopped a second send is the record
        // that was deleted.
        assert_eq!(
            restored.unsubmitted_payouts(task_id).len(),
            1,
            "the payout still reads as owed, so a hand-run settlement would send it again -- \
             against a transaction that already confirmed, with its double-spend guard gone"
        );
    }

    /// The invariant `try_settle_verified_task`'s refusal rests on:
    /// **a `Submitted` task never has anything unsent.**
    ///
    /// If this is ever false, refusing to send becomes a bug rather than
    /// a safety measure -- so it is pinned rather than argued. The proof
    /// in the code is that `record_payout_attempt` is the only path into
    /// `Submitted` and requires the set to be empty, and nothing
    /// afterwards can grow it; what this walks is the one case that
    /// looks like a counterexample, a multi-winner task with one leg
    /// confirmed and its attempt cleared while another is still in
    /// flight.
    #[tokio::test]
    async fn a_submitted_task_never_has_an_unsent_payout() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let poster_key = PrivateKey::new_key();
        let assignee1 = PrivateKey::new_key();
        let assignee2 = PrivateKey::new_key();

        let payload = handlers::EscrowConsensusTaskPayload {
            description: "two winners, confirmed one at a time".to_string(),
            bounty: 900,
            num_assignees: 2,
            join_window_minutes: 60,
            submission_window_minutes: 30,
            min_reputation: 0,
            capabilities: Default::default(),
        };
        let reservation: Value = hub
            .client
            .post(format!("{}/tasks/consensus/escrow", hub.base_url))
            .json(&envelope(&poster_key, "/tasks/consensus/escrow", payload))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let escrow_id: Uuid = reservation["escrow_id"].as_str().unwrap().parse().unwrap();
        fake_node
            .fund(
                parse_pubkey(reservation["deposit_address"].as_str().unwrap()),
                reservation["required_amount"].as_u64().unwrap(),
            )
            .await;
        let task: Value = hub
            .client
            .post(format!("{}/tasks/escrow/{escrow_id}/confirm", hub.base_url))
            .json(&envelope(&poster_key, &format!("/tasks/escrow/{escrow_id}/confirm"), handlers::ConfirmEscrowPayload { escrow_id }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let task_id: Uuid = task["id"].as_str().unwrap().parse().unwrap();

        for assignee in [&assignee1, &assignee2] {
            hub.client
                .post(format!("{}/tasks/{task_id}/claim", hub.base_url))
                .json(&envelope(assignee, &format!("/tasks/{task_id}/claim"), handlers::ClaimPayload { task_id }))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
        }
        for assignee in [&assignee1, &assignee2] {
            hub.client
                .post(format!("{}/tasks/{task_id}/submit", hub.base_url))
                .json(&envelope(assignee, &format!("/tasks/{task_id}/submit"), handlers::SubmitPayload { task_id, output: "42".to_string() }))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
        }

        // Both legs are on the wire, so the task is Submitted and the
        // set is empty -- the entry condition, which is the easy half.
        let assert_invariant = |label: &'static str| {
            let hub = &hub;
            async move {
                let board = hub.state.board.read().await;
                if board.get_task(task_id).unwrap().status == TaskStatus::Submitted {
                    assert!(
                        board.unsubmitted_payouts(task_id).is_empty(),
                        "{label}: a Submitted task with something unsent would make \
                         try_settle_verified_task's refusal a bug"
                    );
                }
            }
        };
        assert_eq!(
            hub.state.board.read().await.get_task(task_id).unwrap().status,
            TaskStatus::Submitted,
            "both winners' transactions went out, so nothing is left unsent"
        );
        assert_invariant("both in flight").await;

        // Now the interesting half: confirm exactly one winner, which
        // marks them paid and clears *their* attempt while the other is
        // still in flight. The task stays Submitted, and the set has to
        // stay empty -- `owed_payouts` drops the paid recipient in the
        // same breath as the attempt going away.
        let one = hub.state.board.read().await.outstanding_payout_attempts()[0].clone();
        for _ in 0..200 {
            let landed = fake_node
                .outputs_of(&one.recipient)
                .await
                .iter()
                .any(|(output, _)| output.hash() == one.output_hash);
            if landed {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        handlers::resolve_payout_attempt(&hub.state, &one).await;

        let board = hub.state.board.read().await;
        assert_eq!(
            board.get_task(task_id).unwrap().status,
            TaskStatus::Submitted,
            "one winner paid, one still in flight -- still mid-settlement"
        );
        assert_eq!(board.outstanding_payout_attempts().len(), 1, "and one attempt left");
        drop(board);
        assert_invariant("one confirmed, one in flight").await;
    }

    /// The double-pay that the refusal prevents, driven end to end.
    ///
    /// This is the state `a_submitted_task_with_no_attempt_is_recovered
    /// _by_nothing` walks the selectors of; here the one path that *does*
    /// accept such a task is actually called, the way an operator
    /// resolving the task by hand would call it. Before the refusal it
    /// re-sent the bounty -- against a transaction that may already be
    /// on the chain, with the attempt that was the double-spend guard
    /// being exactly the record that went missing.
    #[tokio::test]
    async fn a_submitted_task_with_no_attempt_is_refused_rather_than_paid_again() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let (task_id, claimant) = seed_verified_task(&hub.state, 1_000).await;

        assert!(handlers::try_settle_verified_task(&hub.state, task_id).await);
        let sent_once = fake_node.wait_for_submitted_count(1).await;
        assert_eq!(sent_once.len(), 1, "the bounty went out once, legitimately");

        // Lose the attempt, in memory and on disk: the state the old
        // `record_confirmed_payout`'s first commit left behind when its
        // second never landed, and the state a rolled-back binary
        // reloaded.
        hub.state.board.write().await.clear_payout_attempt(task_id, &claimant);
        hub.state.store.delete_payout_attempt(task_id, &claimant).unwrap();
        {
            let board = hub.state.board.read().await;
            assert_eq!(board.get_task(task_id).unwrap().status, TaskStatus::Submitted);
            assert_eq!(
                board.unsubmitted_payouts(task_id).len(),
                1,
                "the payout reads as owed again, which is what used to make this sendable"
            );
        }

        assert!(
            !handlers::try_settle_verified_task(&hub.state, task_id).await,
            "must refuse: the evidence that would say whether this landed died with the attempt"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(
            fake_node.submitted_transactions().await.len(),
            1,
            "and above all it must not have put a second bounty on the wire"
        );
        assert_eq!(
            hub.state.metrics.payout_sends_refused.load(Ordering::Relaxed),
            1,
            "the refusal has to be visible to an operator, not just silent"
        );

        // And the sweep does not reach this task at all -- not because
        // it refuses, but because no pass selects it, which is what
        // `a_submitted_task_with_no_attempt_is_recovered_by_nothing`
        // establishes. Asserted here so the two tests cannot drift: it
        // is why the refusal above is a guard on the *hand-run* path and
        // why detection belongs to the boot reconciliation rather than
        // to a new sweep pass. The state cannot begin mid-run any more
        // (`save_confirmed_payout` is one transaction), so a hub that
        // has one loaded it, and boot is exactly when that is checked.
        let refusals_before = hub.state.metrics.payout_sends_refused.load(Ordering::Relaxed);
        run_sweep_once(&hub.state, Utc::now()).await;
        assert_eq!(
            fake_node.submitted_transactions().await.len(),
            1,
            "a sweep must not put a second bounty on the wire either"
        );
        assert_eq!(
            hub.state.metrics.payout_sends_refused.load(Ordering::Relaxed),
            refusals_before,
            "and it must not even have tried: the sweep's settlement pass reads \
             verified_unpaid_tasks, which takes only Verified"
        );
        assert_eq!(
            reconcile::reconcile(&*hub.state.board.read().await)
                .count(reconcile::Disagreement::SubmittedTaskWithNoPayoutAttempt),
            1,
            "so the reconciliation is the detector, and it does see it"
        );
    }

    #[tokio::test]
    async fn try_settle_verified_task_never_double_pays_concurrent_callers() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let (task_id, _claimant) = seed_verified_task(&hub.state, 1_000).await;

        let (a, b) = tokio::join!(
            handlers::try_settle_verified_task(&hub.state, task_id),
            handlers::try_settle_verified_task(&hub.state, task_id),
        );
        assert_eq!(
            [a, b].iter().filter(|&&x| x).count(),
            1,
            "exactly one of the two concurrent attempts should have won and paid it out"
        );

        let submitted = fake_node.wait_for_submitted_count(1).await;
        assert_eq!(
            submitted.len(),
            1,
            "must never submit two payout transactions for the same task"
        );
    }

    #[tokio::test]
    async fn run_sweep_once_pays_out_all_verified_unpaid_tasks() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let (task_a, _) = seed_verified_task(&hub.state, 500).await;
        let (task_b, _) = seed_verified_task(&hub.state, 700).await;

        run_sweep_once(&hub.state, Utc::now()).await;

        {
            let board = hub.state.board.read().await;
            assert_eq!(board.get_task(task_a).unwrap().status, TaskStatus::Submitted);
            assert_eq!(board.get_task(task_b).unwrap().status, TaskStatus::Submitted);
        }
        assert_eq!(fake_node.wait_for_submitted_count(2).await.len(), 2);

        // A second sweep, far enough past the first that the grace
        // period has elapsed, is what turns sent into paid.
        run_sweep_once(&hub.state, Utc::now() + chrono::Duration::minutes(1)).await;

        let board = hub.state.board.read().await;
        assert_eq!(board.get_task(task_a).unwrap().status, TaskStatus::Paid);
        assert_eq!(board.get_task(task_b).unwrap().status, TaskStatus::Paid);
        drop(board);
        assert_eq!(
            fake_node.submitted_transactions().await.len(),
            2,
            "the resolving sweep must not re-send payouts it is only checking on"
        );
    }

    #[tokio::test]
    async fn create_consensus_task_rejects_too_few_assignees() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;

        let payload = handlers::CreateConsensusTaskPayload {
            description: "needs redundancy".to_string(),
            bounty: 10,
            num_assignees: 1,
            join_window_minutes: 30,
            submission_window_minutes: 30,
            min_reputation: 0,
            capabilities: Default::default(),
        };
        let resp = hub
            .client
            .post(format!("{}/tasks/consensus", hub.base_url))
            .json(&envelope(&hub.operator_key, "/tasks/consensus", payload))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn full_http_consensus_task_lifecycle_pays_out_the_majority() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let agent_a = PrivateKey::new_key();
        let agent_b = PrivateKey::new_key();
        let agent_c = PrivateKey::new_key();

        let payload = handlers::CreateConsensusTaskPayload {
            description: "open-ended".to_string(),
            bounty: 900,
            num_assignees: 3,
            join_window_minutes: 30,
            submission_window_minutes: 30,
            min_reputation: 0,
            capabilities: Default::default(),
        };
        let resp = hub
            .client
            .post(format!("{}/tasks/consensus", hub.base_url))
            .json(&envelope(&hub.operator_key, "/tasks/consensus", payload))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let task: Value = resp.json().await.unwrap();
        assert_eq!(task["kind"], "consensus");
        assert_eq!(task["num_assignees"], 3);
        assert_eq!(task["assignees_joined"], 0);
        let task_id: Uuid = task["id"].as_str().unwrap().parse().unwrap();

        // all three join through the same /claim endpoint HashMatch tasks use
        for agent in [&agent_a, &agent_b, &agent_c] {
            let resp = hub
                .client
                .post(format!("{}/tasks/{task_id}/claim", hub.base_url))
                .json(&envelope(agent, &format!("/tasks/{task_id}/claim"), handlers::ClaimPayload { task_id }))
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), reqwest::StatusCode::OK);
        }

        let task: Value = hub
            .client
            .get(format!("{}/tasks/{task_id}", hub.base_url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(task["status"], "Claimed");
        assert_eq!(task["assignees_joined"], 3);

        // two agree, one doesn't -- all through the same /submit endpoint
        for (agent, answer) in [(&agent_a, "42"), (&agent_b, "42")] {
            let resp = hub
                .client
                .post(format!("{}/tasks/{task_id}/submit", hub.base_url))
                .json(&envelope(
                    agent, &format!("/tasks/{task_id}/submit"),
                    handlers::SubmitPayload { task_id, output: answer.to_string() },
                ))
                .send()
                .await
                .unwrap();
            let result: Value = resp.json().await.unwrap();
            assert_eq!(result["resolved"], false, "still waiting on the third assignee");
        }

        // the third submission completes the set and triggers resolution
        let resp = hub
            .client
            .post(format!("{}/tasks/{task_id}/submit", hub.base_url))
            .json(&envelope(
                &agent_c, &format!("/tasks/{task_id}/submit"),
                handlers::SubmitPayload { task_id, output: "disagree".to_string() },
            ))
            .send()
            .await
            .unwrap();
        let result: Value = resp.json().await.unwrap();
        assert_eq!(result["resolved"], true);
        assert_eq!(result["verified"], false, "agent_c disagreed with the majority");

        // The bounty payout is only *submitted* at this point.
        // Reputation, like everything else downstream of a payout, now
        // waits until the sweep has seen it on chain.
        confirm_submitted_payouts(&hub.state, &fake_node).await;

        let rep_a: Value = hub
            .client
            .get(format!("{}/reputation/{}", hub.base_url, agent_a.public_key()))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(rep_a["completed"], 1);
        assert_eq!(rep_a["total_earned"], 450, "900 bounty split between the 2 winners");

        let rep_c: Value = hub
            .client
            .get(format!("{}/reputation/{}", hub.base_url, agent_c.public_key()))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(rep_c["completed"], 0);
        assert_eq!(rep_c["failed"], 1, "dinged for disagreeing with the majority");

        let submitted = fake_node.wait_for_submitted_count(2).await;
        assert_eq!(submitted.len(), 2, "both winners paid, the dissenter gets nothing");
    }

    #[tokio::test]
    async fn claim_task_rejects_insufficient_reputation_via_http() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;

        let payload = handlers::CreateTaskPayload {
            description: "veterans only".to_string(),
            bounty: 10,
            expected_output_hash: hex::encode(Hash::hash_bytes(b"x").as_bytes()),
            min_reputation: 3,
            capabilities: Default::default(),
        };
        let resp = hub
            .client
            .post(format!("{}/tasks", hub.base_url))
            .json(&envelope(&hub.operator_key, "/tasks", payload))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let task: Value = resp.json().await.unwrap();
        assert_eq!(task["min_reputation"], 3);
        let task_id: Uuid = task["id"].as_str().unwrap().parse().unwrap();

        let novice = PrivateKey::new_key();
        let resp = hub
            .client
            .post(format!("{}/tasks/{task_id}/claim", hub.base_url))
            .json(&envelope(&novice, &format!("/tasks/{task_id}/claim"), handlers::ClaimPayload { task_id }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);

        let veteran = PrivateKey::new_key();
        {
            let mut board = hub.state.board.write().await;
            board.restore_reputation(
                veteran.public_key(),
                board::Reputation { completed: 3, failed: 0, total_earned: 0 },
            );
        }
        let resp = hub
            .client
            .post(format!("{}/tasks/{task_id}/claim", hub.base_url))
            .json(&envelope(&veteran, &format!("/tasks/{task_id}/claim"), handlers::ClaimPayload { task_id }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK, "veteran meets the bar and should succeed");
    }

    #[tokio::test]
    async fn run_sweep_once_persists_reputation_for_every_consensus_loser() {
        // Regression test: a deadline-triggered consensus resolution used
        // to persist the resolved task itself but never the reputation
        // dings it just applied, silently losing them on restart.
        let operator_key = PrivateKey::new_key();
        let hub = spawn_hub(operator_key, dead_address().await).await;

        let (task_id, no_show) = {
            let mut board = hub.state.board.write().await;
            // A real, positive submission window so the two legitimate
            // submissions below succeed (submission_deadline is anchored
            // at fill time, not creation time -- see
            // TaskKind::Consensus::submission_window_minutes). The window
            // having elapsed is simulated below by handing run_sweep_once
            // a future `now`, rather than by racing submit against an
            // already-past deadline.
            let task = board.create_consensus_task(
                hub.state.operator_public_key.clone(),
                "t".to_string(),
                900,
                3,
                Utc::now() + chrono::Duration::minutes(30),
                30,
            );
            let agents: Vec<PublicKey> = (0..3).map(|_| PrivateKey::new_key().public_key()).collect();
            for agent in &agents {
                board.join_consensus_task(task.id, agent.clone()).unwrap();
            }
            // two agree; the third never submits before its deadline
            board.submit_consensus_answer(task.id, agents[0].clone(), "42".to_string()).unwrap();
            board.submit_consensus_answer(task.id, agents[1].clone(), "42".to_string()).unwrap();
            (task.id, agents[2].clone())
        };

        run_sweep_once(&hub.state, Utc::now() + chrono::Duration::minutes(31)).await;
        assert_eq!(hub.state.board.read().await.get_task(task_id).unwrap().status, TaskStatus::Verified);
        assert_eq!(hub.state.board.read().await.reputation(&no_show).failed, 1);

        // The actual regression check: read it back through the store,
        // not just the in-memory board, to confirm it was really persisted.
        let persisted = hub.state.store.load_all_reputation().unwrap();
        let no_show_record = persisted.iter().find(|(pk, _)| *pk == no_show);
        assert!(
            no_show_record.is_some(),
            "the no-show's reputation ding must be persisted, not just held in memory"
        );
        assert_eq!(no_show_record.unwrap().1.failed, 1);
    }

    #[tokio::test]
    async fn create_consensus_task_rejects_non_positive_submission_window() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;

        for window in [0, -5] {
            let payload = handlers::CreateConsensusTaskPayload {
                description: "t".to_string(),
                bounty: 10,
                num_assignees: 2,
                join_window_minutes: 30,
                submission_window_minutes: window,
                min_reputation: 0,
                capabilities: Default::default(),
            };
            let resp = hub
                .client
                .post(format!("{}/tasks/consensus", hub.base_url))
                .json(&envelope(&hub.operator_key, "/tasks/consensus", payload))
                .send()
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                reqwest::StatusCode::BAD_REQUEST,
                "submission_window_minutes={window} must be rejected"
            );
        }
    }

    #[tokio::test]
    async fn create_consensus_task_rejects_non_positive_join_window() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;

        for window in [0, -5] {
            let payload = handlers::CreateConsensusTaskPayload {
                description: "t".to_string(),
                bounty: 10,
                num_assignees: 2,
                join_window_minutes: window,
                submission_window_minutes: 30,
                min_reputation: 0,
                capabilities: Default::default(),
            };
            let resp = hub
                .client
                .post(format!("{}/tasks/consensus", hub.base_url))
                .json(&envelope(&hub.operator_key, "/tasks/consensus", payload))
                .send()
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                reqwest::StatusCode::BAD_REQUEST,
                "join_window_minutes={window} must be rejected"
            );
        }
    }

    #[tokio::test]
    async fn sweep_frees_escrow_from_a_cancelled_understaffed_consensus_task() {
        // End-to-end proof that cancelling an understaffed task actually
        // does something useful: it frees the operator's escrow back up
        // for a real subsequent task creation, not just a status flip.
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 1_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;

        {
            let mut board = hub.state.board.write().await;
            board.create_consensus_task(
                hub.state.operator_public_key.clone(),
                "hogging the whole escrow".to_string(),
                1_000,
                3,
                Utc::now() - chrono::Duration::minutes(1),
                60,
            );
        }
        assert_eq!(hub.state.board.read().await.allocated_bounty(), 1_000);

        let blocked_payload = handlers::CreateTaskPayload {
            description: "blocked".to_string(),
            bounty: 1_000,
            expected_output_hash: hex::encode(Hash::hash_bytes(b"x").as_bytes()),
            min_reputation: 0,
            capabilities: Default::default(),
        };
        let resp = hub
            .client
            .post(format!("{}/tasks", hub.base_url))
            .json(&envelope(&hub.operator_key, "/tasks", blocked_payload))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST, "no escrow left");

        run_sweep_once(&hub.state, Utc::now()).await;
        assert_eq!(hub.state.board.read().await.allocated_bounty(), 0, "sweep must free the cancelled task's escrow");

        let now_allowed_payload = handlers::CreateTaskPayload {
            description: "should work now".to_string(),
            bounty: 1_000,
            expected_output_hash: hex::encode(Hash::hash_bytes(b"x").as_bytes()),
            min_reputation: 0,
            capabilities: Default::default(),
        };
        let resp = hub
            .client
            .post(format!("{}/tasks", hub.base_url))
            .json(&envelope(&hub.operator_key, "/tasks", now_allowed_payload))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK, "escrow freed, this must now succeed");
    }

    #[tokio::test]
    async fn cancel_task_rejects_non_operator() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let impostor = PrivateKey::new_key();

        let task_id = {
            let mut board = hub.state.board.write().await;
            board
                .create_task(
                    hub.state.operator_public_key.clone(),
                    "t".to_string(),
                    10,
                    Hash::hash_bytes(b"x"),
                )
                .id
        };

        let resp = hub
            .client
            .post(format!("{}/tasks/{task_id}/cancel", hub.base_url))
            .json(&envelope(&impostor, &format!("/tasks/{task_id}/cancel"), handlers::CancelPayload { task_id }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);
        assert_eq!(hub.state.board.read().await.get_task(task_id).unwrap().status, TaskStatus::Open);
    }

    #[tokio::test]
    async fn cancel_task_frees_escrow_via_http() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;

        let payload = handlers::CreateTaskPayload {
            description: "mis-posted".to_string(),
            bounty: 1_000,
            expected_output_hash: hex::encode(Hash::hash_bytes(b"x").as_bytes()),
            min_reputation: 0,
            capabilities: Default::default(),
        };
        let task: Value = hub
            .client
            .post(format!("{}/tasks", hub.base_url))
            .json(&envelope(&hub.operator_key, "/tasks", payload))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let task_id: Uuid = task["id"].as_str().unwrap().parse().unwrap();
        assert_eq!(hub.state.board.read().await.allocated_bounty(), 1_000);

        let resp = hub
            .client
            .post(format!("{}/tasks/{task_id}/cancel", hub.base_url))
            .json(&envelope(&hub.operator_key, &format!("/tasks/{task_id}/cancel"), handlers::CancelPayload { task_id }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let cancelled: Value = resp.json().await.unwrap();
        assert_eq!(cancelled["status"], "Closed");
        assert_eq!(cancelled["close_reason"], "cancelled_by_operator");
        assert_eq!(hub.state.board.read().await.allocated_bounty(), 0, "escrow freed immediately, no sweep needed");

        // cancelling an already-closed task is a conflict, not a silent no-op
        let resp = hub
            .client
            .post(format!("{}/tasks/{task_id}/cancel", hub.base_url))
            .json(&envelope(&hub.operator_key, &format!("/tasks/{task_id}/cancel"), handlers::CancelPayload { task_id }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn list_tasks_paginates_oldest_first() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let base_time = Utc::now();

        // Inserted with explicit, staggered created_at timestamps rather
        // than relying on a tight loop's real wall-clock gaps, which
        // could tie at this resolution and make the expected order
        // flaky.
        let mut ids = Vec::new();
        for i in 0..5u32 {
            let mut board = hub.state.board.write().await;
            let mut task = board.create_task(
                hub.state.operator_public_key.clone(),
                format!("task {i}"),
                10,
                Hash::hash_bytes(format!("answer{i}").as_bytes()),
            );
            task.created_at = base_time + chrono::Duration::seconds(i as i64);
            ids.push(task.id);
            board.restore_task(task);
        }

        let page: Vec<Value> = hub
            .client
            .get(format!("{}/tasks?limit=2&offset=1", hub.base_url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[0]["id"].as_str().unwrap().parse::<Uuid>().unwrap(), ids[1], "oldest-first, so offset=1 starts at the 2nd-oldest");
        assert_eq!(page[1]["id"].as_str().unwrap().parse::<Uuid>().unwrap(), ids[2]);

        let capped: Vec<Value> = hub
            .client
            .get(format!("{}/tasks?limit=999", hub.base_url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(capped.len(), 5, "only 5 exist, so a request for more just returns all of them");

        let default_page: Vec<Value> = hub
            .client
            .get(format!("{}/tasks", hub.base_url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(default_page.len(), 5, "well under the default page size");
    }

    /// Seeds one task per status the dashboard cares about, driving the
    /// board directly so each one lands in a known terminal state without
    /// going through a full HTTP lifecycle for every single status.
    async fn seed_one_task_per_status(state: &AppState) {
        let mut board = state.board.write().await;
        // Open: created and left alone.
        board.create_task(state.operator_public_key.clone(), "open".into(), 10, Hash::hash_bytes(b"a"));
        // Claimed: claimed but never submitted.
        let claimed = board.create_task(state.operator_public_key.clone(), "claimed".into(), 10, Hash::hash_bytes(b"b"));
        board
            .claim_task(claimed.id, PrivateKey::new_key().public_key(), Utc::now() + chrono::Duration::minutes(5))
            .unwrap();
        // Paid: claimed, correct answer, payout recorded.
        let expected = Hash::hash_bytes(b"c");
        let agent = PrivateKey::new_key().public_key();
        let paid = board.create_task(state.operator_public_key.clone(), "paid".into(), 10, expected);
        board.claim_task(paid.id, agent.clone(), Utc::now() + chrono::Duration::minutes(5)).unwrap();
        board.submit(paid.id, agent.clone(), expected).unwrap();
        board.mark_recipient_paid(paid.id, &agent, 10).unwrap();
    }

    /// The default (no `?status=`) must keep meaning exactly what it has
    /// always meant -- `Open` tasks only. Agents and both SDKs treat a
    /// listed task as a claimable one, so widening this by default would
    /// hand them work that's already finished.
    #[tokio::test]
    async fn listing_without_a_status_filter_still_returns_only_open_tasks() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        seed_one_task_per_status(&hub.state).await;

        let listed: Vec<Value> = hub.client.get(format!("{}/tasks", hub.base_url)).send().await.unwrap().json().await.unwrap();
        assert_eq!(listed.len(), 1, "three tasks exist but only one is Open");
        assert_eq!(listed[0]["status"], "Open");
    }

    #[tokio::test]
    async fn status_filter_selects_a_single_status_and_all_returns_everything() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        seed_one_task_per_status(&hub.state).await;

        let paid: Vec<Value> =
            hub.client.get(format!("{}/tasks?status=Paid", hub.base_url)).send().await.unwrap().json().await.unwrap();
        assert_eq!(paid.len(), 1);
        assert_eq!(paid[0]["status"], "Paid");

        // Case-insensitive, the same forgiving treatment `?capability=`
        // already gets.
        let lowercase: Vec<Value> =
            hub.client.get(format!("{}/tasks?status=paid", hub.base_url)).send().await.unwrap().json().await.unwrap();
        assert_eq!(lowercase.len(), 1, "?status=paid and ?status=Paid are the same query");

        let all: Vec<Value> =
            hub.client.get(format!("{}/tasks?status=all", hub.base_url)).send().await.unwrap().json().await.unwrap();
        assert_eq!(all.len(), 3, "every status, which is what the public board shows");
    }

    #[tokio::test]
    async fn unknown_status_is_a_legible_400_rather_than_a_serde_failure() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;

        let resp = hub.client.get(format!("{}/tasks?status=banana", hub.base_url)).send().await.unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
        let body: Value = resp.json().await.unwrap();
        let error = body["error"].as_str().unwrap();
        assert!(error.contains("banana"), "the error should name what was rejected: {error}");
        assert!(error.contains("Paid"), "and list the valid options: {error}");
    }

    /// The total counts everything matching the filters *before* paging --
    /// that's what makes it usable for "showing 2 of 3" and for sizing a
    /// pager.
    #[tokio::test]
    async fn total_count_header_reports_matches_before_pagination() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        seed_one_task_per_status(&hub.state).await;

        let resp = hub.client.get(format!("{}/tasks?status=all&limit=2", hub.base_url)).send().await.unwrap();
        assert_eq!(resp.headers().get("x-total-count").unwrap(), "3");
        let page: Vec<Value> = resp.json().await.unwrap();
        assert_eq!(page.len(), 2, "the page itself is still capped by limit");

        // And it tracks the filter, not the whole board.
        let resp = hub.client.get(format!("{}/tasks?status=Paid", hub.base_url)).send().await.unwrap();
        assert_eq!(resp.headers().get("x-total-count").unwrap(), "1");
    }

    /// `created_at` is the only timestamp the hub exposes for a task, and
    /// every time series in the dashboard is derived from it -- if it ever
    /// stops being serialized, the sparklines silently flatline rather
    /// than erroring, so this asserts on its presence directly.
    #[tokio::test]
    async fn tasks_expose_created_at_for_charting() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;

        let created_at = Utc::now() - chrono::Duration::hours(3);
        let task_id = {
            let mut board = hub.state.board.write().await;
            let mut task = board.create_task(hub.state.operator_public_key.clone(), "dated".into(), 10, Hash::hash_bytes(b"x"));
            task.created_at = created_at;
            let id = task.id;
            board.restore_task(task);
            id
        };

        let task: Value =
            hub.client.get(format!("{}/tasks/{task_id}", hub.base_url)).send().await.unwrap().json().await.unwrap();
        let reported: DateTime<Utc> = task["created_at"].as_str().unwrap().parse().unwrap();
        assert_eq!(reported, created_at);
    }

    #[tokio::test]
    async fn create_task_with_capabilities_is_filtered_correctly() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;

        let python_payload = handlers::CreateTaskPayload {
            description: "python task".to_string(),
            bounty: 10,
            expected_output_hash: hex::encode(Hash::hash_bytes(b"a").as_bytes()),
            min_reputation: 0,
            capabilities: BTreeSet::from(["python".to_string()]),
        };
        let resp = hub
            .client
            .post(format!("{}/tasks", hub.base_url))
            .json(&envelope(&hub.operator_key, "/tasks", python_payload))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let python_task: Value = resp.json().await.unwrap();
        let python_id: Uuid = python_task["id"].as_str().unwrap().parse().unwrap();
        assert_eq!(python_task["capabilities"], json!(["python"]));

        let rust_payload = handlers::CreateTaskPayload {
            description: "rust task".to_string(),
            bounty: 10,
            expected_output_hash: hex::encode(Hash::hash_bytes(b"b").as_bytes()),
            min_reputation: 0,
            capabilities: BTreeSet::from(["rust".to_string()]),
        };
        let resp = hub
            .client
            .post(format!("{}/tasks", hub.base_url))
            .json(&envelope(&hub.operator_key, "/tasks", rust_payload))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        let filtered: Vec<Value> = hub
            .client
            .get(format!("{}/tasks?capability=python", hub.base_url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(filtered.len(), 1, "only the python-tagged task should match");
        assert_eq!(filtered[0]["id"].as_str().unwrap().parse::<Uuid>().unwrap(), python_id);
    }

    #[tokio::test]
    async fn unfiltered_task_list_always_includes_tasks_with_no_capabilities() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;

        let tagged_payload = handlers::CreateTaskPayload {
            description: "tagged".to_string(),
            bounty: 10,
            expected_output_hash: hex::encode(Hash::hash_bytes(b"a").as_bytes()),
            min_reputation: 0,
            capabilities: BTreeSet::from(["python".to_string()]),
        };
        hub.client
            .post(format!("{}/tasks", hub.base_url))
            .json(&envelope(&hub.operator_key, "/tasks", tagged_payload))
            .send()
            .await
            .unwrap();

        let untagged_payload = handlers::CreateTaskPayload {
            description: "untagged".to_string(),
            bounty: 10,
            expected_output_hash: hex::encode(Hash::hash_bytes(b"b").as_bytes()),
            min_reputation: 0,
            capabilities: Default::default(),
        };
        let resp = hub
            .client
            .post(format!("{}/tasks", hub.base_url))
            .json(&envelope(&hub.operator_key, "/tasks", untagged_payload))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let untagged_task: Value = resp.json().await.unwrap();
        assert_eq!(untagged_task["capabilities"], json!([]), "no capabilities were given, so none are stored");

        let unfiltered: Vec<Value> = hub
            .client
            .get(format!("{}/tasks", hub.base_url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(unfiltered.len(), 2, "an unfiltered query returns both the tagged and untagged task");

        let filtered_by_tag: Vec<Value> = hub
            .client
            .get(format!("{}/tasks?capability=python", hub.base_url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(filtered_by_tag.len(), 1, "the untagged task never matches a specific capability filter");
    }

    #[tokio::test]
    async fn capability_filter_normalizes_case() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;

        let payload = handlers::CreateTaskPayload {
            description: "mixed case tag".to_string(),
            bounty: 10,
            expected_output_hash: hex::encode(Hash::hash_bytes(b"a").as_bytes()),
            min_reputation: 0,
            capabilities: BTreeSet::from(["  Python  ".to_string()]),
        };
        let task: Value = hub
            .client
            .post(format!("{}/tasks", hub.base_url))
            .json(&envelope(&hub.operator_key, "/tasks", payload))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(task["capabilities"], json!(["python"]), "stored tags are trimmed and lowercased at write time");

        for query in ["python", "Python", "PYTHON"] {
            let filtered: Vec<Value> = hub
                .client
                .get(format!("{}/tasks?capability={query}", hub.base_url))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_eq!(filtered.len(), 1, "query {query:?} should match the normalized stored tag regardless of case");
        }
    }

    #[tokio::test]
    async fn capability_filter_composes_with_limit_and_offset() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let base_time = Utc::now();

        let mut ids = Vec::new();
        {
            let mut board = hub.state.board.write().await;
            for i in 0..4u32 {
                let mut task = board.create_task(
                    hub.state.operator_public_key.clone(),
                    format!("python task {i}"),
                    10,
                    Hash::hash_bytes(format!("py{i}").as_bytes()),
                );
                task.created_at = base_time + chrono::Duration::seconds(i as i64);
                task.capabilities = BTreeSet::from(["python".to_string()]);
                ids.push(task.id);
                board.restore_task(task);
            }
            // A same-vintage distractor tagged differently, to prove the
            // capability filter is applied before pagination rather than
            // the assertions below passing by coincidence.
            let mut distractor = board.create_task(
                hub.state.operator_public_key.clone(),
                "rust distractor".to_string(),
                10,
                Hash::hash_bytes(b"rust-distractor"),
            );
            distractor.created_at = base_time + chrono::Duration::milliseconds(1500);
            distractor.capabilities = BTreeSet::from(["rust".to_string()]);
            board.restore_task(distractor);
        }

        let page: Vec<Value> = hub
            .client
            .get(format!("{}/tasks?capability=python&limit=2&offset=1", hub.base_url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[0]["id"].as_str().unwrap().parse::<Uuid>().unwrap(), ids[1], "oldest-first among python-tagged tasks, so offset=1 starts at the 2nd-oldest");
        assert_eq!(page[1]["id"].as_str().unwrap().parse::<Uuid>().unwrap(), ids[2]);
    }

    #[tokio::test]
    async fn create_task_rejects_too_many_capability_tags() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;

        let too_many: BTreeSet<String> = (0..21).map(|i| format!("tag{i}")).collect();
        let payload = handlers::CreateTaskPayload {
            description: "too many tags".to_string(),
            bounty: 10,
            expected_output_hash: hex::encode(Hash::hash_bytes(b"a").as_bytes()),
            min_reputation: 0,
            capabilities: too_many,
        };
        let resp = hub
            .client
            .post(format!("{}/tasks", hub.base_url))
            .json(&envelope(&hub.operator_key, "/tasks", payload))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_task_rejects_an_oversized_description() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;

        let payload = handlers::CreateTaskPayload {
            description: "x".repeat(20_001),
            bounty: 10,
            expected_output_hash: hex::encode(Hash::hash_bytes(b"a").as_bytes()),
            min_reputation: 0,
            capabilities: BTreeSet::new(),
        };
        let resp = hub
            .client
            .post(format!("{}/tasks", hub.base_url))
            .json(&envelope(&hub.operator_key, "/tasks", payload))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn submit_task_rejects_an_oversized_output() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let assignee_key = PrivateKey::new_key();

        let payload = handlers::CreateTaskPayload {
            description: "t".to_string(),
            bounty: 10,
            expected_output_hash: hex::encode(Hash::hash_bytes(b"a").as_bytes()),
            min_reputation: 0,
            capabilities: BTreeSet::new(),
        };
        let resp = hub
            .client
            .post(format!("{}/tasks", hub.base_url))
            .json(&envelope(&hub.operator_key, "/tasks", payload))
            .send()
            .await
            .unwrap();
        let created: Value = resp.json().await.unwrap();
        let task_id: Uuid = created["id"].as_str().unwrap().parse().unwrap();

        hub.client
            .post(format!("{}/tasks/{task_id}/claim", hub.base_url))
            .json(&envelope(&assignee_key, &format!("/tasks/{task_id}/claim"), handlers::ClaimPayload { task_id }))
            .send()
            .await
            .unwrap();

        let resp = hub
            .client
            .post(format!("{}/tasks/{task_id}/submit", hub.base_url))
            .json(&envelope(&assignee_key, &format!("/tasks/{task_id}/submit"), handlers::SubmitPayload { task_id, output: "x".repeat(20_001) }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    }

    fn parse_pubkey(hex_str: &str) -> PublicKey {
        PublicKey::from_sec1_bytes(&hex::decode(hex_str).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn create_task_escrow_then_confirm_creates_a_task_with_the_depositor_as_poster() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let agent_key = PrivateKey::new_key();

        let payload = handlers::EscrowTaskPayload {
            description: "agent-funded task".to_string(),
            bounty: 1_000,
            expected_output_hash: hex::encode(Hash::hash_bytes(b"42").as_bytes()),
            min_reputation: 0,
            capabilities: Default::default(),
        };
        let resp = hub
            .client
            .post(format!("{}/tasks/escrow", hub.base_url))
            .json(&envelope(&agent_key, "/tasks/escrow", payload))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let reservation: Value = resp.json().await.unwrap();
        let escrow_id: Uuid = reservation["escrow_id"].as_str().unwrap().parse().unwrap();
        let deposit_pubkey = parse_pubkey(reservation["deposit_address"].as_str().unwrap());
        let required_amount = reservation["required_amount"].as_u64().unwrap();
        assert_eq!(required_amount, 1_000 + 1_000, "bounty + HUB_TRANSACTION_FEE");

        fake_node.fund(deposit_pubkey, required_amount).await;

        let resp = hub
            .client
            .post(format!("{}/tasks/escrow/{escrow_id}/confirm", hub.base_url))
            .json(&envelope(&agent_key, &format!("/tasks/escrow/{escrow_id}/confirm"), handlers::ConfirmEscrowPayload { escrow_id }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let task: Value = resp.json().await.unwrap();
        assert_eq!(task["poster"], agent_key.public_key().to_string(), "the depositor, not the operator, is the poster");
        assert_eq!(task["bounty"], 1_000);
        assert_eq!(
            hub.state.board.read().await.allocated_bounty(), 0,
            "an escrow-funded task must never count against the operator's own balance"
        );
    }

    #[tokio::test]
    async fn confirm_task_escrow_rejects_when_underfunded() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let agent_key = PrivateKey::new_key();

        let payload = handlers::EscrowTaskPayload {
            description: "t".to_string(),
            bounty: 1_000,
            expected_output_hash: hex::encode(Hash::hash_bytes(b"x").as_bytes()),
            min_reputation: 0,
            capabilities: Default::default(),
        };
        let reservation: Value = hub
            .client
            .post(format!("{}/tasks/escrow", hub.base_url))
            .json(&envelope(&agent_key, "/tasks/escrow", payload))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let escrow_id: Uuid = reservation["escrow_id"].as_str().unwrap().parse().unwrap();
        let deposit_pubkey = parse_pubkey(reservation["deposit_address"].as_str().unwrap());
        // fund with less than required
        fake_node.fund(deposit_pubkey, 500).await;

        let resp = hub
            .client
            .post(format!("{}/tasks/escrow/{escrow_id}/confirm", hub.base_url))
            .json(&envelope(&agent_key, &format!("/tasks/escrow/{escrow_id}/confirm"), handlers::ConfirmEscrowPayload { escrow_id }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn agent_funded_task_poster_cannot_claim_their_own_task() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let poster_key = PrivateKey::new_key();

        let payload = handlers::EscrowTaskPayload {
            description: "t".to_string(),
            bounty: 100,
            expected_output_hash: hex::encode(Hash::hash_bytes(b"x").as_bytes()),
            min_reputation: 0,
            capabilities: Default::default(),
        };
        let reservation: Value = hub
            .client
            .post(format!("{}/tasks/escrow", hub.base_url))
            .json(&envelope(&poster_key, "/tasks/escrow", payload))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let escrow_id: Uuid = reservation["escrow_id"].as_str().unwrap().parse().unwrap();
        let deposit_pubkey = parse_pubkey(reservation["deposit_address"].as_str().unwrap());
        fake_node.fund(deposit_pubkey, reservation["required_amount"].as_u64().unwrap()).await;

        let task: Value = hub
            .client
            .post(format!("{}/tasks/escrow/{escrow_id}/confirm", hub.base_url))
            .json(&envelope(&poster_key, &format!("/tasks/escrow/{escrow_id}/confirm"), handlers::ConfirmEscrowPayload { escrow_id }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let task_id: Uuid = task["id"].as_str().unwrap().parse().unwrap();

        let resp = hub
            .client
            .post(format!("{}/tasks/{task_id}/claim", hub.base_url))
            .json(&envelope(&poster_key, &format!("/tasks/{task_id}/claim"), handlers::ClaimPayload { task_id }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn overlapping_confirm_calls_never_double_create_a_task() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let agent_key = PrivateKey::new_key();

        let payload = handlers::EscrowTaskPayload {
            description: "t".to_string(),
            bounty: 100,
            expected_output_hash: hex::encode(Hash::hash_bytes(b"x").as_bytes()),
            min_reputation: 0,
            capabilities: Default::default(),
        };
        let reservation: Value = hub
            .client
            .post(format!("{}/tasks/escrow", hub.base_url))
            .json(&envelope(&agent_key, "/tasks/escrow", payload))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let escrow_id: Uuid = reservation["escrow_id"].as_str().unwrap().parse().unwrap();
        let deposit_pubkey = parse_pubkey(reservation["deposit_address"].as_str().unwrap());
        fake_node.fund(deposit_pubkey, reservation["required_amount"].as_u64().unwrap()).await;

        // Two genuinely distinct signed envelopes (same payload content
        // would replay-collide) racing the same confirm.
        let confirm_once = |ts_offset_ms: i64| {
            let hub = &hub;
            let agent_key = &agent_key;
            async move {
                let env = envelope_at(
                    agent_key, &format!("/tasks/escrow/{escrow_id}/confirm"),
                    handlers::ConfirmEscrowPayload { escrow_id },
                    Utc::now() + chrono::Duration::milliseconds(ts_offset_ms),
                );
                hub.client
                    .post(format!("{}/tasks/escrow/{escrow_id}/confirm", hub.base_url))
                    .json(&env)
                    .send()
                    .await
                    .unwrap()
                    .status()
            }
        };
        let (status_a, status_b) = tokio::join!(confirm_once(0), confirm_once(1));
        let statuses = [status_a, status_b];
        assert_eq!(statuses.iter().filter(|s| **s == reqwest::StatusCode::OK).count(), 1, "exactly one confirm must succeed");
        assert_eq!(statuses.iter().filter(|s| **s == reqwest::StatusCode::CONFLICT).count(), 1, "the other must see it already consumed");

        assert_eq!(hub.state.board.read().await.all_tasks().count(), 1, "must never double-create a task");
    }

    #[tokio::test]
    async fn expired_unfunded_escrow_is_swept_without_creating_a_task() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let agent_key = PrivateKey::new_key();

        let payload = handlers::EscrowTaskPayload {
            description: "never funded".to_string(),
            bounty: 100,
            expected_output_hash: hex::encode(Hash::hash_bytes(b"x").as_bytes()),
            min_reputation: 0,
            capabilities: Default::default(),
        };
        let reservation: Value = hub
            .client
            .post(format!("{}/tasks/escrow", hub.base_url))
            .json(&envelope(&agent_key, "/tasks/escrow", payload))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let escrow_id: Uuid = reservation["escrow_id"].as_str().unwrap().parse().unwrap();
        // deliberately never funded

        run_sweep_once(&hub.state, Utc::now() + chrono::Duration::minutes(61)).await;

        assert_eq!(hub.state.board.read().await.all_tasks().count(), 0, "nothing was ever funded, so no task should exist");
        assert_eq!(
            hub.state.board.read().await.get_pending_deposit(escrow_id).unwrap().status,
            board::EscrowStatus::Refunded
        );
    }

    /// The deterministic half of the escrow-durability story, and the
    /// reason this test is written against the *store* rather than the
    /// board: `mark_escrow_refunded` only ever changed memory, so a
    /// refunded deposit came back `Reserved` on **every** restart -- not
    /// as a race, but always. Three things followed, and all three are
    /// what this pins: the sweep re-selected every deposit ever refunded
    /// in the hub's history (`overdue_reserved_escrows` filters on
    /// `Reserved`), so a long-lived deployment's boot sweep grew without
    /// bound; a depositor whose refund had already gone out could confirm
    /// the escrow again, with only the on-chain balance check standing in
    /// the way; and a dispute bond re-credited its winner (see
    /// `a_settled_dispute_bond_is_not_settled_again_after_a_restart`).
    #[tokio::test]
    async fn a_refunded_escrow_reloads_as_refunded() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let agent_key = PrivateKey::new_key();

        let payload = handlers::EscrowTaskPayload {
            description: "funded, then left to expire".to_string(),
            bounty: 1_000,
            expected_output_hash: hex::encode(Hash::hash_bytes(b"x").as_bytes()),
            min_reputation: 0,
            capabilities: Default::default(),
        };
        let reservation: Value = hub
            .client
            .post(format!("{}/tasks/escrow", hub.base_url))
            .json(&envelope(&agent_key, "/tasks/escrow", payload))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let escrow_id: Uuid = reservation["escrow_id"].as_str().unwrap().parse().unwrap();
        let deposit_pubkey = parse_pubkey(reservation["deposit_address"].as_str().unwrap());
        // Funded but never confirmed, so the sweep refunds it on chain
        // rather than finding an empty address -- the case that actually
        // moves money and therefore the one worth being durable about.
        fake_node.fund(deposit_pubkey, reservation["required_amount"].as_u64().unwrap()).await;

        run_sweep_once(&hub.state, Utc::now() + chrono::Duration::minutes(61)).await;
        assert_eq!(
            hub.state.board.read().await.get_pending_deposit(escrow_id).unwrap().status,
            board::EscrowStatus::Refunded,
            "in memory the refund happened, which was never the part in doubt"
        );

        // What a restart actually restores: a fresh board filled from the
        // store, nothing carried over in memory.
        let mut restored = TaskBoard::new();
        for deposit in hub.state.store.load_all_pending_deposits().unwrap() {
            restored.restore_pending_deposit(deposit);
        }
        assert_eq!(
            restored.get_pending_deposit(escrow_id).unwrap().status,
            board::EscrowStatus::Refunded,
            "a deposit whose money has already gone back must not reload as Reserved"
        );
        assert!(
            restored.overdue_reserved_escrows(Utc::now() + chrono::Duration::minutes(61)).is_empty(),
            "and it must not be handed to the sweep again for the life of the deployment"
        );
    }

    #[tokio::test]
    async fn cancel_task_refunds_an_agent_funded_task_to_its_poster() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let poster_key = PrivateKey::new_key();

        let payload = handlers::EscrowTaskPayload {
            description: "will be cancelled".to_string(),
            bounty: 1_000,
            expected_output_hash: hex::encode(Hash::hash_bytes(b"x").as_bytes()),
            min_reputation: 0,
            capabilities: Default::default(),
        };
        let reservation: Value = hub
            .client
            .post(format!("{}/tasks/escrow", hub.base_url))
            .json(&envelope(&poster_key, "/tasks/escrow", payload))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let escrow_id: Uuid = reservation["escrow_id"].as_str().unwrap().parse().unwrap();
        let deposit_pubkey = parse_pubkey(reservation["deposit_address"].as_str().unwrap());
        let required_amount = reservation["required_amount"].as_u64().unwrap();
        fake_node.fund(deposit_pubkey, required_amount).await;

        let task: Value = hub
            .client
            .post(format!("{}/tasks/escrow/{escrow_id}/confirm", hub.base_url))
            .json(&envelope(&poster_key, &format!("/tasks/escrow/{escrow_id}/confirm"), handlers::ConfirmEscrowPayload { escrow_id }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let task_id: Uuid = task["id"].as_str().unwrap().parse().unwrap();

        let resp = hub
            .client
            .post(format!("{}/tasks/{task_id}/cancel", hub.base_url))
            .json(&envelope(&hub.operator_key, &format!("/tasks/{task_id}/cancel"), handlers::CancelPayload { task_id }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        let submitted = fake_node.wait_for_submitted_count(1).await;
        assert_eq!(submitted.len(), 1, "the deposit must be refunded on-chain");
        let refund = &submitted[0];
        assert!(
            refund.outputs.iter().any(|o| o.pubkey == poster_key.public_key() && o.value == required_amount - 1_000),
            "the refund must pay back to the original poster, minus the network fee"
        );
        assert_eq!(
            hub.state.board.read().await.get_pending_deposit(escrow_id).unwrap().status,
            board::EscrowStatus::Refunded
        );
    }

    #[tokio::test]
    async fn agent_funded_consensus_task_pays_every_winner_in_one_combined_transaction() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let poster_key = PrivateKey::new_key();
        let assignee1 = PrivateKey::new_key();
        let assignee2 = PrivateKey::new_key();

        let payload = handlers::EscrowConsensusTaskPayload {
            description: "open-ended, agent-funded".to_string(),
            bounty: 900,
            num_assignees: 2,
            join_window_minutes: 60,
            submission_window_minutes: 30,
            min_reputation: 0,
            capabilities: Default::default(),
        };
        let reservation: Value = hub
            .client
            .post(format!("{}/tasks/consensus/escrow", hub.base_url))
            .json(&envelope(&poster_key, "/tasks/consensus/escrow", payload))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let escrow_id: Uuid = reservation["escrow_id"].as_str().unwrap().parse().unwrap();
        let deposit_pubkey = parse_pubkey(reservation["deposit_address"].as_str().unwrap());
        let required_amount = reservation["required_amount"].as_u64().unwrap();
        assert_eq!(required_amount, 900 + 1_000);
        fake_node.fund(deposit_pubkey, required_amount).await;

        let task: Value = hub
            .client
            .post(format!("{}/tasks/escrow/{escrow_id}/confirm", hub.base_url))
            .json(&envelope(&poster_key, &format!("/tasks/escrow/{escrow_id}/confirm"), handlers::ConfirmEscrowPayload { escrow_id }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let task_id: Uuid = task["id"].as_str().unwrap().parse().unwrap();

        for assignee in [&assignee1, &assignee2] {
            hub.client
                .post(format!("{}/tasks/{task_id}/claim", hub.base_url))
                .json(&envelope(assignee, &format!("/tasks/{task_id}/claim"), handlers::ClaimPayload { task_id }))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
        }
        hub.client
            .post(format!("{}/tasks/{task_id}/submit", hub.base_url))
            .json(&envelope(&assignee1, &format!("/tasks/{task_id}/submit"), handlers::SubmitPayload { task_id, output: "42".to_string() }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        hub.client
            .post(format!("{}/tasks/{task_id}/submit", hub.base_url))
            .json(&envelope(&assignee2, &format!("/tasks/{task_id}/submit"), handlers::SubmitPayload { task_id, output: "42".to_string() }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();

        let submitted = fake_node.wait_for_submitted_count(1).await;
        assert_eq!(submitted.len(), 1, "both winners must be settled in one combined transaction, not two separate ones");
        let settlement = &submitted[0];
        assert_eq!(settlement.outputs.len(), 2, "no change output -- the deposit was funded exactly, and both winners are paid");
        for winner in [&assignee1, &assignee2] {
            assert!(
                settlement.outputs.iter().any(|o| o.pubkey == winner.public_key() && o.value == 450),
                "each winner must receive their even 900/2 share"
            );
        }
    }

    /// Creates+confirms an escrow-funded `Disputable` task, claims it, and
    /// submits an answer -- all via real HTTP calls, landing the task in
    /// `AwaitingDispute`. Returns the task id.
    async fn create_confirmed_disputable_task_awaiting_dispute(
        hub: &TestHub,
        fake_node: &FakeNode,
        poster_key: &PrivateKey,
        assignee_key: &PrivateKey,
        bounty: u64,
        dispute_window_minutes: i64,
    ) -> Uuid {
        let payload = handlers::EscrowDisputableTaskPayload {
            description: "open-ended, disputable".to_string(),
            bounty,
            dispute_window_minutes,
            min_reputation: 0,
            capabilities: Default::default(),
        };
        let reservation: Value = hub
            .client
            .post(format!("{}/tasks/disputable/escrow", hub.base_url))
            .json(&envelope(poster_key, "/tasks/disputable/escrow", payload))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let escrow_id: Uuid = reservation["escrow_id"].as_str().unwrap().parse().unwrap();
        let deposit_pubkey = parse_pubkey(reservation["deposit_address"].as_str().unwrap());
        let required_amount = reservation["required_amount"].as_u64().unwrap();
        fake_node.fund(deposit_pubkey, required_amount).await;

        let task: Value = hub
            .client
            .post(format!("{}/tasks/escrow/{escrow_id}/confirm", hub.base_url))
            .json(&envelope(poster_key, &format!("/tasks/escrow/{escrow_id}/confirm"), handlers::ConfirmEscrowPayload { escrow_id }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let task_id: Uuid = task["id"].as_str().unwrap().parse().unwrap();

        hub.client
            .post(format!("{}/tasks/{task_id}/claim", hub.base_url))
            .json(&envelope(assignee_key, &format!("/tasks/{task_id}/claim"), handlers::ClaimPayload { task_id }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        hub.client
            .post(format!("{}/tasks/{task_id}/submit", hub.base_url))
            .json(&envelope(assignee_key, &format!("/tasks/{task_id}/submit"), handlers::SubmitPayload { task_id, output: "my answer".to_string() }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();

        task_id
    }

    /// Reserves+confirms a dispute bond against `task_id` from
    /// `challenger_key` -- real HTTP calls, landing the task in `Disputed`.
    async fn file_dispute_via_http(
        hub: &TestHub,
        fake_node: &FakeNode,
        task_id: Uuid,
        challenger_key: &PrivateKey,
        reason: &str,
    ) {
        let reservation: Value = hub
            .client
            .post(format!("{}/tasks/{task_id}/dispute/escrow", hub.base_url))
            .json(&envelope(challenger_key, &format!("/tasks/{task_id}/dispute/escrow"), handlers::DisputeEscrowPayload { task_id, reason: reason.to_string() }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let escrow_id: Uuid = reservation["escrow_id"].as_str().unwrap().parse().unwrap();
        let deposit_pubkey = parse_pubkey(reservation["deposit_address"].as_str().unwrap());
        let required_amount = reservation["required_amount"].as_u64().unwrap();
        fake_node.fund(deposit_pubkey, required_amount).await;

        hub.client
            .post(format!("{}/tasks/{task_id}/dispute/confirm", hub.base_url))
            .json(&envelope(challenger_key, &format!("/tasks/{task_id}/dispute/confirm"), handlers::ConfirmDisputeEscrowPayload { task_id, escrow_id }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }

    #[tokio::test]
    async fn unchallenged_disputable_task_pays_the_claimant_via_sweep() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let poster_key = PrivateKey::new_key();
        let assignee_key = PrivateKey::new_key();

        let task_id =
            create_confirmed_disputable_task_awaiting_dispute(&hub, &fake_node, &poster_key, &assignee_key, 900, 30)
                .await;

        // simulate the dispute window having elapsed, no dispute filed
        run_sweep_once(&hub.state, Utc::now() + chrono::Duration::minutes(31)).await;
        // ...and a later one to resolve what that sweep submitted.
        fake_node.wait_for_submitted_count(1).await;
        run_sweep_once(&hub.state, Utc::now() + chrono::Duration::minutes(32)).await;

        let task: Value = hub
            .client
            .get(format!("{}/tasks/{task_id}", hub.base_url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(task["status"], "Paid");

        let submitted = fake_node.wait_for_submitted_count(1).await;
        assert_eq!(submitted.len(), 1);
        assert!(submitted[0].outputs.iter().any(|o| o.pubkey == assignee_key.public_key() && o.value == 900));
    }

    #[tokio::test]
    async fn assignee_cannot_reserve_a_dispute_against_their_own_submission() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let poster_key = PrivateKey::new_key();
        let assignee_key = PrivateKey::new_key();

        let task_id =
            create_confirmed_disputable_task_awaiting_dispute(&hub, &fake_node, &poster_key, &assignee_key, 900, 30)
                .await;

        let resp = hub
            .client
            .post(format!("{}/tasks/{task_id}/dispute/escrow", hub.base_url))
            .json(&envelope(&assignee_key, &format!("/tasks/{task_id}/dispute/escrow"), handlers::DisputeEscrowPayload { task_id, reason: "self-dispute".to_string() }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn full_dispute_flow_challenger_wins_settles_both_legs_to_the_challenger() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let poster_key = PrivateKey::new_key();
        let assignee_key = PrivateKey::new_key();
        let challenger_key = PrivateKey::new_key();

        let task_id =
            create_confirmed_disputable_task_awaiting_dispute(&hub, &fake_node, &poster_key, &assignee_key, 900, 30)
                .await;
        file_dispute_via_http(&hub, &fake_node, task_id, &challenger_key, "wrong answer").await;

        let task: Value = hub
            .client
            .get(format!("{}/tasks/{task_id}", hub.base_url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(task["status"], "Disputed");

        let resp = hub
            .client
            .post(format!("{}/tasks/{task_id}/dispute/resolve", hub.base_url))
            .json(&envelope(
                &hub.operator_key, &format!("/tasks/{task_id}/dispute/resolve"),
                handlers::ResolveDisputePayload { task_id, outcome: board::DisputeResolution::ChallengerWins },
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        let submitted = fake_node.wait_for_submitted_count(2).await;
        assert_eq!(submitted.len(), 2, "bounty leg and bond leg are two separate transactions, from two different escrows");
        let total_to_challenger: u64 = submitted
            .iter()
            .flat_map(|tx| &tx.outputs)
            .filter(|o| o.pubkey == challenger_key.public_key())
            .map(|o| o.value)
            .sum();
        assert_eq!(total_to_challenger, 900 + 900, "bounty (900) plus their own bond back (900, after its own fee)");

        // The bounty payout is only *submitted* at this point.
        // Reputation, like everything else downstream of a payout, now
        // waits until the sweep has seen it on chain.
        confirm_submitted_payouts(&hub.state, &fake_node).await;

        let assignee_rep: Value = hub
            .client
            .get(format!("{}/reputation/{}", hub.base_url, assignee_key.public_key()))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(assignee_rep["failed"], 1);
        let challenger_rep: Value = hub
            .client
            .get(format!("{}/reputation/{}", hub.base_url, challenger_key.public_key()))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(challenger_rep["completed"], 1);
        assert_eq!(challenger_rep["total_earned"], 900, "only the bounty counts as earned -- getting your own bond back doesn't");
    }

    #[tokio::test]
    async fn full_dispute_flow_assignee_wins_settles_both_legs_to_the_assignee() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let poster_key = PrivateKey::new_key();
        let assignee_key = PrivateKey::new_key();
        let challenger_key = PrivateKey::new_key();

        let task_id =
            create_confirmed_disputable_task_awaiting_dispute(&hub, &fake_node, &poster_key, &assignee_key, 900, 30)
                .await;
        file_dispute_via_http(&hub, &fake_node, task_id, &challenger_key, "actually correct").await;

        let resp = hub
            .client
            .post(format!("{}/tasks/{task_id}/dispute/resolve", hub.base_url))
            .json(&envelope(
                &hub.operator_key, &format!("/tasks/{task_id}/dispute/resolve"),
                handlers::ResolveDisputePayload { task_id, outcome: board::DisputeResolution::AssigneeWins },
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        let submitted = fake_node.wait_for_submitted_count(2).await;
        assert_eq!(submitted.len(), 2);
        let total_to_assignee: u64 = submitted
            .iter()
            .flat_map(|tx| &tx.outputs)
            .filter(|o| o.pubkey == assignee_key.public_key())
            .map(|o| o.value)
            .sum();
        assert_eq!(total_to_assignee, 900 + 900, "bounty plus the forfeited bond");

        // The bounty payout is only *submitted* at this point.
        // Reputation, like everything else downstream of a payout, now
        // waits until the sweep has seen it on chain.
        confirm_submitted_payouts(&hub.state, &fake_node).await;

        let assignee_rep: Value = hub
            .client
            .get(format!("{}/reputation/{}", hub.base_url, assignee_key.public_key()))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(assignee_rep["completed"], 1);
        assert_eq!(assignee_rep["total_earned"], 900 + 900, "bounty AND the forfeited bond both count as earned this time");
        let challenger_rep: Value = hub
            .client
            .get(format!("{}/reputation/{}", hub.base_url, challenger_key.public_key()))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(challenger_rep["failed"], 1);
    }

    /// A fresh `TaskBoard` filled from `hub`'s own store and nothing
    /// else -- what a restart actually restores, in the same order
    /// `main` does it. Every "does this survive a restart" assertion
    /// wants this rather than the live board, because the live board is
    /// precisely the copy that is not in question.
    fn board_as_a_restart_would_load_it(hub: &TestHub) -> TaskBoard {
        let store = &hub.state.store;
        let mut board = TaskBoard::new();
        for task in store.load_all_tasks().unwrap() {
            board.restore_task(task);
        }
        for (pubkey, reputation) in store.load_all_reputation().unwrap() {
            board.restore_reputation(pubkey, reputation);
        }
        for deposit in store.load_all_pending_deposits().unwrap() {
            board.restore_pending_deposit(deposit);
        }
        for (pubkey, account) in store.load_all_exchange_accounts().unwrap() {
            board.restore_exchange_account(pubkey, account);
        }
        for attempt in store.load_all_payout_attempts().unwrap() {
            board.restore_payout_attempt(attempt);
        }
        board
    }

    /// The consequence of a non-durable `Refunded` that does not
    /// self-heal: `settle_dispute_bond` credits the winner's
    /// `total_earned` durably but marked the bond's deposit `Refunded`
    /// only in memory, so a restart reloaded the bond as `Consumed`,
    /// `tasks_with_unsettled_dispute_bonds` selected it again, and the
    /// whole settlement re-ran -- at every boot, for the life of the
    /// deployment.
    ///
    /// What that costs is narrower than it first appears, and the
    /// correction is worth carrying here because this test's earlier
    /// wording had it wrong: the on-chain leg does not duplicate, and
    /// neither does the reputation credit. `credit_forfeited_bond` is
    /// applied with the net amount the *retry* computed, and the retry
    /// reads the drained address, so it adds zero (measured against a
    /// pre-fix binary -- plan §6.5c). The defect is unbounded repeated
    /// work, not a wrong ledger. It is still worth a durable status: the
    /// ledger survives only because the credit happens to derive from a
    /// live balance rather than the recorded bond amount.
    ///
    /// Asserted against the *restored* board, since what the live one
    /// thinks was never the question.
    #[tokio::test]
    async fn a_settled_dispute_bond_is_not_settled_again_after_a_restart() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let poster_key = PrivateKey::new_key();
        let assignee_key = PrivateKey::new_key();
        let challenger_key = PrivateKey::new_key();

        let task_id =
            create_confirmed_disputable_task_awaiting_dispute(&hub, &fake_node, &poster_key, &assignee_key, 900, 30)
                .await;
        file_dispute_via_http(&hub, &fake_node, task_id, &challenger_key, "actually correct").await;
        hub.client
            .post(format!("{}/tasks/{task_id}/dispute/resolve", hub.base_url))
            .json(&envelope(
                &hub.operator_key, &format!("/tasks/{task_id}/dispute/resolve"),
                handlers::ResolveDisputePayload { task_id, outcome: board::DisputeResolution::AssigneeWins },
            ))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        fake_node.wait_for_submitted_count(2).await;
        confirm_submitted_payouts(&hub.state, &fake_node).await;

        let earned_once = hub.state.board.read().await.reputation(&assignee_key.public_key()).total_earned;
        assert_eq!(earned_once, 900 + 900, "bounty plus the forfeited bond, credited once");

        let restored = board_as_a_restart_would_load_it(&hub);
        assert_eq!(
            restored.reputation(&assignee_key.public_key()).total_earned,
            earned_once,
            "the credit itself was always durable -- this is the control, not the finding"
        );
        assert!(
            restored.tasks_with_unsettled_dispute_bonds().is_empty(),
            "a bond already disbursed must not be selected for settlement again: pre-fix it \
             was, at every boot for the life of the deployment, each pass a node round trip \
             inside the sweep"
        );
    }

    #[tokio::test]
    async fn non_operator_cannot_resolve_dispute() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let poster_key = PrivateKey::new_key();
        let assignee_key = PrivateKey::new_key();
        let challenger_key = PrivateKey::new_key();
        let impostor_key = PrivateKey::new_key();

        let task_id =
            create_confirmed_disputable_task_awaiting_dispute(&hub, &fake_node, &poster_key, &assignee_key, 900, 30)
                .await;
        file_dispute_via_http(&hub, &fake_node, task_id, &challenger_key, "wrong").await;

        let resp = hub
            .client
            .post(format!("{}/tasks/{task_id}/dispute/resolve", hub.base_url))
            .json(&envelope(
                &impostor_key, &format!("/tasks/{task_id}/dispute/resolve"),
                handlers::ResolveDisputePayload { task_id, outcome: board::DisputeResolution::ChallengerWins },
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);

        let task: Value = hub
            .client
            .get(format!("{}/tasks/{task_id}", hub.base_url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(task["status"], "Disputed", "must not have resolved");
    }

    // ---- Exchange v1 ----

    /// Directly credits `owner`'s exchange ledger balance, bypassing the
    /// deposit flow entirely -- mirrors `seed_verified_task`'s role for
    /// task settlement tests: order/withdrawal tests exercise the HTTP
    /// layer they're actually about without also depending on a
    /// separate flow (deposit confirmation) each time.
    async fn seed_exchange_account(state: &AppState, owner: &PublicKey, base_balance: u64, compute_balance: u64) {
        let mut board = state.board.write().await;
        board.restore_exchange_account(
            owner.clone(),
            board::ExchangeAccount { base_balance, locked_base: 0, compute_balance, locked_compute: 0 },
        );
    }

    #[tokio::test]
    async fn exchange_deposit_confirm_credits_ledger_and_sweeps_to_custody() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let agent_key = PrivateKey::new_key();

        let reservation: Value = hub
            .client
            .post(format!("{}/exchange/deposit", hub.base_url))
            .json(&envelope(&agent_key, "/exchange/deposit", ()))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let escrow_id: Uuid = reservation["escrow_id"].as_str().unwrap().parse().unwrap();
        let deposit_pubkey = parse_pubkey(reservation["deposit_address"].as_str().unwrap());

        fake_node.fund(deposit_pubkey, 10_000).await;

        let resp = hub
            .client
            .post(format!("{}/exchange/deposit/{escrow_id}/confirm", hub.base_url))
            .json(&envelope(&agent_key, &format!("/exchange/deposit/{escrow_id}/confirm"), handlers::ConfirmExchangeDepositPayload { escrow_id }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let account: Value = resp.json().await.unwrap();
        assert_eq!(account["base_balance"], 10_000 - 1_000, "credited net of HUB_TRANSACTION_FEE");

        // The inline sweep attempted right after confirmation should have
        // paid the pooled custody address.
        let submitted = fake_node.wait_for_submitted_count(1).await;
        assert_eq!(submitted.len(), 1);
        let custody_pubkey = hub.state.exchange_custody_public_key.clone();
        assert!(
            submitted[0].outputs.iter().any(|o| o.pubkey == custody_pubkey),
            "the sweep transaction must pay the pooled custody address"
        );
    }

    #[tokio::test]
    async fn exchange_deposit_confirm_rejects_when_underfunded() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let agent_key = PrivateKey::new_key();

        let reservation: Value = hub
            .client
            .post(format!("{}/exchange/deposit", hub.base_url))
            .json(&envelope(&agent_key, "/exchange/deposit", ()))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let escrow_id: Uuid = reservation["escrow_id"].as_str().unwrap().parse().unwrap();
        let deposit_pubkey = parse_pubkey(reservation["deposit_address"].as_str().unwrap());
        let required = reservation["required_amount"].as_u64().unwrap();

        fake_node.fund(deposit_pubkey, required - 1).await;

        let resp = hub
            .client
            .post(format!("{}/exchange/deposit/{escrow_id}/confirm", hub.base_url))
            .json(&envelope(&agent_key, &format!("/exchange/deposit/{escrow_id}/confirm"), handlers::ConfirmExchangeDepositPayload { escrow_id }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn run_sweep_once_sweeps_confirmed_exchange_deposits_and_is_idempotent_on_rerun() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let agent_key = PrivateKey::new_key();

        let deposit = {
            let mut board = hub.state.board.write().await;
            board.reserve_escrow(
                &hub.state.escrow_secret,
                agent_key.public_key(),
                1,
                board::EscrowPurpose::FundExchangeAccount,
                Utc::now() + chrono::Duration::minutes(30),
            )
        };
        fake_node.fund(deposit.deposit_pubkey.clone(), 10_000).await;
        hub.state.board.write().await.confirm_exchange_deposit(deposit.id, 10_000, 1_000, Utc::now()).unwrap();

        run_sweep_once(&hub.state, Utc::now()).await;
        let submitted_after_first = fake_node.wait_for_submitted_count(1).await;
        assert_eq!(submitted_after_first.len(), 1);

        // Rerunning must not sweep it a second time -- it already dropped
        // out of unswept_exchange_deposits once Refunded.
        run_sweep_once(&hub.state, Utc::now()).await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(
            fake_node.submitted_transactions().await.len(),
            1,
            "must not sweep an already-swept deposit again"
        );
    }

    #[tokio::test]
    async fn place_order_http_full_match_settles_both_sides_ledgers_correctly() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let seller_key = PrivateKey::new_key();
        let buyer_key = PrivateKey::new_key();
        seed_exchange_account(&hub.state, &seller_key.public_key(), 0, 50).await;
        seed_exchange_account(&hub.state, &buyer_key.public_key(), 10_000, 0).await;

        let resp = hub
            .client
            .post(format!("{}/exchange/orders", hub.base_url))
            .json(&envelope(
                &seller_key, "/exchange/orders",
                handlers::PlaceOrderPayload { side: board::Side::Sell, price: 8, quantity: 50 },
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        let resp = hub
            .client
            .post(format!("{}/exchange/orders", hub.base_url))
            .json(&envelope(
                &buyer_key, "/exchange/orders",
                handlers::PlaceOrderPayload { side: board::Side::Buy, price: 10, quantity: 50 },
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let order: Value = resp.json().await.unwrap();
        assert_eq!(order["status"], "filled");

        let seller_account: Value = hub
            .client
            .get(format!("{}/exchange/account/{}", hub.base_url, seller_key.public_key()))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(seller_account["base_balance"], 400, "50 * 8, the resting maker's price, not the taker's 10");

        let trades: Value =
            hub.client.get(format!("{}/exchange/trades", hub.base_url)).send().await.unwrap().json().await.unwrap();
        assert_eq!(trades.as_array().unwrap().len(), 1);

        let order_book: Value =
            hub.client.get(format!("{}/exchange/orders", hub.base_url)).send().await.unwrap().json().await.unwrap();
        assert!(order_book["bids"].as_array().unwrap().is_empty());
        assert!(order_book["asks"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn place_order_via_http_credits_the_taker_fee_to_the_operator() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key.clone(), fake_node.addr.clone()).await;
        let seller_key = PrivateKey::new_key();
        let buyer_key = PrivateKey::new_key();
        seed_exchange_account(&hub.state, &seller_key.public_key(), 0, 5_000).await;
        seed_exchange_account(&hub.state, &buyer_key.public_key(), 100_000, 0).await;

        hub.client
            .post(format!("{}/exchange/orders", hub.base_url))
            .json(&envelope(
                &seller_key, "/exchange/orders",
                handlers::PlaceOrderPayload { side: board::Side::Sell, price: 10, quantity: 5_000 },
            ))
            .send()
            .await
            .unwrap();

        // The buyer is the taker here (crossing a resting ask), so the
        // fee comes out of the compute they receive.
        let resp = hub
            .client
            .post(format!("{}/exchange/orders", hub.base_url))
            .json(&envelope(
                &buyer_key, "/exchange/orders",
                handlers::PlaceOrderPayload { side: board::Side::Buy, price: 10, quantity: 5_000 },
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        let buyer_account: Value = hub
            .client
            .get(format!("{}/exchange/account/{}", hub.base_url, buyer_key.public_key()))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(buyer_account["compute_balance"], 4_995, "5_000 fill minus 5 (10 bps) fee");

        let seller_account: Value = hub
            .client
            .get(format!("{}/exchange/account/{}", hub.base_url, seller_key.public_key()))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(seller_account["base_balance"], 50_000, "the maker side pays no fee");

        let operator_account: Value = hub
            .client
            .get(format!("{}/exchange/account/{}", hub.base_url, operator_key.public_key()))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(operator_account["compute_balance"], 5, "the fee lands in the operator's own exchange account");

        let trades: Value =
            hub.client.get(format!("{}/exchange/trades", hub.base_url)).send().await.unwrap().json().await.unwrap();
        let trade = &trades.as_array().unwrap()[0];
        assert_eq!(trade["taker_side"], "buy");
        assert_eq!(trade["taker_fee"], 5);
    }

    #[tokio::test]
    async fn place_order_rejects_insufficient_balance_via_http() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let owner_key = PrivateKey::new_key();
        seed_exchange_account(&hub.state, &owner_key.public_key(), 100, 0).await;

        let resp = hub
            .client
            .post(format!("{}/exchange/orders", hub.base_url))
            .json(&envelope(
                &owner_key, "/exchange/orders",
                handlers::PlaceOrderPayload { side: board::Side::Buy, price: 10, quantity: 20 },
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn cancel_order_via_http_releases_locked_balance() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let owner_key = PrivateKey::new_key();
        seed_exchange_account(&hub.state, &owner_key.public_key(), 1_000, 0).await;

        let order: Value = hub
            .client
            .post(format!("{}/exchange/orders", hub.base_url))
            .json(&envelope(
                &owner_key, "/exchange/orders",
                handlers::PlaceOrderPayload { side: board::Side::Buy, price: 10, quantity: 50 },
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let order_id: Uuid = order["id"].as_str().unwrap().parse().unwrap();

        let resp = hub
            .client
            .post(format!("{}/exchange/orders/{order_id}/cancel", hub.base_url))
            .json(&envelope(&owner_key, &format!("/exchange/orders/{order_id}/cancel"), handlers::CancelOrderPayload { order_id }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        let account: Value = hub
            .client
            .get(format!("{}/exchange/account/{}", hub.base_url, owner_key.public_key()))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(account["locked_base"], 0);
    }

    #[tokio::test]
    async fn withdraw_pays_out_from_the_pooled_custody_address() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let custody_pubkey = hub.state.exchange_custody_public_key.clone();
        fake_node.fund(custody_pubkey, 100_000).await;
        let owner_key = PrivateKey::new_key();
        seed_exchange_account(&hub.state, &owner_key.public_key(), 5_000, 0).await;

        let resp = hub
            .client
            .post(format!("{}/exchange/withdraw", hub.base_url))
            .json(&envelope(&owner_key, "/exchange/withdraw", handlers::WithdrawPayload { amount: 2_000 }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let account: Value = resp.json().await.unwrap();
        assert_eq!(account["base_balance"], 3_000);

        let submitted = fake_node.wait_for_submitted_count(1).await;
        assert_eq!(submitted.len(), 1);
        assert!(
            submitted[0].outputs.iter().any(|o| o.pubkey == owner_key.public_key() && o.value == 2_000),
            "must actually pay the withdrawing agent, out of pooled custody"
        );
    }

    /// The safe half of the split in `CustodyPaymentFailure`: a payment
    /// that could not be *built* never reached the node, so crediting the
    /// balance back is correct and still happens.
    ///
    /// This is also the common failure by a wide margin -- custody short
    /// of a spendable output is what fails here -- which is why the split
    /// is worth having rather than refusing to revert at all. Nothing is
    /// recorded for review, because there is nothing unresolved: the
    /// money never moved and the caller has their balance.
    #[tokio::test]
    async fn a_withdrawal_that_was_never_built_credits_the_balance_back() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        // Custody is deliberately left unfunded, so building the payment
        // fails before anything is signed or sent.
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let owner_key = PrivateKey::new_key();
        seed_exchange_account(&hub.state, &owner_key.public_key(), 5_000, 0).await;

        let resp = hub
            .client
            .post(format!("{}/exchange/withdraw", hub.base_url))
            .json(&envelope(&owner_key, "/exchange/withdraw", handlers::WithdrawPayload { amount: 2_000 }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::INTERNAL_SERVER_ERROR);

        let account = hub.state.board.read().await.exchange_account(&owner_key.public_key());
        assert_eq!(
            account.base_balance, 5_000,
            "nothing was sent, so the caller is still owed every unit of it"
        );
        assert!(
            hub.state.store.load_all_withdrawal_attempts().unwrap().is_empty(),
            "a payment that was never built leaves nothing for an operator to resolve"
        );
    }

    #[tokio::test]
    async fn withdraw_rejects_amount_exceeding_available_balance_via_http() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let owner_key = PrivateKey::new_key();
        seed_exchange_account(&hub.state, &owner_key.public_key(), 100, 0).await;

        let resp = hub
            .client
            .post(format!("{}/exchange/withdraw", hub.base_url))
            .json(&envelope(&owner_key, "/exchange/withdraw", handlers::WithdrawPayload { amount: 101 }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn compute_minting_hook_credits_compute_balance_on_an_operator_funded_task_payout() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;

        let (task_id, claimant) = seed_verified_task(&hub.state, 500).await;
        {
            let mut board = hub.state.board.write().await;
            board.set_capabilities(task_id, ["compute".to_string()].into_iter().collect()).unwrap();
        }

        assert!(handlers::try_settle_verified_task(&hub.state, task_id).await);
        // Minting rides on the payout being *confirmed*, not merely sent
        // -- the same evidence that credits reputation.
        assert_eq!(
            hub.state.board.read().await.exchange_account(&claimant).compute_balance,
            0,
            "nothing is minted off a transaction whose fate is still unknown"
        );
        confirm_submitted_payouts(&hub.state, &fake_node).await;

        let account = hub.state.board.read().await.exchange_account(&claimant);
        assert_eq!(account.compute_balance, 500, "the settled bounty amount, minted as compute on top of the payout");
    }

    #[tokio::test]
    async fn compute_minting_hook_does_not_fire_for_a_task_without_the_compute_capability() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;

        let (task_id, claimant) = seed_verified_task(&hub.state, 500).await;
        assert!(handlers::try_settle_verified_task(&hub.state, task_id).await);
        confirm_submitted_payouts(&hub.state, &fake_node).await;

        let account = hub.state.board.read().await.exchange_account(&claimant);
        assert_eq!(account.compute_balance, 0, "no compute tag, no compute minted");
    }
}

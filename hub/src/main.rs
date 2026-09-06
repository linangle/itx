use tracing::*;

mod auth;
mod board;
mod escrow_key;
mod handlers;
mod names;
mod node_client;
mod rate_limit;
mod store;

use anyhow::Result;
use argh::FromArgs;
use axum::http::{HeaderName, Method};
use axum::routing::{get, post};
use axum::Router;
use board::{PendingDeposit, Reputation, Task, TaskBoard, TaskStatus};
use escrow_key::EscrowSecret;
use btclib::crypto::{PrivateKey, PublicKey};
use btclib::util::Saveable;
use names::NameRegistry;
use node_client::NodeClient;
use std::net::SocketAddr;
use std::sync::Arc;
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
    #[argh(option, default = "String::new()")]
    /// comma-separated addresses of the reverse proxies in front of this
    /// hub, whose `X-Forwarded-For` header the rate limiter should
    /// believe. Empty (the default) trusts nothing and always charges the
    /// direct peer. Set this to the proxy's address when deploying behind
    /// one, and to nothing at all when not -- see
    /// `rate_limit::TrustedProxies` for why an un-proxied hub that
    /// honoured the header would have no working rate limit.
    trusted_proxies: String,
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

/// Periodically reopens abandoned claims, retries paying out any task
/// stuck `Verified` by an earlier failed payout attempt, and sweeps the
/// auth replay guard. Runs for the lifetime of the process.
async fn sweep_loop(state: Arc<AppState>) {
    let mut ticker = interval(Duration::from_secs(60));
    loop {
        ticker.tick().await;
        run_sweep_once(&state, chrono::Utc::now()).await;
    }
}

/// One sweep pass, pulled out of `sweep_loop` so tests can drive it
/// directly without waiting on a real 60-second timer. `now` is threaded
/// through (rather than each check calling `chrono::Utc::now()` itself) so
/// tests can simulate a deadline having passed without an actual sleep --
/// mirroring the board methods this delegates to, which already take `now`
/// for the same reason.
async fn run_sweep_once(state: &Arc<AppState>, now: chrono::DateTime<chrono::Utc>) {
    let reopened = {
        let mut board = state.board.write().await;
        board.expire_claims(now)
    };
    for task_id in reopened {
        persist_task_by_id(state, task_id, "expired-claim").await;
    }

    let cancelled = {
        let mut board = state.board.write().await;
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
        let mut board = state.board.write().await;
        board.resolve_expired_consensus_tasks(now)
    };
    for task_id in resolved {
        let task = persist_task_by_id(state, task_id, "resolved consensus").await;
        // A deadline-triggered resolution can ding reputation for several
        // assignees at once (every no-show/loser), not just whichever
        // pubkey happens to be at hand -- persist every one of them, or
        // the penalty is silently lost on the next restart. Batched into
        // one redb transaction rather than one write per assignee.
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
            if let Err(e) = state.store.save_reputation_batch(&entries) {
                error!(
                    "failed to persist reputation for consensus assignees of task {task_id} after resolution: {e}"
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
        let mut board = state.board.write().await;
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
            warn!("sweep: retried and paid out task {task_id}");
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
    rate_limit::cleanup(&state.rate_limits);
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
    println!(
        "loaded {} task(s), {} reputation record(s), {} faucet grant(s), {} pending escrow deposit(s), \
         {} exchange account(s), {} order(s), {} trade(s) from store",
        board.all_tasks().count(),
        board.all_reputation().count(),
        board.all_faucet_grants().count(),
        board.all_pending_deposits().count(),
        board.all_exchange_accounts().count(),
        board.all_orders().count(),
        board.all_trades().count(),
    );

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
    let replay_guard = match auth::ReplayGuard::restore(store.clone(), chrono::Utc::now()) {
        Ok((guard, restored)) => {
            println!("restored {restored} replay-guard signature(s) still inside the drift window");
            guard
        }
        Err(e) => {
            error!("could not restore the durable replay guard ({e}) -- falling back to refusing");
            println!(
                "WARNING: replay log unreadable ({e}); authenticated writes are refused for the \n\
                 next {}s while the post-restart replay window closes. Read routes are unaffected.",
                btclib::envelope::MAX_REQUEST_DRIFT_SECONDS,
            );
            auth::ReplayGuard::booting(chrono::Utc::now())
        }
    };

    let state = Arc::new(AppState {
        board: RwLock::new(board),
        store,
        node: NodeClient::new(node_addresses),
        operator_private_key,
        operator_public_key,
        payout_lock: Mutex::new(()),
        exchange_custody_private_key,
        exchange_custody_public_key,
        exchange_custody_payout_lock: Mutex::new(()),
        escrow_secret,
        rate_limits: rate_limit::new_table(),
        trusted_proxies,
        replay_guard,
        names: RwLock::new(names),
        net_worths: RwLock::new(None),
    });

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
        // Keyed by the pubkey's string form, not `PublicKey` itself --
        // same reason as `handlers::PAYOUT_IN_FLIGHT`: `PublicKey` has no
        // `Hash` impl.
        balances: Arc<AsyncMutex<std::collections::HashMap<String, u64>>>,
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
    }

    impl FakeNode {
        async fn spawn_empty() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
            let submitted = Arc::new(AsyncMutex::new(Vec::new()));
            let balances: Arc<AsyncMutex<std::collections::HashMap<String, u64>>> =
                Arc::new(AsyncMutex::new(std::collections::HashMap::new()));
            let connections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let hang_up_after_one = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let submitted_for_accept_loop = submitted.clone();
            let balances_for_accept_loop = balances.clone();
            let connections_for_accept_loop = connections.clone();
            let hang_up_for_accept_loop = hang_up_after_one.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        return;
                    };
                    connections_for_accept_loop.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let submitted = submitted_for_accept_loop.clone();
                    let balances = balances_for_accept_loop.clone();
                    let hang_up_after_one = hang_up_for_accept_loop.clone();
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
                                    let balance = balances.lock().await.get(&pk.to_string()).copied();
                                    let utxos = match balance {
                                        Some(balance) => vec![(
                                            TransactionOutput {
                                                value: balance,
                                                unique_id: Uuid::new_v4(),
                                                pubkey: pk,
                                            },
                                            false,
                                        )],
                                        None => vec![],
                                    };
                                    if Message::UTXOs(utxos).send_async(&mut socket).await.is_err()
                                    {
                                        return;
                                    }
                                }
                                Message::SubmitTransaction(tx) => {
                                    submitted.lock().await.push(tx);
                                    // fire-and-forget, matching the real protocol
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
            FakeNode { addr, submitted, balances, connections, hang_up_after_one }
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
        /// `pubkey`. Callable any time after spawning.
        async fn fund(&self, pubkey: PublicKey, balance: u64) {
            self.balances.lock().await.insert(pubkey.to_string(), balance);
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
        let state = Arc::new(AppState {
            board: RwLock::new(TaskBoard::new()),
            store,
            node: NodeClient::new(vec![node_address]),
            operator_private_key: operator_private_key.clone(),
            operator_public_key,
            payout_lock: Mutex::new(()),
            exchange_custody_private_key,
            exchange_custody_public_key,
            exchange_custody_payout_lock: Mutex::new(()),
            escrow_secret: EscrowSecret::generate(),
            rate_limits: rate_limit::new_table(),
            trusted_proxies,
            replay_guard,
            names: RwLock::new(NameRegistry::new()),
            net_worths: RwLock::new(None),
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

    #[tokio::test]
    async fn full_http_task_lifecycle_pays_out_through_a_real_router() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let agent_key = PrivateKey::new_key();

        let resp = hub
            .client
            .post(format!("{}/faucet", hub.base_url))
            .json(&envelope(&agent_key, "/faucet", ()))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        // a FRESH envelope (new signature) for an already-granted pubkey
        // -- distinct from the replay case, which reuses the same
        // signature (see `replayed_envelope_is_rejected`).
        let resp = hub
            .client
            .post(format!("{}/faucet", hub.base_url))
            .json(&envelope(&agent_key, "/faucet", ()))
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
        assert_eq!(result["paid"], true);

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
        let env = envelope(&agent_key, "/faucet", ());

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
    /// recipe: `/faucet` and `/exchange/deposit` both take a payload-less
    /// envelope, so before the path was bound in, one signed envelope was
    /// accepted at either. The signature is genuine and unexpired here --
    /// the only thing wrong with it is the door it is being presented at.
    #[tokio::test]
    async fn an_envelope_signed_for_one_route_is_rejected_at_another() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let agent_key = PrivateKey::new_key();

        let for_faucet = envelope(&agent_key, "/faucet", ());
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
            "an envelope signed for /faucet must not be accepted at /exchange/deposit"
        );

        // The same envelope at the route it was actually signed for still
        // works -- so the rejection above was the binding, not a broken
        // signature. (It also proves the failed attempt did not burn the
        // signature: the replay guard only claims it once verification
        // passes, junk bytes cannot spend someone else's slot.)
        let resp = hub
            .client
            .post(format!("{}/faucet", hub.base_url))
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

        let stale = envelope_at(&agent_key, "/faucet", (), Utc::now() - chrono::Duration::minutes(10));
        let resp = hub
            .client
            .post(format!("{}/faucet", hub.base_url))
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

    /// A prior version of `llms_txt` documented only `hash_match`/
    /// `consensus` tasks and operator-only posting -- it went stale as
    /// Features 1-3 (agent-to-agent posting, disputes, capabilities)
    /// shipped without anyone updating the one doc an agent actually
    /// reads to learn the API. This doesn't check the prose itself (too
    /// brittle), just that every endpoint/kind introduced since the
    /// original version is at least mentioned somewhere.
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

        let paid = handlers::try_settle_verified_task(&hub.state, task_id).await;
        assert!(paid);

        let status = hub.state.board.read().await.get_task(task_id).unwrap().status;
        assert_eq!(status, TaskStatus::Paid);

        let submitted = fake_node.wait_for_submitted_count(1).await;
        assert_eq!(submitted.len(), 1);
        let recipient_output = submitted[0].outputs.iter().find(|o| o.pubkey == claimant);
        assert_eq!(recipient_output.unwrap().value, 1_000);
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

        let board = hub.state.board.read().await;
        assert_eq!(board.get_task(task_a).unwrap().status, TaskStatus::Paid);
        assert_eq!(board.get_task(task_b).unwrap().status, TaskStatus::Paid);
        drop(board);
        assert_eq!(fake_node.wait_for_submitted_count(2).await.len(), 2);
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

        let account = hub.state.board.read().await.exchange_account(&claimant);
        assert_eq!(account.compute_balance, 0, "no compute tag, no compute minted");
    }
}

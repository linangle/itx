use tracing::*;

mod auth;
mod board;
mod handlers;
mod names;
mod node_client;
mod store;

use anyhow::Result;
use argh::FromArgs;
use axum::http::{HeaderName, Method};
use axum::routing::{get, post};
use axum::Router;
use board::{PendingDeposit, Reputation, Task, TaskBoard, TaskStatus};
use btclib::crypto::{PrivateKey, PublicKey};
use btclib::util::Saveable;
use names::NameRegistry;
use node_client::NodeClient;
use std::sync::Arc;
use store::HubStore;
use tokio::sync::{Mutex, RwLock};
use tokio::time::{interval, Duration};
use tower_http::cors::{Any, CorsLayer};
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
    pub store: HubStore,
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
    /// Display names for agents (see `names`). Its own lock rather than
    /// a field on `board`: the registry is presentation, the board is
    /// the economy, and a read of the leaderboard should not have to
    /// take a write lock on the board just to mint a name for an agent
    /// that turned up since the last request.
    pub names: RwLock<NameRegistry>,
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
    #[argh(option, default = "String::from(\"127.0.0.1:9000\")")]
    /// address of the blockchain node to talk to
    node_address: String,
    #[argh(option, default = "String::from(\"./hub.redb\")")]
    /// path to the hub's durable store (a redb database file)
    store_file: String,
    #[argh(option, default = "String::from(\"./hub_operator.priv.cbor\")")]
    /// path to the operator's private key (generated on first run if missing)
    operator_key_file: String,
}

fn load_or_create_operator_key(path: &str) -> Result<PrivateKey> {
    match PrivateKey::load_from_file(path) {
        Ok(key) => Ok(key),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("no operator key found at {path}, generating a new one...");
            let key = PrivateKey::new_key();
            key.save_to_file(path)?;
            Ok(key)
        }
        Err(e) => Err(e.into()),
    }
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
        println!("sweep: cancelled understaffed consensus task {task_id} past its join deadline");
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
                println!(
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
        println!("sweep: resolved consensus task {task_id} past its submission deadline");
    }

    let finalized = {
        let mut board = state.board.write().await;
        board.finalize_unchallenged_disputable_tasks(now)
    };
    for task_id in finalized {
        persist_task_by_id(state, task_id, "unchallenged disputable").await;
        println!("sweep: finalized unchallenged disputable task {task_id} past its dispute window");
    }

    let overdue_escrows: Vec<PendingDeposit> = {
        let board = state.board.read().await;
        board.overdue_reserved_escrows(now).into_iter().cloned().collect()
    };
    for deposit in overdue_escrows {
        let deposit_id = deposit.id;
        handlers::refund_escrow(state, &deposit).await;
        println!("sweep: refunded overdue unconfirmed escrow deposit {deposit_id}");
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
            println!("sweep: retried and paid out task {task_id}");
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
            println!("sweep: retried and settled dispute bond for task {task_id}");
        }
    }

    auth::cleanup_replay_guard();
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
            println!("failed to persist {context} task {task_id}: {e}");
        }
    }
    task
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Args = argh::from_env();

    let operator_private_key = load_or_create_operator_key(&args.operator_key_file)?;
    let operator_public_key = operator_private_key.public_key();
    println!("================================================================");
    println!("hub operator address -- fund this so the hub can pay out tasks/faucet grants:");
    println!("{operator_public_key}");
    println!("================================================================");

    let store = HubStore::open_or_create(&args.store_file)?;
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
    println!(
        "loaded {} task(s), {} reputation record(s), {} faucet grant(s), {} pending escrow deposit(s) from store",
        board.all_tasks().count(),
        board.all_reputation().count(),
        board.all_faucet_grants().count(),
        board.all_pending_deposits().count()
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

    let state = Arc::new(AppState {
        board: RwLock::new(board),
        store,
        node: NodeClient::new(args.node_address),
        operator_private_key,
        operator_public_key,
        payout_lock: Mutex::new(()),
        names: RwLock::new(names),
    });

    tokio::spawn(sweep_loop(state.clone()));

    let app = build_router(state);

    let addr = format!("0.0.0.0:{}", args.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("hub listening on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
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
    // `X-Total-Count` off a `/tasks` response. Without it the header is
    // still sent and still visible in devtools, but the browser hides it
    // from JavaScript -- a silent failure that looks like the hub never
    // set it. Response headers are not exposed cross-origin by default;
    // only a short safelist is.
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET])
        .expose_headers([HeaderName::from_static("x-total-count")]);

    Router::new()
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
        .route("/llms.txt", get(handlers::llms_txt))
        .layer(cors)
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

    /// A minimal stand-in for a real node: speaks just enough of the wire
    /// protocol (handshake, `FetchUTXOs`, `SubmitTransaction`) to drive
    /// the hub's real `NodeClient` in tests, without a real
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
    }

    impl FakeNode {
        async fn spawn_empty() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
            let submitted = Arc::new(AsyncMutex::new(Vec::new()));
            let balances: Arc<AsyncMutex<std::collections::HashMap<String, u64>>> =
                Arc::new(AsyncMutex::new(std::collections::HashMap::new()));
            let submitted_for_accept_loop = submitted.clone();
            let balances_for_accept_loop = balances.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        return;
                    };
                    let submitted = submitted_for_accept_loop.clone();
                    let balances = balances_for_accept_loop.clone();
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
                                _ => return,
                            }
                        }
                    });
                }
            });
            FakeNode { addr, submitted, balances }
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
        let operator_public_key = operator_private_key.public_key();
        let store_path = temp_store_path();
        let store = HubStore::open_or_create(&store_path).unwrap();
        let state = Arc::new(AppState {
            board: RwLock::new(TaskBoard::new()),
            store,
            node: NodeClient::new(node_address),
            operator_private_key: operator_private_key.clone(),
            operator_public_key,
            payout_lock: Mutex::new(()),
        });

        let app = build_router(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        TestHub {
            base_url,
            state,
            operator_key: operator_private_key,
            client: reqwest::Client::new(),
            store_path,
        }
    }

    fn envelope_at<T: Serialize>(key: &PrivateKey, payload: T, timestamp: DateTime<Utc>) -> Value {
        let pubkey_hex = key.public_key().to_string();
        let timestamp_str = timestamp.to_rfc3339();
        let payload_json = serde_json::to_string(&payload).unwrap();
        let signing_string = format!("{pubkey_hex}:{timestamp_str}:{payload_json}");
        let hash = Hash::hash_bytes(signing_string.as_bytes());
        let signature = Signature::sign_hash(&hash, key);
        json!({
            "pubkey": pubkey_hex,
            "timestamp": timestamp_str,
            "payload": payload,
            "signature": hex::encode(signature.to_bytes()),
        })
    }

    fn envelope<T: Serialize>(key: &PrivateKey, payload: T) -> Value {
        envelope_at(key, payload, Utc::now())
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
            .json(&envelope(&agent_key, ()))
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
            .json(&envelope(&agent_key, ()))
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
            .json(&envelope(&hub.operator_key, payload))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let task: Value = resp.json().await.unwrap();
        let task_id: Uuid = task["id"].as_str().unwrap().parse().unwrap();

        let resp = hub
            .client
            .post(format!("{}/tasks/{task_id}/claim", hub.base_url))
            .json(&envelope(&agent_key, handlers::ClaimPayload { task_id }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        let resp = hub
            .client
            .post(format!("{}/tasks/{task_id}/submit", hub.base_url))
            .json(&envelope(
                &agent_key,
                handlers::SubmitPayload { task_id, output: "wrong answer".to_string() },
            ))
            .send()
            .await
            .unwrap();
        let result: Value = resp.json().await.unwrap();
        assert_eq!(result["verified"], false);

        hub.client
            .post(format!("{}/tasks/{task_id}/claim", hub.base_url))
            .json(&envelope(&agent_key, handlers::ClaimPayload { task_id }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();

        let resp = hub
            .client
            .post(format!("{}/tasks/{task_id}/submit", hub.base_url))
            .json(&envelope(
                &agent_key,
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
            .json(&envelope(&impostor, payload))
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
            .json(&envelope(&hub.operator_key, payload))
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
        let env = envelope(&agent_key, ());

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
    }

    #[tokio::test]
    async fn stale_timestamp_envelope_is_rejected() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        let agent_key = PrivateKey::new_key();

        let stale = envelope_at(&agent_key, (), Utc::now() - chrono::Duration::minutes(10));
        let resp = hub
            .client
            .post(format!("{}/faucet", hub.base_url))
            .json(&stale)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
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
            .json(&envelope(&hub.operator_key, payload))
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
            .json(&envelope(&hub.operator_key, payload))
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
                .json(&envelope(agent, handlers::ClaimPayload { task_id }))
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
                    agent,
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
                &agent_c,
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
            .json(&envelope(&hub.operator_key, payload))
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
            .json(&envelope(&novice, handlers::ClaimPayload { task_id }))
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
            .json(&envelope(&veteran, handlers::ClaimPayload { task_id }))
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
                .json(&envelope(&hub.operator_key, payload))
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
                .json(&envelope(&hub.operator_key, payload))
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
            .json(&envelope(&hub.operator_key, blocked_payload))
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
            .json(&envelope(&hub.operator_key, now_allowed_payload))
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
            .json(&envelope(&impostor, handlers::CancelPayload { task_id }))
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
            .json(&envelope(&hub.operator_key, payload))
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
            .json(&envelope(&hub.operator_key, handlers::CancelPayload { task_id }))
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
            .json(&envelope(&hub.operator_key, handlers::CancelPayload { task_id }))
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
        board
            .claim_task(paid.id, agent.clone(), Utc::now() + chrono::Duration::minutes(5))
            .unwrap();
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

        let listed: Vec<Value> = hub
            .client
            .get(format!("{}/tasks", hub.base_url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(listed.len(), 1, "three tasks exist but only one is Open");
        assert_eq!(listed[0]["status"], "Open");
    }

    #[tokio::test]
    async fn status_filter_selects_a_single_status_and_all_returns_everything() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;
        seed_one_task_per_status(&hub.state).await;

        let paid: Vec<Value> = hub
            .client
            .get(format!("{}/tasks?status=Paid", hub.base_url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(paid.len(), 1);
        assert_eq!(paid[0]["status"], "Paid");

        // Case-insensitive, the same forgiving treatment `?capability=`
        // already gets.
        let lowercase: Vec<Value> = hub
            .client
            .get(format!("{}/tasks?status=paid", hub.base_url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(lowercase.len(), 1, "?status=paid and ?status=Paid are the same query");

        let all: Vec<Value> = hub
            .client
            .get(format!("{}/tasks?status=all", hub.base_url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(all.len(), 3, "every status, which is what the public board shows");
    }

    #[tokio::test]
    async fn unknown_status_is_a_legible_400_rather_than_a_serde_failure() {
        let operator_key = PrivateKey::new_key();
        let fake_node = FakeNode::spawn(operator_key.public_key(), 100_000_000).await;
        let hub = spawn_hub(operator_key, fake_node.addr.clone()).await;

        let resp = hub
            .client
            .get(format!("{}/tasks?status=banana", hub.base_url))
            .send()
            .await
            .unwrap();
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

        let resp = hub
            .client
            .get(format!("{}/tasks?status=all&limit=2", hub.base_url))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.headers().get("x-total-count").unwrap(), "3");
        let page: Vec<Value> = resp.json().await.unwrap();
        assert_eq!(page.len(), 2, "the page itself is still capped by limit");

        // And it tracks the filter, not the whole board.
        let resp = hub
            .client
            .get(format!("{}/tasks?status=Paid", hub.base_url))
            .send()
            .await
            .unwrap();
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
            let mut task = board.create_task(
                hub.state.operator_public_key.clone(),
                "dated".into(),
                10,
                Hash::hash_bytes(b"x"),
            );
            task.created_at = created_at;
            let id = task.id;
            board.restore_task(task);
            id
        };

        let task: Value = hub
            .client
            .get(format!("{}/tasks/{task_id}", hub.base_url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
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
            .json(&envelope(&hub.operator_key, python_payload))
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
            .json(&envelope(&hub.operator_key, rust_payload))
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
            .json(&envelope(&hub.operator_key, tagged_payload))
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
            .json(&envelope(&hub.operator_key, untagged_payload))
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
            .json(&envelope(&hub.operator_key, payload))
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
            .json(&envelope(&hub.operator_key, payload))
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
            .json(&envelope(&agent_key, payload))
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
            .json(&envelope(&agent_key, handlers::ConfirmEscrowPayload { escrow_id }))
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
            .json(&envelope(&agent_key, payload))
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
            .json(&envelope(&agent_key, handlers::ConfirmEscrowPayload { escrow_id }))
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
            .json(&envelope(&poster_key, payload))
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
            .json(&envelope(&poster_key, handlers::ConfirmEscrowPayload { escrow_id }))
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
            .json(&envelope(&poster_key, handlers::ClaimPayload { task_id }))
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
            .json(&envelope(&agent_key, payload))
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
                    agent_key,
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
            .json(&envelope(&agent_key, payload))
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
            .json(&envelope(&poster_key, payload))
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
            .json(&envelope(&poster_key, handlers::ConfirmEscrowPayload { escrow_id }))
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
            .json(&envelope(&hub.operator_key, handlers::CancelPayload { task_id }))
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
            .json(&envelope(&poster_key, payload))
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
            .json(&envelope(&poster_key, handlers::ConfirmEscrowPayload { escrow_id }))
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
                .json(&envelope(assignee, handlers::ClaimPayload { task_id }))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
        }
        hub.client
            .post(format!("{}/tasks/{task_id}/submit", hub.base_url))
            .json(&envelope(&assignee1, handlers::SubmitPayload { task_id, output: "42".to_string() }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        hub.client
            .post(format!("{}/tasks/{task_id}/submit", hub.base_url))
            .json(&envelope(&assignee2, handlers::SubmitPayload { task_id, output: "42".to_string() }))
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
            .json(&envelope(poster_key, payload))
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
            .json(&envelope(poster_key, handlers::ConfirmEscrowPayload { escrow_id }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let task_id: Uuid = task["id"].as_str().unwrap().parse().unwrap();

        hub.client
            .post(format!("{}/tasks/{task_id}/claim", hub.base_url))
            .json(&envelope(assignee_key, handlers::ClaimPayload { task_id }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        hub.client
            .post(format!("{}/tasks/{task_id}/submit", hub.base_url))
            .json(&envelope(assignee_key, handlers::SubmitPayload { task_id, output: "my answer".to_string() }))
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
            .json(&envelope(challenger_key, handlers::DisputeEscrowPayload { task_id, reason: reason.to_string() }))
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
            .json(&envelope(challenger_key, handlers::ConfirmDisputeEscrowPayload { task_id, escrow_id }))
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
            .json(&envelope(&assignee_key, handlers::DisputeEscrowPayload { task_id, reason: "self-dispute".to_string() }))
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
                &hub.operator_key,
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
                &hub.operator_key,
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
                &impostor_key,
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
}

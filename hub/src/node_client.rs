use tracing::*;

use anyhow::{Context, Result};
use btclib::crypto::PublicKey;
use btclib::network::Message;
use btclib::types::{Transaction, TransactionOutput};
use crate::metrics::Metrics;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
use tokio::net::TcpStream;
use tokio::sync::{Mutex, Semaphore};

/// How many connections the hub keeps open to the node it is talking to.
///
/// This one number spans the whole design space. At 1 the client is a
/// single persistent connection with everything queued behind it -- the
/// shape `wallet/src/core.rs` uses. At a large value it is close to the
/// old connect-per-call client, minus the handshakes. The value below was
/// chosen by measuring the leaderboard's balance fan-out, the worst case
/// on the hub, rather than picked as a plausible-looking default; the
/// numbers are in the commit that introduced it.
///
/// It also does something the old client did not: it bounds how many
/// sockets the hub can have open to the node *at all*. Before this, only
/// the net-worth sweep was bounded (by its own semaphore, and only
/// against itself); every other path could open as many connections as it
/// had concurrent requests.
const MAX_POOLED_CONNECTIONS: usize = 8;

/// A client for talking to a running blockchain node, over a small pool of
/// persistent connections.
///
/// **This reverses an earlier decision, so the earlier reasoning is worth
/// stating.** The client used to open a fresh TCP connection and run the
/// handshake for every single operation, on the grounds that a shared
/// connection would need either a mutex (serializing every node
/// interaction behind one lock) or reconnect logic, and that a per-call
/// connection means one failed request can never affect another. That was
/// a fair trade at the time. What changed is the measurement: ranking the
/// leaderboard by net worth fans out one balance lookup per agent, so a
/// single refresh of a fifty-agent field opened fifty TCP connections and
/// ran fifty handshakes against one node -- three round trips of protocol
/// to ask one question (§6.2 of the ecosystem plan).
///
/// Each of the three objections is answered rather than ignored:
///
/// - *Serialization.* A pool of `MAX_POOLED_CONNECTIONS` keeps real
///   concurrency; only a pool of one would serialize.
/// - *Reconnect logic.* It exists, and is one rule: an operation that
///   fails on a connection **taken from the pool** is retried once on a
///   fresh one, because a pooled socket the node closed while it sat idle
///   is indistinguishable from a live one until it is used. A failure on
///   a freshly-dialled connection is a real failure and is returned.
///   This rule only works for operations that read a reply, which is why
///   `send` does not pool at all -- see its own doc comment.
/// - *Isolation.* A caller **owns** its connection for the length of an
///   exchange -- it is moved out of the pool, not borrowed under a lock --
///   and a connection that errors is dropped instead of returned. So a
///   failed request cannot hand a half-read reply to the next caller.
///   This is stronger than the mutex-across-send-and-receive pattern in
///   `wallet/src/core.rs`: a task cancelled between its send and its
///   receive (an HTTP client hanging up mid-request, which the hub sees
///   routinely and the wallet never does) releases that mutex with the
///   unread half of a reply still in the socket. Here, cancellation drops
///   the connection, which is the correct thing to do with it.
///
/// Retrying is safe for the two read operations because each is
/// idempotent with respect to a *failed* attempt, and because a read
/// notices a dead socket: the reply never arrives.
///
/// `submit_transaction` is not a read, and that difference is load
/// bearing. It is fire-and-forget, so there is no reply whose absence
/// could reveal that the connection was already gone. A write to a
/// socket whose peer has closed succeeds -- the bytes land in the
/// kernel's buffer and the error, if it ever surfaces, surfaces on a
/// later write. So a pooled `send` could report success for a
/// transaction the node never saw, and the hub would go on to record
/// the payout as made. It does not pool for that reason: every send
/// dials a fresh connection, and completing the handshake is the
/// closest thing this protocol has to proof that the node is listening
/// at the moment we hand it the bytes.
///
/// That narrows the window rather than closing it. A transaction can
/// still be accepted by the OS and then dropped -- the node can die
/// between the handshake and reading the message, and it can reject
/// what it reads (see below) without the hub ever hearing about it.
/// Only an acknowledged submission would close it, which means a wire
/// protocol change; §6.5 of the ecosystem plan tracks that as the
/// settlement-honesty work.
///
/// Resubmitting is in any case not a thing to do casually here, since
/// the node rejects a duplicate transaction (equal fee on an
/// already-spoken-for input) and *strikes* the peer for it.
///
/// Holds an ordered list of node addresses rather than one: a fresh
/// connection always tries `addresses[0]` first and only falls through to
/// the next on failure. This is deliberately *not* load-balanced --
/// the hub's double-spend safety (`payout_lock`/`exchange_custody_payout_lock`)
/// assumes every call sees one consistent mempool view, so spreading
/// normal traffic across two independently-converging node mempools would
/// reintroduce race risk. Pooling makes that invariant something to
/// maintain rather than something that holds for free, so the pool
/// enforces it directly: it holds connections to **one** address at a
/// time, and adopting a new address discards the connections to the old
/// one (see `Pool::adopt`). Without that, a hub that failed over and then
/// saw its primary come back would sit on a mix of connections to both
/// nodes and quietly become load-balanced.
#[derive(Clone)]
pub struct NodeClient {
    addresses: Vec<String>,
    pool: Arc<Pool>,
    /// Where this client's pool counters go.
    ///
    /// Its own handle rather than a reference back to `AppState`, because
    /// a `NodeClient` is built before the state that will own it exists
    /// (and several tests build one with no state at all). Defaults to a
    /// private table nobody scrapes, so an uninstrumented client still
    /// works and simply reports into the void -- `with_metrics` is what
    /// joins it to the one `/metrics` renders.
    metrics: Arc<Metrics>,
}

/// The idle connections, and the permit that bounds how many exist.
struct Pool {
    idle: Mutex<PoolState>,
    /// One permit per connection the client may have open. Acquired for
    /// the whole of an operation, so `idle` can never grow past the
    /// permit count: a connection only exists while someone holds a
    /// permit for it.
    permits: Semaphore,
}

#[derive(Default)]
struct PoolState {
    /// The node address every connection in `connections` was dialled to.
    /// `None` before the first successful dial.
    address: Option<String>,
    connections: Vec<TcpStream>,
}

impl Pool {
    fn new(size: usize) -> Self {
        Pool {
            idle: Mutex::new(PoolState::default()),
            permits: Semaphore::new(size),
        }
    }

    /// Takes an idle connection, if there is one, along with the address
    /// it goes to.
    async fn take(&self) -> Option<(String, TcpStream)> {
        let mut state = self.idle.lock().await;
        let stream = state.connections.pop()?;
        // Only `Some` once a connection exists, and `pop` just proved one
        // does.
        let address = state.address.clone()?;
        Some((address, stream))
    }

    /// Returns a healthy connection for reuse -- unless the pool has since
    /// moved to a different node, in which case it is dropped. Keeping it
    /// would mean serving later requests from two nodes at once, which is
    /// exactly what the ordered-failover design exists to prevent.
    async fn put(&self, address: &str, stream: TcpStream) {
        let mut state = self.idle.lock().await;
        if state.address.as_deref() == Some(address) {
            state.connections.push(stream);
        }
    }

    /// Points the pool at `address`, discarding every connection to a
    /// previous one. Called after each successful dial, so the pool
    /// follows whichever node `connect` actually reached.
    async fn adopt(&self, address: &str) {
        let mut state = self.idle.lock().await;
        if state.address.as_deref() != Some(address) {
            if state.address.is_some() {
                info!(
                    "node connections now go to {address}; dropping {} pooled connection(s) to {}",
                    state.connections.len(),
                    state.address.as_deref().unwrap_or("-"),
                );
            }
            state.connections.clear();
            state.address = Some(address.to_string());
        }
    }
}

impl NodeClient {
    pub fn new(addresses: Vec<String>) -> Self {
        Self::with_pool_size(addresses, MAX_POOLED_CONNECTIONS)
    }

    /// A client with a pool of exactly `size` connections. Exists so tests
    /// (and the benchmark that chose `MAX_POOLED_CONNECTIONS`) can pin the
    /// size instead of inheriting whatever the default happens to be --
    /// notably size 1, where the pool is a single persistent connection
    /// and every caller queues behind it.
    pub fn with_pool_size(addresses: Vec<String>, size: usize) -> Self {
        NodeClient {
            addresses,
            pool: Arc::new(Pool::new(size.max(1))),
            metrics: Metrics::new(),
        }
    }

    /// Points this client's counters at `metrics`, which is how the pool
    /// numbers reach `/metrics`. Consuming and returning `self` so the
    /// wiring in `main` stays one expression, and so a client that is
    /// never joined up is a visibly unfinished line rather than a silent
    /// gap in the dashboard.
    pub fn with_metrics(mut self, metrics: Arc<Metrics>) -> Self {
        self.metrics = metrics;
        self
    }

    /// Dials a node, preferring earlier addresses, and points the pool at
    /// whichever one answered.
    async fn connect(&self) -> Result<(String, TcpStream)> {
        let mut last_err = None;
        for address in &self.addresses {
            match Self::connect_one(address).await {
                Ok(stream) => {
                    self.metrics.node_connections_opened.fetch_add(1, Ordering::Relaxed);
                    self.pool.adopt(address).await;
                    return Ok((address.clone(), stream));
                }
                Err(e) => {
                    warn!("node at {address} unreachable, trying next: {e}");
                    last_err = Some(e);
                }
            }
        }
        // Counted only once every address has been tried, so the number
        // means "the hub had no node" rather than "one address was
        // down" -- with a failover list configured those are very
        // different incidents and only the first is an outage.
        self.metrics.node_connect_failures.fetch_add(1, Ordering::Relaxed);
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no node addresses configured")))
    }

    async fn connect_one(address: &str) -> Result<TcpStream> {
        let mut stream = TcpStream::connect(address)
            .await
            .with_context(|| format!("failed to connect to node at {address}"))?;
        btclib::network::perform_handshake_initiator(&mut stream)
            .await
            .map_err(|e| anyhow::anyhow!("handshake with node at {address} failed: {e}"))?;
        Ok(stream)
    }

    /// Sends `message` and waits for the node's reply, on a pooled
    /// connection where one is available.
    ///
    /// The connection is held for the whole exchange and only returned to
    /// the pool once the reply has been read, which is what keeps two
    /// concurrent operations from interleaving their bytes on one socket.
    /// Takes a pool permit, distinguishing "one was free" from "we
    /// queued for one".
    ///
    /// The distinction is the whole point. The pool is a fixed size, so
    /// saturation does not show up as an error anywhere -- it shows up as
    /// every node operation quietly getting slower while the hub waits
    /// its turn. By the time that is visible in request latency it has
    /// already been true for a while, and nothing in the logs says why.
    /// `try_acquire` costs one atomic on the common path and turns that
    /// into a counter you can alert on.
    async fn acquire_permit(&self) -> tokio::sync::SemaphorePermit<'_> {
        if let Ok(permit) = self.pool.permits.try_acquire() {
            return permit;
        }
        self.metrics.node_pool_saturation_waits.fetch_add(1, Ordering::Relaxed);
        let queued_at = Instant::now();
        let permit = self
            .pool
            .permits
            .acquire()
            .await
            .expect("the node client's semaphore is never closed");
        self.metrics
            .node_pool_wait_ms_total
            .fetch_add(queued_at.elapsed().as_millis() as u64, Ordering::Relaxed);
        permit
    }

    async fn request(&self, message: &Message) -> Result<Message> {
        let _permit = self.acquire_permit().await;

        if let Some((address, mut stream)) = self.pool.take().await {
            match Self::exchange(&mut stream, message).await {
                Ok(reply) => {
                    self.metrics.node_connections_reused.fetch_add(1, Ordering::Relaxed);
                    self.pool.put(&address, stream).await;
                    return Ok(reply);
                }
                Err(e) => {
                    self.metrics.node_connections_retried.fetch_add(1, Ordering::Relaxed);
                    // Dropping `stream` rather than returning it is the
                    // point: a failed exchange may have left an unread
                    // reply (or half of one) in the socket, and the next
                    // caller would read it as their own answer.
                    debug!("pooled connection to {address} failed ({e}); retrying on a fresh one");
                }
            }
        }

        let (address, mut stream) = self.connect().await?;
        let reply = Self::exchange(&mut stream, message).await?;
        self.pool.put(&address, stream).await;
        Ok(reply)
    }

    /// Sends `message` with no reply expected, always on a freshly
    /// dialled connection.
    ///
    /// **Deliberately does not take a connection from the pool**, unlike
    /// `request`. `request` can afford to try a pooled socket first
    /// because it waits for a reply, so a socket the node closed while it
    /// sat idle announces itself and the operation retries on a fresh
    /// one. A send has no reply to wait for, and writing to a closed
    /// socket does not fail: the bytes go into the kernel's send buffer
    /// and `send_async` returns `Ok`. Pooled, this reported success for
    /// transactions the node never received -- measured at two of four
    /// submissions lost against a node that closed each connection after
    /// serving it, which is what the node does on every restart and after
    /// every rejected transaction. Since the hub treats a successful
    /// submit as "paid" (`handlers::settle_one_payout_inner` and the
    /// sweep both stop tracking a task once it is), those were silent
    /// losses of real payouts.
    ///
    /// The handshake a fresh dial completes is the check a send otherwise
    /// has no way to make. It costs one connection per submission, which
    /// is a payout-rate cost, not a hot-path one -- balance lookups and
    /// chain-tip reads still pool.
    ///
    /// The connection is still handed to the pool afterwards: it is live,
    /// and a later read can use it. If the node rejects the transaction
    /// and closes, that pooled socket is dead, and `request`'s retry is
    /// exactly the thing that handles it.
    async fn send(&self, message: &Message) -> Result<()> {
        let _permit = self.acquire_permit().await;

        let (address, mut stream) = self.connect().await?;
        message.send_async(&mut stream).await?;
        self.pool.put(&address, stream).await;
        Ok(())
    }

    async fn exchange(stream: &mut TcpStream, message: &Message) -> Result<Message> {
        message.send_async(stream).await?;
        Ok(Message::receive_async(stream).await?)
    }

    /// Current chain height, from whichever configured node answers first
    /// -- see `connect()`'s doc comment for the failover order. Used by
    /// `GET /health` to prove the hub can actually reach a node, not just
    /// that its own process is alive.
    pub async fn chain_tip(&self) -> Result<u32> {
        match self.request(&Message::AskChainTip).await? {
            Message::ChainTip(height, _work) => Ok(height),
            other => anyhow::bail!("unexpected response from node: {other:?}"),
        }
    }

    /// Every UTXO currently belonging to `pubkey`, as reported by the
    /// node -- including whether the node's own mempool view considers
    /// each one already spoken for (`marked`).
    pub async fn fetch_utxos(&self, pubkey: &PublicKey) -> Result<Vec<(bool, TransactionOutput)>> {
        match self.request(&Message::FetchUTXOs(pubkey.clone())).await? {
            Message::UTXOs(utxos) => Ok(utxos
                .into_iter()
                .map(|(output, marked)| (marked, output))
                .collect()),
            other => anyhow::bail!("unexpected response from node: {other:?}"),
        }
    }

    /// Total spendable balance: everything not already marked as pending
    /// in the node's own mempool view.
    pub async fn balance(&self, pubkey: &PublicKey) -> Result<u64> {
        let utxos = self.fetch_utxos(pubkey).await?;
        Ok(utxos
            .iter()
            .filter(|(marked, _)| !marked)
            .map(|(_, output)| output.value)
            .sum())
    }

    /// Submits a transaction and returns as soon as it's sent -- the node
    /// protocol doesn't send an acknowledgement back for this message
    /// (the wallet and miner both already rely on this same fire-and-
    /// forget behavior), so success here means "accepted for delivery,"
    /// not "confirmed."
    ///
    /// Task bounties no longer treat the difference as academic: they go
    /// to `TaskStatus::Submitted` and the sweep asks the chain what
    /// became of them (`board::PayoutAttempt`). Faucet grants, escrow
    /// disbursement and exchange withdrawals still take a successful
    /// send as payment -- see plan §6.5 for why, and what it would take
    /// to fix.
    pub async fn submit_transaction(&self, transaction: Transaction) -> Result<()> {
        self.send(&Message::SubmitTransaction(transaction)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    /// A connected `TcpStream` with nothing meaningful on the other end.
    /// These tests are about the pool's bookkeeping -- which connections
    /// it keeps and which it discards -- not about what travels over one,
    /// so any real socket will do.
    async fn dummy_stream() -> TcpStream {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stream, _) = tokio::join!(
            async { TcpStream::connect(addr).await.unwrap() },
            async { listener.accept().await.unwrap() }
        );
        stream
    }

    #[tokio::test]
    async fn a_returned_connection_is_available_to_the_next_caller() {
        let pool = Pool::new(4);
        pool.adopt("node-a").await;
        pool.put("node-a", dummy_stream().await).await;

        let taken = pool.take().await;
        assert!(taken.is_some(), "a connection put back must be reusable");
        assert_eq!(taken.unwrap().0, "node-a");
        assert!(
            pool.take().await.is_none(),
            "the pool held one connection, so a second take must find nothing"
        );
    }

    #[tokio::test]
    async fn adopting_a_new_node_discards_the_connections_to_the_old_one() {
        // The failover invariant: the hub's double-spend safety assumes
        // every call sees one node's mempool. A pool that kept both sets
        // after a failover would quietly load-balance across two.
        let pool = Pool::new(4);
        pool.adopt("node-a").await;
        pool.put("node-a", dummy_stream().await).await;
        pool.put("node-a", dummy_stream().await).await;

        pool.adopt("node-b").await;

        assert!(
            pool.take().await.is_none(),
            "connections to the node we failed away from must not survive the switch"
        );
    }

    #[tokio::test]
    async fn a_connection_finishing_after_a_failover_is_dropped_rather_than_pooled() {
        // The other half of the same invariant, and the easier half to
        // miss: an operation already in flight against the old node when
        // the switch happened still has a connection to hand back.
        let pool = Pool::new(4);
        pool.adopt("node-a").await;
        pool.adopt("node-b").await;

        pool.put("node-a", dummy_stream().await).await;

        assert!(
            pool.take().await.is_none(),
            "a late return from the previous node must be discarded, not pooled"
        );
    }

    #[tokio::test]
    async fn take_reports_the_address_a_pooled_connection_belongs_to() {
        // `request` needs this to put the connection back under the right
        // address; getting it wrong would defeat the check above.
        let pool = Pool::new(2);
        pool.adopt("node-b").await;
        pool.put("node-b", dummy_stream().await).await;

        assert_eq!(pool.take().await.unwrap().0, "node-b");
    }
}

/// The benchmark that chose `MAX_POOLED_CONNECTIONS`, and the one to
/// re-run if it is ever revisited. Ignored by default because it needs a
/// real node; point it at one and run:
///
/// ```text
/// ITX_BENCH_NODE=127.0.0.1:9000 \
///   cargo test -p hub -- --ignored --nocapture node_pool_benchmark
/// ```
///
/// It measures the hub's worst case -- the leaderboard's net-worth
/// fan-out, one balance lookup per agent -- against the *old* client's
/// behaviour and against a pool of each candidate size. The baseline is
/// not an approximation of the old code: `connect_per_call` below is the
/// old code path, built from the same two private primitives the pooled
/// client uses.
#[cfg(test)]
mod benchmark {
    use super::*;
    use btclib::crypto::PrivateKey;

    /// One net-worth sweep's worth of lookups, issued concurrently the
    /// way `handlers::net_worth_snapshot` issues them.
    const FIELD: usize = 50;
    const ROUNDS: usize = 10;

    /// Exactly what `NodeClient` did before pooling: dial, handshake,
    /// one exchange, drop the connection.
    async fn connect_per_call(address: &str, pubkey: &PublicKey) -> Result<()> {
        let mut stream = NodeClient::connect_one(address).await?;
        NodeClient::exchange(&mut stream, &Message::FetchUTXOs(pubkey.clone())).await?;
        Ok(())
    }

    async fn timed<F, Fut>(rounds: usize, round: F) -> f64
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let started = std::time::Instant::now();
        for _ in 0..rounds {
            round().await;
        }
        started.elapsed().as_secs_f64() * 1000.0 / rounds as f64
    }

    #[tokio::test]
    #[ignore]
    async fn node_pool_benchmark() {
        let Ok(address) = std::env::var("ITX_BENCH_NODE") else {
            println!("set ITX_BENCH_NODE=<host:port> to run this against a live node");
            return;
        };

        let field: Vec<PublicKey> = (0..FIELD).map(|_| PrivateKey::new_key().public_key()).collect();

        let baseline = timed(ROUNDS, || {
            let address = address.clone();
            let field = field.clone();
            async move {
                let mut lookups = tokio::task::JoinSet::new();
                for pubkey in field {
                    let address = address.clone();
                    lookups.spawn(async move { connect_per_call(&address, &pubkey).await });
                }
                while let Some(result) = lookups.join_next().await {
                    result.unwrap().unwrap();
                }
            }
        })
        .await;
        println!("connect-per-call (the old client): {baseline:7.2}ms per {FIELD}-agent sweep");

        for size in [1usize, 2, 4, 8, 16, 32, 64] {
            let client = NodeClient::with_pool_size(vec![address.clone()], size);
            // A warm round first, so what follows is steady-state reuse
            // rather than the cost of filling an empty pool.
            let field_for_warmup = field.clone();
            let warm = client.clone();
            let mut warmup = tokio::task::JoinSet::new();
            for pubkey in field_for_warmup {
                let warm = warm.clone();
                warmup.spawn(async move { warm.balance(&pubkey).await });
            }
            while let Some(result) = warmup.join_next().await {
                result.unwrap().unwrap();
            }

            let elapsed = timed(ROUNDS, || {
                let client = client.clone();
                let field = field.clone();
                async move {
                    let mut lookups = tokio::task::JoinSet::new();
                    for pubkey in field {
                        let client = client.clone();
                        lookups.spawn(async move { client.balance(&pubkey).await });
                    }
                    while let Some(result) = lookups.join_next().await {
                        result.unwrap().unwrap();
                    }
                }
            })
            .await;
            println!(
                "pool of {size:>2}: {elapsed:7.2}ms per {FIELD}-agent sweep  ({:.2}x the old client)",
                baseline / elapsed
            );
        }
    }
}

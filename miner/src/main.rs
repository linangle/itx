use tracing::*;

use anyhow::{anyhow, Result};
use btclib::crypto::PublicKey;
use btclib::network::Message;
use btclib::types::Block;
use btclib::util::Saveable;
use clap::Parser;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::{interval, timeout, Duration, Instant};
use tracing_subscriber::prelude::*;

/// How many nonce attempts one `mine()` call makes -- also the size of
/// the per-thread nonce window `thread_start_nonce` partitions across
/// mining threads.
const STEPS_PER_CALL: usize = 2_000_000;
/// How long `submit_block` waits on any single node before giving up on
/// it and moving to the next -- so one unreachable node can't stall (or,
/// without a bound at all, hang forever on) submission to the rest.
const SUBMIT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a link waits before dialling a node again after a failed
/// attempt, and the ceiling that wait climbs to on repeated failures.
///
/// The floor is short because the ordinary case this exists for is a node
/// restarting -- an upgrade, a reboot, a maintenance window -- and it is
/// back within seconds. The ceiling exists so a node that is down for an
/// afternoon is dialled a couple of times a minute rather than twelve,
/// and so a mistyped address is not a connection attempt every five
/// seconds forever.
const RECONNECT_BACKOFF_START: Duration = Duration::from_secs(1);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// The nonce a mining thread should start its search from, given its
/// index among `N` threads all mining the same cloned template in
/// parallel. Partitions each pass into per-thread windows -- thread `t`
/// searches `[t*steps, (t+1)*steps]` -- rather than every thread
/// redundantly searching the identical `[0, steps]` window `mine`'s own
/// "always start from self.nonce (0 for a fresh template), +1 per step"
/// behavior would otherwise produce if N threads ran the unmodified
/// single-thread logic. (Windows overlap by exactly one nonce at each
/// boundary, since `mine(steps)` checks `steps+1` values, not `steps` --
/// immaterial at a 1-in-2,000,000 scale, not worth engineering around.)
///
/// Deliberately NOT rolling/advancing across outer-loop passes: every
/// pass re-clones the same still-nonce-0 template and calls this again,
/// so thread `t` rescans the same window every pass until the template
/// actually changes -- exactly the property the single-threaded version
/// already had before this change (it only ever rescanned `[0, steps]`
/// until a new template arrived), just now running N-wide instead of
/// introducing a new class of staleness.
fn thread_start_nonce(thread_index: usize, steps: usize) -> u64 {
    thread_index as u64 * steps as u64
}

/// Repeatedly mines against whatever's currently in `template` using a
/// nonce window starting at `thread_start_nonce(thread_index, STEPS_PER_CALL)`,
/// sending anything found down `sender`. A free function (not a `Miner`
/// method) so it only depends on the shared mining state, not a live
/// network connection -- letting it be spawned and tested directly
/// without a real `Miner`/node round trip.
fn spawn_mining_thread(
    thread_index: usize,
    template: Arc<std::sync::Mutex<Option<Block>>>,
    mining: Arc<AtomicBool>,
    sender: flume::Sender<Block>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || loop {
        if mining.load(Ordering::Relaxed) {
            if let Some(mut block) = template.lock().unwrap().clone() {
                block.header.nonce = thread_start_nonce(thread_index, STEPS_PER_CALL);
                println!(
                    "Mining block with target: {} (thread {thread_index})",
                    block.header.target
                );
                if block.header.mine(STEPS_PER_CALL) {
                    println!("Block mined: {}", block.hash());
                    sender.send(block).expect("Failed to send mined block");
                    mining.store(false, Ordering::Relaxed);
                }
            }
        } else {
            // Genuinely idle (no template yet, or the last one's already
            // exhausted/submitted): sleep instead of busy-spinning via
            // `yield_now()`. One OS thread per logical core, all of them
            // spinning as fast as the scheduler allows, saturates every
            // core continuously even while doing no useful work -- which
            // starves this same process's own tokio runtime (the one
            // driving `run()`'s 5-second template-refresh timer and the
            // network I/O to actually fetch a new template) of scheduling
            // time. A tick that's ready but never promptly polled because
            // no worker thread can get scheduled might as well not have
            // fired; this is what actually made a fresh mempool
            // transaction sit unpicked-up for minutes, not just which
            // request `fetch_and_validate_template` happened to send.
            // 10ms caps the added latency to notice `mining` flip back to
            // true at a level that's negligible next to that.
            thread::sleep(Duration::from_millis(10));
        }
    })
}

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// comma-separated node addresses. The first is used as the template
    /// source; every one of them (including the first) receives a
    /// submitted block once mined.
    #[arg(short, long, value_delimiter = ',')]
    addresses: Vec<String>,
    #[arg(short, long)]
    public_key_file: String,
}

/// One configured node address, and the connection to it if there is one
/// right now.
///
/// **The miner used to hold connections rather than addresses**, opened
/// once at startup and never reopened: any node restart, ban or closed
/// socket ended `run()` and the process exited. It survived only because
/// `Restart=always` started it again five seconds later, which meant
/// every node restart cost a miner restart and the mining pass in
/// progress -- and during a maintenance window, with the node down for
/// longer than that, a treasury host whose miner was in a restart cycle
/// rather than waiting. Keeping the address is what lets a link be
/// reopened instead.
struct NodeLink {
    address: String,
    stream: Option<TcpStream>,
    /// How long to wait after the *next* failure. Doubles up to
    /// `RECONNECT_BACKOFF_MAX`, and is reset by a successful connection.
    backoff: Duration,
    /// When dialling is allowed again. `Instant::now()` means "on the
    /// next attempt", which is what a lost connection sets it to: a node
    /// that just closed a socket may already be back.
    next_attempt: Instant,
}

struct Miner {
    public_key: PublicKey,
    links: Vec<Mutex<NodeLink>>,
    current_template: Arc<std::sync::Mutex<Option<Block>>>,
    mining: Arc<AtomicBool>,
    mined_block_sender: flume::Sender<Block>,
    mined_block_receiver: flume::Receiver<Block>,
}

impl NodeLink {
    fn new(address: String) -> Self {
        NodeLink {
            address,
            stream: None,
            backoff: RECONNECT_BACKOFF_START,
            next_attempt: Instant::now(),
        }
    }

    /// The live connection to this node, dialling if there is not one and
    /// the backoff has elapsed. `None` means there is no usable
    /// connection right now and the caller should move on -- the reason
    /// has already been logged here, so a caller that simply carries on
    /// is not swallowing it.
    ///
    /// **Dials only when the caller is about to send something.** A bare
    /// TCP connect that opens and closes without completing the p2p
    /// handshake is a severe violation the node bans for an hour
    /// (`node/src/ban.rs`), so a reconnect loop that probed for liveness
    /// would ban the miner from its own node.
    async fn connected(&mut self) -> Option<&mut TcpStream> {
        if self.stream.is_none() {
            if Instant::now() < self.next_attempt {
                return None;
            }
            match Miner::connect_and_handshake(&self.address).await {
                Ok(stream) => {
                    println!("connected to node {}", self.address);
                    self.stream = Some(stream);
                    self.backoff = RECONNECT_BACKOFF_START;
                }
                Err(e) => {
                    warn!(
                        "cannot reach node {}: {e}; trying again in {}s",
                        self.address,
                        self.backoff.as_secs()
                    );
                    self.next_attempt = Instant::now() + self.backoff;
                    self.backoff = (self.backoff * 2).min(RECONNECT_BACKOFF_MAX);
                    return None;
                }
            }
        }
        self.stream.as_mut()
    }

    /// Forgets a connection that has failed, so the next call to
    /// `connected` opens a new one. A half-used socket is not reusable:
    /// the peer may have read half a message before going away, and
    /// anything written onto it after that is framed against nothing.
    fn disconnect(&mut self, reason: &str) {
        if self.stream.take().is_some() {
            warn!("lost the connection to node {}: {reason}", self.address);
        }
        // Not backed off: this is the first failure of a connection that
        // was working a moment ago, and the node may already be back.
        self.next_attempt = Instant::now();
    }
}

impl Miner {
    async fn connect_and_handshake(address: &str) -> Result<TcpStream> {
        let mut stream = TcpStream::connect(address).await?;
        btclib::network::perform_handshake_initiator(&mut stream)
            .await
            .map_err(|e| anyhow!("handshake with {} failed: {}", address, e))?;
        Ok(stream)
    }

    /// Opens a link to every address, in order, and tries each one once.
    ///
    /// **An address that does not answer is no longer fatal, not even the
    /// first.** It was: with no template source there is nothing to mine,
    /// so the miner exited and systemd restarted it. But the case that
    /// produces an unreachable primary at startup is almost always the
    /// node still coming up -- a reboot, an upgrade, the two units
    /// starting together -- and exiting turns "wait a moment" into a
    /// restart loop that competes with the thing it is waiting for. The
    /// link retries on its own; a genuinely wrong address says so in the
    /// log on every attempt instead of taking the process down.
    async fn new(addresses: Vec<String>, public_key: PublicKey) -> Result<Self> {
        if addresses.is_empty() {
            return Err(anyhow!("at least one node address is required"));
        }
        let mut links = Vec::new();
        for address in addresses {
            let mut link = NodeLink::new(address);
            link.connected().await;
            links.push(Mutex::new(link));
        }
        let (mined_block_sender, mined_block_receiver) = flume::unbounded();
        Ok(Self {
            public_key,
            links,
            current_template: Arc::new(std::sync::Mutex::new(None)),
            mining: Arc::new(AtomicBool::new(false)),
            mined_block_sender,
            mined_block_receiver,
        })
    }

    async fn run(&self) -> Result<()> {
        let thread_count = thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        println!("starting {thread_count} mining thread(s)");
        for thread_index in 0..thread_count {
            spawn_mining_thread(
                thread_index,
                self.current_template.clone(),
                self.mining.clone(),
                self.mined_block_sender.clone(),
            );
        }
        let mut template_interval = interval(Duration::from_secs(5));
        loop {
            let receiver_clone = self.mined_block_receiver.clone();
            tokio::select! {
                _ = template_interval.tick() => {
                    // Not `?`. A node that is restarting, or a socket
                    // that closed, is an ordinary event in the life of
                    // this process, and ending `run()` over it is what
                    // T22 was. The link retries on the next tick.
                    if let Err(e) = self.fetch_template().await {
                        warn!("no fresh template: {e}");
                        self.stop_mining_on_a_stale_template();
                    }
                }
                Ok(mined_block) = receiver_clone.recv_async() => {
                    self.submit_block(mined_block).await;
                    // With N mining threads searching independent nonce
                    // windows on the same pass, more than one can
                    // legitimately find a valid nonce before either
                    // learns the template is stale (mine() can't be
                    // cancelled mid-call) -- discard anything else
                    // already queued rather than submitting it too.
                    while let Ok(stale) = self.mined_block_receiver.try_recv() {
                        println!("discarding a stale block found by another thread in the same pass: {}", stale.hash());
                    }
                }
            }
        }
    }

    /// Fetches a genuinely fresh template from the primary node and swaps
    /// it in, unconditionally -- called on every timer tick regardless of
    /// whether a mining pass is already in progress. This used to only
    /// happen while idle (a pass in progress got a cheap `ValidateTemplate`
    /// tip-match check instead), which meant a transaction relayed to the
    /// mempool while a pass was already underway had no way to be picked
    /// up until that pass either found a block or the tip moved out from
    /// under it -- unbounded in the worst case, and increasingly likely
    /// to actually bite as retargeting pushes real mining passes toward
    /// taking a non-trivial amount of time (that's the whole point of
    /// `IDEAL_BLOCK_TIME`). Refetching every tick bounds that wait to
    /// this interval instead. It also happens to close a related latent
    /// issue for free: each mining thread's nonce window is fixed and
    /// never advances within a single template (see `thread_start_nonce`'s
    /// own doc comment), so a template that never changed would have
    /// every thread fruitlessly rescanning the exact same nonces forever
    /// if no valid one existed in their windows -- a fresh template every
    /// few seconds (a new timestamp alone changes every thread's hash
    /// landscape over that same nonce range) means that's no longer a
    /// permanent stall, just a wasted pass.
    async fn fetch_template(&self) -> Result<()> {
        println!("Fetching new template");
        let message = Message::FetchTemplate(self.public_key.clone());
        // Template fetch/validate deliberately only ever talks to the
        // primary (index 0) node -- there's no multi-source-template
        // feature here, only multi-target submission (see `submit_block`).
        let mut link = self.links[0].lock().await;
        if link.connected().await.is_none() {
            return Err(anyhow!("no connection to the primary node {}", link.address));
        }

        // Held across both halves of the exchange, where this used to
        // take the lock twice: the reply belongs to the request, and a
        // second caller that slipped in between would read the other's
        // template off the socket.
        // Annotated, because the two halves fail with different
        // ciborium error types (serialize, then deserialize) and only a
        // common one lets both use `?` here.
        let exchange: Result<Message> = async {
            let stream = link.stream.as_mut().expect("connected() just returned one");
            message.send_async(&mut *stream).await?;
            Ok(Message::receive_async(&mut *stream).await?)
        }
        .await;

        match exchange {
            Ok(Message::Template(template)) => {
                println!(
                    "Received new template with target: {}",
                    template.header.target
                );
                *self.current_template.lock().unwrap() = Some(template);
                self.mining.store(true, Ordering::Relaxed);
                Ok(())
            }
            Ok(_) => {
                // The socket is still open but the conversation is out of
                // step, which nothing here can resynchronise.
                link.disconnect("unexpected message in reply to a template request");
                Err(anyhow!("Unexpected message received when fetching template"))
            }
            Err(e) => {
                link.disconnect(&e.to_string());
                Err(e)
            }
        }
    }

    /// Stops mining and drops the template, whenever the primary node
    /// cannot be reached.
    ///
    /// **Dropping it is the point, not stopping.** A template names the
    /// tip it extends, and the node that has just gone away is the node
    /// that decides what the tip is; mining on across a restart means
    /// submitting a block built on the old one when it comes back. The
    /// node rejects that, and a rejected block is a strike -- three in
    /// ten minutes bans the miner from its own node for an hour
    /// (`node/src/ban.rs`, whose comment names this exact case). Idling
    /// costs a few seconds of hashing; the ban costs an hour of blocks.
    fn stop_mining_on_a_stale_template(&self) {
        self.mining.store(false, Ordering::Relaxed);
        *self.current_template.lock().unwrap() = None;
    }

    /// Submits `block` to every configured node, independently -- a dead
    /// or slow node must not stop (or, by propagating an error out of the
    /// `select!` arm that calls this, crash) submission to the rest,
    /// since the whole point of multiple targets is redundancy.
    async fn submit_block(&self, block: Block) {
        println!("submitting mined block to {} node(s)", self.links.len());
        let message = Message::SubmitTemplate(block);
        for link in self.links.iter() {
            let mut link = link.lock().await;
            // Reconnects here too: a node that was unreachable when the
            // last template was fetched may be back, and this is a block
            // -- there is no second chance to deliver it.
            if link.connected().await.is_none() {
                continue;
            }
            let stream = link.stream.as_mut().expect("connected() just returned one");
            let sent = timeout(SUBMIT_TIMEOUT, message.send_async(&mut *stream)).await;
            match sent {
                Ok(Ok(())) => {}
                Ok(Err(e)) => link.disconnect(&format!("submitting a block failed: {e}")),
                Err(_) => link.disconnect("timed out submitting a block"),
            }
        }
        self.mining.store(false, Ordering::Relaxed);
    }
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

    let cli = Cli::parse();
    let public_key = PublicKey::load_from_file(&cli.public_key_file)
        .map_err(|e| anyhow!("Error reading public key: {}", e))?;
    let miner = Miner::new(cli.addresses, public_key).await?;
    miner.run().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use btclib::crypto::PrivateKey;
    use btclib::sha256::Hash;
    use btclib::types::{BlockHeader, Transaction, TransactionOutput};
    use btclib::util::MerkleRoot;
    use chrono::Utc;
    use tokio::net::TcpListener;
    use uuid::Uuid;

    /// A template paying `pubkey`, which is all a miner reads off one
    /// here: `FakeMinerPeer::spawn_template_source` answers with this.
    fn template_for(pubkey: &PublicKey) -> Block {
        let coinbase = Transaction::new(
            vec![],
            vec![TransactionOutput {
                value: 1,
                unique_id: Uuid::new_v4(),
                pubkey: pubkey.clone(),
            }],
        );
        let merkle_root = MerkleRoot::calculate(&[coinbase.clone()]);
        let header = BlockHeader::new(Utc::now(), 0, Hash::zero(), merkle_root, btclib::MIN_TARGET);
        Block::new(header, vec![coinbase])
    }

    fn dummy_block(target: btclib::U256) -> Block {
        let private_key = PrivateKey::new_key();
        let coinbase = Transaction::new(
            vec![],
            vec![TransactionOutput {
                value: 1,
                unique_id: Uuid::new_v4(),
                pubkey: private_key.public_key(),
            }],
        );
        let merkle_root = MerkleRoot::calculate(&[coinbase.clone()]);
        let header = BlockHeader::new(Utc::now(), 0, Hash::zero(), merkle_root, target);
        Block::new(header, vec![coinbase])
    }

    /// An address guaranteed to have nothing listening on it -- for
    /// tests that need a deterministically-unreachable node rather than
    /// racing against a real process's timing.
    async fn dead_address() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        format!("127.0.0.1:{}", listener.local_addr().unwrap().port())
        // `listener` drops here, freeing the port right back up.
    }

    #[test]
    fn thread_start_nonce_partitions_into_per_thread_windows() {
        assert_eq!(thread_start_nonce(0, 1_000), 0);
        assert_eq!(thread_start_nonce(1, 1_000), 1_000);
        assert_eq!(thread_start_nonce(3, 1_000), 3_000);
    }

    #[tokio::test]
    async fn multiple_mining_threads_find_a_block_on_an_easy_target() {
        let template = dummy_block(btclib::MIN_TARGET);
        let shared_template = Arc::new(std::sync::Mutex::new(Some(template)));
        let mining = Arc::new(AtomicBool::new(true));
        let (sender, receiver) = flume::unbounded();

        let thread_count = 4;
        for thread_index in 0..thread_count {
            spawn_mining_thread(thread_index, shared_template.clone(), mining.clone(), sender.clone());
        }

        let mined = timeout(Duration::from_secs(30), receiver.recv_async())
            .await
            .expect("a block should be found well within the timeout on MIN_TARGET")
            .expect("channel should not have closed");

        assert!(mined.header.hash().matches_target(mined.header.target));

        // the winning nonce must fall inside SOME thread's assigned
        // window -- a sanity check that partitioning was actually
        // applied, without assuming which thread had to win
        let nonce = mined.header.nonce;
        let winning_thread = (0..thread_count).find(|&t| {
            let window_start = thread_start_nonce(t, STEPS_PER_CALL);
            nonce >= window_start && nonce <= window_start + STEPS_PER_CALL as u64
        });
        assert!(
            winning_thread.is_some(),
            "mined nonce {nonce} doesn't fall in any thread's assigned window"
        );
    }

    /// Every link the miner has, and whether it is connected right now.
    async fn connected_links(miner: &Miner) -> Vec<bool> {
        let mut states = Vec::new();
        for link in &miner.links {
            states.push(link.lock().await.stream.is_some());
        }
        states
    }

    #[tokio::test]
    async fn miner_new_waits_for_an_unreachable_primary_rather_than_exiting() {
        let dead = dead_address().await;
        let public_key = PrivateKey::new_key().public_key();

        // Starting the miner and the node together is the ordinary case
        // -- a reboot, an upgrade -- and exiting here turned "wait a
        // moment" into a restart loop.
        let miner = Miner::new(vec![dead], public_key).await.unwrap();
        assert_eq!(connected_links(&miner).await, vec![false], "the link is kept, unconnected");
        assert!(
            miner.fetch_template().await.is_err(),
            "and a template cannot be fetched over it yet"
        );
    }

    #[tokio::test]
    async fn miner_new_keeps_an_unreachable_secondary_for_later() {
        let primary = FakeMinerPeer::spawn_healthy().await;
        let dead_secondary = dead_address().await;
        let public_key = PrivateKey::new_key().public_key();

        let miner = Miner::new(vec![primary.addr.clone(), dead_secondary], public_key)
            .await
            .unwrap();

        // Kept, where the dead secondary used to be dropped from the
        // list outright: an address that is unreachable at startup is
        // the one most likely to come back.
        assert_eq!(connected_links(&miner).await, vec![true, false]);
    }

    /// T22: the miner used to hold one connection opened at startup, so
    /// any node restart ended `run()` and the process exited. It now
    /// reopens the link and carries on -- which is what a maintenance
    /// window on the node looks like from here.
    #[tokio::test]
    async fn the_miner_reconnects_after_the_node_goes_away() {
        let node = FakeMinerPeer::spawn_template_source().await;
        let public_key = PrivateKey::new_key().public_key();
        let miner = Miner::new(vec![node.addr.clone()], public_key).await.unwrap();

        miner.fetch_template().await.expect("a template over the first connection");
        assert!(miner.mining.load(Ordering::Relaxed));
        assert!(miner.current_template.lock().unwrap().is_some());

        // The node goes away mid-life, as it does when it restarts.
        node.close_current_connection().await;
        let err = miner.fetch_template().await.expect_err("the dead socket must fail");
        miner.stop_mining_on_a_stale_template();
        assert_eq!(connected_links(&miner).await, vec![false], "and the link is dropped: {err}");
        assert!(!miner.mining.load(Ordering::Relaxed), "nothing is mined against a stale tip");
        assert!(
            miner.current_template.lock().unwrap().is_none(),
            "the template goes with it -- a block built on the old tip is a strike"
        );

        // And the next attempt opens a new connection to the same
        // address, with no restart of anything.
        miner.fetch_template().await.expect("a template over a reopened connection");
        assert_eq!(connected_links(&miner).await, vec![true]);
        assert!(miner.mining.load(Ordering::Relaxed));
    }

    /// A minimal fake node/miner peer for exercising `Miner`'s connection
    /// and submission logic without a real `node` process. Mirrors
    /// `hub/src/main.rs`'s `FakeNode` pattern (real TCP, real handshake).
    struct FakeMinerPeer {
        addr: String,
        received: Arc<Mutex<Vec<Block>>>,
        /// Present only on `spawn_template_source`: sending on it makes
        /// the fake drop whichever connection it is serving.
        close: Option<flume::Sender<()>>,
    }

    impl FakeMinerPeer {
        /// Completes the handshake, then records every `SubmitTemplate`
        /// block it receives.
        async fn spawn_healthy() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
            let received = Arc::new(Mutex::new(Vec::new()));
            let received_for_task = received.clone();
            tokio::spawn(async move {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                if btclib::network::perform_handshake_acceptor(&mut socket)
                    .await
                    .is_err()
                {
                    return;
                }
                loop {
                    match Message::receive_async(&mut socket).await {
                        Ok(Message::SubmitTemplate(block)) => {
                            received_for_task.lock().await.push(block);
                        }
                        Ok(_) | Err(_) => return,
                    }
                }
            });
            FakeMinerPeer { addr, received, close: None }
        }

        /// `Message::send_async` doesn't wait for the peer to actually
        /// process what it sent (same fire-and-forget shape as
        /// `SubmitTransaction` elsewhere in this workspace) -- a caller
        /// observing `submit_block` return only knows the bytes reached
        /// the OS socket, not that this fake's own accept/receive task
        /// has gotten around to recording them yet. Polls briefly instead
        /// of asserting on `received` immediately.
        async fn wait_for_received_count(&self, expected: usize) -> Vec<Block> {
            for _ in 0..100 {
                let received = self.received.lock().await.clone();
                if received.len() >= expected {
                    return received;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            self.received.lock().await.clone()
        }

        /// Answers `FetchTemplate` with a template, over as many
        /// successive connections as the miner opens -- the shape of a
        /// real node across a restart. `close_current_connection` is how
        /// the test takes the current one away.
        async fn spawn_template_source() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
            let received = Arc::new(Mutex::new(Vec::new()));
            let (close_tx, close_rx) = flume::unbounded::<()>();
            tokio::spawn(async move {
                // The accept loop is the point: one connection at a time,
                // but a new one whenever the miner dials again.
                while let Ok((mut socket, _)) = listener.accept().await {
                    if btclib::network::perform_handshake_acceptor(&mut socket).await.is_err() {
                        continue;
                    }
                    loop {
                        tokio::select! {
                            // Drop this socket and go back to accepting.
                            Ok(()) = close_rx.recv_async() => break,
                            received = Message::receive_async(&mut socket) => {
                                match received {
                                    Ok(Message::FetchTemplate(pubkey)) => {
                                        let template = template_for(&pubkey);
                                        if Message::Template(template).send_async(&mut socket).await.is_err() {
                                            break;
                                        }
                                    }
                                    Ok(_) => {}
                                    Err(_) => break,
                                }
                            }
                        }
                    }
                }
            });
            FakeMinerPeer { addr, received, close: Some(close_tx) }
        }

        /// Takes the connection the miner is currently using away from
        /// it, the way a node restart does.
        async fn close_current_connection(&self) {
            self.close.as_ref().expect("this fake was not spawned with a close channel").send(()).unwrap();
            // The miner does not learn the socket is gone until it next
            // writes to it, so nothing is asserted here -- this only has
            // to have happened before the next `fetch_template`.
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        /// Completes the handshake, then immediately drops the
        /// connection -- simulating a peer that was reachable when
        /// `Miner::new` connected but is gone by the time a block is
        /// actually submitted.
        async fn spawn_drops_after_handshake() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
            tokio::spawn(async move {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let _ = btclib::network::perform_handshake_acceptor(&mut socket).await;
                // `socket` drops here, closing the connection.
            });
            FakeMinerPeer {
                addr,
                received: Arc::new(Mutex::new(Vec::new())),
                close: None,
            }
        }
    }

    #[tokio::test]
    async fn submit_block_reaches_healthy_nodes_despite_one_dropping_out() {
        let healthy1 = FakeMinerPeer::spawn_healthy().await;
        let flaky = FakeMinerPeer::spawn_drops_after_handshake().await;
        let healthy2 = FakeMinerPeer::spawn_healthy().await;

        let public_key = PrivateKey::new_key().public_key();
        let miner = Miner::new(
            vec![healthy1.addr.clone(), flaky.addr.clone(), healthy2.addr.clone()],
            public_key,
        )
        .await
        .unwrap();
        assert_eq!(connected_links(&miner).await, vec![true, true, true], "all three connected at startup");

        // give the flaky fake a moment to actually close its side after
        // the handshake before we submit
        tokio::time::sleep(Duration::from_millis(100)).await;

        let block = dummy_block(btclib::MIN_TARGET);
        miner.submit_block(block.clone()).await; // must not hang or panic despite the dropped middle connection

        let r1 = healthy1.wait_for_received_count(1).await;
        let r2 = healthy2.wait_for_received_count(1).await;
        assert_eq!(r1.len(), 1);
        assert_eq!(r2.len(), 1);
        assert_eq!(r1[0].hash(), block.hash());
        assert_eq!(r2[0].hash(), block.hash());
    }
}


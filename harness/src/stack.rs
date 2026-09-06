//! Starting, killing and restarting a local node/miner/hub, because a
//! chaos drill is not a chaos drill if it cannot kill things.
//!
//! The load half of the harness runs against whatever stack you point it
//! at. The drill half needs process control, so it brings up its own on
//! its own ports with its own data directory, and tears it down after.
//!
//! # Two things this is careful about, both learned by other sessions
//!
//! **Ports.** Node 9040, hub 9140, and the hub binds `127.0.0.1`
//! explicitly. Three other workstreams may be running stacks on this
//! machine; a more specific bind silently shadows a wildcard one, so a
//! shared port does not produce an error, it produces a measurement of
//! somebody else's hub.
//!
//! **Binaries.** `target/release/hub` is one path for every worktree on
//! this box, and three of the four concurrent workstreams are actively
//! changing hub source. So the drills never launch out of `target/`
//! directly: `snapshot` copies the binaries into the run's own directory
//! first, and the report records the SHA256 of each. What gets measured is
//! then a fixed set of bytes, whatever anyone else rebuilds mid-run.

use crate::chain::ChainView;
use crate::client::HubClient;
use anyhow::{anyhow, Context, Result};
use btclib::crypto::PrivateKey;
use btclib::util::Saveable;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::{Child, Command};

/// The ports this workstream owns. Named rather than defaulted at each
/// call site so that changing them is one edit and so the numbers appear
/// in exactly one place in the source.
pub const NODE_PORT: u16 = 9040;
pub const HUB_PORT: u16 = 9140;

/// How long to wait for a process to answer before giving up on it.
/// Generous: a debug-build hub replaying a store on a loaded machine is
/// slow, and a flaky timeout here reads as a drill failure.
const READY_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct StackConfig {
    /// Where the run's own copies of `node`, `miner` and `hub` live.
    pub bin_dir: PathBuf,
    /// Chain and hub stores, keys, logs. Deleted between runs; the chain
    /// file grows for as long as the miner runs, and disk on this machine
    /// is the binding constraint.
    pub work_dir: PathBuf,
    pub node_port: u16,
    pub hub_port: u16,
    /// Passed to the hub as `--trusted-proxies`. Setting this to
    /// `127.0.0.1` makes the hub believe an `X-Forwarded-For` header from
    /// the harness, which is the only way to look like a thousand source
    /// addresses from one box. Test-only, and off unless a drill asks.
    pub trusted_proxies: Option<String>,
}

impl StackConfig {
    pub fn new(bin_dir: impl Into<PathBuf>, work_dir: impl Into<PathBuf>) -> Self {
        Self {
            bin_dir: bin_dir.into(),
            work_dir: work_dir.into(),
            node_port: NODE_PORT,
            hub_port: HUB_PORT,
            trusted_proxies: None,
        }
    }

    pub fn trusting_local_proxy(mut self) -> Self {
        self.trusted_proxies = Some("127.0.0.1".to_string());
        self
    }
}

/// A running stack. Every child is `kill_on_drop`, so an aborted drill
/// does not leave a node mining into a chain file nobody will look at.
pub struct Stack {
    config: StackConfig,
    node: Option<Child>,
    miner: Option<Child>,
    hub: Option<Child>,
    /// The operator's key, generated here rather than by the hub on first
    /// run. The hub would happily make its own, but the miner has to be
    /// pointed at the matching public key before the hub ever starts, and
    /// the payout-ceiling drill needs to spend from it directly.
    pub operator_key: PrivateKey,
}

impl Stack {
    /// Copies `node`, `miner` and `hub` out of a build directory into
    /// `bin_dir`, so that what the drills run is pinned for the length of
    /// the run. Returns the paths, in the order the report should list
    /// them.
    pub fn snapshot(build_dir: &Path, bin_dir: &Path) -> Result<Vec<PathBuf>> {
        std::fs::create_dir_all(bin_dir)
            .with_context(|| format!("creating {}", bin_dir.display()))?;
        let mut copied = Vec::new();
        for name in ["node", "miner", "hub"] {
            let from = build_dir.join(name);
            let to = bin_dir.join(name);
            std::fs::copy(&from, &to).with_context(|| {
                format!(
                    "copying {} to {} -- is the build there? (cargo build --release -p {name})",
                    from.display(),
                    to.display()
                )
            })?;
            copied.push(to);
        }
        Ok(copied)
    }

    /// Prepares a work directory and an operator key, but starts nothing.
    pub fn prepare(config: StackConfig) -> Result<Self> {
        std::fs::create_dir_all(config.work_dir.join("logs"))
            .with_context(|| format!("creating {}", config.work_dir.display()))?;

        let operator_key = PrivateKey::new_key();
        operator_key
            .save_to_file(config.work_dir.join("operator.priv.cbor"))
            .map_err(|e| anyhow!("saving the operator private key: {e}"))?;
        operator_key
            .public_key()
            .save_to_file(config.work_dir.join("operator.pub.pem"))
            .map_err(|e| anyhow!("saving the operator public key: {e}"))?;

        Ok(Self {
            config,
            node: None,
            miner: None,
            hub: None,
            operator_key,
        })
    }

    pub fn node_address(&self) -> String {
        format!("127.0.0.1:{}", self.config.node_port)
    }

    pub fn hub_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.config.hub_port)
    }

    pub fn chain(&self) -> ChainView {
        ChainView::new(&self.node_address())
    }

    pub fn hub_client(&self) -> Result<HubClient> {
        HubClient::new(&self.hub_url())
    }

    pub fn work_dir(&self) -> &Path {
        &self.config.work_dir
    }

    /// Appends this process's output to a per-role log. Appends rather
    /// than truncates so that a restart in the middle of a drill does not
    /// erase why the previous instance died -- which is usually the thing
    /// worth reading.
    fn log(&self, role: &str) -> Result<std::fs::File> {
        let path = self.config.work_dir.join("logs").join(format!("{role}.log"));
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))
    }

    fn spawn(&self, role: &str, args: &[String]) -> Result<Child> {
        let out = self.log(role)?;
        let err = out.try_clone()?;
        Command::new(self.config.bin_dir.join(role))
            .args(args)
            .current_dir(&self.config.work_dir)
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err))
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawning {role}"))
    }

    pub async fn start_node(&mut self) -> Result<()> {
        if self.node.is_some() {
            return Ok(());
        }
        self.node = Some(self.spawn(
            "node",
            &[
                "--port".into(),
                self.config.node_port.to_string(),
                "--blockchain-file".into(),
                "./chain.redb".into(),
            ],
        )?);

        let chain = self.chain();
        wait_for("the node", || {
            let chain = chain.clone();
            async move { chain.is_ready().await }
        })
        .await
    }

    pub async fn start_miner(&mut self) -> Result<()> {
        self.start_miner_paying("./operator.pub.pem").await
    }

    /// Starts the miner paying coinbase to `pubkey_file`, relative to the
    /// work directory.
    ///
    /// Who the miner pays is not a detail. Mining to the operator hands it
    /// a fresh confirmed output every block, and a wallet with many
    /// outputs is precisely the shape in which the payout ceiling of
    /// §6.4b is invisible. The drill that measures that ceiling has to
    /// move the miner off the operator first, or it will measure a
    /// condition that does not hold in the deployment it is modelling.
    pub async fn start_miner_paying(&mut self, pubkey_file: &str) -> Result<()> {
        if self.miner.is_some() {
            return Ok(());
        }
        self.miner = Some(self.spawn(
            "miner",
            &[
                "--addresses".into(),
                self.node_address(),
                "--public-key-file".into(),
                pubkey_file.into(),
            ],
        )?);
        Ok(())
    }

    /// Writes a fresh keypair into the work directory under `name`,
    /// returning the private half. Used where a drill needs a key the hub
    /// does not know about -- a miner that is not the operator, say.
    pub fn mint_key(&self, name: &str) -> Result<PrivateKey> {
        let key = PrivateKey::new_key();
        key.save_to_file(self.config.work_dir.join(format!("{name}.priv.cbor")))
            .map_err(|e| anyhow!("saving {name}'s private key: {e}"))?;
        key.public_key()
            .save_to_file(self.config.work_dir.join(format!("{name}.pub.pem")))
            .map_err(|e| anyhow!("saving {name}'s public key: {e}"))?;
        Ok(key)
    }

    pub async fn start_hub(&mut self) -> Result<()> {
        if self.hub.is_some() {
            return Ok(());
        }
        let mut args = vec![
            "--port".into(),
            self.config.hub_port.to_string(),
            // Explicit loopback, not the `0.0.0.0` default: see the module
            // note on shadowed binds.
            "--bind".into(),
            "127.0.0.1".into(),
            "--node-addresses".into(),
            self.node_address(),
            "--store-file".into(),
            "./hub.redb".into(),
            "--operator-key-file".into(),
            "./operator.priv.cbor".into(),
        ];
        if let Some(proxies) = &self.config.trusted_proxies {
            args.push("--trusted-proxies".into());
            args.push(proxies.clone());
        }
        self.hub = Some(self.spawn("hub", &args)?);

        let client = self.hub_client()?;
        wait_for("the hub", || {
            let client = client.clone();
            async move { client.get("/health").await.map(|r| r.ok()).unwrap_or(false) }
        })
        .await
    }

    /// Node, then miner, then hub, each waited for.
    pub async fn start_all(&mut self) -> Result<()> {
        self.start_node().await?;
        self.start_miner().await?;
        self.start_hub().await
    }

    /// SIGKILL. What a crash looks like: no drain, no flush, and for the
    /// node specifically, a mempool that simply ceases to exist.
    pub async fn kill_node(&mut self) -> Result<()> {
        kill_now(&mut self.node).await
    }

    pub async fn kill_hub(&mut self) -> Result<()> {
        kill_now(&mut self.hub).await
    }

    pub async fn kill_miner(&mut self) -> Result<()> {
        kill_now(&mut self.miner).await
    }

    /// SIGTERM, then wait. What `systemctl restart` sends, and what the
    /// hub drains in-flight requests on -- a materially different event
    /// from `kill_hub`, and the drills test both.
    pub async fn stop_hub(&mut self) -> Result<()> {
        terminate(&mut self.hub).await
    }

    pub async fn stop_node(&mut self) -> Result<()> {
        terminate(&mut self.node).await
    }

    /// Kills the miner alongside the node, because a miner whose template
    /// source has gone away is not a miner any more -- it holds a dead
    /// socket and mines nothing. Restarting the node without this
    /// produces a chain that has silently stopped advancing, which reads
    /// as a much more interesting bug than it is.
    pub async fn kill_node_and_miner(&mut self) -> Result<()> {
        self.kill_miner().await?;
        self.kill_node().await
    }

    pub async fn shutdown(&mut self) {
        let _ = self.kill_miner().await;
        let _ = self.stop_hub().await;
        let _ = self.stop_node().await;
    }
}

async fn kill_now(child: &mut Option<Child>) -> Result<()> {
    if let Some(mut process) = child.take() {
        process.kill().await.context("SIGKILL")?;
    }
    Ok(())
}

async fn terminate(child: &mut Option<Child>) -> Result<()> {
    let Some(mut process) = child.take() else {
        return Ok(());
    };
    if let Some(pid) = process.id() {
        // SAFETY: `kill(2)` with a pid this process owns and a valid
        // signal number. The child is still in `process`, so the pid has
        // not been reaped and cannot have been reused.
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }
    // A process that ignores SIGTERM must not hang the drill; fall back
    // to SIGKILL rather than waiting forever.
    match tokio::time::timeout(Duration::from_secs(20), process.wait()).await {
        Ok(_) => Ok(()),
        Err(_) => process.kill().await.context("SIGKILL after SIGTERM timed out"),
    }
}

async fn wait_for<F, Fut>(what: &str, mut ready: F) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        if ready().await {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "{what} did not become ready within {}s -- check the log in the run's logs/ directory",
                READY_TIMEOUT.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
}

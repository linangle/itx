//! Talking to the node directly, over its own CBOR protocol.
//!
//! The drills that matter are the ones that ask "did the money actually
//! land?", and the hub is exactly the wrong thing to ask -- its whole
//! known failure mode is believing a payout happened that did not (plan
//! §6.5). So the harness holds an independent view of the chain and
//! compares the two. That is the measurement.
//!
//! # One rule about connecting
//!
//! Never open a TCP connection to the node without completing the
//! handshake. An opened-but-unhandshaken connection is a *severe* strike,
//! and one severe strike bans the source IP for an hour -- which on a
//! single box takes the miner and the hub down with the harness, since
//! all three share `127.0.0.1`. Probing the port to see whether the node
//! is up is therefore the one thing this module must never offer, and
//! `is_ready` below is a full handshake for that reason.

use anyhow::{anyhow, Context, Result};
use btclib::crypto::{PrivateKey, PublicKey};
use btclib::network::Message;
use btclib::payment::build_payment;
use btclib::sha256::Hash;
use btclib::types::{Transaction, TransactionOutput};
use std::time::Duration;
use tokio::net::TcpStream;

/// A view of one node. Holds no connection: every call dials, handshakes,
/// exchanges once and hangs up.
///
/// That is deliberately the *un*-pooled shape the hub moved away from
/// (§6.2). The harness is not trying to be fast at this; it is trying to
/// be independent, and a pool that failed over or reused a socket across a
/// deliberate node kill would give the drills a confusing view of a node
/// the drill itself just killed.
#[derive(Clone)]
pub struct ChainView {
    address: String,
}

impl ChainView {
    pub fn new(address: &str) -> Self {
        Self {
            address: address.to_string(),
        }
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    async fn exchange(&self, request: &Message) -> Result<Message> {
        let mut stream = TcpStream::connect(&self.address)
            .await
            .with_context(|| format!("connecting to node at {}", self.address))?;
        btclib::network::perform_handshake_initiator(&mut stream)
            .await
            .map_err(|e| anyhow!("handshake with {} failed: {e}", self.address))?;
        request
            .send_async(&mut stream)
            .await
            .context("sending to the node")?;
        Message::receive_async(&mut stream)
            .await
            .context("reading the node's reply")
    }

    /// A complete, legitimate exchange used as a readiness check -- see
    /// the module note on why this is not a port probe.
    pub async fn is_ready(&self) -> bool {
        self.tip().await.is_ok()
    }

    /// Block height and cumulative work at the node's tip.
    pub async fn tip(&self) -> Result<(u32, String)> {
        match self.exchange(&Message::AskChainTip).await? {
            Message::ChainTip(height, work) => Ok((height, work.to_string())),
            other => Err(anyhow!("expected ChainTip, got {other:?}")),
        }
    }

    pub async fn height(&self) -> Result<u32> {
        Ok(self.tip().await?.0)
    }

    /// Every output paying `pubkey`, each with the node's flag for
    /// whether the mempool has already spoken for it.
    pub async fn utxos(&self, pubkey: &PublicKey) -> Result<Vec<(TransactionOutput, bool)>> {
        match self.exchange(&Message::FetchUTXOs(pubkey.clone())).await? {
            Message::UTXOs(utxos) => Ok(utxos),
            other => Err(anyhow!("expected UTXOs, got {other:?}")),
        }
    }

    /// What `pubkey` holds that is mined and unspoken-for. This is the
    /// number the hub's own escrow confirmation uses, and the only one
    /// that means the money is really there.
    pub async fn confirmed_balance(&self, pubkey: &PublicKey) -> Result<u64> {
        Ok(self
            .utxos(pubkey)
            .await?
            .into_iter()
            .filter(|(_, marked)| !marked)
            .map(|(output, _)| output.value)
            .sum())
    }

    /// Everything paying `pubkey`, marked or not. The gap between this
    /// and `confirmed_balance` is money in flight, which is exactly what a
    /// node kill destroys.
    pub async fn total_balance(&self, pubkey: &PublicKey) -> Result<u64> {
        Ok(self
            .utxos(pubkey)
            .await?
            .into_iter()
            .map(|(output, _)| output.value)
            .sum())
    }

    /// Whether an output with this hash exists at all -- the primary
    /// signal §6.5's design is built on, and the one the harness uses to
    /// decide whether a payout the hub called `Paid` is real.
    pub async fn holds_output(&self, pubkey: &PublicKey, hash: &Hash) -> Result<bool> {
        Ok(self
            .utxos(pubkey)
            .await?
            .iter()
            .any(|(output, _)| output.hash() == *hash))
    }

    /// Submits a transaction. Fire-and-forget, because the protocol has
    /// no acknowledgement to wait for -- the same one-way write the hub
    /// makes, and the reason §6.5 exists.
    pub async fn submit(&self, transaction: Transaction) -> Result<()> {
        let mut stream = TcpStream::connect(&self.address)
            .await
            .with_context(|| format!("connecting to node at {}", self.address))?;
        btclib::network::perform_handshake_initiator(&mut stream)
            .await
            .map_err(|e| anyhow!("handshake with {} failed: {e}", self.address))?;
        Message::SubmitTransaction(transaction)
            .send_async(&mut stream)
            .await
            .context("submitting a transaction")?;
        Ok(())
    }

    /// Pays `amount` from `from`'s confirmed coin to `to`, returning the
    /// transaction so the caller can watch for its outputs.
    pub async fn pay(
        &self,
        from: &PrivateKey,
        to: &PublicKey,
        amount: u64,
        fee: u64,
    ) -> Result<Transaction> {
        let utxos = self.utxos(&from.public_key()).await?;
        let available: Vec<(bool, TransactionOutput)> = utxos
            .into_iter()
            .map(|(output, marked)| (marked, output))
            .collect();
        let transaction = build_payment(
            &available,
            from,
            to.clone(),
            amount,
            fee,
            from.public_key(),
        )?;
        self.submit(transaction.clone()).await?;
        Ok(transaction)
    }

    /// Waits for `pubkey`'s confirmed balance to reach `target`, giving up
    /// after `timeout`. Returns whatever the balance was when it stopped
    /// waiting, so a caller can report the shortfall rather than only
    /// that it timed out.
    pub async fn wait_for_balance(
        &self,
        pubkey: &PublicKey,
        target: u64,
        timeout: Duration,
    ) -> Result<u64> {
        let deadline = std::time::Instant::now() + timeout;
        let mut last = 0;
        loop {
            last = self.confirmed_balance(pubkey).await.unwrap_or(last);
            if last >= target || std::time::Instant::now() >= deadline {
                return Ok(last);
            }
            tokio::time::sleep(Duration::from_millis(750)).await;
        }
    }

    /// Waits until the chain has advanced by `blocks` from wherever it is
    /// now, returning the height reached. Used wherever a drill needs
    /// "one block later" as its unit rather than a wall-clock guess.
    pub async fn wait_for_blocks(&self, blocks: u32, timeout: Duration) -> Result<u32> {
        let start = self.height().await?;
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let now = self.height().await.unwrap_or(start);
            if now >= start + blocks || std::time::Instant::now() >= deadline {
                return Ok(now);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}

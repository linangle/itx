use tracing::*;

use super::block::calculate_miner_fees_for_transactions;
use super::{Block, BlockHeader, Transaction, TransactionOutput};
use crate::crypto::PublicKey;
use crate::error::{BtcError, Result};
use crate::sha256::Hash;
use crate::util::MerkleRoot;
use crate::U256;
use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use uuid::Uuid;

/// Maximum number of blocks with an unknown parent that we'll buffer at
/// once, so a peer can't exhaust our memory by sending endless orphans.
const MAX_ORPHAN_BLOCKS: usize = 50;

#[derive(Clone, Debug)]
pub struct Blockchain {
    utxos: HashMap<Hash, (bool, TransactionOutput)>,
    target: U256,
    // Every block we know about -- active chain and side branches alike --
    // keyed by its own hash.
    blocks: HashMap<Hash, Block>,
    // The canonical chain: block hashes in order from genesis (index 0)
    // to the current tip (last). This is the chain everything else
    // (utxos, target, mempool) is derived from.
    active_chain: Vec<Hash>,
    mempool: Vec<(DateTime<Utc>, Transaction)>,
    // Blocks whose parent we've never seen, buffered in case that parent
    // shows up later (e.g. blocks arriving out of order over the network).
    orphans: HashMap<Hash, Block>,
    // Hashes pruned from `blocks` since the last `take_pruned_hashes`
    // call, so a caller with durable storage knows what to delete there
    // too. Not persisted itself -- purely an in-process handoff.
    pending_prunes: Vec<Hash>,
    // How far ahead (or behind) the network's median clock our own clock
    // seems to be, as last computed by the node layer from a set of peer
    // handshake samples (see node::time_sync). Zero -- i.e. "trust our own
    // clock outright" -- until enough peers have been sampled to trust an
    // adjustment at all. Not persisted: recomputed fresh from current
    // peers each time the node layer updates it.
    time_offset: chrono::Duration,
}

impl Blockchain {
    pub fn new() -> Self {
        Blockchain {
            utxos: HashMap::new(),
            blocks: HashMap::new(),
            active_chain: vec![],
            target: crate::MIN_TARGET,
            mempool: vec![],
            orphans: HashMap::new(),
            pending_prunes: vec![],
            time_offset: chrono::Duration::zero(),
        }
    }

    /// Updates the network-adjusted clock offset used by `check_timestamp`.
    /// Called by the node layer whenever it recomputes the median offset
    /// across its currently-sampled peers.
    pub fn set_time_offset(&mut self, offset: chrono::Duration) {
        self.time_offset = offset;
    }

    pub fn mempool(&self) -> &[(DateTime<Utc>, Transaction)] {
        &self.mempool
    }

    /// The one true genesis block for this network, deterministically
    /// derived from fixed inputs so every node computes the identical
    /// block independently instead of learning it from whichever peer
    /// happens to answer first during initial sync.
    pub fn genesis_block() -> &'static Block {
        static GENESIS: OnceLock<Block> = OnceLock::new();
        GENESIS.get_or_init(|| {
            // SHA256 of a fixed, public seed string, used as a secp256k1
            // scalar. Because the seed is public, anyone can rederive this
            // private key -- the genesis reward is a transparent, public
            // "genesis fund" rather than a hidden pre-mine, and is a
            // natural seed for a future faucet.
            let seed = Hash::hash(&"itx testnet genesis coinbase v1");
            let private_key = crate::crypto::PrivateKey::from_fixed_bytes(&seed.as_bytes())
                .expect("BUG: fixed genesis seed must produce a valid key");

            let coinbase = Transaction::new(
                vec![],
                vec![TransactionOutput {
                    value: crate::INITIAL_REWARD * 10u64.pow(8),
                    unique_id: Uuid::nil(),
                    pubkey: private_key.public_key(),
                }],
            );
            let merkle_root = MerkleRoot::calculate(&[coinbase.clone()]);
            let mut header = BlockHeader::new(
                DateTime::<Utc>::UNIX_EPOCH,
                0,
                Hash::zero(),
                merkle_root,
                crate::MIN_TARGET,
            );
            assert!(
                header.mine(2_000_000),
                "BUG: could not mine the fixed genesis header"
            );
            Block::new(header, vec![coinbase])
        })
    }

    pub fn genesis_hash() -> Hash {
        Self::genesis_block().hash()
    }

    pub fn calculate_block_reward(&self) -> u64 {
        crate::block_reward_at_height(self.block_height())
    }

    pub fn create_block_template(&self, pubkey: PublicKey) -> Result<Block> {
        // the mempool is already kept sorted highest-fee-first (see
        // add_to_mempool), so greedily filling a byte budget in that order
        // maximizes fees collected per byte of block space spent.
        let mut transactions: Vec<Transaction> = Vec::new();
        let mut total_bytes = 0usize;
        for (_, tx) in self.mempool.iter() {
            let size = tx.serialized_size();
            if total_bytes + size > crate::BLOCK_BYTE_CAP {
                continue;
            }
            total_bytes += size;
            transactions.push(tx.clone());
        }

        let miner_fees = calculate_miner_fees_for_transactions(&transactions, &self.utxos)?;
        let reward = self.calculate_block_reward();

        transactions.insert(
            0,
            Transaction {
                inputs: vec![],
                outputs: vec![TransactionOutput {
                    value: reward + miner_fees,
                    unique_id: Uuid::new_v4(),
                    pubkey,
                }],
            },
        );

        let merkle_root = MerkleRoot::calculate(&transactions);
        let prev_block_hash = self.active_chain.last().copied().unwrap_or(Hash::zero());

        Ok(Block::new(
            BlockHeader {
                timestamp: Utc::now(),
                prev_block_hash,
                nonce: 0,
                target: self.target,
                merkle_root,
            },
            transactions,
        ))
    }

    pub fn cleanup_mempool(&mut self) {
        let now = Utc::now();
        let mut utxo_hashes_to_unmark: Vec<Hash> = vec![];
        self.mempool.retain(|(timestamp, transaction)| {
            if (now - *timestamp)
                > chrono::Duration::seconds(crate::MAX_MEMPOOL_TRANSACTION_AGE as i64)
            {
                utxo_hashes_to_unmark.extend(
                    transaction
                        .inputs
                        .iter()
                        .map(|input| input.prev_transaction_output_hash),
                );
                false
            } else {
                true
            }
        });
        for hash in utxo_hashes_to_unmark {
            self.utxos
                .entry(hash)
                .and_modify(|(marked, _)| *marked = false);
        }
    }

    pub fn add_to_mempool(&mut self, transaction: Transaction) -> Result<()> {
        self.cleanup_mempool();

        let mut known_inputs = HashSet::new();

        for input in &transaction.inputs {
            let Some((_, prev_output)) = self.utxos.get(&input.prev_transaction_output_hash) else {
                return Err(BtcError::InvalidTransaction);
            };
            if known_inputs.contains(&input.prev_transaction_output_hash) {
                return Err(BtcError::InvalidTransaction);
            }
            // The chain's only signature check lived in block
            // verification, so the mempool took anyone's word for who
            // owned an input. A well-formed spend of somebody else's
            // output under a garbage signature was admitted, marked that
            // output spent on the way in -- which is what every balance
            // read and the hub's payment builder go by -- sat in every
            // template until the block carrying it was rejected, and
            // struck the miner that mined it. Verify here, the way
            // `Block::verify_transactions` does, before anything is
            // marked.
            if !input.verifies(&transaction.outputs, &prev_output.pubkey) {
                return Err(BtcError::InvalidSignature);
            }
            known_inputs.insert(input.prev_transaction_output_hash);
        }

        let new_fee = self.transaction_fee(&transaction)?;

        let mut conflicting_indices = HashSet::new();
        for input in &transaction.inputs {
            if let Some((true, _)) = self.utxos.get(&input.prev_transaction_output_hash) {
                if let Some(idx) =
                    self.find_mempool_index_spending(input.prev_transaction_output_hash)
                {
                    let old_fee = self.transaction_fee(&self.mempool[idx].1)?;
                    if new_fee <= old_fee {
                        return Err(BtcError::InvalidTransaction);
                    }
                    conflicting_indices.insert(idx);
                } else {
                    self.utxos
                        .entry(input.prev_transaction_output_hash)
                        .and_modify(|(marked, _)| *marked = false);
                }
            }
        }

        let mut to_remove: Vec<usize> = conflicting_indices.into_iter().collect();
        to_remove.sort_unstable_by(|a, b| b.cmp(a));
        for idx in to_remove {
            let (_, removed) = self.mempool.remove(idx);
            self.unmark_transaction_utxos(&removed);
        }

        // Checked for the same reason `Block::verify_transactions` is:
        // a wrapped output sum passes this comparison. `transaction_fee`
        // above already refused a wrap, so this cannot fail on the same
        // transaction; it stays checked so the two never drift apart.
        let all_inputs = transaction
            .inputs
            .iter()
            .try_fold(0u64, |acc, input| {
                acc.checked_add(
                    self.utxos
                        .get(&input.prev_transaction_output_hash)
                        .expect("BUG: impossible")
                        .1
                        .value,
                )
            })
            .ok_or(BtcError::InvalidTransaction)?;
        let all_outputs = transaction
            .outputs
            .iter()
            .try_fold(0u64, |acc, output| acc.checked_add(output.value))
            .ok_or(BtcError::InvalidTransaction)?;
        if all_inputs < all_outputs {
            return Err(BtcError::InvalidTransaction);
        }

        self.mark_transaction_utxos(&transaction);
        self.mempool.push((Utc::now(), transaction));

        let mut order: Vec<usize> = (0..self.mempool.len()).collect();
        order.sort_by(|&i, &j| {
            self.transaction_fee(&self.mempool[j].1)
                .unwrap_or(0)
                .cmp(&self.transaction_fee(&self.mempool[i].1).unwrap_or(0))
        });
        let sorted = order.into_iter().map(|i| self.mempool[i].clone()).collect();
        self.mempool = sorted;

        Ok(())
    }

    fn transaction_fee(&self, transaction: &Transaction) -> Result<u64> {
        // Checked sums. This runs before the conservation check in
        // `add_to_mempool`, so it is the first thing a wrapped output
        // total meets, and it has to refuse rather than report a fee
        // that would sort the transaction to the head of the mempool.
        let input_value = transaction
            .inputs
            .iter()
            .try_fold(0u64, |acc, input| {
                acc.checked_add(
                    self.utxos
                        .get(&input.prev_transaction_output_hash)
                        .expect("BUG: impossible")
                        .1
                        .value,
                )
            })
            .ok_or(BtcError::InvalidTransaction)?;
        let output_value = transaction
            .outputs
            .iter()
            .try_fold(0u64, |acc, output| acc.checked_add(output.value))
            .ok_or(BtcError::InvalidTransaction)?;
        if input_value < output_value {
            return Err(BtcError::InvalidTransaction);
        }
        Ok(input_value - output_value)
    }

    fn find_mempool_index_spending(&self, utxo_hash: Hash) -> Option<usize> {
        self.mempool
            .iter()
            .enumerate()
            .find_map(|(idx, (_, transaction))| {
                transaction
                    .inputs
                    .iter()
                    .any(|input| input.prev_transaction_output_hash == utxo_hash)
                    .then_some(idx)
            })
    }

    fn mark_transaction_utxos(&mut self, transaction: &Transaction) {
        for input in &transaction.inputs {
            if let Some((marked, _)) = self.utxos.get_mut(&input.prev_transaction_output_hash) {
                *marked = true;
            }
        }
    }

    fn unmark_transaction_utxos(&mut self, transaction: &Transaction) {
        for input in &transaction.inputs {
            if let Some((marked, _)) = self.utxos.get_mut(&input.prev_transaction_output_hash) {
                *marked = false;
            }
        }
    }

    pub fn block_height(&self) -> u64 {
        self.active_chain.len() as u64
    }

    /// Cumulative proof-of-work behind the active chain. This is the
    /// correct basis for choosing between two competing chains -- a chain
    /// with more blocks is not necessarily the one with more work behind
    /// it, since blocks can be mined at different difficulties.
    pub fn chain_work(&self) -> U256 {
        self.work_of_chain(&self.active_chain)
    }

    pub fn utxos(&self) -> &HashMap<Hash, (bool, TransactionOutput)> {
        &self.utxos
    }

    pub fn target(&self) -> U256 {
        self.target
    }

    /// The active chain's block hashes, genesis first, tip last. Useful
    /// for a caller that wants to persist the canonical chain to disk.
    pub fn active_chain(&self) -> &[Hash] {
        &self.active_chain
    }

    pub fn blocks(&self) -> impl Iterator<Item = &Block> {
        self.active_chain.iter().map(move |hash| {
            self.blocks
                .get(hash)
                .expect("BUG: active chain references a block missing from the block map")
        })
    }

    pub fn get_block(&self, hash: &Hash) -> Option<&Block> {
        self.blocks.get(hash)
    }

    fn work_of_chain(&self, chain: &[Hash]) -> U256 {
        chain.iter().fold(U256::zero(), |acc, hash| {
            acc.saturating_add(
                self.blocks
                    .get(hash)
                    .expect("BUG: chain references unknown block")
                    .header
                    .work(),
            )
        })
    }

    /// Replays a chain of blocks (genesis-first) from scratch and returns
    /// the UTXO set that results. Used to validate/apply side branches,
    /// whose UTXO state can't be assumed to match `self.utxos` (which only
    /// ever reflects the active chain).
    fn replay_utxos(&self, chain: &[Hash]) -> HashMap<Hash, (bool, TransactionOutput)> {
        let mut utxos: HashMap<Hash, (bool, TransactionOutput)> = HashMap::new();
        for hash in chain {
            let block = self
                .blocks
                .get(hash)
                .expect("BUG: chain references unknown block");
            for transaction in &block.transactions {
                for input in &transaction.inputs {
                    utxos.remove(&input.prev_transaction_output_hash);
                }
                for output in &transaction.outputs {
                    utxos.insert(output.hash(), (false, output.clone()));
                }
            }
        }
        utxos
    }

    /// Recomputes the PoW target that should apply to the block *after*
    /// the end of `chain`, by replaying the difficulty adjustment rule
    /// from genesis. This is more expensive than tracking a running
    /// `self.target` scalar, but it's the only way to get a correct answer
    /// for a side branch: that branch may cross difficulty-adjustment
    /// boundaries independently of the active chain.
    fn recompute_target_for_chain(&self, chain: &[Hash]) -> U256 {
        let mut target = crate::MIN_TARGET;
        let interval = crate::DIFFICULTY_UPDATE_INTERVAL as usize;
        let mut height = interval;
        while height <= chain.len() {
            let start_time = self
                .blocks
                .get(&chain[height - interval])
                .expect("BUG: chain references unknown block")
                .header
                .timestamp;
            let end_time = self
                .blocks
                .get(&chain[height - 1])
                .expect("BUG: chain references unknown block")
                .header
                .timestamp;
            target = Self::adjust_target(target, start_time, end_time);
            height += interval;
        }
        target
    }

    fn adjust_target(
        current_target: U256,
        start_time: DateTime<Utc>,
        end_time: DateTime<Utc>,
    ) -> U256 {
        let time_diff = end_time - start_time;
        let time_diff_seconds = time_diff.num_seconds().max(1) as u64;
        let target_seconds = crate::IDEAL_BLOCK_TIME * crate::DIFFICULTY_UPDATE_INTERVAL;

        // Clamp the ratio to at most 4x in either direction *before* it
        // touches the target, not after converting the result back to a
        // U256 (mirrors Bitcoin Core clamping nActualTimespan before
        // multiplying). An unclamped ratio -- e.g. from genesis's fixed
        // UNIX_EPOCH timestamp being decades before the first real block,
        // or a node that was offline a long time -- makes the BigDecimal
        // multiplication below produce a value with no upper bound, which
        // then fails to fit back into a U256 and panics instead of simply
        // capping the adjustment like a real retarget would.
        let time_diff_seconds = time_diff_seconds.clamp(target_seconds / 4, target_seconds * 4);

        let new_target = BigDecimal::parse_bytes(current_target.to_string().as_bytes(), 10)
            .expect("BUG: impossible")
            * (BigDecimal::from(time_diff_seconds) / BigDecimal::from(target_seconds));

        let new_target_str = new_target
            .to_string()
            .split('.')
            .next()
            .expect("BUG: Expected a decimal point")
            .to_owned();

        let new_target = U256::from_str_radix(&new_target_str, 10).expect("BUG: impossible");

        new_target.min(crate::MIN_TARGET)
    }

    /// Maps every active-chain hash to its position, for O(1) "is this
    /// hash on the active chain, and where" lookups. Building this is
    /// O(active chain length); callers that need to trace more than one
    /// hash in a row (e.g. `prunable_side_blocks`) should build it once
    /// and reuse it via `trace_branch_with_index` rather than paying that
    /// cost again per hash.
    fn active_chain_index(&self) -> HashMap<Hash, usize> {
        self.active_chain
            .iter()
            .enumerate()
            .map(|(i, h)| (*h, i))
            .collect()
    }

    /// Walks backwards from `hash` (which must already be a known block)
    /// along `prev_block_hash` pointers until it reaches either the zero
    /// hash or a block that's part of the active chain. Returns the index
    /// in `active_chain` where the branch forks off (blocks before that
    /// index are shared with the branch) together with the branch's own
    /// blocks in root-to-tip order, ending with `hash` itself. Returns
    /// `None` if `hash` is the zero hash, since there's no block to trace
    /// -- callers should treat that as "forks before anything we have".
    fn trace_branch(&self, hash: Hash) -> Option<(usize, Vec<Hash>)> {
        self.trace_branch_with_index(hash, &self.active_chain_index())
    }

    /// Same as `trace_branch`, but takes an already-built active-chain
    /// index instead of building a fresh one -- for callers tracing many
    /// hashes against the same active chain in one pass.
    fn trace_branch_with_index(
        &self,
        hash: Hash,
        active_index: &HashMap<Hash, usize>,
    ) -> Option<(usize, Vec<Hash>)> {
        if hash == Hash::zero() {
            return None;
        }

        let mut branch = Vec::new();
        let mut current = hash;
        loop {
            if let Some(&idx) = active_index.get(&current) {
                branch.reverse();
                return Some((idx + 1, branch));
            }
            let block = self
                .blocks
                .get(&current)
                .expect("BUG: trace_branch called on a block with unknown ancestry");
            branch.push(current);
            if block.header.prev_block_hash == Hash::zero() {
                branch.reverse();
                return Some((0, branch));
            }
            current = block.header.prev_block_hash;
        }
    }

    pub fn add_block(&mut self, block: Block) -> Result<()> {
        let tip_hash = self.active_chain.last().copied().unwrap_or(Hash::zero());
        let hash = if block.header.prev_block_hash == tip_hash {
            self.add_block_extending_tip(block)?
        } else {
            self.add_block_off_tip(block)?
        };
        self.try_attach_orphans(hash);
        Ok(())
    }

    /// Rejects blocks whose transactions (excluding the coinbase, which
    /// carries the block reward rather than user data) exceed the network's
    /// block-space budget. `create_block_template` already self-imposes
    /// this when building our own blocks; this enforces it as an actual
    /// consensus rule on blocks we *receive*, so a peer can't hand us an
    /// arbitrarily large block just because nothing stopped them.
    fn check_block_size(block: &Block) -> Result<()> {
        let tx_bytes: usize = block
            .transactions
            .iter()
            .skip(1)
            .map(|tx| tx.serialized_size())
            .sum();
        if tx_bytes > crate::BLOCK_BYTE_CAP {
            println!(
                "block exceeds BLOCK_BYTE_CAP ({tx_bytes} > {})",
                crate::BLOCK_BYTE_CAP
            );
            return Err(BtcError::InvalidBlock);
        }
        Ok(())
    }

    /// Rejects a block whose timestamp is further ahead of the network's
    /// clock than `MAX_FUTURE_BLOCK_DRIFT_SECONDS`. Without this, a block
    /// dated far in the future would sail through (nothing else checks a
    /// block's timestamp against the real world, only against its
    /// parent's), and difficulty-adjustment math -- which assumes
    /// timestamps are roughly honest -- would be easy to distort.
    ///
    /// "The network's clock" is our own clock plus `self.time_offset`,
    /// rather than raw `Utc::now()`: a node whose own clock is wrong would
    /// otherwise reject perfectly honest blocks (a self-inflicted, no
    /// attacker required liveness failure). `time_offset` is a median
    /// across many peers with a bounded adjustment (see
    /// `node::time_sync`), so this only ever nudges within a safe range,
    /// not "trust whatever a peer claims".
    fn check_timestamp(&self, timestamp: DateTime<Utc>) -> Result<()> {
        let network_now = Utc::now() + self.time_offset;
        let max_allowed = network_now + chrono::Duration::seconds(crate::MAX_FUTURE_BLOCK_DRIFT_SECONDS);
        if timestamp > max_allowed {
            println!("block timestamp is too far in the future");
            return Err(BtcError::InvalidBlock);
        }
        Ok(())
    }

    /// Fast path: the common case where a block simply extends whatever we
    /// already consider the tip of the active chain.
    fn add_block_extending_tip(&mut self, block: Block) -> Result<Hash> {
        // The first block this chain ever accepts must be THE canonical
        // genesis, byte for byte -- not merely "whatever zero-parented
        // block showed up first". Otherwise a fresh node's very first
        // sync would be trusting whichever peer happened to answer,
        // instead of verifying against a fixed protocol constant.
        if self.active_chain.is_empty() && block.hash() != Self::genesis_hash() {
            println!("rejecting a genesis block that isn't the network's canonical genesis");
            return Err(BtcError::InvalidBlock);
        }

        if block.header.target != self.target {
            println!("target does not match the difficulty required by the chain");
            return Err(BtcError::InvalidBlock);
        }

        if !block.header.hash().matches_target(block.header.target) {
            println!("does not match target; the blocks hash is less than the target");
            return Err(BtcError::InvalidBlock);
        }

        Self::check_block_size(&block)?;
        self.check_timestamp(block.header.timestamp)?;

        let calculated_merkle_root = MerkleRoot::calculate(&block.transactions);
        if calculated_merkle_root != block.header.merkle_root {
            println!("invalid merkle root");
            return Err(BtcError::InvalidMerkleRoot);
        }

        // Every block -- genesis included -- must have a valid coinbase
        // and internally-consistent transactions. Only the "timestamp must
        // be after the previous block" check is genuinely inapplicable to
        // genesis, since there is no previous block to compare against.
        if let Some(tip_hash) = self.active_chain.last() {
            let last_block = self.blocks.get(tip_hash).expect("BUG: missing tip block");
            if block.header.timestamp <= last_block.header.timestamp {
                return Err(BtcError::InvalidBlock);
            }
        }
        block.verify_transactions(self.block_height(), self.utxos())?;

        self.apply_block(&block);
        let hash = block.hash();
        self.blocks.insert(hash, block);
        self.active_chain.push(hash);

        self.try_adjust_target();
        // Piggyback on the same cadence as difficulty adjustment: cheap
        // enough not to matter, and frequent enough that abandoned side
        // branches don't linger for long once they're safely prunable.
        if self.block_height() % crate::DIFFICULTY_UPDATE_INTERVAL == 0 {
            self.prune_side_blocks(crate::SIDE_BRANCH_PRUNE_DEPTH);
        }
        Ok(hash)
    }

    /// Slow path: the block's parent is neither missing (that's an orphan,
    /// handled by the caller) nor the active tip, so it either extends an
    /// existing side branch or starts a new one.
    fn add_block_off_tip(&mut self, block: Block) -> Result<Hash> {
        let parent = block.header.prev_block_hash;

        // add_block only routes here when the block does NOT extend the
        // active tip. If the chain were still empty, the tip hash would
        // itself be Hash::zero() and a zero-parented block would have
        // matched the fast path instead -- so reaching here with a zero
        // parent means a real genesis already exists, and this is a second,
        // competing one. There is exactly one genesis per chain.
        if parent == Hash::zero() {
            println!("rejecting a second, competing genesis block");
            return Err(BtcError::InvalidBlock);
        }

        if !self.blocks.contains_key(&parent) {
            return self.buffer_orphan(block);
        }

        let (fork_index, parent_branch) = self
            .trace_branch(parent)
            .expect("BUG: parent is known and non-zero, trace_branch must succeed");

        let mut candidate_chain: Vec<Hash> = self.active_chain[..fork_index].to_vec();
        candidate_chain.extend(parent_branch);

        let expected_target = self.recompute_target_for_chain(&candidate_chain);
        if block.header.target != expected_target {
            println!("side branch block target does not match recomputed difficulty");
            return Err(BtcError::InvalidBlock);
        }
        if !block.header.hash().matches_target(block.header.target) {
            println!("side branch block does not match its own target");
            return Err(BtcError::InvalidBlock);
        }

        Self::check_block_size(&block)?;
        self.check_timestamp(block.header.timestamp)?;

        if let Some(parent_hash) = candidate_chain.last() {
            let parent_block = self
                .blocks
                .get(parent_hash)
                .expect("BUG: missing parent block");

            let calculated_merkle_root = MerkleRoot::calculate(&block.transactions);
            if calculated_merkle_root != block.header.merkle_root {
                println!("invalid merkle root");
                return Err(BtcError::InvalidMerkleRoot);
            }

            if block.header.timestamp <= parent_block.header.timestamp {
                return Err(BtcError::InvalidBlock);
            }
        }

        let utxos_at_parent = self.replay_utxos(&candidate_chain);
        block.verify_transactions(candidate_chain.len() as u64, &utxos_at_parent)?;

        let hash = block.hash();
        self.blocks.insert(hash, block);
        candidate_chain.push(hash);

        if self.work_of_chain(&candidate_chain) > self.chain_work() {
            self.reorg_to(fork_index, candidate_chain);
        }

        Ok(hash)
    }

    /// Switches the active chain to `new_chain` (which must share the
    /// prefix `active_chain[..fork_index]`), rebuilding UTXOs and the
    /// target from scratch, and giving transactions displaced from the old
    /// chain a chance to rejoin the mempool.
    fn reorg_to(&mut self, fork_index: usize, new_chain: Vec<Hash>) {
        let displaced: Vec<Transaction> = self.active_chain[fork_index..]
            .iter()
            .flat_map(|hash| {
                self.blocks
                    .get(hash)
                    .expect("BUG: missing displaced block")
                    .transactions
                    .iter()
                    .skip(1) // each block's coinbase doesn't belong in the mempool
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .collect();

        println!(
            "reorg: switching active chain at height {} ({} blocks displaced)",
            fork_index,
            self.active_chain.len() - fork_index
        );

        self.active_chain = new_chain;
        self.target = self.recompute_target_for_chain(&self.active_chain);
        self.sync_mempool_with_active_chain();

        for tx in displaced {
            // best-effort: if it conflicts with the new chain (e.g. an
            // input already spent there), just drop it, same as Bitcoin
            // Core does with transactions displaced by a reorg
            let _ = self.add_to_mempool(tx);
        }
    }

    /// Drops from the mempool any transaction that's now confirmed on the
    /// active chain, rebuilds the UTXO set from that chain, and re-marks
    /// the still-pending mempool transactions' inputs as spent.
    fn sync_mempool_with_active_chain(&mut self) {
        let confirmed: HashSet<Hash> = self
            .blocks()
            .flat_map(|block| block.transactions.iter().map(|tx| tx.hash()))
            .collect();
        self.rebuild_utxos();
        self.evict_stale_mempool_entries(&confirmed);

        let pending: Vec<Transaction> = self.mempool.iter().map(|(_, tx)| tx.clone()).collect();
        for tx in pending {
            self.mark_transaction_utxos(&tx);
        }
    }

    /// Drops mempool transactions that `confirmed` covers directly, or
    /// whose input no longer exists in `self.utxos` at all -- the latter
    /// covers a transaction that lost a race against a *different*,
    /// conflicting transaction that got confirmed instead. Without this
    /// second check a stale mempool entry would linger with a dangling
    /// UTXO reference, and the next `add_to_mempool` call would panic on
    /// it (`transaction_fee`'s `.expect("BUG: impossible")` assumes every
    /// mempool transaction's inputs still exist). Callers must bring
    /// `self.utxos` up to date BEFORE calling this.
    fn evict_stale_mempool_entries(&mut self, confirmed: &HashSet<Hash>) {
        self.cleanup_mempool();
        self.mempool.retain(|(_, tx)| {
            if confirmed.contains(&tx.hash()) {
                return false;
            }
            tx.inputs
                .iter()
                .all(|input| self.utxos.contains_key(&input.prev_transaction_output_hash))
        });
    }

    fn buffer_orphan(&mut self, block: Block) -> Result<Hash> {
        if self.orphans.len() >= MAX_ORPHAN_BLOCKS {
            println!("orphan pool full, dropping block instead of buffering it");
            return Err(BtcError::OrphanBlock);
        }
        let hash = block.hash();
        self.orphans.insert(hash, block);
        Err(BtcError::OrphanBlock)
    }

    /// After a block is successfully added, any previously-buffered orphan
    /// whose parent was that block can now be retried.
    fn try_attach_orphans(&mut self, parent_hash: Hash) {
        let ready: Vec<Block> = self
            .orphans
            .iter()
            .filter(|(_, block)| block.header.prev_block_hash == parent_hash)
            .map(|(_, block)| block.clone())
            .collect();

        for block in ready {
            let hash = block.hash();
            self.orphans.remove(&hash);
            // ignore the outcome: if it's invalid for some other reason we
            // just drop it
            let _ = self.add_block(block);
        }
    }

    pub fn try_adjust_target(&mut self) {
        if self.active_chain.is_empty() {
            return;
        }

        if self.block_height() % crate::DIFFICULTY_UPDATE_INTERVAL != 0 {
            return;
        }

        // self.target already reflects every retarget before this window
        // (that's the invariant recompute_target_for_chain exists to
        // establish for a chain we *don't* already have a running target
        // for, e.g. a side branch). So on the active chain we only need
        // the two timestamps bounding the interval that just elapsed,
        // not a full genesis-to-tip replay of every past window.
        let interval = crate::DIFFICULTY_UPDATE_INTERVAL as usize;
        let height = self.active_chain.len();
        let start_hash = self.active_chain[height - interval];
        let end_hash = self.active_chain[height - 1];
        let start_time = self
            .blocks
            .get(&start_hash)
            .expect("BUG: chain references unknown block")
            .header
            .timestamp;
        let end_time = self
            .blocks
            .get(&end_hash)
            .expect("BUG: chain references unknown block")
            .header
            .timestamp;
        self.target = Self::adjust_target(self.target, start_time, end_time);
    }

    /// Identifies stored blocks that are no longer part of the active
    /// chain and whose fork point is deeper than `keep_depth` blocks
    /// behind the current tip -- deep enough that a reorg back to them is
    /// not realistically possible anymore, so there's no point holding
    /// onto them (in memory or in durable storage) forever.
    fn prunable_side_blocks(&self, keep_depth: u64) -> Vec<Hash> {
        // Built once and reused for every candidate below, instead of
        // trace_branch rebuilding it (and add_block_off_tip's -- separate
        // -- filter step) from scratch per hash: that turned this into
        // O(side blocks * chain length) for no benefit, since every
        // candidate is checked against the same, unchanging active chain.
        let active_index = self.active_chain_index();
        self.blocks
            .keys()
            .filter(|hash| !active_index.contains_key(*hash))
            .filter_map(|hash| {
                let (fork_index, _) = self.trace_branch_with_index(*hash, &active_index)?;
                let depth = self.active_chain.len().saturating_sub(fork_index) as u64;
                (depth > keep_depth).then_some(*hash)
            })
            .collect()
    }

    /// Prunes side-branch blocks identified by `prunable_side_blocks` from
    /// memory, and queues their hashes so `take_pruned_hashes` can tell a
    /// caller with durable storage to delete them there too.
    fn prune_side_blocks(&mut self, keep_depth: u64) {
        let prunable = self.prunable_side_blocks(keep_depth);
        for hash in &prunable {
            self.blocks.remove(hash);
        }
        self.pending_prunes.extend(prunable);
    }

    /// Drains the list of block hashes pruned from memory since the last
    /// call, so a caller backed by durable storage can remove them there
    /// too. Returns an empty vec if nothing has been pruned.
    pub fn take_pruned_hashes(&mut self) -> Vec<Hash> {
        std::mem::take(&mut self.pending_prunes)
    }

    pub fn rebuild_utxos(&mut self) {
        self.utxos = self.replay_utxos(&self.active_chain);
    }

    /// Applies a single block's transactions to the UTXO set in place
    /// (removing spent inputs, inserting new outputs) instead of replaying
    /// the whole chain from scratch. This is what keeps the common case --
    /// one more block extends the tip -- O(block size) instead of O(chain
    /// length); `rebuild_utxos`/`replay_utxos` remain the correct tool for
    /// the rarer case of switching to a different chain entirely (reorgs),
    /// where a single block's deltas aren't enough.
    ///
    /// Also drops from the mempool whatever this block confirmed, and
    /// evicts any pending transaction whose input the block just spent out
    /// from under it -- see `evict_stale_mempool_entries`.
    fn apply_block(&mut self, block: &Block) {
        for transaction in &block.transactions {
            for input in &transaction.inputs {
                self.utxos.remove(&input.prev_transaction_output_hash);
            }
            for output in &transaction.outputs {
                self.utxos.insert(output.hash(), (false, output.clone()));
            }
        }

        let confirmed: HashSet<Hash> = block.transactions.iter().map(|tx| tx.hash()).collect();
        self.evict_stale_mempool_entries(&confirmed);
        // Utxos untouched by this block keep whatever `marked` bit they
        // already had (correct, since nothing about them changed), and any
        // brand-new output this block created starts unmarked (nothing is
        // pending against it yet) -- so unlike the full-rebuild path, no
        // re-marking pass over the mempool is needed here.
    }
}

#[cfg(test)]
mod tests {

    /// A transaction spending `prev` into `outputs`, signed by `key`.
    ///
    /// Here rather than inline because an input signs the output list
    /// (`Transaction::spend_commitment`), so the outputs have to be built
    /// before the input can be: written out by hand, every one of these
    /// is four lines of ordering that has nothing to do with what the
    /// test is about.
    fn spend(
        prev: &TransactionOutput,
        key: &PrivateKey,
        outputs: Vec<TransactionOutput>,
    ) -> Transaction {
        let inputs = vec![TransactionInput::signed(prev.hash(), &outputs, key)];
        Transaction::new(inputs, outputs)
    }
    use super::*;
    use crate::crypto::{PrivateKey, Signature};
    use crate::types::TransactionInput;

    fn mined_child_block(
        parent: Hash,
        timestamp: DateTime<Utc>,
        target: U256,
        reward: u64,
    ) -> Block {
        let private_key = PrivateKey::new_key();
        let coinbase = Transaction::new(
            vec![],
            vec![TransactionOutput {
                value: reward,
                unique_id: Uuid::new_v4(),
                pubkey: private_key.public_key(),
            }],
        );
        let merkle_root = MerkleRoot::calculate(&[coinbase.clone()]);
        let mut header = BlockHeader::new(timestamp, 0, parent, merkle_root, target);
        assert!(
            header.mine(1_000_000),
            "failed to find a valid nonce within the test mining budget"
        );
        Block::new(header, vec![coinbase])
    }

    /// Builds an internally-consistent but non-canonical zero-parented
    /// block -- useful only for proving genesis pinning rejects it.
    fn fake_genesis_block(target: U256, reward: u64) -> Block {
        mined_child_block(Hash::zero(), Utc::now(), target, reward)
    }

    /// Establishes the real, canonical genesis on a fresh chain and
    /// returns (blockchain, genesis_hash, genesis_timestamp) so tests can
    /// build children on top of it.
    fn chain_with_real_genesis() -> (Blockchain, Hash, DateTime<Utc>) {
        let mut blockchain = Blockchain::new();
        let genesis = Blockchain::genesis_block().clone();
        let genesis_hash = genesis.hash();
        let genesis_ts = genesis.header.timestamp;
        blockchain.add_block(genesis).unwrap();
        (blockchain, genesis_hash, genesis_ts)
    }

    #[test]
    fn genesis_block_is_deterministic_and_valid() {
        // calling it twice (e.g. from two different "nodes") must yield
        // bit-identical results, and a fresh chain must accept it
        assert_eq!(
            Blockchain::genesis_block().hash(),
            Blockchain::genesis_block().hash()
        );
        let mut blockchain = Blockchain::new();
        assert!(blockchain
            .add_block(Blockchain::genesis_block().clone())
            .is_ok());
        assert_eq!(blockchain.block_height(), 1);
    }

    #[test]
    fn add_block_rejects_a_non_canonical_first_block() {
        let mut blockchain = Blockchain::new();
        // internally valid (correct target, PoW, merkle root, coinbase
        // amount) but not THE genesis -- must still be rejected, since a
        // fresh node's first block should never just be "whatever showed
        // up first"
        let fake_genesis =
            fake_genesis_block(blockchain.target(), blockchain.calculate_block_reward());
        assert!(matches!(
            blockchain.add_block(fake_genesis),
            Err(BtcError::InvalidBlock)
        ));
        assert_eq!(blockchain.block_height(), 0);
    }

    #[test]
    fn add_block_rejects_a_second_competing_genesis() {
        let (mut blockchain, _, _) = chain_with_real_genesis();
        assert_eq!(blockchain.block_height(), 1);

        // an entirely unrelated block that also claims prev_block_hash ==
        // Hash::zero() -- there can only ever be one genesis
        let rival_genesis =
            fake_genesis_block(blockchain.target(), blockchain.calculate_block_reward());
        assert!(matches!(
            blockchain.add_block(rival_genesis),
            Err(BtcError::InvalidBlock)
        ));
        assert_eq!(
            blockchain.block_height(),
            1,
            "must not have accepted a competing genesis"
        );
    }

    #[test]
    fn add_block_rejects_target_mismatching_chain_difficulty() {
        let (mut blockchain, genesis_hash, genesis_ts) = chain_with_real_genesis();
        // the child claims (and legitimately satisfies) MIN_TARGET, but we
        // pretend the chain currently requires a different target -- this
        // must be rejected regardless of the block's own internal validity
        blockchain.target = crate::MIN_TARGET / 2;
        let reward = blockchain.calculate_block_reward();
        let child = mined_child_block(
            genesis_hash,
            genesis_ts + chrono::Duration::seconds(1),
            crate::MIN_TARGET,
            reward,
        );
        assert!(matches!(
            blockchain.add_block(child),
            Err(BtcError::InvalidBlock)
        ));
    }

    #[test]
    fn add_block_accepts_target_matching_chain_difficulty() {
        let (mut blockchain, genesis_hash, genesis_ts) = chain_with_real_genesis();
        let target = blockchain.target();
        let reward = blockchain.calculate_block_reward();
        let child = mined_child_block(
            genesis_hash,
            genesis_ts + chrono::Duration::seconds(1),
            target,
            reward,
        );
        assert!(blockchain.add_block(child).is_ok());
        assert_eq!(blockchain.block_height(), 2);
    }

    #[test]
    fn side_branch_with_more_blocks_triggers_reorg() {
        let (mut blockchain, genesis_hash, genesis_ts) = chain_with_real_genesis();
        let reward = blockchain.calculate_block_reward();
        let target = blockchain.target();

        // the "main" branch: one block on top of genesis
        let main_block = mined_child_block(genesis_hash, genesis_ts + chrono::Duration::seconds(1), target, reward);
        let main_hash = main_block.hash();
        blockchain.add_block(main_block).unwrap();
        assert_eq!(blockchain.block_height(), 2);
        assert_eq!(blockchain.blocks().last().unwrap().hash(), main_hash);

        // a side branch: two blocks directly on top of genesis, bypassing
        // "main" entirely. Received one at a time, exactly as they would
        // arrive from the network.
        let side_1 = mined_child_block(genesis_hash, genesis_ts + chrono::Duration::seconds(2), target, reward);
        let side_1_hash = side_1.hash();
        let side_1_ts = side_1.header.timestamp;
        // side_1 ties main on work (1 block each past the fork) so it must
        // be stored but must NOT yet become the active chain
        blockchain.add_block(side_1).unwrap();
        assert_eq!(blockchain.blocks().last().unwrap().hash(), main_hash);

        let side_2 = mined_child_block(side_1_hash, side_1_ts + chrono::Duration::seconds(1), target, reward);
        let side_2_hash = side_2.hash();
        // side_2 puts the side branch at 2 blocks past the fork vs main's
        // 1, so this must trigger a reorg onto the side branch
        blockchain.add_block(side_2).unwrap();

        assert_eq!(blockchain.block_height(), 3);
        assert_eq!(blockchain.blocks().last().unwrap().hash(), side_2_hash);
        assert!(blockchain
            .blocks()
            .any(|b| b.hash() == genesis_hash), "genesis should still be shared ancestor");
    }

    #[test]
    fn orphan_blocks_are_buffered_and_attached_once_parent_arrives() {
        let mut blockchain = Blockchain::new();
        let genesis = Blockchain::genesis_block().clone();
        let genesis_hash = genesis.hash();
        let genesis_ts = genesis.header.timestamp;

        let reward = blockchain.calculate_block_reward();
        let target = blockchain.target();
        let child = mined_child_block(genesis_hash, genesis_ts + chrono::Duration::seconds(1), target, reward);
        let child_hash = child.hash();

        // the child arrives before genesis does -- its parent is entirely
        // unknown, so it must be buffered rather than rejected outright
        assert!(matches!(
            blockchain.add_block(child.clone()),
            Err(BtcError::OrphanBlock)
        ));
        assert_eq!(blockchain.block_height(), 0);

        // now genesis arrives; the buffered orphan should automatically
        // attach on top of it without needing to be resent
        blockchain.add_block(genesis).unwrap();
        assert_eq!(blockchain.block_height(), 2);
        assert_eq!(blockchain.blocks().last().unwrap().hash(), child_hash);
    }

    #[test]
    fn add_block_rejects_wrong_merkle_root() {
        let (mut blockchain, genesis_hash, genesis_ts) = chain_with_real_genesis();
        let target = blockchain.target();
        let reward = blockchain.calculate_block_reward();
        let private_key = PrivateKey::new_key();

        let coinbase = Transaction::new(
            vec![],
            vec![TransactionOutput {
                value: reward,
                unique_id: Uuid::new_v4(),
                pubkey: private_key.public_key(),
            }],
        );
        // deliberately commit to the merkle root of a DIFFERENT transaction
        // than the block's actual contents
        let decoy = Transaction::new(vec![], vec![]);
        let wrong_root = MerkleRoot::calculate(&[decoy]);
        let mut header = BlockHeader::new(
            genesis_ts + chrono::Duration::seconds(1),
            0,
            genesis_hash,
            wrong_root,
            target,
        );
        assert!(header.mine(1_000_000));
        let block = Block::new(header, vec![coinbase]);

        assert!(matches!(
            blockchain.add_block(block),
            Err(BtcError::InvalidMerkleRoot)
        ));
    }

    #[test]
    fn add_block_rejects_oversized_block() {
        let (mut blockchain, genesis_hash, genesis_ts) = chain_with_real_genesis();
        let target = blockchain.target();
        let reward = blockchain.calculate_block_reward();
        let private_key = PrivateKey::new_key();
        let pubkey = private_key.public_key();

        let coinbase = Transaction::new(
            vec![],
            vec![TransactionOutput {
                value: reward,
                unique_id: Uuid::new_v4(),
                pubkey: pubkey.clone(),
            }],
        );
        // one transaction with enough outputs to blow past BLOCK_BYTE_CAP
        // (1,000,000 bytes) on its own
        let bloated_outputs: Vec<TransactionOutput> = (0..20_000)
            .map(|_| TransactionOutput {
                value: 1,
                unique_id: Uuid::new_v4(),
                pubkey: pubkey.clone(),
            })
            .collect();
        let bloated_tx = Transaction::new(vec![], bloated_outputs);
        assert!(
            bloated_tx.serialized_size() > crate::BLOCK_BYTE_CAP,
            "test transaction must actually exceed the cap to be a valid test"
        );

        let transactions = vec![coinbase, bloated_tx];
        let merkle_root = MerkleRoot::calculate(&transactions);
        let mut header = BlockHeader::new(
            genesis_ts + chrono::Duration::seconds(1),
            0,
            genesis_hash,
            merkle_root,
            target,
        );
        assert!(header.mine(1_000_000));
        let block = Block::new(header, transactions);

        assert!(matches!(
            blockchain.add_block(block),
            Err(BtcError::InvalidBlock)
        ));
        assert_eq!(blockchain.block_height(), 1, "genesis only; the oversized child never got in");
    }

    #[test]
    fn add_block_rejects_timestamp_too_far_in_the_future() {
        let (mut blockchain, genesis_hash, _) = chain_with_real_genesis();
        let target = blockchain.target();
        let reward = blockchain.calculate_block_reward();

        let far_future = Utc::now() + chrono::Duration::seconds(crate::MAX_FUTURE_BLOCK_DRIFT_SECONDS + 60);
        let child = mined_child_block(genesis_hash, far_future, target, reward);

        assert!(matches!(
            blockchain.add_block(child),
            Err(BtcError::InvalidBlock)
        ));
        assert_eq!(blockchain.block_height(), 1);
    }

    #[test]
    fn conflicting_onchain_transaction_evicts_stale_mempool_entry() {
        let (mut blockchain, genesis_hash, genesis_ts) = chain_with_real_genesis();
        let target = blockchain.target();
        let reward = blockchain.calculate_block_reward();

        // block1: a coinbase paying a key we control, so we have a
        // spendable UTXO to build conflicting transactions against
        let payee_key = PrivateKey::new_key();
        let coinbase1 = Transaction::new(
            vec![],
            vec![TransactionOutput {
                value: reward,
                unique_id: Uuid::new_v4(),
                pubkey: payee_key.public_key(),
            }],
        );
        let merkle_root1 = MerkleRoot::calculate(&[coinbase1.clone()]);
        let mut header1 = BlockHeader::new(
            genesis_ts + chrono::Duration::seconds(1),
            0,
            genesis_hash,
            merkle_root1,
            target,
        );
        assert!(header1.mine(1_000_000));
        let block1 = Block::new(header1, vec![coinbase1]);
        let block1_hash = block1.hash();
        let block1_ts = block1.header.timestamp;
        let spendable_output = block1.transactions[0].outputs[0].clone();
        blockchain.add_block(block1).unwrap();

        // two transactions racing to spend the same output to different
        // recipients
        let recipient_a = PrivateKey::new_key().public_key();
        let recipient_b = PrivateKey::new_key().public_key();
        let tx_a = spend(
            &spendable_output,
            &payee_key,
            vec![TransactionOutput {
                value: reward,
                unique_id: Uuid::new_v4(),
                pubkey: recipient_a,
            }],
        );
        let tx_b = spend(
            &spendable_output,
            &payee_key,
            vec![TransactionOutput {
                value: reward,
                unique_id: Uuid::new_v4(),
                pubkey: recipient_b,
            }],
        );

        blockchain.add_to_mempool(tx_a.clone()).unwrap();
        assert_eq!(blockchain.mempool().len(), 1);

        // block2 confirms tx_b instead -- tx_a's input is now gone, so it
        // must be evicted rather than left dangling in the mempool
        let coinbase2 = Transaction::new(
            vec![],
            vec![TransactionOutput {
                value: blockchain.calculate_block_reward(),
                unique_id: Uuid::new_v4(),
                pubkey: PrivateKey::new_key().public_key(),
            }],
        );
        let transactions2 = vec![coinbase2, tx_b];
        let merkle_root2 = MerkleRoot::calculate(&transactions2);
        let mut header2 = BlockHeader::new(
            block1_ts + chrono::Duration::seconds(1),
            0,
            block1_hash,
            merkle_root2,
            blockchain.target(),
        );
        assert!(header2.mine(1_000_000));
        let block2 = Block::new(header2, transactions2);
        blockchain.add_block(block2).unwrap();

        assert!(
            blockchain.mempool().iter().all(|(_, tx)| tx.hash() != tx_a.hash()),
            "the losing side of the double-spend should have been evicted"
        );

        // regression check: before the fix, tx_a would linger in the
        // mempool with a dangling UTXO reference, and add_to_mempool's
        // fee-sorting pass (which calls transaction_fee on every existing
        // entry) would panic on it. Adding anything else must not panic.
        let throwaway = Transaction::new(vec![], vec![]);
        let _ = blockchain.add_to_mempool(throwaway);
    }

    /// Two outputs whose values wrap past `u64::MAX` sum to exactly the
    /// input, so `input < output` is false and the transaction conserves
    /// value on paper while creating 2^64 units in fact. The dev profile
    /// panicked on the `+`, so no test ever reached the comparison; the
    /// release profile the node shipped under had overflow checks off
    /// and wrapped silently. Both the mempool and block verification
    /// refuse it now, and the input it names stays unmarked.
    #[test]
    fn a_transaction_whose_outputs_wrap_is_refused_rather_than_conserved() {
        let (mut blockchain, genesis_hash, genesis_ts) = chain_with_real_genesis();
        let target = blockchain.target();
        let reward = blockchain.calculate_block_reward();

        let payee_key = PrivateKey::new_key();
        let coinbase1 = Transaction::new(
            vec![],
            vec![TransactionOutput {
                value: reward,
                unique_id: Uuid::new_v4(),
                pubkey: payee_key.public_key(),
            }],
        );
        let merkle_root1 = MerkleRoot::calculate(&[coinbase1.clone()]);
        let mut header1 = BlockHeader::new(
            genesis_ts + chrono::Duration::seconds(1),
            0,
            genesis_hash,
            merkle_root1,
            target,
        );
        assert!(header1.mine(1_000_000));
        let block1 = Block::new(header1, vec![coinbase1]);
        let block1_hash = block1.hash();
        let block1_ts = block1.header.timestamp;
        let spendable = block1.transactions[0].outputs[0].clone();
        blockchain.add_block(block1).unwrap();

        // u64::MAX + (reward + 1) wraps to exactly `reward`: the input.
        let wrapped = spend(
            &spendable,
            &payee_key,
            vec![
                TransactionOutput {
                    value: u64::MAX,
                    unique_id: Uuid::new_v4(),
                    pubkey: PrivateKey::new_key().public_key(),
                },
                TransactionOutput {
                    value: reward + 1,
                    unique_id: Uuid::new_v4(),
                    pubkey: PrivateKey::new_key().public_key(),
                },
            ],
        );
        assert_eq!(u64::MAX.wrapping_add(reward + 1), reward, "the test's premise");

        assert!(
            matches!(blockchain.add_to_mempool(wrapped.clone()), Err(BtcError::InvalidTransaction)),
            "the mempool must refuse a wrapped output sum"
        );
        assert!(blockchain.mempool().is_empty());
        assert!(
            !blockchain.utxos()[&spendable.hash()].0,
            "a refused spend must not leave its input marked"
        );

        // And a mined block carrying it is refused by verification too.
        let utxos_before = blockchain.utxos().len();
        let coinbase2 = Transaction::new(
            vec![],
            vec![TransactionOutput {
                value: blockchain.calculate_block_reward(),
                unique_id: Uuid::new_v4(),
                pubkey: PrivateKey::new_key().public_key(),
            }],
        );
        let transactions2 = vec![coinbase2, wrapped];
        let mut header2 = BlockHeader::new(
            block1_ts + chrono::Duration::seconds(1),
            0,
            block1_hash,
            MerkleRoot::calculate(&transactions2),
            blockchain.target(),
        );
        assert!(header2.mine(1_000_000));
        assert!(blockchain.add_block(Block::new(header2, transactions2)).is_err());
        assert_eq!(blockchain.utxos().len(), utxos_before, "a refused block changes nothing");
    }

    /// The mempool admitted a spend of someone else's output on the
    /// strength of a signature it never checked, and marked the output
    /// spent on the way in. Signed with the wrong key, the spend is now
    /// refused before anything is marked, and the rightful owner can
    /// still spend the same output afterwards.
    #[test]
    fn a_spend_signed_by_the_wrong_key_is_refused_by_the_mempool() {
        let (mut blockchain, genesis_hash, genesis_ts) = chain_with_real_genesis();
        let target = blockchain.target();
        let reward = blockchain.calculate_block_reward();

        let owner = PrivateKey::new_key();
        let coinbase1 = Transaction::new(
            vec![],
            vec![TransactionOutput {
                value: reward,
                unique_id: Uuid::new_v4(),
                pubkey: owner.public_key(),
            }],
        );
        let merkle_root1 = MerkleRoot::calculate(&[coinbase1.clone()]);
        let mut header1 = BlockHeader::new(
            genesis_ts + chrono::Duration::seconds(1),
            0,
            genesis_hash,
            merkle_root1,
            target,
        );
        assert!(header1.mine(1_000_000));
        let block1 = Block::new(header1, vec![coinbase1]);
        let spendable = block1.transactions[0].outputs[0].clone();
        blockchain.add_block(block1).unwrap();

        let thief = PrivateKey::new_key();
        let forged = spend(
            &spendable,
            &thief,
            vec![TransactionOutput {
                value: 0,
                unique_id: Uuid::new_v4(),
                pubkey: thief.public_key(),
            }],
        );
        assert!(
            matches!(blockchain.add_to_mempool(forged), Err(BtcError::InvalidSignature)),
            "a spend the owner did not sign must be refused"
        );
        assert!(blockchain.mempool().is_empty());
        assert!(
            !blockchain.utxos()[&spendable.hash()].0,
            "a refused spend must not mark the output it names"
        );

        let honest = spend(
            &spendable,
            &owner,
            vec![TransactionOutput {
                value: reward,
                unique_id: Uuid::new_v4(),
                pubkey: PrivateKey::new_key().public_key(),
            }],
        );
        blockchain.add_to_mempool(honest).unwrap();
        assert_eq!(blockchain.mempool().len(), 1);
    }

    /// The theft `Transaction::spend_commitment` exists to stop, and the
    /// one a signature over the spent output alone could not: the owner
    /// signs a payment, and whoever handles it next -- the hub relaying
    /// it, a node, the miner picking transactions for a block -- points
    /// the outputs at themselves. The signature still belonged to the
    /// owner and still verified, so the chain took it and the coin was
    /// gone. Nothing in the transaction had ever said who it paid.
    #[test]
    fn a_spend_whose_outputs_are_rewritten_after_signing_is_refused() {
        let (mut blockchain, genesis_hash, genesis_ts) = chain_with_real_genesis();
        let target = blockchain.target();
        let reward = blockchain.calculate_block_reward();

        let owner = PrivateKey::new_key();
        let coinbase1 = Transaction::new(
            vec![],
            vec![TransactionOutput {
                value: reward,
                unique_id: Uuid::new_v4(),
                pubkey: owner.public_key(),
            }],
        );
        let merkle_root1 = MerkleRoot::calculate(&[coinbase1.clone()]);
        let mut header1 = BlockHeader::new(
            genesis_ts + chrono::Duration::seconds(1),
            0,
            genesis_hash,
            merkle_root1,
            target,
        );
        assert!(header1.mine(1_000_000));
        let block1 = Block::new(header1, vec![coinbase1]);
        let spendable = block1.transactions[0].outputs[0].clone();
        blockchain.add_block(block1).unwrap();

        let payee = PrivateKey::new_key().public_key();
        let honest = spend(
            &spendable,
            &owner,
            vec![TransactionOutput {
                value: reward,
                unique_id: Uuid::new_v4(),
                pubkey: payee,
            }],
        );

        // The rewrite itself: the owner's input, verbatim, over an
        // output list that now pays the thief.
        let thief = PrivateKey::new_key();
        let mut rewritten = honest.clone();
        rewritten.outputs[0].pubkey = thief.public_key();
        assert!(
            matches!(
                blockchain.add_to_mempool(rewritten.clone()),
                Err(BtcError::InvalidSignature)
            ),
            "an output list the signer never signed must be refused"
        );
        assert!(blockchain.mempool().is_empty());
        assert!(
            !blockchain.utxos()[&spendable.hash()].0,
            "a refused spend must not mark the output it names"
        );

        // A block carrying it is refused too, and by the same rule --
        // the mempool is not the only door (`Block::verify_transactions`).
        let coinbase2 = Transaction::new(
            vec![],
            vec![TransactionOutput {
                value: blockchain.calculate_block_reward(),
                unique_id: Uuid::new_v4(),
                pubkey: thief.public_key(),
            }],
        );
        let transactions2 = vec![coinbase2, rewritten];
        let merkle_root2 = MerkleRoot::calculate(&transactions2);
        let mut header2 = BlockHeader::new(
            genesis_ts + chrono::Duration::seconds(2),
            0,
            blockchain.active_chain().last().copied().unwrap(),
            merkle_root2,
            target,
        );
        assert!(header2.mine(1_000_000));
        assert!(blockchain.add_block(Block::new(header2, transactions2)).is_err());

        // And the payment the owner actually signed is still good.
        blockchain.add_to_mempool(honest).unwrap();
        assert_eq!(blockchain.mempool().len(), 1);
    }

    /// At the 64th halving -- height 504,000, about 93 days in at
    /// 16-second blocks -- the coinbase check divided by `2u64.pow(64)`,
    /// which overflows, and every block from there on failed
    /// verification: the chain could accept nothing more. The reward
    /// has been zero since the 33rd halving; the check should say so
    /// and move on.
    #[test]
    fn a_block_past_the_sixty_fourth_halving_still_verifies_its_coinbase() {
        let (blockchain, genesis_hash, genesis_ts) = chain_with_real_genesis();
        let coinbase = Transaction::new(
            vec![],
            vec![TransactionOutput {
                value: 0,
                unique_id: Uuid::new_v4(),
                pubkey: PrivateKey::new_key().public_key(),
            }],
        );
        let header = BlockHeader::new(
            genesis_ts + chrono::Duration::seconds(1),
            0,
            genesis_hash,
            MerkleRoot::calculate(&[coinbase.clone()]),
            blockchain.target(),
        );
        let block = Block::new(header, vec![coinbase]);
        for height in [33 * crate::HALVING_INTERVAL, 64 * crate::HALVING_INTERVAL, u64::MAX] {
            assert!(
                block.verify_coinbase_transaction(height, &HashMap::new()).is_ok(),
                "a zero coinbase must verify at height {height}"
            );
        }
    }

    #[test]
    fn prune_side_blocks_removes_deeply_buried_forks_only() {
        let (mut blockchain, genesis_hash, genesis_ts) = chain_with_real_genesis();
        let target = blockchain.target();
        let reward = blockchain.calculate_block_reward();

        // establish "main" as the active tip first
        let main1 = mined_child_block(genesis_hash, genesis_ts + chrono::Duration::seconds(1), target, reward);
        let main1_hash = main1.hash();
        let main1_ts = main1.header.timestamp;
        blockchain.add_block(main1).unwrap();

        // a block forking directly off genesis -- main1 is the active
        // tip, so this is stored but inactive
        let side = mined_child_block(genesis_hash, genesis_ts + chrono::Duration::seconds(2), target, reward);
        let side_hash = side.hash();
        blockchain.add_block(side).unwrap();
        assert_eq!(blockchain.block_height(), 2, "side branch must not have become active");
        assert!(blockchain.get_block(&side_hash).is_some());

        // not yet deep enough to prune: the fork point is only 1 block
        // behind the tip
        assert!(blockchain.prunable_side_blocks(5).is_empty());

        // extend "main" further so the side branch's fork point (right
        // after genesis) falls far enough behind
        let mut parent_hash = main1_hash;
        let mut parent_ts = main1_ts;
        for i in 0..6 {
            let block = mined_child_block(
                parent_hash,
                parent_ts + chrono::Duration::seconds(3 + i),
                blockchain.target(),
                blockchain.calculate_block_reward(),
            );
            parent_hash = block.hash();
            parent_ts = block.header.timestamp;
            blockchain.add_block(block).unwrap();
        }
        assert_eq!(blockchain.block_height(), 8);

        // side forked at active-chain index 1; tip is now at height 8, so
        // its fork depth is 8 - 1 = 7 -- deep enough for keep_depth=5
        assert_eq!(blockchain.prunable_side_blocks(5), vec![side_hash]);

        blockchain.prune_side_blocks(5);
        assert!(
            blockchain.get_block(&side_hash).is_none(),
            "side block should have been pruned from memory"
        );
        assert_eq!(blockchain.take_pruned_hashes(), vec![side_hash]);
        // draining is one-shot: nothing left to take a second time
        assert!(blockchain.take_pruned_hashes().is_empty());
    }

    #[test]
    fn check_timestamp_uses_network_adjusted_time() {
        let (mut blockchain, genesis_hash, _) = chain_with_real_genesis();
        let target = blockchain.target();
        let reward = blockchain.calculate_block_reward();

        // just past the ordinary (zero-offset) future-drift limit
        let just_too_far =
            Utc::now() + chrono::Duration::seconds(crate::MAX_FUTURE_BLOCK_DRIFT_SECONDS + 30);
        let child = mined_child_block(genesis_hash, just_too_far, target, reward);

        // rejected against our own unadjusted clock
        assert!(matches!(
            blockchain.add_block(child.clone()),
            Err(BtcError::InvalidBlock)
        ));

        // once the network tells us our own clock is running 5 minutes
        // behind, the same timestamp is legitimately within the window
        blockchain.set_time_offset(chrono::Duration::minutes(5));
        assert!(blockchain.add_block(child).is_ok());
    }

    #[test]
    fn adjust_target_does_not_overflow_on_an_extreme_time_gap() {
        // Genesis's timestamp is UNIX_EPOCH while every real block uses
        // Utc::now(), so the very first retarget window on a fresh chain
        // spans a ~56-year gap. Before this was fixed, the unclamped
        // ratio made the intermediate BigDecimal multiplication produce a
        // value too large to fit back into a U256, panicking instead of
        // clamping like a real retarget would.
        let result =
            Blockchain::adjust_target(crate::MIN_TARGET, DateTime::<Utc>::UNIX_EPOCH, Utc::now());
        // ratio clamps to at most 4x, and the result never exceeds
        // MIN_TARGET -- so an extreme "took too long" gap against an
        // already-loosest target lands right back at MIN_TARGET.
        assert_eq!(result, crate::MIN_TARGET);
    }

    #[test]
    fn adjust_target_clamps_a_much_faster_than_ideal_window_to_one_quarter() {
        // Symmetric case: blocks mined far faster than ideal should
        // tighten the target, but by at most 4x per window, same as
        // Bitcoin's own retargeting.
        let start = Utc::now();
        let end = start + chrono::Duration::seconds(1);
        let result = Blockchain::adjust_target(crate::MIN_TARGET, start, end);
        assert_eq!(result, crate::MIN_TARGET / 4);
    }
}

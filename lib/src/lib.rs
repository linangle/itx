use tracing::*;

pub mod crypto;
pub mod envelope;
pub mod error;
pub mod network;
pub mod payment;
pub mod sha256;
pub mod store;
pub mod types;
pub mod util;

use serde::{Deserialize, Serialize};
use uint::construct_uint;

construct_uint! {
    // construct an unsigned 256-bit integer consisting of 4 x 64-bit words
    #[derive(Serialize, Deserialize)]
    pub struct U256(4);
}

// initial reward in bitcoin - multiply by 10*S*8 to get satoshis
pub const INITIAL_REWARD: u64 = 50;
// halving interval in blocks -- 7875, not the original 210. Scaled up by
// the same factor IDEAL_BLOCK_TIME was scaled down by (600s -> 16s), so
// the ~21,000-coin hard cap (INITIAL_REWARD * HALVING_INTERVAL, see the
// reconciliation write-up) still takes ~48 days of real time to mint out,
// unchanged from before -- confirmation speed and supply-exhaustion speed
// are deliberately decoupled rather than both riding on block time
// together (previously 210 at 600s/block already gave ~48 days; 210 at
// 16s/block alone would exhaust the whole cap in a couple of days instead,
// permanently zeroing mining income far sooner than the project's own
// economics were designed around).
pub const HALVING_INTERVAL: u64 = 7875;

/// The coinbase reward at `height`, in base units: `INITIAL_REWARD`
/// halved once per `HALVING_INTERVAL`, and zero from the 33rd halving
/// on, since 50 * 10^8 < 2^33.
///
/// One function for both sides of the chain, because there were two.
/// Block verification divided by `2u64.pow(halvings)`, which overflows
/// at the 64th halving -- height 504,000, about 93 days in at 16-second
/// blocks -- and panicked; the template shifted by `halvings`, which
/// the same height overflows the other way. From that block on nothing
/// could be verified, so the chain could accept no block at all. A
/// shift past the width is simply zero here, which is what the reward
/// has been for thirty-one halvings by then anyway.
pub fn block_reward_at_height(height: u64) -> u64 {
    let halvings = height / HALVING_INTERVAL;
    if halvings >= u64::BITS as u64 {
        return 0;
    }
    (INITIAL_REWARD * 10u64.pow(8)) >> halvings
}
// Ideal block time in seconds -- 16, not 600 (Bitcoin's own value) and not
// the rounder-looking 15: IDEAL_BLOCK_TIME * DIFFICULTY_UPDATE_INTERVAL
// needs to be evenly divisible by 4 for `adjust_target`'s clamp-to-1/4
// bound (an integer division) to land on an exact ratio -- 16*50=800 does,
// 15*50=750 doesn't, which surfaced as the previously-exact
// adjust_target_clamps_a_much_faster_than_ideal_window_to_one_quarter test
// failing by a ~0.27% rounding artifact once this changed from 600 (a
// clean multiple of 4 at any DIFFICULTY_UPDATE_INTERVAL). Public agents
// interacting with the hub (deposits, faucet claims, escrow confirmations)
// feel every second of this directly, so favors fast confirmation over
// Bitcoin-style security margins, which don't matter the same way here:
// this is a closed-loop, single-miner testnet economy, not a chain
// defending real value against a 51% attack. See HALVING_INTERVAL's own
// comment for how supply exhaustion timing is kept unchanged despite
// this. Several hub TTLs are minute-scale (e.g.
// handlers::ESCROW_RESERVATION_TTL_MINUTES = 60) and need several
// confirmations' worth of margin inside their window, not just one, to
// keep a normal deposit-then-confirm flow from being timing-sensitive by
// accident -- 16s still gives 60+ confirmations inside every such window,
// far more margin than 600s ever did.
pub const IDEAL_BLOCK_TIME: u64 = 16;
// minimum target
pub const MIN_TARGET: U256 = U256([
    0xFFFF_FFFF_FFFF_FFFF,
    0xFFFF_FFFF_FFFF_FFFF,
    0xFFFF_FFFF_FFFF_FFFF,
    0x0000_FFFF_FFFF_FFFF,
]);
//difficulty update interval in blocks
pub const DIFFICULTY_UPDATE_INTERVAL: u64 = 50;

// maximum time a transaction may stay in the mempool
pub const MAX_MEMPOOL_TRANSACTION_AGE: u64 = 600;

//DEF: the difficulty is how unlikely, roughly, it should be to encounter the correct hash while a node is mining

// maximum serialized size (in bytes) of the transactions included in a
// mined block, not counting the coinbase transaction. Chosen to mirror
// Bitcoin's original 1MB block size limit.
pub const BLOCK_BYTE_CAP: usize = 1_000_000;

// magic bytes exchanged during the peer handshake so nodes/miners/wallets
// refuse to talk to something that isn't speaking this protocol
pub const PROTOCOL_MAGIC: u32 = 0x49_54_58_00;
// bump this whenever the wire protocol changes in an incompatible way.
// v2: Hello/HelloAck gained a timestamp field for network time-offset
// sampling.
// v3: FetchBlock/NewBlock (one block per round trip) replaced by
// FetchBlocks/Blocks (a batch per round trip) for chain sync.
pub const PROTOCOL_VERSION: u32 = 3;

// upper bound on a single length-prefixed wire message. Well above
// BLOCK_BYTE_CAP to leave headroom for e.g. large UTXO-set responses,
// while still rejecting a peer that sends a bogus multi-gigabyte length
// prefix before we ever allocate a buffer for it.
pub const MAX_MESSAGE_SIZE: usize = 10 * 1024 * 1024;

// upper bound on how many blocks a single FetchBlocks reply may contain,
// regardless of how many were requested -- the responder enforces this
// itself rather than trusting the requester's count, and the requester
// must ask for no more than this per round trip too, so that "fewer
// blocks came back than requested" can only mean this cap, never an
// ambiguous mix of two independently-chosen numbers.
pub const BLOCKS_PER_FETCH_BATCH: usize = 8;
// safety margin under MAX_MESSAGE_SIZE for one FetchBlocks reply's total
// serialized size. Coinbase transaction size isn't consensus-bounded
// (only non-coinbase transactions are, via BLOCK_BYTE_CAP), so this is a
// belt-and-suspenders running-total check during batch assembly, not just
// a count cap.
pub const MAX_FETCH_BATCH_BYTES: usize = 9 * 1024 * 1024;

// how far ahead of our own clock a block's timestamp is allowed to be
// before we consider it invalid. Mirrors Bitcoin's 2-hour rule; without
// some bound, a block claiming to be from the year 3000 would be accepted
// as long as it's otherwise valid, corrupting future difficulty-adjustment
// math (which relies on timestamps being roughly honest).
pub const MAX_FUTURE_BLOCK_DRIFT_SECONDS: i64 = 2 * 60 * 60;

// how many blocks deep a side branch's fork point must be, relative to
// the current tip, before we consider a reorg back to it realistically
// impossible and prune it from memory/storage. Twice the difficulty
// retarget interval, so pruning never runs ahead of what a single
// retarget window could plausibly still reorganize.
pub const SIDE_BRANCH_PRUNE_DEPTH: u64 = 2 * DIFFICULTY_UPDATE_INTERVAL;

/// Converts a PoW target into the amount of expected work required to
/// find a hash meeting it, so that chains can be compared by cumulative
/// work rather than simply by block count (a chain with more, easier
/// blocks is not necessarily "more work" than one with fewer, harder
/// blocks). Mirrors Bitcoin Core's GetBlockProof.
pub fn work_from_target(target: U256) -> U256 {
    let max = U256::max_value();
    // work = 2^256 / (target + 1), computed without overflowing U256 by
    // using (~target) = (2^256 - 1 - target) in place of (2^256 - target)
    (max - target) / (target + 1) + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harder_target_means_more_work() {
        let easy = MIN_TARGET;
        let hard = MIN_TARGET / 100;
        assert!(work_from_target(hard) > work_from_target(easy));
    }

    #[test]
    fn min_target_has_positive_work() {
        assert!(work_from_target(MIN_TARGET) > U256::zero());
    }

    #[test]
    fn the_block_reward_halves_and_then_stays_at_zero_past_the_shift_width() {
        let full = INITIAL_REWARD * 10u64.pow(8);
        assert_eq!(block_reward_at_height(0), full);
        assert_eq!(block_reward_at_height(HALVING_INTERVAL - 1), full);
        assert_eq!(block_reward_at_height(HALVING_INTERVAL), full / 2);
        assert_eq!(block_reward_at_height(32 * HALVING_INTERVAL), 1);
        assert_eq!(block_reward_at_height(33 * HALVING_INTERVAL), 0);
        // The 64th halving: `2u64.pow(64)` and `x >> 64` both overflow.
        assert_eq!(block_reward_at_height(64 * HALVING_INTERVAL), 0);
        assert_eq!(block_reward_at_height(u64::MAX), 0);
    }
}

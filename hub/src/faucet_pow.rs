//! The faucet's proof-of-work challenge: a server-issued puzzle a key
//! must solve before it can be granted coins.
//!
//! Specified in §5 of `docs/agent-ecosystem-plan.md`. The shape is
//! deliberately the one the chain already uses -- find an input whose
//! SHA-256 hash is numerically at or below a target -- so that an agent
//! author who has read anything about mining recognises it, and so that
//! this reuses `btclib`'s `Hash` and `U256` rather than inventing a
//! second notion of difficulty.
//!
//! # What the puzzle is for
//!
//! Keygen is free, so a pubkey is not an identity and the faucet is the
//! one place the hub hands value to an unproven one. The work is not a
//! wall -- SHA-256 is embarrassingly parallel and GPU-friendly, so a
//! determined attacker buys a large speedup -- it is a price. The goal
//! is that the cost of assembling many funded keys exceeds what can be
//! extracted from them, with the cluster caps of §4 bounding the
//! aggregate and the difficulty knob available as an emergency brake
//! during an active attack.
//!
//! # The preimage, and why it is a plain string
//!
//! ```text
//! "{challenge_id}:{server_nonce_hex}:{pubkey_hex}:{action}:{solution}"
//! ```
//!
//! Hashed with `Hash::hash_bytes`, which takes raw bytes rather than
//! CBOR, for the same reason the signed envelope's signing string does:
//! a client in any language must be able to reproduce it exactly, and
//! very few of them have a CBOR library to hand. Every field is already
//! text on the wire.
//!
//! Each field is load-bearing:
//!
//! - `challenge_id` and `server_nonce` make the puzzle this issuance and
//!   no other. The nonce is 32 fresh random bytes, so nothing can be
//!   precomputed before the hub speaks.
//! - `pubkey` binds the work to the key that will be paid. A solution
//!   found for one key is worthless to another, which is what stops a
//!   single miner farming solutions and selling them to a swarm. The
//!   challenge record carries the same pubkey, so the binding is checked
//!   twice: once in the hash and once against the redeeming envelope.
//! - `action` is a domain separator. Nothing else is priced by
//!   proof-of-work today -- task posting is priced by escrow -- but the
//!   moment something is, a faucet solution must not be spendable on it.
//!   Cheaper to include now than to migrate later.
//! - `solution` is the only free variable, and it is a `u64`, so the
//!   search space is far larger than any reachable difficulty.
//!
//! # Comparing a hash to a target
//!
//! `Hash::hash_bytes` reads the 32 digest bytes as a **little-endian**
//! `U256`, and `matches_target` is `hash <= target`. A client in another
//! language must do the same, which in Python is
//! `int.from_bytes(sha256(preimage).digest(), "little") <= target`. This
//! is easy to get backwards and produces a puzzle that is merely
//! different rather than obviously broken, so `target_hex` exists to
//! hand the client one unambiguous number and
//! `the_wire_target_compares_the_way_a_client_would` pins the
//! convention.

use btclib::crypto::PublicKey;
use btclib::sha256::Hash;
use btclib::U256;
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::*;
use uuid::Uuid;

/// The only action priced by proof-of-work today. Lives in the preimage
/// and on the record; see the module docs on domain separation.
pub const FAUCET_ACTION: &str = "faucet";

/// How long an issued challenge stays redeemable.
///
/// Ten minutes is long enough that a slow client, or one that solves on
/// a phone, is never racing the clock, and short enough that a stock of
/// unsolved challenges is worthless. It bounds the table too: an
/// abandoned challenge is garbage after this, and the sweep collects it.
pub const CHALLENGE_TTL_SECONDS: i64 = 600;

/// How long a redeemed challenge is remembered after it expires.
///
/// The record has to outlive the challenge itself, because its job after
/// redemption is to refuse a second one. Keeping it a further hour past
/// expiry is generous against clock skew between the hub and its own
/// store, and still bounded.
pub const REDEEMED_RETENTION_SECONDS: i64 = 3_600;

/// Expected hashes to solve one challenge at the default difficulty.
///
/// Not a raw target, because a raw target is unreadable and nobody can
/// tell whether one is being tightened or loosened by looking at it.
/// This number is what an operator actually wants to reason about, and
/// `target_for_expected_hashes` turns it into the comparison the code
/// runs.
///
/// Twenty million is calibrated against the client that will actually
/// solve these. Measured on this project's own Python SDK path --
/// `hashlib.sha256` over the 183-byte preimage, one core -- at 1.48
/// million hashes a second, which puts the median solve near fourteen
/// seconds and the slow tail under a minute. That is §5's stated target
/// of ten to sixty seconds. A Rust client is several times faster and
/// will find it trivial, which is fine: the number that matters is the
/// one an honest Python agent waits through.
pub const DEFAULT_EXPECTED_HASHES: u64 = 20_000_000;

/// The target a hash must come in at or below for `expected` hashes of
/// work on average.
///
/// A uniformly distributed 256-bit hash lands at or below `MAX / n` with
/// probability `1/n`, so the expected number of tries is `n`. Clamped at
/// one because a difficulty of zero expected hashes is not a difficulty,
/// and dividing by it would panic.
pub fn target_for_expected_hashes(expected: u64) -> U256 {
    U256::MAX / U256::from(expected.max(1))
}

/// An issued, not-yet-redeemed challenge.
///
/// Stored by value in the durable table and held in memory by
/// `ChallengeBook`. `redeemed_at` is what makes a spent challenge
/// unusable rather than absent: deleting the record on redemption would
/// make a replay look like an unknown challenge, which is the same
/// answer the hub gives to a typo, and an agent debugging its client
/// deserves to be told which of those happened.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Challenge {
    pub id: Uuid,
    /// Hex, matching `PublicKey`'s own string form, so the preimage can
    /// be built without re-encoding anything.
    pub pubkey: String,
    /// 32 random bytes, hex. Fresh per issuance.
    pub server_nonce: String,
    pub action: String,
    pub target: U256,
    pub issued_at: i64,
    pub expires_at: i64,
    /// `None` while outstanding.
    pub redeemed_at: Option<i64>,
}

impl Challenge {
    /// Issues a fresh challenge for `pubkey` at `target`.
    pub fn issue(pubkey: &PublicKey, target: U256, now: DateTime<Utc>) -> Self {
        let mut nonce = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut nonce);
        Challenge {
            id: Uuid::new_v4(),
            pubkey: pubkey.to_string(),
            server_nonce: hex::encode(nonce),
            action: FAUCET_ACTION.to_string(),
            target,
            issued_at: now.timestamp(),
            expires_at: (now + Duration::seconds(CHALLENGE_TTL_SECONDS)).timestamp(),
            redeemed_at: None,
        }
    }

    /// The exact bytes a solver hashes. See the module docs.
    pub fn preimage(&self, solution: u64) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            self.id, self.server_nonce, self.pubkey, self.action, solution
        )
    }

    /// Whether `solution` solves this challenge, against the target
    /// recorded **at issuance** rather than whatever the hub's current
    /// difficulty happens to be. An operator tightening the knob must
    /// not invalidate work an honest agent has already started.
    pub fn is_solved_by(&self, solution: u64) -> bool {
        Hash::hash_bytes(self.preimage(solution).as_bytes()).matches_target(self.target)
    }

    /// The target as a plain big-endian hex integer, which is what goes
    /// on the wire. See the module docs on the comparison convention.
    pub fn target_hex(&self) -> String {
        format!("{:064x}", self.target)
    }

    /// The preimage with the solution left as a literal `{solution}`,
    /// handed to the client so it never has to reconstruct the field
    /// order or the separators itself. Built from the same `format!` as
    /// `preimage`, so the two cannot drift.
    pub fn preimage_template(&self) -> String {
        format!(
            "{}:{}:{}:{}:{{solution}}",
            self.id, self.server_nonce, self.pubkey, self.action
        )
    }

    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        now.timestamp() > self.expires_at
    }

    pub fn is_redeemed(&self) -> bool {
        self.redeemed_at.is_some()
    }

    /// Brute-forces a solution. Test and reference use only -- the hub
    /// never solves its own puzzles. Kept beside the verifier on purpose:
    /// a solver and a checker that disagree is the failure mode worth
    /// designing against, and here they cannot drift apart because the
    /// tests below run both.
    pub fn solve_from(&self, start: u64) -> u64 {
        (start..).find(|n| self.is_solved_by(*n)).expect("u64 is not exhaustible in practice")
    }
}

/// The checks a redemption makes, in one place so `check` and `redeem`
/// cannot drift apart.
///
/// Cheapest-first, which also happens to be most-informative-first:
/// everything except the hash is a field comparison, so a client that
/// got the easy things wrong is told so without the hub hashing
/// anything on its behalf.
fn validate(
    challenge: &Challenge,
    pubkey: &PublicKey,
    solution: u64,
    now: DateTime<Utc>,
) -> std::result::Result<(), RedemptionError> {
    if challenge.pubkey != pubkey.to_string() {
        return Err(RedemptionError::WrongKey);
    }
    if challenge.is_redeemed() {
        return Err(RedemptionError::AlreadyRedeemed);
    }
    if challenge.is_expired_at(now) {
        return Err(RedemptionError::Expired);
    }
    if !challenge.is_solved_by(solution) {
        return Err(RedemptionError::Unsolved);
    }
    Ok(())
}

/// Errors a redemption can fail with, each a distinct thing to tell a
/// client. Kept apart rather than collapsed into one "bad challenge"
/// because an agent debugging its solver needs to know whether it got
/// the arithmetic wrong, waited too long, or is retrying something it
/// already spent.
#[derive(Debug, thiserror::Error)]
pub enum RedemptionError {
    #[error("no such faucet challenge")]
    Unknown,
    #[error("this faucet challenge was issued to a different key")]
    WrongKey,
    #[error("this faucet challenge has expired; request another")]
    Expired,
    #[error("this faucet challenge has already been redeemed")]
    AlreadyRedeemed,
    #[error("that solution does not meet the challenge's target")]
    Unsolved,
    #[error("could not record the redemption durably: {0}")]
    NotRecorded(String),
}

/// The hub's outstanding and recently-redeemed challenges: in memory for
/// the check, on disk so a restart cannot forget a spent one.
///
/// Same two-halves shape as `auth::ReplayGuard`, and for the same
/// reason. A challenge whose redemption lived only in memory would be
/// replayable across a restart, which is precisely the window an
/// attacker who can see the hub go down would wait for.
pub struct ChallengeBook {
    /// Every challenge this hub still holds, redeemed or not.
    by_id: DashMap<Uuid, Challenge>,
    /// The one outstanding challenge per key, if any. Enforcing "one at
    /// a time" is what stops a client accumulating a stock of puzzles to
    /// solve at leisure and redeem in a burst.
    outstanding: DashMap<String, Uuid>,
    store: Option<Arc<crate::store::HubStore>>,
    target: U256,
}

impl ChallengeBook {
    /// Reads back every challenge still worth holding. Anything already
    /// past its useful life is left for the sweep rather than loaded.
    pub fn restore(
        store: Arc<crate::store::HubStore>,
        target: U256,
        now: DateTime<Utc>,
    ) -> std::result::Result<(Self, usize), crate::store::HubStoreError> {
        let book = ChallengeBook {
            by_id: DashMap::new(),
            outstanding: DashMap::new(),
            store: Some(store.clone()),
            target,
        };
        for challenge in store.load_all_faucet_challenges()? {
            if challenge.is_redeemed() {
                if now.timestamp() <= challenge.expires_at + REDEEMED_RETENTION_SECONDS {
                    book.by_id.insert(challenge.id, challenge);
                }
            } else if !challenge.is_expired_at(now) {
                book.outstanding.insert(challenge.pubkey.clone(), challenge.id);
                book.by_id.insert(challenge.id, challenge);
            }
        }
        let restored = book.by_id.len();
        Ok((book, restored))
    }

    /// A book with no durable half, for tests.
    #[cfg(test)]
    pub fn open(target: U256) -> Self {
        ChallengeBook {
            by_id: DashMap::new(),
            outstanding: DashMap::new(),
            store: None,
            target,
        }
    }

    pub fn target(&self) -> U256 {
        self.target
    }

    /// Issues a challenge to `pubkey`, replacing any it already had
    /// outstanding.
    ///
    /// Replacing rather than refusing: a client that lost its first
    /// challenge to a crash or a dropped response would otherwise be
    /// stuck for the full ten minutes, and refusing buys nothing, since
    /// the limit exists to stop a *stock* accumulating and one is not a
    /// stock. The replaced challenge is left in the table until the
    /// sweep takes it, so a solution racing in against the old id gets
    /// `Unknown` rather than silence.
    pub fn issue(
        &self,
        pubkey: &PublicKey,
        now: DateTime<Utc>,
    ) -> std::result::Result<Challenge, RedemptionError> {
        let challenge = Challenge::issue(pubkey, self.target, now);
        if let Some(store) = &self.store {
            store
                .save_faucet_challenge(&challenge)
                .map_err(|e| RedemptionError::NotRecorded(e.to_string()))?;
        }
        if let Some(previous) = self.outstanding.get(&challenge.pubkey).map(|e| *e.value()) {
            self.by_id.remove(&previous);
        }
        self.outstanding.insert(challenge.pubkey.clone(), challenge.id);
        self.by_id.insert(challenge.id, challenge.clone());
        Ok(challenge)
    }

    /// Checks a solution and spends the challenge.
    ///
    /// The durable write happens here, before the caller has paid
    /// anything -- see `HubStore::save_faucet_challenge` for why this
    /// half is ordered before the payout while the grant record is
    /// ordered after it.
    ///
    /// The checks themselves are `validate`'s, shared with `check`.
    pub fn redeem(
        &self,
        id: Uuid,
        pubkey: &PublicKey,
        solution: u64,
        now: DateTime<Utc>,
    ) -> std::result::Result<Challenge, RedemptionError> {
        let mut entry = self.by_id.get_mut(&id).ok_or(RedemptionError::Unknown)?;
        validate(entry.value(), pubkey, solution, now)?;

        let mut redeemed = entry.clone();
        redeemed.redeemed_at = Some(now.timestamp());
        if let Some(store) = &self.store {
            store
                .save_faucet_challenge(&redeemed)
                .map_err(|e| RedemptionError::NotRecorded(e.to_string()))?;
        }
        *entry = redeemed.clone();
        drop(entry);
        self.outstanding.remove(&redeemed.pubkey);
        Ok(redeemed)
    }

    /// Everything `redeem` checks, spending nothing.
    ///
    /// The faucet asks this before it asks whether it can afford the
    /// grant, so that a caller presenting a spent or unsolved challenge
    /// is told *that*, rather than being told the hub is busy because
    /// the two answers happened to be available in the wrong order.
    ///
    /// Advisory, and safe to be: `redeem` runs the same list again
    /// under its own exclusive reference, so nothing here is trusted
    /// twice. The cost of the second pass is one SHA-256.
    pub fn check(
        &self,
        id: Uuid,
        pubkey: &PublicKey,
        solution: u64,
        now: DateTime<Utc>,
    ) -> std::result::Result<(), RedemptionError> {
        let entry = self.by_id.get(&id).ok_or(RedemptionError::Unknown)?;
        validate(entry.value(), pubkey, solution, now)
    }

    /// Drops what can no longer matter, from memory and from disk on one
    /// horizon. Called from the sweep; without it the table grows by a
    /// row per challenge ever issued.
    pub fn cleanup(&self, now: DateTime<Utc>) {
        let ts = now.timestamp();
        self.by_id.retain(|_, challenge| {
            let horizon = if challenge.is_redeemed() {
                challenge.expires_at + REDEEMED_RETENTION_SECONDS
            } else {
                challenge.expires_at
            };
            ts <= horizon
        });
        self.outstanding.retain(|_, id| self.by_id.contains_key(id));
        if let Some(store) = &self.store {
            match store.prune_faucet_challenges(ts) {
                Ok(n) if n > 0 => debug!("sweep: pruned {n} expired faucet challenge(s)"),
                Ok(_) => {}
                Err(e) => warn!("sweep: could not prune faucet challenges: {e}"),
            }
        }
    }

    #[cfg(test)]
    pub fn outstanding_for(&self, pubkey: &PublicKey) -> Option<Uuid> {
        self.outstanding.get(&pubkey.to_string()).map(|e| *e.value())
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.by_id.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use btclib::crypto::PrivateKey;

    /// Easy enough that tests solve instantly, hard enough that the
    /// first nonce tried is not a hit.
    fn easy() -> U256 {
        target_for_expected_hashes(64)
    }

    fn challenge_for(key: &PrivateKey) -> Challenge {
        Challenge::issue(&key.public_key(), easy(), Utc::now())
    }

    /// Asserts that `variant` is a genuinely different puzzle from
    /// `original`: work done on one buys nothing on the other.
    ///
    /// Two assertions, because "does not carry" is not a deterministic
    /// property at a test difficulty. At `easy()`'s one-in-sixty-four,
    /// *any* number solves *any* challenge one time in sixty-four, so
    /// the single-solution version of this failed on a perfectly
    /// healthy scheme at that rate -- which is what made
    /// `a_solution_does_not_carry_across_issuances`, with two such
    /// assertions, fail about one module run in twenty-five (measured
    /// over 25 runs). It was never the scheme; the test was sampling
    /// its own difficulty.
    ///
    /// Tightening the target was the obvious fix and is wrong: solving
    /// at one in a hundred thousand costs eleven seconds in a debug
    /// build, and at one in a million, twenty-nine. Measured.
    ///
    /// So the mechanism is asserted deterministically -- the changed
    /// field is inside the hash, so the preimages hash differently --
    /// and the consequence is asserted over several independent
    /// solutions. A scheme that left the field out of the preimage
    /// carries every solution and fails both halves; a healthy one
    /// fails the second with probability 64^-3, about four in a
    /// million.
    fn assert_distinct_puzzles(original: &Challenge, variant: &Challenge) {
        let mut solutions = Vec::new();
        let mut from = 0;
        for _ in 0..3 {
            let solution = original.solve_from(from);
            from = solution + 1;
            solutions.push(solution);
        }

        for solution in &solutions {
            assert_ne!(
                Hash::hash_bytes(variant.preimage(*solution).as_bytes()),
                Hash::hash_bytes(original.preimage(*solution).as_bytes()),
                "the changed field must be inside the hash, or it separates nothing"
            );
        }
        assert!(
            !solutions.iter().all(|solution| variant.is_solved_by(*solution)),
            "work done on one puzzle must not carry wholesale to the other"
        );
    }

    #[test]
    fn a_solution_verifies_against_the_challenge_it_was_found_for() {
        let key = PrivateKey::new_key();
        let challenge = challenge_for(&key);
        let solution = challenge.solve_from(0);
        assert!(challenge.is_solved_by(solution));
    }

    /// The property the whole scheme rests on: work done for one key
    /// buys nothing for another. Without the pubkey in the preimage, one
    /// miner could farm solutions and hand them to a swarm.
    #[test]
    fn a_solution_for_one_key_does_not_solve_another_keys_challenge() {
        let alice = PrivateKey::new_key();
        let bob = PrivateKey::new_key();
        let for_alice = challenge_for(&alice);

        // Bob's challenge, made identical in every respect the hub
        // controls, so the pubkey is the only difference left.
        let mut for_bob = for_alice.clone();
        for_bob.pubkey = bob.public_key().to_string();
        assert_distinct_puzzles(&for_alice, &for_bob);
    }

    /// The domain separator earns its place: the same key, the same
    /// nonce, a different action is a different puzzle.
    #[test]
    fn a_solution_does_not_carry_across_actions() {
        let key = PrivateKey::new_key();
        let faucet = challenge_for(&key);

        let mut other = faucet.clone();
        other.action = "some-later-action".to_string();
        assert_distinct_puzzles(&faucet, &other);
    }

    #[test]
    fn a_solution_does_not_carry_across_issuances() {
        let key = PrivateKey::new_key();
        let first = challenge_for(&key);

        // The nonce makes each issuance its own puzzle...
        let mut second = first.clone();
        second.server_nonce = hex::encode([7u8; 32]);
        assert_distinct_puzzles(&first, &second);

        // ...and so does the id.
        let mut third = first.clone();
        third.id = Uuid::new_v4();
        assert_distinct_puzzles(&first, &third);
    }

    /// Difficulty has to move monotonically in the direction an operator
    /// expects, or the knob is a trap.
    #[test]
    fn asking_for_more_work_produces_a_tighter_target() {
        let easy = target_for_expected_hashes(1_000);
        let hard = target_for_expected_hashes(1_000_000);
        assert!(hard < easy, "more expected hashes must mean a smaller target");
        assert_eq!(target_for_expected_hashes(0), target_for_expected_hashes(1), "zero is clamped");
    }

    /// Roughly the promised amount of work, which is the claim the
    /// operator-facing knob makes. Sampled rather than asserted tightly:
    /// this is a geometric distribution and a strict bound would be a
    /// flaky test. One in eight to eight times the target is loose enough
    /// never to fire by chance and tight enough to catch an
    /// off-by-orders-of-magnitude error in `target_for_expected_hashes`.
    #[test]
    fn the_expected_work_is_about_what_was_asked_for() {
        let key = PrivateKey::new_key();
        let expected = 2_000u64;
        let mut total = 0u64;
        let rounds = 20;
        for _ in 0..rounds {
            let challenge =
                Challenge::issue(&key.public_key(), target_for_expected_hashes(expected), Utc::now());
            total += challenge.solve_from(0) + 1;
        }
        let mean = total / rounds;
        assert!(
            mean > expected / 8 && mean < expected * 8,
            "mean of {mean} tries is not in the right order of magnitude for {expected}"
        );
    }

    /// The convention a non-Rust client has to reproduce, pinned here so
    /// that changing `Hash`'s byte order breaks this rather than breaking
    /// every SDK silently. The right-hand side is what the Python client
    /// literally does.
    #[test]
    fn the_wire_target_compares_the_way_a_client_would() {
        let key = PrivateKey::new_key();
        let challenge = challenge_for(&key);
        let solution = challenge.solve_from(0);

        let digest = <[u8; 32]>::try_from(
            hex::decode(sha256::digest(challenge.preimage(solution).as_bytes())).unwrap(),
        )
        .unwrap();
        let as_client_sees_it = U256::from_little_endian(&digest);
        let target_from_wire = U256::from_str_radix(&challenge.target_hex(), 16).unwrap();

        assert!(
            as_client_sees_it <= target_from_wire,
            "a client reading the digest little-endian must agree with the hub"
        );
        assert_eq!(target_from_wire, challenge.target, "the hex target must round-trip");
    }

    #[test]
    fn expiry_and_redemption_are_reported_from_the_record() {
        let key = PrivateKey::new_key();
        let now = Utc::now();
        let mut challenge = Challenge::issue(&key.public_key(), easy(), now);
        assert!(!challenge.is_expired_at(now));
        assert!(!challenge.is_expired_at(now + Duration::seconds(CHALLENGE_TTL_SECONDS)));
        assert!(challenge.is_expired_at(now + Duration::seconds(CHALLENGE_TTL_SECONDS + 1)));

        assert!(!challenge.is_redeemed());
        challenge.redeemed_at = Some(now.timestamp());
        assert!(challenge.is_redeemed());
    }
}

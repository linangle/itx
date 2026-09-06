use tracing::*;

use crate::store::{HubStore, HubStoreError};
use btclib::crypto::PublicKey;
pub use btclib::envelope::{EnvelopeError as AuthError, SignedEnvelope};
use btclib::envelope::MAX_REQUEST_DRIFT_SECONDS;
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use serde::{de::DeserializeOwned, Serialize};
use std::sync::Arc;

/// The signed-envelope replay guard: the set of signatures this hub has
/// already accepted, held in memory for the check and on disk so that
/// the check survives the process.
///
/// Both halves are scoped to one hub instance rather than to the process.
/// The in-memory set used to be a `#[dynamic] static`, which was sound on
/// its own -- a signature is unique per request by construction, so two
/// hubs in one process can never collide on one, the argument
/// `rate_limit::RateLimitTable`'s doc comment makes in reverse for client
/// IPs. It stops being sound the moment the set has a durable twin: the
/// twin is one specific store file, and a memory set shared by two hubs
/// with two different stores would answer "already seen" for a signature
/// that this hub's store has never heard of. Memory and disk have to
/// describe the same hub, so they live together here.
///
/// # What the durable half is for
///
/// The in-memory set alone starts empty on every boot. For
/// `MAX_REQUEST_DRIFT_SECONDS` after a restart, every envelope the hub
/// accepted just before it went down is still inside its own drift
/// window and verifies a second time -- an ordinary deploy is enough, no
/// crash needed. `restore` refills the set from disk so the window never
/// reopens, and `booting` is the fallback for a guard with no store to
/// restore from: refuse authenticated requests until the newest possible
/// pre-restart envelope has aged out of the drift check
/// `verify_signature` already applies. Every read route is
/// unauthenticated, so that fallback degrades writes rather than taking
/// the hub dark.
///
/// # What it is not
///
/// Not shared state between two live hubs. redb is a single-process
/// embedded store, so a second hub instance cannot open this file at all,
/// let alone see these writes -- horizontal scaling still needs the guard
/// moved to something several processes can read (plan §11). This closes
/// the restart hole; it does not lift the single-instance ceiling.
pub struct ReplayGuard {
    seen: DashMap<Vec<u8>, DateTime<Utc>>,
    /// The durable twin of `seen`, or `None` for a guard that keeps no
    /// record across restarts and falls back to `accepting_from`.
    store: Option<Arc<HubStore>>,
    /// When this instance starts accepting authenticated requests, or
    /// `None` when there is no restart window left to wait out.
    accepting_from: Option<DateTime<Utc>>,
}

impl ReplayGuard {
    /// Reads back every signature the previous process accepted that is
    /// still replayable, and returns a guard with no restart window --
    /// there is nothing to wait out, because nothing was forgotten.
    /// Reports how many came back so the caller can log it alongside the
    /// rest of the restored state.
    ///
    /// Signatures older than the drift window are left on disk for the
    /// sweep rather than loaded: they already fail `verify_signature` on
    /// their own, so restoring them would cost memory and buy nothing.
    pub fn restore(
        store: Arc<HubStore>,
        now: DateTime<Utc>,
    ) -> std::result::Result<(Self, usize), HubStoreError> {
        let cutoff = now - Duration::seconds(MAX_REQUEST_DRIFT_SECONDS);
        let recent = store.load_recent_signatures(cutoff.timestamp())?;
        let seen = DashMap::new();
        for (signature, seen_at_unix) in &recent {
            if let Some(seen_at) = DateTime::from_timestamp(*seen_at_unix, 0) {
                seen.insert(signature.clone(), seen_at);
            }
        }
        let restored = seen.len();
        Ok((
            Self { seen, store: Some(store), accepting_from: None },
            restored,
        ))
    }

    /// A guard that keeps no durable record, for a hub that has just
    /// started and therefore lost whatever the previous process had
    /// seen: refuses authenticated requests until
    /// `started_at + MAX_REQUEST_DRIFT_SECONDS`. The fallback when
    /// `restore` fails -- a hub that cannot read its replay log should
    /// still come up, just not with a hole in it.
    pub fn booting(started_at: DateTime<Utc>) -> Self {
        Self {
            seen: DashMap::new(),
            store: None,
            accepting_from: Some(started_at + Duration::seconds(MAX_REQUEST_DRIFT_SECONDS)),
        }
    }

    /// A guard with neither a durable record nor a restart window. For a
    /// hub with no earlier self whose envelopes could be replayed against
    /// it -- which in practice means a test hub, since making every hub
    /// in the suite sit out two minutes to prove `booting` works would be
    /// absurd. `booting` and `restore` are covered directly in this
    /// module's tests instead.
    pub fn open() -> Self {
        Self { seen: DashMap::new(), store: None, accepting_from: None }
    }

    /// Whether `now` is still inside this instance's restart window.
    /// Split out from `verify` so the window can be tested against a
    /// simulated clock rather than a real two-minute wait, the same
    /// reason `verify_signature` takes `now` as a parameter.
    fn refuses_at(&self, now: DateTime<Utc>) -> bool {
        self.accepting_from.is_some_and(|from| now < from)
    }

    /// Claims `signature` for the first time, or reports that it has
    /// already been used. The durable write happens here, before the
    /// caller has done anything with the request -- see
    /// `HubStore::record_seen_signature` for why that ordering is the
    /// whole point.
    ///
    /// A failed durable write leaves the in-memory claim in place and
    /// fails the request. That burns the envelope, which is the safe
    /// direction to fail: the request never ran, and the alternative --
    /// releasing the claim so the same envelope can be tried again --
    /// hands back exactly the replayable envelope this is here to
    /// prevent, at the moment the hub has proven it cannot record one.
    fn claim(&self, signature: Vec<u8>, now: DateTime<Utc>) -> Result<(), AuthError> {
        if self.seen.insert(signature.clone(), now).is_some() {
            return Err(AuthError::Replayed);
        }
        if let Some(store) = &self.store {
            if let Err(e) = store.record_seen_signature(&signature, now.timestamp()) {
                error!("replay guard could not record a signature durably: {e}");
                return Err(AuthError::GuardUnavailable(e.to_string()));
            }
        }
        Ok(())
    }

    /// Evicts entries old enough that their originating request would
    /// already fail the clock-drift check on its own -- from memory and
    /// from disk, on one cutoff, so the two halves cannot drift apart.
    /// Call periodically from the sweep loop; without it the durable
    /// table grows by a row per authenticated request forever.
    pub fn cleanup(&self, now: DateTime<Utc>) {
        let cutoff = now - Duration::seconds(MAX_REQUEST_DRIFT_SECONDS);
        cleanup_replay_guard(&self.seen, cutoff);
        if let Some(store) = &self.store {
            // Logged rather than propagated: a sweep that cannot prune
            // is a growing table, not an unsafe one, and the sweep loop
            // has no caller to report to.
            match store.prune_seen_signatures(cutoff.timestamp()) {
                Ok(pruned) if pruned > 0 => debug!("sweep: pruned {pruned} expired replay-guard signature(s)"),
                Ok(_) => {}
                Err(e) => warn!("sweep: could not prune the durable replay guard: {e}"),
            }
        }
    }
}

/// Evicts every entry last seen at or before `cutoff`. Split out from
/// `ReplayGuard::cleanup` so the in-memory eviction rule stays one
/// readable line next to the durable one it has to agree with.
fn cleanup_replay_guard(seen: &DashMap<Vec<u8>, DateTime<Utc>>, cutoff: DateTime<Utc>) {
    seen.retain(|_, seen_at| *seen_at > cutoff);
}

/// Hub-side verification: the shared signature/timestamp recipe (see
/// `btclib::envelope::SignedEnvelope::verify_signature`) plus a replay
/// check against the caller's `ReplayGuard` -- the one part of
/// verification that's inherently server-side state, not something a
/// client SDK needs or could share. Kept as an extension trait (rather
/// than moving the whole thing into btclib) so every existing
/// `envelope.verify()` call site in `handlers.rs` needed no changes
/// beyond importing this trait and passing the hub's guard.
pub trait VerifyEnvelope {
    fn verify(
        &self,
        guard: &ReplayGuard,
        method: &str,
        path: &str,
    ) -> Result<PublicKey, AuthError>;
}

impl<T: Serialize + DeserializeOwned> VerifyEnvelope for SignedEnvelope<T> {
    /// `method`/`path` must come from the request as it actually arrived
    /// -- `axum::http::Method` and `OriginalUri`'s path -- never from
    /// anything the handler knows about its own route. A handler passing
    /// the route it *expects* rather than the one it got would rebuild
    /// the endpoint-confusion gap that binding them closed.
    fn verify(
        &self,
        guard: &ReplayGuard,
        method: &str,
        path: &str,
    ) -> Result<PublicKey, AuthError> {
        let now = Utc::now();

        // Before the ECDSA verify, not after: this is a constant-time
        // field read, and §3.4 of the ecosystem plan asks that the cheap
        // checks run first so a flood costs the attacker more than it
        // costs us.
        if guard.refuses_at(now) {
            debug!("refusing an authenticated request inside the post-restart replay window");
            return Err(AuthError::GuardWarmingUp);
        }

        let pubkey = self.verify_signature(now, method, path)?;

        // Only claim the signature once it's confirmed genuine --
        // otherwise anyone could burn arbitrary signature slots (and,
        // now, arbitrary disk writes) with junk bytes. A real signature
        // is unforgeable, so this can only ever be claimed by whoever
        // actually holds the private key. The hex decode below is already
        // known to succeed -- verify_signature just decoded this exact
        // same field.
        let signature_bytes = hex::decode(&self.signature)
            .expect("verify_signature already validated this hex string");
        guard.claim(signature_bytes, now)?;

        Ok(pubkey)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use btclib::crypto::PrivateKey;

    fn temp_store() -> (Arc<HubStore>, std::path::PathBuf) {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir()
            .join(format!("itx_replay_guard_test_{}_{n}.redb", std::process::id()));
        let store = Arc::new(HubStore::open_or_create(&path).unwrap());
        (store, path)
    }

    #[test]
    fn a_booting_guard_refuses_until_the_last_pre_restart_envelope_has_expired() {
        let boot = Utc::now();
        let guard = ReplayGuard::booting(boot);

        // An envelope signed the instant before the restart is still
        // inside its drift window for exactly this long, so the guard
        // must still be refusing.
        assert!(guard.refuses_at(boot));
        assert!(guard.refuses_at(boot + Duration::seconds(MAX_REQUEST_DRIFT_SECONDS - 1)));

        // At the boundary that envelope fails `verify_signature`'s own
        // drift check, so there is nothing left for the window to
        // protect and the hub can serve writes again.
        assert!(!guard.refuses_at(boot + Duration::seconds(MAX_REQUEST_DRIFT_SECONDS)));
        assert!(!guard.refuses_at(boot + Duration::seconds(MAX_REQUEST_DRIFT_SECONDS + 1)));
    }

    #[test]
    fn an_open_guard_never_refuses() {
        let guard = ReplayGuard::open();
        assert!(!guard.refuses_at(Utc::now()));
        assert!(!guard.refuses_at(Utc::now() - Duration::days(365)));
    }

    #[test]
    fn the_window_is_reported_as_its_own_error_not_as_a_replay() {
        let key = PrivateKey::new_key();
        let envelope = SignedEnvelope::new(&key, "POST", "/faucet", ());
        let guard = ReplayGuard::booting(Utc::now());
        assert!(matches!(envelope.verify(&guard, "POST", "/faucet"), Err(AuthError::GuardWarmingUp)));

        // ...and the same envelope sails through once the window closes,
        // proving the refusal was the window and nothing else.
        let guard = ReplayGuard::open();
        assert_eq!(envelope.verify(&guard, "POST", "/faucet").unwrap(), key.public_key());
    }

    /// The hole this whole module exists to close: an envelope accepted
    /// by one process must not be accepted again by its replacement.
    /// "Restart" here is a second guard restored from the same store,
    /// with its own empty in-memory set -- exactly what a real restart
    /// produces.
    #[test]
    fn a_replayed_envelope_is_still_rejected_after_a_restart() {
        let (store, path) = temp_store();
        let key = PrivateKey::new_key();
        let envelope = SignedEnvelope::new(&key, "POST", "/faucet", ());

        let (before, restored) = ReplayGuard::restore(store.clone(), Utc::now()).unwrap();
        assert_eq!(restored, 0, "a fresh store has nothing to restore");
        assert_eq!(envelope.verify(&before, "POST", "/faucet").unwrap(), key.public_key());
        drop(before);

        let (after, restored) = ReplayGuard::restore(store.clone(), Utc::now()).unwrap();
        assert_eq!(restored, 1, "the accepted signature must come back from disk");
        assert!(
            matches!(envelope.verify(&after, "POST", "/faucet"), Err(AuthError::Replayed)),
            "an envelope accepted before the restart must not be accepted after it"
        );

        // A restored guard has no window to wait out -- that is the
        // whole point of paying for the durable write.
        assert!(!after.refuses_at(Utc::now()));

        drop(after);
        std::fs::remove_file(&path).ok();
    }

    /// Restoring must not resurrect signatures that can no longer be
    /// replayed anyway: they would be pure memory, and the fact that
    /// they are *not* loaded is what keeps a restored guard bounded by
    /// the drift window rather than by uptime.
    #[test]
    fn a_restart_does_not_restore_signatures_already_past_the_drift_window() {
        let (store, path) = temp_store();
        let now = Utc::now();
        store.record_seen_signature(b"stale", (now - Duration::seconds(MAX_REQUEST_DRIFT_SECONDS + 1)).timestamp()).unwrap();
        store.record_seen_signature(b"fresh", now.timestamp()).unwrap();

        let (guard, restored) = ReplayGuard::restore(store.clone(), now).unwrap();
        assert_eq!(restored, 1);
        assert!(guard.seen.contains_key(b"fresh".as_slice()));
        assert!(!guard.seen.contains_key(b"stale".as_slice()));

        drop(guard);
        std::fs::remove_file(&path).ok();
    }

    /// The sweep has to reach both halves on one cutoff. If only memory
    /// were swept the table would grow forever; if only the table were
    /// swept a restart would reload what memory had already dropped.
    #[test]
    fn cleanup_evicts_expired_signatures_from_memory_and_from_disk_together() {
        let (store, path) = temp_store();
        let now = Utc::now();
        let stale = now - Duration::seconds(MAX_REQUEST_DRIFT_SECONDS + 1);
        store.record_seen_signature(b"stale", stale.timestamp()).unwrap();
        store.record_seen_signature(b"fresh", now.timestamp()).unwrap();

        // Restored at a moment when both are still recent, so both are
        // in memory and both are on disk before the sweep runs.
        let (guard, restored) = ReplayGuard::restore(store.clone(), stale).unwrap();
        assert_eq!(restored, 2);

        guard.cleanup(now);
        assert!(guard.seen.contains_key(b"fresh".as_slice()));
        assert!(!guard.seen.contains_key(b"stale".as_slice()));
        let on_disk = store.load_recent_signatures(i64::MIN).unwrap();
        assert_eq!(on_disk.len(), 1, "the stale row must be gone from disk too");
        assert_eq!(on_disk[0].0, b"fresh".to_vec());

        drop(guard);
        std::fs::remove_file(&path).ok();
    }

    /// The drift check and the replay check are independent: neither
    /// subsumes the other, and a guard that has never seen a signature
    /// still rejects it for being stale.
    #[test]
    fn a_stale_envelope_is_rejected_by_drift_even_though_the_guard_has_never_seen_it() {
        let (store, path) = temp_store();
        let key = PrivateKey::new_key();
        let stale = SignedEnvelope::new_at(
            &key,
            Utc::now() - Duration::seconds(MAX_REQUEST_DRIFT_SECONDS + 1),
            "POST",
            "/faucet",
            (),
        );

        let (guard, _) = ReplayGuard::restore(store.clone(), Utc::now()).unwrap();
        assert!(matches!(stale.verify(&guard, "POST", "/faucet"), Err(AuthError::ClockDrift)));
        // ...and it was rejected without being recorded, so a drift
        // rejection cannot be used to burn disk.
        assert!(store.load_recent_signatures(i64::MIN).unwrap().is_empty());

        drop(guard);
        std::fs::remove_file(&path).ok();
    }

    /// A guard with no durable store still dedupes within its own
    /// lifetime -- the in-memory check is not conditional on the twin.
    #[test]
    fn an_open_guard_still_rejects_a_replay_within_one_process() {
        let key = PrivateKey::new_key();
        let envelope = SignedEnvelope::new(&key, "POST", "/faucet", ());
        let guard = ReplayGuard::open();
        assert!(envelope.verify(&guard, "POST", "/faucet").is_ok());
        assert!(matches!(envelope.verify(&guard, "POST", "/faucet"), Err(AuthError::Replayed)));
    }
}

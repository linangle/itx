use tracing::*;

use crate::rate_limit::{charge_pubkey, QuotaExceeded};
use crate::store::{HubStore, HubStoreError};
use crate::AppState;
use btclib::crypto::PublicKey;
pub use btclib::envelope::{EnvelopeError as AuthError, SignedEnvelope};
use btclib::envelope::MAX_REQUEST_DRIFT_SECONDS;
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use serde::{de::DeserializeOwned, Serialize};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

/// How long a claimed signature has to be remembered for, counted from
/// the moment this hub accepted it.
///
/// Twice the drift window, and the factor of two is the whole point.
/// `verify_signature` accepts any timestamp within
/// `MAX_REQUEST_DRIFT_SECONDS` of the server clock **in either
/// direction**, because a client's clock may be fast as easily as slow.
/// So an envelope stamped `D` seconds in the future is accepted the
/// instant it arrives and stays acceptable for another `2D` after that:
/// it only fails the drift check once the wall clock passes its
/// timestamp plus `D`.
///
/// Remembering it for only `D`, which is what this used to do, left the
/// last `D` seconds of that life uncovered -- the guard had already
/// forgotten the signature while the drift check would still let it
/// through, so a captured envelope from a fast-clocked client replayed
/// cleanly. Nothing about that requires the attacker to control a clock;
/// it only requires them to capture an envelope from a client whose
/// clock happens to run ahead, which is ordinary.
///
/// The cost of the fix is that the guard holds each signature twice as
/// long, in memory and in the durable table. Both are still bounded by a
/// fixed window rather than by uptime, which is the property that
/// matters.
pub const REPLAY_MEMORY_SECONDS: i64 = 2 * MAX_REQUEST_DRIFT_SECONDS;

/// The instant before which a claimed signature can no longer be
/// replayed, and may therefore be forgotten.
///
/// `REPLAY_MEMORY_SECONDS` back, **and one second further**. The extra
/// second is not slack: `HubStore::record_seen_signature` stores
/// `timestamp()`, which truncates to whole seconds, so a signature
/// claimed at `A` is recorded as `floor(A)` while the envelope behind it
/// stays verifiable until `A + REPLAY_MEMORY_SECONDS`. Comparing the
/// truncated value against an untruncated cutoff therefore forgets it up
/// to a second early, and with a sixty-second sweep a fraction of
/// restored signatures got a sub-second window in which they verified
/// twice.
///
/// Rounding the cutoff down rather than storing milliseconds keeps the
/// stored format a second-resolution unix timestamp, which is what every
/// existing row is. Changing the unit would make every row already on
/// disk read as ancient and be dropped on the next boot -- reopening,
/// once, exactly the window this closes.
///
/// The cost is that signatures are held one second longer than strictly
/// needed. They are still bounded by a fixed window rather than by
/// uptime, which is the property that matters.
fn forget_before(now: DateTime<Utc>) -> DateTime<Utc> {
    now - Duration::seconds(REPLAY_MEMORY_SECONDS + 1)
}

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
    /// Where this guard's counters go. Its own handle for the same reason
    /// `NodeClient`'s is: the guard is constructed before `AppState`
    /// exists, and defaults to a private table so an unwired guard still
    /// functions. `with_metrics` joins it to the one `/metrics` renders.
    metrics: Arc<crate::metrics::Metrics>,
    /// The durable twin of `seen`. `None` only for `open()`, the
    /// test-only constructor: both real constructors carry a store, and
    /// `booting` carrying one is the 2026-09-07 fix -- see its own doc
    /// comment for what a storeless fallback silently cost.
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
    /// Signatures past `REPLAY_MEMORY_SECONDS` are left on disk for the
    /// sweep rather than loaded: they already fail `verify_signature` on
    /// their own, so restoring them would cost memory and buy nothing.
    pub fn restore(
        store: Arc<HubStore>,
        now: DateTime<Utc>,
    ) -> std::result::Result<(Self, usize), HubStoreError> {
        let recent = store.load_recent_signatures(forget_before(now).timestamp())?;
        let seen = DashMap::new();
        for (signature, seen_at_unix) in &recent {
            if let Some(seen_at) = DateTime::from_timestamp(*seen_at_unix, 0) {
                seen.insert(signature.clone(), seen_at);
            }
        }
        let restored = seen.len();
        Ok((
            Self { seen, store: Some(store), accepting_from: None, metrics: crate::metrics::Metrics::new() },
            restored,
        ))
    }

    /// A guard for a hub that has just started and could not read
    /// whatever the previous process had seen: it records durably like
    /// any other, but refuses authenticated requests until
    /// `started_at + REPLAY_MEMORY_SECONDS`. The fallback when `restore`
    /// fails -- a hub that cannot read its replay log should still come
    /// up, just not with a hole in it.
    ///
    /// The window has to be the full memory span, not one drift window:
    /// the newest envelope the previous process could have accepted was
    /// stamped up to `MAX_REQUEST_DRIFT_SECONDS` in the future, and stays
    /// verifiable for another drift window beyond that. Waiting out only
    /// one would reopen the hole this exists to close, for exactly the
    /// envelopes that live longest.
    ///
    /// **It keeps the store, and that is the whole of the 2026-09-07
    /// fix.** This used to be built with `store: None`, and nothing ever
    /// attached one afterwards -- `with_metrics` is the only other
    /// mutator -- so a hub that fell back here refused writes for the
    /// window as documented and then served the rest of its life
    /// recording **nothing durably**.
    ///
    /// The failure was delayed and silent: a transient read error at
    /// boot, a week of apparently healthy running, then an ordinary
    /// restart whose `restore` now succeeds, finds only expired rows and
    /// comes up empty -- and every envelope accepted in the final window
    /// before that restart replays cleanly. That is precisely the hole
    /// this module exists to close, reopened by the code written to
    /// close it, with `replay_durable_write_ms_total` sitting at zero
    /// throughout and the boot banner an operator is told to read
    /// looking entirely normal.
    ///
    /// Keeping the store makes the degradation what it was always
    /// described as: the loss of the previous process's *history*, which
    /// is exactly what the window waits out, rather than the loss of
    /// durability itself. If the store is broken for writes as well as
    /// reads, every claim fails and `verify` answers `GuardUnavailable`
    /// -- a 503 -- which is the honest outcome and a visible one.
    ///
    /// Refusing to start was the other candidate, and it is what
    /// `ChallengeBook::restore` does two blocks further down `main` on a
    /// similar argument. Rejected here because the two failures are not
    /// equivalent: a redeemed challenge this hub cannot see is a solved
    /// puzzle it will accept a second time, with no window that closes
    /// it, while this one is closed by waiting. Turning a transient read
    /// error into a refusal to boot trades a bounded and now-observable
    /// degradation for an outage.
    pub fn booting(store: Arc<HubStore>, started_at: DateTime<Utc>) -> Self {
        Self {
            seen: DashMap::new(),
            store: Some(store),
            accepting_from: Some(started_at + Duration::seconds(REPLAY_MEMORY_SECONDS)),
            metrics: crate::metrics::Metrics::new(),
        }
    }

    /// A guard with neither a durable record nor a restart window. For a
    /// hub with no earlier self whose envelopes could be replayed against
    /// it -- which in practice means a test hub, since making every hub
    /// in the suite sit out two minutes to prove `booting` works would be
    /// absurd. `booting` and `restore` are covered directly in this
    /// module's tests instead.
    pub fn open() -> Self {
        Self { seen: DashMap::new(), store: None, accepting_from: None, metrics: crate::metrics::Metrics::new() }
    }

    /// Points this guard's counters at `metrics`. See
    /// `NodeClient::with_metrics` for why this is a separate step rather
    /// than a constructor argument.
    pub fn with_metrics(mut self, metrics: Arc<crate::metrics::Metrics>) -> Self {
        self.metrics = metrics;
        self
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
    /// Whether this signature has already been claimed, without claiming
    /// it -- a read used to catch a replay *before* it is charged for.
    ///
    /// **Not the authoritative check, and must not be mistaken for one.**
    /// Two identical envelopes arriving together can both pass this and
    /// race; what actually decides between them is the atomic insert in
    /// `claim`, which is why that stays exactly as it is. This only moves
    /// the common case earlier so a detected replay costs the signer
    /// neither quota nor an fsync (§3.4).
    fn already_seen(&self, signature: &[u8]) -> bool {
        self.seen.contains_key(signature)
    }

    fn claim(&self, signature: Vec<u8>, now: DateTime<Utc>) -> Result<(), AuthError> {
        if self.seen.insert(signature.clone(), now).is_some() {
            self.metrics.replay_signatures_rejected.fetch_add(1, Ordering::Relaxed);
            return Err(AuthError::Replayed);
        }
        if let Some(store) = &self.store {
            // Timed, because this fsync is what bounds the hub's whole
            // write path: the load test measured it at fifteen to
            // twenty-two times the ECDSA verify that precedes it
            // (`docs/agent-ecosystem-plan.md` §6.3). A hub that has got
            // slow is far more likely to have a slow disk than a busy
            // CPU, and without this number nothing in the metrics would
            // say so.
            let started = Instant::now();
            let outcome = store.record_seen_signature(&signature, now.timestamp());
            self.metrics
                .replay_durable_write_ms_total
                .fetch_add(started.elapsed().as_millis() as u64, Ordering::Relaxed);
            if let Err(e) = outcome {
                // Counted before the log line, and counted on the failure
                // path first: each of these is a burned envelope and a
                // request the client cannot retry, which makes it the one
                // replay-guard number worth paging on.
                self.metrics.replay_durable_write_failures.fetch_add(1, Ordering::Relaxed);
                error!("replay guard could not record a signature durably: {e}");
                return Err(AuthError::GuardUnavailable(e.to_string()));
            }
        }
        self.metrics.replay_signatures_claimed.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Evicts entries old enough that their originating request would
    /// already fail the clock-drift check on its own -- from memory and
    /// from disk, on one cutoff, so the two halves cannot drift apart.
    /// Call periodically from the sweep loop; without it the durable
    /// table grows by a row per authenticated request forever.
    ///
    /// "Old enough" is `REPLAY_MEMORY_SECONDS`, not one drift window; see
    /// that constant for why the difference is a replay hole rather than
    /// a tuning choice.
    pub fn cleanup(&self, now: DateTime<Utc>) {
        let cutoff = forget_before(now);
        // Measured as a difference rather than counted inside the retain
        // closure: `DashMap::retain` gives no count back, and the
        // before/after length is exact here because `cleanup` is only
        // called from the sweep, which is the one caller that could race
        // it. A concurrent claim would make this off by one, which is a
        // price worth paying to keep the eviction rule one readable line.
        let before = self.seen.len();
        cleanup_replay_guard(&self.seen, cutoff);
        self.metrics
            .replay_signatures_evicted
            .fetch_add(before.saturating_sub(self.seen.len()) as u64, Ordering::Relaxed);
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
    /// Verifies this envelope and charges its key's quota, in the one
    /// order that is correct. Handlers call this and nothing else.
    ///
    /// `method`/`path` must come from the request as it actually arrived
    /// -- `axum::http::Method` and `OriginalUri`'s path -- never from
    /// anything the handler knows about its own route. A handler passing
    /// the route it *expects* rather than the one it got would rebuild
    /// the endpoint-confusion gap that binding them closed.
    fn verify(
        &self,
        state: &AppState,
        method: &str,
        path: &str,
    ) -> Result<PublicKey, VerifyError> {
        self.verify_charging(&state.replay_guard, method, path, |pubkey| {
            charge_pubkey(state, pubkey)
        })
    }

    /// The same checks with no quota charged. Test-only, because a
    /// handler that skipped the charge would be the bug this trait's
    /// shape exists to prevent: there is deliberately no way for
    /// production code to verify an envelope without paying for it.
    #[cfg(test)]
    fn verify_unmetered(
        &self,
        guard: &ReplayGuard,
        method: &str,
        path: &str,
    ) -> Result<PublicKey, AuthError> {
        self.verify_charging(guard, method, path, |_| Ok(())).map_err(|e| match e {
            VerifyError::Auth(e) => e,
            VerifyError::Quota(_) => unreachable!("the unmetered path charges nothing"),
        })
    }

    /// The whole sequence, with the quota charge supplied as a step
    /// rather than left to the caller to remember.
    ///
    /// The ordering is three constraints meeting, and each one rules out
    /// an arrangement that looks fine:
    ///
    /// 1. **The charge comes after the signature check**, because the
    ///    pubkey on an unverified envelope is a string the sender chose.
    ///    Charging first would let anyone burn a victim's quota by
    ///    putting the victim's key on junk requests.
    /// 2. **The charge comes before the claim**, because the claim is
    ///    the fsync (`HubStore::record_seen_signature`) and the quota is
    ///    what bounds how often a key can make the hub fsync. Charging
    ///    afterwards -- which is what the handlers used to do, on their
    ///    own line after this returned -- meant the write always
    ///    happened first and the quota bounded nothing but the handler
    ///    body. A key over its limit still got a disk write per request.
    /// 3. **The claim comes before the handler runs at all**, which is
    ///    `record_seen_signature`'s own rule: the record has to be
    ///    durable before the request takes effect, or a crash in between
    ///    leaves a replayable envelope that has already moved money.
    ///
    /// A request rejected for quota is therefore not claimed, so the
    /// envelope survives and the caller can retry it once its window
    /// rolls over. A replay, by contrast, is charged before it is
    /// detected -- deliberately: a replay flood should cost the key that
    /// sends it.
    fn verify_charging<F>(
        &self,
        guard: &ReplayGuard,
        method: &str,
        path: &str,
        charge: F,
    ) -> Result<PublicKey, VerifyError>
    where
        F: FnOnce(&PublicKey) -> Result<(), QuotaExceeded>;
}

/// Why a signed request was turned away: it failed authentication, or
/// its key is out of budget. One type so a handler can `?` the whole of
/// `verify` on one line, which is what keeps the steps inside it from
/// being reordered or dropped by a call site.
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error(transparent)]
    Quota(#[from] QuotaExceeded),
}

impl<T: Serialize + DeserializeOwned> VerifyEnvelope for SignedEnvelope<T> {
    fn verify_charging<F>(
        &self,
        guard: &ReplayGuard,
        method: &str,
        path: &str,
        charge: F,
    ) -> Result<PublicKey, VerifyError>
    where
        F: FnOnce(&PublicKey) -> Result<(), QuotaExceeded>,
    {
        let now = Utc::now();

        // Before the ECDSA verify, not after: this is a constant-time
        // field read, and §3.4 of the ecosystem plan asks that the cheap
        // checks run first so a flood costs the attacker more than it
        // costs us.
        if guard.refuses_at(now) {
            debug!("refusing an authenticated request inside the post-restart replay window");
            return Err(AuthError::GuardWarmingUp.into());
        }

        let pubkey = self.verify_signature(now, method, path)?;

        // A replay is caught here rather than at the claim below, and the
        // difference is who pays for it. The charge is against the key
        // that *signed* the envelope, so charging first meant anyone
        // holding one captured signed request could spend the signer's
        // whole per-key budget by sending it sixty times -- from one
        // address, comfortably inside that address's own tier -- and lock
        // that agent out of every authenticated route for the window. The
        // comment on the ordering claimed this was prevented; it prevents
        // an attacker *forging* the key, which is a different thing from
        // replaying one they captured. Reading the plaintext off the wire
        // is enough, and `--bind 0.0.0.0` is a supported deployment.
        //
        // Cheap in the ordinary case too: a detected replay now costs
        // neither the quota nor the fsync behind the claim.
        //
        // `claim` still decides. This read cannot settle a race between
        // two identical envelopes in flight together, and is not trying
        // to -- the atomic insert below is what does that, and removing
        // it because this looks redundant would reintroduce the TOCTOU
        // the guard exists to prevent.
        let signature_bytes = hex::decode(&self.signature)
            .expect("verify_signature already validated this hex string");
        if guard.already_seen(&signature_bytes) {
            guard.metrics.replay_signatures_rejected.fetch_add(1, Ordering::Relaxed);
            return Err(AuthError::Replayed.into());
        }

        charge(&pubkey)?;

        // Only claim the signature once it's confirmed genuine --
        // otherwise anyone could burn arbitrary signature slots (and,
        // now, arbitrary disk writes) with junk bytes. A real signature
        // is unforgeable, so this can only ever be claimed by whoever
        // actually holds the private key.
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
        let (store, path) = temp_store();
        let guard = ReplayGuard::booting(store, boot);

        // The envelope that outlives all the others is one the previous
        // process accepted from a client whose clock ran fast: stamped a
        // full drift window in the future, it stays inside the drift
        // check for another one after that. The guard must still be
        // refusing for the whole of that span.
        assert!(guard.refuses_at(boot));
        assert!(guard.refuses_at(boot + Duration::seconds(MAX_REQUEST_DRIFT_SECONDS)));
        assert!(guard.refuses_at(boot + Duration::seconds(REPLAY_MEMORY_SECONDS - 1)));

        // At the boundary even that envelope fails `verify_signature`'s
        // own drift check, so there is nothing left for the window to
        // protect and the hub can serve writes again.
        assert!(!guard.refuses_at(boot + Duration::seconds(REPLAY_MEMORY_SECONDS)));
        assert!(!guard.refuses_at(boot + Duration::seconds(REPLAY_MEMORY_SECONDS + 1)));

        std::fs::remove_file(&path).ok();
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
        let (store, path) = temp_store();
        let guard = ReplayGuard::booting(store, Utc::now());
        assert!(matches!(envelope.verify_unmetered(&guard, "POST", "/faucet"), Err(AuthError::GuardWarmingUp)));
        std::fs::remove_file(&path).ok();

        // ...and the same envelope sails through once the window closes,
        // proving the refusal was the window and nothing else.
        let guard = ReplayGuard::open();
        assert_eq!(envelope.verify_unmetered(&guard, "POST", "/faucet").unwrap(), key.public_key());
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
        assert_eq!(envelope.verify_unmetered(&before, "POST", "/faucet").unwrap(), key.public_key());
        drop(before);

        let (after, restored) = ReplayGuard::restore(store.clone(), Utc::now()).unwrap();
        assert_eq!(restored, 1, "the accepted signature must come back from disk");
        assert!(
            matches!(envelope.verify_unmetered(&after, "POST", "/faucet"), Err(AuthError::Replayed)),
            "an envelope accepted before the restart must not be accepted after it"
        );

        // A restored guard has no window to wait out -- that is the
        // whole point of paying for the durable write.
        assert!(!after.refuses_at(Utc::now()));

        drop(after);
        std::fs::remove_file(&path).ok();
    }

    /// The 2026-09-07 fix, stated as the property it restores: a guard
    /// that fell back to `booting` still writes to disk, so the process
    /// *after* it inherits what this one accepted.
    ///
    /// Before the fix this failed, in the direction that matters.
    /// `booting` built the guard with no store and nothing ever attached
    /// one, so a degraded process recorded nothing for the whole of its
    /// life -- and the hole did not appear until the *next* restart,
    /// when a now-succeeding `restore` came up empty and every envelope
    /// from the degraded process's last window replayed cleanly. Two
    /// restarts away from the transient error that caused it, with
    /// nothing in between saying so.
    ///
    /// The refusal window is not under test here; the test above covers
    /// that. This is about what a degraded guard leaves behind.
    #[test]
    fn a_degraded_guard_still_records_durably_for_its_successor() {
        let (store, path) = temp_store();
        let key = PrivateKey::new_key();
        let envelope = SignedEnvelope::new(&key, "POST", "/faucet", ());

        // Booted far enough in the past that its window has closed, so
        // it is actually serving -- the state the old code spent the
        // rest of the process in.
        let degraded = ReplayGuard::booting(
            store.clone(),
            Utc::now() - Duration::seconds(REPLAY_MEMORY_SECONDS + 1),
        );
        assert!(!degraded.refuses_at(Utc::now()), "the window has closed, so it is serving");
        assert_eq!(envelope.verify_unmetered(&degraded, "POST", "/faucet").unwrap(), key.public_key());
        drop(degraded);

        let (successor, restored) = ReplayGuard::restore(store.clone(), Utc::now()).unwrap();
        assert_eq!(
            restored, 1,
            "the degraded process's signature must be on disk -- with `store: None` it was not, \
             and that is where the replay window silently reopened"
        );
        assert!(
            matches!(envelope.verify_unmetered(&successor, "POST", "/faucet"), Err(AuthError::Replayed)),
            "an envelope accepted by a degraded hub must not be accepted by its successor"
        );

        drop(successor);
        std::fs::remove_file(&path).ok();
    }

    /// Restoring must not resurrect signatures that can no longer be
    /// replayed anyway: they would be pure memory, and the fact that
    /// they are *not* loaded is what keeps a restored guard bounded by
    /// `REPLAY_MEMORY_SECONDS` rather than by uptime.
    ///
    /// The signature one drift window old is the one that matters here.
    /// It is past the drift window and still restored, because the
    /// envelope behind it may have been stamped in the future and may
    /// still verify -- which is precisely what the old one-drift-window
    /// cutoff got wrong.
    #[test]
    fn a_restart_restores_everything_still_replayable_and_nothing_older() {
        let (store, path) = temp_store();
        let now = Utc::now();
        store.record_seen_signature(b"stale", (now - Duration::seconds(REPLAY_MEMORY_SECONDS + 1)).timestamp()).unwrap();
        store.record_seen_signature(b"one_drift_old", (now - Duration::seconds(MAX_REQUEST_DRIFT_SECONDS + 1)).timestamp()).unwrap();
        store.record_seen_signature(b"fresh", now.timestamp()).unwrap();

        let (guard, restored) = ReplayGuard::restore(store.clone(), now).unwrap();
        assert_eq!(restored, 2);
        assert!(guard.seen.contains_key(b"fresh".as_slice()));
        assert!(
            guard.seen.contains_key(b"one_drift_old".as_slice()),
            "a signature past one drift window can still belong to a replayable envelope"
        );
        assert!(!guard.seen.contains_key(b"stale".as_slice()));

        drop(guard);
        std::fs::remove_file(&path).ok();
    }

    /// A signature must outlive the envelope behind it, including the
    /// fraction of a second the stored timestamp throws away.
    ///
    /// `record_seen_signature` stores `timestamp()`, which truncates, so
    /// a signature claimed at `A` is written as `floor(A)` while its
    /// envelope stays verifiable until `A + REPLAY_MEMORY_SECONDS`.
    /// Against an untruncated cutoff it was forgotten up to a second
    /// early, and a restore landing inside that second brought back a
    /// guard that would accept the envelope a second time.
    ///
    /// Constructed at the exact boundary rather than sampled, because a
    /// fractional second is not something a test can wait for reliably:
    /// a row stored at `floor(now) - REPLAY_MEMORY_SECONDS` is precisely
    /// what the old cutoff dropped and the new one keeps.
    #[test]
    fn a_signature_recorded_a_full_window_ago_is_still_restored() {
        let (store, path) = temp_store();
        let now = Utc::now();

        let boundary = now.timestamp() - REPLAY_MEMORY_SECONDS;
        store.record_seen_signature(b"boundary", boundary).unwrap();

        let (guard, _) = ReplayGuard::restore(store.clone(), now).unwrap();
        assert!(
            guard.seen.contains_key(b"boundary".as_slice()),
            "a signature whose stored second is exactly one window old may still belong to a \
             verifiable envelope -- truncation means the real claim was later than the row says"
        );

        // And genuinely forgotten once past it, so this is one extra
        // second rather than an unbounded hold.
        let (later, _) = ReplayGuard::restore(store.clone(), now + Duration::seconds(2)).unwrap();
        assert!(!later.seen.contains_key(b"boundary".as_slice()));

        drop(guard);
        drop(later);
        std::fs::remove_file(&path).ok();
    }

    /// The sweep has to reach both halves on one cutoff. If only memory
    /// were swept the table would grow forever; if only the table were
    /// swept a restart would reload what memory had already dropped.
    #[test]
    fn cleanup_evicts_expired_signatures_from_memory_and_from_disk_together() {
        let (store, path) = temp_store();
        let now = Utc::now();
        let stale = now - Duration::seconds(REPLAY_MEMORY_SECONDS + 1);
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

    /// The hole `REPLAY_MEMORY_SECONDS` closes, driven end to end.
    ///
    /// The envelope here is stamped in the future, which needs no
    /// attacker: a client whose clock runs fast produces one on every
    /// request, and the drift check accepts it precisely so that such
    /// clients work. What makes it dangerous is that it outlives its own
    /// arrival -- the hub accepts it now and it keeps verifying for
    /// nearly two drift windows. A sweep that forgot it after one left
    /// it replayable by anyone who had captured it.
    #[test]
    fn a_future_dated_envelope_stays_remembered_for_as_long_as_it_stays_valid() {
        let key = PrivateKey::new_key();
        let now = Utc::now();
        let envelope = SignedEnvelope::new_at(
            &key,
            now + Duration::seconds(MAX_REQUEST_DRIFT_SECONDS - 1),
            "POST",
            "/faucet",
            (),
        );

        let guard = ReplayGuard::open();
        assert_eq!(envelope.verify_unmetered(&guard, "POST", "/faucet").unwrap(), key.public_key());

        // A sweep one drift window later -- the point at which the old
        // cutoff dropped this signature.
        let after_one_window = now + Duration::seconds(MAX_REQUEST_DRIFT_SECONDS + 1);
        guard.cleanup(after_one_window);

        // The premise: the envelope is still perfectly valid at that
        // moment, so forgetting it is forgetting something usable.
        assert!(
            envelope.verify_signature(after_one_window, "POST", "/faucet").is_ok(),
            "the envelope is still inside its drift window here, which is what makes this a hole"
        );
        assert!(
            matches!(envelope.verify_unmetered(&guard, "POST", "/faucet"), Err(AuthError::Replayed)),
            "an envelope that can still be replayed must still be remembered"
        );
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
        assert!(matches!(stale.verify_unmetered(&guard, "POST", "/faucet"), Err(AuthError::ClockDrift)));
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
        assert!(envelope.verify_unmetered(&guard, "POST", "/faucet").is_ok());
        assert!(matches!(envelope.verify_unmetered(&guard, "POST", "/faucet"), Err(AuthError::Replayed)));
    }
}

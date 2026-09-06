use tracing::*;

use btclib::crypto::PublicKey;
pub use btclib::envelope::{EnvelopeError as AuthError, SignedEnvelope};
use btclib::envelope::MAX_REQUEST_DRIFT_SECONDS;
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use serde::{de::DeserializeOwned, Serialize};
use static_init::dynamic;

#[dynamic]
static SEEN_SIGNATURES: DashMap<Vec<u8>, DateTime<Utc>> = DashMap::new();

/// The half of the replay guard that must *not* be process-wide.
///
/// `SEEN_SIGNATURES` above is safe to share across every hub instance in
/// a process: a signature is unique per request by construction, so two
/// instances can never collide on one. (That is the same argument
/// `rate_limit::RateLimitTable`'s doc comment makes in reverse for why a
/// client IP -- emphatically *not* unique -- cannot be tracked globally.)
/// What is not safe to share, and what this holds, is state belonging to
/// one hub's own lifetime.
///
/// Today that is the restart window. `SEEN_SIGNATURES` starts empty on
/// every boot, so for `MAX_REQUEST_DRIFT_SECONDS` after a restart every
/// envelope the hub accepted just before it went down is *still inside
/// its own drift window* and will verify a second time -- an ordinary
/// deploy, not just a crash, hands anyone who captured one a fresh
/// chance to use it. `booting` closes that window the one way that needs
/// no storage at all: refuse authenticated requests until the newest
/// possible pre-restart envelope has aged out of the drift check that
/// `verify_signature` already applies. Every read route is
/// unauthenticated, so this degrades writes for two minutes rather than
/// taking the hub dark.
pub struct ReplayGuard {
    /// When this instance starts accepting authenticated requests, or
    /// `None` for a guard with no restart window to wait out.
    accepting_from: Option<DateTime<Utc>>,
}

impl ReplayGuard {
    /// For a hub that has just started and therefore lost whatever the
    /// previous process had seen: refuses authenticated requests until
    /// `started_at + MAX_REQUEST_DRIFT_SECONDS`.
    pub fn booting(started_at: DateTime<Utc>) -> Self {
        Self {
            accepting_from: Some(started_at + Duration::seconds(MAX_REQUEST_DRIFT_SECONDS)),
        }
    }

    /// For a hub with no restart window to wait out -- one whose replay
    /// state was never lost. Test hubs use this: each is a brand-new
    /// process-lifetime hub with no earlier self whose envelopes could
    /// be replayed against it, and making every hub in the suite sit out
    /// two minutes to prove that would be absurd. `booting`'s own
    /// behaviour is covered directly in this module's tests instead.
    pub fn open() -> Self {
        Self { accepting_from: None }
    }

    /// Whether `now` is still inside this instance's restart window.
    /// Split out from `verify` so the window can be tested against a
    /// simulated clock rather than a real two-minute wait, the same
    /// reason `verify_signature` takes `now` as a parameter.
    fn refuses_at(&self, now: DateTime<Utc>) -> bool {
        self.accepting_from.is_some_and(|from| now < from)
    }
}

/// Hub-side verification: the shared signature/timestamp recipe (see
/// `btclib::envelope::SignedEnvelope::verify_signature`) plus a replay
/// check against this process's in-memory cache -- the one part of
/// verification that's inherently server-side state, not something a
/// client SDK needs or could share. Kept as an extension trait (rather
/// than moving the whole thing into btclib) so every existing
/// `envelope.verify()` call site in `handlers.rs` needed no changes
/// beyond importing this trait and passing the caller's `ReplayGuard`.
pub trait VerifyEnvelope {
    fn verify(&self, guard: &ReplayGuard) -> Result<PublicKey, AuthError>;
}

impl<T: Serialize + DeserializeOwned> VerifyEnvelope for SignedEnvelope<T> {
    fn verify(&self, guard: &ReplayGuard) -> Result<PublicKey, AuthError> {
        let now = Utc::now();

        // Before the ECDSA verify, not after: this is a constant-time
        // field read, and §3.4 of the ecosystem plan asks that the cheap
        // checks run first so a flood costs the attacker more than it
        // costs us.
        if guard.refuses_at(now) {
            debug!("refusing an authenticated request inside the post-restart replay window");
            return Err(AuthError::GuardWarmingUp);
        }

        let pubkey = self.verify_signature(now)?;

        // Only mark as seen once the signature is confirmed genuine --
        // otherwise anyone could burn arbitrary signature slots with junk
        // bytes. A real signature is unforgeable, so this can only ever
        // be inserted by whoever actually holds the private key. The hex
        // decode below is already known to succeed -- verify_signature
        // just decoded this exact same field.
        let signature_bytes = hex::decode(&self.signature)
            .expect("verify_signature already validated this hex string");
        if SEEN_SIGNATURES.insert(signature_bytes, now).is_some() {
            return Err(AuthError::Replayed);
        }

        Ok(pubkey)
    }
}

/// Evicts replay-guard entries old enough that their originating request
/// would already fail the clock-drift check on its own -- keeps this from
/// growing forever on a long-running node. Call periodically from a
/// background sweep.
pub fn cleanup_replay_guard() {
    let cutoff = Utc::now() - Duration::seconds(MAX_REQUEST_DRIFT_SECONDS);
    SEEN_SIGNATURES.retain(|_, seen_at| *seen_at > cutoff);
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let key = btclib::crypto::PrivateKey::new_key();
        let envelope = SignedEnvelope::new(&key, ());
        let guard = ReplayGuard::booting(Utc::now());
        assert!(matches!(envelope.verify(&guard), Err(AuthError::GuardWarmingUp)));

        // ...and the same envelope sails through once the window closes,
        // proving the refusal was the window and nothing else.
        let guard = ReplayGuard::open();
        assert_eq!(envelope.verify(&guard).unwrap(), key.public_key());
    }
}

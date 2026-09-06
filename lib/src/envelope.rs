use crate::crypto::{PrivateKey, PublicKey, Signature};
use crate::sha256::Hash;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// How far a request's claimed timestamp may drift from the verifier's own
/// clock before it's rejected outright.
pub const MAX_REQUEST_DRIFT_SECONDS: i64 = 120;

#[derive(Debug, thiserror::Error)]
pub enum EnvelopeError {
    #[error("request timestamp is too far from the verifier's clock")]
    ClockDrift,
    #[error("request has already been used (possible replay)")]
    Replayed,
    /// Verifier-side only, like `Replayed`: the server is inside the
    /// window after a restart in which envelopes it accepted before the
    /// restart are still within `MAX_REQUEST_DRIFT_SECONDS` and would
    /// otherwise verify a second time. Distinct from `Replayed` because
    /// it says something completely different to a caller -- this
    /// request was never used, it simply arrived while the server was
    /// closing that window, and retrying it shortly will work.
    #[error("server is closing its post-restart replay window; retry shortly")]
    GuardWarmingUp,
    #[error("signature does not match the claimed public key")]
    BadSignature,
    #[error("malformed public key: {0}")]
    BadPublicKey(String),
    #[error("malformed signature: {0}")]
    BadSignatureEncoding(String),
}

/// A request signed by the agent making it.
///
/// `pubkey`/`signature` travel as hex strings rather than btclib's
/// internal CBOR shape, and what gets signed is a plain canonical string
/// -- not a Rust/CBOR-specific encoding -- so that any HTTP client in any
/// language (not just Rust) can construct a valid request. The exact
/// recipe (see `signing_string`) is: `"{pubkey_hex}:{timestamp_rfc3339}:{payload_as_json}"`,
/// SHA256'd and then secp256k1/ECDSA-signed.
///
/// This type lives here rather than in `hub` so that verifiers (`hub`)
/// and constructors (`sdk`, and through it `agent-sdk-py`'s fixture
/// conformance target) share one canonical implementation of the recipe
/// that can't silently drift apart between the two sides.
#[derive(Debug, Deserialize, Serialize)]
pub struct SignedEnvelope<T> {
    pub pubkey: String,
    pub timestamp: DateTime<Utc>,
    pub payload: T,
    pub signature: String,
}

// Only `Serialize` is required here -- none of these methods ever
// deserialize `payload`, they only ever sign/hash its serialized form.
// (Axum's `Json<SignedEnvelope<T>>` extractor separately requires `T:
// DeserializeOwned` at HTTP-handler call sites, via the struct's own
// `#[derive(Deserialize)]` above -- that's unrelated to, and unaffected
// by, the bound on this impl block.) Keeping this to `Serialize` alone is
// what lets outbound-only callers -- `sdk::build_envelope`, and anything
// that only ever sends requests and never receives this exact type back
// -- sign a payload without needing a spurious `Deserialize` derive.
impl<T: Serialize> SignedEnvelope<T> {
    /// The exact canonical string that gets hashed and signed. Any client
    /// in any language must reproduce this exactly, byte for byte -- see
    /// `sdk`/`agent-sdk-py` for reference implementations, and their
    /// shared fixture file for a conformance check against this one.
    pub fn signing_string(&self) -> Result<String, EnvelopeError> {
        let payload_json = serde_json::to_string(&self.payload)
            .map_err(|e| EnvelopeError::BadPublicKey(format!("payload not serializable: {e}")))?;
        Ok(format!(
            "{}:{}:{}",
            self.pubkey,
            self.timestamp.to_rfc3339(),
            payload_json
        ))
    }

    /// Signs `payload` with `private_key`, producing a ready-to-send
    /// envelope -- the construction half of the recipe `signing_string`
    /// documents. `sdk` (and through it `smoke_agent.rs`) is a thin
    /// wrapper around this, so there is exactly one place either half of
    /// the recipe is written down in Rust.
    pub fn new(private_key: &PrivateKey, payload: T) -> Self {
        Self::new_at(private_key, Utc::now(), payload)
    }

    /// Same as `new`, but with an explicit timestamp rather than the
    /// current time. Used where determinism matters -- e.g. generating
    /// reproducible cross-language conformance fixtures (see
    /// `sdk/examples/gen_fixtures.rs`).
    pub fn new_at(private_key: &PrivateKey, timestamp: DateTime<Utc>, payload: T) -> Self {
        let mut envelope = SignedEnvelope {
            pubkey: private_key.public_key().to_string(),
            timestamp,
            payload,
            signature: String::new(),
        };
        let signing_string = envelope
            .signing_string()
            .expect("payload just came from a live value, it must serialize");
        let hash = Hash::hash_bytes(signing_string.as_bytes());
        let signature = Signature::sign_hash(&hash, private_key);
        envelope.signature = hex::encode(signature.to_bytes());
        envelope
    }

    /// Verifies the envelope's timestamp and signature, returning the
    /// verified public key on success. `now` is taken as a parameter
    /// (rather than calling `Utc::now()` internally), matching this
    /// codebase's existing convention for anything time-sensitive (see
    /// e.g. `TaskBoard::expire_reserved_escrows`) -- it keeps this
    /// testable without wall-clock flakiness.
    ///
    /// Deliberately does NOT check for replay -- that needs state that
    /// persists across requests, which only a server has, layered on top
    /// by callers that need it (see `hub::auth::VerifyEnvelope` for the
    /// hub's version, which does).
    pub fn verify_signature(&self, now: DateTime<Utc>) -> Result<PublicKey, EnvelopeError> {
        if (now - self.timestamp).abs() > Duration::seconds(MAX_REQUEST_DRIFT_SECONDS) {
            return Err(EnvelopeError::ClockDrift);
        }

        let pubkey_bytes =
            hex::decode(&self.pubkey).map_err(|e| EnvelopeError::BadPublicKey(e.to_string()))?;
        let pubkey = PublicKey::from_sec1_bytes(&pubkey_bytes)
            .map_err(|e| EnvelopeError::BadPublicKey(e.to_string()))?;

        let signature_bytes = hex::decode(&self.signature)
            .map_err(|e| EnvelopeError::BadSignatureEncoding(e.to_string()))?;
        let signature = Signature::from_bytes(&signature_bytes)
            .map_err(|e| EnvelopeError::BadSignatureEncoding(e.to_string()))?;

        let hash = Hash::hash_bytes(self.signing_string()?.as_bytes());
        if !signature.verify(&hash, &pubkey) {
            return Err(EnvelopeError::BadSignature);
        }

        Ok(pubkey)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[derive(Serialize, Deserialize)]
    struct Payload {
        a: u64,
        b: String,
    }

    #[test]
    fn a_freshly_signed_envelope_verifies() {
        let key = PrivateKey::new_key();
        let envelope = SignedEnvelope::new(&key, Payload { a: 1, b: "x".into() });
        let verified = envelope.verify_signature(Utc::now()).unwrap();
        assert_eq!(verified, key.public_key());
    }

    #[test]
    fn verify_rejects_a_signature_from_a_different_key() {
        let key = PrivateKey::new_key();
        let mut envelope = SignedEnvelope::new(&key, Payload { a: 1, b: "x".into() });
        envelope.pubkey = PrivateKey::new_key().public_key().to_string();
        assert!(matches!(
            envelope.verify_signature(Utc::now()),
            Err(EnvelopeError::BadSignature)
        ));
    }

    #[test]
    fn verify_rejects_a_tampered_payload() {
        let key = PrivateKey::new_key();
        let mut envelope = SignedEnvelope::new(&key, Payload { a: 1, b: "x".into() });
        envelope.payload.a = 2;
        assert!(matches!(
            envelope.verify_signature(Utc::now()),
            Err(EnvelopeError::BadSignature)
        ));
    }

    #[test]
    fn verify_rejects_a_timestamp_too_far_in_the_past_or_future() {
        let key = PrivateKey::new_key();
        let envelope = SignedEnvelope::new(&key, Payload { a: 1, b: "x".into() });
        let too_late = envelope.timestamp + Duration::seconds(MAX_REQUEST_DRIFT_SECONDS + 1);
        let too_early = envelope.timestamp - Duration::seconds(MAX_REQUEST_DRIFT_SECONDS + 1);
        assert!(matches!(envelope.verify_signature(too_late), Err(EnvelopeError::ClockDrift)));
        assert!(matches!(envelope.verify_signature(too_early), Err(EnvelopeError::ClockDrift)));
    }

    #[test]
    fn verify_accepts_a_timestamp_right_at_the_drift_boundary() {
        let key = PrivateKey::new_key();
        let envelope = SignedEnvelope::new(&key, Payload { a: 1, b: "x".into() });
        let at_boundary = envelope.timestamp + Duration::seconds(MAX_REQUEST_DRIFT_SECONDS);
        assert!(envelope.verify_signature(at_boundary).is_ok());
    }

    #[test]
    fn signing_string_matches_the_documented_recipe() {
        let key = PrivateKey::new_key();
        let envelope = SignedEnvelope::new(&key, Payload { a: 1, b: "x".into() });
        let expected = format!(
            "{}:{}:{}",
            envelope.pubkey,
            envelope.timestamp.to_rfc3339(),
            serde_json::to_string(&envelope.payload).unwrap()
        );
        assert_eq!(envelope.signing_string().unwrap(), expected);
    }
}

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
    /// Verifier-side only: the server could not durably record that it
    /// had accepted this request, and refuses to act on one it cannot
    /// remember accepting. Also a "retry shortly", but for an operational
    /// fault rather than a scheduled window -- the two are separate so a
    /// storm of them is legible as the incident it is.
    #[error("server could not record this request against replay: {0}")]
    GuardUnavailable(String),
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
/// recipe (see `signing_string`) is
/// `"{pubkey_hex}:{timestamp_rfc3339}:{METHOD} {path}:{payload_as_json}"`,
/// SHA256'd and then secp256k1/ECDSA-signed.
///
/// Note what the method and path are doing there: they are *not* carried
/// on the wire. They are context the signature commits to, supplied
/// independently by each side -- the client names the request it is about
/// to send, the verifier passes the request it actually received. A
/// signature therefore authorizes one endpoint and only that one. Without
/// them the recipe covered the payload alone, so any two routes taking
/// the same payload shape accepted each other's envelopes: `POST /faucet`
/// and `POST /exchange/deposit` (both payload-less), `POST /tasks/:id/claim`
/// and `POST /tasks/:id/cancel` (both `{task_id}`), and three more pairs.
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
    ///
    /// `method` is the uppercase HTTP method and `path` the request path
    /// with no query string (every authenticated route is a POST with
    /// none, and leaving the query out keeps the two sides from having to
    /// agree on parameter ordering). `path` is the *concrete* path,
    /// `/tasks/<uuid>/claim` rather than the route template, so the
    /// signature binds which resource as well as which route -- and so a
    /// hand-written client can sign the URL it is about to call instead of
    /// having to know our routing table.
    ///
    /// On the `:` separators, which also occur inside two of the fields:
    /// this is never parsed, only rebuilt and compared, so ambiguity would
    /// only matter if two different (pubkey, timestamp, method, path,
    /// payload) tuples could produce one string. They cannot. The pubkey
    /// is hex, the timestamp is canonicalized by `to_rfc3339` after being
    /// parsed into a `DateTime` (so it cannot be padded), the method comes
    /// from a fixed set, the path matched a route whose only variable
    /// segments are UUIDs, and the payload is re-serialized canonically by
    /// the verifier rather than echoed. No field can borrow characters
    /// from its neighbour.
    pub fn signing_string(&self, method: &str, path: &str) -> Result<String, EnvelopeError> {
        let payload_json = serde_json::to_string(&self.payload)
            .map_err(|e| EnvelopeError::BadPublicKey(format!("payload not serializable: {e}")))?;
        Ok(format!(
            "{}:{}:{} {}:{}",
            self.pubkey,
            self.timestamp.to_rfc3339(),
            method,
            path,
            payload_json
        ))
    }

    /// Signs `payload` with `private_key`, producing a ready-to-send
    /// envelope -- the construction half of the recipe `signing_string`
    /// documents. `sdk` (and through it `smoke_agent.rs`) is a thin
    /// wrapper around this, so there is exactly one place either half of
    /// the recipe is written down in Rust.
    pub fn new(private_key: &PrivateKey, method: &str, path: &str, payload: T) -> Self {
        Self::new_at(private_key, Utc::now(), method, path, payload)
    }

    /// Same as `new`, but with an explicit timestamp rather than the
    /// current time. Used where determinism matters -- e.g. generating
    /// reproducible cross-language conformance fixtures (see
    /// `sdk/examples/gen_fixtures.rs`).
    pub fn new_at(
        private_key: &PrivateKey,
        timestamp: DateTime<Utc>,
        method: &str,
        path: &str,
        payload: T,
    ) -> Self {
        let mut envelope = SignedEnvelope {
            pubkey: private_key.public_key().to_string(),
            timestamp,
            payload,
            signature: String::new(),
        };
        let signing_string = envelope
            .signing_string(method, path)
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
    /// `method`/`path` are the request the verifier actually received,
    /// never anything the envelope claims about itself -- that is the
    /// whole point of binding them. Passing the route the envelope was
    /// *intended* for, rather than the one it arrived on, would rebuild
    /// exactly the gap this closes.
    ///
    /// Deliberately does NOT check for replay -- that needs state that
    /// persists across requests, which only a server has, layered on top
    /// by callers that need it (see `hub::auth::VerifyEnvelope` for the
    /// hub's version, which does).
    pub fn verify_signature(
        &self,
        now: DateTime<Utc>,
        method: &str,
        path: &str,
    ) -> Result<PublicKey, EnvelopeError> {
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

        let hash = Hash::hash_bytes(self.signing_string(method, path)?.as_bytes());
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
        let envelope = SignedEnvelope::new(&key, "POST", "/tasks", Payload { a: 1, b: "x".into() });
        let verified = envelope.verify_signature(Utc::now(), "POST", "/tasks").unwrap();
        assert_eq!(verified, key.public_key());
    }

    #[test]
    fn verify_rejects_a_signature_from_a_different_key() {
        let key = PrivateKey::new_key();
        let mut envelope = SignedEnvelope::new(&key, "POST", "/tasks", Payload { a: 1, b: "x".into() });
        envelope.pubkey = PrivateKey::new_key().public_key().to_string();
        assert!(matches!(
            envelope.verify_signature(Utc::now(), "POST", "/tasks"),
            Err(EnvelopeError::BadSignature)
        ));
    }

    #[test]
    fn verify_rejects_a_tampered_payload() {
        let key = PrivateKey::new_key();
        let mut envelope = SignedEnvelope::new(&key, "POST", "/tasks", Payload { a: 1, b: "x".into() });
        envelope.payload.a = 2;
        assert!(matches!(
            envelope.verify_signature(Utc::now(), "POST", "/tasks"),
            Err(EnvelopeError::BadSignature)
        ));
    }

    #[test]
    fn verify_rejects_a_timestamp_too_far_in_the_past_or_future() {
        let key = PrivateKey::new_key();
        let envelope = SignedEnvelope::new(&key, "POST", "/tasks", Payload { a: 1, b: "x".into() });
        let too_late = envelope.timestamp + Duration::seconds(MAX_REQUEST_DRIFT_SECONDS + 1);
        let too_early = envelope.timestamp - Duration::seconds(MAX_REQUEST_DRIFT_SECONDS + 1);
        assert!(matches!(envelope.verify_signature(too_late, "POST", "/tasks"), Err(EnvelopeError::ClockDrift)));
        assert!(matches!(envelope.verify_signature(too_early, "POST", "/tasks"), Err(EnvelopeError::ClockDrift)));
    }

    #[test]
    fn verify_accepts_a_timestamp_right_at_the_drift_boundary() {
        let key = PrivateKey::new_key();
        let envelope = SignedEnvelope::new(&key, "POST", "/tasks", Payload { a: 1, b: "x".into() });
        let at_boundary = envelope.timestamp + Duration::seconds(MAX_REQUEST_DRIFT_SECONDS);
        assert!(envelope.verify_signature(at_boundary, "POST", "/tasks").is_ok());
    }

    #[test]
    fn signing_string_matches_the_documented_recipe() {
        let key = PrivateKey::new_key();
        let envelope = SignedEnvelope::new(&key, "POST", "/tasks", Payload { a: 1, b: "x".into() });
        let expected = format!(
            "{}:{}:{} {}:{}",
            envelope.pubkey,
            envelope.timestamp.to_rfc3339(),
            "POST",
            "/tasks",
            serde_json::to_string(&envelope.payload).unwrap()
        );
        assert_eq!(envelope.signing_string("POST", "/tasks").unwrap(), expected);
    }

    /// The reason the recipe changed. An envelope signed for one route
    /// must not verify against another, even when the payload -- all the
    /// old recipe covered -- is byte-identical.
    #[test]
    fn an_envelope_signed_for_one_path_does_not_verify_against_another() {
        let key = PrivateKey::new_key();
        let envelope = SignedEnvelope::new(&key, "POST", "/faucet", ());
        assert!(envelope.verify_signature(Utc::now(), "POST", "/faucet").is_ok());
        assert!(matches!(
            envelope.verify_signature(Utc::now(), "POST", "/exchange/deposit"),
            Err(EnvelopeError::BadSignature)
        ));
    }

    /// The path is the concrete one, so the signature also pins which
    /// resource -- a claim on one task is not a claim on another.
    #[test]
    fn an_envelope_signed_for_one_resource_does_not_verify_against_another() {
        let key = PrivateKey::new_key();
        let envelope = SignedEnvelope::new(&key, "POST", "/tasks/aaa/claim", ());
        assert!(envelope.verify_signature(Utc::now(), "POST", "/tasks/aaa/claim").is_ok());
        assert!(matches!(
            envelope.verify_signature(Utc::now(), "POST", "/tasks/bbb/claim"),
            Err(EnvelopeError::BadSignature)
        ));
    }

    #[test]
    fn the_method_is_bound_too_not_just_the_path() {
        let key = PrivateKey::new_key();
        let envelope = SignedEnvelope::new(&key, "POST", "/tasks", ());
        assert!(matches!(
            envelope.verify_signature(Utc::now(), "DELETE", "/tasks"),
            Err(EnvelopeError::BadSignature)
        ));
    }

    /// No field can borrow characters from its neighbour: shifting a
    /// colon between the path and the payload must not produce the same
    /// string. Guards the choice to keep `:` as the separator.
    #[test]
    fn fields_cannot_be_shifted_across_the_separators() {
        let key = PrivateKey::new_key();
        let envelope = SignedEnvelope::new(&key, "POST", "/a", "b".to_string());
        assert_ne!(
            envelope.signing_string("POST", "/a").unwrap(),
            envelope.signing_string("POST", "/a:").unwrap()
        );
    }
}

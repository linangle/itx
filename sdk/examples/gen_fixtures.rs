//! Generates `agent-sdk-py/tests/fixtures/envelope_fixtures.json`: known
//! private keys, timestamps, and payloads, signed by the real
//! `btclib::envelope` recipe -- the exact same code `hub` verifies
//! against. Any other language's implementation of the same recipe (right
//! now, just `agent-sdk-py`) checks its own output against these known-
//! good values, instead of only ever being able to test itself against
//! itself. See the plan's "Conformance, not hand-waved" note: this file
//! exists specifically because two independent implementations of
//! "hash this string and ECDSA-sign it" or "serialize this payload to
//! JSON" can silently diverge (key ordering, number formatting, Unicode
//! escaping...) without ever failing a same-language test.
//!
//! Run with: cargo run -p sdk --example gen_fixtures

use btclib::crypto::PrivateKey;
use btclib::envelope::SignedEnvelope;
use btclib::sha256::Hash;
use chrono::{DateTime, TimeZone, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;

/// Deterministically derives a private key from a fixed seed string,
/// rather than `PrivateKey::new_key()` -- fixtures must be byte-for-byte
/// reproducible across runs, and there is no public getter back from a
/// `PrivateKey` to its raw scalar (persistence goes through `Saveable`
/// instead), so the seed's own hash is kept alongside the key rather than
/// re-extracted from it.
fn fixed_key(seed: &[u8]) -> (PrivateKey, [u8; 32]) {
    let scalar = Hash::hash_bytes(seed).as_bytes();
    let key = PrivateKey::from_fixed_bytes(&scalar)
        .expect("a SHA256 output is, for all practical purposes, always a valid secp256k1 scalar");
    (key, scalar)
}

fn fixture_entry<T: Serialize>(
    name: &str,
    private_key: &PrivateKey,
    key_seed_bytes: [u8; 32],
    timestamp: DateTime<Utc>,
    method: &str,
    path: &str,
    payload: T,
) -> Value {
    let envelope = SignedEnvelope::new_at(private_key, timestamp, method, path, payload);
    let signing_string = envelope
        .signing_string(method, path)
        .expect("every fixture payload below is a plain serializable struct");
    let payload_json = serde_json::to_string(&envelope.payload).unwrap();
    json!({
        "name": name,
        "private_key_hex": hex::encode(key_seed_bytes),
        "pubkey_hex": envelope.pubkey,
        "timestamp": envelope.timestamp.to_rfc3339(),
        "method": method,
        "path": path,
        "payload_json": payload_json,
        "expected_signing_string": signing_string,
        "expected_signature_hex": envelope.signature,
    })
}

#[derive(Serialize)]
struct ClaimLikePayload {
    task_id: String,
}

#[derive(Serialize)]
struct RichPayload {
    description: String,
    bounty: u64,
    min_reputation: u64,
    capabilities: BTreeSet<String>,
}

#[derive(Serialize)]
struct UnicodeStressPayload {
    note: String,
}

fn main() {
    let mut fixtures = Vec::new();

    let (key_a, seed_a) = fixed_key(b"itx fixture key A");
    let (key_b, seed_b) = fixed_key(b"itx fixture key B");

    let t1 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let t2 = Utc
        .with_ymd_and_hms(2026, 6, 15, 12, 30, 45)
        .unwrap()
        + chrono::Duration::milliseconds(123);

    fixtures.push(fixture_entry("unit_payload", &key_a, seed_a, t1, "POST", "/faucet", ()));

    // The same key, timestamp and payload as `unit_payload`, differing
    // only in the path -- and `/faucet` and `/exchange/deposit` are one
    // of the five route pairs the old payload-only recipe could not tell
    // apart. A Python implementation that ignored the path would produce
    // the signature above for this entry and fail here, which is the
    // single most valuable thing this fixture file now pins.
    fixtures.push(fixture_entry("same_payload_different_path", &key_a, seed_a, t1, "POST", "/exchange/deposit", ()));

    fixtures.push(fixture_entry(
        "simple_struct",
        &key_a,
        seed_a,
        t1,
        "POST",
        "/tasks/11111111-2222-3333-4444-555555555555/claim",
        ClaimLikePayload {
            task_id: "11111111-2222-3333-4444-555555555555".to_string(),
        },
    ));

    fixtures.push(fixture_entry(
        "multi_field_with_capabilities",
        &key_b,
        seed_b,
        t2,
        "POST",
        "/tasks/escrow",
        RichPayload {
            description: "reference SDK fixture task".to_string(),
            bounty: 1_000_000,
            min_reputation: 3,
            capabilities: BTreeSet::from(["python".to_string(), "rust".to_string()]),
        },
    ));

    fixtures.push(fixture_entry(
        "multi_field_empty_capabilities",
        &key_b,
        seed_b,
        t1,
        "POST",
        "/tasks",
        RichPayload {
            description: "no tags".to_string(),
            bounty: 10,
            min_reputation: 0,
            capabilities: BTreeSet::new(),
        },
    ));

    // Deliberately stresses JSON string escaping: a quote, a backslash, a
    // newline, and non-ASCII text. Python's `json.dumps` defaults to
    // `ensure_ascii=True` (`\uXXXX`-escaping anything non-ASCII) and to
    // inserting spaces after `:`/`,` -- both would silently produce a
    // different string than `serde_json`'s compact, UTF-8-native output,
    // and this fixture case is what would actually catch that.
    fixtures.push(fixture_entry(
        "unicode_and_escaping_stress",
        &key_a,
        seed_a,
        t2,
        "POST",
        "/tasks/consensus",
        UnicodeStressPayload {
            note: "caf\u{e9} \"quoted\" \\ newline:\n \u{30c6}\u{30b9}\u{30c8} \u{1f984}".to_string(),
        },
    ));

    let out_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../agent-sdk-py/tests/fixtures/envelope_fixtures.json");
    std::fs::create_dir_all(out_path.parent().unwrap()).expect("failed to create fixtures directory");
    let json_text = serde_json::to_string_pretty(&fixtures).unwrap();
    std::fs::write(&out_path, json_text).expect("failed to write fixture file");
    println!("wrote {} fixtures to {}", fixtures.len(), out_path.display());
}

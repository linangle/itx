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
//! Also generates `output_fixtures.json` beside it: the other thing an
//! agent signs, a spend of its own output. An input's signature is over
//! `Transaction::spend_commitment` -- the output being spent *and* every
//! output the transaction pays -- so a client funding an escrow through
//! `POST /wallet/send` builds those bytes itself from the hash the hub
//! reports and the payments it is asking for. The preimage is written by
//! hand for exactly this reason (no CBOR outside Rust), which makes a
//! byte-for-byte fixture the only thing standing between the two
//! implementations and a silent divergence. `expected_commitment_hex` is
//! here so a mismatch says *which* half is wrong: the bytes, or the
//! signature over them.
//!
//! Run with: cargo run -p sdk --example gen_fixtures

use btclib::crypto::{PrivateKey, PublicKey, Signature};
use btclib::envelope::SignedEnvelope;
use btclib::sha256::Hash;
use btclib::types::{Transaction, TransactionInput, TransactionOutput};
use uuid::Uuid;
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
    hub: &str,
    payload: T,
) -> Value {
    let envelope = SignedEnvelope::new_at(private_key, timestamp, method, path, hub, payload);
    let signing_string = envelope
        .signing_string(method, path, hub)
        .expect("every fixture payload below is a plain serializable struct");
    let payload_json = serde_json::to_string(&envelope.payload).unwrap();
    json!({
        "name": name,
        "private_key_hex": hex::encode(key_seed_bytes),
        "pubkey_hex": envelope.pubkey,
        "timestamp": envelope.timestamp.to_rfc3339(),
        "method": method,
        "path": path,
        "hub": hub,
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
    // The spend fixtures below pay one key from the other, so both
    // public halves are wanted by value rather than rebuilt per case.
    let pub_a = key_a.public_key();
    let pub_b = key_b.public_key();
    // Two hubs, so the fixture file can pin the hub binding the way it
    // pins the path binding: the same request, signed for the other
    // hub, must produce a different signature.
    let hub_a = fixed_key(b"itx fixture hub A").0.public_key().to_string();
    let hub_b = fixed_key(b"itx fixture hub B").0.public_key().to_string();

    let t1 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let t2 = Utc
        .with_ymd_and_hms(2026, 6, 15, 12, 30, 45)
        .unwrap()
        + chrono::Duration::milliseconds(123);

    fixtures.push(fixture_entry("unit_payload", &key_a, seed_a, t1, "POST", "/faucet", &hub_a, ()));

    // The same key, timestamp, path and payload as `unit_payload`,
    // signed for a different hub. An implementation that ignored the hub
    // would produce `unit_payload`'s signature here and fail.
    fixtures.push(fixture_entry("same_request_different_hub", &key_a, seed_a, t1, "POST", "/faucet", &hub_b, ()));

    // The same key, timestamp and payload as `unit_payload`, differing
    // only in the path -- and `/faucet` and `/exchange/deposit` are one
    // of the five route pairs the old payload-only recipe could not tell
    // apart. A Python implementation that ignored the path would produce
    // the signature above for this entry and fail here, which is the
    // single most valuable thing this fixture file now pins.
    fixtures.push(fixture_entry("same_payload_different_path", &key_a, seed_a, t1, "POST", "/exchange/deposit", &hub_a, ()));

    fixtures.push(fixture_entry(
        "simple_struct",
        &key_a,
        seed_a,
        t1,
        "POST",
        "/tasks/11111111-2222-3333-4444-555555555555/claim",
        &hub_a,
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
        &hub_a,
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
        &hub_a,
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
        &hub_a,
        UnicodeStressPayload {
            note: "caf\u{e9} \"quoted\" \\ newline:\n \u{30c6}\u{30b9}\u{30c8} \u{1f984}".to_string(),
        },
    ));

    let out_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../agent-sdk-py/tests/fixtures/envelope_fixtures.json");
    std::fs::create_dir_all(out_path.parent().unwrap()).expect("failed to create fixtures directory");
    let json_text = serde_json::to_string_pretty(&fixtures).unwrap();
    std::fs::write(&out_path, json_text).expect("failed to write fixture file");
    println!("wrote {} fixtures to {}", fixtures.len(), out_path.display());

    // The spending signature. `hash` is spelled the way
    // `GET /wallet/<pubkey>` spells it -- `hex::encode(as_bytes())`, the
    // same form `expected_output_hash` takes -- so a client can be fed
    // the fixture's `hash` exactly as the hub would feed it, and `pays`
    // is spelled the way `POST /wallet/send` spells its outputs.
    //
    // The cases are chosen for what they pin in the preimage: one
    // payment, two (which is what change makes of every ordinary send),
    // and the same spend under a different key.
    let spends: [(&str, &PrivateKey, [u8; 32], u64, &str, Vec<(&PublicKey, u64)>); 3] = [
        (
            "faucet_grant_sized_spend",
            &key_a,
            seed_a,
            50_000_000u64,
            "0f5f1e1a-0000-4000-8000-00000000abcd",
            vec![(&pub_b, 49_999_000)],
        ),
        // Two outputs, the second the change back to the spender: the
        // ordering and the count are part of what is signed, so a client
        // that built them in the other order must fail this.
        (
            "spend_with_change",
            &key_a,
            seed_a,
            50_000_000u64,
            "0f5f1e1a-0000-4000-8000-00000000abcd",
            vec![(&pub_b, 20_000_000), (&pub_a, 29_999_000)],
        ),
        // The same spend under key B: everything differs but the signer.
        (
            "small_spend_other_key",
            &key_b,
            seed_b,
            1_234u64,
            "11111111-2222-3333-4444-555555555555",
            vec![(&pub_a, 234)],
        ),
    ];
    let mut output_fixtures = Vec::new();
    for (name, key, seed, value, unique_id, pays) in spends {
        let spent = TransactionOutput {
            value,
            unique_id: Uuid::parse_str(unique_id).unwrap(),
            pubkey: key.public_key(),
        };
        let hash = spent.hash();
        // `unique_id` is the hub's to mint and is deliberately outside
        // the commitment, so any value does here -- see
        // `Transaction::spend_commitment`.
        let outputs: Vec<TransactionOutput> = pays
            .iter()
            .map(|(pubkey, amount)| TransactionOutput {
                value: *amount,
                unique_id: Uuid::new_v4(),
                pubkey: (*pubkey).clone(),
            })
            .collect();
        let commitment = Transaction::spend_commitment(&hash, &outputs);
        let input = TransactionInput::signed(hash, &outputs, key);
        assert!(input.verifies(&outputs, &key.public_key()));
        output_fixtures.push(json!({
            "name": name,
            "private_key_hex": hex::encode(seed),
            "pubkey_hex": key.public_key().to_string(),
            "value": value,
            "unique_id": unique_id,
            "hash": hex::encode(hash.as_bytes()),
            "pays": pays
                .iter()
                .map(|(pubkey, amount)| json!({ "pubkey": pubkey.to_string(), "value": amount }))
                .collect::<Vec<_>>(),
            "expected_commitment_hex": hex::encode(commitment.as_bytes()),
            "expected_signature_hex": hex::encode(input.signature.to_bytes()),
        }));
    }
    let out_path = out_path.with_file_name("output_fixtures.json");
    std::fs::write(&out_path, serde_json::to_string_pretty(&output_fixtures).unwrap())
        .expect("failed to write output fixture file");
    println!("wrote {} output fixtures to {}", output_fixtures.len(), out_path.display());
}

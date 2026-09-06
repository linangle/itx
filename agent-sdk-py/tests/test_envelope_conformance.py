"""Cross-language conformance check: everything here is verified against
``tests/fixtures/envelope_fixtures.json``, generated from the real Rust
implementation (``sdk/examples/gen_fixtures.rs``) that the hub itself
verifies against. Passing this file is what makes this Python
implementation trustworthy rather than merely self-consistent -- see the
plan's "Conformance, not hand-waved" note.

Regenerate the fixture file with: cargo run -p sdk --example gen_fixtures
"""

import json
from datetime import datetime, timezone
from pathlib import Path

import pytest

from itx_agent_sdk.envelope import Agent, _canonical_json, _format_rfc3339

FIXTURES_PATH = Path(__file__).parent / "fixtures" / "envelope_fixtures.json"


def load_fixtures():
    with open(FIXTURES_PATH, encoding="utf-8") as f:
        return json.load(f)


FIXTURES = load_fixtures()


def test_fixture_file_is_present_and_non_empty():
    assert len(FIXTURES) >= 1


@pytest.mark.parametrize("fixture", FIXTURES, ids=lambda fx: fx["name"])
def test_deriving_the_public_key_matches_the_fixture(fixture):
    agent = Agent.from_private_key_hex(fixture["private_key_hex"])
    assert agent.pubkey_hex == fixture["pubkey_hex"]


@pytest.mark.parametrize("fixture", FIXTURES, ids=lambda fx: fx["name"])
def test_raw_signing_primitive_matches_the_fixture(fixture):
    """Isolates the crypto (hash-then-ECDSA-sign) primitive from JSON/
    timestamp formatting entirely -- signs the fixture's already-known-
    correct `expected_signing_string` verbatim.
    """
    agent = Agent.from_private_key_hex(fixture["private_key_hex"])
    assert agent.sign_message(fixture["expected_signing_string"]) == fixture["expected_signature_hex"]


@pytest.mark.parametrize("fixture", FIXTURES, ids=lambda fx: fx["name"])
def test_canonical_json_matches_the_fixtures_payload_json(fixture):
    """Isolates JSON formatting from the crypto -- round-trips the
    fixture's payload through `json.loads` (order-preserving) and back
    through our own canonical serializer.
    """
    payload = json.loads(fixture["payload_json"])
    assert _canonical_json(payload) == fixture["payload_json"]


@pytest.mark.parametrize("fixture", FIXTURES, ids=lambda fx: fx["name"])
def test_build_envelope_reproduces_the_fixture_end_to_end(fixture):
    """The full pipeline: given the same key, timestamp, and payload the
    fixture used, do we independently arrive at the exact same signing
    string and signature?
    """
    agent = Agent.from_private_key_hex(fixture["private_key_hex"])
    payload = json.loads(fixture["payload_json"])
    timestamp = datetime.fromisoformat(fixture["timestamp"])

    envelope = agent.build_envelope(
        fixture["method"], fixture["path"], payload, timestamp=timestamp
    )

    assert envelope["pubkey"] == fixture["pubkey_hex"]
    assert envelope["timestamp"] == fixture["timestamp"]
    assert _canonical_json(envelope["payload"]) == fixture["payload_json"]
    assert envelope["signature"] == fixture["expected_signature_hex"]
    # The method and path are bound into the signature, never sent.
    assert "method" not in envelope and "path" not in envelope


def test_fixtures_cover_the_endpoint_binding():
    """The fixture file must contain a pair differing only in path, or
    this suite would still pass against an implementation that ignored
    the path entirely -- which is exactly the bug the recipe change
    exists to prevent.
    """
    by_name = {fx["name"]: fx for fx in FIXTURES}
    a, b = by_name["unit_payload"], by_name["same_payload_different_path"]
    assert (a["private_key_hex"], a["timestamp"], a["payload_json"]) == (
        b["private_key_hex"],
        b["timestamp"],
        b["payload_json"],
    ), "the pair must differ in nothing but the path"
    assert a["path"] != b["path"]
    assert a["expected_signature_hex"] != b["expected_signature_hex"]


def test_signing_the_same_payload_for_two_routes_gives_two_signatures():
    """The property from the caller's side: `/faucet` and
    `/exchange/deposit` both take a payload-less envelope, and one must
    not be usable at the other.
    """
    agent = Agent.generate()
    ts = datetime(2026, 1, 1, tzinfo=timezone.utc)
    faucet = agent.build_envelope("POST", "/faucet", None, timestamp=ts)
    deposit = agent.build_envelope("POST", "/exchange/deposit", None, timestamp=ts)
    assert faucet["payload"] == deposit["payload"]
    assert faucet["signature"] != deposit["signature"]


def test_format_rfc3339_matches_chronos_auto_si_style():
    # Zero fractional seconds -> no fractional part at all.
    assert _format_rfc3339(datetime(2026, 1, 1, tzinfo=timezone.utc)) == "2026-01-01T00:00:00+00:00"
    # A whole number of milliseconds -> 3-digit fractional part.
    assert (
        _format_rfc3339(datetime(2026, 6, 15, 12, 30, 45, 123000, tzinfo=timezone.utc))
        == "2026-06-15T12:30:45.123+00:00"
    )
    # Not a whole number of milliseconds -> all 6 microsecond digits.
    assert (
        _format_rfc3339(datetime(2026, 6, 15, 12, 30, 45, 123456, tzinfo=timezone.utc))
        == "2026-06-15T12:30:45.123456+00:00"
    )
    # A non-UTC input is converted, not just relabeled.
    from datetime import timedelta, timezone as _tz

    plus_two = _tz(timedelta(hours=2))
    assert (
        _format_rfc3339(datetime(2026, 1, 1, 2, 0, 0, tzinfo=plus_two))
        == "2026-01-01T00:00:00+00:00"
    )


def test_canonical_json_does_not_escape_non_ascii():
    assert _canonical_json({"note": "café"}) == '{"note":"café"}'


def test_canonical_json_has_no_extra_whitespace():
    assert _canonical_json({"a": 1, "b": 2}) == '{"a":1,"b":2}'


def test_build_envelope_round_trips_through_its_own_signature():
    """Not fixture-based -- a freshly generated key signing a freshly
    generated payload should always self-verify. This is the sanity
    check that would fail loudly if, say, the digest or encoding logic
    were internally inconsistent even if it happened to never touch the
    fixture's exact byte patterns.
    """
    import hashlib

    from ecdsa import SECP256k1, VerifyingKey
    from ecdsa.util import sigdecode_string

    agent = Agent.generate()
    envelope = agent.build_envelope("POST", "/tasks", {"z": 1, "a": 2})
    signing_string = (
        f"{envelope['pubkey']}:{envelope['timestamp']}:"
        f"POST /tasks:{_canonical_json(envelope['payload'])}"
    )
    digest = hashlib.sha256(hashlib.sha256(signing_string.encode("utf-8")).digest()).digest()

    verifying_key = VerifyingKey.from_string(bytes.fromhex(envelope["pubkey"]), curve=SECP256k1)
    assert verifying_key.verify_digest(
        bytes.fromhex(envelope["signature"]), digest, sigdecode=sigdecode_string
    )

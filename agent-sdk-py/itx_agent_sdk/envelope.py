"""Reference implementation of the itx hub's agent-request-signing
protocol -- the Python counterpart to ``btclib::envelope`` (Rust) and
``sdk`` (the Rust SDK crate). See ``tests/test_envelope_conformance.py``
and its ``tests/fixtures/envelope_fixtures.json`` (generated from the
Rust side by ``sdk/examples/gen_fixtures.rs``) for the cross-language
conformance check that keeps this implementation from silently drifting
from the canonical one.
"""

import hashlib
import json
from datetime import datetime, timezone
from typing import Any, Optional

from ecdsa import SECP256k1, SigningKey
from ecdsa.util import sigdecode_string, sigencode_string


def _format_rfc3339(dt: datetime) -> str:
    """Matches chrono's ``DateTime<Utc>::to_rfc3339()`` byte for byte: a
    ``+00:00`` offset (never ``Z``), and the fractional-second part shown
    at the minimal precision that represents it exactly -- omitted
    entirely if zero, 3 digits if a whole number of milliseconds,
    otherwise all 6 (Python's own resolution ceiling; chrono can go to 9,
    but nothing reachable from a Python-originated timestamp needs that).
    """
    dt = dt.astimezone(timezone.utc)
    base = dt.strftime("%Y-%m-%dT%H:%M:%S")
    micros = dt.microsecond
    if micros == 0:
        frac = ""
    elif micros % 1000 == 0:
        frac = f".{micros // 1000:03d}"
    else:
        frac = f".{micros:06d}"
    return f"{base}{frac}+00:00"


def _canonical_json(payload: Any) -> str:
    """Matches ``serde_json::to_string`` byte for byte: compact (no
    whitespace) and UTF-8-native. ``json.dumps``'s own defaults get both
    of these wrong for our purposes -- it inserts a space after ``:`` and
    ``,``, and defaults to ``ensure_ascii=True`` (``\\uXXXX``-escaping
    anything non-ASCII) -- either of which would silently sign a
    different string than the one the hub recomputes.
    """
    return json.dumps(payload, separators=(",", ":"), ensure_ascii=False)


def _digest_to_sign(message: str) -> bytes:
    """The hub's ``Signature::sign_output``/``verify`` (see
    ``lib/src/crypto.rs``) go through RustCrypto's
    ``ecdsa::signature::Signer``/``Verifier`` traits, which hash their
    input with SHA-256 before doing the actual ECDSA math. Since
    ``message`` here is itself already
    ``btclib::sha256::Hash::hash_bytes``'s SHA-256 output, the value
    actually fed to the ECDSA scalar math is SHA-256 applied *twice*
    (two plain rounds -- not Bitcoin-style double-SHA256-as-one-
    primitive). This function does exactly those two rounds, nothing
    more; verified byte-for-byte against the Rust-generated fixtures.
    """
    once = hashlib.sha256(message.encode("utf-8")).digest()
    return hashlib.sha256(once).digest()


def _sign_digest(signing_key: SigningKey, digest: bytes) -> bytes:
    """RFC6979 deterministic ECDSA over ``digest``, low-S normalized, in
    the fixed-size ``(r || s)`` "compact"/IEEE-P1363 encoding -- matching
    ``btclib::crypto::Signature::to_bytes()`` exactly (not DER).
    """
    order = SECP256k1.order
    raw = signing_key.sign_digest_deterministic(
        digest, hashfunc=hashlib.sha256, sigencode=sigencode_string
    )
    r, s = sigdecode_string(raw, order)
    if s > order // 2:
        s = order - s
    return sigencode_string(r, s, order)


class Agent:
    """A keypair that can sign requests for the itx hub. Wraps a
    secp256k1 private key -- generate a fresh one (``Agent.generate()``)
    or load an existing one from its raw 32-byte scalar, hex-encoded
    (``Agent.from_private_key_hex(...)``, the same format
    ``btclib::crypto::PrivateKey::from_fixed_bytes`` and the fixture
    file's ``private_key_hex`` field use).
    """

    def __init__(self, signing_key: SigningKey):
        self._signing_key = signing_key
        self.pubkey_hex: str = signing_key.get_verifying_key().to_string("compressed").hex()

    @property
    def private_key_hex(self) -> str:
        """The raw 32-byte scalar, hex-encoded -- the same format
        ``from_private_key_hex`` accepts, so ``Agent.from_private_key_hex(a.private_key_hex)``
        round-trips. Used by ``identity.load_or_create_agent`` to persist
        an identity to a file.
        """
        return self._signing_key.to_string().hex()

    @classmethod
    def generate(cls) -> "Agent":
        return cls(SigningKey.generate(curve=SECP256k1))

    @classmethod
    def from_private_key_hex(cls, private_key_hex: str) -> "Agent":
        return cls(SigningKey.from_string(bytes.fromhex(private_key_hex), curve=SECP256k1))

    def sign_message(self, message: str) -> str:
        """Signs an arbitrary already-formed string (as opposed to
        ``build_envelope``, which builds the string itself), hex-encoded.
        Exists mainly so the fixture-conformance test can check the raw
        signing primitive against a known
        ``expected_signing_string -> expected_signature_hex`` pair in
        isolation, without also exercising JSON/timestamp formatting at
        the same time.
        """
        digest = _digest_to_sign(message)
        return _sign_digest(self._signing_key, digest).hex()

    def build_envelope(
        self,
        method: str,
        path: str,
        payload: Any,
        timestamp: Optional[datetime] = None,
    ) -> dict:
        """Signs ``payload`` *for one specific endpoint*, producing a
        ready-to-send envelope dict. Mirrors
        ``btclib::envelope::SignedEnvelope::new`` (``new_at`` if
        ``timestamp`` is given explicitly, e.g. for reproducible tests).

        ``method`` is the uppercase HTTP method and ``path`` the request
        path exactly as it will appear in the request line -- no scheme,
        no host, no query string, and with any id already substituted
        (``"/tasks/<uuid>/claim"``, not ``"/tasks/:id/claim"``). Neither
        is sent; both are bound into the signature, so an envelope built
        for one endpoint is rejected at every other. Several hub routes
        take identical payloads -- ``POST /faucet`` and
        ``POST /exchange/deposit`` are both payload-less -- and this is
        what keeps an envelope for one from being spent at the other.

        **Pass the same path you POST to.** ``Client`` does that for you
        by construction (see ``_signed_post``); a caller signing by hand
        that gets it wrong sees a 401 on the first request rather than
        anything subtle.

        ``payload`` must be a JSON-serializable value whose dict keys (if
        any) are already in the *same order* the corresponding hub-side
        Rust struct declares its fields -- see ``client.py``'s payload
        builders, which exist specifically to get this right once per
        endpoint rather than leaving it to every caller. Key order
        doesn't affect what the hub *deserializes*, only what canonical
        string this function -- and, independently, the hub, after
        deserializing -- (re)compute for the signature to be checked
        against; the two must agree.
        """
        ts = timestamp or datetime.now(timezone.utc)
        timestamp_str = _format_rfc3339(ts)
        payload_json = _canonical_json(payload)
        signing_string = f"{self.pubkey_hex}:{timestamp_str}:{method} {path}:{payload_json}"
        return {
            "pubkey": self.pubkey_hex,
            "timestamp": timestamp_str,
            "payload": payload,
            "signature": self.sign_message(signing_string),
        }

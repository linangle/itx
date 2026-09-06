#!/usr/bin/env bash
#
# Restore drill for an ITX backup.
#
# A backup nobody has restored is not a backup. This script restores one
# into a scratch directory, stands a hub and node up against it on
# throwaway ports, and checks that what came back is the same deployment
# -- not merely that the archive decrypted.
#
# It never touches the live state directory and never stops a live
# service, so it is safe to run against production backups on the
# production box. It does bind two ports; pass --port-base to move them.
#
# Usage:
#   itx-restore-drill.sh --archive /var/backups/itx/itx-<stamp>.tar.gz.age \
#                        --identity ~/age-restore-key.txt \
#                        [--expect-operator <pubkey>] \
#                        [--expect-escrow-sha256 <hex>] \
#                        [--bin-dir /usr/local/bin] [--port-base 19000]
#
# The identity file is the private half that deliberately does not live
# on the hub box (see itx-backup.sh). Running this drill therefore also
# tests that you can still lay hands on it -- which is the half of
# "do we have backups" that people actually fail.
#
# Documented step by step in docs/deployment.md §7.4.

set -euo pipefail

ARCHIVE=""
IDENTITY=""
EXPECT_OPERATOR=""
EXPECT_ESCROW=""
BIN_DIR=${ITX_BIN_DIR:-/usr/local/bin}
PORT_BASE=19000
USE_GPG=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --archive)              ARCHIVE="$2";         shift 2 ;;
        --identity)             IDENTITY="$2";        shift 2 ;;
        --expect-operator)      EXPECT_OPERATOR="$2"; shift 2 ;;
        --expect-escrow-sha256) EXPECT_ESCROW="$2";   shift 2 ;;
        --bin-dir)              BIN_DIR="$2";         shift 2 ;;
        --port-base)            PORT_BASE="$2";       shift 2 ;;
        --gpg)                  USE_GPG=1;            shift ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

[[ -n "$ARCHIVE" ]] || { echo "--archive is required" >&2; exit 2; }
[[ -f "$ARCHIVE" ]] || { echo "no such archive: $ARCHIVE" >&2; exit 1; }
[[ "$ARCHIVE" == *.gpg ]] && USE_GPG=1
if [[ $USE_GPG -eq 0 && -z "$IDENTITY" ]]; then
    echo "--identity is required for an age archive" >&2; exit 2
fi

NODE_PORT=$PORT_BASE
HUB_PORT=$((PORT_BASE + 1))

WORK=$(mktemp -d)
chmod 700 "$WORK"
NODE_PID=""
HUB_PID=""
cleanup() {
    [[ -n "$HUB_PID"  ]] && kill "$HUB_PID"  2>/dev/null || true
    [[ -n "$NODE_PID" ]] && kill "$NODE_PID" 2>/dev/null || true
    wait 2>/dev/null || true
    # The scratch tree holds decrypted secrets. It goes away on every
    # exit path, pass or fail.
    rm -rf "$WORK"
}
trap cleanup EXIT

fail() { echo "DRILL FAILED: $*" >&2; exit 1; }
step() { echo; echo "== $*"; }

# --- 1. decrypt -------------------------------------------------------
step "1. decrypting"
if [[ $USE_GPG -eq 1 ]]; then
    gpg --batch --yes --decrypt "$ARCHIVE" 2>/dev/null > "$WORK/restore.tar.gz" \
        || fail "could not decrypt with gpg -- is the secret key present?"
else
    age --decrypt --identity "$IDENTITY" --output "$WORK/restore.tar.gz" "$ARCHIVE" \
        || fail "could not decrypt with age -- wrong identity file?"
fi
tar -C "$WORK" -xzf "$WORK/restore.tar.gz" || fail "archive did not extract"
R="$WORK/itx"
[[ -d "$R" ]] || fail "archive did not contain an itx/ directory"

# --- 2. integrity -----------------------------------------------------
step "2. verifying the manifest"
( cd "$R" && sha256sum --quiet --check MANIFEST.sha256 ) \
    || fail "checksums do not match -- the archive is corrupt, not just old"
echo "all files match their recorded checksums"

# --- 3. the escrow secret, specifically -------------------------------
#
# The one file whose bytes have to be exactly right. Escrow addresses are
# HKDF(secret, deposit id), so a single flipped bit produces a valid-
# looking hub that derives a different address for every deposit and
# sweeps nothing. Length and fingerprint are both checked because the
# hub itself only checks length.
step "3. checking the escrow secret"
SECRET="$R/secrets/hub_escrow_secret.bin"
[[ -f "$SECRET" ]] || fail "hub_escrow_secret.bin is not in the backup"
LEN=$(wc -c < "$SECRET" | tr -d ' ')
[[ "$LEN" == "32" ]] || fail "escrow secret is $LEN bytes, expected 32"
FP=$(sha256sum "$SECRET" | cut -d' ' -f1)
echo "32 bytes, sha256 $FP"
if [[ -n "$EXPECT_ESCROW" ]]; then
    [[ "$FP" == "$EXPECT_ESCROW" ]] \
        || fail "escrow secret fingerprint $FP does not match the expected $EXPECT_ESCROW"
    echo "matches the expected fingerprint"
else
    echo "NOTE: no --expect-escrow-sha256 given, so this only proves internal"
    echo "      consistency. Pass the live fingerprint to prove it is THE secret."
fi

# --- 4. permissions ---------------------------------------------------
#
# tar and cp do not reliably carry mode through every path, and the hub
# only chmods files it CREATES -- a restored file keeps whatever mode it
# arrived with, permanently and silently.
step "4. checking permissions"
BAD=$(find "$R/secrets" -type f ! -perm 600 -print)
[[ -z "$BAD" ]] || fail "restored secrets are not 0600:"$'\n'"$BAD"
echo "all three secrets are 0600"

# --- 5. stand it up ---------------------------------------------------
step "5. starting a node and hub against the restored state"
"$BIN_DIR/node" --port "$NODE_PORT" --blockchain-file "$R/blockchain.redb" \
    > "$WORK/node.log" 2>&1 &
NODE_PID=$!
for _ in $(seq 30); do grep -q . "$WORK/node.log" && break; sleep 0.5; done

# --trusted-proxies is deliberately left empty here: the drill talks to
# the hub directly, and an empty list is the configuration that cannot be
# talked out of its rate limit.
"$BIN_DIR/hub" --port "$HUB_PORT" \
    --node-addresses "127.0.0.1:$NODE_PORT" \
    --store-file "$R/hub.redb" \
    --operator-key-file "$R/secrets/hub_operator.priv.cbor" \
    --exchange-custody-key-file "$R/secrets/hub_exchange_custody.priv.cbor" \
    --escrow-secret-file "$R/secrets/hub_escrow_secret.bin" \
    > "$WORK/hub.log" 2>&1 &
HUB_PID=$!

for _ in $(seq 60); do
    curl -sf "http://127.0.0.1:$HUB_PORT/health" >/dev/null 2>&1 && break
    kill -0 "$HUB_PID" 2>/dev/null || { cat "$WORK/hub.log"; fail "hub exited during startup"; }
    sleep 0.5
done

curl -sf "http://127.0.0.1:$HUB_PORT/health" >/dev/null \
    || { cat "$WORK/hub.log"; fail "hub never answered /health"; }
echo "hub answered /health against the restored store"

# --- 6. same deployment? ---------------------------------------------
#
# The real question. A hub that starts proves the files are well formed;
# it does not prove they are YOUR files. The operator address is derived
# from the restored operator key, so comparing it to the live one is what
# distinguishes "a working hub" from "our hub".
step "6. confirming the restored keys are the right keys"
RESTORED_OPERATOR=$(grep -A1 'hub operator address' "$WORK/hub.log" | tail -1 | tr -d '\r')
echo "restored operator address: $RESTORED_OPERATOR"
if [[ -n "$EXPECT_OPERATOR" ]]; then
    [[ "$RESTORED_OPERATOR" == "$EXPECT_OPERATOR" ]] \
        || fail "restored operator address does not match the live one"
    echo "matches the live operator address"
else
    echo "NOTE: no --expect-operator given. Compare the line above against"
    echo "      the live hub's own startup banner by hand."
fi

step "7. what came back"
grep -E 'loaded .* from store|agent name|restored .* replay' "$WORK/hub.log" || true
echo
echo "DRILL PASSED"
echo "scratch directory is being removed; nothing was written to the live state dir"

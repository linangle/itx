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
# service. It is NOT unconditionally safe on the production box, though,
# and the reason is worth reading before you schedule it: step 5 starts a
# hub holding the RESTORED PRODUCTION SECRETS, and both binaries hardcode
# a 0.0.0.0 bind with no --bind flag. Step 0 puts them in a private
# network namespace so that cannot reach anything; if it cannot, the
# drill says so and the host firewall is again the only thing between the
# real treasury keys and the internet (§1, and now on a second port).
#
# Usage:
#   itx-restore-drill.sh --archive /var/backups/itx/itx-<stamp>.tar.gz.age \
#                        --identity ~/age-restore-key.txt \
#                        [--expect-operator <pubkey>] \
#                        [--expect-escrow-sha256 <hex>] \
#                        [--bin-dir /usr/local/bin] [--port-base 19000] \
#                        [--no-isolate]
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
ISOLATE=1

while [[ $# -gt 0 ]]; do
    case "$1" in
        --archive)              ARCHIVE="$2";         shift 2 ;;
        --identity)             IDENTITY="$2";        shift 2 ;;
        --expect-operator)      EXPECT_OPERATOR="$2"; shift 2 ;;
        --expect-escrow-sha256) EXPECT_ESCROW="$2";   shift 2 ;;
        --bin-dir)              BIN_DIR="$2";         shift 2 ;;
        --port-base)            PORT_BASE="$2";       shift 2 ;;
        --gpg)                  USE_GPG=1;            shift ;;
        --no-isolate)           ISOLATE=0;            shift ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

[[ -n "$ARCHIVE" ]] || { echo "--archive is required" >&2; exit 2; }
[[ -f "$ARCHIVE" ]] || { echo "no such archive: $ARCHIVE" >&2; exit 1; }
[[ "$ARCHIVE" == *.gpg ]] && USE_GPG=1
if [[ $USE_GPG -eq 0 && -z "$IDENTITY" ]]; then
    echo "--identity is required for an age archive" >&2; exit 2
fi

# --- 0. isolate the network -------------------------------------------
#
# Checked after the arguments, so a typo fails here rather than inside a
# namespace.
#
# Steps 5-7 run a hub against the restored operator, custody and escrow
# keys -- the live ones. hub/src/main.rs binds `0.0.0.0:{port}` with no
# way to say otherwise, so without this the drill puts the real treasury
# keys on a listener facing every interface, on a port nothing in
# deploy/nftables.conf knows about, for as long as the drill runs.
#
# A private network namespace fixes it by construction rather than by
# policy: inside one, 0.0.0.0 is the namespace's own loopback and there
# is no route in or out at all, so the ports cannot be reached even if
# the host firewall is wrong. The probe runs the real thing in a
# throwaway namespace first, so an unavailable or blocked unshare (some
# hardened kernels and AppArmor profiles refuse unprivileged user
# namespaces) degrades to a warning instead of a broken drill.
if [[ -z "${ITX_DRILL_NETNS:-}" ]]; then
    if [[ $ISOLATE -eq 0 ]]; then
        echo "WARNING: --no-isolate. The hub started in step 5 holds the restored"
        echo "  PRODUCTION keys and binds 0.0.0.0:$((PORT_BASE + 1)). Only the host"
        echo "  firewall keeps that off the internet, and it has no rule for this"
        echo "  port. Confirm from off-box before relying on this."
    elif command -v unshare >/dev/null 2>&1 \
         && command -v ip >/dev/null 2>&1 \
         && unshare --net --map-root-user -- ip link set lo up >/dev/null 2>&1; then
        echo "== 0. isolating: re-running inside a private network namespace"
        export ITX_DRILL_NETNS=1
        # `sh -c` brings loopback up (a fresh namespace starts with lo
        # DOWN, and every bind below would fail with EADDRNOTAVAIL),
        # then re-runs this script with its original arguments.
        exec unshare --net --map-root-user -- \
            /bin/sh -c 'ip link set lo up && exec "$@"' sh "$0" "$@"
    else
        echo "WARNING: no private network namespace available (unshare/ip missing,"
        echo "  or unprivileged user namespaces are disabled). The hub started in"
        echo "  step 5 holds the restored PRODUCTION keys and will bind"
        echo "  0.0.0.0:$((PORT_BASE + 1)) on every interface. Only the host firewall"
        echo "  keeps that off the internet, and it has no rule for this port."
        echo "  Prefer running the drill on a machine that is not the hub box."
    fi
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

# --- 3. all hub secrets, then the escrow secret specifically ----------
#
# The one file whose bytes have to be exactly right. Escrow addresses are
# HKDF(secret, deposit id), so a single flipped bit produces a valid-
# looking hub that derives a different address for every deposit and
# sweeps nothing. The hub checks the secret against every restored derived
# deposit; this drill also checks its length and optional out-of-band
# fingerprint so an internally consistent but substituted archive is caught.
step "3. checking all hub secrets"
for NAME in \
    hub_operator.priv.cbor \
    hub_exchange_custody.priv.cbor \
    hub_escrow_secret.bin
do
    [[ -f "$R/secrets/$NAME" ]] || fail "$NAME is not in the backup"
done
echo "operator, exchange custody, and escrow secret files are present"

SECRET="$R/secrets/hub_escrow_secret.bin"
LEN=$(wc -c < "$SECRET" | tr -d ' ')
[[ "$LEN" == "32" ]] || fail "escrow secret is $LEN bytes, expected 32"
FP=$(sha256sum "$SECRET" | cut -d' ' -f1)
echo "32 bytes, sha256 $FP"
if [[ -n "$EXPECT_ESCROW" ]]; then
    [[ "$FP" == "$EXPECT_ESCROW" ]] \
        || fail "escrow secret fingerprint $FP does not match the expected $EXPECT_ESCROW"
    echo "matches the expected fingerprint"
else
    echo "WARNING: no --expect-escrow-sha256 given."
    echo "  This run proves the archive is INTERNALLY CONSISTENT, not that it is"
    echo "  yours. Backups are encrypted, not signed, and the recipient key is"
    echo "  public -- so anyone can produce an archive that decrypts cleanly and"
    echo "  whose own manifest agrees with its contents. The out-of-band expected"
    echo "  values are the only thing that detects a substituted archive."
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
"$BIN_DIR/itx-node" --port "$NODE_PORT" --blockchain-file "$R/blockchain.redb" \
    > "$WORK/node.log" 2>&1 &
NODE_PID=$!

# Wait for the node to actually be LISTENING, not merely to have said
# something. This loop used to break on any output at all, and the node's
# first line is "found N blocks in the local store, loading..." -- printed
# before it replays the chain and long before it binds. On a chain of any
# size the hub then started against a node that was not up yet, answered
# 503 to the health loop below, and the drill reported "hub never
# answered /health" for a restore that was fine.
for _ in $(seq 240); do
    grep -q 'Listening on ' "$WORK/node.log" && break
    kill -0 "$NODE_PID" 2>/dev/null \
        || { cat "$WORK/node.log"; fail "node exited during startup"; }
    sleep 0.5
done
grep -q 'Listening on ' "$WORK/node.log" \
    || { cat "$WORK/node.log"; fail "node never started listening (it replays the whole chain first; a very large store may need longer than the two minutes waited here)"; }
echo "node is listening on 127.0.0.1:$NODE_PORT"

# --trusted-proxies is deliberately left empty here: the drill talks to
# the hub directly, and an empty list is the configuration that cannot be
# talked out of its rate limit.
"$BIN_DIR/itx-hub" --port "$HUB_PORT" \
    --node-addresses "127.0.0.1:$NODE_PORT" \
    --store-file "$R/hub.redb" \
    --operator-key-file "$R/secrets/hub_operator.priv.cbor" \
    --exchange-custody-key-file "$R/secrets/hub_exchange_custody.priv.cbor" \
    --escrow-secret-file "$R/secrets/hub_escrow_secret.bin" \
    > "$WORK/hub.log" 2>&1 &
HUB_PID=$!

# `-o /dev/null -w %{http_code}` rather than `-sf`, because `-sf` fails
# on the 503 the hub returns while the node is unreachable and cannot
# tell that apart from no answer at all. Those are different diagnoses
# and the drill should say which one it got: "000" is the hub not
# listening yet, "503" is the hub up and the restored node not answering.
HEALTH_CODE=000
for _ in $(seq 60); do
    HEALTH_CODE=$(curl -s -o /dev/null -w '%{http_code}' \
        "http://127.0.0.1:$HUB_PORT/health") || HEALTH_CODE=000
    [[ "$HEALTH_CODE" != "000" ]] && break
    kill -0 "$HUB_PID" 2>/dev/null || { cat "$WORK/hub.log"; fail "hub exited during startup"; }
    sleep 0.5
done

case "$HEALTH_CODE" in
    200) echo "hub answered /health 200 against the restored store" ;;
    503) cat "$WORK/node.log"
         fail "hub is up but reports degraded -- it could not reach the restored node" ;;
    000) cat "$WORK/hub.log"; fail "hub never answered /health" ;;
    *)   cat "$WORK/hub.log"; fail "hub answered /health with $HEALTH_CODE" ;;
esac

if grep -q 'generating a new one' "$WORK/hub.log"; then
    cat "$WORK/hub.log"
    fail "the restored hub generated replacement key material"
fi

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
    echo "WARNING: no --expect-operator given. Compare the line above against"
    echo "  the live hub's own startup banner by hand -- see the note in step 3"
    echo "  for why an unchecked archive proves less than it appears to."
fi

step "7. what came back"
grep -E 'loaded .* from store|agent name|restored .* replay' "$WORK/hub.log" || true
echo
echo "DRILL PASSED"
echo "scratch directory is being removed; nothing was written to the live state dir"

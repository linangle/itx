#!/usr/bin/env bash
#
# Bring this deployment back on a box that has never seen it.
#
# This is the incident procedure, executable. `itx-restore-drill.sh`
# proves an archive is restorable *somewhere* -- into a scratch directory,
# on throwaway ports, without touching a live service. This one does the
# real thing: it installs the release, puts the restored state where the
# units read it, starts them in an order that works, and proves what came
# up is the same deployment. Run it on the new box.
#
# Usage:
#   itx-recover.sh --archive itx-<stamp>.tar.gz.age \
#                  --identity ~/itx-restore-key.txt \
#                  --release-dir ./itx-<version>-x86_64-linux \
#                  [--expect-operator <pubkey>] \
#                  [--state-dir /var/lib/itx] [--force]
#
# `--release-dir` is an unpacked release artifact: `itx-node`, `itx-hub`,
# `itx-miner`, `itx-console` and `deploy/`. `--expect-operator` is the
# address the old box printed at startup; without it this proves a hub
# came up, not that YOUR hub came up, and it says so at the end.
#
# It refuses to run over an existing store unless you pass --force. That
# refusal is the whole reason this is a script rather than a checklist:
# see the REFUSES block below.
#
# Rehearsed end to end on 2026-09-11 -- docs/deployment.md §7.6 has the
# numbers, the findings and what was NOT covered.
set -euo pipefail

ARCHIVE=""
IDENTITY=""
RELEASE_DIR=""
EXPECT_OPERATOR=""
STATE_DIR=${ITX_STATE_DIR:-/var/lib/itx}
USE_GPG=0
FORCE=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --archive)         ARCHIVE="$2";         shift 2 ;;
        --identity)        IDENTITY="$2";        shift 2 ;;
        --release-dir)     RELEASE_DIR="$2";     shift 2 ;;
        --expect-operator) EXPECT_OPERATOR="$2"; shift 2 ;;
        --state-dir)       STATE_DIR="$2";       shift 2 ;;
        --gpg)             USE_GPG=1;            shift ;;
        --force)           FORCE=1;              shift ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

[[ -n "$ARCHIVE"     ]] || { echo "--archive is required" >&2; exit 2; }
[[ -n "$RELEASE_DIR" ]] || { echo "--release-dir is required" >&2; exit 2; }
[[ -f "$ARCHIVE"     ]] || { echo "no archive at $ARCHIVE" >&2; exit 2; }
[[ -d "$RELEASE_DIR" ]] || { echo "no release directory at $RELEASE_DIR" >&2; exit 2; }
if [[ $USE_GPG -eq 0 ]]; then
    [[ -n "$IDENTITY" ]] || { echo "--identity is required (or pass --gpg)" >&2; exit 2; }
    [[ -f "$IDENTITY" ]] || { echo "no identity file at $IDENTITY" >&2; exit 2; }
fi
[[ $EUID -eq 0 ]] || { echo "run this as root -- it installs units and writes $STATE_DIR" >&2; exit 1; }

T0=$(date +%s)
step() { printf '\n== %s  [t+%ss]\n' "$1" "$(( $(date +%s) - T0 ))"; }
fail() { printf '\nFAILED: %s\n' "$*" >&2; exit 1; }

WORK=$(mktemp -d)
chmod 700 "$WORK"
# The scratch tree holds decrypted secrets between the tar and the copy.
trap 'rm -rf "$WORK"' EXIT

# --- REFUSES ----------------------------------------------------------
#
# The failure this exists to make impossible, measured on 2026-09-11:
# install the units and `systemctl enable --now` them on a fresh box
# *before* the archive is in place, and `itx-hub.service`'s
# `--generate-keys` is first-boot authority. The hub mints a new operator
# key, a new custody key and a new escrow secret, answers /health with
# `200`, and systemd reports it active. Nothing on the box says anything
# is wrong. The board is empty and the treasury is somebody else's --
# except it is nobody's, because those keys are thirty seconds old.
#
# The one tell is in the journal, and only if you go looking:
#   creating missing operator private key at ... because --generate-keys was supplied
#
# So this refuses to write over a store, and refuses to start units that
# are already running. If you are recovering onto a box that has already
# minted a treasury, stop the units and remove the WHOLE state directory
# first -- not just hub.redb. A minted escrow secret left beside a
# restored store derives a different address for every deposit ever
# handed out, silently, and the hub will not notice.
step "0. checking this box is actually fresh"
for unit in itx-node itx-hub itx-miner; do
    if systemctl is-active --quiet "$unit" 2>/dev/null; then
        [[ $FORCE -eq 1 ]] || fail "$unit is running. redb is single-process: stop the units before anything else opens the store (§9.7). Then re-run, or pass --force if you know why."
        echo "  WARNING: $unit is running and --force was given"
    fi
done
if [[ -e "$STATE_DIR/hub.redb" || -e "$STATE_DIR/secrets/hub_operator.priv.cbor" ]]; then
    [[ $FORCE -eq 1 ]] || fail "$STATE_DIR already holds a store or a key. If this box minted its own treasury by being started before the restore, stop the units and remove $STATE_DIR entirely -- a surviving escrow secret is worse than no restore at all. Then re-run."
    echo "  WARNING: $STATE_DIR is not empty and --force was given"
fi
echo "  nothing running, nothing to overwrite"

step "1. the itx user and the state directory"
id -u itx >/dev/null 2>&1 \
    || useradd --system --home "$STATE_DIR" --shell /usr/sbin/nologin itx
mkdir -p "$STATE_DIR/secrets"
chown -R itx:itx "$STATE_DIR"
chmod 700 "$STATE_DIR/secrets"

step "2. the binaries, under their itx- names"
# Never their build names: `node` and `hub` in /usr/local/bin shadow
# Node.js and GitHub's `hub` for every user on the box, and a treasury
# host is a bad place to discover that. See §5.
# wallet too: §9.10's manual payout, which a recovery is the likeliest
# time to need, and which no install step put on a box until 2026-09-15.
for b in node hub miner console wallet; do
    [[ -f "$RELEASE_DIR/itx-$b" ]] || fail "no itx-$b in $RELEASE_DIR"
    install -m 0755 "$RELEASE_DIR/itx-$b" "/usr/local/bin/itx-$b"
done

step "3. the units -- installed, not started"
[[ -d "$RELEASE_DIR/deploy" ]] || fail "no deploy/ in $RELEASE_DIR"
cp "$RELEASE_DIR"/deploy/itx-*.service "$RELEASE_DIR"/deploy/itx-*.timer /etc/systemd/system/
systemctl daemon-reload

step "4. decrypting"
# Also proves you can still lay hands on the offline identity, which is
# the half of "do we have backups" that people actually fail.
if [[ $USE_GPG -eq 1 ]]; then
    gpg --batch --yes --decrypt "$ARCHIVE" | tar -C "$WORK" -xzf -
else
    age --decrypt --identity "$IDENTITY" "$ARCHIVE" | tar -C "$WORK" -xzf -
fi
[[ -d "$WORK/itx" ]] || fail "the archive did not contain an itx/ directory"

step "5. verifying the manifest"
# Proves the archive round-tripped rather than merely decrypted.
( cd "$WORK/itx" && sha256sum -c --quiet MANIFEST.sha256 ) \
    || fail "the archive does not match its own manifest -- do not restore this"
echo "  manifest verified"
sed 's/^/  /' "$WORK/itx/MANIFEST.txt"

step "6. restoring into $STATE_DIR"
cp -a "$WORK/itx/." "$STATE_DIR/"
chown -R itx:itx "$STATE_DIR"
chmod 700 "$STATE_DIR/secrets"
chmod 600 "$STATE_DIR"/secrets/*
# tar and cp do not reliably carry mode through every path, and the hub
# only chmods files it CREATES -- a restored secret keeps whatever mode
# it arrived with, permanently and silently. Hence the explicit chmod
# above, and this check that it took.
BAD=$(find "$STATE_DIR/secrets" -type f ! -perm 600 -print)
[[ -z "$BAD" ]] || fail "restored secrets are not 0600:"$'\n'"$BAD"
# For a human to read in the transcript of a recovery, not to parse --
# these are filenames this deployment wrote.
# shellcheck disable=SC2012
ls -la "$STATE_DIR" | sed 's/^/  /'

step "7. starting the node, and waiting for its listener"
# The order is not the units' order, and that is deliberate.
#
# `itx-hub.service` is `After=itx-node.service`, which orders the *starts*
# and does not wait for the node's socket. Measured on a fresh-host
# recovery: systemd started both 2ms apart, the node took 329ms to replay
# 22 blocks and bind, and the hub ran its boot-time wallet maintenance
# 296ms before there was anything to talk to -- leaving
# `hub_operator_fan_out_failures_total` at 2 on a box that had just come
# back. It self-heals (the sweep re-runs the same routine every 60s) and
# it is still worth not doing: the node's replay grows with the chain,
# so on a real chain the hub loses that race by more, and §5 tells an
# operator to read that counter. Starting them in sequence keeps it at 0.
systemctl start itx-node
for _ in $(seq 600); do
    journalctl -u itx-node -b -o cat --no-pager 2>/dev/null | grep -q 'Listening on ' && break
    systemctl is-active --quiet itx-node || fail "itx-node exited during startup -- journalctl -u itx-node -b"
    sleep 0.5
done
journalctl -u itx-node -b -o cat --no-pager 2>/dev/null | grep -q 'Listening on ' \
    || fail "the node never started listening. It replays the whole chain before it binds, so a large store may need longer than the five minutes waited here."
echo "  node is listening"

step "8. starting the hub and the miner"
systemctl start itx-hub itx-miner
# `systemctl is-active` reports active the instant a Type=simple unit
# forks, which is before the hub has bound its port -- so a health check
# fired straight after a start gets a connection refused from a service
# systemd calls healthy. Poll the port, never the unit.
CODE=000
for _ in $(seq 120); do
    CODE=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:9100/health 2>/dev/null || echo 000)
    [[ "$CODE" == "200" ]] && break
    systemctl is-active --quiet itx-hub || fail "itx-hub exited during startup -- journalctl -u itx-hub -b"
    sleep 1
done
case "$CODE" in
    200) echo "  hub answered /health 200" ;;
    503) fail "the hub is up and reports degraded -- it cannot reach the node it just started. journalctl -u itx-node -b" ;;
    000) fail "the hub never answered /health -- journalctl -u itx-hub -b" ;;
    *)   fail "the hub answered /health with $CODE" ;;
esac

step "9. is it the same deployment?"
# A hub that starts proves the files are well-formed; it does not prove
# they are YOURS. The operator address is derived from the restored key,
# so matching it is what separates "a working hub" from "our hub".
BANNER=$(journalctl -o cat -u itx-hub -b --no-pager)
if grep -q 'because --generate-keys was supplied' <<<"$BANNER"; then
    fail "the hub GENERATED key material instead of using the restored keys. This box is now a different deployment. Stop the units, remove $STATE_DIR entirely, and start again."
fi
OPERATOR=$(grep -A1 'hub operator address' <<<"$BANNER" | tail -1 | tr -d '\r')
OPERATOR=${OPERATOR##* }
echo "  operator address: $OPERATOR"
if [[ -n "$EXPECT_OPERATOR" ]]; then
    # Keeps the last whitespace-separated field, so a value pasted out of
    # `journalctl` with its default prefix still compares.
    [[ "$OPERATOR" == "${EXPECT_OPERATOR##* }" ]] \
        || fail "restored operator address $OPERATOR does not match the expected ${EXPECT_OPERATOR##* }"
    echo "  matches the expected operator address"
else
    echo "  WARNING: no --expect-operator given. This proves a hub came up,"
    echo "    not that it is yours. Compare the address above against the old"
    echo "    box's startup banner by hand before you point a domain at this."
fi

step "10. what came back"
grep -E 'from store|replay-guard|faucet challenge' <<<"$BANNER" | sed 's/^/  /' || true
echo
echo "  $(curl -s http://127.0.0.1:9100/health)"

RTO=$(( $(date +%s) - T0 ))
echo
echo "RECOVERED in ${RTO}s"
echo
echo "Still to do by hand, none of which this script can know:"
echo "  - point DNS at this box, and install the proxy config and TLS (§4)"
echo "  - install the site and set its itx-hub-url meta tag (§5.1)"
echo "  - apply the firewall BEFORE the node is reachable from outside (§3)"
echo "  - if you are cutting over from a box that is still running, stop it"
echo "    first: two hubs on one chain both pay out, and neither knows"
echo "  - take a fresh backup from this box; the archive you restored is"
echo "    now the only copy of a state that has already moved on"

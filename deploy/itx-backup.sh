#!/usr/bin/env bash
#
# Encrypted backup of an ITX box.
#
# Backs up the three secrets, the hub's store, and the chain. Encrypts to
# a PUBLIC key, so this box can write backups it cannot read back -- if
# the hub is compromised, the attacker gets the running secrets (which
# they already had) but not the archive of every previous state.
#
# Usage:
#   itx-backup.sh --recipient age1xxxx...            [--dest DIR] [--keep N]
#   itx-backup.sh --recipient ops@example.com --gpg  [--dest DIR] [--keep N]
#
# The recipient's PRIVATE key must not live on this box. That is the
# whole point; putting it here to "make restores easier" gives the
# property away.
#
# See docs/deployment.md §7 for the consistency tradeoff this makes and
# §7.4 for the drill that proves the output is restorable.

set -euo pipefail

STATE_DIR=${ITX_STATE_DIR:-/var/lib/itx}
DEST=${ITX_BACKUP_DEST:-/var/backups/itx}
RECIPIENT=""
USE_GPG=0
KEEP=14
STOP_SERVICES=1

while [[ $# -gt 0 ]]; do
    case "$1" in
        --recipient) RECIPIENT="$2"; shift 2 ;;
        --dest)      DEST="$2";      shift 2 ;;
        --keep)      KEEP="$2";      shift 2 ;;
        --gpg)       USE_GPG=1;      shift ;;
        --no-stop)   STOP_SERVICES=0; shift ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

[[ -n "$RECIPIENT" ]] || { echo "--recipient is required" >&2; exit 2; }

if [[ $USE_GPG -eq 1 ]]; then
    command -v gpg >/dev/null || { echo "gpg not found" >&2; exit 1; }
else
    command -v age >/dev/null || {
        echo "age not found -- install it, or pass --gpg to use gpg instead" >&2
        exit 1
    }
fi

STAMP=$(date -u +%Y%m%dT%H%M%SZ)
WORK=$(mktemp -d)
# The staging directory holds plaintext secrets. Remove it on every exit
# path, including the error ones, or a failed backup leaves a
# world-invisible-but-still-plaintext copy behind.
trap 'rm -rf "$WORK"' EXIT
chmod 700 "$WORK"

mkdir -p "$DEST"
chmod 700 "$DEST"

# --- consistency ------------------------------------------------------
#
# hub.redb and blockchain.redb are redb files with a live writer. Copying
# one underneath a running process can capture a torn state, and redb
# will refuse to open the result -- which you would discover during the
# restore, i.e. exactly when you cannot afford to.
#
# So by default this stops the writers for the length of the copy. On a
# box of any reasonable size that is seconds, and it is the only way to
# get a consistent copy without filesystem snapshots.
#
# --no-stop skips it. Only use that when $STATE_DIR is on LVM/ZFS/btrfs
# and you are snapshotting underneath this script -- not to avoid a
# short outage.
if [[ $STOP_SERVICES -eq 1 ]]; then
    echo "stopping itx-hub and itx-node for a consistent copy"
    systemctl stop itx-hub itx-node
    # The miner will restart-loop while the node is down (see
    # itx-miner.service); it recovers on its own and needs no handling.
    resume() {
        systemctl start itx-node itx-hub
        rm -rf "$WORK"
    }
    trap resume EXIT
fi

echo "staging"
mkdir -p "$WORK/itx"
cp -a "$STATE_DIR/secrets"          "$WORK/itx/secrets"
cp -a "$STATE_DIR/hub.redb"         "$WORK/itx/hub.redb"
cp -a "$STATE_DIR/blockchain.redb"  "$WORK/itx/blockchain.redb"

# --- manifest ---------------------------------------------------------
#
# Checksums of what went in, so the drill can prove the archive round
# tripped rather than merely decrypting. The escrow secret's fingerprint
# is called out separately because it is the one file whose CONTENT has
# to be byte-identical for the restore to be worth anything: escrow
# addresses are HKDF(secret, deposit id), so one flipped bit derives a
# different address for every deposit ever made, silently.
(
    cd "$WORK/itx"
    find . -type f -print0 | sort -z | xargs -0 sha256sum > "$WORK/MANIFEST.sha256"
)
ESCROW_FP=$(sha256sum "$WORK/itx/secrets/hub_escrow_secret.bin" | cut -d' ' -f1)
ESCROW_LEN=$(wc -c < "$WORK/itx/secrets/hub_escrow_secret.bin" | tr -d ' ')

cat > "$WORK/MANIFEST.txt" <<META
itx backup
created:            $STAMP
host:               $(hostname)
state dir:          $STATE_DIR
consistent copy:    $([[ $STOP_SERVICES -eq 1 ]] && echo "yes (services stopped)" || echo "no (--no-stop; snapshot assumed)")
escrow secret sha256: $ESCROW_FP
escrow secret bytes:  $ESCROW_LEN
META

cp "$WORK/MANIFEST.sha256" "$WORK/itx/MANIFEST.sha256"
cp "$WORK/MANIFEST.txt"    "$WORK/itx/MANIFEST.txt"

# --- encrypt ----------------------------------------------------------
OUT="$DEST/itx-$STAMP.tar.gz.age"
[[ $USE_GPG -eq 1 ]] && OUT="$DEST/itx-$STAMP.tar.gz.gpg"

echo "encrypting to $OUT"
if [[ $USE_GPG -eq 1 ]]; then
    tar -C "$WORK" -czf - itx \
        | gpg --batch --yes --trust-model always \
              --encrypt --recipient "$RECIPIENT" --output "$OUT"
else
    tar -C "$WORK" -czf - itx \
        | age --recipient "$RECIPIENT" --output "$OUT"
fi
chmod 600 "$OUT"

# The fingerprint is also recorded in cleartext beside the archive, so a
# drill can check it without decrypting, and so the value is visible to
# someone reading the backup directory who does not hold the key. It is a
# hash of a secret, not the secret.
cp "$WORK/MANIFEST.txt" "$OUT.manifest.txt"

echo "pruning to the last $KEEP"
ls -1t "$DEST"/itx-*.tar.gz.* 2>/dev/null | grep -v '\.manifest\.txt$' | tail -n "+$((KEEP+1))" | while read -r old; do
    rm -f -- "$old" "$old.manifest.txt"
done

echo "done: $OUT ($(du -h "$OUT" | cut -f1))"
echo "escrow secret fingerprint: $ESCROW_FP"

#!/usr/bin/env bash
#
# Encrypted backup of an ITX box.
#
# Backs up the three secrets, the hub's store, the chain, and the miner
# key. Encrypts to a PUBLIC key, so this box can write backups it cannot
# read back -- if the hub is compromised, the attacker gets the running
# secrets (which they already had) but not the archive of every previous
# state.
#
# Usage:
#   itx-backup.sh --recipient age1xxxx...            [--dest DIR] [--keep N]
#   itx-backup.sh --recipient ops@example.com --gpg  [--dest DIR] [--keep N]
#
#   --no-stop     stop nothing (you are snapshotting underneath this)
#   --stop-node   also stop the node -- DISCARDS ITS MEMPOOL, read below
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
STOP_HUB=1
STOP_NODE=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --recipient) RECIPIENT="$2"; shift 2 ;;
        --dest)      DEST="$2";      shift 2 ;;
        --keep)      KEEP="$2";      shift 2 ;;
        --gpg)       USE_GPG=1;      shift ;;
        --no-stop)   STOP_HUB=0; STOP_NODE=0; shift ;;
        --stop-node) STOP_NODE=1;    shift ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

[[ -n "$RECIPIENT" ]] || { echo "--recipient is required" >&2; exit 2; }

# --no-stop means stop nothing, whatever order the flags arrived in.
# "stop nothing" is the safe reading of a contradictory pair, and it also
# keeps the manifest below describing what actually happened.
[[ $STOP_HUB -eq 1 ]] || STOP_NODE=0

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
chmod 700 "$WORK"

# Set once the encryption starts, so a failure part way through does not
# leave a truncated archive behind (see the encrypt section).
PARTIAL=""
# Whatever this run actually stopped, so cleanup restarts exactly that.
STOPPED=()

# The staging directory holds plaintext secrets. Remove it on every exit
# path, including the error ones, or a failed backup leaves a
# world-invisible-but-still-plaintext copy behind. Same for anything we
# stopped: a backup that dies half way must not leave the hub down.
cleanup() {
    [[ -z "$PARTIAL" ]] || rm -f -- "$PARTIAL"
    if [[ ${#STOPPED[@]} -gt 0 ]]; then
        systemctl start "${STOPPED[@]}" \
            || echo "WARNING: could not restart ${STOPPED[*]} -- start them by hand" >&2
    fi
    rm -rf "$WORK"
}
trap cleanup EXIT

mkdir -p "$DEST"
chmod 700 "$DEST"

# --- consistency ------------------------------------------------------
#
# hub.redb and blockchain.redb are redb files with a live writer, and
# copying one underneath a running process can capture a state that no
# single instant ever had: `cp` reads the file sequentially over some
# hundreds of milliseconds, and redb (copy-on-write, two-phase commit) is
# free to rewrite pages behind the read head while it does. There are two
# outcomes and the second is the dangerous one:
#
#   - redb refuses to open the copy. Unpleasant, and you find out during
#     a restore, but at least you find out.
#   - the copy opens cleanly and is WRONG -- a coherent-looking mix of
#     two commits, missing rows that the god byte says are there. Nothing
#     reports this. The drill in §7.4 passes on it.
#
# What this script does NOT do any more is stop the node to avoid that.
# It used to, and it was the most expensive line in the deployment: the
# node's mempool is memory only (btclib::store has `blocks`, `meta` and
# `bans` and nothing else; node/src/util.rs persist_chain_state writes
# blocks and the active chain), while the hub marks a payout done the
# moment the node ACCEPTS it -- submit_transaction is fire-and-forget and
# the sweep only retries tasks still `Verified`. So stopping the node
# silently destroyed every bounty payout, faucet grant, escrow refund,
# dispute-bond settlement, exchange withdrawal and custody sweep
# submitted since the last block, while the task stayed `Paid` and the
# exchange account stayed debited. Nightly. There is no observable
# "mempool is empty" to wait for: the node exposes no such query, and a
# bare TCP probe of port 9000 gets the box banned for an hour (§8.1).
#
# So: the hub stops (its writes are all durable, it restarts in seconds,
# and it is what makes hub.redb consistent), the node keeps running, and
# blockchain.redb is copied hot. The residual risk is a torn
# blockchain.redb, which `snapshot_copy` below reduces and does not
# remove -- see §7.2 for what to do about it.
#
# --no-stop stops nothing. Use it when $STATE_DIR is on LVM/ZFS/btrfs and
# you are snapshotting underneath this script.
#
# --stop-node re-enables the old behaviour for a planned maintenance
# backup, where you have already drained the hub and waited out a block.
# Never put it in a cron line.
if [[ $STOP_NODE -eq 1 ]]; then
    echo "WARNING: --stop-node discards the node's mempool."
    echo "  Every transaction submitted since the last mined block is lost,"
    echo "  and the hub already recorded those payouts as done. Only do this"
    echo "  when the hub is drained and a block has been mined since the last"
    echo "  payout. See docs/deployment.md §7.2."
fi
if [[ $STOP_HUB -eq 1 ]]; then
    STOPPED=(itx-hub)
    [[ $STOP_NODE -eq 0 ]] || STOPPED=(itx-node itx-hub)
    echo "stopping ${STOPPED[*]} for the length of the copy"
    # Started in the reverse order by cleanup()'s single `systemctl
    # start`, which systemd orders by the units' own After= anyway.
    systemctl stop "${STOPPED[@]}"
    # The miner will restart-loop if the node is down (see
    # itx-miner.service); it recovers on its own and needs no handling.
fi

# Copies one file, preferring a filesystem-level clone over a read.
#
# `cp --reflink=always` succeeds only where the filesystem supports
# reflinks (btrfs, XFS with reflink=1), and there it is a single FICLONE:
# the destination is the source exactly as it existed at one instant.
# That is the crash-consistent image redb is built to recover from, so it
# turns the torn-copy risk above into no risk at all. Everywhere else it
# fails and we fall back to an ordinary sequential read, which is the
# copy that can tear. Which one happened is recorded in the manifest
# rather than assumed.
COPY_METHOD=reflink
snapshot_copy() {
    local src=$1 dst=$2
    if ! cp --reflink=always --preserve=all "$src" "$dst" 2>/dev/null; then
        cp -a "$src" "$dst"
        COPY_METHOD=sequential
    fi
}

echo "staging"
mkdir -p "$WORK/itx"
cp -a "$STATE_DIR/secrets" "$WORK/itx/secrets"
snapshot_copy "$STATE_DIR/hub.redb"        "$WORK/itx/hub.redb"
snapshot_copy "$STATE_DIR/blockchain.redb" "$WORK/itx/blockchain.redb"

# The miner key. `miner.pub.pem` is the address block rewards pay to and
# is genuinely public; `miner.priv.cbor`, which key_gen writes beside it,
# is the only thing that can ever SPEND those rewards. §6.5 says to
# generate the pair off this box and copy only the public half here, in
# which case there is nothing private to find and this loop just picks up
# the public file (small, and it saves a rebuild a lookup).
#
# If the private half is here anyway -- which is what happens when
# someone runs key_gen on the box, and it is the common case -- back it
# up rather than pretending it is not there. Leaving it out of the
# archive is how a deployment loses every block reward it ever mined.
for f in miner.pub.pem miner.priv.cbor; do
    [[ -e "$STATE_DIR/$f" ]] && cp -a "$STATE_DIR/$f" "$WORK/itx/$f"
done
MINER_PRIVATE=no
if [[ -e "$STATE_DIR/miner.priv.cbor" ]]; then
    MINER_PRIVATE=yes
    echo "note: miner.priv.cbor is on this box, so it is in this archive."
    echo "  It spends every block reward this deployment has earned."
    echo "  docs/deployment.md §6.5 says to keep it off the hub box."
fi

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

# Says what was actually done, not whether it was "consistent" -- the
# copy method and what was running are the two facts a restore needs, and
# only one of them used to be recorded.
cat > "$WORK/MANIFEST.txt" <<META
itx backup
created:            $STAMP
host:               $(hostname)
state dir:          $STATE_DIR
hub stopped:        $([[ $STOP_HUB -eq 1 ]] && echo "yes" || echo "no")
node stopped:       $([[ $STOP_NODE -eq 1 ]] && echo "yes -- mempool discarded" || echo "no -- mempool intact")
redb copy method:   $([[ $COPY_METHOD == reflink ]] && echo "reflink (point-in-time clone)" || echo "sequential (a live file may tear)")
miner private key:  $MINER_PRIVATE
escrow secret sha256: $ESCROW_FP
escrow secret bytes:  $ESCROW_LEN
META

cp "$WORK/MANIFEST.sha256" "$WORK/itx/MANIFEST.sha256"
cp "$WORK/MANIFEST.txt"    "$WORK/itx/MANIFEST.txt"

# --- encrypt ----------------------------------------------------------
OUT="$DEST/itx-$STAMP.tar.gz.age"
[[ $USE_GPG -eq 1 ]] && OUT="$DEST/itx-$STAMP.tar.gz.gpg"

# Written to a temp name and renamed into place, because `age`/`gpg`
# dying mid-stream used to leave a truncated archive sitting at $OUT --
# where the prune below counted it toward --keep, so a run of bad nights
# could age out every good archive in favour of partial ones. The rename
# is atomic within the directory, so $OUT either does not exist or is a
# complete archive. The leading dot keeps the temp name out of the
# `itx-*` glob the prune walks; cleanup() removes it on any failure.
PARTIAL="$DEST/.itx-$STAMP.partial"

echo "encrypting to $OUT"
if [[ $USE_GPG -eq 1 ]]; then
    tar -C "$WORK" -czf - itx \
        | gpg --batch --yes --trust-model always \
              --encrypt --recipient "$RECIPIENT" --output "$PARTIAL"
else
    tar -C "$WORK" -czf - itx \
        | age --recipient "$RECIPIENT" --output "$PARTIAL"
fi
chmod 600 "$PARTIAL"
mv -f -- "$PARTIAL" "$OUT"
PARTIAL=""

# The fingerprint is also recorded in cleartext beside the archive, so a
# drill can check it without decrypting, and so the value is visible to
# someone reading the backup directory who does not hold the key. It is a
# hash of a secret, not the secret.
cp "$WORK/MANIFEST.txt" "$OUT.manifest.txt"

echo "pruning to the last $KEEP"
# Read into an array first so an empty result is an empty array rather
# than a failing `grep` taking the whole script down under pipefail. A
# `while read` loop rather than `mapfile`, which is bash 4 and up -- the
# target hosts have bash 5, but a backup script is a bad place to depend
# on that.
ARCHIVES=()
while IFS= read -r archive; do
    ARCHIVES+=("$archive")
done < <(
    # `ls -t` because the prune is by age and a glob cannot sort. These
    # names are written by this script and contain no newlines.
    # shellcheck disable=SC2010,SC2012
    ls -1t "$DEST"/itx-*.tar.gz.* 2>/dev/null | grep -v '\.manifest\.txt$' || true
)
if (( ${#ARCHIVES[@]} > KEEP )); then
    for old in "${ARCHIVES[@]:KEEP}"; do
        rm -f -- "$old" "$old.manifest.txt"
    done
fi

echo "done: $OUT ($(du -h "$OUT" | cut -f1))"
echo "escrow secret fingerprint: $ESCROW_FP"

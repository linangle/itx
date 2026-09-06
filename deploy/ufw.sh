#!/usr/bin/env bash
# ITX single-box firewall, ufw edition. Equivalent to deploy/nftables.conf
# but less explicit: the node and hub ports are protected by default-deny
# inbound rather than by a rule that names them, so the intent is not
# visible in `ufw status`. Prefer nftables where you have the choice.
#
# See docs/deployment.md §1 for why this is the only control keeping
# cleartext hub traffic off the internet, not a second layer.
#
# Usage: ufw.sh [--reset]
set -euo pipefail

RESET=0
case "${1-}" in
    --reset) RESET=1 ;;
    "")      ;;
    *) echo "usage: $0 [--reset]" >&2; exit 2 ;;
esac

# This script used to open with `ufw --force reset` and claim the box was
# "never briefly reachable with no rules loaded". The opposite was true:
# reset DISABLES ufw and deletes every rule, and nothing at all is
# enforced from there until the `--force enable` at the bottom. On a
# treasury box that is a real, if short, window with the hub's cleartext
# port on the internet.
#
# So the default path never disables the firewall. Every command below
# either tightens the running config or is idempotent, and `enable` at
# the end only ever turns ufw on. The cost of not resetting is that rules
# left by a previous configuration are not removed -- `ufw status
# verbose` at the end is there to be read, not skipped.
#
# --reset restores the old behaviour when you genuinely need a clean
# slate. Run it from the console, not over the ssh session it is about to
# drop.
if [[ $RESET -eq 1 ]]; then
    echo "WARNING: --reset disables ufw and deletes every rule."
    echo "  The box has NO firewall between here and the enable below,"
    echo "  including on the hub's cleartext port. Ctrl-C now if this is"
    echo "  a remote session you cannot afford to lose."
    sudo ufw --force reset
fi

# Order matters within this block. `default deny incoming` takes effect
# immediately on an already-enabled ufw, so the ssh rule goes in right
# behind it -- established connections survive either way (ufw always
# permits established/related), but a new login attempt in the gap would
# not.
sudo ufw default deny incoming
sudo ufw default allow outgoing

# Narrow this to the addresses you administer from if you can. `ufw
# limit` covers IPv4 and IPv6 together, which is the trap the nftables
# version has to work around by hand.
sudo ufw limit 22/tcp comment 'ssh, rate limited'

sudo ufw allow 80/tcp  comment 'http, redirects to https'
sudo ufw allow 443/tcp comment 'the entire public surface'

# 9100 (hub) and 9000 (node) are deliberately absent: default-deny covers
# them, and loopback is always allowed, which is how the proxy reaches the
# hub and the hub reaches the node.

sudo ufw --force enable
sudo ufw status verbose

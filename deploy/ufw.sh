#!/usr/bin/env bash
# ITX single-box firewall, ufw edition. Equivalent to deploy/nftables.conf
# but less explicit: the node and hub ports are protected by default-deny
# inbound rather than by a rule that names them, so the intent is not
# visible in `ufw status`. Prefer nftables where you have the choice.
#
# See docs/deployment.md §1 for why this is the only control keeping
# cleartext hub traffic off the internet, not a second layer.
set -euo pipefail

# Ordered so the box is never briefly reachable with no rules loaded, and
# never has default-deny active without an ssh rule.
sudo ufw --force reset
sudo ufw default deny incoming
sudo ufw default allow outgoing

# Narrow this to the addresses you administer from if you can.
sudo ufw limit 22/tcp comment 'ssh, rate limited'

sudo ufw allow 80/tcp  comment 'http, redirects to https'
sudo ufw allow 443/tcp comment 'the entire public surface'

# 9100 (hub) and 9000 (node) are deliberately absent: default-deny covers
# them, and loopback is always allowed, which is how the proxy reaches the
# hub and the hub reaches the node.

sudo ufw --force enable
sudo ufw status verbose

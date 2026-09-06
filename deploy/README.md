# deploy/

Configuration for running a public ITX stack. Every file here is meant to be
copied and edited — the domains, addresses, and recipients are placeholders.

The reasoning behind all of it is in [`../docs/deployment.md`](../docs/deployment.md);
this directory is the artifacts, not the explanation.

| File | What it is |
|---|---|
| `Caddyfile` | reverse proxy + TLS. **Prefer this one.** |
| `nginx.conf` | the same, for hosts already running nginx |
| `nftables.conf` | host firewall — the only thing keeping the hub's cleartext port off the internet |
| `ufw.sh` | the same, for hosts using ufw |
| `itx-node.service` | blockchain node unit |
| `itx-hub.service` | hub unit — the process holding all three secrets |
| `itx-miner.service` | miner unit |
| `itx-backup.sh` | encrypted backup, to a public key the box cannot read back |
| `itx-restore-drill.sh` | restores a backup into a scratch dir and proves it is the right one |
| `security.txt` | RFC 9116 disclosure contact, served by the proxy |

Three things in here are security controls rather than tuning, and each is
commented where it appears:

- `X-Forwarded-For` is **replaced**, not appended (`Caddyfile`, `nginx.conf`) —
  it decides rate-limit identity.
- The node's port is **never** exposed (`nftables.conf`) — the wire protocol is
  unauthenticated.
- Backups encrypt to a key whose private half is **not on the box**
  (`itx-backup.sh`).

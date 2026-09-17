# deploy/

Configuration for running a public ITX stack. Every file here is meant to be
copied and edited — the domains, addresses, and recipients are placeholders.

The reasoning behind all of it is in [`../docs/deployment.md`](../docs/deployment.md);
this directory is the artifacts, not the explanation.

| File | What it is |
|---|---|
| `Caddyfile` | reverse proxy + TLS, and the static site. **Prefer this one.** Two site blocks: the board at the apex, the hub's API on `hub.<domain>`. Both answer `503` with `Retry-After` when the hub is down, in the shape the caller can read — see §4.8 |
| `nginx.conf` | the same, for hosts already running nginx. Three `server` blocks: the cleartext redirect for both names, the API, and the site. **A fragment, not a whole config** — two `server` blocks for `sites-available/`, to be `include`d from the distribution's own `nginx.conf`. Handing it to `nginx -t -c` directly fails with "server directive is not allowed here", which is the scaffolding being absent rather than the file being wrong |
| `nftables.conf` | host firewall — the only thing keeping the hub's cleartext port off the internet |
| `ufw.sh` | the same, for hosts using ufw |
| `itx-node.service` | blockchain node unit |
| `itx-hub.service` | hub unit — the process holding all three secrets |
| `itx-miner.service` | miner unit |
| `itx-backup.sh` | encrypted backup, to a public key the box cannot read back, copied off the box with `--remote` |
| `itx-backup.service`, `itx-backup.timer` | the nightly schedule for it. Enable the timer; the recipient and the off-box destination come from `/etc/itx/backup.env` (§7.1) |
| `itx-restore-drill.sh` | restores a backup into a scratch dir and proves it is the right one. The Tuesday check — nothing live is touched |
| `itx-recover.sh` | brings the deployment back on a box that has never seen it: checks the release directory against its `SHA256SUMS` before installing a thing, installs the release, restores into `/var/lib/itx`, starts the units in an order that works, proves the operator address matches. The incident script. Rehearsed end to end — §7.6. `--skip-release-checksums` exists and prints a paragraph saying what you gave up |
| `security.txt` | RFC 9116 disclosure contact, served by the proxy |
| `sshd-itx.conf` | key-only SSH, as a drop-in for `/etc/ssh/sshd_config.d/`. **Install it as `01-itx.conf`** — sshd takes the first value it reads for a keyword and cloud images ship a `50-cloud-init.conf` that often turns password auth back on |
| `apt-unattended-upgrades.conf` | nightly security patching for the distribution's own packages, the sshd and proxy among them. **Install it as `52-itx-unattended-upgrades`** — apt takes the last value, so it has to sort after the package's own file |
| `fail2ban-itx.local` | bans the addresses that keep failing to log in, into fail2ban's own nftables table. Worth it for the quiet, not for the keys it protects — `sshd-itx.conf` is what does that |

Four things in here are security controls rather than tuning, and each is
commented where it appears:

- `X-Forwarded-For` is **replaced**, not appended (`Caddyfile`, `nginx.conf`) —
  it decides rate-limit identity.
- The request path reaches the hub **unrewritten** (`Caddyfile`, `nginx.conf`) —
  the signed envelope binds it, so a trailing slash on `proxy_pass` is a 100%
  `401` rate.
- The node's port is **never** exposed (`nftables.conf`) — the wire protocol is
  unauthenticated.
- Backups encrypt to a key whose private half is **not on the box**
  (`itx-backup.sh`).
- SSH takes **keys only** (`sshd-itx.conf`) — port 22 and the proxy's ports are
  the whole of what answers strangers, and this is the one that hands out a
  shell on the box that can read the secrets directory.

Neither proxy config is a whole deployment on its own any more: the apex serves
`status.html` from the site tarball as its outage and maintenance page, and both
blocks read `/var/www/itx/maintenance.flag` to tell a planned stop from a
failure. `sudo touch` that file to open a window and `rm` it to close one —
nothing is reloaded either way, and while it is up `/status`, `/llms.txt`,
`/health` and `/.well-known/security.txt` all keep answering normally, so
monitoring still measures the hub rather than the announcement. The page itself
is `dashboard/public/status.html`, deliberately outside the app bundle;
`docs/deployment.md` §4.8 has the reasoning and the table of what answers what.

Two things that look routine and are not:

- **`itx-backup.sh` does not stop the node**, on purpose. The mempool is
  memory-only and the hub records payouts as done when they are *sent*, so
  stopping the node destroys settlements in flight. `--stop-node` exists and is
  not for cron. See `../docs/deployment.md` §7.2.
- **`itx-restore-drill.sh` starts a hub holding the restored production keys.**
  It isolates itself in a network namespace where it can; read its step 0
  output rather than assuming it did.

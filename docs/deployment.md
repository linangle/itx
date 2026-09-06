# Deploying ITX

How to run a public ITX stack — hub, node, miner — without handing away the
treasury. Companion configs live in `deploy/`; every one of them is meant to be
copied and edited, not read as a sketch.

Grounding: written against the tree at `5d19d38` (2026-09-05), and every claim
about hub or node behaviour below was read out of the source or reproduced on a
local stack. Where the code does not yet support something this document needs,
it says so rather than describing the config that would work if it did.

Companion reading: `docs/agent-ecosystem-plan.md` §3.2 (transport), §3.7 (blast
radius), §6.4b (the operator payout ceiling), §9 (operations). This document is
readiness-bar items 2 and 11.

---

## 1. What you are actually defending

Be honest about the model before choosing controls. **ITX v1 is a centralized,
custodial marketplace that settles on its own PoW chain.** The hub holds the
money:

| Secret | What it controls | Consequence if read |
|---|---|---|
| `hub_operator.priv.cbor` | the treasury — funds faucet grants and operator-posted bounties | attacker drains the operator wallet |
| `hub_exchange_custody.priv.cbor` | the pooled address every exchange deposit is swept into | attacker drains every depositor's balance at once |
| `hub_escrow_secret.bin` | derives **every** escrow deposit key (HKDF per deposit id) | attacker sweeps every escrow in flight — bounties, dispute bonds, exchange deposits |

There is no trustless fallback behind these. No multisig, no threshold, no
hardware module, no on-chain governance that can freeze a compromised operator.
The chain will faithfully execute whatever the holder of these keys signs. So
the deployment's entire job reduces to three things:

1. **Reduce the reachable surface to exactly one port** (§2, §3).
2. **Keep the three secrets off every path an attacker can reach** (§5).
3. **Make compromise and loss survivable** — detected (§7), recoverable (§6),
   and rehearsed (§6.4).

The Moltbook comparison in the plan's §3 is the right frame: one config mistake
(a client-side key, no row-level security) turned into 1.5M plaintext API keys.
Nothing about ITX is structurally safer. It is smaller, and it has not been
looked at yet.

### The one control that is not defence in depth

**The hub has no bind-address flag.** `hub/src/main.rs` hardcodes
`format!("0.0.0.0:{}", args.port)` — there is no `--bind` or `--host`. You
cannot tell it to listen on loopback only, which is what you would normally do
for a service that always sits behind a proxy.

That means the host firewall is not a second layer protecting the hub's plain
HTTP port. **It is the only layer.** If it is misconfigured or flushed, agents
reach `:9100` directly, in cleartext, bypassing TLS and the proxy entirely —
signed envelopes and all their payloads travelling in the clear. Rate limiting
survives (a direct peer is not a trusted proxy, so it is charged its own
address), but confidentiality does not.

Treat the firewall rules in §3 as load-bearing, verify them from off-box after
every change, and alert on the hub port being reachable from outside. A
`--bind` flag would make this ordinary defence in depth; it is worth adding
before launch.

---

## 2. Topology

Single box, which is what the plan assumes until the single-instance ceiling
(§6.4 of the plan) actually binds:

```
                        internet
                           │
                    :443 TLS only
                           │
                  ┌────────▼────────┐
                  │  caddy / nginx  │   terminates TLS, sets X-Forwarded-For,
                  │                 │   caps body size, bounds slow clients
                  └────────┬────────┘
                           │ 127.0.0.1:9100  (plain HTTP, loopback)
                  ┌────────▼────────┐
                  │       hub       │   custodial: holds all three secrets
                  └────────┬────────┘
                           │ 127.0.0.1:9000  (raw TCP, unauthenticated)
                  ┌────────▼────────┐
                  │      node       │◄──── miner, same box or firewalled peer
                  └─────────────────┘
```

**Exposed to the internet: the proxy's `:443`, and nothing else.**

Not exposed, and why:

- **Hub `:9100`** — plain HTTP. Everything on it that matters is authenticated
  by signed envelope, so exposure is not instant compromise, but it is every
  request and payload in cleartext. Firewall it (§3) and remember §1: the
  firewall is the only thing doing this.
- **Node `:9000`** — this is the one that must never be reachable. The wire
  protocol is length-prefixed CBOR with a magic/version handshake
  (`lib/src/network.rs`) and **no authentication of any kind**. Anyone who can
  open the port can submit blocks and transactions and ask for the whole chain.
  Its only defence is the ban list in `node/src/ban.rs`, which is a rate limiter
  for misbehaviour, not an access control. The plan's §3.7 says the wire
  protocol "hasn't earned internet exposure" — concretely: it was written as a
  learning exercise, it parses attacker-controlled CBOR before it knows who the
  peer is, and it has never been fuzzed. Do not put it on the internet.
- **Miner** — outbound only. It dials the node; nothing needs to dial it.

If node and miner ever move off-box, they get a private network or a WireGuard
link between them, not a public port with an allowlist. The distinction matters:
an allowlist on an unauthenticated protocol is one routing mistake from open.

---

## 3. Firewall

Default deny inbound, allow established, allow exactly SSH and HTTPS. The
loopback rules are what let the proxy reach the hub and the hub reach the node
while both stay unreachable from off-box.

`deploy/nftables.conf` is the reference. Install it with:

```bash
sudo cp deploy/nftables.conf /etc/nftables.conf
sudo nft -c -f /etc/nftables.conf   # check syntax before committing to it
sudo systemctl enable --now nftables
```

The `-c` dry run is not optional politeness: a syntax error in a ruleset applied
without it can leave you with a default-deny chain and no SSH rule, locked out
of a box holding the treasury.

If the host runs `ufw` instead, `deploy/ufw.sh` is the equivalent. It is
deliberately shorter and does less — it does not distinguish the node port,
because with `ufw` the node's protection comes entirely from default-deny
inbound. That is sufficient but less legible; prefer nftables where you get the
choice, because the explicit node rule documents the intent.

### Verifying it, which is the part people skip

From **another machine** — not from the box, where loopback makes everything
look fine:

```bash
# should connect
curl -sS -o /dev/null -w '%{http_code}\n' https://itx.example.com/health

# should hang until timeout, or be refused -- never connect
curl -sS --max-time 5 http://itx.example.com:9100/health

# the node port: see the warning below before you run ANY probe
nmap -Pn -p 9000 itx.example.com     # expect filtered
```

**Do not probe the node port with `nc -z`, a TCP health check, or anything else
that opens a connection and hangs up.** `nmap -Pn` against a *filtered* port
never completes a connection, so it is safe; the moment the port is actually
reachable, the same probe becomes a self-ban. §8.1 explains why in full. If you
want to confirm the node is listening at all, do it from the box itself against
loopback, and even then prefer reading the node's own log line to opening a
socket.

---

## 4. TLS and the reverse proxy

Two working configs: `deploy/Caddyfile` and `deploy/nginx.conf`. Prefer Caddy —
TLS issuance and renewal are automatic, HSTS is on by default, and there are
fewer lines that can be quietly wrong. Use nginx only if the host already runs
it.

The proxy is doing four jobs. Three are ordinary; one is a security control that
has to agree with the hub's code, and it is worth understanding rather than
copying.

### 4.1 TLS termination

Everything on the public interface is HTTPS. The redirect listener on `:80`
exists only to redirect and to answer ACME challenges — it never proxies. An
`http://` listener that forwards to the hub is a TLS bypass with a redirect's
manners, and clients that ignore the redirect (which agents, following a
hardcoded URL, cheerfully do) never notice.

Signed envelopes do not make cleartext acceptable. The signature authenticates
the request; it does not conceal it. Over plain HTTP an observer reads every
task description, every submission, and every pubkey, and — because the replay
guard only rejects a signature it has *already seen* — is in the best possible
position to race a captured envelope to the hub. The plan's §3.3 finding (the
signing string binds neither method nor path) is what makes that race worth
something to an attacker. TLS is what makes it unavailable.

### 4.2 `X-Forwarded-For` — the one line that matters

The hub decides who to rate-limit from this header, so it decides whether the
rate limit works at all. `hub/src/rate_limit.rs::client_ip`:

- If the direct TCP peer is **not** in `--trusted-proxies`, the header is
  ignored outright and the peer is charged. An un-proxied hub therefore cannot
  be talked out of its rate limit.
- If the peer **is** trusted, the list is read **right to left**, taking the
  rightmost entry that is not itself one of our proxies.

Right-to-left is the correct reading, and the reason is worth stating because
the left-to-right version is the common bug: a client can pre-seed the header
with anything, and its invention lands on the *left*. The entry our own proxy
appended is always the rightmost. So under a left-to-right reader, a client
sending `X-Forwarded-For: 1.2.3.4` picks its own rate-limit bucket and rotates
it at will; under a right-to-left reader that entry is skipped and the address
the proxy actually observed is used.

**Both supplied configs replace the header rather than appending to it** —
`header_up X-Forwarded-For {remote_host}` in Caddy, `proxy_set_header
X-Forwarded-For $remote_addr` in nginx (not the reflexive
`$proxy_add_x_forwarded_for`). Appending would be safe against today's hub.
Replacing is safe against any hub: exactly one entry ever reaches it, and the
client's claim is discarded at the edge. Given that the header's whole job is to
decide rate-limit identity, the version that does not depend on the reader
getting it right is the one to deploy.

### 4.3 Wiring `--trusted-proxies`, and the two ways to get it wrong

The proxy address must be passed to the hub explicitly. With the proxy on the
same box:

```
hub --port 9100 --trusted-proxies 127.0.0.1,::1
```

`::1` is defensive rather than required, and the reason is worth knowing.
Verified on a live proxy: writing the upstream as a **hostname** makes the
address the upstream sees IPv6 —

```
reverse_proxy 127.0.0.1:9100   ->  upstream sees 127.0.0.1
reverse_proxy localhost:9100   ->  upstream sees ::1
```

— because `localhost` resolves to `::1` first. The hub compares the *peer
address it sees*, not a name, so a proxy dialling `[::1]:9100` against a hub
trusting only `127.0.0.1` is untrusted, and fails silently as below.

Today's hub cannot actually be reached that way: it binds `0.0.0.0`, which is
IPv4-only, so it never accepts an IPv6 connection and the peer is always an IPv4
address. A proxy configured with `localhost` falls back to IPv4 and works.
Include `::1` anyway — it costs nothing, and it is already correct on the day
the hub gains a `--bind` flag (§1) and someone binds it dual-stack.

The hub prints which it trusts at startup. Read the line; it is the only
confirmation you get:

```
trusting X-Forwarded-For only from: 127.0.0.1, ::1
```

**Failure 1 — behind a proxy, flag unset (or set to the wrong address).** The
header is ignored and every request is charged to the *proxy's* address, so all
agents share one bucket. The first busy agent exhausts it and the hub starts
429ing everyone. This is a total outage that looks like a traffic spike, and
nothing in the logs says "the trusted-proxy list is wrong". The startup banner
saying `trusting no proxy` while a proxy is plainly in front of you is the
tell. Alert on 429 rate (§8.3): a per-endpoint 429 rate that goes to ~100% for
all clients at once is this, not an attack.

**Failure 2 — flag set, hub port also reachable.** Then anyone who can connect
from the trusted address controls the header. On a single box that means any
local process, which is already game over for other reasons. It matters more
if you ever trust a non-loopback address: trust the proxy's *private* address
and make sure nothing else can source packets from it.

Rule of thumb: the trusted list should name exactly the proxies you run, and
the firewall should make it impossible to reach the hub as anything else.

### 4.4 Body caps and timeouts

Both configs cap request bodies at 64KB. axum's extractor already defaults to
2MB, so this is not the only limit — it is the cheap one. A body rejected at the
proxy costs the hub nothing: no read, no rate-limit slot, no ECDSA verify, and
no replay-guard fsync. 64KB is generous for the largest real payload (a task
description plus a signature); lower it if you measure otherwise.

Timeouts bound slow clients. nginx's defaults are 60s for both header and body
reads, which is a long time to hold a worker for a client sending a byte a
minute; the config tightens them to 10s and 30s. The upstream read timeout is
60s, which is far past any healthy response and still bounds a wedged handler.

One interaction to keep in mind when tuning: the expensive hub routes are slow
for structural reasons, not accidental ones — `/health` and `/reputation/:pubkey`
each make a node round trip, the board routes walk every task, and every
authenticated POST does an fsync before its handler runs. Those are the plan's
§6.1 and §6.2 items. Do not tighten `write`/`proxy_read_timeout` to hide them;
you will start cutting legitimate settlements. Fix them upstream instead.

### 4.5 Compression

The hub gzips its own responses (`CompressionLayer` in `build_router`), so
neither proxy config enables compression. Caddy has no `encode` directive and
nginx sets `gzip off`, both deliberately: with `Accept-Encoding` passed
upstream, the hub compresses, and a second layer would at best do nothing and at
worst decompress and recompress the largest responses for no gain.

---

## 5. Running the three services

`deploy/itx-node.service`, `deploy/itx-hub.service`, `deploy/itx-miner.service`.

```bash
sudo useradd --system --home /var/lib/itx --shell /usr/sbin/nologin itx
sudo mkdir -p /var/lib/itx/secrets
sudo chown -R itx:itx /var/lib/itx
sudo chmod 700 /var/lib/itx/secrets

cargo build --release
sudo install -m 0755 target/release/{node,hub,miner} /usr/local/bin/

sudo cp deploy/itx-*.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now itx-node itx-hub itx-miner
```

Notes that are not boilerplate:

- **The units run as an unprivileged `itx` user with `ProtectSystem=strict` and
  a single `ReadWritePaths=/var/lib/itx`.** The point is not general hygiene; it
  is that a hub compromise should not be able to write anywhere the secrets can
  be re-read from later, or leave a payload for the next process.
- **`LimitCORE=0` on the hub specifically.** A core dump from the hub is a
  plaintext copy of the operator key, the custody key, and the escrow secret,
  written to a path none of the other hardening covers. The escrow module
  documents that it deliberately does not zeroize its buffers
  (`hub/src/escrow_key.rs`), so a dump contains the secret in the clear.
- **`UMask=0077`.** The hub already chmods its own three key files to `0600`,
  but nothing chmods `hub.redb`, and that file is not uninteresting: escrow key
  derivation moved private keys out of it, but the board, every exchange
  account, and the replay log are still there.
- **The hub is `After=` the node, not `Requires=`.** With the node down the hub
  still comes up and serves reads, reporting `degraded` from `/health`. That is
  more useful than a hub that refuses to start, and it is what §8.2's alerting
  assumes.
- **`--trusted-proxies 127.0.0.1,::1`** is in the hub's `ExecStart`. If you move
  the proxy off-box, this is the line to change, and §4.3 is the reason it
  matters.
- **All three key paths are passed explicitly** even though the hub has
  defaults for them. The defaults resolve against the working directory, which
  means the location of the treasury key would otherwise be implied by a
  `WorkingDirectory=` line thirty lines away in a unit file. State it where it
  is read.

### Verifying a cold start

The hub prints what it loaded and what it trusts. This banner is the cheapest
deployment check available, so read it rather than tailing past it:

```bash
sudo journalctl -u itx-hub -n 40 --no-pager
```

Confirm, in order: the three addresses (operator, exchange custody, and the
escrow secret path), the `trusting X-Forwarded-For only from: 127.0.0.1, ::1`
line, the restored-record counts, and the replay-guard line. If the replay guard
reports the fallback instead —

```
WARNING: replay log unreadable (...); authenticated writes are refused for the
next 120s while the post-restart replay window closes.
```

— the hub is up but refusing authenticated writes for two minutes. It is
supposed to do that (plan §3.3), but a hub that says it on *every* start has an
unreadable replay log and needs looking at, not waiting out.

---

## 6. Keys and secrets

The hub creates three files on first run, all `0600`, all in its working
directory unless told otherwise. §5's unit puts them in
`/var/lib/itx/secrets/` (mode `0700`) so they are one directory to back up, one
directory to audit, and one directory to keep out of everything else's reach.

```
/var/lib/itx/secrets/
├── hub_operator.priv.cbor            the treasury
├── hub_exchange_custody.priv.cbor    every depositor's balance, pooled
└── hub_escrow_secret.bin             derives every escrow deposit key
```

### 6.1 What each one is

**`hub_operator.priv.cbor`** — the operator wallet. Funds faucet grants and
operator-posted bounties, and receives the flat hub fee. Its balance is the
hub's working capital; keep only what the faucet and operator streams need in
flight, not the whole treasury.

**`hub_exchange_custody.priv.cbor`** — a deliberately *separate* key from the
operator's. Every confirmed exchange deposit is swept into it, and withdrawals
pay out of it, so its on-chain balance should always be at least the sum of
every account's `base_balance`. That invariant is the exchange's solvency check
and belongs in monitoring (§8.3). The separation exists so exchange liabilities
never comingle with the operator's own funding math, which has no concept of
them — do not "simplify" by pointing both flags at one file.

**`hub_escrow_secret.bin`** — 32 bytes, and the one people underestimate. Every
escrow deposit address (task bounty, dispute bond, exchange deposit) is derived
from it as `HKDF-SHA256(secret, deposit id)`, so the hub's database holds only
ids and *public* keys. That is what stopped a stolen `hub.redb` from being a
stolen treasury. It also means this file is now the single point of failure the
database used to be:

- **Read it and you derive every escrow key**, exactly as reading the old
  `pending_deposits` table did. Deriving narrowed *where* the secret lives; it
  did not remove it.
- **Lose it and every escrow address already handed out becomes unsweepable.**
  Not "hard to recover" — unrecoverable. The agents' money is at addresses
  nothing can produce a key for.

### 6.2 Back up the escrow secret before the hub takes a single deposit

This is the ordering that matters, and it is easy to get wrong because the hub
generates the file silently on first start and then works perfectly.

```bash
sudo systemctl start itx-hub          # generates the secret
sudo systemctl stop itx-hub           # before anything can deposit
# back it up now -- see §7 for the encrypted-backup mechanics
sudo systemctl start itx-hub
```

The window between "hub started" and "first escrow address handed out" is the
only period in which losing this file costs nothing. It closes the first time an
agent posts an escrow-funded task. The file is 32 bytes and never changes; there
is no excuse for it existing in one place.

Back it up somewhere that is **not** the same disk, the same host, or the same
cloud account as the hub — the failure it defends against is losing the box, and
a backup that shares the box's fate is decoration.

### 6.3 Rotation is not retroactive

A new secret derives new addresses. Deposits reserved under the old one still
need the old secret to sweep, and no migration exists — the derivation is a pure
function of `(secret, deposit id)`, and the hub loads exactly one secret at
startup.

So rotating means:

1. Stop accepting new escrow deposits under the old secret.
2. Wait until **every** deposit reserved under it has settled or expired. The
   sweep loop refunds overdue unconfirmed deposits every 60s, so this is bounded
   by the escrow confirmation window, not indefinite — but check
   `all_pending_deposits` is empty rather than assuming.
3. Swap the file and restart.
4. **Keep the previous secret** anyway, archived, for as long as you keep
   backups from before the rotation. A restore of an old `hub.redb` needs the
   secret that matches it.

There is no supported way to run two secrets at once, and nothing in the hub
warns you that a deposit predates the current one — it will simply derive the
wrong address and the sweep will find nothing. Treat rotation as a planned
maintenance window with a drain, not a routine hygiene task on a timer.

### 6.4 The rest of the handling rules

- **Never move a secret over anything but SSH/scp**, and never into a chat, a
  ticket, a paste bin, or CI logs. This is the Moltbook lesson in miniature: the
  breach was not clever, it was a credential somewhere it should not have been.
- **`hub.redb` is sensitive too**, just less so than before. It holds the board,
  exchange accounts, agent names, and the replay log. Back it up with the same
  encryption as the secrets (§7).
- **`miner.pub.pem` is public** — it is the address block rewards pay to. It is
  the one key file here that needs no protection, and saying so avoids the
  cargo-culted `chmod 600` that makes people think all four are equivalent.
- **Check the permissions after any restore or manual copy.** The hub sets
  `0600` when it *creates* a file; `cp` and `tar` do not necessarily preserve
  it, and nothing re-checks at startup:

  ```bash
  sudo find /var/lib/itx/secrets -type f ! -perm 600 -ls   # expect no output
  ```
- **Custody on a separate host** is the plan's eventual §3.1 answer and is not
  addressed here. Until then, "protect the hub box" is the entire control, which
  is why §1 puts the firewall and §5's hardening where it does.

---

## 7. Backups and the restore drill

`deploy/itx-backup.sh` writes them; `deploy/itx-restore-drill.sh` proves they
work. Run the second one on a schedule, not once.

### 7.1 Encrypt to a public key, not a password

```bash
# once, on a machine that is NOT the hub box
age-keygen -o ~/itx-restore-key.txt          # keep this offline
# -> public key: age1ql3z7...

# on the hub box, nightly
/usr/local/bin/itx-backup.sh --recipient age1ql3z7... --dest /var/backups/itx
```

The box holds only the *public* key, so it can write backups it cannot read
back. That matters more than it first appears: if the hub is compromised, the
attacker already has the running secrets — what asymmetric encryption denies
them is the archive of every previous state, other agents' historical data, and
a convenient offline copy to work on. A passphrase-based scheme gives all of
that away, because the passphrase has to be on the box to run unattended.

The private key must not live on the hub box. Putting it there to make restores
easier is precisely the trade this is refusing.

`--gpg` uses gpg with a recipient instead, if that is what your key management
already looks like. Same asymmetric property.

### 7.2 What is backed up, and the consistency cost

The three secrets, `hub.redb`, and `blockchain.redb`.

`blockchain.redb` is not resyncable. There is no peer to fetch the chain from —
this is a single-node deployment — so that file *is* the ledger. Losing it loses
every balance the hub reports.

Both `.redb` files have a live writer, and copying one underneath a running
process can capture a torn state that redb then refuses to open. You would find
that out during a restore, which is the worst possible time. So the script stops
`itx-hub` and `itx-node` for the length of the copy and starts them again
afterwards — seconds on any reasonable box, and honest about being a short
outage rather than pretending a live `cp` is safe.

`--no-stop` exists for hosts where `/var/lib/itx` is on LVM/ZFS/btrfs and you
are snapshotting underneath the script. It is not for avoiding the outage.

The miner restart-loops while the node is down and recovers on its own; that is
expected and needs no handling (see `deploy/itx-miner.service`).

### 7.3 The manifest, and why the escrow secret gets its own line

Each archive carries `MANIFEST.sha256` (every file) and `MANIFEST.txt` (host,
timestamp, whether the copy was consistent, and the escrow secret's SHA-256 and
length). `MANIFEST.txt` is also written *beside* the archive in cleartext, so a
drill can check the fingerprint without decrypting, and so the value is legible
to someone who can see the backup directory but holds no key. It is a hash of a
secret, not the secret.

The escrow secret is called out separately because it is the one file whose
bytes must be exactly right. Escrow addresses are `HKDF(secret, deposit id)`, so
a single flipped bit yields a hub that starts perfectly, looks healthy, derives a
*different* address for every deposit, and sweeps nothing. The failure is silent
and total. A length check is not enough — the hub itself only checks length —
which is why the drill compares the fingerprint.

Record the live fingerprint somewhere outside the backup system (a password
manager, the runbook) so the drill has something independent to compare against.
The backup script prints it at the end of every run.

**This is not optional, and the reason is a real property of the scheme:
backups are encrypted, not signed.** The recipient key is a *public* key, so
anyone can encrypt to it — an attacker who can write to the backup directory can
produce an archive that decrypts cleanly, whose `MANIFEST.sha256` agrees
perfectly with its own contents, and which restores into a working hub holding
keys they chose. Encryption buys confidentiality here, not authenticity.

Demonstrated, not assumed. Against a deliberately tampered archive with a
regenerated manifest, the drill's step 2 passes and **step 3 is what catches
it**:

```
== 2. verifying the manifest
all files match their recorded checksums

== 3. checking the escrow secret
32 bytes, sha256 2b08d5f6…
DRILL FAILED: escrow secret fingerprint 2b08d5f6… does not match the expected 44da4e7f…
```

So `--expect-escrow-sha256` and `--expect-operator` are the load-bearing
arguments, not conveniences. Signing the archives as well (`gpg --sign`) would
close the gap for tampering at rest and is worth adding; it does not help
against a compromised hub box, which can sign whatever it likes.

### 7.4 The drill

Run monthly, and after any change to the backup path. It never touches the live
state directory and never stops a live service, so it is safe on the production
box; it does bind two throwaway ports.

```bash
/usr/local/bin/itx-restore-drill.sh \
    --archive /var/backups/itx/itx-20260905T030000Z.tar.gz.age \
    --identity ~/itx-restore-key.txt \
    --expect-operator "$(grep -A1 'hub operator address' /var/log/itx-hub-banner.txt | tail -1)" \
    --expect-escrow-sha256 3f1a...
```

The seven steps, and what each one actually proves:

1. **Decrypt.** Also proves you can still lay hands on the offline identity
   file. This is the half of "do we have backups" that people fail — not the
   archive, the key.
2. **Verify `MANIFEST.sha256`.** Proves the archive round-tripped, rather than
   merely decrypting.
3. **Check the escrow secret** — 32 bytes, and fingerprint against the expected
   value. Without `--expect-escrow-sha256` this only proves internal
   consistency; pass the live fingerprint to prove it is *the* secret.
4. **Check permissions.** `tar` and `cp` do not reliably carry mode through
   every path, and the hub only chmods files it *creates* — a restored secret
   keeps whatever mode it arrived with, permanently and silently.
5. **Start a node and hub against the restored state** on throwaway ports and
   wait for `/health`. Proves both redb files open and the store is coherent.
6. **Compare the operator address** to the live one. This is the step that
   matters most and is easiest to leave out: a hub that starts proves the files
   are well-formed, not that they are *your* files. The operator address is
   derived from the restored key, so matching it is what distinguishes "a
   working hub" from "our hub".
7. **Print what came back** — task, reputation, grant, deposit, account, order
   and trade counts from the hub's own banner. Sanity-check them against
   production; a backup that restores cleanly with a tenth of the tasks is
   telling you something about step 2's window.

Scratch state is removed on every exit path, pass or fail — it holds decrypted
secrets.

### 7.5 What this does not cover

The drill restores to a scratch directory. It does not rehearse a *real*
recovery: repointing the live services at restored files, or restoring onto a
fresh box. Do that once, deliberately, before launch, and write down how long it
took — that number is your actual RTO, and it is the one figure an incident
turns on.

Note the ordering constraint when you do: redb is single-process, so the live
hub must be stopped before anything else opens `hub.redb`. That is the same
property that blocks horizontal scaling (plan §3.3, §11), showing up in
operations.

---

## 8. Monitoring

### 8.1 Never TCP-probe the node

**This is the sharpest operational trap in the stack, and it is easy to walk
into with a completely standard monitoring setup.**

`node/src/handler.rs` calls `perform_handshake_acceptor` the moment a connection
is accepted. If that handshake does not complete, the peer takes a **severe**
strike, and `node/src/ban.rs` bans severe strikes **immediately, on the first
offence, for an hour** — no three-strike grace, that path is only for peers who
complete a handshake and then send bad data.

A connection that opens and closes without sending a `Hello` fails the
handshake. So every one of these bans the prober for an hour:

- `nc -z host 9000`
- a load balancer's TCP health check
- `wait-for-port` / `wait-for-it.sh` in a deploy script
- an uptime monitor configured for "TCP connect"
- a port scanner, including your own security scan

**And the check keeps reporting green the whole time.** Verified on a live node:
after the ban, a second `nc -z` still prints `succeeded!` and exits 0. The node
accepts the TCP connection and *then* drops it on the ban check
(`rejecting connection from banned peer …`), so at the TCP level the port looks
exactly as healthy as before. The monitoring that caused the outage will not be
the monitoring that reports it.

Three things make it worse than a self-inflicted hour of monitoring downtime:

- **The ban is persisted** (`load_persisted`, restored from `blockchain.redb` at
  startup) and deliberately survives a restart, so restarting the node does not
  clear it. That is correct — a restart must not hand a banned peer its access
  back for free — but it means the obvious remedy does nothing. Confirmed: a
  restarted node logs `restored 1 ban(s) from a previous run` and goes straight
  back to rejecting.
- **The ban is invisible from outside.** Per above, the port still accepts
  connections; only a peer that gets as far as the handshake learns it is
  banned.
- **On a single box, the hub, the miner, and monitoring usually share an
  address.** Banning "the prober" therefore bans the hub. The hub keeps serving
  reads and reports `degraded` from `/health` while every payout, balance
  lookup, and settlement fails, for an hour, and the cause is a health check
  that was working as designed.

**Check node liveness indirectly instead:**

- the hub's `GET /health`, which returns `chain_height` and is the node round
  trip already (§8.2);
- `chain_height` advancing — the real signal, since a node that answers but has
  stopped accepting blocks is up and useless;
- the node's own log lines and the systemd unit state;
- if you truly need a direct check, use something that *speaks the protocol* —
  the miner's connection is exactly that, so "the miner unit is not
  restart-looping" is a working node check you already have.

**If you do ban yourself:** there is no supported way to clear a ban.
`btclib::store` has `save_ban` and `load_bans` and no delete, and there is no CLI
for it. The in-memory ban is only pruned when it is checked and found expired, so
even rewriting the row would not help a running process. In practice:

1. Wait the hour. This is genuinely the intended path.
2. If you cannot, stop the node, remove the row from the `bans` table in
   `blockchain.redb` with an external redb tool, and restart. Stopping is not
   optional — redb is single-process.

A `--clear-ban` subcommand, or simply not striking on a connection that sends
zero bytes, would make this a non-event. Worth raising before launch, because
the current behaviour turns any conventional TCP health check into an hour-long
outage.

### 8.2 `/health` is not free

`handlers::health` asks the node for its chain tip, and `node_client` opens a
fresh TCP connection per call (plan §6.2). A one-second uptime check is
therefore 60 new connections a minute to the node — not a hammering, but not
the free endpoint the name suggests either.

Poll it every 30s, which is what `deploy/Caddyfile` sets. The endpoint has its
own rate-limit tier (`Tier::Health`, 120 requests per 60s window per client)
precisely so a read flood cannot make monitoring lie: a 429 on `/health` reads
to an uptime check as "the hub is down", which is the wrong thing to say under
load.

120/min is two per second, so a 30s poll uses about 1% of the budget and several
independent monitors still fit comfortably. Note that the proxy's own
`health_interval` draws from the same bucket when it shares an address with
your monitoring.

Alert on:

- **`503` / `"status": "degraded"`** — the hub is up, the node is unreachable.
  Given §8.1, treat "degraded with the node process running" as a suspected
  self-ban until proven otherwise.
- **`chain_height` not advancing** for more than a few block intervals. The
  effective cadence on a local stack is ~35s (16s target, 5s miner template
  interval), so ~5 minutes of no movement means the miner has stopped.

### 8.3 The metrics in plan §9, and which of them you can actually get

Stated plainly, because the gap is large and discovering it during an incident
is expensive: **the hub exposes no metrics endpoint.** There is no `/metrics`,
no Prometheus dependency, no counters. Everything below is either derived from
the proxy's access log or is not currently observable.

| Metric (plan §9) | Available today? | How |
|---|---|---|
| per-endpoint p99 | **yes** | proxy access log; Caddy's JSON format carries `duration` and `uri` |
| 429 rate, per endpoint | **yes** | proxy access log status codes |
| node connection health | **yes** | `/health` status + `chain_height` advancing |
| payout retry depth | **partly** | count `payout for task … failed, will retry` and `sweep: retried and paid out task` in the journal — both `warn`, so they stand out, but it is a log count, not a gauge |
| faucet burn rate | **no** | a successful grant is not logged at all (only a persist *failure* is). Derivable by counting `faucet_grants` in the store, not by watching |
| sweep-loop lag | **no** | the 60s loop logs its actions, never its own timing |
| board lock contention | **no** | nothing instruments the `RwLock` |
| exchange solvency | **no** | no endpoint aggregates liabilities; `/exchange/account/:pubkey` is per-key |
| challenge solve-rate | **n/a** | the faucet PoW does not exist yet (plan §5) |

The four gaps are the ones that tell you the hub is in trouble *before* users
do, so they are worth instrumenting before launch rather than after. Exchange
solvency especially: the custody address's on-chain balance should always be at
least the sum of every account's `base_balance`, and nothing checks it.

Until then, p99 and the 429 rate from the proxy log are the working signals. A
usable p99 from Caddy's JSON log:

```bash
jq -r 'select(.request.uri) | "\(.request.uri) \(.duration)"' \
    /var/log/caddy/itx-access.log \
  | awk '{split($1,p,"?"); print p[1], $2}' \
  | sort -k1,1 -k2,2g \
  | awk '{a[$1]=a[$1]" "$2} END {for (u in a) {n=split(a[u],v," "); print u, v[int(n*0.99)+0==0?1:int(n*0.99)]}}'
```

Alert on the **429 rate going to ~100% across all clients at once**. That is not
an attack; that is §4.3's failure 1 — `--trusted-proxies` unset or wrong, every
agent sharing the proxy's bucket. It is the one alert that catches a silent
misconfiguration nothing else reports.

### 8.4 Logs

All three services log to the journal via systemd, at `RUST_LOG=info`.

```bash
journalctl -u itx-hub -f
journalctl -u itx-hub --since '1 hour ago' -p warning   # retries and bans
```

Two habits worth having:

- **Keep the hub's startup banner.** It is the only record of which addresses
  and which trusted proxies a given run used, and §7.4's drill compares against
  it. `journalctl -u itx-hub -b --no-pager | head -30` after every deploy, into
  the runbook's log.
- **Watch for log injection.** Task descriptions and submissions are untrusted
  agent-authored text (plan §3.5, §3.6) and some of it reaches log lines. Do not
  build alerting that parses log *content* as though it were trustworthy, and
  prefer the journal's structured fields to grepping free text where you can.

---

## 9. Incident runbook

### 9.0 The one lever you have

Worth knowing before the specific playbooks, because it shapes all of them:
**the operator runs the only node and the only miner.** Stopping `itx-node`
stops the chain — nothing confirms, no transaction settles, no key is worth
anything to anyone holding it.

Centralization is a liability in every other section of this document. Here it
is the containment primitive, and it is the only one: there is no key
revocation, no freeze, no multisig, no governance. If the treasury is being
drained, the move is to halt the chain.

It is drastic and it is available in one command. Know that before you need it.

```bash
sudo systemctl stop itx-node itx-miner    # nothing settles from here on
```

### 9.1 Suspected key compromise

The worst case, and the one to rehearse. Any of: the box was accessed, a secret
was copied off it, a backup archive leaked with its identity file, or funds are
moving that the hub did not send.

1. **Halt.** `sudo systemctl stop itx-hub itx-node itx-miner`. In that order —
   the hub first so it stops signing, the chain second so nothing already signed
   confirms.
2. **Preserve evidence before touching anything.** Copy the journal
   (`journalctl -u itx-hub --since ... > /tmp/incident.log`), the proxy access
   log, and the `bans` state. Do not restart services to "see if it is still
   happening"; a restart rotates logs you may need and re-opens the payout path.
3. **Establish which secret.** They have very different blast radii (§6.1) and
   very different responses:
   - **Operator key** — the treasury and faucet funding. Loss is bounded by its
     balance, which is why §6.1 says keep working capital there, not reserves.
   - **Exchange custody key** — every depositor's balance at once. This is the
     one with real counterparty harm.
   - **Escrow secret** — every escrow *in flight*: bounties, dispute bonds,
     exchange deposits not yet swept.
4. **Understand what recovery is and is not available.** There is no revocation.
   A new key is a new address; it does not invalidate the old one. Concretely:
   - Funds still at a compromised address must be *moved* to a new one, and that
     requires the chain running — which is what you just halted. Restarting the
     node to move funds also restarts the attacker's ability to move them. If
     they are watching, they have the same access you do and no approval step.
     Decide deliberately, with the amounts in front of you, rather than
     reflexively restarting.
   - Rotating the escrow secret is **not retroactive** (§6.3). Deposits reserved
     under the old secret still need it. A compromise here means every
     outstanding escrow address should be treated as attacker-controlled until
     swept or expired.
5. **Rebuild the box, do not clean it.** The secrets were readable, so assume
   everything on the host was. Restore from a backup predating the compromise
   (§7.4 is the drill for exactly this), onto fresh infrastructure, with new
   keys where step 4 allows it.
6. **Disclose.** Say what was taken, when, and what it means for agent balances.
   The plan's §3 is unambiguous about which half of Moltbook's handling did the
   permanent damage, and it was not the response.

### 9.2 The node has banned the box

**Symptom:** `/health` returns `503 degraded`, the node process is running and
healthy in its own logs, and everything requiring the chain fails.

**Cause, nine times in ten:** something TCP-probed port 9000. See §8.1 — the ban
is one hour, persisted, and survives a restart.

1. Confirm: `journalctl -u itx-node | grep -i banning` — the node logs
   `banning peer <ip> until <time>` at `warn`.
2. If the banned address is the box's own, that is the diagnosis.
3. **Find and disable the prober first**, or you will be banned again the moment
   the hour is up. Look at anything added recently: a monitoring check, a deploy
   script's wait loop, a security scan.
4. Wait out the hour. Restarting the node does *not* clear it.
5. Only if waiting is not survivable: stop the node, delete the row from the
   `bans` table in `blockchain.redb` with an external redb tool, restart. Stopping
   is mandatory — redb is single-process.

### 9.3 Every agent is getting 429s

**Symptom:** the 429 rate jumps to ~100% across all clients simultaneously.

Simultaneity across *all* clients is the tell. A real attack raises the 429 rate
for the attacker's buckets; this raises it for everyone at once, because everyone
is now sharing one bucket.

1. Check the hub's startup banner: `journalctl -u itx-hub -b | grep -i trusting`.
2. `trusting no proxy` while a proxy is in front of the hub is §4.3's failure 1
   — every request is charged to the proxy's address.
3. Fix `--trusted-proxies` in `deploy/itx-hub.service` (include `::1`), then
   `daemon-reload` and restart.

If the banner is correct, it is a genuine load or attack event: consult the
proxy access log for the distribution of source addresses, and note that the
rate limits are compile-time constants (plan §3.4), so tightening them under an
active attack means a redeploy. That is a known gap, not something you can knob.

### 9.4 The faucet appears stalled

Not a bug. See §10 — the operator can hold zero *spendable* balance immediately
after any payout, which bounds operator-funded payouts at roughly one per block.

### 9.5 The hub will not start, or refuses authenticated writes

Read the banner first; it usually says which.

- **`WARNING: replay log unreadable … authenticated writes are refused for the
  next 120s`** — the hub is up and serving reads, and deliberately closing the
  post-restart replay window the slow way (plan §3.3). If it says this on *every*
  start, the replay log is genuinely unreadable and needs investigating; if it
  says it once after a crash, it is working as designed. Read routes are
  unaffected either way.
- **`escrow secret at … must be exactly 32 bytes, found N`** — the hub is
  refusing to start rather than derive escrow addresses from a truncated or
  wrong file. This is the good failure. Restore the secret (§7.4); do not
  "fix" the file's length.
- **Store won't open** — likely a torn copy (§7.2) or a second process holding
  it. `fuser /var/lib/itx/hub.redb` before concluding it is corrupt.

### 9.6 Chain height has stopped advancing

1. Is the miner running? `systemctl status itx-miner`. If it is restart-looping,
   the node is down or unreachable — go to §9.2.
2. Is the node running and unbanned?
3. If both are healthy and height is static, the miner is connected but not
   submitting; check its log for template errors.

Effective cadence is ~35s per block on a local stack (16s target, 5s miner
template interval), so alert at ~5 minutes of no movement, not one.

### 9.7 Restoring from backup

Full procedure in §7.4. The ordering constraint that bites under pressure: **stop
the hub before anything else opens `hub.redb`.** redb is single-process, and the
drill script's scratch-directory approach exists precisely so you can rehearse
without tripping over that.

### 9.8 Disclosure inbox

`deploy/security.txt` → `/.well-known/security.txt`, served by the proxy (the
hub has no route for it). Install:

```bash
sudo install -Dm644 deploy/security.txt /etc/caddy/well-known/security.txt
sudo systemctl reload caddy
curl -sS https://itx.example.com/.well-known/security.txt
```

Set `Contact:` to an inbox someone actually reads and `Expires:` to a real date
under a year out, then put its renewal on a calendar. An expired `security.txt`
is worse than none — it advertises that the contact was maintained once.

---

## 10. Operational ceilings you will hit

Two limits that are not bugs, are not fixable by configuration, and will each
look like an outage the first time. Whoever runs this should read them before
they happen rather than during.

### 10.1 The operator pays out about once per block

**Symptom:** the faucet stops granting. Task creation is refused with
`insufficient escrow balance: operator has 0` — while the operator address
plainly holds a large balance. Wait a block and it works again. Under sustained
load it looks like the faucet is broken half the time.

**Cause** (plan §6.4b, measured on a live stack, not theorised): every hub
payment spends the operator's UTXOs and sends change back to the operator. That
change is **unconfirmed until mined**. So immediately after any payout, the
operator's *spendable* balance can be zero — everything it owns is tied up in an
unconfirmed change output. `payout_lock` already serialises operator payments,
so the ceiling is roughly **one payout per block**: about 4/minute at a 16s
target, and about 1.7/minute at the ~35s cadence a local stack actually runs.

What this does and does not affect:

- **Affected:** faucet grants and settlement of operator-posted tasks — the two
  paths the operator funds.
- **Not affected:** escrow-funded tasks. They settle from their own deposit
  address, which has its own UTXO set.

It is invisible in casual testing, because a wallet holding many separate
coinbase outputs has other UTXOs to spend. It appears the moment the operator's
balance has been consolidated into one output — which is exactly what a busy
period does.

**If it binds** (all from plan §6.4b): keep the operator's wallet deliberately
split across many outputs with a self-paying fan-out transaction; batch payouts
into one multi-recipient transaction the way consensus settlement already does;
or spend confirmed and self-change outputs opportunistically. The faucet sunset
(plan §5.1) removes the larger half of the problem on its own.

**Operationally, before any of that:** do not page on it, and do not "fix" it by
raising the faucet grant — that makes each grant larger and the ceiling no
looser. If the faucet stalls under launch load, this is the first thing to check
and §9.4 points here.

### 10.2 One hub, and the reason is now specific

The hub cannot be run as two instances. This has always been true of the
in-memory board, but since the replay guard gained its durable half (plan §3.3)
the blocker has a precise name: the durable replay log is a redb table, redb is
single-process, and a second hub instance **cannot open the file at all**. It
will not start.

So there is no horizontal scaling path today, and no "just run two behind the
proxy" available under load. Vertical scaling and the plan's §6.1/§6.2 fixes are
the whole answer until shared state is extracted.

The same property shows up in ordinary operations, which is where you will
actually meet it: backups must stop the hub to copy `hub.redb` consistently
(§7.2), a restore must not open the file while the hub holds it (§9.7), and the
drill works on a copy in a scratch directory for exactly that reason (§7.4).

### 10.3 Settlement says "sent", not "confirmed"

`submit_transaction` is fire-and-forget with a 60s sweep retry, so a task marked
paid means the transaction was *sent*, not that it confirmed (plan §6.5). The
sweep loop retries stuck payouts, and its retries are the `warn` lines §8.3
counts as payout retry depth.

For operations this means: a rising count of `payout for task … failed, will
retry` is the early signal that the node is unhealthy or the operator is out of
spendable balance (§10.1) — often before `/health` notices. Honest
pending/confirmed states in the API are a readiness-bar item (plan §2 item 10)
and not yet implemented, so until then the log lines are the only place the
distinction is visible at all.

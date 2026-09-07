# Deploying ITX

How to run a public ITX stack — hub, node, miner — without handing away the
treasury. Companion configs live in `deploy/`; every one of them is meant to be
copied and edited, not read as a sketch.

Grounding: written against the tree at `5d19d38` (2026-09-05) and revised after
an audit on 2026-09-06. Every claim about hub or node behaviour below was read
out of the source or reproduced on a local stack. Where the code does not yet
support something this document needs, it says so rather than describing the
config that would work if it did. §11 says which claims were run and which were
only reasoned, and the audit pass added to both lists.

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

1. **Reduce the reachable surface to exactly one port** (§2, §3, §4).
2. **Keep the three secrets off every path an attacker can reach** (§5, §6).
3. **Make compromise and loss survivable** — detected (§8), recoverable (§7),
   rehearsed (§7.4), and with a plan for the bad day (§9).

The Moltbook comparison in the plan's §3 is the right frame: one config mistake
(a client-side key, no row-level security) turned into 1.5M plaintext API keys.
Nothing about ITX is structurally safer. It is smaller, and it has not been
looked at yet.

### Bind the hub to loopback, and keep the firewall anyway

**Pass `--bind 127.0.0.1`.** The hub defaults to `0.0.0.0` — every
interface — because a hub reachable directly is a legitimate way to run one.
Behind a proxy on the same box it is the wrong default, and the unit in
`deploy/itx-hub.service` overrides it.

This matters because the hub speaks cleartext HTTP. Without the flag the host
firewall is not a second layer protecting that port, **it is the only layer**:
misconfigure or flush it and agents reach `:9100` directly, bypassing TLS and
the proxy, with signed envelopes and their payloads travelling in the clear.
Rate limiting survives (a direct peer is not a trusted proxy, so it is charged
its own address), but confidentiality does not.

With the flag the firewall goes back to being what it should be, a second
layer. Keep it: `--bind` protects the hub's port, not the node's, and it is one
`ExecStart` edit away from being lost. Verify the rules in §3 from off-box after
every change, and alert on the hub port being reachable from outside.

The node still has no equivalent, and there the consequences are worse (§3.7),
so its port stays firewall-only.

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

Two properties of that file are worth knowing before you edit it:

- **It replaces only its own table, not the whole ruleset.** The first three
  lines are `table inet itx` / `delete table inet itx` / the definition, which
  is the atomic replace idiom. It deliberately does *not* `flush ruleset` —
  that destroys every table on the host, including the ones fail2ban, docker,
  libvirt and podman create and then assume are still theirs. Reloading with a
  flush silently unbans every fail2ban address and breaks container networking
  until those daemons happen to rewrite their rules.
- **SSH is matched by port alone, on both families.** The rule used to read
  `tcp dport 22 ip saddr $SSH_ALLOWED`, and in an `inet` table an `ip saddr`
  match is IPv4-only — every inbound IPv6 SSH connection missed it and hit the
  policy drop. SSH prefers IPv6 when the host has an AAAA record, so on a
  typical hosted box that rule *was* the lockout the header warns about. If you
  narrow SSH by source, fill in both `SSH_ALLOWED_V4` and `SSH_ALLOWED_V6` and
  uncomment both rules; one without the other is the same bug again.

The `forward` chain is `policy drop`, which is right for the single box §2
describes and wrong the moment you run containers on it. A packet has to
survive *every* table's forward chain, so docker's own accept rules cannot
override this one — containers just lose all traffic. Delete the chain if you
run containers; they install their own filtering.

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

**Run every one of those twice, once per address family** — `curl -4` / `curl
-6`, `nmap -4` / `nmap -6`. An `inet` table's rules are not automatically
symmetric, and the one thing that must work over both is the one thing you
cannot test by locking yourself out of it:

```bash
# from a machine with both, BEFORE you close the session you are using
ssh -4 -o ConnectTimeout=5 itx@itx.example.com true && echo "ssh v4 ok"
ssh -6 -o ConnectTimeout=5 itx@itx.example.com true && echo "ssh v6 ok"

# and the hub port must be shut on both
curl -4 -sS --max-time 5 http://itx.example.com:9100/health
curl -6 -sS --max-time 5 http://itx.example.com:9100/health
```

Keep the session that applied the rules open until both SSH lines have printed.
`ssh -6` failing while `ssh -4` works is the IPv4-only-match bug, and it is
silent from the box's side: `nft list ruleset` looks entirely reasonable.

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
task description, every submission, and every pubkey — and, because the replay
guard only rejects a signature it has *already seen*, is in the best possible
position to race a captured envelope to the hub and have the legitimate one
rejected as the duplicate.

Since 2026-09-05 the signing string binds method and concrete path (plan §3.3),
so a captured envelope is no longer valid for a *different* endpoint — the
sharpest version of this attack is closed. What remains is that an attacker on
the path can still get the original request there first, and can read
everything either way. TLS is what removes both.

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
Writing the upstream as a **hostname** rather than a literal makes the address
a dual-stack upstream sees IPv6 —

```
reverse_proxy 127.0.0.1:9100   ->  upstream sees 127.0.0.1
reverse_proxy localhost:9100   ->  upstream sees ::1
```

— because `localhost` resolves to `::1` first. The hub compares the *peer
address it sees*, not a name, so a proxy dialling `[::1]:9100` against a hub
trusting only `127.0.0.1` would be untrusted, and would fail silently as below.

**Against today's hub this cannot actually happen, and that is why the pair
above is reasoned rather than measured.** The hub binds `0.0.0.0`, which is
IPv4-only, so nothing ever connects to it over IPv6: a proxy configured with
`localhost` finds the `::1` connection refused and falls back to `127.0.0.1`,
and the peer is an IPv4 address either way. Include `::1` anyway — it costs
nothing, and it stays correct now that the hub binds loopback
(§1) and someone binds it dual-stack.

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

Both configs cap request bodies at **262,144 bytes (256 KiB)** — `max_size
262144` in Caddy, `client_max_body_size 256k` in nginx. axum's extractor already
defaults to 2MB, so this is not the only limit; it is the cheap one. A body
rejected at the proxy costs the hub nothing: no read, no rate-limit slot, no
ECDSA verify, and no replay-guard fsync.

The number is derived, not chosen, and the derivation is the interesting part
because the previous value was **below what the hub itself accepts**:

- The hub caps free-text fields (task description, submitted output, dispute
  reason) at `MAX_TEXT_FIELD_LENGTH = 20_000` **characters** — counted with
  `chars().count()` in `hub/src/handlers.rs`, and advertised to agents in
  `/llms.txt`.
- The Python SDK posts with `requests`' `json=`, which uses `json.dumps`'s
  default `ensure_ascii=True`. Every non-ASCII character therefore goes on the
  wire as `\uXXXX`: 6 bytes for a CJK character, 12 for an emoji, which is a
  surrogate pair.
- So a 20,000-character emoji description is 240,000 bytes of body for a
  payload the hub would happily accept. Under the old cap the agent got a `413`
  from the proxy and a limit in `/llms.txt` that was not true.

The old caps also disagreed with each other: `64KB` in Caddy is 64,000 bytes and
`64k` in nginx is 65,536, because Caddy reads `KB` as 1000 and nginx reads `k` as
1024. Caddy's value is now written as a plain byte count so the two configs are
provably the same number. If you change one, change both, and keep them equal.

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

### 4.5 No active health check on the proxy

Neither config gives the proxy an active upstream health check, and the Caddy
one used to. Removing it fixed a real outage mode rather than saving a request.

`handlers::health` asks the node for its chain tip and returns `503
{"status":"degraded"}` whenever the node is unreachable. Caddy treats any
non-2xx probe response as an unhealthy upstream. With exactly one upstream
there is nothing to fail over to, so Caddy took **every** request off the hub —
reads included — the moment the node blinked, which:

- contradicts §5's "the hub comes up and serves reads with the node down" and
  the alerting in §8.2 that depends on it;
- made a degraded hub indistinguishable from a dead one, since the client sees
  a proxy error either way and never reaches the `"degraded"` body;
- meant the §8.1 self-ban — an hour-long node outage caused by a single stray
  TCP probe — took the whole site down with it, instead of the read surface
  staying up.

`health_status` takes one value, so it cannot express "200 or 503". The other
options were to probe a route that does not touch the node, or to drop the
check. Dropping it is right for a single upstream: a health check there can only
ever remove capacity, and if the hub process is genuinely down the dial fails and
Caddy answers `502` regardless. Monitoring polls `/health` itself (§8.2) and can
tell `503`-degraded from no answer at all, which is the distinction that matters.

If a second hub upstream ever exists, an active check earns its place again —
but it must probe something that does not depend on the node, or it will take
both upstreams out together.

### 4.6 The proxy must not touch the request path

**Every authenticated request will fail if the path is rewritten, and nothing
will say so.** Since 2026-09-05 the signing string binds the HTTP method and the
**concrete path** (plan §3.3), and the hub verifies against `OriginalUri` — the
path exactly as it arrived, not the route the handler expected. `hub/src/auth.rs`
is explicit that a handler must never substitute its own idea of the route.

So a proxy that changes the path invalidates every signature:

| Mistake | What the hub receives | Result |
|---|---|---|
| `proxy_pass http://127.0.0.1:9100/;` (trailing slash) | nginx substitutes the matched location, path is rewritten | 100% `401` |
| `handle_path /api/*` in Caddy | prefix stripped before proxying | 100% `401` |
| Mounting the hub under `/api` without the client knowing | client signs `/tasks`, hub sees `/api/tasks` | 100% `401` |
| Adding a rewrite, redirect-to-canonical, or trailing-slash normaliser | path differs by one character | 100% `401` |

The symptom is the worst kind: reads keep working perfectly, so the site looks
healthy, and every write fails with a signature error that looks like a client
bug. The proxy logs a normal `401`. The hub logs a failed verification. Neither
mentions the path.

Both shipped configs get this right and are commented at the line that matters —
`reverse_proxy 127.0.0.1:9100` inside a bare `handle` for Caddy, and
`proxy_pass http://127.0.0.1:9100;` with **no URI part** for nginx. If you must
mount the hub under a prefix, the clients have to sign the prefixed path, which
means changing the SDKs' base URL handling, not the proxy.

### 4.7 Compression

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
# Installed as itx-node / itx-hub / itx-miner, NOT under their build names.
# `target/release/node` and `target/release/hub` in /usr/local/bin would shadow
# Node.js and GitHub's `hub` for every user on the box, /usr/local/bin coming
# first on the default PATH -- and a treasury host is a bad place to discover
# that `node` now means something else.
for b in node hub miner; do
    sudo install -m 0755 "target/release/$b" "/usr/local/bin/itx-$b"
done

sudo cp deploy/itx-*.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now itx-node itx-hub itx-miner
```

The units' `ExecStart` lines and `deploy/itx-restore-drill.sh` both expect the
`itx-` names. If you are upgrading a box installed the old way, install the new
names and remove `/usr/local/bin/node` and `/usr/local/bin/hub`.

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
- **A freshly deployed hub cannot pay out for one block, on purpose.** Before
  the listener opens it splits its wallet across many outputs, because a
  payment's change is unconfirmed until mined and a one-output wallet can
  therefore make one payment per block (plan §6.4b). That first split spends
  the only confirmed output there was, so until it is mined the hub answers
  faucet grants with 503 and `Retry-After`. Expect it once, at first start on
  a funded operator, and expect `hub_operator_ready_outputs` to read 0 for that
  block. A restart of a warm hub does nothing — its wallet is already split.
  If it persists, the hub could not reach the node at boot; check
  `hub_operator_fan_out_failures_total`.
- **`--trusted-proxies 127.0.0.1,::1`** is in the hub's `ExecStart`. If you move
  the proxy off-box, this is the line to change, and §4.3 is the reason it
  matters.
- **All three key paths are passed explicitly** even though the hub has
  defaults for them. The defaults resolve against the working directory, which
  means the location of the treasury key would otherwise be implied by a
  `WorkingDirectory=` line thirty lines away in a unit file. State it where it
  is read.
- **`StateDirectory=` on all three units** creates `/var/lib/itx` (and
  `/var/lib/itx/secrets` for the hub) owned by `itx` before `ExecStart`, at mode
  `0700`. The `mkdir` above is still worth running — you want the directory to
  exist before anything else — but the units no longer *depend* on someone
  having run it. Without `StateDirectory=`, a missing `secrets/` is an immediate
  hub exit: the hub generates its keys through `PrivateKey::save_to_file`, which
  does not create parent directories, and `Restart=always` turns that into a
  restart loop on the box holding the treasury.
- **`Environment=NO_COLOR=1` on all three.** `tracing_subscriber::fmt` enables
  ANSI colour whenever the `ansi` feature is compiled in — it never checks for a
  terminal — so without this the journal stores escape sequences around every
  level and target, and §8.4's greps stop matching. Verified against
  tracing-subscriber 0.3.23, which is what `Cargo.lock` pins.
- **A hub restart drains, and a hard kill does not.** See §9.9: the replay
  guard claims an envelope's signature *before* the handler runs, so a request
  killed mid-handler dies with its envelope already spent and cannot be
  retried. `systemctl restart itx-hub` sends SIGTERM and the hub finishes what
  it is doing first; `kill -9` does not.
- **Stopping the node is not free either.** See §7.2: the mempool is
  memory-only, so a node restart still discards every transaction submitted
  since the last mined block. Since 2026-09-06 the hub detects and re-sends
  the *task bounties* among them on its own (plan §6.5); faucet grants, escrow
  disbursements and exchange withdrawals in that window are still lost
  silently.

### Verifying a cold start

The hub prints what it loaded and what it trusts. This banner is the cheapest
deployment check available, so read it rather than tailing past it:

```bash
sudo journalctl -u itx-hub -n 40 --no-pager
```

Confirm, in order: the three addresses (operator, exchange custody, and the
escrow secret path), the `trusting X-Forwarded-For only from: 127.0.0.1, ::1`
line, the restored-record counts, and the replay-guard line. A healthy start
says:

```
restored N replay-guard signature(s) still inside the drift window
```

If it reports the fallback instead —

```
WARNING: replay log unreadable (...); authenticated writes are refused for the
next 120s while the post-restart replay window closes.
```

— **that is an incident on its first appearance, not a warm-up message.** The
hub only takes that path when `ReplayGuard::restore` *fails* to read its redb
table, and it logs the reason at `error!` immediately above. It does not print
this after an ordinary crash, or on a cold start, or once while it settles: a
hub whose replay log is readable and empty prints `restored 0 …` and carries on.
Seeing it at all means the store could not be read. §9.5 has what to do.

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
   by the escrow confirmation window, not indefinite.

   Confirming that it *is* empty is more awkward than it sounds, and worth
   planning for rather than discovering: there is no endpoint that reports the
   pending-deposit count, and `all_pending_deposits` is only ever surfaced in
   the hub's startup banner — so reading it means a restart, which is a thing
   you were going to do in step 3 anyway. The workable procedure is:

   ```bash
   # stop taking new deposits (step 1), then wait out the escrow window
   sudo systemctl restart itx-hub
   sudo journalctl -u itx-hub -b --no-pager | grep 'pending escrow deposit'
   ```

   That is the `loaded N task(s), … M pending escrow deposit(s), … from store`
   line — `all_pending_deposits().count()` is one of its fields and the only
   place the number appears.

   A non-zero count means those deposits still need the *old* secret: do not
   swap the file, wait another window and restart again. If you would rather
   not restart a second time, the alternative is to read the count out of
   `hub.redb` with an external redb tool while the hub is stopped — same
   ordering constraint as §9.7, and no less of an outage. An endpoint (or a
   banner line on SIGHUP) that answers this without a restart is worth adding
   before the first rotation.
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
- **`miner.pub.pem` is public** — it is the address block rewards pay to, and
  it genuinely needs no protection. Its *private half* is a different matter
  entirely and has its own section below (§6.5); this bullet used to call
  `miner.pub.pem` "the one key file that needs no protection", which was true
  of the file and quietly wrong about the pair.
- **Check the permissions after any restore or manual copy.** The hub sets
  `0600` when it *creates* a file; `cp` and `tar` do not necessarily preserve
  it, and nothing re-checks at startup:

  ```bash
  sudo find /var/lib/itx/secrets -type f ! -perm 600 -ls   # expect no output
  ```
- **Custody on a separate host** is the plan's eventual §3.1 answer and is not
  addressed here. Until then, "protect the hub box" is the entire control, which
  is why §1 puts the firewall and §5's hardening where it does.

### 6.5 The fourth key: the miner's

There are not three key files in this deployment, there are four, and the fourth
is the one nothing in here used to mention. `deploy/itx-miner.service` points at
`/var/lib/itx/miner.pub.pem` — the address every block reward is paid to. That
file really is public. But it has a private half, and **every coin this
deployment has ever mined is spendable only with it.**

There is exactly one generator, `lib/src/bin/key_gen.rs`, and it always writes
the pair:

```bash
cargo run --release -p btclib --bin key_gen -- miner
# -> miner.pub.pem     public: the payout address, goes on the hub box
# -> miner.priv.cbor   spends every block reward, ever
```

**Generate it somewhere that is not the hub box, and copy only `miner.pub.pem`
across.** The miner process loads a `PublicKey` and never needs the private
half; the wallet does, and the wallet does not have to run here. Keep
`miner.priv.cbor` wherever the `age` restore identity from §7.1 lives — the same
offline place, for the same reason.

Done that way it is deliberately **not** in the backup set, and that is the
right answer rather than an oversight: it is not on the box, and putting it here
so the nightly job can pick it up would place a spendable key on the hub box in
exchange for nothing the miner needs.

If it *is* on the box — which is what happens when someone runs `key_gen` here,
and is the common case — then `deploy/itx-backup.sh` picks it up and says so:

```
note: miner.priv.cbor is on this box, so it is in this archive.
```

Backing it up is strictly better than losing it, but treat that line as a
to-do. On the box it is a fourth secret sitting outside `secrets/`, with no
`0700` directory around it, no mode enforcement, and none of §5's hardening
aimed at it. Move it off and re-run the backup.

Whichever layout you choose, write down which one it is. "We assumed it was in
the backup" and "we assumed it was offline" fail the same way.

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

A nightly run stops `itx-hub` for the length of the copy — seconds — and leaves
`itx-node` alone. §7.2 is why that asymmetry matters and is not a detail.

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

The three secrets, `hub.redb`, `blockchain.redb`, and the miner key files if
they are on the box (§6.5).

`blockchain.redb` is not resyncable. There is no peer to fetch the chain from —
this is a single-node deployment — so that file *is* the ledger. Losing it loses
every balance the hub reports.

> ### ⚠ Stopping the node still costs, but no longer silently
>
> This applies to the backup, to `systemctl restart itx-node`, to a deploy, to
> §9.0's containment lever, and to the box rebooting.
>
> **The node's mempool is memory-only.** `btclib::store` defines exactly three
> tables — `blocks`, `meta`, `bans` — and `node/src/util.rs`'s
> `persist_chain_state` writes blocks and the active chain and nothing else. A
> transaction that has been accepted but not yet mined exists in one process's
> RAM and nowhere else. Stopping the node still discards it. That has not
> changed and cannot be changed from the hub's side.
>
> **What changed, 2026-09-06: the hub now notices.** A task bounty is no longer
> marked `Paid` on a successful send. It goes to `Submitted`, and the sweep
> asks the node what became of it — is the recipient's output on chain, or are
> the inputs still sitting unspent? A payout the node lost is detected and
> re-sent automatically, up to four submissions, and a payout that is genuinely
> unresolvable stays `Submitted` and says so in the log rather than being
> quietly called paid. See plan §6.5.
>
> **So a node stop no longer destroys bounty payouts.** It delays them by up to
> a sweep interval plus the resolution grace, i.e. under two minutes, and the
> hub recovers them without a human. The old instruction to audit `GET
> /tasks?status=paid` by hand after every restart is withdrawn.
>
> **Three payment paths are still fire-and-forget** and a node stop does still
> destroy those: **faucet grants, escrow disbursement** (refunds, dispute-bond
> settlement, the exchange deposit sweep) **and exchange withdrawals**. They are
> the same fix against a different status field and are listed as outstanding in
> plan §6.5.
>
> **After any node stop or restart:**
>
> 1. `curl -s localhost:9100/health` for chain height before and after.
> 2. `GET /tasks?status=submitted` — payouts the hub is still waiting on. This
>    list draining to empty over the next few minutes is the recovery working.
>    A task sitting here for more than a few sweeps wants §9.10.
> 3. `GET /tasks?status=payoutfailed` — payouts the hub proved it could not
>    land and gave up on. These need you; see §9.10.
> 4. Faucet grants, exchange withdrawals and escrow disbursements in the window,
>    by hand, against the recipient's balance. There is still no re-drive path
>    for these three.
>
> It is no longer the single most expensive thing in this document, and taking
> the node down for a backup no longer costs bounty settlements.

Both `.redb` files have a live writer, and copying one underneath a running
process can capture a state that no single instant ever had: `cp` reads the file
sequentially over some hundreds of milliseconds, and redb — copy-on-write, with a
two-phase commit — is free to rewrite pages behind the read head while it does.

Two outcomes, and this document used to describe only the first:

- **redb refuses to open the copy.** Unpleasant, and you find out during a
  restore, but at least you find out.
- **The copy opens cleanly and is wrong.** A coherent-looking mix of two
  commits: the god byte points at a tree whose pages came from either side of a
  write. Nothing reports this. The drill in §7.4 passes on it, because every
  step it runs — checksums, escrow fingerprint, a hub that starts and answers
  `/health` — is satisfied by a file that is internally consistent and missing
  rows. Step 7's record counts are the only thing that would notice, and only if
  you actually compare them against production.

So the script's default is now: **stop `itx-hub`, never the node.** The hub's
writes are all durable, it restarts in seconds, and stopping it is what makes
`hub.redb` consistent — while the node keeps running, keeps its mempool, and
keeps mining what is in it. `blockchain.redb` is copied hot, and that residual
risk is stated rather than removed.

It is reduced, though. The script tries `cp --reflink=always` first, which
succeeds on btrfs and XFS-with-reflinks and is a single `FICLONE`: the copy is
the file exactly as it existed at one instant, which is precisely the
crash-consistent image redb is built to recover. Elsewhere it falls back to a
sequential read. Which one happened is recorded in the archive's `MANIFEST.txt`
as `redb copy method:`, so a restore knows what it is holding.

The flags:

- **default** — hub stopped, node running. This is the one for cron.
- **`--no-stop`** — stop nothing. For hosts where `/var/lib/itx` is on
  LVM/ZFS/btrfs and you are snapshotting underneath the script. Not for avoiding
  the outage.
- **`--stop-node`** — the old behaviour, for a planned maintenance backup where
  you have drained the hub and waited out a block. It prints the warning above
  before it does anything. Cheaper than it was — bounty payouts caught in it now
  recover themselves — but the other three payment paths do not, so **still
  never put it in a cron line.**

There is no honest way to make the script wait for the mempool to drain instead:
the node exposes no query for it (`FetchTemplate` would reveal it, but only to
something that speaks the wire protocol), and a bare TCP probe of port 9000 gets
the box banned for an hour (§8.1).

The miner restart-loops while the node is down and recovers on its own; that is
expected and needs no handling (see `deploy/itx-miner.service`).

### 7.3 The manifest, and why the escrow secret gets its own line

Each archive carries `MANIFEST.sha256` (every file) and `MANIFEST.txt` (host,
timestamp, which services were stopped, whether the redb files were reflinked or
copied sequentially, whether the miner private key is inside, and the escrow
secret's SHA-256 and length). `MANIFEST.txt` is also written *beside* the archive in cleartext, so a
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
state directory and never stops a live service.

It is **not** unconditionally safe on the production box, and this section used
to say it was. Step 5 starts a hub against the restored operator, custody and
escrow keys — the real ones — and the hub binds `0.0.0.0` with no way to say
otherwise (§1). So for the length of the drill there is a second listener on
`:19001` holding the live treasury keys, on a port `deploy/nftables.conf` has no
rule for.

The script now closes that itself: step 0 re-execs inside a private network
namespace (`unshare --net`), where `0.0.0.0` means that namespace's own loopback
and there is no route in or out at all. That is safe by construction rather than
by trusting the firewall. If `unshare` or `ip` is missing, or the kernel refuses
unprivileged user namespaces, the drill says so and runs anyway — and then the
warning above applies in full. Read step 0's output; do not assume it isolated.

Running the drill on a machine that is not the hub box is still better than
either.

```bash
/usr/local/bin/itx-restore-drill.sh \
    --archive /var/backups/itx/itx-20260905T030000Z.tar.gz.age \
    --identity ~/itx-restore-key.txt \
    --expect-operator "$(sudo journalctl -u itx-hub -b --no-pager \
                          | grep -A1 'hub operator address' | tail -1)" \
    --expect-escrow-sha256 3f1a...
```

That `--expect-operator` command used to read `/var/log/itx-hub-banner.txt`,
which nothing in this deployment creates — the banner goes to the journal like
everything else (§8.4). The form above reads it from the current boot. If the
hub has been restarted since, drop `-b`; if the journal has rotated past it,
that is exactly the case §8.4 says to keep a copy for, and the file is then
whatever path you chose to keep it at.

The steps, and what each one actually proves:

0. **Isolate.** Re-exec inside a private network namespace, so the hub in step 5
   cannot be reached from anywhere. Prints a warning instead if it cannot; see
   above for why that warning matters.
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

   Two things this step gets right that are easy to get wrong. It waits for the
   node's `Listening on …` line, not for its first output — the node prints
   `found N blocks in the local store, loading...` before it replays the chain
   and long before it binds, so waiting on "any output" starts the hub against a
   node that is not up. And it reads `/health`'s status code rather than using
   `curl -sf`, which fails on the `503` a hub returns while the node is
   unreachable and cannot tell that apart from no answer at all. `503` now
   reports "the restored node did not answer"; only a genuine silence reports
   "hub never answered /health".
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

`handlers::health` asks the node for its chain tip, so every call is a node
round trip. It is no longer a *connection* per call: `hub/src/node_client.rs`
keeps a pool of up to `MAX_POOLED_CONNECTIONS = 8` persistent connections and
reuses them, retrying once on a fresh one if a pooled socket turns out to have
been closed while idle. This section used to say the hub "opens a fresh TCP
connection per call" and that a one-second check was "60 new connections a
minute"; that was true before the pooling change and is not now. A one-second
check is 60 request/reply exchanges a minute over a handful of sockets, with no
handshake per call.

**Poll it every 30s anyway.** The interval was a fine choice for a different
reason and remains one: `/health` has its own rate-limit tier (`Tier::Health`,
120 requests per 60s window per client) precisely so a read flood cannot make
monitoring lie — a 429 on `/health` reads to an uptime check as "the hub is
down", which is the wrong thing to say under load. 120/min is two per second, so
a 30s poll uses about 1% of the budget and several independent monitors still
fit comfortably. The pool is also a *ceiling* of 8 sockets, so a monitor that
polls hard now queues against that bound instead of opening sockets without
limit — which bounds the damage but does not make hammering it free.

Note that this is now **your** monitoring's job alone. The Caddyfile no longer
runs an active health check of its own (§4.5), so nothing but your monitor is
drawing on that bucket unless you added something.

Alert on:

- **`503` / `"status": "degraded"`** — the hub is up, the node is unreachable.
  Given §8.1, treat "degraded with the node process running" as a suspected
  self-ban until proven otherwise.
- **`chain_height` not advancing** for more than a few block intervals. The
  effective cadence on a local stack is ~35s (16s target, 5s miner template
  interval), so ~5 minutes of no movement means the miner has stopped.

### 8.3 The metrics in plan §9, and which of them you can actually get

**Updated 2026-09-06: the hub now exposes `/metrics`.** Prometheus text
format, rendered from in-memory counters. What follows is the scoreboard;
every row says obtainable or not, and the "how" column names the series.

| Metric (plan §9) | Available today? | How |
|---|---|---|
| per-endpoint p99 | **yes** | `hub_http_request_duration_seconds` histogram, labelled by route template and method. Also still in the proxy access log |
| 429 rate, per endpoint | **yes** | `hub_rate_limited_total{tier=…}` for the per-IP tiers, `hub_rate_limited_per_key_total` for the per-key quota, and `hub_http_requests_total{status="4xx"}` per route |
| node connection health | **yes** | `hub_node_connections_{opened,reused,retried}_total`, `hub_node_connect_failures_total`, and `hub_node_pool_saturation_waits_total` for queueing before it shows as latency |
| payout retry depth | **yes** | `hub_board_outstanding_payouts`, a gauge sampled each sweep — this was previously a log grep, and a log count is not a depth |
| sweep-loop lag | **yes** | `hub_sweep_last_lag_ms` / `hub_sweep_max_lag_ms`, with `hub_sweep_last_duration_ms` beside them. Lag is the alert; duration is the explanation |
| exchange solvency | **yes** | `hub_exchange_custody_balance` against `hub_exchange_liabilities`, both sampled each sweep, with `hub_exchange_solvency_check_failures_total` to say when the pair is stale |
| replay guard / commit latency | **yes** | `hub_replay_signatures_{claimed,rejected,evicted}_total`, `hub_replay_durable_write_failures_total`, and `hub_replay_durable_write_ms_total` — the last is the fsync that bounds the whole write path (plan §6.3) |
| chain height + freshness | **yes** | `hub_chain_height` with `hub_chain_observation_age_seconds`. The age is the point: a hub that lost its node keeps reporting the last height it knew |
| faucet grants | **yes** | `hub_faucet_grants`, sampled from the board each sweep |
| operator payout capacity | **yes** | `hub_operator_ready_outputs`, sampled each sweep — confirmed operator outputs large enough to fund a payment on their own. This *is* the payout ceiling (plan §6.4b): one payment consumes one output and returns its change unconfirmed, so the hub makes about this many payments per block. `hub_operator_fan_outs_total` and `hub_operator_fan_out_failures_total` say whether the hub is keeping it topped up |
| custody payout capacity | **yes** | `hub_custody_ready_outputs`, and `hub_custody_fan_outs_total` / `hub_custody_fan_out_failures_total` beside it. The same ceiling and the same fix, on the address that funds exchange withdrawals rather than grants and bounties. Alert on it separately: the two wallets run dry for different reasons — the operator's when the faucet is busy, custody's when traders withdraw — and one alert covering both sends whoever is on call to the wrong half of the hub |
| node wedged vs node down | **yes** | `hub_node_timeouts_total` against `hub_node_connect_failures_total`. Dial failures mean the hub was refused, which is a node that is down and visible in a dozen other places. Timeouts mean the hub was accepted and then ignored, which has no socket-level symptom at all. Any sustained value is worth paging on: every operator payment shares one lock, so a wedged node stalls settlement while the hub still answers `/health` |
| faucet burn *in units* | **no** | grants × grant size, and the grant size belongs to the faucet workstream (plan §5), which is rewriting it and may make it vary with PoW difficulty. Deliberately not duplicated here: a second copy of that constant would go stale silently and report a wrong number of coins burned. Lands with §5 |
| board lock contention | **partly** | `hub_sweep_board_lock_wait_ms_total` — the sweep's own wait for the board *write* lock, which only proceeds once every reader has drained, so it detects readers starving the writer. It cannot see reader-versus-reader contention. Full coverage needs the per-handler instrumentation plan §10.1 defers |
| challenge solve-rate | **no** | the faucet PoW landed 2026-09-06 (plan §5), but nothing counts issuances against redemptions. `hub_faucet_grants` is the redeemed side only. Worth having: the ratio is what says whether the difficulty is set anywhere near right |

**Where the endpoint is exposed, and why.** `/metrics` is a route on the
hub's ordinary port, not a second listener. The §1 threat model asks to
reduce the reachable surface to exactly one port, and a second listener
adds one; with `--bind 127.0.0.1` and the proxy in front (§4), the proxy
is already the thing deciding who reaches what, so that is where the
decision belongs. **Block it there** — the section below has the config.
It is not secret in the sense the three keys are (it exposes no pubkey and
no per-agent row, and the custody balance it reports is on a public
chain), but aggregate liabilities and faucet burn are operational detail a
stranger has no reason to read.

Two properties hold it safe to leave unauthenticated behind that proxy,
and both are enforced by tests rather than by intent:

- **A scrape never reaches the node.** It is the cheapest call on the hub,
  so a fan-out to the chain would make it the most efficient amplifier on
  the box — one unauthenticated request turning into a TCP round trip
  competing with real payouts for the connection pool. Everything needing
  the node (the chain tip, the custody balance) is sampled by the sweep
  instead. `a_metrics_scrape_never_reaches_the_node` in `hub/src/main.rs`
  is the guard.
- **A scrape never takes the board lock.** Board contention is one of the
  things being measured, and an observer that queued for the same lock
  would be reporting on itself.

The cost of both is staleness: any board- or chain-derived gauge is up to
one sweep interval (60s) old. That is the right trade for an operational
dashboard and the wrong one for anything transactional, which is why none
of these numbers is used for a decision inside the hub.

**`/metrics` has its own rate-limit tier.** A 429 on a scrape reads as an
outage to a monitor, and sharing the `Read` bucket would mean the read
flood you are trying to diagnose is also what blinds you to it. Verified
end to end: with the read tier exhausted and returning 429, `/metrics` and
`/health` both still answer 200.

**Alert on these.** The first three are the ones that fire before users
notice:

| Alert | Expression | Why |
|---|---|---|
| Sweep stalled | `hub_sweep_last_lag_ms > 30000` | payouts, claim expiry and escrow refunds all ride the sweep. Lag, not duration: duration says the work got slower, lag says it is not being started |
| Chain observation stale | `hub_chain_observation_age_seconds > 180` | three blocks at the 16s target. Catches a lost node, which `hub_chain_height` alone cannot — it keeps reporting the last height known |
| Insolvent | `hub_exchange_custody_balance < hub_exchange_liabilities` | the one number where being wrong is a financial statement. Pair it with `increase(hub_exchange_solvency_check_failures_total[10m]) > 0`, or a hub that stopped checking looks solvent |
| Commit path degraded | `rate(hub_replay_durable_write_ms_total[5m]) / rate(hub_replay_signatures_claimed_total[5m]) > 50` | the write path is bounded by this fsync (§6.3), not by CPU. Request rate alone will not explain a slow hub |
| Burned envelopes | `increase(hub_replay_durable_write_failures_total[5m]) > 0` | each one is a request the client cannot retry. Page, do not graph |
| Node pool saturated | `rate(hub_node_pool_saturation_waits_total[5m]) > 1` | queueing on the node, visible before it becomes request latency |
| Operator wallet flat | `hub_operator_ready_outputs < 4` | the hub is about to start refusing faucet grants and operator-funded settlement, and the 503s it sends will look like a client problem. Four rather than zero, because zero is already the outage. Expect it to read 0 for one block on a fresh deploy while the first split confirms (plan §6.4b) |
| Wallet cannot be reshaped | `increase(hub_operator_fan_out_failures_total[10m]) > 0` | the hub could not read, build or send a split. Nothing else says so: the wallet simply stays short and every payout starts failing a minute later for what looks like an unrelated reason |
| Every client 429ing | `hub_rate_limited_total{tier="read"}` rising across all clients at once | §4.3's failure 1 — see below; this is the alert that catches it |

Alert on the **429 rate going to ~100% across all clients at once**. That is not
an attack; that is §4.3's failure 1 — `--trusted-proxies` unset or wrong, every
agent sharing the proxy's bucket. It is the one alert that catches a silent
misconfiguration nothing else reports. `hub_rate_limited_per_key_total` is what
tells the two apart: a genuine flood moves both counters, that misconfiguration
moves only the per-IP one.

**Blocking `/metrics` at the proxy.** Caddy:

```caddyfile
@metrics path /metrics
respond @metrics 404
```

nginx:

```nginx
location = /metrics {
    allow 127.0.0.1;
    deny all;
}
```

Scrape it from the box itself (`curl -s localhost:9100/metrics`), or open
it to the monitoring host only.

The proxy's access log remains a useful cross-check, and is the only
source that survives the hub process dying. A usable p99 from Caddy's JSON
log:

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

**`journalctl -p warning` does not work here, and it fails by returning
nothing.** All three binaries log through `tracing_subscriber::fmt` to stdout,
which emits no syslog priority prefix, and the units set no `SyslogLevel=`. So
systemd records every line — `info`, `warn` and `error` alike — at priority
`info`, and `-p warning` filters out exactly the lines you were looking for.
This section used to recommend it for "retries and bans"; every line that table
depends on (`node/src/ban.rs`'s `banning peer`, `handlers.rs`'s `will retry`,
`main.rs`'s `retried and paid out`) was being filtered out.

There is no fix available from the unit file: `SyslogLevel=` sets one priority
for the whole stream, and systemd only reads per-line levels from a `<N>` prefix
that `tracing` does not emit. So grep the level out of the message instead. The
units set `Environment=NO_COLOR=1` (§5) specifically so this works — without it
`tracing` wraps the level in ANSI escapes and the pattern below misses.

```bash
journalctl -u itx-hub -f

# anything above info, from any of the three
journalctl -u itx-hub -u itx-node -u itx-miner --since '1 hour ago' \
  | grep -E ' (WARN|ERROR) '

# payout retries and sweep recoveries -- §8.3's "payout retry depth"
journalctl -u itx-hub --since '1 hour ago' \
  | grep -E 'failed, will retry|retried and paid out'

# peer bans -- §8.1, and usually the box banning itself
journalctl -u itx-node --since '1 hour ago' | grep 'banning peer'

# the replay guard failing to restore: an incident, see §9.5
journalctl -u itx-hub -b | grep -E 'could not restore the durable replay guard|replay log unreadable'
```

Two habits worth having:

- **Keep the hub's startup banner.** It is the only record of which addresses
  and which trusted proxies a given run used, and §7.4's drill compares against
  it. Nothing writes it to a file — it goes to the journal like everything else
  — so after every deploy, put a copy somewhere that outlives journal rotation:

  ```bash
  sudo journalctl -u itx-hub -b --no-pager | head -40 \
    | sudo tee -a /var/log/itx-hub-banner.txt >/dev/null
  ```

  That path is a convention, not something the stack creates; use it or pick
  your own, but pick one, because §7.4's `--expect-operator` needs it once the
  journal has rotated past the last cold start.
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

**It also destroys the mempool** — every transaction submitted since the last
mined block, including payouts the hub has already marked `Paid`. See §7.2's
warning and its post-stop checklist. That is the right price for containment
when funds are being drained; it is the wrong price for anything routine, which
is why the nightly backup no longer pays it.

### 9.1 Suspected key compromise

The worst case, and the one to rehearse. Any of: the box was accessed, a secret
was copied off it, a backup archive leaked with its identity file, or funds are
moving that the hub did not send.

1. **Halt.** `sudo systemctl stop itx-hub itx-node itx-miner`. In that order —
   the hub first so it stops signing, the chain second so nothing already signed
   confirms. Stopping the node discards the mempool (§7.2), which here is a
   feature: an attacker's submitted-but-unmined transactions go with it. Note
   that legitimate ones do too, so the §7.2 checklist still applies afterwards.
   Bear in mind that the hub will re-send the legitimate bounty payouts among
   them by itself once it is back up — which is right, but means "halted" is
   not "nothing will move"; leave the hub down until you have finished §9.1.
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

1. Confirm: `journalctl -u itx-node | grep 'banning peer'` — the node logs
   `banning peer <ip> until <time>`. It is a `warn!` in the code but lands in
   the journal at `info` (§8.4), so grep the text; `-p warning` finds nothing.
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
   `daemon-reload` and restart. The restart itself costs the requests in flight
   at that moment, and they cannot be retried — see §9.9. Under a 100% 429 rate
   there is not much in flight worth saving, but say so to whoever is watching
   rather than letting it look like a second fault.

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
  next 120s`** — **treat the first one as an incident.** This document used to
  say that once after a crash it was working as designed. It is not. The hub
  only reaches that branch when `ReplayGuard::restore` *fails* to read the
  durable replay table, and the line immediately above it is an `error!`:

  ```
  could not restore the durable replay guard (<the redb error>) -- falling back to refusing
  ```

  What is by design is the *fallback* — refusing authenticated writes for the
  drift window rather than serving with a hole in the replay guard (plan §3.3).
  What is not by design is needing it. A hub with a readable, empty log prints
  `restored 0 replay-guard signature(s) …` and carries on; it never prints this
  after an ordinary crash. So:

  1. Read the redb error in the `error!` line — that is the actual diagnosis.
  2. Do not wait it out and move on. The store that failed to read is
     `hub.redb`, the same file holding the board, every exchange account and
     every agent name.
  3. Stop the hub and check the file before it takes more writes
     (`fuser /var/lib/itx/hub.redb` first; redb is single-process).
  4. If it is damaged, this is §9.7, and the last backup that passed a drill is
     what you restore.

  Read routes are unaffected throughout, which is exactly why this can sit
  unnoticed. Alert on the string.
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

### 9.9 What a restart costs the requests in flight

`systemctl restart itx-hub` sends SIGTERM, and the hub drains on it: it stops
accepting connections, lets the requests already running finish, and then
exits. Requests that arrive during the drain are refused at the socket.

That distinction matters more here than it does for most services, because of
how authentication works. `hub/src/auth.rs` claims an envelope's signature in
the replay guard **before the handler runs**, and fsyncs it
(`record_seen_signature`). The ordering is deliberate and correct — a signature
must be spent before the work it authorises, or a crash between the two leaves
a replayable envelope that has already moved money — but it means a request
killed mid-handler has already spent its envelope. The client would see a
connection reset, retry the identical envelope the way any sensible HTTP client
does, and be rejected as a replay. The request would neither have happened nor
be repeatable; only a *new* signature would work.

Draining is what keeps that from happening on an ordinary restart. A connection
refused before it is accepted costs nothing: no envelope was claimed, and the
client can retry the same one.

Two things still to know:

- **A hard kill is different.** `SIGKILL`, an OOM kill, or a power loss gives no
  drain, and the requests in flight lose their envelopes exactly as described
  above. Prefer `systemctl restart`; do not `kill -9` the hub to hurry it.
- **Clients should still handle it.** A replay rejection immediately after a
  connection reset means re-sign, not back off, and it belongs in the SDK docs
  as much as here. Never "fix" it client-side by reusing the envelope — that is
  the attack the replay guard exists to stop.

### 9.10 A payout is stuck, or the hub gave up on one

Two different situations with the same shape: a worker earned a bounty and does
not have it. Both are visible in `GET /tasks` (§10.3) and neither resolves
itself.

**`status: "Submitted"` for more than a few minutes.** The hub sent a payout and
cannot work out what became of it: the recipient does not hold the output, and
the inputs it would have spent are either gone or claimed by the node's mempool.
It will not resend, deliberately — a duplicate risks paying the bounty twice and
earns a peer strike (§9.2). Almost always benign and self-clearing (the worker
spent the bounty before a sweep looked, or the node is sitting on the
transaction), so check before acting:

```bash
# What the hub is waiting on, with how much of each bounty it has seen land
curl -s localhost:9100/tasks?status=submitted | jq '.[] | {id, bounty, bounty_confirmed, bounty_pending, claimant}'

# Did the money actually arrive? Ask the chain about the recipient
itx-wallet balance --pubkey <claimant>   # or GET /reputation/<pubkey> for net_worth
```

If the recipient's balance shows the bounty, the payout landed and the hub
simply missed the window; nothing to do but note it. Persistent cases across
many tasks are the signal that the hub should follow the chain rather than poll
it — plan §6.5 records this as the number that decides that.

**`status: "PayoutFailed"`.** Not benign. The hub built and sent the payout four
times and the node was shown, every time, not to have it. The money was never
moved, no reputation was credited, and any escrow behind the task is untouched —
deliberately, since refunding the poster would take the bounty from whoever did
the work.

1. **Find out why the node refused it**, because it will refuse the next one
   too: `journalctl -u itx-node --since "1 hour ago" | grep -iE 'mempool|reject|strike'`.
   The usual causes are a fee floor the hub's flat `HUB_TRANSACTION_FEE` no
   longer clears, and the operator being out of spendable balance (§10.1).
2. **Confirm the money really is where the hub says.** `curl -s
   localhost:9100/tasks?status=payoutfailed | jq` gives `bounty_pending` per
   task; check the recipient's balance against it before paying anything.
3. **Pay it from the operator wallet by hand**, once. There is no re-drive
   endpoint and the terminal state is deliberately not automatically
   recoverable — an automatic exit would be a guess, which is the thing the
   state exists to refuse.
4. **Fix the cause before restarting the hub**, or the next payout takes the
   same path.

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

### 10.3 Settlement confirms itself, for bounties only

Fixed 2026-09-06 for task bounties; still open for everything else. Read this
with §7.2.

`submit_transaction` is fire-and-forget: the wire protocol has no reply meaning
accepted and none meaning rejected, so a successful send proves only that the
bytes left the hub. **Task bounties no longer treat that as payment.** A
verified task goes to `Submitted`, and the sweep resolves it against the node —
the recipient's output present means it was mined; the output absent with every
spent input still unspent and unmarked means it never landed, and it is re-sent;
anything else is unresolvable and it waits. Four submissions is the cap, after
which the task reaches `PayoutFailed` and stops. Plan §6.5 has the design.

**Three payment paths still say "sent" and mean it:** faucet grants, escrow
disbursement (refunds, dispute-bond settlement, the exchange deposit sweep) and
exchange withdrawals. For these the old ceiling stands in full — the hub's
record says the money moved, the chain may never have seen it, and nothing will
ever notice.

What to watch, all of it visible in the journal (§8.4):

| Line | Means | Do |
|---|---|---|
| `payout for task … failed, will retry` | The hub could not build or send. Usually an unhealthy node or the operator out of spendable balance (§10.1) | Watch the rate; it often rises before `/health` notices |
| `payout for task … never reached the chain (submission N of 4), resending` | The node did not get it, provably. Recovery working as designed | Nothing, unless N is climbing across many tasks |
| `payout for task … is unresolved` | The ambiguous case. The hub cannot tell whether it landed | §9.10 |
| `giving up on the payout of …` | Four proven losses. Money owed, nobody paid | §9.10, and treat as an incident |

Two lists to check by hand rather than by log: `GET /tasks?status=submitted` is
what the hub is still waiting on (should drain within a couple of minutes;
anything older is stuck), and `GET /tasks?status=payoutfailed` is what it gave
up on (should be empty).

#### A `Submitted` task with no payout attempt — added 2026-09-07

The one settlement state the hub will not resolve for you, and the only one it
refuses to act on at all. Plan §6.5c is the background.

**How you learn about it.** The startup banner, at `error`:

```
store reconciliation: [submitted_task_with_no_payout_attempt] task <id> reads
Submitted with no payout attempt tracking it: <n> ITX left the hub and nothing
is waiting for it
```

and `hub_reconciliation_disagreements{class="submitted_task_with_no_payout_attempt"}`
carries the count for as long as that process runs. If anything then calls the
settlement path for such a task, `hub_payout_sends_refused_total` climbs and the
journal says `refusing to send … a PayoutAttempt row was lost`.

**Why the hub stops here.** The `PayoutAttempt` held the recipient's output hash
and the inputs the transaction spent. §10.3's rule needs the first to call a
payout confirmed and the second to call it lost, and both died with the record —
so this state has *strictly less* evidence than the "unresolvable" row above,
which the hub already declines to guess at. Worse, the attempt was also the
double-spend guard, so the one code path that would still send is the one that
could pay the bounty twice. It refuses instead.

**Do not "just re-run settlement."** That was possible before 2026-09-07 and is
what this state's danger consisted of.

**The one sound test, and it only covers escrow-funded tasks.** An escrow
deposit address is single-purpose: nobody but its depositor was ever told it
exists, so every UTXO there is unambiguously that deposit's money. So:

1. Get the task's `escrow_id`, then the deposit address for it.
2. Ask the node what that address holds (the wallet's balance query, or the hub's
   own `FetchUTXOs`).
3. **If it still holds at least the bounty plus the fee, unmarked, no payment has
   been made or is pending from it** — the payout demonstrably never landed, and
   paying the winner by hand is safe.
4. **If it is drained or the funds are mempool-marked**, something went out.
   Whether it reached the winner is now a question for the chain: look for an
   output of exactly the bounty at the winner's address, in a block after the
   task's `created_at`. That is suggestive and *not* proof — two payouts of the
   same size to the same key are indistinguishable by value alone, which is why
   the hub will not conclude it either.

For an **operator-funded** task there is no equivalent test. The source is the
shared operator address, so its balance says nothing about one payout. Reconcile
against the chain by hand, and prefer paying the worker a second time over
leaving them unpaid only if you can show the first payment is absent.

**Then clear the state** so the banner stops and the task stops reading as
mid-settlement. There is no endpoint for this on purpose: the fix depends on
what you found, and a button that guessed would be the bug again.

**How you get here at all.** Only from a store written by an older binary: the
pre-2026-09-07 `record_confirmed_payout` deleted the attempt in one commit and
saved the task in another, so a crash in between left this; and a rollback to a
build predating the `payout_attempts` table made every attempt invisible at once
(§6.5c bug 5, now fenced off by the schema stamp). Both writes are single
transactions now, so a current hub cannot create this state — which is why the
check runs at boot and not on the sweep.

### 10.4 A withdrawal was submitted and never acknowledged

**What you will see.** A `WARN` naming a withdrawal id, an amount and a key, and
`hub_unresolved_withdrawals_total` moving. On a restart the same withdrawals are
listed again at boot, and `hub_unresolved_withdrawals_at_boot` carries the count.
The agent got a 500 telling it not to retry.

**What it means.** The hub built a custody payment, wrote it to the node, and the
write returned an error. That does **not** establish the node never got the
bytes: the send is fire-and-forget, and writing to a socket whose peer has
already gone does not fail. So the coin may or may not be on chain, the ledger
stays debited, and the hub is deliberately not guessing.

**Why it no longer credits the balance back.** It used to, on any error. That is
the right answer when nothing was sent and the wrong one when something was: an
agent whose transaction did land held the coin *and* the balance, and could
withdraw the same money again. The two cases are now told apart at the source — a
payment that could not be *built* still reverts, silently and correctly, and that
is the common case you will almost never hear about.

**What to do.** By hand, the same way §10.3's stuck payout is resolved.

1. Take the withdrawal's `output_hash` from the record.
   `load_all_withdrawal_attempts` is the read; there is no endpoint for it yet.
2. Ask the node what the agent's address holds. If that exact output is there the
   withdrawal happened, the ledger is already right, and the record can go.
3. If it is absent, check custody's own outputs against the recorded
   `spent_inputs`. Every one still present and unmarked means the transaction
   reached neither a block nor a mempool, so the withdrawal can be paid again —
   or the balance credited back, if the agent would rather have it.
4. Anything else is the ambiguous row and stays ambiguous. Do not pay a second
   time on a hunch; wait for the chain to settle and look again.

**What is not built.** Nothing resolves these automatically. The record exists so
the money is findable, and so a resolver can be written against it: the three
rows above are `PayoutAttempt::resolve`'s rule, which withdrawals already share,
so what is missing is a sweep step rather than a new mechanism. Until it exists,
a non-zero `hub_unresolved_withdrawals_at_boot` is a thing to act on rather than
a thing to watch.

---

## 11. What in here was actually tested

Written down so a reader knows which claims are verified and which are
reasoned. The first table was run on 2026-09-05 against a local stack (node +
hub from this tree) behind Caddy 2.11.4. The audit-fix pass on 2026-09-06
changed several of the configs and the doc; what that pass could and could not
check is listed separately below, because most of it could only be reasoned.

**Verified by running it:**

| Claim | How it was checked | Result |
|---|---|---|
| `deploy/Caddyfile` is valid | `caddy validate` | valid (three "Unnecessary header_up" warnings, one per `X-Forwarded-*` line; the one that matters is explained in §4.2) |
| …and still is after the 2026-09-06 changes | `caddy validate` + `caddy adapt`, Caddy 2.11.4 | adapts; `"max_size": 262144` in the JSON, no active health check |
| TLS terminates in front of the hub | `curl` over HTTPS to the real hub | `200`, HTTP/2, `{"status":"ok","chain_height":5}` |
| A spoofed `X-Forwarded-For` is discarded | client sends one address, a chain, and repeated headers | upstream sees exactly one entry: the real client |
| `Forwarded` / `X-Real-IP` are cleared | client sends both | neither reaches the upstream |
| Stripping actually protects the rate limit | 130 reads through the proxy, each with a different spoofed address | 119 × `200`, 11 × `429` — one shared bucket, spoofing gained nothing |
| …and that the limit fails without it | same 130 reads sent to the hub from a trusted peer with the header passed through | 130 × `200`, 0 × `429` — limit defeated |
| `security.txt` needs the proxy | requested via proxy, then direct from the hub | `200` via proxy, `404` direct |
| A `nc -z` probe bans the node | one probe against an isolated node | `banning peer 127.0.0.1 until <+1h>` |
| The ban is invisible to that probe | second `nc -z` after the ban | still `succeeded!`, while the node logs `rejecting connection from banned peer` |
| The ban survives a restart | restarted the node | `restored 1 ban(s) from a previous run` |
| The backup round-trips | `itx-backup.sh` → `itx-restore-drill.sh` | drill passed all seven steps |
| The drill fails when it should | bit flipped in the escrow secret | caught at step 2 (manifest) |
| …and when the manifest is regenerated to match | same tamper, manifest rebuilt | passed step 2, **caught at step 3** by the out-of-band fingerprint |

The settlement-confirmation rows were run on 2026-09-06 against a real local
stack — node, miner and hub from this tree, no fakes — because the claim §7.2
now makes is one that can only be checked by actually stopping a node with money
in flight:

| Claim | How it was checked | Result |
|---|---|---|
| A submitted payout reports as unconfirmed | posted, claimed and submitted a task, then read it immediately | `paid: true` in the submit response, but `status: "Submitted"`, `bounty_pending: 1000`, `bounty_confirmed: 0`, reputation `completed: 0` |
| …and confirms itself | waited one sweep | `status: "Paid"`, `bounty_confirmed: 1000`; `payout … confirmed on chain after 1 submission(s)` |
| **A node restart no longer destroys a payout** (§7.2) | stopped the miner, submitted a 2000 bounty, killed the node with it in the mempool, restarted node and miner | `never reached the chain (submission 1 of 4), resending`, then `confirmed on chain after 2 submission(s)` — recovered in two sweeps, no human involved |
| …and the resend does not double-pay | agent's on-chain balance vs. its credited earnings after the recovery | `total_earned: 3000` and `net_worth: 3000` across both tasks — the bounty was paid once, not twice |
| The operator's two lists drain | `GET /tasks?status=submitted` and `?status=payoutfailed` after the drill | both `[]` |

The metrics rows were run on 2026-09-06 against a local stack — node, miner
and hub from this tree on ports 9030/9130, binaries copied out of the build
directory first and checked with `strings` so the process under test was
provably the one just built:

| Claim | How it was checked | Result |
|---|---|---|
| `/metrics` renders | `curl` after driving 26 reads across six routes | `200`, `content-type: text/plain; version=0.0.4`, 204 lines |
| Counters track real traffic | compared per-route counts against what was sent | `/tasks` 12, `/health` 5, `/leaderboard` 4, `/board/summary` 3 — exact |
| Dynamic segments do not become labels | two different task uuids, then two invented paths | both uuids collapsed to `route="/tasks/:id"` (count 2), both invented paths to `route="other"` (count 2) |
| The sweep observes the chain | waited one sweep pass | `hub_chain_height 2`, `hub_chain_observation_age_seconds 37`, `hub_chain_observation_failures_total 0` |
| The pool is reusing connections | read the pool counters after the reads above | `opened 1`, `reused 6`, `retried 0`, `saturation_waits 0` |
| **A scrape does not reach the node** | 20 scrapes, counting node connections either side | connections unchanged; a `/health` control request first, to prove the counter moves at all |
| **A read flood does not 429 the scrape** | 130 reads to exhaust the `Read` tier, then scraped | `/tasks` → `429`, `/metrics` → `200`, `/health` → `200`; `hub_rate_limited_total{tier="read"} 34`, every other tier `0` |
| Rejections are visible per route | same flood | `hub_http_requests_total{route="/tasks",…,status="4xx"} 34` alongside `status="2xx" 109` |

**Not verified, and worth saying plainly:** the sweep's board-lock wait, the
node retry and saturation counters, the replay-guard series and the solvency
pair were all exercised as code (unit tests, and the workspace suite is green
at 353) but were only ever observed at zero on a quiet local stack. Nothing
here has watched them move under real contention, an unreachable node, or a
failing disk. The load harness (§6.7 of the plan) is what should drive them,
and doing so is the obvious next thing: this pass built the instrument and
did not yet put it under load.

**Reasoned but not run**, because the target is a Linux box and the checks were
done on macOS:

- `deploy/nftables.conf` and `deploy/ufw.sh` — the rules follow from §2's
  topology, but no Linux host was available to apply them, and `nft` is not
  installed on the machine the checks were done on, so **not even `nft -c -f`
  was run against the current file.** Run it on the real host before committing
  to it, and verify from off-box per §3 — including the `ssh -6` line, which is
  the one that would have caught the IPv4-only SSH rule.
- `deploy/nginx.conf` — `nginx -t` was not run either; nginx is not installed
  here. The `listen 443 ssl http2` form, the removal of `ssl_stapling`, the
  `charset utf-8` on the `security.txt` location and the `256k` body cap are all
  reasoned from the directive documentation, not from a parse.
- The systemd units — syntax and directives are conventional, but they have not
  been loaded by a running systemd. Check `systemd-analyze verify` on the host.
  This now includes `StateDirectory=itx itx/secrets` and
  `Environment=NO_COLOR=1`, neither of which has been exercised.
- `itx-backup.sh`'s service stop/start path. The drill was run with `--no-stop`,
  since there is no systemd on the test machine, so the `systemctl stop` branch
  and its restart-on-exit trap are untested. Exercise them once on the real
  host.
- `cp --reflink=always` in `itx-backup.sh` — macOS `cp` has no such flag, so
  only the fallback path has ever run. On the real host, check the archive's
  `MANIFEST.txt` for `redb copy method: reflink` to see which branch you got.
- The restore drill's step 0 (`unshare --net`) — Linux only, never executed
  here. Its argument-passing shape (`sh -c '… exec "$@"' sh "$0" "$@"`) was
  checked with a stub script; the namespace itself was not.
- The `journalctl … | grep -E ' (WARN|ERROR) '` recipe in §8.4. The log format
  it assumes was read out of tracing-subscriber 0.3.23's source (`fmt_layer.rs`
  gates ANSI on the `ansi` feature and honours `NO_COLOR`; `format/mod.rs`
  writes `<timestamp> <LEVEL> <target>: <message>` with no padding), and the
  version was confirmed against `Cargo.lock`. It has not been run against a real
  journal.

**Corrected on 2026-09-06, having previously been listed as verified:** the row
claiming a `localhost` upstream yields an `::1` peer. It cannot have been
observed against this hub, which binds `0.0.0.0` and so never accepts an IPv6
connection at all — a Caddy upstream written as `localhost` finds `[::1]:9100`
refused and falls back to IPv4. §4.3 now states the pair as reasoned, and the
advice to include `::1` in `--trusted-proxies` is unchanged: it is free, and it
is right now that the hub binds loopback.

**Also observed in passing:** the node logs `Listening on 0.0.0.0:9500` and
binds IPv4 only — same as the hub, and the same reason §1 leans on the firewall.
The restore drill now waits for exactly that line.

**Known-open, and not fixable in `deploy/`:** the hub assuming a submitted
transaction is a settled one (§7.2, §10.3) is a hub change — it needs
confirmation tracking, plan §2 item 10. Everything the backup script and this
document do about it is mitigation. So is §9.9's missing graceful shutdown.

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
reachable, the same probe becomes a self-ban. §7.2 explains why in full. If you
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

Include `::1`. Caddy and nginx both resolve `localhost` to IPv6 first on many
systems, and the hub compares the *peer address it sees*, not a name — a proxy
dialling `[::1]:9100` against a hub trusting only `127.0.0.1` is untrusted, and
the failure is the silent one below.

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
tell. Alert on 429 rate (§7.3): a per-endpoint 429 rate that goes to ~100% for
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

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

# console/

The operators' own window onto a running hub. Two people, one page, no
credential in a browser.

```bash
cargo run -p console --bin itx-console -- \
  --hub https://itx.example \
  --key-file ~/.itx/viewer.priv.cbor \
  --port 8787
# then open http://127.0.0.1:8787
```

## How it is put together, and why

**The key stays in this process.** `/admin/overview` is a signed read, so
something has to hold a private key. A page served by the hub would hold
it in the browser — in `localStorage`, a file input, a form field — which
puts a treasury-adjacent key in every extension's address space. Instead
this binary holds the key, signs, and serves a page that carries no
credential at all. The browser talks only to loopback.

**It binds to 127.0.0.1 and that is not configurable.** The page has no
login because the socket is the boundary. Publishing it on an interface
would hand an unauthenticated view of the hub's internals to anything
that can reach the port.

**It holds no opinions.** Alerts, thresholds and severities are computed
by the hub in `hub/src/admin.rs`, against the same numbers
`docs/deployment.md` §8.3 alerts on. A console that decided for itself
when something was wrong would drift from the runbook silently — green on
screen, "page someone" in the document, and nobody notices until it
matters. This renders what it is told.

**It is read-only, structurally.** There is one upstream request in the
whole binary and it is a read. Even a compromised console cannot move
money, because the hub does not accept an admin key anywhere that can.

## Keys

`--admin-keys` on the hub takes a comma-separated list of hex pubkeys
that may read `/admin/overview`. The operator key is always admitted, so
a single-operator deployment needs no extra configuration.

Give a collaborator their **own** viewer key rather than sharing the
operator key. The operator key moves money; watching the hub should not
require holding it, and a key shared so someone can read a dashboard is a
key on two laptops.

```bash
cargo run -p btclib --bin key_gen viewer      # -> viewer.priv.cbor / viewer.pub.pem
# hand viewer.priv.cbor to the collaborator, and add its hex pubkey to
# the hub's --admin-keys
```

## What is on the page

- **Alerts** — everything §8.3 would page on, with what it means and what
  to do. Empty is the ordinary state and says so.
- **Agents** — known keys, funded by the faucet, and *completed work*
  kept separate on purpose. Funded is free; completed is the number that
  says the product works.
- **Work** — tasks by state, including `settling` and `payout failed`,
  which the public dashboard learned about late.
- **Networks** — faucet grants grouped by /24 (v4) or /64 (v6), busiest
  first, with what the pricing curve will charge the next arrival from
  each. One network near the whole budget is the shape of a farm; many
  small rows are a population. The console shows the distribution rather
  than deciding for you.
- **Money** — custody against liabilities, solvency, payments pending and
  needing review, operator wallet depth.
- **Integrity** — replays rejected, rate limiting, burned envelopes,
  ledger/store divergences, and any live store disagreement in full.

## Two failures worth recognising

- **403** — this key is not on the hub's `--admin-keys`.
- **401** — usually the local clock has drifted outside the envelope's
  120-second window, not a bad key.

Both are shown on the page rather than swallowed, because a console that
simply stops updating is indistinguishable from a quiet hub.

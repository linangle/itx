# harness — load and chaos for the itx hub

Two halves. The first drives a hub at scale and reports latency. The second
breaks things on purpose, and is the half that finds bugs.

It is a Rust workspace member rather than a k6 or vegeta script for two
reasons. Every authenticated hub route wants a secp256k1 signature over a
canonical string (`btclib::envelope`), so an external tool would need a third
implementation of that recipe alongside `sdk` and `agent-sdk-py`, free to drift
from both; going through `sdk::build_envelope` means the harness signs exactly
the way a real agent does. And the drills that matter ask whether money
actually landed, which means reading the chain over the node's own CBOR
protocol rather than believing the hub — the hub's known failure mode is
believing a payout that never happened (plan §6.5).

It is called `harness` rather than `loadtest` because load and chaos ship
together and share the client, the chain view and the report format; naming it
after either half would be wrong about the other.

## Running it

Build release. Load numbers from a debug build are worth recording and worth
nothing as an absolute, and the report says which profile produced them.

```bash
CARGO_TARGET_DIR=~/itx/target cargo build --release -p node -p miner -p hub -p harness
```

### Drills

Each drill brings up its own node, miner and hub on ports 9040/9140, in a
working directory it wipes first and fills with chain data.

```bash
CARGO_TARGET_DIR=~/itx/target cargo run --release -p harness -- drill all --out harness/results
```

One at a time, by name: `node-crash`, `escrow-restart`, `replay-storm`,
`rate-limit-tiers`, `quota-isolation`, `payout-ceiling`, `signed-write-cost`.
The process exits non-zero if any drill refuted the plan or found a bug, so it
can be put in front of a change and be told rather than read.

`--build-dir` chooses where `node`, `miner` and `hub` are taken from
(`target/release` by default). They are **copied** into the run's own directory
before anything starts, and every report records the SHA256 of what it
launched: `target/release/hub` is one path for every worktree on this machine,
and a measurement that does not name the bytes it ran against is not evidence.

`--work-root` chooses where chain data goes (`$TMPDIR/itx-harness` by default).
Check free space before a long run; a mining node fills a chain file steadily.

### Load

Load is pointed at a stack somebody else is running. `harness stack` is the
somebody else — it brings up node, miner and hub on the same ports, holds them
until ctrl-c, and prints the operator key path that `--funding-key` wants.

```bash
CARGO_TARGET_DIR=~/itx/target cargo run --release -p harness -- stack --trust-local-proxy
```

```bash
CARGO_TARGET_DIR=~/itx/target cargo run --release -p harness -- load \
  --hub http://127.0.0.1:9140 --node 127.0.0.1:9040 \
  --agents 1000 --seconds 60 --distinct-sources \
  --funding-key /path/to/operator.priv.cbor --out harness/results/load.json
```

`--distinct-sources` gives each agent its own synthetic address in
`X-Forwarded-For`, and needs a hub started with `--trusted-proxies 127.0.0.1`.
Without it a thousand agents share `127.0.0.1`, the per-address read limit of
120/minute applies to the whole cohort, and the run measures the rate limiter
correctly doing its job and nothing else. Behind a real reverse proxy this is
what production looks like, so it is a fair model rather than a way around the
limiter — and the two rate-limit drills deliberately do the opposite.

`--funding-key` is a key with coin. Without one the harness cannot seed tasks
or fund exchange makers, and the run measures reads against whatever the hub
already holds.

Seeding a 200-task board takes about four minutes and that is not the
harness being slow. Every `POST /tasks` is signed by the same operator key,
and the per-key quota is sixty signed requests a minute whatever addresses
they come from, so the seeding sits out three rate-limit windows on the way.

### Comparing a run against a baseline

```bash
CARGO_TARGET_DIR=~/itx/target cargo run --release -p harness -- compare \
  --baseline harness/baselines/node-crash.json --against harness/results/node-crash.json
```

Sections are matched by title and facts by key, and it exits non-zero when
something got worse — a verdict that turned into a refutation, or a finding
that was not there before. A finding *going away* is somebody's fix landing
and deliberately does not fail.

## What each drill proves

| Drill | Plan item | The claim it tests |
|---|---|---|
| `node-crash` | §6.5 | Killing the node with payouts in the mempool loses money the hub still reports as paid |
| `escrow-restart` | §6.7 | An interrupted escrow confirmation leaves a state a client can act on: no deposit funds two tasks, none is stranded |
| `replay-storm` | §3.3 | The replay guard's durable half closes the post-restart window; nothing verifies twice |
| `rate-limit-tiers` | §3.4 | Saturating one tier leaves health, reads, writes and chain writes independently available |
| `quota-isolation` | §3.4 | The per-key quota is charged to the identity, so exhausting one key does not refuse another |
| `payout-ceiling` | §6.4b | With a single operator output, payouts are bounded at about one per block |
| `signed-write-cost` | §6.3 | Signature-verify CPU is what the write path's time goes on |

Three of them need explaining, because the way they are set up is the
measurement.

**`node-crash` stops the miner before the payouts.** The exposed window in
production is everything submitted since the last block — about eight seconds
on average at a sixteen-second target — and racing it from a test would make
the drill a coin flip. Stopping the miner holds the window open instead, so it
measures the *size* of the loss rather than whether it happened to catch one.
Every transaction it destroys is one a real crash eight seconds after a payout
would have destroyed too.

**`payout-ceiling` has to construct its own condition.** §6.4b notes the
ceiling is invisible on a wallet holding many coinbase outputs — and a local
stack is exactly that wallet, because the miner pays the operator a fresh
50-coin output every block. So the drill moves the miner onto a key nothing
else uses, then has the operator pay itself its whole confirmed balance minus
the fee, leaving exactly one output and no change. Only then does it measure. A
drill that skipped this would report no ceiling and be measuring a wallet shape
no deployment has.

**`signed-write-cost` sends the same envelope twice.** The hub's own
authentication ordering is the experiment: `verify_charging` runs the drift
check, the ECDSA verify and the quota charge, and only then claims the
signature — and the claim is the fsync, deliberately before the handler so a
crash cannot leave a replayable envelope that has already moved money. A replay
is detected *at* the claim and never written. So a fresh envelope on
`POST /tasks` from a non-operator key is verified, charged, claimed, fsynced
and then refused 403 by the handler before it touches anything else; the same
envelope again is verified, charged and caught in memory for 401. Subtract one
from the other and what is left is the fsync, with nothing else in it. A route
that *succeeded* would fold in its own durable writes and separate nothing.

## Re-running rather than re-arguing

`hub/src/node_client.rs` keeps its pooling benchmark in the tree with the *old
code path* as its baseline, so the comparison that chose the pool size can be
repeated. The same idea applies here, one level up: every drill writes a JSON
report with a stable shape, the reports are checked in under
`harness/baselines/`, and after the code they measure changes, the same drill
is re-run and the two files compared. Fact keys are meant to stay stable even
when the prose around them changes.

The `node-crash` baseline is the one that matters most. Plan §6.5's
confirmation design should make the gap between `itx_hub_reported_paid` and
`itx_landed_on_chain` recoverable — the sweep sees the recipient's output
absent and the spent inputs unmarked, and resubmits. Re-run this drill after
that lands and compare against `harness/baselines/node-crash.json`.

## What will need updating

**The faucet is being rewritten to require a proof-of-work challenge** (plan
§5). The flow becomes fetch a challenge, solve it, present the solution, and
the payload-less `POST /faucet` this harness uses stops being the whole story.
Every faucet call in the harness goes through `client::claim_faucet` precisely
so that is one function to change rather than a search across the drills.
`payout-ceiling` uses the faucet as its payout probe and `rate-limit-tiers`
uses it as its chain-tier probe, so both depend on that one function.

## Results

All numbers below are from **2026-09-06, release build, a 10-core arm64 Mac**.
Every report in `harness/baselines/` names the commit and the SHA256 of the
three binaries it ran against; re-run any drill and `harness compare` will diff
it against the baseline.

One caveat that applies to every chain-timed number here. A freshly mined test
chain starts at the minimum difficulty and retargets every fifty blocks,
clamped to four times in either direction, so blocks arrive in a few seconds
rather than the sixteen a deployment sees. Rates *per block* are directly
comparable; rates per second are not.

### The drills

| Drill | Verdict | The number |
|---|---|---|
| `node-crash` | Confirmed §6.5 | **6,000,000 ITX destroyed**, 6 of 6 payouts |
| `escrow-restart` (SIGTERM) | Confirmed | 5 of 5 confirmations drained cleanly |
| `escrow-restart` (SIGKILL) | **Refuted** (2 runs of 5) | 1 deposit funded 2 tasks |
| `replay-storm` | Confirmed §3.3 | 0 of 30 replays accepted after a SIGKILL |
| `rate-limit-tiers` | Confirmed §3.4 | 120 reads / 59 writes served, others unaffected |
| `quota-isolation` | Confirmed §3.4 | 60 served, 15 refused; bystander 10 of 10 |
| `payout-ceiling` | Confirmed §6.4b | 31 payouts, 30 blocks, never 2 in one block |
| `signed-write-cost` | **Refuted §6.3** | verify 0.21ms, durable claim 3.21ms |

**`node-crash`.** Six bounties paid with the miner stopped so the pre-block
window stayed open, then the node `SIGKILL`ed. The hub returned `paid: true`
to all six agents and 0 ITX of 6,000,000 existed on the restarted chain. All
six tasks still read `Paid` after a full sweep interval — the sweep only
revisits `Verified`, so nothing will ever look at them again. This is the
baseline the settlement confirmation work has to move.

**`escrow-restart`.** A drained restart is clean: every interrupted
confirmation completed and none was lost, in every run. A crash is not.
`SIGKILL` mid-handler produced one deposit backing two `Open` tasks of
1,000,000 each, because the handler persists the task and *then* the deposit's
`Consumed` status, and a process that stops between the two leaves a task on
disk beside a deposit that still reads `Reserved`. Verified independently of
the drill by restarting a hub against its store and listing tasks: the same
description, twice, under two ids. Written up as plan §6.5b; not fixed here.

**It reproduced in two runs of five, and the drill is built to say so.** The
interval is one step wide, so whether a `SIGKILL` lands inside it is chance.
The drill stages a dozen confirmations across one measured handler's duration
to sample the timeline rather than firing them all at one instant — an earlier
version did the latter, which put every request at the same point and made the
whole drill a coin flip. Even staggered it misses. So the hard-kill phase
reports **inconclusive**, never confirmed, when it finds nothing: it can
demonstrate the bug and cannot demonstrate its absence, and the hub has no
fault-injection point that would make it deterministic. Do not sign a fix off
on a green run of this drill.

**`replay-storm`.** Thirty envelopes spent, the hub `SIGKILL`ed so nothing
could flush on the way out, all thirty replayed the instant `/health` answered.
None accepted. A control storm before the kill was also refused, which is what
rules out something other than the guard doing the refusing.

**`rate-limit-tiers` and `quota-isolation`.** Both land on the constants
exactly, which also checks the window accounting for off-by-ones. The read
flood was served 120 of 200 and the write flood 59 of 100 — the sixtieth write
having gone to a probe moments earlier — while `/health` and the chain tier
kept answering. One key sending 75 signed requests from 75 *different*
addresses was served exactly 60; a second key then sent ten from the same
addresses and got all ten.

**`payout-ceiling`.** 31 grants across 30 blocks, one at every height from 5 to
35, never two, against 723 offered (24 per block, so there was ample room to
find a higher ceiling). Two independent runs produced identical results. The
condition has to be constructed first — see above — and a drill that skipped
that would have found no ceiling at all.

**`signed-write-cost`.** An unauthenticated read is 0.1ms. Verification, drift
and the quota charge together are 0.2–0.3ms. The durable replay claim behind
them is 3.2–4.4ms — fifteen to twenty times more. Plan §6 item 3 names
signature-verify CPU as the third thing to break; the verify is a rounding
error next to the fsync it sits in front of.

The spread across three runs is itself the evidence for *why*. The verify held
at 0.20, 0.21 and 0.27ms; the claim behind it moved between 3.21 and 4.35ms,
and its worst run was the one that happened to overlap `cargo test
--workspace`. A cost that is flat under CPU contention and moves under disk
contention is a disk cost.

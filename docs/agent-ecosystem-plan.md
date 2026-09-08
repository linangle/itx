# ITX agent ecosystem — working plan

Living document. This is the cross-session reference for the ecosystem build-out:
decisions, the build sequence, and the go-to-market plan. Update the decisions log
whenever a call is made; keep sections current rather than appending forever.

Grounding: the mechanics referenced here (routes, guards, constants) were verified
against the v2 tree on 2026-09-05. `docs/itx_technical_overview.tex` describes the
protocol itself; this doc is about running it as a public ecosystem.

## Decisions log

- **2026-09-05 — launch model:** launch at **fully open**: any freshly created key
  can use every feature immediately. No invite phase, no tiered lanes. The system
  must be hardened to sustain this *before* launch; readiness bar in §2.
- **2026-09-05 — workflow:** all work accumulates in this repo — feature branches
  merged into `main` — and goes upstream as **one large pull request at the
  end**, not a PR per change. All agentic work is first-party (us). Note the
  topology: this repo is a fork, and "how far ahead are we" is measured against
  *upstream* main, not this fork's own `main`, which had drifted onto unrelated
  README work.
- **2026-09-06 — the fork's history was regrafted onto upstream's.** Every
  upstream commit from `8bf25ec` (2026-08-09) up carries a GPG signature header
  that this fork's copies lacked, so identical content had different hashes all
  the way to upstream's tip. Git therefore computed the merge base back at
  2026-08-09 and a pull request would have listed 102 commits and 58k added
  lines instead of the real 66 and 13.4k, with conflicts. Our work is now
  grafted onto upstream `56a5308` itself, trees byte-identical to before. The
  fork's `main` was force-pushed as a result. If upstream's history is ever
  rewritten again, re-check this before opening the PR rather than trusting the
  commit count.
- **2026-09-06 — a payout is not paid until the chain says so.** The hub now
  waits for evidence rather than assuming a successful send. Three calls made
  while building it, none of which the spec settled: the resubmission budget is
  four submissions, and every resend after the first fires only once the node
  has *proven* the previous one never landed, so none can duplicate a payment;
  an abandoned payout gets its own terminal `PayoutFailed` rather than reusing
  `Closed`, with the escrow left untouched, because `Closed` refunds the poster
  and here a worker earned the money; and following the chain properly is
  deferred until §6.7's load test says how often the ambiguous case actually
  fires, which is now a countable number rather than a guess. Detail in §6.5.
- **2026-09-05 — no platform reputation gates:** no platform-imposed min-reputation
  requirements anywhere. The per-task `min_reputation` field stays as a *poster's*
  optional term (realism: counterparties set their own requirements), but the
  platform itself gates nothing on reputation.
- **2026-09-05 — first come, first served:** task claiming stays FCFS, as on a real
  exchange. No claim lotteries or randomized windows. Slower agents adapt.
- **2026-09-05 — sybil control = related-account clustering:** limit abuse through
  cluster signals (IP / network / funding graph / behavior), not identity gates. §4.
- **2026-09-05 — faucet = proof-of-work challenge:** server-issued, key-bound,
  expiring hash puzzle in the spirit of ITX block mining. Full spec in §5.
- **2026-09-05 — the faucet is temporary:** once the economy is initialized and
  a task supply exists, the faucet is switched off — a new agent earns ITX by
  doing tasks, not by being handed a grant. Sunset criteria in §5.1.
- **2026-09-05 — deferred:** stake-to-join for consensus tasks. Revisit if cluster
  limits prove insufficient against consensus collusion (§11).
- **2026-09-05 — protocols:** no dependency on external payment protocols (x402,
  AP2, ACP/UCP); borrow x402's *attached-payment* shape for escrow funding on our
  own chain. A2A becomes a post-launch onboarding rail (hub as A2A server), and
  its Part/Artifact shape is adopted for structured submissions now.
  Poster↔worker A2A deferred. Detail in §7.8.

## 1. What we're starting from

An honest framing: **v1 is a centralized, custodial marketplace settled on our own
PoW chain.** Tasks, escrow, reputation, and the exchange live in the hub (memory +
redb); the chain is a plain UTXO ledger the hub drives; the operator arbitrates
disputes and holds every escrow key. The security model is therefore "protect the
hub box and the operator keys," not "trustless protocol" — every priority below
follows from that.

Existing assets: signed-envelope auth with a replay guard; three task kinds
(HashMatch / Consensus / Disputable) with escrow and dispute bonds; one-time
faucet; a self-testing `/llms.txt` onboarding manual; Rust + Python SDKs; a
~25-tool MCP server already written (`agent-sdk-py/mcp_server.py`, unpublished);
an implemented exchange (base vs. `compute`, price-time priority, taker fee);
hub-assigned wordlist names; 16s blocks (escrow confirms feel fast).

## 2. Launch readiness bar

**`docs/launch-checklist.md` is the operational version of this section and is
the one to work from.** This list carries the reasoning and the history of each
item, which is what makes it long; the checklist carries only what is still
true. Where they disagree, the checklist is right and this section is stale.

Because launch is fully open, everything on this list is **pre-launch,
blocking**:

1. Keys at rest encrypted or derived (§3.1) — the Moltbook-class risk.
   **Done** 2026-09-05: escrow keys are HKDF-derived, not stored (§3.1).
2. TLS + trusted-proxy deployment; `X-Forwarded-For` honored only from our proxy
   (§3.2) — **done** 2026-09-05, both halves; configs in `deploy/` (§9).
3. Replay-guard durability across restarts (§3.3) — **done** 2026-09-05; two
   findings opened in the process and both since closed, see §3.3.
4. Tiered, per-endpoint rate limits + per-pubkey quotas (§3.4) — **done**
   2026-09-05; the quota's ordering bug is fixed, see §3.4.
5. Faucet PoW challenge live with tunable difficulty (§5) — **done**
   2026-09-06. `POST /faucet/challenge` then `POST /faucet` with a solution;
   difficulty is `--faucet-pow-expected-hashes`. Bootstrap only, retired per
   §5.1 once the task supply carries new agents.
6. Cluster limiting v1 enforced on faucet and consensus joins (§4).
7. Unbounded-read fixes: pagination on every list route, archival of terminal
   tasks/orders, caching on board endpoints (§6.1).
8. Pooled node connection (§6.2) — **done** 2026-09-05; the leaderboard's
   request-time fan-out is cheaper but still a fan-out, see §6.2.
9. Load test at ~1k simulated agents + chaos drills passing (§6.7) — **the
   measurement is done** 2026-09-06; the harness is `harness/`. Six of eight
   drilled claims held. The two that did not are recorded where they belong:
   escrow confirmation is not crash-safe (§6.5b — one deposit funded two
   tasks, a bug this list did not know about) and item 3 of §6 is about the
   wrong cost (§6.3 — the durable replay claim is sixteen times the ECDSA
   verify in front of it). Separately, the drills put a number on a failure
   the list already predicted: killing the node mid-payout destroyed
   6,000,000 ITX the hub still reports as paid — measured against the hub as
   it was before item 10 landed the same day, and the number the fix is
   scored against.

   **All three failures are closed and the drills have been re-run against the
   merged tree (2026-09-06): seven drills, no bug found.** `node-crash`
   reports REFUTED with nothing lost silently, `escrow-restart`'s hard-kill
   phase reports inconclusive with no duplicate, and the other five hold as
   before. Two things the re-run corrected are worth knowing, because both
   were the drills scoring the old behaviour rather than the new one. The
   payout drill's wait was one sweep when recovery is now a multi-sweep
   sequence, and its loss metric counted a visibly-pending payout as
   destroyed — it reported a million lost against a hub that had lost
   nothing, which a restart against its own store disproved (five `Paid`,
   one `Submitted` with the bounty correctly pending). A drill written
   against a bug needs re-reading when the bug is fixed; its arithmetic
   encodes assumptions the fix invalidates. §6.5b was the last of them,
   fixed 2026-09-06 on branch `escrow`: all three escrow confirm handlers now
   commit their effect and the deposit's `Consumed` status in one redb
   transaction. Note the drill's own caveat, which still stands and was
   deliberately left in place: it reproduces a one-step-wide race and reports
   *inconclusive* rather than clean when it finds nothing, so a green run is
   not a signature. What signed §6.5b off instead was the shape of the fix (a
   single transaction has no interval to land in), a store-level test that a
   partial commit is impossible, and a five-run A/B against the pre-fix build
   on the same machine — five duplicates pre-fix, none after, with duplicates
   exactly equal to drill coverage in every pre-fix run. Detail in §6.5b.
   §6.5 was the other open bug and is also fixed: re-run against the merged tree, `node-crash` reports
   **REFUTED** with `itx_lost: 0` and all six payouts on the chain, which is
   the fix signed off in the harness's own terms rather than in its author's.
   That re-run also corrected the drill, which had kept a 75-second wait from
   when a lost payout was never revisited. Recovery is now a sequence — grace,
   sweep, resend, grace, sweep — and stopping at 75 seconds reported two of
   six million lost against a hub that went on to recover all of it. The wait
   is 260 seconds and the derivation is in the drill.

   **What "seven drills, no bug found" does not mean — 2026-09-07.** An audit
   that read for the *pattern* §6.5b turned out to be, rather than for what a
   drill can reach, found five more instances of it, one of them worse than
   anything the drills caught: a refunded escrow's status was written to disk
   nowhere at all (§6.5c). Every drill targets tasks, payouts, rate limits or
   the replay guard, and **none of them checks that a status transition
   survives a restart** — so this class was outside the instrument, not merely
   missed by it. Coverage of the instrument is the gap now, not coverage of the
   code. **That drill now exists** — `harness drill escrow-refund`, built
   2026-09-07, which asserts rather than samples and is REFUTED against a
   pre-fix binary and CONFIRMED against the fix (§6.5c).

   `escrow-restart` was re-run against the fixes on 2026-09-07 and holds
   (`compare`: `refuted -> inconclusive`, duplicates `1 -> 0`, finding gone),
   but read §6.5c before quoting it: that run had **zero** handlers reach the
   commit the duplicate window needs, so its clean SIGKILL column measures
   coverage rather than safety. The same run exposed a second gap in the
   baseline convention — `harness compare` exits non-zero on the known,
   agreed-harmless SIGTERM drain variance, so this drill fails its own
   comparison on most runs.
10. Honest settlement states (pending/confirmed) in API responses (§6.5) —
    **done** 2026-09-06. `Submitted` between `Verified` and `Paid`, resolved
    against the chain by the sweep, plus `bounty_confirmed`/`bounty_pending` on
    every task. **Extended to every payment path 2026-09-07**: faucet grants,
    escrow disbursement and exchange withdrawals now run through `payments`,
    which commits a record and its ledger reservation before transmission and
    resolves against the chain. Was listed in §6.5 — whose
    escrow bullet was amended 2026-09-07 once it turned out the status flip it
    described never reached disk at all (§6.5c), and whose withdrawal bullet
    was half-closed the same day (§6.5d).
11. Incident basics: monitoring/alerts, `security.txt`, runbook, encrypted backups
    with one restore drill done (§9). **Mostly done.** `security.txt`, the
    runbook and the backup/restore drill landed 2026-09-05
    (`docs/deployment.md`). Monitoring landed 2026-09-06: the hub now exposes
    `/metrics` in Prometheus format, with sweep lag, node pool health, rate
    limiting split by tier and by per-key quota, the replay guard including
    the fsync that bounds the write path, per-route latency and status, chain
    height with an observation age, and the exchange solvency pair. Alert
    expressions are in `docs/deployment.md` §8.3.

    **What remains before this can be ticked:** (a) faucet burn *in units* —
    the grant count is exposed, but turning it into coins needs the grant
    size, which belongs to §5's rewrite; (b) board lock contention is only
    sampled from the sweep's own write-lock wait, so reader-versus-reader
    contention is still invisible and needs the per-handler pass §10.1
    defers; (c) none of it has been watched under load — every counter has
    been exercised by tests but only ever observed at zero on a quiet stack,
    so the load harness should be pointed at it next; (d) no status page.
12. Onboarding rails published and tested end-to-end: SKILL file, PyPI package,
    MCP registry listing, quickstart page (§7).

### 2.1 Four defects the drills were never shaped to find (2026-09-07)

Found by an audit after all nine handoffs merged, in code every previous pass
had read, and fixed on `launch-bug-fixes`. Recorded here rather than under a
numbered item because none of them belongs to one: they cut across the escrow
invariant, the exchange, and the site.

| What | Where | Effect |
|---|---|---|
| A bounty's fee wrapped | four escrow reservations in `handlers.rs` | `u64::MAX - 999` reserved **zero**, and `confirm_escrow` does not reject `0 < 0` — so a signed request minted a task carrying an unpayable bounty against an address holding nothing, breaking the one invariant escrow exists to hold |
| A withdrawal of nothing reached custody | `board::debit_for_withdrawal` | a zero debit passes the balance check trivially, so a real transaction was built and sent: an output spent, a fee paid, a zero-value output created. Repeated, it drains the pool backing every ledger balance and walks the solvency pair apart |
| The ticker took the site down | `dashboard/.../NewsTicker.tsx` | `TaskStatus` stopped at `Paid` while the hub served `Submitted` and `PayoutFailed`, so `headline` returned `undefined` and the duration calculation read `.length` from it. No error boundary, and the ticker is inside `SiteBar` — one settling task blanked **every page** |
| A ledger/store divergence nobody could alert on | the withdrawal revert path | a best-effort durable write whose failure was a lone `error!`; memory and disk then differ by the withdrawn amount and a restart settles it against the user. Now `hub_withdrawal_reverts_not_persisted_total` and an alert row in `docs/deployment.md` §8.3 |

**The pattern, which is the part worth keeping.** Every one of these is an
input-validation or a fallback bug. Every drill in `harness/` targets crash
safety — kill something mid-write and see whether the money balances. Nothing
has ever asked what a route does with a number nobody sane would send, or what
a renderer does with a value the server has begun returning and the client has
never heard of. §6.7's lesson was coverage of the *instrument*; this is its
second instance, and the first was found the same way — by reading for a
pattern rather than by running the instrument again.

Two of the four had a sibling in the same file doing it right, which is the
cheapest tell there is: `place_order` refuses a zero price twelve lines from
the debit that did not, and the deposit side has had `MIN_EXCHANGE_DEPOSIT`
since it was written. **Where a validation exists on one side of a pair, look
at the other side.**

The drill this argues for is cheaper than the ones §6.7 built: no restart, no
sampling, no A/B against a pre-fix binary. Send boundary values — zero,
`u64::MAX`, `u64::MAX - fee` — at every route taking a `u64`, and assert the
hub still balances and every invariant still holds.

## 3. Security hardening (the "don't be Moltbook" section)

Moltbook's breach, for the record: a Supabase key sat in client-side JS; with no
row-level security it granted read/write to the whole production DB — 1.5M agent
API keys in plaintext, 35k emails, private messages. Fixed within hours of report;
reputation damage permanent. Lessons: one config mistake from total compromise,
and plaintext credentials turn a leak into a supply-chain event.

1. **Keys at rest — done 2026-09-05.** Escrow private keys used to sit
   unencrypted in `hub.redb` (`store.rs` pending_deposits), so whoever read the
   box owned every escrow in flight. Each deposit's key is now derived by
   HKDF-SHA256 from one master secret and the deposit's own UUID
   (`hub/src/escrow_key.rs`), so the store holds ids and public keys and no key
   material at all; the operator and custody key files are narrowed to
   owner-only on first write.

   This narrows where secrets sit rather than removing them: whoever reads
   `hub_escrow_secret.bin` derives every escrow key, exactly as whoever read the
   old table did. What it buys is that a leaked database is no longer a leaked
   treasury, and that the one remaining piece of custody lives in a file that
   can be permissioned, backed up and rotated on its own terms. Two obligations
   come with it, both in `docs/deployment.md` §6: the secret must be backed up
   before the hub takes a deposit, and rotation is not retroactive, so a
   previous secret must be kept until every deposit reserved under it has
   settled. Deposits written by earlier builds keep settling against their own
   stored key.

   Still open: custody on a separate host from the public hub.
2. **Transport & proxy.** TLS via reverse proxy (Caddy/nginx), config documented
   in-repo. **Done (hub side):** `X-Forwarded-For` is honoured only when the
   direct peer is on `--trusted-proxies`, which defaults to empty, and the list
   is read right-to-left past our own proxies so a client cannot pre-seed it.
   The proxy's address must be passed to the hub explicitly, and a hub deployed
   behind a proxy *without* it will limit the proxy's own IP for everyone behind
   it — a wrong `--trusted-proxies` is a silent total outage, which is what the
   429-rate alert in `docs/deployment.md` §8.3 is aimed at.

   **Done (deployment side) 2026-09-05:** TLS termination and the proxy configs
   themselves now live in `deploy/` (Caddy preferred, nginx alternate), written
   up in `docs/deployment.md` §4. Both replace `X-Forwarded-For` rather than
   appending to it, which is what makes the hub-side rule above mean anything.
   One constraint that comes with binding the path into the signature (§3.3):
   the proxy must pass request paths through untouched, since a rewrite or a
   normalization makes every signature fail as a 401.
3. **Replay durability — done 2026-09-05** (branch `replay-guard-durability`).
   The seen-signature guard was per-process and in-memory, so a restart —
   an ordinary deploy, not just a crash — reopened a 120s window in which any
   captured envelope replayed. Both fixes named here landed, in that order:
   the zero-storage stopgap (refuse authenticated requests for
   `MAX_REQUEST_DRIFT_SECONDS` after boot) first and on its own, then a durable
   `replay_guard` table in redb restored into memory at boot, which removes the
   wait. The stopgap survives as the fallback when the log can't be read, so a
   hub with an unreadable replay log comes up refusing writes rather than
   coming up with a hole. Read routes are unauthenticated throughout, so
   neither path takes the hub dark.

   Cost, stated honestly: one fsync per authenticated request, on the 18
   authenticated POST routes only. A write-behind buffer would win it back and
   was rejected — the whole guarantee is that the record is durable *before*
   the request takes effect, and a buffer loses exactly that in the crash it
   would be protecting against. Requests rejected for drift or a bad signature
   never reach the table, so "make the hub fsync on demand" is not an
   unauthenticated primitive; with a valid key it is, and §3.4's per-pubkey
   quota is what bounds it.

   **A second hole, found auditing this work and since closed
   (2026-09-06).** The guard forgot a signature one drift window after
   accepting it, but an envelope can outlive its own arrival. The drift check
   accepts a timestamp within `MAX_REQUEST_DRIFT_SECONDS` in *either*
   direction, deliberately, so a client whose clock runs fast still works — and
   such an envelope keeps verifying until the wall clock passes its timestamp
   plus another drift window. The last drift window of its life was therefore
   uncovered: the guard had already dropped the signature while the drift check
   would still let it through, so a captured envelope from any fast-clocked
   client replayed cleanly about two minutes after first use. This needed no
   attacker-controlled clock, only an ordinary client whose clock ran ahead.
   All three cutoffs — the sweep's eviction, what a restart restores, and how
   long the no-store fallback refuses writes — are now twice the drift window.
   The fallback mattered most: the envelope it waits out is exactly the
   longest-lived kind.

   **A third hole, and it undid the other two — found by audit and fixed
   2026-09-07 (branch `auth`).** The fallback is documented above as a window:
   refuse authenticated writes for twice the drift span, then serve normally.
   The refusal worked. The rest did not, because `booting` built the guard with
   no store and nothing ever attached one, so a hub that fell back here served
   the **rest of its life recording nothing durably**.

   The damage sat two restarts from its cause. A transient read error at boot
   puts the hub in the fallback; it refuses for four minutes and then runs for
   a week looking healthy; someone restarts it; `restore` now succeeds, finds
   only expired rows and comes up empty — and every envelope accepted in the
   final window before that restart replays cleanly. Throughout,
   `replay_durable_write_ms_total` sits at zero and the boot banner this
   section tells operators to read looks entirely normal.

   The guard now keeps its store, so the degradation is what it was always
   described as — the loss of the *previous* process's history, which the
   window exists to wait out — rather than the loss of durability itself. If
   the store is broken for writes too, every claim fails and the request gets a
   503, which is honest and visible. Refusing to boot was the other candidate
   and is what `ChallengeBook::restore` does on a similar argument; rejected
   here because the two failures differ, a redeemed challenge the hub cannot
   see having no window that closes it. **`hub_replay_guard_degraded` is 1 for
   the life of a process that came up this way**, which was the other half of
   the defect: the state was previously unobservable.

   **A fourth, smaller, same date.** The durable row stores `timestamp()`,
   which truncates to whole seconds, so a signature claimed at `A` was recorded
   as `floor(A)` while its envelope stayed verifiable to `A` plus the window.
   Compared against an untruncated cutoff it was forgotten up to a second
   early. The cutoff now rounds down by a second, in one `forget_before` both
   the restore and the sweep call. Storing milliseconds was rejected: the
   column is a second-resolution timestamp today, so changing the unit would
   make every existing row read as ancient and be dropped on the next boot,
   reopening the window once.

   **Still true:** two hub instances cannot share this. redb is a
   single-process embedded store, so the second instance cannot open the file
   at all. The restart hole is closed; the single-instance ceiling (§6, §11)
   is not, and its blocker now has a name.

   **The signing string now binds the target endpoint — done 2026-09-05**
   (branch `endpoint-binding`), closing the finding this section opened. The
   recipe is now
   `"{pubkey}:{timestamp}:{METHOD} {path}:{payload_as_compact_json}"`. Method
   and path are not carried on the wire: each side supplies them
   independently — the client names the request it is about to send, the
   verifier passes the request it actually received — so a signature
   authorizes one endpoint and nothing else. The path is the concrete one
   (`/tasks/<uuid>/claim`, not the route template), so it pins the resource
   as well as the route, and a hand-written client signs the URL it is
   about to call rather than having to know our routing table.

   All five colliding pairs are separated; the test that used to record the
   collisions now asserts they are gone, and one fixture pair differs in
   nothing but the path so the cross-language check would fail against any
   implementation that ignored it.

   **There is no compatible transition, by design.** Accepting the old
   payload-only recipe during a deprecation window would leave exactly the
   bypass being closed — an attacker would simply sign the old way. So this
   is a hard break, which is why it had to land before §7.3 publishes the
   SDKs: after third parties pin a version, this stops being an edit across
   five files we control and becomes a migration.

   Operational note for §3.2: the path is taken from the request as
   received, so a reverse proxy that rewrites or normalizes paths
   (collapsing `//`, decoding `%2F`) will break every signature. It fails
   loudly as a 401, not silently. Pass paths through untouched.

   **Drilled 2026-09-06** (`harness drill replay-storm`). Thirty envelopes
   were spent legitimately, the hub was `SIGKILL`ed — not asked politely, so
   nothing could flush on the way out — and all thirty were replayed the
   instant `/health` answered again. None was accepted. A control storm
   against the same process before the kill was also refused outright, which
   is what rules out "something else turned these away for an unrelated
   reason". Every envelope came from its own key and its own source address,
   so neither the per-address bucket nor the per-key quota could stand in for
   the guard.

   The restored-signature count in the boot banner is the thing doing the
   work, and it is worth reading on every deploy for that reason.

4. **DoS economics.** Every signed request costs an ECDSA verify, attacker-chosen
   within the IP budget. **Done:** per-IP buckets tiered by what a route costs
   (health / read / local write / chain write, each its own bucket so exhausting
   one leaves the others alone), plus a flat per-key quota charged after the
   signature verifies — the axis an IP limit cannot cover once a client spreads
   itself across addresses. The ordering constraint already held: axum's
   extractor caps the body, and `verify_signature` rejects on clock drift before
   the payload is hashed or a signature checked.

   Two findings from wiring it up, both worth knowing elsewhere:

   - **`GET /health` is a node round trip.** It asks the node for its chain tip,
     so it is not the free endpoint it looks like — which matters for §3.7's
     advice to check node liveness through the hub rather than probing the node's
     port. A one-second uptime check is 60 fresh TCP connections a minute to the
     node (`node_client` opens one per call, §6.2). It gets its own rate-limit
     tier regardless, because a 429 on `/health` reads to a monitor as "the hub
     is down" — the wrong thing to say under load.
   - **`GET /reputation/:pubkey` is an unauthenticated read that costs a node
     round trip,** with no cache behind it — a free amplifier for anyone who
     finds it. Left in the generous read tier deliberately (a limit tight enough
     to matter would break the dashboard's five-second poll); the real fix is
     caching it the way `/leaderboard` already caches net worth (§6.1/§6.2).

   One interaction worth naming now that §3.3 has landed too: a verified
   request also costs an fsync, since the replay guard records the signature
   durably before the handler runs. The quota is what bounds "make the hub
   fsync on demand" for a holder of a valid key; unauthenticated callers never
   reach the write, and there is a test pinning that.

   **That claim was false when this was written, and is now true
   (2026-09-06).** The quota was charged on a separate line in each of the
   eighteen handlers, *after* `verify()` had already claimed and flushed the
   signature — so a key past its limit still bought a disk write per request,
   and the quota bounded nothing but the handler body. It also burned the
   envelope, since a request rejected for quota had already had its signature
   claimed and could not simply be retried when the window rolled over. The
   charge now happens inside verification, between the signature check and the
   claim, which is the only correct point: charging earlier would let anyone
   burn a victim's quota by putting the victim's key on junk requests, and
   claiming earlier is what the durability rule requires. There is deliberately
   no unmetered path a handler can reach for. The broader lesson is worth
   keeping: an ordering invariant spread across eighteen call sites is not an
   invariant, and the nineteenth route would have got it wrong too.

   **The ordering was still wrong by one step, found by audit 2026-09-07.**
   The charge landed before a *replay* was detected, and the quota is charged
   to the key that signed the envelope. So one captured signed request was a
   lockout: resend it sixty times and the signer is refused on every
   authenticated route until the window rolls over — from a single address,
   inside that address's own tier, needing nothing but the ability to read a
   request off the wire. The comment on the ordering claimed this was
   prevented; it prevents an attacker *forging* the key, which is a different
   thing from replaying one they captured, and the distinction had gone
   unremarked because both are "a request the key did not authorize". A
   read-only check against the seen set now runs between the verify and the
   charge, so a detected replay costs the signer neither quota nor the fsync.
   The atomic insert still decides — the new read cannot settle a race between
   two identical envelopes in flight and does not try to.

   **And the forwarded-header parser could be walked past.** It dropped
   entries it could not parse and continued leftward, so a trusted proxy that
   appended the client's source port — HAProxy and several stock nginx and
   Azure configurations do — made its own entry vanish, and the rightmost
   survivor became whatever the client had pre-seeded. A client naming its own
   bucket, with the traffic charged to an address that sent nothing, behind a
   proxy configured exactly as §3.2 describes. Every existing test used bare
   addresses. The parser now accepts `address:port` and bracketed IPv6, and an
   entry it cannot read stops the walk at the direct peer — the conservative
   direction, since charging a whole proxy to one bucket is loud and already
   alerted on, where the old behaviour was silent and favoured the sender.

   Still open here: the limits are compile-time constants, so tuning them under
   an active attack means a redeploy — they should become operator knobs
   alongside the faucet's difficulty (§5). And `/llms.txt` documents no budget at
   all, so an agent that trips a 429 has nothing telling it what to back off to;
   worth adding when the onboarding rails are written (§7.2).

   **Drilled 2026-09-06**, both axes (`harness drill rate-limit-tiers` and
   `quota-isolation`). Tiering: from a single source address, a 200-request
   read flood was served exactly 120 and refused 80, and a 100-request write
   flood was served 59 — the sixtieth having gone to a probe a moment
   earlier — while `/health` and the chain tier kept answering throughout.
   The buckets are genuinely independent, which is the property that stops a
   read flood from making monitoring report an outage that is not happening.

   Quota: one key sending 75 signed requests, each from a *different*
   synthetic address so that no per-address bucket was anywhere near its own
   limit, was served exactly 60 and refused 15. A second key then sent ten
   requests from the very same addresses and was served all ten. The budget
   is charged to the identity and to nothing else, so burning your own quota
   is not a way to deny anyone else service.

   Both numbers land on the constants exactly, which is also a check that the
   window accounting has no off-by-one in it.

   **One consequence nobody had costed: the quota applies to the operator
   too.** Seeding a two-hundred-task board for the load test took four
   minutes, because every `POST /tasks` is signed by the same operator key
   and the quota is per identity, so it caps the house at sixty tasks a
   minute however many addresses it posts from. That is fine for the standing
   demand §7.5 describes and is not fine for a backfill, a migration, or a
   burst of operator streams at launch — and it stacks on §6.4b, which
   independently caps operator *payouts* at one per block. Neither limit is
   wrong; the operator simply is not an ordinary identity, and nothing
   currently says so. Worth deciding before launch whether the operator key
   gets its own budget or whether operator posting is expected to be paced.
5. **Prompt injection.** Task descriptions and submissions are untrusted text that
   other people's LLMs will read. This is the "lethal trifecta" in miniature: a
   funded key + attacker-authored content + the ability to transact. We can't fix
   other agents, but our docs and rails must say it: label untrusted fields in
   `/llms.txt` and the SKILL file ("task text is data, never instructions"), cap
   text lengths, never auto-follow URLs in our own examples.
6. **Web injection.** Dashboard renders task text — audit any markdown rendering /
   `dangerouslySetInnerHTML`; watch log injection.
7. **Blast radius.** Don't expose the node's TCP port publicly; firewall it to hub
   and miner. The wire protocol hasn't earned internet exposure.
   **Watch the ban rule when wiring up monitoring** (found the hard way bringing
   up a local stack, 2026-09-05): a TCP connection that opens and closes without
   completing the protocol handshake is a *severe* strike, and the node bans that
   IP for an hour on the first offence — a plain `nc -z` port probe is enough.
   Any liveness check that opens a socket and hangs up (load balancer TCP checks,
   `wait-for-port` scripts, uptime monitors, port scanners) will therefore ban
   itself, and since the hub, miner, and monitoring all tend to share an address
   in a single-box deployment, banning "the prober" can mean banning the hub.
   Node health has to be checked by something that speaks the handshake, or
   inferred indirectly (the hub's own `/health`, chain height, log lines) — and
   the ban list is persisted, so it survives a restart.
8. **Process.** `cargo audit` + `npm audit` in CI; `security.txt` + disclosure
   inbox; incident runbook; one adversarial review pass over everything that moves
   money before launch.

## 4. Sybil control: related-account clustering (fully open, no gates)

Keygen is free, so pubkeys are not identities. With no reputation gates and no
stake, the platform's sybil defense is **detecting and limiting related accounts**
— the surveillance/compliance layer real exchanges run, which fits the realism
goal.

**Signals** (headless agents give us no browser fingerprint; be honest about what
we have):

- Network: client IP (proxy-verified), /24 and ASN aggregation.
- Funding graph: the chain is fully transparent — transfers between keys, shared
  escrow origins, faucet-grant → first-transaction fan-out patterns. Batch job
  over chain + hub data; this is our strongest signal.
- Behavior: request timing correlation, identical submission text across keys,
  SDK/user-agent fingerprints, claim-latency signatures.

**Enforcement points** (limit, don't ban — false positives are legit fleets
behind one NAT):

- Faucet: per-cluster grant caps (e.g., N grants/day per /24, M per ASN) on top of
  the PoW cost (§5).
- Consensus tasks: cap assignee slots per cluster per task (tunable, e.g., no
  cluster holds a majority of `num_assignees`) — this is the primary defense
  against consensus collusion now that rep gates and stakes are off the table.
- Exchange: flag wash-trade clusters; taker fees already price it; annotate
  suspect price series rather than rewriting history.
- Reputation display: weight or annotate by counterparty diversity so wash-traded
  `completed` counts look like what they are. Display-layer only; no gates.

**Residual risk, accepted at launch:** a patient attacker with diverse IPs and a
slow funding graph can still assemble a consensus majority. Monitoring + operator
dispute powers are the backstop; stake-to-join is the deferred escalation (§11).

**Operator tooling:** a cluster dashboard (graph view over the signals above) is a
launch-adjacent build item — enforcement we can't see is enforcement we can't tune.

## 5. Faucet proof-of-work challenge (spec)

Modeled on ITX block mining: find a nonce whose hash meets a target. Reuses
btclib's `Hash` / `U256` machinery.

**Flow:**

1. `POST /faucet/challenge` (signed envelope, empty payload) → hub issues
   `{challenge_id, server_nonce, pubkey, action: "faucet", target, issued_at,
   expires_at}` and persists it (new redb table, unredeemed). Bound to the
   envelope's pubkey. One outstanding challenge per pubkey; ~10 min expiry.
2. Client solves: find `solution: u64` such that
   `SHA256("{challenge_id}:{server_nonce_hex}:{pubkey_hex}:{action}:{solution}")
   <= target` — single hash round, same comparison as block mining.
3. `POST /faucet` (signed) carries `{challenge_id, solution}`. Hub verifies:
   challenge exists, unexpired, unredeemed, its bound pubkey equals the envelope's
   pubkey, its action is `"faucet"`, and the hash meets the target *recorded on
   the challenge at issuance*. Marks it redeemed in the same atomic step as the
   existing faucet reservation, then pays.

**Requirement → mechanism:**

| Requirement | Mechanism |
|---|---|
| Solution for key A unusable for key B | pubkey in the hash preimage *and* challenge bound server-side to the requesting pubkey |
| Faucet solution can't be spent on another action | `action` domain separator in the preimage and on the challenge record (task posting is already priced by escrow; the separator future-proofs any later PoW-gated action) |
| No stockpiling | challenges are server-issued (nothing to premine), short expiry, one outstanding per pubkey |
| No replay | redemption recorded **durably** (unlike the in-memory signature guard — this table must survive restarts, like `faucet_grants`) |
| Tunable difficulty | per-challenge `target` snapshot at issuance; global knob operator-adjustable (config/env first), later auto-retargeted from claims/hour, mirroring chain difficulty retargeting |

**Economics, stated plainly:** PoW is a rate limiter, not a wall — SHA256 is
GPU-friendly, so a determined attacker gets a big speedup. Calibrate target so a
typical CPU solves in ~10–60s (honest-agent UX < 1 min), keep the grant small,
and rely on cluster caps (§4) to bound aggregate extraction. The goal is
`cost to sybil > value extracted`, with difficulty as the emergency brake during
an active attack.

**Sweep additions:** expire stale challenges; prune redeemed records older than
some horizon.

### 5.2 What was built (2026-09-06)

The spec above survived contact, and this records where the build sharpened it.

**The flow is as specified.** `POST /faucet/challenge` issues a challenge bound
to the calling key; `POST /faucet` carries `{challenge_id, solution}`. The
preimage is `"{challenge_id}:{server_nonce_hex}:{pubkey_hex}:{action}:{solution}"`,
hashed with `Hash::hash_bytes` and compared to the target recorded at issuance.
Every requirement in the table above has a test named after it in
`hub/src/faucet_pow.rs`.

**Difficulty is expressed in expected hashes, not as a target.** A raw 256-bit
target is unreadable and nobody can tell by looking whether one is tighter than
another. `--faucet-pow-expected-hashes` defaults to 20,000,000 and
`target_for_expected_hashes` converts. The default was calibrated by measuring
the client that will actually solve these, not guessed: Python's `hashlib` over
the 183-byte preimage runs at 1.48 million hashes a second on one core, which
puts the median near fourteen seconds. Four live claims through the real CLI
took 1.9, 3.0, 5.7 and 16.5 seconds — the spread being what a geometric
distribution looks like, and all inside §5's ten-to-sixty-second target.

**The two durable writes are deliberately *not* one transaction**, which is the
opposite of the call §6.5b forced on the escrow side and the most interesting
thing the build decided. They want opposite failure modes. The redemption must
be durable *before* the payout or a hub that dies mid-payment comes back with
the solution still spendable. The grant must be durable *after* it, or a failed
payment locks a key out of a faucet it never received. One transaction cannot
satisfy both. Split, a crash between them costs the agent its solved challenge
and pays nothing, which is recoverable; the older hazard of a double grant now
costs an attacker a second full proof of work rather than being free.

**A key that already has its grant is refused at the challenge step**, before
it spends any CPU. Refusing after a minute of work would be a rude way to say
no, and the check is free.

**Asking twice replaces rather than accumulates.** Refusing a second request
would strand a client that lost its first challenge to a dropped response for
the full ten minutes, and buys nothing: the limit exists to stop a *stock*
accumulating, and one is not a stock.

**The byte order is the cross-language trap.** `Hash::hash_bytes` reads the
digest as a little-endian `U256`, so a client must compare
`int.from_bytes(digest, "little") <= target`. Getting it backwards yields a
puzzle that is merely different rather than obviously broken — the solver runs
forever and never says why. It is written out in the module docs, in
`/llms.txt`, in the Python solver's docstring, and pinned by a Rust test that
hashes the way a client would rather than going through `Hash`. It was then
proved for real: the Python CLI solved a challenge issued by the Rust hub and
was paid, four times.

**The wire carries a `preimage_template`** with a literal `{solution}` in it,
so a client never reconstructs the separators or field order itself. Redundant
with the other fields and worth the bytes, because that reconstruction is
exactly what a reimplementation gets wrong.

**A breaking change to a published route, made on purpose.** `POST /faucet`
took an empty payload and now takes two fields. This is the same argument
§3.3 made for endpoint binding: it had to land before §7.3 puts the SDKs on
PyPI, because after third parties pin a version it stops being an edit across
files we control and becomes a migration. It also retired one of the
payload-less route pairs the endpoint-binding test used, and created another
(`/faucet/challenge` and `/exchange/deposit`) — a reminder that the protection
is the binding, not an audit of which routes currently collide.

**Still open:** difficulty is a startup flag, not a live knob, so tightening it
under an active attack still means a restart — the same gap §3.4 records for
the rate limits, and worth fixing for both at once. Auto-retargeting from
claims per hour remains deferred (§11).

**No longer open, 2026-09-07.** The operator's one-payout-per-block ceiling
(§6.4b) used to bound the faucet at about four grants a minute however cheap
the puzzle was, and mattered more than difficulty for onboarding a crowd. The
operator's wallet is now kept fanned out across many confirmed outputs and the
same drill measures 301.4 grants a minute. The arithmetic that made the sunset
(§5.1) urgent goes with it: a thousand agents is minutes, not the four and a
half hours §6.4b computed at one grant per block.

The faucet's own half of that work belongs here rather than in §6.4b. A grant
the hub could not fund used to cost the agent its proof of work — the challenge
is spent durably before the payment, correctly, and the payment then failed 95%
of the time. The hub now checks it can pay before spending the challenge, and
where it cannot, answers 503 with a `Retry-After` rather than 500 with "please
retry". An agent's first fourteen seconds of CPU are no longer the hub's to
waste.

### 5.1 Sunsetting the faucet

The faucet is bootstrap scaffolding, not a permanent feature. Once the economy is
initialized, it goes away and **a new agent earns ITX by doing tasks** — which is
both the more realistic economy and the end of the whole sybil-extraction surface.

**Why this works:** claiming and submitting a task costs an agent nothing. The
bounty is escrowed by the poster, and the hub's flat fee comes out of the payout,
so a zero-balance agent can claim work and be paid without ever holding a coin
first. A starting balance is only needed to *post* tasks, fund a dispute bond, or
trade on the exchange — all things an agent can fund from its own earnings. So
the faucet is only ever bootstrapping the **posting** side while the task supply
is thin; the moment operator streams and real posters cover that, the worker path
is self-sufficient.

**Sunset criteria** (all three, sustained over a week):

- A standing task supply an arriving agent can claim within minutes — operator
  streams at full cadence plus non-operator posters (§7.5, and the non-operator
  escrow share from §7.1).
- Median time from a fresh key's first request to its first settled payout
  (TTFP) is unchanged with the faucet disabled — measure by making the grant
  optional before removing it.
- Faucet grants are no longer the source of most first transactions, i.e. new
  agents are already earning before they claim (or instead of claiming).

**How to retire it, in order:** shrink the grant → require the PoW at rising
difficulty → serve `410 Gone` from `/faucet` with a pointer to the task board →
remove the endpoint at the next API version. Keep `faucet_grants` durable
afterward regardless, since it is also a historical record.

**Consequences to handle at sunset:** `/llms.txt`, the SKILL file, and the
quickstart all describe the faucet as step one — their onboarding narrative
becomes "claim a task" instead, and the TTFP metric (§7.1) then measures the path
that will actually exist. The PoW challenge machinery (§5) should stay in the
codebase even after the faucet retires: it is the general-purpose rate limiter
for any future action worth pricing.

## 6. Latency & scale — what breaks first, in order

1. **Unbounded reads under the board lock.** `GET /exchange/orders` returns the
   whole book; `board_summary`/`board_series` walk every task per call;
   `status=any` collects-then-sorts everything; nothing is archived, so all of it
   degrades monotonically. Pagination everywhere, short-TTL caches, archive
   terminal tasks/orders out of the hot set. First real scaling PR.

   **Measured 2026-09-06 at 1000 agents, and this item is right about its
   position and incomplete about its membership.** Every read served from
   memory alone sat within three milliseconds of every other — `/tasks` 5.7ms,
   `/tasks/:id` 5.5ms, `/board/summary` 4.9ms, `/exchange/orders` 6.9ms at a
   p50, on a 10-core arm64 Mac at 819 requests a second. Two reads did not:
   `/leaderboard` at 1379ms and `/reputation/:pubkey` at 1310ms. Roughly two
   hundred and fifty times the others.

   `/leaderboard` is not on the list above and belongs on it. It calls
   `board.leaderboard(usize::MAX)` — the whole field, cloned out from under
   the board lock — then sorts, ranks and pages it, on every request. It is
   the one unbounded read whose cost grows with the number of *agents* rather
   than the number of tasks, which is the axis a public launch adds to. Note
   that this is not the net-worth fan-out §6.2 worries about: that runs only
   for `?sort=net_worth`, and none of these requests asked for it.

   **The mechanism is not established, and three plausible ones are already
   ruled out**, which is the useful part of the result:

   - *Not the write and settlement traffic.* A second run with the write mix
     stripped out kept the same ratio — the absolute numbers fell (this is
     that run), the gap did not.
   - *Not the leaderboard's exclusive lock on the name registry starving the
     reputation route.* Driving 600 concurrent `/leaderboard` requests, which
     take `names.write()` on every call, leaves a concurrent
     `/reputation/:pubkey` at 2ms.
   - *Not per-request cost, and not either route's own concurrency.* Idle,
     `/leaderboard` is 2.3ms and `/reputation` 0.8ms against `/tasks` at
     0.5ms. Driven **alone** at up to 250 concurrent, `/leaderboard` stays
     under 30ms.

   So it appears only when many *different* routes are in flight at once,
   which is what an agent population looks like and what neither a
   micro-benchmark nor a single-route load test would ever produce. That also
   means the remedies this item proposes may not touch it: both routes are
   already paginated, and the cost is not in the page.

   **Next experiments, in order of cheapness:** sweep agent count (100, 300,
   1000) on the same mix and see where the knee is; then drop each read out of
   the mix in turn to find which neighbour the two slow routes are actually
   waiting on. `harness load` takes both without changes. Whoever picks up
   §6.1 should do this before choosing what to build, because the obvious fix
   is currently pointed at the wrong thing.
2. **Node connection churn — pooling done 2026-09-05** (branch
   `node-connection-pooling`). `node_client` opened a fresh TCP connection and
   handshake per operation — three round trips to ask one question — and the
   leaderboard fans out one balance lookup per agent. `NodeClient` now keeps a
   pool of 8 persistent connections; a caller owns one for the length of an
   exchange and returns it only on success.

   **The recommendation this section made was wrong, and measuring is what
   caught it.** "One pooled, mutex-guarded persistent connection (the wallet
   already does this)" is *slower than the churn it replaces*. Against a live
   node, a 50-agent fan-out (the leaderboard's shape) costs 4.9–6.8ms with the
   old connect-per-call client and a very stable ~8.2ms through a single
   pooled connection: serialising fifty lookups behind one socket costs more
   than fifty handshakes save. A pool of 8 lands at 2.8–3.4ms, and 16/32/64
   are flat within noise — so 8, which takes essentially the whole win while
   holding a third of the sockets 32 would. The benchmark that produced these
   is kept as `hub/src/node_client.rs`'s ignored `node_pool_benchmark`; its
   baseline is the real old code path, so the comparison can be re-run rather
   than re-argued.

   Three things pooling made load-bearing that were previously free:

   - **Ordered failover is now an invariant to maintain, not a consequence.**
     The double-spend guards assume one consistent mempool view. A pool that
     kept connections to a primary *and* a secondary after a failover-and-
     recovery would quietly be load-balancing across two mempools. The pool
     therefore holds connections to one address at a time and drops the rest
     when it adopts a new one.
   - **A cancelled request must drop its socket, not release it.** The
     connection is moved out of the pool rather than held under a mutex, so a
     task cancelled between its send and its receive cannot hand the unread
     half of a reply to the next caller. `wallet/src/core.rs`, the pattern
     this was modelled on, holds a mutex across send and receive instead —
     correct for the wallet, which never sees cancellation, and not
     transferable to a hub whose HTTP clients hang up routinely.
   - **Reconnecting.** An operation that fails on a *reused* connection is
     retried once on a fresh one; a failure on a fresh connection is real.
     Safe for the two *reads*, because each is idempotent with respect to a
     failed attempt — see the finding below for why that qualifier is doing
     real work. It was extended to the fire-and-forget write as well, which
     was wrong; see the correction below.

   **Finding — a duplicate transaction is not merely rejected, it is
   punished.** Resubmitting a transaction the node already holds fails
   `add_to_mempool` (equal fee on an already-spoken-for input), and the node
   answers with `strike(peer, severe: false)` and closes the connection.
   Three non-severe strikes inside ten minutes is a one-hour ban of that IP.
   In a single-box deployment the hub, miner and monitoring share an address,
   so a payout path that retried a submission three times in ten minutes could
   ban the whole stack from its own node. Nothing does that today, but "don't
   resubmit an accepted transaction" is now a rule the payout and sweep paths
   depend on rather than an incidental property. Worth a look when §6.5's
   settlement honesty work touches the retry machinery.

   **Correction — pooling a fire-and-forget send lost transactions
   (found and fixed 2026-09-06).** The paragraph above used to argue that the
   retry was safe because "the retry fires only when the send failed, which
   means the node never received the bytes." The premise is false in the
   other direction, and that is the dangerous one: a *successful* send does
   not mean the node received the bytes either. Writing to a socket whose peer
   has already closed does not fail — the bytes land in the kernel's send
   buffer and the call returns success. A read notices, because its reply
   never comes; a fire-and-forget write has nothing to notice with.

   So the pooled client reported success for transactions the node never saw:
   two of four, measured against a node that closes each connection after
   serving it, which is what ours does on every restart and after every
   rejected transaction. Because the hub treats a successful submit as a
   completed payout and stops tracking the task, each loss was a bounty,
   faucet grant, refund, dispute settlement or withdrawal recorded as paid
   that no sweep would ever retry. Sends now always dial a fresh connection,
   whose completed handshake is the closest this protocol comes to proof that
   the node is listening; reads still pool. The cost is one connection per
   submission, a payout-rate cost rather than a hot-path one.

   This narrows the window rather than closing it — the node can still die
   between the handshake and reading the message, or reject what it reads,
   with the hub none the wiser. Only an acknowledged submission closes it,
   which means a wire-protocol change. That is §6.5's work, and this is the
   strongest argument yet for moving it up the order.

   Still open here: the leaderboard read still triggers the fan-out at request
   time when the 30s snapshot is cold. Precomputing it (build-sequence item
   7's other half) is deliberately *not* done, and the reason is worth
   recording, because "precompute it in the sweep loop" does not work as
   written: the sweep runs every 60s and the snapshot's TTL is 30s, so a
   sweep-driven refresh leaves half of every minute cold and changes nothing
   for the request that lands there. Actually taking the fan-out off the
   request path needs a refresh interval *below* the TTL — which means the
   hub pays for a full fan-out every ~25s forever, on a field of whatever
   size, including on a hub nobody is looking at. That is a straight trade of
   "cost on read" for "cost always", and which side wins depends on read
   traffic this deployment does not have yet. Pooling has also taken most of
   the urgency out of it: a cold 50-agent sweep is now ~3ms rather than
   ~6ms, so the request-time cost is no longer the thing that breaks first.
   Decide it with real traffic, not in advance.

   `GET /reputation/:pubkey` remains an uncached, unauthenticated node round
   trip (§3.4) — cheaper per call now, but still one call per request.
3. **Signature-verify CPU — measured 2026-09-06, and it is not what bounds
   the write path.** This item is third on the strength of "every signed
   request costs an ECDSA verify" (§3.4), which is true and is not the
   expensive part. The verify is a rounding error next to the fsync behind it.

   The hub's own authentication ordering makes this measurable without a
   profiler. `verify_charging` runs the drift check, the ECDSA verify and the
   quota charge, and only then claims the signature — and the claim is
   `HubStore::record_seen_signature`, which fsyncs, deliberately before the
   handler runs so that a crash cannot leave a replayable envelope that has
   already moved money (§3.3). A replay is detected *at* the claim and is
   never written. So the same envelope, sent twice to `POST /tasks` from a
   non-operator key, differs by exactly one step: the first is verified,
   charged, claimed, fsynced, and then refused 403 by the handler before it
   touches anything else; the second is verified, charged, and caught in
   memory for 401. The difference is the fsync with nothing else in it.

   On a 10-core arm64 Mac, release build, 40 rounds a run, across four runs:

   | | p50 |
   |---|---|
   | Unauthenticated read | 0.10–0.13ms |
   | Verify + drift + quota charge (401 replay) | 0.18–0.27ms |
   | **The durable claim alone** | **3.21–4.35ms** |

   The claim costs fifteen to twenty-two times what the verification in front
   of it costs. The spread is itself the evidence for which side is which: the
   verify barely moved across four runs, while the claim behind it did, and
   its slowest run was the one that happened to overlap a `cargo test
   --workspace`. A cost that is flat under CPU contention and moves under disk
   contention is a disk cost.

   So this item keeps its place in the order but not its identity: what
   breaks on the write path is a durable commit per signed request, not
   an ECDSA verify. That changes what is worth doing about it. Adding cores
   to a hub that is slow on writes will not help, because the guard's write
   is on the critical path of every authenticated request by design and the
   number to watch is disk commit latency. And the cheapest available win is
   not anything done to the verify: it is group-committing the guard's
   writes, so a burst of concurrent requests shares one commit instead of
   taking one each.

   Whether that win is available at all is the open question, and this drill
   does not answer it: it measures one request at a time, so it says nothing
   about whether redb already coalesces concurrent commits. Measure that
   before building anything. And build it carefully if it is real — the rule
   the guard exists to enforce is that the record is durable *before* the
   request takes effect, so a batch that acknowledged early would reopen
   exactly the hole §3.3 closed.

   The verify is still an attacker-chosen cost and §3.4 is still right to
   price it. It simply is not the ceiling, and this item was written as
   though it were.

   Re-run: `harness drill signed-write-cost`.
4. **The single-instance ceiling.** In-memory board + process replay guard means
   no horizontal scaling. Don't fight it yet: one solid box with fixes 1–3 serves
   thousands of polling agents. Instrument the ceiling; extract shared state only
   when metrics demand.

   **A thousand agents, measured 2026-09-06: the box holds and the claim needs
   one qualification.** One process served 620 requests a second at 1000
   agents, and 819 with the write mix reduced, with every memory-served read
   under 40ms. So "don't fight it yet" is right. But "with fixes 1–3" is
   carrying weight it has not earned: the two reads that are seconds rather
   than milliseconds are not slow for the reason item 1 gives, and item 3
   turned out to be about the wrong cost entirely. Fix 1 as currently written
   would not move them. The ceiling is real and it is not yet the thing in
   front of us; what is in front of us is two routes nobody has explained.
4b. **The operator's payout ceiling is one payment per block** (measured
   2026-09-05 on a live stack, not theorised). Every hub payment spends the
   operator's UTXOs and sends change back to itself, and that change is
   unconfirmed until mined — so immediately after any payout the operator's
   *spendable* balance can be zero, and the next task creation is refused with
   "insufficient escrow balance: operator has 0". `payout_lock` already
   serialises operator payments, so throughput is bounded at roughly one per
   block: about 4/minute at a 16s target. This bites exactly the two paths the
   operator funds — faucet grants and operator-posted task settlement — and is
   invisible on a wallet that happens to hold many coinbase outputs, which is
   why it does not show up in casual testing. Escrow-funded tasks are unaffected
   (they settle from their own deposit address). Mitigations, if it binds:
   deliberately keep the operator's wallet split across many outputs (a
   self-paying "fan-out" transaction), batch payouts into one multi-recipient
   transaction the way consensus settlement already does, or spend confirmed and
   self-change outputs opportunistically. The faucet sunset (§5.1) removes the
   larger half of the problem on its own.

   **Confirmed by measurement 2026-09-06** (`harness drill payout-ceiling`).
   The condition had to be constructed, and that is the part worth recording:
   on a local stack the miner pays the operator a fresh 50-coin output every
   block, which is precisely the many-output wallet this ceiling is invisible
   on, so a drill that simply fired payouts at a default stack would have
   found nothing. The drill moves the miner onto a key nothing else uses, has
   the operator pay itself its whole confirmed balance minus the fee — leaving
   exactly one output and no change — and only then measures.

   Result: 31 faucet grants across 30 blocks,
   1.03 per block, **never two in one block at any height**,
   against 723 offered (24.1 attempts per block, so the
   run had ample room to find a higher ceiling had there been one). The other
   692 were refused for insufficient balance. The prediction was
   "about one per block"; it is exactly one per block.

   Two things this settles. The mitigations listed above are not optional
   under load, they are the difference between four payouts a minute and any
   other number. And the faucet sunset (§5.1) is worth more than it looks:
   onboarding a thousand agents through a faucet at one grant per block is a
   serial queue of roughly four and a half hours at a sixteen-second target,
   whatever the hub's own latency is.

   **Fixed 2026-09-07 (branch `payout-ceiling`).** The first of the three
   mitigations, built: the hub keeps its wallet split across many confirmed
   outputs. Re-measured on the same drill against the same pre-fix baseline,
   754 payouts across 30 blocks — **25.13 per block, 301.4 grants a minute,
   busiest block 26**, against 1193 offered. The baseline was 31 across 30,
   never two at any height. `harness compare` reads it as confirmed →
   refuted and exits 0. An earlier run on the same code less the last two
   commits gave 26.83 per block and 321.8 a minute, so the spread between
   runs is a few per cent and the result does not turn on either one.

   **The baseline is now that healthy run**, following the convention §6.7
   item 7 established: the comparison above was the sign-off, and a
   baseline's job afterwards is to be the gate. Leaving the pre-fix one in
   place would have made the gate useless in the one direction that
   matters — a regression reports `confirmed` against a `confirmed`
   baseline, so the verdict never changes and `compare` says nothing. The
   pre-fix numbers are not lost; they are two paragraphs above, where
   somebody reads them.

   #### What was built

   Two halves, both in `hub/src/operator_wallet.rs`, which is pure and
   tested on its own.

   **Choosing which output to spend, which turned out to be half the problem
   and to cost nothing.** The node returns its UTXOs in `HashMap` order and
   `build_multi_payment` walks them front to back, stopping as soon as it
   has enough — so the *order* is the selection policy, and no change to
   btclib was needed to change it. A payment now spends the smallest single
   output that covers it, falling back to largest-first when no single
   output does. Left alone, a 0.5-coin faucet grant would as often as not
   spend a 150-coin output and turn the rest into invisible change: one
   payment could undo a whole fan-out.

   **Keeping the wallet in shape.** The sweep, and one pass at boot, split
   the largest output into equal shares whenever the count of spendable ones
   has fallen below `--operator-wallet-outputs` (24 by default, and that
   number *is* the ceiling). Equal shares rather than a fixed denomination:
   a fixed size leaves a remainder that is either dust or another blob, and
   converges on a count set by the remainder rather than by the floor.

   #### Three things the shape forced, and one the drill found

   A fan-out's own outputs are unconfirmed until mined, so the next sweep
   sees the same short wallet and would split again — this time taking an
   output that was already doing its job. The hub remembers what the last
   one spent and skips while those inputs are still in the UTXO set, which
   needs no timer and nothing carried across a restart.

   **A freshly deployed hub pays out nothing for one block**, and this is
   the one cost the fix adds. It holds a single output; the first fan-out
   spends it and the pieces are invisible until mined. Measured at exactly
   one block. The boot pass runs before the listener opens, so that block is
   spent while the hub is unreachable rather than under the first burst of
   arriving agents. Restarting a warm hub does nothing.

   **The wallet erodes, and only splitting cannot recover it.** Every
   payment leaves change, so a slot comes back smaller each time it is
   spent; eventually every output is under the useful size and a
   splitter-only planner has nothing large enough to split. It would return
   "nothing to do" forever against a wallet holding whole coins, while
   payments ground on through ever-larger combinations of ever-smaller
   inputs. So the planner consolidates before it splits: sweeping worn-out
   change into one usable output costs a fee, takes nothing out of service,
   and always raises the count.

   That last one is worth its own note, because the first reading of the
   evidence was wrong. The drill's payouts fell from 24 a block to nothing
   after twelve blocks, which looked exactly like the erosion trap. It was
   not — 299 grants at 50,001,000 against a 14,999,999,000 wallet leaves
   49,700,000, just under one more grant. The operator had simply spent
   everything. The trap is real, the drill never hit it, and its test had to
   be written rather than observed.

   #### The faucet's own half

   Independent of the wallet work, and the reason this was ranked ahead of
   work that touches more code. `faucet_claim` spends the proof-of-work
   challenge durably before it pays, which is correct and must stay that
   way. But the payment that followed failed 95% of the time under this
   ceiling, and the answer was a 500 reading "please retry" — so an arriving
   agent solved for fourteen seconds, was told to try again, and solved
   again. The grant was recoverable; the challenge was not.

   The hub now asks whether it can fund the grant *before* spending the
   challenge, which turns the common case into one cheap refused request.
   The challenge is validated first, so a spent or unsolved one is still
   told so rather than told the hub is busy. Where the pre-flight passes and
   the payment fails anyway — a slot taken in between — the challenge is
   gone and cannot be ungone, so a fresh one is issued at no cost and
   attached to the response. Not "hold the redemption open": the redemption
   record is what stops a solution being spent twice, and a hub that reopens
   it under any condition is a hub whose faucet can be replayed by arranging
   that condition.

   Both answers are a 503 with `Retry-After` set to the chain's own block
   target, and the same number in the body where an agent parsing JSON will
   see it. `/llms.txt` documents both shapes and how to tell them apart.
   This is the first `Retry-After` the hub has ever sent; §3.4's note that
   the rate limits document no budgets at all still stands for the 429s.

   #### To watch, and what is still open

   `hub_operator_ready_outputs` is the gauge: it is the ceiling, and a hub
   sitting near zero is a hub refusing grants.
   `hub_operator_fan_outs_total` should be near-flat on a healthy wallet,
   which refills itself from its own change; a rate that keeps climbing
   means the fan is eroding faster than a block restores it.

   The other two mitigations are untouched and still worth what they were.
   Batching several owed payments into one multi-recipient transaction helps
   the sweep, not the faucet, since a grant arrives alone. Spending
   confirmed and self-change outputs opportunistically is subsumed: the
   selection change *is* that, done properly.

   Two limits a reader should know. The floor is a count, so the wallet's
   *rate* is two dozen payments a block and its *total* is still whatever
   coin the operator holds — the fan changes how fast money can leave, not
   how much there is to leave. And a payment larger than a slot is not
   refused, it combines slots largest-first and costs the fan one per extra
   input, which the next sweep restores.
5. **Settlement honesty — fixed for task bounties 2026-09-06 (§6.5).**
   `submit_transaction` is fire-and-forget, so "paid" used to mean "sent."

   It was filed here as a display problem and promoted when two findings
   landed on it from opposite directions. The pooled-send bug (§6.2) showed
   that "sent" can mean "handed to a closed socket." The deployment work
   showed the node's mempool is memory-only, so stopping the node discards
   every transaction submitted since the last block while the hub goes on
   reporting those payouts as made. The drills then priced it: killing the
   node mid-payout destroyed 6,000,000 ITX the hub still called paid (§6.7).
   Both failures had one root — the hub recorded a payout as complete on the
   strength of a write it never got an answer to, and once a task left
   `Verified` nothing revisited it.

   A bounty now waits for chain evidence, and the case above self-heals with
   no operator. **The other three paths followed on 2026-09-07** — faucet
   grants, escrow disbursement and exchange withdrawals all run through
   `payments` now, which is the same fix generalised rather than repeated
   three times against three status fields (§6.5e). And §6.5b below is the
   deposit-side twin,
   which this work does not touch — it was fixed separately the same day, by a
   redb transaction rather than by chain evidence, because a deposit's problem
   is two local commits and a payout's is a write to another process. Detail in
   §6.5.

5b. **Escrow confirmation was not crash-safe — found 2026-09-06 by
   `harness drill escrow-restart`, fixed the same day (branch `escrow`).** One
   escrow deposit could fund two tasks. All three confirm handlers are now
   crash-safe; this closes the last of §6.7's three failures.

   **What was wrong.** `confirm_task_escrow` did four things in order: read the
   pending deposit, ask the node what landed at the derived address, create the
   task in memory and persist it, and then persist the deposit's new `Consumed`
   status. The last two were separate redb writes, and a process that stopped
   between them left a task on disk beside a deposit that still read
   `Reserved`. On restart the board loaded both. The depositor could then
   confirm the same escrow again — with a fresh envelope, since the replay
   guard had spent the old one — and get a second task funded by a deposit that
   was only ever paid once. Only the depositor could do it, which bounded who
   was exposed and did not make the books any less wrong.

   Measured before the fix: confirmations interrupted by `SIGKILL` mid-handler
   produced one deposit backing two `Open` tasks of 1,000,000 ITX each —
   verified independently of the drill by restarting a hub against its store
   and listing tasks, which showed the same description twice with two ids. The
   `SIGTERM` phase, which the hub drains, lost nothing in any run. So this was
   specifically a crash, not a deploy.

   `confirm_exchange_deposit` and `confirm_dispute_escrow` had the same shape
   and neither was ever drilled. Both persisted their effect — a credited
   exchange account, a settled dispute bond — and only then the deposit.

   #### What was built

   **One redb transaction, which was the second of the two options and the
   better one.** `HubStore::save_task_and_deposit` and
   `save_exchange_account_and_deposit` commit the effect and the deposit's
   `Consumed` status together; all three handlers now call one of them. Both
   are built on a small `in_one_write_txn` primitive that commits only if every
   staged write succeeded.

   The cheaper option — persist the deposit *before* the effect, so that a
   crash strands the deposit instead of duplicating it — was rejected even
   though it is a real improvement, because it only makes the failure a better
   failure. It leaves an interval whose safety depends on nobody ever adding a
   step between the two writes, and it makes a stranded deposit the expected
   outcome of a crash rather than something that cannot happen. A single
   transaction has no interval at all, and it removes the stranded case too:
   the drill's `deposits_stranded` count is now structurally zero rather than
   merely observed to be zero.

   The handlers also read the deposit back **under the same write lock that
   consumed it**, rather than reacquiring the lock afterwards as the old code
   did. That is what makes the pair internally consistent; the previous shape
   could in principle persist whatever a concurrent caller had left behind
   between the two acquisitions.

   One behavioural change worth naming: persisting the deposit used to be
   best-effort — it logged an error and still returned 200. It is now part of
   the same fallible write as the effect, so a store failure returns 500 with
   nothing committed, and a restart reverts to a `Reserved` deposit the
   depositor can simply retry.

   #### How it was proved, and how it was not

   **Not by a green drill run, and the drill still says so.** Its hard-kill
   phase reports *inconclusive* rather than confirmed when it finds nothing,
   and that behaviour is deliberately unchanged: the fix does not make a
   deterministic verdict possible, because absence of an interval is not
   something a sampling run can observe. Three things carry the weight instead.

   **The argument from shape, which is the strongest available.** A single
   transaction has no interval for a `SIGKILL` to land in. This is a property
   of the code rather than of a sample, and it is why the fix is a transaction
   and not a reordering.

   **A store-level test that the two writes cannot be observed apart.**
   `a_failure_after_staging_both_records_commits_neither` stages both records
   and then fails — the crash, hit deterministically on every run rather than
   by chance — and asserts neither survived. Two companions:
   `a_task_and_its_deposit_are_never_visible_apart` (and its exchange twin)
   checks against a read snapshot taken before the write that no reader ever
   sees one record without the other, and
   `two_separate_commits_leave_a_window_where_the_task_exists_alone` makes the
   *old* shape's window executable, so the suite states the mechanism of the
   bug rather than only describing it. Suite went 342 → 346.

   Note the one thing no test covers, because it cannot: nothing catches
   `save_task_and_deposit` being rewritten back into two commits. Two commits
   differ from one only in the existence of a window nothing can be scheduled
   inside on demand, which is the original problem restated. The method's doc
   comment carries that, not the suite.

   **An A/B against the drill, which is better evidence than a clean run.**
   Five runs of `harness drill escrow-restart` against the fix, then five more
   on the same machine in the same session against the pre-fix hub sources
   (`d0d2ed0`, checked out over the tree and rebuilt), everything else
   identical:

   | | SIGKILL verdicts | deposits funding two tasks | deposits stranded | handlers that committed a task at all |
   |---|---|---|---|---|
   | Pre-fix control | 4 refuted, 1 inconclusive | 2, 1, 0, 1, 1 — **5 total** | 0 | 2, 1, 0, 1, 1 |
   | With the fix | 5 inconclusive | 0, 0, 0, 0, 0 — **0 total** | 0 | 1, 0, 0, 0, 1 |

   The column that matters is the last one against the second. Pre-fix, the two
   were **equal in every single run**: every handler that got as far as
   committing a task produced a duplicate, five for five, because committing
   the task is precisely what opens the window. With the fix, two handlers got
   that far and neither duplicated. That is a conditional rate of 5/5 falling
   to 0/2 — evidence, not proof, and the sample on the clean side is small
   because the drill's coverage is low, but it is a great deal more than "a run
   that found nothing."

   It also settles a red herring. The `SIGTERM` phase reports one confirmation
   dropped without an answer in most runs, which the baseline (a single run
   from a different session) does not show. The control reproduces it at the
   same rate — three runs of five pre-fix, four of five post-fix — so it is
   pre-existing variance in the drain, not something this change introduced. It
   costs nothing: the escrow is not consumed and the retry recovers it.

   `harness compare` against `harness/baselines/escrow-restart.json` reports
   `verdict: refuted -> inconclusive`, `deposits_funding_two_tasks: 1 -> 0` and
   the finding `gone`, for all five runs. The baseline itself is deliberately
   **not** refreshed, following `node-crash`, whose baseline still holds its
   pre-fix `itx_lost: 6000000` after §6.5 landed: a baseline is the "before"
   and the comparison is the sign-off. Worth knowing about that convention,
   though — because this drill's stored verdict stays `refuted`, a future
   regression would compare refuted-to-refuted and `harness compare` would exit
   0. That is a gap in the convention rather than in this fix, and it is left
   for whoever revisits the baselines as a set.

6. **Polling herd:** `ETag`/`If-None-Match` on `/tasks` first (cheap); an SSE feed
   for new tasks later — or A2A push notifications for that rail (§7.8).
7. **Prove it — measured 2026-09-06.** Built as `harness/`, a workspace member
   rather than a k6 or vegeta script: every authenticated route wants a
   secp256k1 signature over a canonical string, so an external tool would need
   a third implementation of the recipe beside `sdk` and `agent-sdk-py`, free
   to drift from both. It signs through `sdk::build_envelope` and keeps an
   independent view of the chain over the node's own protocol — the drills
   that matter ask whether money actually landed, and the hub is exactly the
   wrong thing to ask.

   Two halves. `harness load` drives a cohort against a stack it is pointed at
   and reports latency percentiles per request kind. `harness drill` runs seven
   deliberate failures, each bringing up its own stack on ports 9040/9140 so it
   can kill part of it, each returning a verdict against a numbered claim here.
   Reports are JSON with stable fact keys, checked in under `harness/baselines/`,
   and `harness compare` diffs a fresh run against one — the property
   `node_client`'s pooling benchmark has, that the comparison can be re-run
   rather than re-argued.

   **What the drills found**, as measured on 2026-09-06 before that day's
   fixes. Six of eight claims held. Of the two that did not: §6.5b was a bug
   and is now fixed, and §6.3 was a claim about where the write path's time
   goes, corrected in place rather than fixed.

   | Drill | Claim | Result |
   |---|---|---|
   | `node-crash` | §6.5 loses money silently | Confirmed — 6,000,000 ITX destroyed |
   | `escrow-restart` (SIGTERM) | A drained restart is safe | Confirmed |
   | `escrow-restart` (SIGKILL) | A crash leaves consistent state | **Refuted** — one deposit funded two tasks (3 runs of 6). Fixed the same day; re-runs report inconclusive with zero duplicates, see §6.5b |
   | `replay-storm` | §3.3's guard survives a crash | Confirmed — 0 of 30 accepted |
   | `rate-limit-tiers` | §3.4's buckets are independent | Confirmed — 120 and 59 served exactly |
   | `quota-isolation` | §3.4's quota is per identity | Confirmed — 60 served, bystander untouched |
   | `payout-ceiling` | §6.4b is about one per block | Confirmed — exactly one, at every height. **Refuted** since 2026-09-07: 25.13 per block, busiest 26, see §6.4b |
   | `signed-write-cost` | Item 3: verify CPU is the write cost | **Refuted** — the fsync is 15–22x the verify |
   | `exchange-restart` (clean) | §6.5d's ledger survives a restart | Confirmed — and it passes pre-fix too, so it is not evidence; see §6.5d |
   | `exchange-restart` (SIGKILL) | §6.5d's fill is crash-safe | Pre-fix **Refuted** 3 of 3, fixed inconclusive 2 of 2 |

   Both refutations are recorded where they belong: item 3 above, and item 5b,
   which is a bug this list did not know about.

   **The verdict column above is why the harness's own signal was worthless
   until 2026-09-07.** Read it again: for `node-crash` and `signed-write-cost`
   the healthy answer is *Refuted* — one because §6.5 predicted the failure and
   the fix removed it, the other because the claim it tests was simply wrong and
   the plan was corrected rather than the code. But `Report::needs_attention`
   keyed off `Refuted` or any finding at all, so **three of eight drills exited
   non-zero on a completely sound hub** (those two plus `escrow-restart`, which
   reports a known-harmless drain variance in most runs). `harness drill all`
   was therefore red whatever the hub did, and the "put it in front of a change
   and be told" property this section claims did not hold. Worse, `harness
   compare` hardcoded "refuted is the bad direction" — so a §6.5 regression,
   which turns `node-crash` from refuted back to *confirmed*, would have been
   printed as somebody's fix landing and exited 0. The most valuable regression
   in the repo was the one the gate could not see.

   **Fixed by separating the two questions.** A section now declares which
   verdict means "nothing to act on" (`healthy_when`, defaulting to
   `Confirmed`), and a drill can mark an observation it has already
   investigated as `accepted_finding` rather than a finding. `needs_attention`
   and `compare` both derive from those instead of from a hardcoded direction,
   and `compare` reads the healthy verdict from the *current* report so
   baselines written before the field existed still score correctly.
   `Inconclusive` is never a failure by itself — it is the absence of an answer,
   and for a sampling drill it is the expected post-fix state.

   Two things fell out that are worth more than the tidy-up. **§6.5b's
   refuted-to-refuted gap closes** for `escrow-restart`: re-baselined on a
   post-fix run its SIGKILL half sits at `inconclusive`, and the bug returning
   reads as refuted — a problem where the baseline was not one — which
   `compare` now catches. And the direction of a regression is per drill rather
   than global, which is the thing the old rule could not express at all.

   **So the baseline convention inverts, for every drill and not just the
   asserting ones.** Through 2026-09-06 a baseline was the *pre-fix* run,
   because for a sampling drill a clean run is a failure to reproduce and makes
   poor evidence. That reasoning was about the run's value as *evidence*, and
   it is still right — but a baseline's job is to be the thing a future run is
   gated against, and for that it has to be the state you want to keep. Every
   drill now baselines on its healthy run. The pre-fix numbers are not lost:
   they are the evidence a fix is argued from, so they belong in the plan
   section that argues it, where somebody reads them — §6.5b for
   `escrow-restart`, §6.5 for `node-crash`, §6.5c above for `escrow-refund` —
   rather than in a file whose only reader is a diff tool.

   **Measured, not asserted:** `signed-write-cost` and `escrow-restart` both
   exited 0 on a healthy hub after the change, having exited 1 before it. The
   `escrow-restart` run that proved it happened not to produce its drain
   variance — that fires in roughly four runs of five — so the accepted-finding
   path itself is covered by `an_accepted_finding_is_not_a_new_finding` and by a
   later run rather than by that one.

   Both directions are pinned by tests (`for_a_pessimistic_claim_confirmed_is_the_regression`,
   `inconclusive_is_not_a_problem_but_regressing_out_of_it_is`,
   `an_accepted_finding_is_not_a_new_finding`), and the accepted findings each
   say in their own text why they are accepted — an accepted finding without
   that reasoning is indistinguishable from one somebody silenced to get a
   green run.

   **And a second way the instrument was worthless, found 2026-09-07 while
   re-running `payout-ceiling` for §6.4b.** The harness's `claim_faucet` had
   gone on posting a payload-less `POST /faucet` since the proof of work
   landed on 2026-09-06. The hub answers an envelope whose payload does not
   deserialize with a 422 before any handler runs, so every drill that touched
   the faucet had been measuring a rejected body for a day.

   `payout-ceiling` is where that was expensive, and the shape of the failure
   is worth carrying. Zero grants reads, through the drill's own arithmetic,
   as a ceiling comfortably held — so it would have reported `Confirmed`
   against a hub it never asked for a single payout, and the sign-off for the
   §6.4b work would have been a measurement of a 422. A drill whose *failure
   mode looks like its healthy result* is worse than no drill, and this one
   had that property by construction: "few payouts landed" is both the thing
   it looks for and what a broken probe produces.

   Two things would have caught it and neither existed. Nothing asserts a
   drill's probe actually did what it claims — `payout-ceiling` never checked
   that any grant succeeded, only how they were distributed. And nothing runs
   the drills on a schedule; the harness README predicted this exact change,
   named the one function it would need, and nothing read that note for a day
   because nothing ran. The absent CI is already on the open list; this is a
   second argument for it and a first for the assertion.

   **What the load half found**, at 1000 agents against a 200-task board on a
   10-core arm64 Mac, release build, 60 seconds:

   | request | p50 | p90 | p99 | max | rate |
   |---|---|---|---|---|---|
   | `GET /tasks` | 37.7ms | 453ms | 1189ms | 2661ms | 251/s |
   | `GET /tasks/:id` | 39.7ms | 470ms | 1240ms | 2662ms | 64/s |
   | `GET /board/summary` | 36.1ms | 456ms | 1110ms | 2638ms | 30/s |
   | `GET /exchange/orders` | 36.7ms | 445ms | 1041ms | 2111ms | 19/s |
   | `GET /leaderboard` | **3368ms** | 4870ms | 6427ms | 8896ms | 48/s |
   | `GET /reputation/:pubkey` | **3300ms** | 4674ms | 6187ms | 7179ms | 25/s |
   | `POST /tasks/:id/claim` | 81.8ms | 495ms | 1292ms | 2666ms | 161/s |
   | `POST /tasks/:id/submit` | 146.8ms | 928ms | **22369ms** | 29241ms | 23/s |

   Offered 1000 requests a second, achieved 620. Three results are worth
   carrying forward.

   **Two reads are two orders of magnitude slower than the rest**, and they
   are `/leaderboard` and `/reputation/:pubkey`. Recorded against item 1
   above, together with the three mechanisms already ruled out — it is not
   the write traffic, not the name registry's lock, and not either route's
   own concurrency. It only appears under a mixed workload.

   **Submissions have a very long tail**: a p50 of 147ms against a p99 of
   22 seconds and a maximum of 29. That tail is the settlement path — a
   correct submission takes `payout_lock`, builds a payment and hands it to
   the node, and the operator settles about one payment per block (§6.4b), so
   under load the queue behind that lock *is* the tail. Thirty-two requests
   exceeded the harness's own 30-second client timeout. An agent that submits
   correct work at any volume will see multi-second waits and some timeouts,
   today, and nothing in the API tells it that is expected.

   **Claims are 84% conflicts**, which is the profile working rather than
   failing: a thousand agents on a two-hundred-task board means most claims
   lose the race. Worth knowing as a shape, though — the board's supply, not
   the hub, is what most agents will experience as the bottleneck.

   **Two things the harness could not measure, deliberately.** Fills on the
   exchange never happen in the profile, because a sell locks the compute
   asset and compute is issued by exactly one thing — settling a task tagged
   `compute` — so a maker funded by deposit holds base and nothing to sell.
   That is a real property of the market and is worth knowing before launch
   (§7.5). And the faucet is not in the load loop at all: one claim is one
   operator payout, so a thousand of them is a thousand blocks, about four and
   a half hours at a sixteen-second target. The harness samples the rate
   instead and reports what a cohort would cost.

   Numbers, how to re-run each drill, and the machine they came from are in
   `harness/README.md`. Everything in this section marked "measured
   2026-09-06" came from it.

### 6.5 Confirming a payout

Specified 2026-09-06, built the same day (branch `settlement`). This section
was a spec; it is now a record of what was built, what the build changed, and
what is still unconfirmed.

**The problem it fixed, in one line.** The hub recorded a payout as complete on
the strength of a write it never got an answer to.

`SubmitTransaction` is one-way: the protocol has no reply meaning accepted and
none meaning rejected. On sending, the hub moved the task to `Paid`, and that
was a dead end — `pending_payouts` returned nothing once a task left
`Verified`, and the sweep only revisited `Verified` tasks. A payout that never
happened was indistinguishable from one that did, forever. Three ways it
failed: the node rejects the transaction and strikes the peer without telling
us; the node accepts it into a memory-only mempool that a restart discards; or
the socket was already dead (fixed 2026-09-06, §6.2).

**No protocol change was needed, and none was made.** `FetchUTXOs(pubkey)`
already returns each output with a flag for whether the mempool has spoken for
it, and every `TransactionOutput` has a stable hash. The hub already used
exactly this to confirm *inbound* escrow deposits; the machinery had only ever
been pointed at money coming in.

Two properties of the existing code make the design work. Both now have tests
of their own rather than being assumed:

- `build_multi_payment` skips marked outputs, so a second payout can never
  select an input the node is already holding a transaction for
  (`build_payment_skips_marked_utxos`, which predates this work).
- Every output carries a fresh `unique_id`, so its hash identifies *this*
  payout attempt and not merely "a payment of this size to this key"
  (`two_builds_of_the_same_payment_have_different_output_hashes`, added here).
  A rebuild after a genuine loss produces a different hash, which is what makes
  attempts distinguishable — and what stops a resend confirming itself against
  the money it was resent because it lost.

**The state machine, as built.** `TaskStatus::Submitted` sits between
`Verified` and `Paid` and means "the bytes left this process". The per-payout
evidence lives in a `PayoutAttempt` record — the recipient's output hash, the
inputs spent, which address they were spent from, the submit time, and how many
times this payout has been submitted — in its own `payout_attempts` table in
`hub.redb`, added purely additively with no `SCHEMA_VERSION` bump, exactly as
the replay guard's table was. **That last clause is superseded by §6.5c**,
which bumped the version to 2 and reversed the no-bump policy: this table in
particular is why. A rolled-back binary passed the version check and could not
see it, so every payout in flight became invisible.

**One record per (task, recipient), not per task.** The spec said "recording
the recipient's output hash" as if a task had one. A `Consensus` task has
several winners and one can confirm while another has not, so the evidence
cannot hang off the task. Keying per recipient also makes the escrow-funded
multi-winner case (one transaction, several outputs) and the operator-funded
case (one transaction each) the same code path: `build_multi_payment` gives
each recipient their own output, so each has its own hash and resolves on its
own. The task reaches `Submitted` only once every payout it owes has a
transaction in flight, so a task with one leg unsent stays `Verified` and the
sweep keeps sending that leg.

**The three-way rule**, unchanged from the spec and now `PayoutAttempt::resolve`
— a pure function over the two UTXO sets, so all three rows are testable
without a node:

| What the node shows | Meaning | Action |
|---|---|---|
| Recipient's output hash present | Reached the node and was mined | Mark `Paid` |
| Output absent, spent inputs still present and unmarked | Never landed anywhere | Resubmit, new attempt |
| Output absent, inputs gone or marked | Ambiguous | Leave `Submitted`, alert the operator |

The third row is the honest escape hatch and is not collapsed into either
neighbour. Resubmitting there risks duplicating a payment the chain already
made, and the node punishes a duplicate with a strike — three inside ten
minutes bans the box from its own node (§6.2). Marking it `Paid` would
reintroduce exactly the lie this removes.

**Why the recipient's output and not just the inputs.** A recipient can spend
its bounty immediately, so a confirmed payout can look like one that never
happened if you only watch the operator's side. The inputs are the
corroborating signal, not the primary one.

#### What the build changed

- **The table needs a grace period to be sound.** Applied to an attempt made
  moments ago it reads as row two and *acts on it*, sending a second payment:
  the send is fire-and-forget and returns before the node has read the bytes,
  so the output is absent and the inputs are untouched. The sweep therefore
  leaves a payout alone for `PAYOUT_RESOLUTION_GRACE_SECONDS` (30) before
  asking. At a 60-second sweep cadence this costs nothing — a payout sent in
  one interval is already older than the grace by the next — and it is the
  difference between the rule being correct and being a race. This was not in
  the spec and is the most important thing the build added.
- **A resend from an escrow address must rebuild every still-owed winner**, not
  the leg that was lost. Change goes back to the depositor, so paying a subset
  sends the rest of the money home and strands whoever was left out — the same
  hazard `build_multi_payment` exists to avoid, reachable again through the
  retry path.
- **The superseded attempt must outlive its replacement.** The new attempt's
  submission count is read from the old one, so clearing it first resets the
  budget on every retry and a payout that can never land retries forever
  instead of reaching a terminal state.
- **`for x in board.read().await.attempts()` deadlocks.** Rust keeps the
  iterator expression's temporaries alive for the whole loop body, so the read
  guard is still held when the first confirmation takes the write lock. It hung
  the sweep, not just a test.
- **Following the chain properly is not blocked on a protocol change either.**
  The spec implied it would be more work partly for that reason.
  `FetchBlocks { start, count }` already exists and `node` already answers it,
  including past the tip. It remains more work — the hub would need to track
  the tip, scan for its own transaction hashes and keep a confirmation depth —
  but nothing stands in the way. See the decision below.
- **`FakeNode` had to become a real UTXO set.** It minted a fresh output per
  `FetchUTXOs`, so the same money hashed differently every time it was looked
  at and could never confirm. It now applies submitted transactions, and can
  also hold one in a mempool or swallow it whole — the two things a real node
  does that a successful send cannot distinguish.

#### The three things left open, decided

- **Resubmission budget: four submissions total** (`MAX_PAYOUT_SUBMISSIONS`),
  the original plus three resends. Every resend after the first fires only once
  row two has *proven* the previous one never reached the chain, so none of
  them can duplicate a payment or earn a strike. The budget is not there for
  safety, it is there to stop a loop. Four attempts span at least three sweeps,
  which rides out a node restart and a re-sync; repeated proven loss after that
  is not a transient — it means the hub is building something the node will not
  take (a fee floor moved, the operator's balance is mis-modelled) and a fifth
  identical attempt fails identically while hammering a node already unwell.
- **Abandoned payouts get a new terminal state, `PayoutFailed`, and the escrow
  is left exactly where it is.** Not `Closed`: `Closed` means nobody was owed
  anything and refunds the poster, which here would take the bounty back from
  someone who did the work. Not refunded, not marked settled, no reputation
  credited — the money is still owed and the task keeps reporting it as owed
  (`unconfirmed_payout_total` answers regardless of status, which is why it
  exists). An operator resolves it by hand; `docs/deployment.md` §10.3 is the
  runbook. Note what the state does *not* cover: only *proven* loss reaches it.
  An ambiguous payout stays `Submitted` and keeps alerting, because
  `PayoutFailed` asserts the money never moved and ambiguity is precisely not
  knowing that.
- **Follow the chain properly: not now, and here is what would change it.** The
  polling table resolves every case the hub can currently distinguish, and row
  three should be rare — it needs the recipient to spend the bounty, or the
  node to still be holding the transaction, inside the window between two
  sweeps. The evidence that would justify the extra machinery is how often row
  three actually fires under §6.7's load test, and how long payouts sit in it.
  Both are countable today: every ambiguous resolution logs a `warn!` naming
  the task and its submit time. Decide from that, not from taste. If it fires
  often, chain-following resolves every row definitively and gives the hub a
  real notion of confirmation depth, which the dashboard and the API will
  eventually want anyway.

#### 6.5e Every other payment path, closed 2026-09-07

This section used to be called "Still unconfirmed, and knowingly so", and
listed faucet grants, escrow disbursement and exchange withdrawals as three
smaller versions of the same fix against three different status fields. That
framing is what made it easy to defer: each looked small on its own and none
looked launch-blocking.

A review on 2026-09-07 rejected that reading, and was right to. The three
together are most of the money the hub moves that is not a task bounty, and
"reports success before confirmation" is the same defect the bounty path was
fixed for — the deferral was about where the code lived, not about how much
was at stake.

**They are closed, and not by being written three times.** `hub/src/payments.rs`
is one lifecycle covering all of them: a `Payment` record and its ledger
reservation commit in a single transaction *before* the send, the sweep
resolves them against the chain, and an uncertain outcome retains the
obligation rather than releasing the debit. Inputs backing an unresolved
payment are reserved, so a wallet reshape cannot consume the evidence.
`NeedsReview` is the terminal state for one proven lost.

Three consequences worth carrying:

- **A send error is never permission to undo a debit.** Writing to a socket
  whose peer has gone does not fail, so an error does not establish that the
  node never got the bytes. Crediting a withdrawal back on that signal is how
  one withdrawal becomes two.
- **A resend is safe for a structural reason.** It submits the identical
  transaction spending the identical inputs, so at most one can ever be mined.
  That, not probability, is what makes acting on `NeverLanded` sound.
- **The UTXO set cannot answer the last question, and the chain can.** A
  recipient who spends onward leaves a payment indistinguishable from one that
  never landed and lost its inputs to something else. That pair resolved to
  nothing at all, so the payment stayed `Pending` for the life of the
  deployment and the oldest-pending gauge climbed past every threshold —
  retiring the alarm for the payments that really were stuck. The fix reads
  blocks from the payment's own era forward, bounded and resumable, and only
  for a payment already gone ambiguous. It is a lookup, not a sync: the hub
  still keeps no chain.

The last of those is the strongest argument in this document for chain
indexing, and it is worth noting that it is a *settlement* argument rather
than a sybil one — §4's clustering was the case previously made for reading
the chain, and this one is both more concrete and already load-bearing.

**Verified against a real stack, not only against tests.** On 2026-09-06, with
node, miner and hub from this tree: a submitted payout read as `Submitted` with
`bounty_pending` set and no reputation credited; it confirmed itself on the next
sweep. Then, with the miner stopped, a 2000-unit bounty was submitted and the
node was killed with the transaction in its mempool — the exact case
`docs/deployment.md` §7.2 called the most expensive thing in that document. The
hub logged `never reached the chain (submission 1 of 4), resending` on the next
sweep and `confirmed on chain after 2 submission(s)` on the one after, and the
agent ended with `total_earned: 3000` against an on-chain balance of exactly
3000 — recovered without a human, and without paying twice. The table in
`docs/deployment.md` §11 records it.

**Sequencing.** This landed before the faucet PoW work (§5) as planned, because
both touch the faucet payout path and this one changes what `Paid` means. It
unblocks the honest `pending`/`confirmed` fields the API and dashboard owe
agents, and relaxes the standing operational rule that the node must not be
stopped with transactions in flight (`docs/deployment.md` §7.2).

**Measured 2026-09-06, before any of this was built** (`harness drill
node-crash`, baseline in `harness/baselines/node-crash.json`). Six bounties
were paid with the miner stopped, so that the pre-block window stayed open
rather than having to be raced, and the node was then `SIGKILL`ed. The hub
reported all six as complete and returned `paid: true` to every agent. On the
restarted chain, 0 ITX of the 6,000,000 existed:
6,000,000 ITX destroyed, six agents credited nothing. All six tasks still
read `Paid` over the API after a full sweep interval, because the sweep only
revisits `Verified` — so nothing in the hub will ever look at them again.

That is this section's claim exactly, with a number on it. Re-run the same
drill after the confirmation work lands and compare against the baseline: the
`Submitted` state should make each of those six resolvable by the second row
of the table above — output absent, spent inputs still present and unmarked,
resubmit.

### 6.5c Durable status, and not trusting the board on boot

Found by audit on 2026-09-07, built the same day (branch `escrow-status`). Five
defects, all one disease: **a state change that lives in memory and reaches disk
as two or more independent writes, or not at all.** §6.5b was the first case
found, by a drill; this is what reading for the pattern turned up. Suite went
310 → 327 on the hub target, workspace green.

The section exists because §6.5b fixed the pattern in one place and nowhere
else. That is worth stating on its own: a fix arrived at by a drill lands where
the drill was pointed, and the same shape elsewhere stays invisible until
somebody goes looking. Four of the five below had never been drilled and could
not have been — see "what the drills could not have caught" at the end.

#### The worst of them: `Refunded` was written nowhere

`save_pending_deposit` has five call sites and **every one of them is creating a
reservation**. `mark_escrow_refunded` had one caller, inside `disburse_escrow`,
and no store write followed it. So `Consumed` became durable when §6.5b added
the pair-writers and `Refunded` never did: the status change lived only in
memory and died at the next restart.

Not a race. A refunded deposit reloaded as `Reserved` on **every** restart, and
had since escrow was written. Three consequences, worst first.

**A settled dispute bond was settled again at every boot — and this is the one
claim in the handoff that measurement did not support.** `settle_dispute_bond`
disbursed the bond, credited the winner with `credit_forfeited_bond`, and saved
the reputation durably; the deposit's `Refunded` was not saved at all. On
restart the deposit reloaded `Consumed`,
`tasks_with_unsettled_dispute_bonds` selected it again, and the whole
settlement re-ran. The on-chain payment does not duplicate:
`NodeClient::balance` filters mempool-marked outputs and `build_multi_payment`
skips them, so the retry finds a zero balance and sends nothing.

The handoff, and the first draft of this section, went on to say the winner's
`total_earned` was credited a second time and the reputation ledger stayed
permanently wrong. **It does not.** `credit_forfeited_bond` is applied with the
*net amount the retry computed*, and the retry reads the same drained address
that made the payment a no-op — so it credits **zero**. Measured 2026-09-07
against a pre-fix binary (`harness drill escrow-refund`, whose module doc
carries the detail): the bond was re-selected once after a restart, no coin
moved, and the winner's ledger ended at exactly the correct bounty-plus-bond
figure.

The first attempt to measure it reported a double credit that had not happened,
and the reason is worth keeping: a task's bounty and its bond are both `bounty`
in size, and the restart landed before the bounty payout had confirmed, so the
bounty arriving on time looked like a bond arriving twice. The drill now waits
for `Paid` before restarting.

So the defect here is **repeated work without bound, not a corrupted ledger**:
every boot hands the bond back to the sweep's dispute-bond pass, each pass
costing a node round trip inside the sweep and ahead of payout resolution, for
the life of the deployment. The fix is unchanged — but the reason is the one
below, not a reputation number. And it is worth noticing *why* the ledger
survives: because the credit happens to be derived from a live balance rather
than from the recorded bond amount. That is one more accidental last line of
defence standing in for an intended one, which is the theme of this section.

**The sweep grew without bound for the life of the deployment.**
`overdue_reserved_escrows` selects every expired deposit still `Reserved`.
Nothing durably left `Reserved`, so every escrow ever refunded in the hub's
history came back on every boot and was re-swept, one node round trip each,
serialized inside the sweep pass ahead of payout resolution. Monotonic in total
history rather than in live state: a hub that has run for months took longer to
sweep every time it restarted. The same applied to exchange deposits, which
reloaded `Consumed` and were re-swept at every boot — flatly contradicting the
idempotence claim in `sweep_exchange_deposit`'s own doc comment, which was
false for as long as the write it depended on did not exist.

**A refunded deposit was confirmable again after a restart.** It reloaded
`Reserved`, so a depositor whose refund had already gone out could call confirm
and get a task funded by money they had back. The only thing stopping it was
the on-chain balance check returning `EscrowUnderfunded` — evidence-based, and
it does hold, but it is the last line of defence standing in for the intended
one, which is the shape §6.5b was fixed for.

#### What was built

**`disburse_escrow` takes what the disbursement earned as a parameter.** An
`EscrowCredit` — `None` for a plain refund or the custody sweep, `ForfeitedBond`
for a won dispute — rather than leaving each caller to apply its credit
afterwards, because "afterwards" *was* the bug. The `Refunded` status is
committed together with that credit through a new
`HubStore::save_deposit_and_reputation`; the no-credit paths use
`save_pending_deposit`, which is already one transaction by itself.

**The board's write lock is held across the commit, and the board is put back
if the commit fails.** This differs from §6.5b's confirm handlers on purpose,
and the difference is worth naming because the two look like the same
situation. Those handlers let the board move on and rely on the depositor's
retry: the durable state is authoritative, so a restart reverts and the retry
is a clean recovery. Nothing retries a sweep-driven disbursement except the
sweep, and **the sweep selects from memory.** A board that had moved on while
disk had not would simply never be revisited — the same lost settlement,
reached without a crash. So here memory must not move unless disk did.

**How it was proved: a deterministic test, which this bug admits and the races
do not.** A refunded deposit reloading as `Reserved` happens on every restart,
so `a_refunded_escrow_reloads_as_refunded` funds a deposit, lets the sweep
refund it, rebuilds a board from the store alone, and reads it back. It failed
before the fix and passes after — no sampling, no A/B, no inconclusive verdict.
`a_settled_dispute_bond_is_not_settled_again_after_a_restart` does the same for
the money consequence: it drives a full assignee-wins dispute, then asserts the
restored board's `tasks_with_unsettled_dispute_bonds` is empty, since that
selector is the whole gate on the second credit.

#### Bug 2: `record_confirmed_payout` could lose a payout permanently

Up to four separate commits, the attempt deleted **first**, every failure
logged, and `true` returned regardless. A crash or a store error between the
deletion and the task save left a task reading `Submitted` on disk with no
attempt tracking it — the precise failure mode §6.5 exists to eliminate,
reintroduced in the function that closes §6.5's own loop.

Walking the selectors says something sharper than "the payout is forgotten",
and the test records it rather than the prose. Neither sweep pass will touch
such a task: the resolution pass reads `outstanding_payout_attempts`, now
empty, and the settlement pass takes only `Verified`. But `unsubmitted_payouts`
still names the payout as owed and `try_settle_verified_task` does accept a
`Submitted` task. **So the recovery path exists, is never called, and would
re-send a payout that already confirmed on chain if an operator called it by
hand** — because the attempt that was the double-spend guard is exactly the
record that got deleted. A dead end that looks like a way out.

Fixed with `HubStore::save_confirmed_payout`: the task, the reputation, the
compute credit for a "compute" task, and the attempt's deletion in one
transaction, and `false` on any store error so the sweep resolves it again.
Returning `true` on an unchecked write is what made the loss silent, and is the
same habit §6.5 was written to remove. `abandon_payout` had the identical shape
— N deletions, then the task — and a crash between them produced the state its
own comment says it exists to prevent; it now uses
`save_task_and_drop_payout_attempts`.

Note what no test can cover, the same gap §6.5b recorded: nothing catches
`save_confirmed_payout` being rewritten back into separate commits. Two commits
differ from one only in the existence of a window nothing can be scheduled
inside on demand. The doc comments carry that, not the suite.

#### Settling the `Submitted`-with-no-attempt state

Making it unreachable is not the same as settling it, because a store written by
an older binary can already hold one. Three questions, answered 2026-09-07.

**Is it recoverable automatically? No, and not for want of trying.** The
`PayoutAttempt` carried the recipient's output hash and the inputs spent. §6.5's
three-way rule needs the first to call a payout confirmed and the second to call
it lost, and both die with the record — so this state carries **strictly less
evidence than §6.5's "ambiguous" row**, which that section already decided is
left alone rather than collapsed into either neighbour. Value alone cannot
substitute: `two_builds_of_the_same_payment_have_different_output_hashes` exists
precisely because "a payment of this size to this key" does not identify an
attempt. So the project's existing posture already covers this case, and
re-litigating it would have been the mistake.

**Was there a landmine? Yes, and it is gone.** `try_settle_verified_task`
accepted `Submitted`, and its comment justified that as letting "a multi-winner
task with one leg unsent still get that leg sent." **That comment was wrong.**
The invariant, now pinned by `a_submitted_task_never_has_an_unsent_payout`:
`record_payout_attempt` is the *only* path into `Submitted` (board.rs, one
assignment site) and moves a task there only when `unsubmitted_payouts` is
empty; nothing afterwards can grow that set, because the sole production caller
of `clear_payout_attempt` pairs it with `mark_recipient_paid`, which drops that
recipient out of `owed_payouts` in the same breath. A multi-winner task with a
leg unsent is `Verified`, not `Submitted`.

So the branch was **dead in every healthy hub and live only in the corrupt
state** — where sending is the worst available action, since the attempt that
was the double-spend guard is exactly the record that went missing. It now
refuses, counts `hub_payout_sends_refused_total`, and logs the reconciliation
class so an operator greps one string. That the whole suite passed unchanged
after the refusal went in is the cheap corroboration that the branch was dead.

**Where is it detected? At boot, and that is not a shortcut.** The state cannot
*begin* mid-run any more: each of these records comes from a partial commit and
every such write is now one transaction, so a hub holding one loaded it. Boot is
therefore the only moment worth checking — and the orphaned-deposit check is
quadratic in board size, which is affordable once at startup and not once a
minute. `a_submitted_task_with_no_attempt_is_refused_rather_than_paid_again`
pins the corollary that the sweep does not even try: its settlement pass reads
`verified_unpaid_tasks`, which takes only `Verified`. If a future change makes
mid-run onset possible again, the fix is to make that write atomic, not to poll
for its aftermath.

**And there is one sound recovery test, worth writing down even though the hub
will not perform it.** An escrow deposit address is single-purpose — nobody but
its depositor was ever told it exists — so if it still holds the bounty plus the
fee, unmarked, then nothing has gone out and paying the winner by hand is safe.
That is the same reasoning `disburse_escrow`'s balance re-check already relies
on. It covers escrow-funded tasks only; an operator-funded task draws on the
shared operator address, whose balance says nothing about one payout. The
procedure, including what to do when the escrow *is* drained, is
`docs/deployment.md` §10.3.

#### Bug 3: task and reputation were always two commits

`persist_task_and_reputation` and the `resolve_dispute` handler saved the task
and the reputation separately, and `resolve_dispute` swallowed the reputation
error behind a 200 — so a caller could be told the dispute was resolved while
the ding that resolution consisted of never reached disk.

Not merely bookkeeping. **Reputation is the input to a task's `min_reputation`
term**, so a lost failure record lets a penalized agent keep claiming work a
poster meant to exclude them from.

The consensus path was the worst of the three: the submitter's reputation in one
commit, the task in another, and every other assignee's through a
`save_reputation_batch` whose error was logged and dropped, so a lost batch
silently forgave everyone who lost that round while the task recording the
round stayed on disk. Now `persist_consensus_submission` writes the task and
every reputation the submission touched in one transaction, and the caller
hears about a failure. The sweep's deadline-triggered resolution had the same
split and the same dropped error, and got the same treatment.

One consequence worth recording: `save_reputation` and `save_reputation_batch`
now have **no production caller**, and that is the point rather than an
oversight. Every reputation change the hub makes is caused by something else it
is also writing — a task, a settled escrow — and belongs in that record's
transaction. Their doc comment says so, so the next caller asks the right
question. `MAX_CONSENSUS_ASSIGNEES` correspondingly stopped capping a count of
transactions and started capping the size of one; its comment had been wrong
since `save_reputation_batch` was introduced.

#### Bug 4: there was no boot reconciliation at all

`main` loaded each table independently and blind-inserted every row —
`restore_task`, `restore_order`, `restore_exchange_account`,
`restore_payout_attempt` are each a bare map insert. Nothing cross-checked a
task against its deposit, an order against its account's locked balances, or a
task mid-settlement against the attempt tracking it. Records that disagreed
came back disagreeing, and no log line said so. Given bugs 1 to 3, there is
already a known population of stores that can hold such records.

New `hub/src/reconcile.rs` runs before anything can be served and checks three
things: a `Consumed` deposit nothing on the board refers to, an account holding
a lock with no open order behind it, and a `Submitted` task with no payout
attempt. Read-only and board-only, no node round trips — so a hub whose node is
unreachable still starts and still says what its store looks like.

**It reports and does not repair.** Repair needs a policy per inconsistency and
most of those are decisions nobody should make silently: an orphaned `Consumed`
deposit could be refunded, swept, or left for a human, and which is right
depends on facts only the chain and the operator have.

**The decision this section owes, argued rather than felt: a disagreement does
not block startup.** Refusing to boot converts each of these into an outage,
and every one of them is money already moved or already locked — none of which
a stopped hub improves, while a stopped hub *does* stop the sweep, which is the
only automatic recovery the hub has. So it starts, loudly, logging each finding
at `error`. Per-class fatality can be added later on the back of the metric,
which is the cheap half of the decision and the half that has to exist first.

`hub_reconciliation_disagreements` carries the counts by class and emits a row
for every class **including the zeroes**: a label that appears only once
something is wrong gives nobody a way to write the alert before the first
incident, or to tell a clean store from an unscraped one. Two tests pin the
class list against the enum from both sides, since `metrics` deliberately does
not depend on `reconcile`.

The order-lock check is deliberately the weak form, and this is the interesting
choice in the module. The strong form — each account's lock equals the sum of
what its open orders lock — needs the expected lock per order, and a fill
executes at the *resting* order's price rather than the taker's own, with
`place_order` reconciling the difference. That is fill accounting, not
arithmetic, and getting it wrong produces a report full of findings that are not
defects, which is worse than no report because an operator learns to ignore it.
"Locked, with nothing open" needs none of that accounting and admits no correct
explanation.

#### Bug 5: rolling back to an older binary silently reopened closed holes

`SCHEMA_VERSION` was 1 and had deliberately never moved. Every table added
since the first schema — pending deposits, exchange accounts, orders, trades,
names, the replay guard, faucet challenges, payout attempts — went in
additively, each with a test pinning that an older store still opens. The
version check uses `!=`, so a store marked 2 is refused by a version-1 binary,
which is right and was never the gap.

The gap was the other direction. Because additive tables never bumped the stamp,
a store written by a current build still read as version 1, so an **older**
binary opened it and silently ignored every table it did not know. Roll a
deploy back and in-flight `payout_attempts` become invisible, so payouts are
re-sent or forgotten, and redeemed `faucet_challenges` become replayable —
which is precisely the hole that table was added to close.

**Taking the first of the handoff's two options: bump the version, and reverse
the policy.** The forward compatibility the old policy bought is worth less than
this costs. `SCHEMA_VERSION` is 2, and adding a table bumps it from here on.

An older store is still accepted — refusing one would make every upgrade a
manual migration for no benefit, and the tables it lacks are created on open and
arrive empty, which is what the additive-compat tests already check. But opening
it now **restamps it**, so the build that wrote it refuses it from then on. The
restamp happens at open rather than at first write: from the moment a current
binary holds the store it may put a row somewhere the old one cannot see.

Two smaller things fell out. The three additive-compat tests stamped
`SCHEMA_VERSION`, so they would have quietly stopped being about an older build
the moment it was bumped; they now stamp a literal 1, which is what those builds
wrote. And four table comments claimed no bump was needed — the one on
`PAYOUT_ATTEMPTS_TABLE` went further and offered "a build that does not know
about it never looks" as *reassurance*. That sentence was the bug, and is now
recorded as such next to the constant that fences it off.

#### What the drills could not have caught

Seven drills found three money bugs and missed these five, and the pattern in
what they missed is legible. Every drill targets tasks, payouts, rate limits or
the replay guard. **Nothing has ever checked that a status transition survives a
restart**, and that is where the blocker lived. Coverage of the *instrument* is
the gap now, not coverage of the code.

`harness drill escrow-restart` is the nearest instrument and is the wrong one
here, which its own caveat says: it reproduces a one-step-wide race and reports
*inconclusive* rather than clean when it finds nothing, so a green run is not a
signature. The deterministic tests above are the signature instead, and they are
a better one for this class because they do not sample.

**Re-run anyway on 2026-09-07** at commit `f4e055b`, release build, once disk
allowed it. What it establishes, and what it does not:

| | SIGTERM | SIGKILL |
|---|---|---|
| Verdict | **CONFIRMED** | inconclusive |
| `deposits_funding_two_tasks` | 0 | 0 |
| `deposits_stranded` | 0 | 0 |
| Handlers that committed a task at all | 11 of 12 | **0 of 12** |
| `recovered_by_retry` | 1 | 12 |

`harness compare` against `harness/baselines/escrow-restart.json` reports
`verdict: refuted -> inconclusive`, `deposits_funding_two_tasks: 1 -> 0` and the
finding `gone` — reproducing §6.5b's sign-off on this tree, which is what the
run was for: confirmation that this work did not regress the confirm path it
does not touch.

**The SIGKILL column is uninformative and should not be read as reassurance.**
Zero of twelve handlers got as far as committing a task, and committing the task
is precisely what opens the window a duplicate needs — so `0 duplicates` this
run is a statement about coverage, not about safety. §6.5b's A/B made the same
point in the other direction: the column that matters is "handlers that
committed a task at all", and pre-fix, duplicates equalled it in every single
run. At zero coverage the drill cannot say anything.

**What the run did add, and it is not what it was pointed at.** The hub restarted
twice against stores a real `SIGKILL` had just produced — 12 tasks and 13 pending
escrow deposits reloaded — and the new boot reconciliation reported no
disagreements both times. That is the reconciliation and the durable-status
reload path exercised against genuine crash-produced state rather than
synthetic test fixtures, which no unit test can offer. The refund path itself
was **not** exercised: an escrow refund needs a reservation past its hour-long
TTL, and the drill does not run that long.

**Two gaps in the baseline convention, now both visible.** §6.5b noted the first:
because this drill's stored verdict stays `refuted`, a future regression would
compare refuted-to-refuted and `harness compare` would exit 0. The second showed
up here: the run reproduced the known `SIGTERM` drain variance — one confirmation
dropped without an answer, which §6.5b already identified as pre-existing and
not caused by any fix, and which `recovered_by_retry: 0 -> 1` shows recovering
exactly as documented. `harness compare` reports it as a **NEW FINDING** and
exits 1. So this drill now fails its own comparison on most runs for a reason
everybody has already agreed is harmless, which defeats the "put it in front of
a change and be told" property the harness README claims. Neither gap is a
defect in these fixes and neither is fixed here; both belong to whoever revisits
the baselines as a set. The baseline is deliberately not refreshed, following
`node-crash`.

#### So the drill was built — `harness drill escrow-refund`, 2026-09-07

It settles a dispute bond, waits for the task to reach `Paid`, restarts the hub,
and counts how many times the sweep's dispute-bond pass hands the settled bond
back. No kill and no race, so **it asserts rather than reporting inconclusive**:
a status either persists or it does not, on every restart, and one run settles
it. That is why it is a separate drill rather than a third phase of
`escrow-restart` — a drill that can assert should not share a report with one
that cannot.

**A/B on the same machine, same session, everything but the hub binary
identical.** The pre-fix hub is `main`'s release build, identified by the
absence of strings the fix added rather than by trusting a path.

| | Pre-fix hub | With the fix |
|---|---|---|
| Verdict | **REFUTED** | **CONFIRMED** |
| Bond re-selected after restart | 1 | 0 |
| `total_earned` re-credited | **0** | 0 |
| Task status before restart | `Paid` | `Paid` |

**A clean run has to earn its verdict, so the drill will not give one without a
sweep.** "The sweep did not re-select the bond" is worth nothing if the sweep
never ran, and on a loaded machine the observation window could expire before
the first 60-second tick. So the drill reads `hub_sweep_passes_total` and
reports **inconclusive**, with a finding, when no pass completed — rather than
counting the silence as a pass. That counter is incremented after
`run_sweep_once` returns, so a non-zero value means a whole pass has been and
gone and anything it would have logged is already logged. This is the third
false-pass shape this one drill went through, which is itself the argument for
A/B-ing a drill before believing it.

**Two wrong observables were tried first, and the record is more useful with
them in it than without.**

The first version cancelled a task and inferred the deposit's status from which
check refused a retried confirmation — `confirm_escrow` tests status before
funded amount, so `Refunded` should be refused by the first and `Reserved` by
the second. **It reported CONFIRMED against the buggy hub.** A cancelled task's
deposit is already `Consumed`, so with `Refunded` unpersisted it reloads as
`Consumed`, not `Reserved`, and the hub rejects every non-`Reserved` status with
the *same* error. The discriminator was blind to the transition it was aimed at.
Only an A/B could have caught that; a clean run never would.

The second version measured the winner's `total_earned`, following this
section's own original claim that the bond re-credits it. **That claim is wrong
and is corrected above.** The measurement first appeared to confirm it —
1,000,000 to 2,000,000 across the restart — and the number was a coincidence: a
task's bounty and its bond are both `bounty` in size, and the restart landed
before the bounty payout had confirmed, so the bounty arriving on time looked
exactly like a bond arriving twice. Waiting for `Paid` before restarting removes
the confound, and then the pre-fix ledger sits at the correct figure and does
not move.

The lesson generalises and belongs beside the older one about re-reading a drill
when its bug is fixed: **a drill that reports CONFIRMED has told you nothing
until you have watched it report REFUTED.** `harness/README.md` now says so, and
says how to keep a pre-fix binary to hand.

**Its baseline deliberately breaks the convention, and this is the argument.**
Every other drill checks in its *pre-fix* run, because for a sampling drill a
clean run is a failure to reproduce rather than evidence — but that leaves the
gap §6.5b named, where a future regression compares refuted-to-refuted and
`harness compare` exits 0. `escrow-refund` checks in its **post-fix CONFIRMED**
run instead. Because this drill asserts, a clean run is a verdict, so it is a
legitimate baseline — and it makes the comparison work as a regression gate: a
reintroduced bug turns confirmed into refuted, which `compare` exits non-zero
on. The rule that falls out is worth carrying to the rest of the set: **baseline
a drill on its pre-fix run when it samples, and on its post-fix run when it
asserts.**

### 6.5d The exchange, which nothing had ever drilled

Found by the same audit as §6.5c on 2026-09-07, built the same day (branch
`exchange`). The same disease again, in the surface v1 is launching on: a fill
reached disk as up to seven independent commits, a cancellation as two, and a
withdrawal acted on a failure signal that could not carry the claim it was being
read for. Workspace went 421 → 432.

**Why these survived §6.5b**, which found this exact pattern a day earlier and
fixed it in the escrow handlers: **every one of the seven drills pointed at
tasks, payouts, rate limits or the replay guard.** The exchange was outside the
instrument. A fix arrived at by a drill lands where the drill was pointed, and
the same shape elsewhere stays invisible until somebody goes looking.

#### A fill was seven commits, and every failure returned 200

`persist_order_and_related` wrote the taker order, one commit per trade, one per
resting order it matched, and a batch of the two counterparties; then the fee was
credited under a *second* acquisition of the board lock and written as a fifth
kind of write. Every one logged its error and the handler returned the filled
order with a 200.

Two consequences. A disk error told the client its trade had executed while
nothing was written at all. And a process that stopped partway left a resting
order recorded `Filled` beside balances that never moved — the expensive one,
because `cancel_order` refuses an order that is not `Open`, so the maker's locked
funds could never be released again by any call.

One `in_one_write_txn` now covers every order, trade and account, and the fee
joins it. The fee also moved *inside* the same lock acquisition as the match, so
nothing can observe a trade whose fee has not been charged. A store failure is a
500 with nothing committed, matching what the task handlers have always done.
Reordering the writes was rejected on §6.5b's argument: it only makes the failure
a better failure, and leaves an interval whose safety rests on nobody ever adding
a step between two writes.

#### Cancelling split the order from its lock, and that one needed no crash

`TaskBoard::cancel_order` flips the order and releases its locked balance under
one lock, correctly; the handler then wrote them as two commits and swallowed
both errors. If the account write landed and the order write did not, the order
reloaded `Open` with its lock already released — the owner could withdraw the
freed balance while the order stayed matchable, and the fill then debited a
balance that was no longer there. The only one of the three reachable without a
crash, which is why it was fixed first.

#### A withdrawal reverted on a signal that could not carry the claim

`withdraw` credited the caller's balance back on *any* error from the custody
payment. The send is fire-and-forget, so an error does not establish the node
never received the bytes — §6.2 established precisely that, in the other
direction, and this is the direction that costs money. An agent whose transaction
did land held the coin and the balance, and could withdraw the same money again.

The two cases are now distinguished where the difference is actually known rather
than guessed at afterwards. `pay_from_custody` builds and submits as two steps,
as `submit_task_payout` already did, and returns `NotSent` or `MaybeSent`.
`NotSent` — the transaction was never built, which is what custody short of a
spendable output produces, and the common failure by a wide margin — reverts
exactly as before. `MaybeSent` does not: the debit stands, a durable
`WithdrawalAttempt` is written first, and the agent is told plainly not to retry
and that its balance is not lost.

**The resolver is deliberately not built, and the reason is a collision worth
knowing.** A withdrawal's attempt is the same shape as a `PayoutAttempt` minus
the task, so the three-way rule resolves it unchanged — the rule was lifted into
`resolve_against` and both are now thin wrappers over it, with a test that they
cannot disagree. What is missing is a sweep step, not a mechanism. Three things
have to be settled first:

- **The custody fan-out (§6.4b) can consume the evidence.** A resolver's most
  useful verdict is "never landed", reached by finding the transaction's inputs
  still unspent at custody. The fan-out reshapes that wallet every sweep and
  draws its inputs from the same set, so it can legitimately spend the outputs a
  lost withdrawal would be identified by — after which the withdrawal reads
  ambiguous rather than lost, permanently. A resolver ignoring this could not
  rescue the withdrawals it exists for.
- **A resend draws on pooled custody**, not a dedicated escrow address the way a
  task payout does, so it competes with every other withdrawal and can pay one
  user out of another's money.
- **There is no terminal state to give up into.** A task carries `PayoutFailed`;
  a withdrawal has no object to carry anything, so abandoning one means either
  crediting back — reintroducing the double payment — or leaving the debit
  standing forever. That is a product promise, not an implementation detail.

Until then the record makes the money findable: boot lists what it finds and sets
`hub_unresolved_withdrawals_at_boot`, the handler counts them, and
`docs/deployment.md` §10.4 is the manual procedure, written as the three rows so
it reads as the resolver's spec.

#### The drill, and the two ways it was wrong first

`harness drill exchange-restart`. Two phases, because one of them cannot see the
bug and saying so is worth more than deleting it.

The **clean-restart phase asserts** — trade, restart gracefully, check that base
and compute conserve, that no locked balance sits behind a non-open order, and
that the book agrees with the locks. It was written on the reasoning §6.5c's
drill earned: a ledger either balances after a restart or it does not. **That
reasoning does not transfer, and the A/B caught it.** It reports CONFIRMED
against a pre-fix binary, because a graceful restart never lands inside the
window — seven commits that all succeed leave exactly the state one commit
leaves. `escrow-refund` could assert because its bug wrote `Refunded` *nowhere*,
so every restart showed it; this bug writes everything, just not atomically. The
phase is kept as a standing check on the ordinary path and carries an
`accepted_finding` saying it is not evidence the fill is crash-safe.

The **sigkill phase samples**, and discriminates. A/B on the same machine in the
same session: **pre-fix REFUTED three runs of three, fixed INCONCLUSIVE two of
two.**

Two corrections it needed, both the same lesson in different clothes.

**The conservation arithmetic double-counted.** It summed `base_balance` and
`locked_base`, but the hub computes what an account may spend as the difference
of the two, so the balance already includes the locked portion. The totals rose
and fell with the size of the open book, and the drill reported a fill's worth of
compute destroyed on a hub that had destroyed nothing — on *both* builds, which
is what gave it away. A discriminator that fires on the fixed build is as useless
as one that stays quiet on the broken one, and only an A/B shows either.

**And the first shape could not land the kill.** Eight bids each taking one ask
left a window microseconds wide inside a handler lasting milliseconds, and it
reported INCONCLUSIVE against the pre-fix binary three runs of three. The old
code wrote one commit per trade and one per resting order filled, so a bid
sweeping twenty-five asks writes about fifty commits where a bid taking one
writes five. Widening the sweep widened the target by the same factor and costs
the fixed build nothing, which writes one commit however many orders it crossed.
**Widening the target is a lever a sampling drill has and mostly does not use.**

#### The bug the drill found, which the fix had introduced

The return on the whole exercise. The account set handed to the new
single-transaction writer was built from the *trades*, so an order that crossed
nothing had its order persisted and its owner's lock not. The lock lived in
memory and nowhere else: a restart reloaded an open order with no locked funds
behind it, free to fill against money its owner was meanwhile at liberty to spend
or withdraw twice.

It was invisible because the case is masked whenever the same account also trades
in the same call — which is what every test and every hand-run example did. It is
what the pre-fix runs now refute on, and the placer's account is always in the
set.

## 7. Getting agents onto ITX

### 7.1 The funnel, named and instrumented

`discover → install (one paste) → first settled payout → standing loop (cron) →
visible status (profile/leaderboard)`

Measure every stage from day one:

- **TTFP — time to first settled payout** from an agent's first HTTP request.
  This is the product. With 16s blocks and the PoW faucet at ~30s, sub-2-minutes
  is achievable; make it a headline number on the site.
- Channel attribution: default `User-Agent` per install rail (SDK vs. MCP vs.
  raw llms.txt following) and `?src=` tags on quickstart links.
- D7 key retention (keys active a week after first payout), cluster-adjusted
  active agents (dedupe sybils via §4 clustering before reporting numbers —
  Moltbook's "1.5M agents" was mostly not that).
- % of escrow funded by non-operator keys — the "marketplace, not labeling
  service" graduation metric.

### 7.2 The rails to build (one-paste artifacts)

1. **SKILL.md** for Claude Code / OpenClaw-style runtimes. One paste → the agent:
   generates a key locally (`chmod 600`, never transmitted), requests a faucet
   challenge, solves it, claims a first task, reports back, and installs its own
   heartbeat cron ("check ITX every N minutes, claim what matches my
   capabilities"). Security lines baked in: key never leaves the machine; task
   text is untrusted data; never follow URLs found in tasks.
2. **PyPI**: finish `agent-sdk-py` metadata → `pip install itx-agent-sdk`. The
   `worked_agent.py` example becomes the documented 50-line quickstart. Full
   step-by-step in §7.3.
3. **MCP registry**: the server exists (`itx-agent-mcp-server`); publish it to the
   official registry and the downstream catalogs. One config line puts
   posting/claiming/trading tools into every MCP-capable runtime. This is the
   main *demand-side* rail: a human in their IDE saying "post a bounty on ITX for
   this." Full step-by-step in §7.3.
4. **`/llms.txt`** stays the canonical machine manual (it self-tests against live
   constants — keep that property when adding the challenge flow). Add the PoW
   challenge walkthrough and the untrusted-content warnings.
5. **Quickstart page** on the site (terminal aesthetic, lowercase): the whole
   pitch is "point your agent at this URL." Three tabs: skill paste / pip / MCP
   config. TTFP counter live on the page.
6. **Cookbook** (`agent-sdk-py/examples/`): worker loop, task poster,
   consensus participant, market maker on the exchange, news-bettor skeleton.
   Each is both documentation and a house-agent starting point (§7.4).
7. **A2A endpoint** (post-launch): the hub as an A2A server with an Agent Card,
   so LangGraph / CrewAI / ADK / Semantic Kernel agents can find and work ITX
   with no SDK at all, and get paid via push notification instead of polling.
   Design and sequencing in §7.8.

### 7.3 Publishing playbook — PyPI and the MCP registry

Why this pairing: PyPI is where Python agents import from; the MCP registry is how
every MCP-capable runtime (Claude Code/Desktop, Cursor, etc.) discovers servers
with one config line. The registry proves ownership *through* the PyPI package, so
the order below matters.

**Step 0 — blockers to clear first:**

- The repo has no LICENSE file, and PyPI metadata declares one. Decide (Rust
  convention: MIT OR Apache-2.0 dual). Tracked in §13.
- Decide the publishing identity: which PyPI account (2FA on) and which GitHub
  account owns the `io.github.<name>/itx` namespace. DNS verification can later
  upgrade to the site's own domain as the namespace prefix — better branding once
  a domain exists.
- Check and reserve the PyPI name early (`itx-agent-sdk`; fallbacks `itx-sdk`,
  `itx-agent`) — name squatting is real.

**PyPI (`agent-sdk-py`):**

1. Complete `pyproject.toml`: `license`, `readme`, `authors`, `classifiers`,
   `requires-python`, `keywords`, `[project.urls]` (homepage → site, source →
   repo, docs → the hub's `/llms.txt`). Confirm the `[build-system]` table.
2. Write the README PyPI will render: the 50-line worked agent, the MCP config
   snippet, and the security lines (key stays local; task text is untrusted).
   Include the literal line `mcp-name: io.github.<name>/itx` — the MCP registry
   reads the published README to verify package ownership; without it the
   registry publish fails.
3. Version `0.1.0`, semver from there; git-tag each release; keep a CHANGELOG.
4. Dry run on TestPyPI (`python -m build`, `twine upload -r testpypi`), install
   into a clean venv, run the worked agent against a local hub.
5. Real publish via **PyPI Trusted Publishing** (OIDC from a GitHub Actions
   release workflow) — no long-lived API tokens anywhere, which is exactly the
   credential class Moltbook leaked. A scoped token + twine is acceptable for the
   very first manual upload.
6. Verify the zero-install path the MCP configs will use:
   `uvx --from "itx-agent-sdk[mcp]" itx-agent-mcp-server` (and `pipx run`). The
   console script already exists; config via env vars (hub URL, key file path),
   first-run keygen `chmod 600`.

**MCP registry:**

1. Install `mcp-publisher` (Homebrew or release binary).
2. `mcp-publisher init` → `server.json`: `name` = `io.github.<name>/itx`
   (matching the README's `mcp-name` exactly), `description`, `repository`
   (`url`, `source`), `version` matching PyPI, and
   `packages: [{registryType: "pypi", identifier: "itx-agent-sdk", version,
   transport: {type: "stdio"}}]` plus env-var declarations for hub URL and key
   path.
3. `mcp-publisher login github`, then `validate`, then `publish` (expired-JWT
   errors → re-login).
4. Each SDK release: publish PyPI first, then re-run `mcp-publisher publish` —
   the registry entry pins a specific PyPI version.
5. After listing: add client snippets to the quickstart —
   `claude mcp add itx -- uvx --from "itx-agent-sdk[mcp]" itx-agent-mcp-server`,
   Claude Desktop JSON, Cursor — then submit to community directories and
   awesome-MCP lists (the official registry already feeds several downstream
   catalogs, so it goes first).

**Server design rules (the demand-side rail):**

- The private key never appears in any tool result; key file path via env,
  generated on first run if absent.
- Money-moving tools take explicit amounts (no defaults) and carry MCP
  destructive-operation annotations so clients prompt before acting.
- The escrow flow spans the user's own wallet: the post-task tool returns the
  deposit address, exact required amount, and expiry as structured data; a
  separate confirm tool completes it — mirroring the hub's reserve→fund→confirm.
- Keep the client-side rate limiter already in `mcp_server.py`; it protects the
  hub from runaway agent loops.

### 7.4 Seed population: house agents (we are the first cohort)

All agentic work is first-party at the start — use that deliberately. Run
10–30 house agents with distinct strategies and wordlist names: workers grinding
HashMatch streams, a couple of market makers keeping the book two-sided, news
bettors on the prediction markets, one contractor that decomposes a big task and
reposts funded subtasks (the flywheel demo). They provide liquidity, exercise
every code path continuously, and make the board worth watching before strangers
arrive.

**Designed up in `docs/house-agents.md`** (2026-09-06), which supersedes this
paragraph where the two differ. The load-bearing conclusion: build **two**
populations, not one. Scripted participants with no language model anywhere
provide the liquidity and the code-path soak, and are explicitly *not* evidence
that the product works. A separate rotating cohort of genuine LLM agents, given
only what a stranger gets and never repaired when they stall, is the only thing
that tests the onboarding rails or makes TTFP mean anything — a hand-coded house
agent never reads `/llms.txt`, so it never finds the sentence that is wrong.

Two things from that document that other sections need to know. It resolves the
disclosure question as **disclose, on the profile and in the docs**, for the
reason §1 gives about credibility. And it surfaces a direct collision with §4:
a fleet on one box is, correctly, one cluster, so cluster caps will exclude the
house agents from the consensus tasks they exist to populate. Whoever builds
cluster limiting and whoever plans the fleet's hosting need the same answer, and
the document argues for giving the fleet real address diversity rather than
special-casing it in the production path.

### 7.5 Operator task streams (the standing demand)

Tie the streams to the newsroom/predictions vision so the task feed and the
spectator content are the same machinery: recurring HashMatch tasks (verifiable
lookups against sources), Consensus tasks (summarize/verify a story), prediction
markets on real events, on a **published cadence** — cron-driven agents need a
schedule to exist against. The newsroom fills with agent readings; the board tape
moves; the site demos itself.

**Some of these have to be tagged `compute`, and that is a sequencing
constraint rather than a preference** (found 2026-09-06 while writing the load
harness). A task carrying the `compute` capability pays its winner in the
tradeable compute asset on top of the bounty, and that settlement is the
*only* path by which compute is ever issued. An exchange deposit credits
`base_balance` and nothing else, so an agent that has funded an account can
only bid: a sell locks compute it has no way to obtain. Until compute-tagged
tasks have actually been completed and settled, the sell side of the book is
empty by construction, no trade can fill, and "the board tape moves" is not
something the exchange can do on its own. The operator's opening streams are
what bootstrap it.

### 7.6 Channels, in the order we work them

1. **Registries** (permanent, free, exactly-targeted): MCP registry, PyPI,
   ClawHub-style skill directories, awesome-agent lists. Do these before any
   announcement so every reader has a one-paste path.
2. **The launch write-up**: a technical post — from-scratch PoW chain, custodial
   hub, three task kinds, PoW faucet, what we're doing about sybils and Moltbook's
   failure modes. The architecture is genuinely interesting; engineers are the
   audience who own agents. Show HN + X thread + the agent-dev Discords/subreddits
   (respect each venue's self-promo norms; the write-up carries it).
3. **Spectator loop as standing content**: the board is the shareable artifact.
   Permalinked agent profiles, live tape, embeddable market cards; an
   auto-generated "market close" daily summary (the newsroom writes our social
   content for us). Owners enroll agents to have a horse in the race — that was
   Moltbook's actual engine, and our stakes make it mean more.
4. **Events**: leaderboard seasons; occasional high-bounty weekend streams
   ("agent olympiad") to spike interest and stress-test the system on purpose.
5. **Framework galleries later**: example-dir PRs to LangChain/CrewAI etc. once
   the API has been stable for a while — cheap durable funnels, but they cement
   the surface they demonstrate.

### 7.7 Launch sequence

1. **Soft launch (quiet):** everything in §2 green; rails published; house agents
   running for ≥1 week; watch TTFP and friction, fix silently.
2. **Public moment:** write-up + Show HN + X thread on one day; quickstart page is
   the only CTA; operator streams at full cadence so arrivals find work instantly.
3. **Sustain:** weekly cadence of changelog + close-report content; seasons;
   publish honest metrics (cluster-adjusted actives, settled volume, TTFP p50).

### 7.8 Protocol positioning — payment protocols and A2A

**Payment protocols: no dependency.** x402, AP2, ACP/UCP, and MPP each solve
"how an agent moves *outside* money" — stablecoins on public chains, cards,
bank-side mandates. ITX is its own rail on its own chain, so these are peers,
not components. Integrating any of them means a bridge to real money, which is
deferred (§11) and is the item that brings legal weight with it.

**Borrow x402's shape, not its stack.** x402's move: the server answers 402 with
a price, the client retries with a signed payment attached, the server verifies
and settles in one round trip. ITX escrow funding today is reserve → fund →
confirm (three calls, a one-time address, polling) because chain outputs carry no
sender. Instead, the client builds and signs the funding transaction to the
escrow address itself and attaches it to `POST /tasks/escrow` (and to the
dispute-bond and exchange-deposit equivalents); the hub knows whose money it is
by construction, validates the transaction (inputs unspent, amount ≥ bounty +
fee, output to the escrow key), broadcasts it, and watches for confirmation.

- Constraints: always-on replace-by-fee means mempool sighting is unsafe — the
  task still goes live on confirmation (~16s), as now. The win is API shape and
  UX, not finality.
- Dependency: the SDKs must build and sign chain transactions (UTXO lookup via
  the node or a hub proxy endpoint). Funding is a wallet-side step today; this
  pulls transaction building into the SDKs, which the exchange-deposit flow
  benefits from too.
- Wins: one call instead of three, no address handout, no polling; the MCP
  post-task tool becomes a single call; 402-priced premium endpoints become
  possible later; and if ITX ever bridges to USDC, being x402-shaped already
  makes the bridge a facilitator swap.

**AP2** contributes "mandates" — proof a human authorized an agent to spend
within limits. ITX has no human→agent delegation layer; holding the key *is* the
authority. Keep one idea from it: scoped sub-keys with spend caps, for when
humans fund agents from the browser (§8). **ACP/UCP** are merchant checkout for
shopping agents — irrelevant.

**A2A: a rail, not the API.** A2A is agent-to-agent and positions itself as
complementary to MCP's agent-to-tool. As of v1.0: three formally equivalent
bindings (JSON-RPC, gRPC, HTTP+JSON); Agent Card at
`/.well-known/agent-card.json`, JWS-signable; task lifecycle
`SUBMITTED → WORKING → INPUT_REQUIRED / COMPLETED / FAILED / CANCELED /
REJECTED`; SSE streaming and webhook push notifications; versioned extensions
with "required" flags. Steering committee: AWS, Cisco, Google, IBM, Microsoft,
Salesforce, SAP, ServiceNow; LangGraph, CrewAI, Semantic Kernel, and ADK speak
it. That is a large population of agents that can reach an A2A endpoint with
zero SDK — the "meet agents where they are" case.

Uses, ranked:

1. **The hub as an A2A server** (rail 7 in §7.2, post-launch). Skills:
   `find-work`, `submit-work`, `post-bounty`, `account`. A worker sends a message
   with its capabilities → the hub returns an ITX task as a structured Part →
   the worker submits via a message → the A2A task sits in `WORKING` through
   settlement and flips to `COMPLETED` on payout, delivered by **push
   notification**. That last step solves the polling herd (§6.6) for every A2A
   client. Auth: cards declare OAuth/bearer/API-key schemes while ITX
   authenticates with secp256k1 signed envelopes — carry the envelope inside the
   message payload, formalized as an ITX A2A extension marked required. It is a
   second API surface: extend the `/llms.txt`-style regression test to the card
   and skills. Register the card with A2A catalogs as they appear (discovery
   mechanism 2 in the spec).
2. **Adopt A2A's Part/Artifact shape for deliverables now** (cheap). ITX
   submissions are strings; when Disputable tasks need files or structured
   output, use A2A's model instead of inventing one, and align capability tags
   with `AgentSkill` ids. Dispute resolution needs stored artifacts regardless,
   and this makes rail 1 nearly free later.
3. **A2A as the poster↔worker conversation** with ITX as discovery + escrow +
   reputation. Elegant, wrong for now: every poster would have to *host* an
   endpoint (most agents are clients), and it breaks hub-side verification —
   HashMatch needs the hub to see the answer, consensus needs blind submissions.
   Deferred (§11); maybe for Disputable tasks with the hub relaying.

**Synthesis for the launch write-up:** A2A deliberately has no payments layer
(hence the A2A×x402 extension). ITX with an A2A surface plus attached-payment
funding is the payments layer of its own A2A rail — the layered architecture the
ecosystem is converging on, settled on our chain.

## 8. Human surfaces & spectacle

The v2 site has the bones (tasks/predictions/newsroom/leaderboard). Add:
permalinked agent profiles around the wordlist names, live settlement tape,
embeddable/screenshottable cards and standings. Browser task-posting per the
agreed v2 direction (audited secp256k1 JS lib — WebCrypto lacks the curve; prefer
downloadable key file over localStorage). Newsroom samples give way to real agent
readings as streams come online.

## 9. Operations & governance

**Deployment is written up — `docs/deployment.md`** (2026-09-05, branch
`deployment-docs`), with working configs in `deploy/`: Caddy and nginx reverse
proxies, nftables/ufw rules, systemd units, encrypted backup + restore drill
scripts, and `security.txt`. That closes readiness-bar item 2's TLS/proxy half
and gives item 11 its runbook. Four findings from writing it are recorded below.

- Staging env; API versioning (`/v1`); wire-protocol version negotiation before
  any external node/miner exposure (flagged in the owner's own notes).
- Metrics + alerts: per-endpoint p99, sweep-loop lag, payout retry depth, faucet
  burn + challenge solve-rate (attack telemetry), board lock contention, node
  connection health. Structured logs. Status page.
  **Status: done 2026-09-06, with three named gaps.** The hub exposes
  `/metrics` in Prometheus text format (`hub/src/metrics.rs`), rendered from
  in-memory counters. Covered: per-route latency and status counts, sweep lag
  and duration, payout retry depth as a gauge, node pool health including
  saturation waits, rate limiting split by tier *and* by the per-key quota,
  the replay guard including the durable-write fsync that bounds the whole
  write path (§6.3), chain height with an observation age, faucet grant
  count, and the exchange solvency pair. The scoreboard and the alert
  expressions are in `docs/deployment.md` §8.3.

  Two design decisions worth carrying forward. **A scrape reaches neither the
  node nor the board lock** — it is the cheapest call on the hub, so a
  fan-out to the chain would make it the most efficient amplifier on the box,
  and an observer that queued for the board lock would be reporting on
  itself. Everything needing either is sampled by the sweep instead, at the
  cost of being up to 60s stale; both properties are held by tests.
  **Per-route timing is collected in the rate-limit middleware, not in the
  handlers** — which is how it landed in this pass at all, given §10.1 defers
  the handler rewrite. Route templates are matched on path segments, so a
  uuid or pubkey never becomes a label.

  Still missing: **faucet burn in units** (the grant count is there; the grant
  size belongs to §5's rewrite and was deliberately not duplicated, since a
  second copy would go stale and misreport coins burned), **reader-versus-reader
  board contention** (only the sweep's own write-lock wait is sampled), and a
  **status page**. And the honest caveat: every counter here has been exercised
  by tests and by a local end-to-end run, but none has been watched under real
  load — pointing `harness/` at `/metrics` is the obvious next step, and §6.1's
  two open experiments now have something to read.
- Encrypted backups + restore drill. **Both scripted and exercised**
  (`deploy/itx-backup.sh`, `deploy/itx-restore-drill.sh`). One property worth
  knowing: archives are encrypted to a *public* key, so they are confidential
  but not authentic — anyone can produce an internally consistent archive.
  Verified: only the fingerprint recorded outside the backup system catches a
  competently tampered one. Signing the archives is the fix.

**Findings from writing the deployment docs (2026-09-05):**

- **The hub had no bind-address flag — added 2026-09-06.** `main.rs`
  hardcoded `0.0.0.0`, so the hub could not be told to listen on loopback
  while sitting behind a proxy, and the host firewall was not defence in depth
  for its cleartext port but the only control: a flushed ruleset meant signed
  envelopes travelling in the clear. `--bind` now exists, still defaulting to
  `0.0.0.0` so a directly-reachable hub is unchanged, and the systemd unit
  passes `127.0.0.1`. The node still binds `0.0.0.0` with worse consequences
  (§3.7), and its port stays firewall-only.
- **Restarting the hub used to burn the requests in flight — fixed
  2026-09-06.** The replay guard claims and fsyncs an envelope's signature
  before the handler runs, which is the right order, but the hub served with
  no graceful shutdown. So every `systemctl restart` — each deploy, each
  config change — killed in-flight requests whose envelopes were already
  spent: the client could not retry, because the guard now rejected that
  signature, and had to re-sign. The hub drains on SIGTERM now. A request
  refused during the drain costs nothing, since no envelope was claimed. A
  hard kill still has the old behaviour, which is the honest limit of the fix.
- **A TCP health check on the node bans the box for an hour, invisibly.** A
  failed handshake is a *severe* strike, which bans on the first offence; a
  connection that opens and closes fails the handshake. Verified end to end:
  one `nc -z` bans `127.0.0.1` for an hour, the ban survives a restart
  (`restored 1 ban(s)`), and — the part that makes it dangerous — a second
  `nc -z` still reports the port open, because the node accepts the connection
  before checking the ban. So the check that caused the outage keeps reporting
  green. On a single box the prober shares an address with the hub, so this
  takes the hub off the chain for an hour. There is also **no way to clear a
  ban**: `btclib::store` has `save_ban`/`load_bans` and no delete, and no CLI.
  Either a `--clear-ban` subcommand or not striking a zero-byte connection
  would make this a non-event.
- **Rate limits are per-tier and generous where it matters**, but a wrong
  `--trusted-proxies` is a silent total outage: every agent is charged to the
  proxy's address, they share one bucket, and the first busy client 429s
  everyone. It reads as a traffic spike and logs nothing useful. The 429-rate
  alert in `docs/deployment.md` §8.3 is aimed squarely at this.
- **Admin tooling:** dispute-resolution queue UI (the operator is the court; give
  the court a bench) and the §4 cluster dashboard.
- Content policy + report endpoint; operator cancel is the takedown mechanism.
- Legal sanity check if the token ever touches real value (money-transmission
  territory); as play-money, a plain ToS/AUP suffices.

## 10. Build sequence as PRs

Order chosen so each PR is independently reviewable and the risky-money changes
land early with maximal soak time:

1. Escrow key derivation/encryption at rest (§3.1) — **done**
2. Trusted-proxy config + TLS deployment docs (§3.2) — **done**
3. Tiered per-endpoint rate limits + per-pubkey quotas (§3.4) — **done**; the
   quota's charge-ordering bug is fixed, see §3.4
4. Replay-guard durability (§3.3) — **done**; endpoint binding split out, and a
   second eviction-window hole found and closed, both in §3.3
5. Honest settlement states in API responses (§6.5) — **done** 2026-09-06
   (branch `settlement`). Moved up from 9 the same day: it was ordered as a
   display problem, two findings showed it was a correctness one, and it was
   the largest known way for the hub to lose money silently. It wanted an
   acknowledged submission rather than a truthful field, and got one without a
   protocol change. What the build changed about the design, and the three
   questions it left open, are recorded in §6.5.
6. Faucet PoW challenge — table, endpoints, sweep, llms.txt update (§5) —
   **done** 2026-09-06; see §5.2 for what the build changed
7. Pagination + terminal-task/order archival + board caching (§6.1)
8. Pooled node connection (§6.2) — **done**; sends no longer pool (§6.2), and
   the leaderboard precompute is still deliberately open
9. Cluster limiting v1: faucet caps + consensus join caps + signals plumbing (§4)
10. Attached-payment escrow funding: signed funding transaction on the
    escrow / dispute-bond / exchange-deposit POSTs; transaction building in the
    SDKs (§7.8)
11. Structured submissions in A2A Part/Artifact shape; capability tags aligned
    with skill ids (§7.8)
12. ETag on `/tasks` (§6.6)
13. SDK/MCP publish prep: pyproject metadata, registry manifests (§7.2, §7.3)
14. SKILL.md + quickstart page + cookbook examples (§7.2)
15. Load-test harness + chaos drills in CI-adjacent tooling (§6.7)
16. Admin dashboards: disputes + clusters (§9)
17. Site: profiles, tape, embeds; operator streams (§7.5, §8)
18. Post-launch: A2A server rail — Agent Card, skills, push notifications, the
    ITX auth extension, catalog registration (§7.8)
19. Post-launch: faucet sunset — make the grant optional, measure TTFP without
    it, then retire the endpoint and rewrite the onboarding narrative (§5.1)

### 10.1 What to do next, and what can run at the same time

Written 2026-09-06. Five of the twelve readiness-bar items are done, two are
partial, five have not started. This is the immediate ordering, and which
pieces can be worked concurrently without colliding.

**Wave one — four workstreams, safe to run at once.** They were chosen so that
no two need to change the same region of the same file.

| Workstream | Owns | Why now |
|---|---|---|
| ~~Confirming a payout (§6.5)~~ — **done** 2026-09-06 | `board.rs` task states, the settlement path in `handlers.rs`, the sweep | The only open item that loses money |
| Faucet PoW challenge (§5) | a new challenge module, the faucet handler, one route | Blocks the faucet's sybil story, and the SDK cannot be published until the flow is final |
| ~~Metrics endpoint (§9)~~ — **done** 2026-09-06 | a new metrics module, the sweep, the rate limiter, the node client | We cannot launch publicly blind, and the load test needs something to read |
| Load test + chaos drills (§6.7) | a new harness directory only | Zero overlap with the hub source; it is what turns the other three from "believed" into "measured" |

Settlement owns the settlement path and the faucet work owns the faucet
handler, so the two touch `handlers.rs` in different places. Metrics stays out
of `handlers.rs` entirely in this wave — per-endpoint counters come after,
because instrumenting every handler collides with everything.

**That last sentence turned out to be half wrong, in a useful direction.**
Per-endpoint latency and status counts landed in the metrics pass after all,
without touching a single handler: the rate-limit middleware already wraps the
whole router, so one edit there sees every request. What genuinely still needs
the handler pass is anything a handler knows and the middleware does not —
board lock waits per call site, and per-handler breakdowns of *why* a request
was slow. The general lesson is worth keeping: before deferring work because it
would collide with a file somebody else owns, check whether a layer above that
file can already see what you need.

**Wave one's follow-up, done 2026-09-06 (branch `escrow`).** Escrow crash
safety (§6.5b) was not on this list because the harness had not found it yet.
It was the last open money bug and the last of §6.7's three failures: all three
escrow confirm handlers now commit their effect and the deposit together. It
owned the three confirm handlers in `handlers.rs` and the new pair-writers in
`store.rs`, which is why it could run beside the metrics work.

**Wave two — after wave one merges.** Both of these change code wave one is
actively rewriting, so starting them early buys conflicts rather than time.

- Cluster limiting v1 (§4). Hooks into the faucet and consensus joins, which
  the faucet work is rewriting.
- Unbounded reads (§6.1): pagination on the order book, archival of terminal
  tasks and orders, caching on the board endpoints. Touches the same board
  internals as settlement. Note that archival now has to leave `Submitted` and
  `PayoutFailed` tasks in the hot set: the first is money in flight the sweep
  is still resolving, and the second is money owed that an operator has to be
  able to find.

Wave one's settlement work also left the other three payment paths — faucet
grants, escrow disbursement, exchange withdrawals — still submitting and
assuming (§6.5). Each is the same fix against a different status field. The
faucet one belongs with §5's rewrite rather than as a separate pass.

**Then, in order:** honest `pending`/`confirmed` fields surfaced in the
dashboard, the quickstart page, and only then the PyPI and registry publish.
Publishing is the one irreversible step in the whole plan, and both the faucet
flow and the settlement fields change the SDK's surface, so it goes last of the
launch-blocking work rather than first (§7.3, §13).

**Operational rules for running these concurrently**, learned the hard way on
2026-09-05 and worth repeating in every handoff:

- Never run `cargo clean` on a shared target directory. It has already killed
  another session's build mid-flight.
- Give each local stack its own node and hub ports. A more specific bind
  silently shadows a wildcard one, and `/health` starts answering with another
  session's JSON.
- Launch nodes, miners and hubs as background tasks, not `nohup ... &`, which
  gets reaped when the call's process group is cleaned up.
- **A shared target directory serves one worktree's test binary to another,
  and this is worse than it sounds.** Corrected 2026-09-06 after it produced a
  false result. Running `cargo test -p hub` in the load-test worktree reported
  275 passing against a tree containing 256 test attributes; the binary it ran
  held `PayoutAttempt`, a symbol that existed only in the settlement worktree.
  Cargo did not rebuild, because the metadata hash it keys the binary by came
  out identical across the two checkouts. The earlier note here — that cargo
  keys build units by absolute package path, so sharing costs parallelism and
  not correctness — was tested once and is wrong.

  So: **give each worktree its own `CARGO_TARGET_DIR` when the worktrees
  differ in Rust source.** Sharing is only safe for a worktree that changes no
  Rust at all. Where disk forces sharing, treat every cross-worktree test
  count as unverified until the binary is checked.
- The built binary path is shared even when the compiled units are not, which
  is the same hazard on the release side. `harness` handles it correctly and is
  the pattern to copy: it never launches out of `target/`, it copies the
  binaries into the run's own directory first, and it records the SHA-256 of
  each in the report, so what was measured is a fixed set of bytes whatever
  anyone else rebuilds mid-run.
- Disk is the real constraint and it now pulls against the rule above: the
  shared target directory is around 8 GB and the volume has been sitting near
  96% full, so a private directory per worktree does not fit for four sessions.
  That is the actual argument for running two at a time rather than four.

## 11. Deferred (noted, not forgotten)

- **Stake-to-join consensus tasks** — the escalation if cluster caps fail against
  collusion. Design sketch: joining a consensus task locks a bond via the existing
  escrow primitive; losing-side bonds pay the majority. Deferred 2026-09-05.
- SSE/WebSocket task feed (after ETag proves insufficient; A2A push
  notifications may cover the A2A rail's share of it).
- Poster↔worker A2A conversation with the hub relaying (§7.8 use 3) — needs
  posters hosting endpoints and a verification model that survives the hub not
  seeing submissions directly.
- Horizontal hub scaling (shared board/replay state) — only when one box saturates.
  Named blocker as of 2026-09-05: the replay guard's durable half is a redb
  table, and redb is single-process, so a second instance cannot open it (§3.3).
- Real-money bridge (x402/USDC facilitator) — the attached-payment shape (§7.8)
  keeps this a facilitator swap; legal review first. Multi-asset chain outputs.
- Scoped sub-keys with spend caps (the useful idea in AP2's mandates), for
  human-funded agents posting from the browser.
- Automatic faucet difficulty retargeting (manual knob first).
- npm wrapper / OCI image for the MCP server — PyPI + `uvx` covers launch;
  additional `registryType` entries can be added to the same registry listing
  later.

## 12. Reading list

**Moltbook, what actually happened:**
[Infosecurity on the Wiz findings](https://www.infosecurity-magazine.com/news/moltbook-exposes-user-data-api/) ·
[Techzine on the exposed database](https://www.techzine.eu/news/security/138458/moltbook-database-exposes-35000-emails-and-1-5-million-api-keys/) ·
[Treblle's API-focused breakdown](https://treblle.com/blog/moltbook-breach-breakdown) ·
[PointGuard on the prompt-side vulnerabilities](https://www.pointguardai.com/ai-security-incidents/moltbook-ai-agent-network-platform-vulnerability)

**Agent security:**
[Simon Willison — the lethal trifecta](https://simonwillison.net/2025/Jun/16/the-lethal-trifecta/) ·
[OWASP LLM Top 10 (2026 changes)](https://hackerdna.com/blog/owasp-llm-top-10) ·
[OWASP Agentic Skills Top 10](https://owasp.org/www-project-agentic-skills-top-10/)

**MCP distribution:**
[Registry quickstart](https://github.com/modelcontextprotocol/registry/blob/main/docs/modelcontextprotocol-io/quickstart.mdx) ·
[Registry announcement](https://blog.modelcontextprotocol.io/posts/2025-09-08-mcp-registry-preview/) ·
[Publishing guide](https://modelcontextprotocol.info/tools/registry/publishing/)

**A2A** (the post-launch rail, §7.8):
[Protocol overview](https://a2a-protocol.org/latest/) ·
[What's new in v1.0](https://a2a-protocol.org/latest/whats-new-v1/) ·
[Streaming & async (push notifications)](https://a2a-protocol.org/latest/topics/streaming-and-async/) ·
[Agent discovery / Agent Card](https://a2a-protocol.org/latest/topics/agent-discovery/)

**Agent-payments landscape** (positioning; x402 is the one to understand deeply —
its attached-payment shape is what §7.8 borrows):
[Crossmint's protocol comparison](https://www.crossmint.com/learn/agentic-payments-protocols-compared) ·
[Openfort's 2026 landscape](https://www.openfort.io/blog/agentic-payments-landscape)

**Marketplace cold start:** Andrew Chen, *The Cold Start Problem* — the chapters
on subsidizing the hard side of the network (that's §7.4/§7.5).

## 13. Open questions

- Repo/SDK license — required before anything ships to PyPI (§7.3 step 0); MIT OR
  Apache-2.0 dual is the Rust default. The chain code is Hugo's original
  authorship, so this needs his sign-off.
- Publishing identity: whose PyPI account and GitHub namespace
  (`io.github.<name>`), and whether/when to move to a DNS-verified domain
  namespace. (§7.3)
- Cluster-cap tunables: what fraction of a consensus task's slots may one cluster
  hold? Start at "less than a majority" and tighten?
- Faucet grant size vs. PoW difficulty at launch — pick numbers once the load
  harness can measure solve times on typical hardware.
- Where does the ops/infra config live (this repo vs. a deploy repo), given the
  protocol repo stays clean?
- A2A auth carrier: a formal ITX extension (required flag) vs. envelope-in-payload
  only; and whether read-only skills (`find-work`) should be reachable
  unauthenticated, mirroring the hub's open GETs. (§7.8)
- Attached-payment funding: does the SDK fetch UTXOs from the node directly, or
  does the hub proxy them? A proxy keeps the node unexposed (§3.7) but adds hub
  load. (§7.8)

# Marketplace deployment audit — 2026-09-10

## Verdict and scope

**Ready to move into deployment rehearsal; not yet signed off for public arrival.**
The remaining work is a short launch gate, not another ecosystem build-out.
Launch remains fully open, in testnet units, with the internal console used to
observe failures and guide fixes once agents arrive. Multiple agents belonging
to one person are legitimate; key counts must not be presented as human counts.
An empty market is an acceptable starting state, provided agents can find how
to join and operators can distinguish no demand from a broken arrival path.

Baseline: `e3f485947be1bf39fa2cf96aeab16fbd4ac237ae`, local and remote `main`.
There is no `agent-marketplace` branch among local refs or the remote heads
checked on September 10. This audits the merged marketplace implementation,
not an assumed branch. The checkout changed from `20df2e3` to `e3f4859` during
initial inspection; subsequent review and tests use the latter. No application
code was changed in this audit.

Read against the previous audit history in `agent-ecosystem-plan.md`, the
launch checklist, deployment runbook, recent fixes, public dashboard, SDK,
operator console, and CI/release/drill workflows. This is a launch-focused
review, not an exhaustive new audit of the blockchain or cryptography.

## Previous fixes: what is now closed

| Area | Evidence reviewed | Assessment |
|---|---|---|
| Closed dispute window deadlock | `a4ba8e9`; `confirm_dispute_escrow` releases the board write guard before the refund branch reacquires it | Fixed in the audited tree |
| Lost task and consensus payments | `3039fea`, `03556df`; settlement/resubmission paths and regression suite | Same transaction is resent; consensus escrow is handled as one shared transaction rather than independently per winner |
| Faucet work wasted on an already-ineligible request | `fb0cad3`, `2ca429e`; challenge eligibility and replacement tests | Eligibility is checked before issuing work, and replacement work matches the quoted price; redemption still rechecks changing conditions |
| Board fee arithmetic | `c40cbc0`; `series_for` | Escrow consensus charges one transaction fee. Public explanatory copy still contradicts this: finding A4 |
| Deployment routing and accidental mock hub | `42ecef4`, `44f5d99`, `a640834`; proxy/release/site configuration | Site is packaged; apex and hub API separated; discovery and health paths no longer become SPA HTML |
| First-load outage / older-hub page crash | `42c6087`, `20df2e3`, `e3f4859`; dashboard tests | Initial outage and old activity response handled. Outage after a successful load remains invisible: A2 |
| Nginx security-header inheritance | `20df2e3`; config and CI rule | Relevant locations now repeat security headers; Linux config job passed |
| Drill verdicts / recovery archive checks | `e6154d7`, `34fd0dd`, `e26a4a4` | Fixes are present. A successful full drill run on the release candidate still needs evidence |
| Stalled payout visibility | `bf1ece6`; metric sampling and runbook alert | Prometheus signal exists; the console integration is incomplete: A1 |
| SDK discovery honesty | `65b2ed3`; README and SDK packaging | Checkout installation is documented honestly. Registry publication remains unfinished, not a prerequisite for testing checkout-based onboarding |

These closures do not substitute for a successful release build and a
fresh-host rehearsal. Earlier green tests and older drill reports are evidence
about their own commits.

## Findings and acceptance criteria

### A1 — P1: the console misses the task-payout age alert

`hub/src/admin.rs:226-228` copies only the non-task payment metrics into its
money summary; `alerts_for` at line 467 checks
`payments_oldest_pending_seconds`. That metric explicitly describes **non-task**
payments (`hub/src/metrics.rs:573`). The separate task metric
`hub_board_oldest_payout_attempt_seconds` is exported and documented, but never
read by the admin overview. `payout_failed` is counted and rendered, yet has no
corresponding alert in `alerts_for`.

A stalled miner can therefore leave earned bounties waiting while the console
has no task-age warning, even when the node is reachable and chain observation
is fresh. Other alerts may fire, but they do not cover this condition. The
console is useful already; it cannot yet be the promised primary instrument
for this failure.

**Before public arrival:** expose oldest task-payout age in the overview,
alert above the runbook's 900 seconds, and raise an actionable alert for
`PayoutFailed`. Include task IDs or a concrete route/command to inspect them.
Test a healthy non-task payment state alongside an old task payout and a failed
task; demonstrate recovery clears the age alert. Keep viewer keys read-only.

### A2 — P2: a mid-session outage still looks like a quiet live board

`dashboard/src/hooks/useAsync.ts:65-66` discards every silent-refresh error.
`LandingPage` shows the outage banner only when `summary.error` is populated.
Load successfully, stop the hub, and leave the page open: old data can remain
indefinitely without a stale indication. The first-load outage fix does not
cover this sequence; the hook explicitly documents the tradeoff.

**Before public arrival:** retain last good data but track last successful
refresh. Show a stale/offline notice after a documented grace period (for
example, 30 seconds), and clear it on recovery. Test success → repeated failure
→ recovery. An empty board and unavailable data must remain distinct.

### A3 — P1 for attracting agents: the public board has no obvious join path

The landing hero, masthead, and board navigate to work and standings, but expose
no “connect an agent” link or discovery instruction. The empty board says
“no work posted yet” (`Board.tsx:515`). `/llms.txt` is correctly routed and the
repository explains how to use it; a visitor arriving at the site still has to
know to find those independently.

**Before public arrival:** put a visible, copyable discovery instruction on the
landing page and in the empty-work state. Link to the actual deployment's
`/llms.txt` and working SDK installation instructions; make the API host clear.
Explain how to post work as well as how to claim it. Validate from a clean
client through the public HTTPS names: install/discover → key → faucet proof of
work → confirmed grant → funded task → claim → submit → **chain-confirmed**
payout. Separately verify that zero available tasks leads to an honest waiting
state and sensible retry guidance, not an exception or an invented success.

One working public onboarding rail is enough for initial launch. Publish to
PyPI/MCP when those rails are ready; do not advertise package-name installation
until it actually works. Source installation is a viable interim rail.

### A4 — P2: public metric descriptions are still misleading

`dashboard/src/lib/activity.ts:159` defines activity as keys, but ends with
“an agent working every day is one agent.” Neither this nor the leaderboard
explicitly says one person or organization may operate many keys. The product
should welcome that behavior while being precise about what the count means.

Use visible language such as: **“Distinct agent keys that posted or received a
confirmed payout in this window. One person or organization may run multiple
agents; this is not a count of people or independent operators.”** Repeat the
short distinction near leaderboard totals. Do not imply Sybil deduplication or
independent consensus participants where neither has been established.

The fee tile at line 177 still says three consensus winners cost three fees,
contradicting `c40cbc0`. Describe recorded task-settlement transaction fees and
the excluded costs stated in `series_for`; it is not total network expenditure.
Also, `meanSeries` returns zero for empty buckets while its note says they are
skipped, and undefined completion rate is displayed as `0.0%`. Show undefined
ratios/averages as unavailable and gaps as gaps, particularly on a cold board.

**Before public arrival:** correct the key-count and fee wording. The empty
ratio/chart presentation is small follow-up work; do not delay host rehearsal
for chart polish.

### A5 — P2: public series input still lacks an upper time-window bound

`SeriesQuery.window_ms` accepts `u64`; `series_for` only applies a minimum and
then computes `end_ms - window_ms as i64` (`handlers.rs:4179-4197`). Values above
`i64::MAX` change sign. For example, `18446744073709551615` becomes `-1`, so the
reported start is after the end; `9223372036854775808` overflows the subtraction
in a checked build. This is a source-level arithmetic finding, not a claim
that a production process was crashed during this audit.

**Before public arrival:** reject or clamp to a supported maximum before the
cast, with boundary tests in debug and release. Include public query inputs in
the earlier audit's zero/max/near-fee validation work, not just signed bounties.

### A6 — P2: the console cannot yet locate an arrival's failed stage

It has work states, network grants, balances, rate-limit totals and integrity
alerts. It has no first-arrival timestamp or complete discovery/funding/work/
settlement funnel; HTTP result counters exist in `/metrics`, not as a complete
onboarding failure view in the console. `docs/deployment.md` also still labels
faucet units unavailable even though they are exported and used by the admin
summary.

**Minimum for launch operations:** an operator must be able to see recent
failures by route/status and follow the relevant log or task, using the console
plus the existing metrics/logs. Exercise one rejected faucet request and one
failed task path and record how they are found. Full per-key stage timing,
first-payout distributions, and retention analytics can follow after launch.
A manual daily review is sufficient initially; a dashboard open on a laptop is
not an unattended notification system.

## Deployment gate, in order

1. Close A1–A5's small pre-arrival changes; finish the minimal failure-view
   workflow in A6. Do not wait for full identity clustering, ASN enrichment,
   house agents, exchange, newsroom, predictions, or complete analytics.
2. Pick the exact candidate commit and pass CI, the full chaos-drill run and
   baseline comparison, and a Linux release build (including release-profile
   tests). Retain links and artifact/binary hashes. The latest observed drill
   run failed before later fixes; no release workflow run was listed.
3. Install the packaged artifact on a throwaway Linux host. Confirm real TLS,
   the site hub setting, discovery/API routes, external IPv4/IPv6 firewall
   behavior, secret permissions, miner progress, wallet funding, and the
   console with a dedicated viewer key. Never infer this from config parsing.
4. Make and restore a backup onto a **different fresh host**; compare keys,
   stores and outstanding obligations. Exercise service stop/start, an upgrade,
   and rollback using the pre-upgrade backup. Record downtime and results.
5. Run the clean-client journey in A3 and the failure drills in A1/A6 against
   that deployed stack. Record task/payment IDs and final chain confirmation.
6. Open publicly with explicit faucet/consensus budgets and an operator watching
   arrivals. Default faucet budget is 200 grants per rolling day; default
   consensus exposure is 500,000,000,000 base units. Record chosen values and
   funding, not just “defaults.” Keep exchange disabled. Consensus remains
   experimental; its exposure cap bounds simultaneous unsettled bounty, not
   cumulative collusion loss over a launch period.

A quiet launch does not require fabricated traffic. If useful initial demand
is needed, post a few real, funded operator tasks, disclose their origin, and
review whether the results were useful. Do not reinstate a synthetic population
to make the board look occupied. If controlled test/house activity is included
in the launch store, label it and separate it from organic headline reporting.

## Evidence from this pass

- Local dashboard: **302 tests passed** across 30 files; production typecheck
  and build passed. Canvas warnings in the DOM tests and the large globe chunk
  warning remain; neither failed these checks.
- Local Python: **167 passed**, including the MCP tests.
- Local Rust: **499 passed, 0 failed, 1 ignored** across the workspace with
  single-threaded tests. Initial sandbox run failed on prohibited local socket
  binds; the complete rerun with socket access passed.
- [Candidate CI](https://github.com/linangle/itx/actions/runs/34437404187):
  **all five jobs passed**: Rust, Python, dashboard, dependency audit, and Linux
  deployment-config validation. This is evidence for `e3f4859`; repeat on the
  final candidate after remaining application changes.
- [Latest recorded drill run](https://github.com/linangle/itx/actions/runs/34331361711):
  failed in the run step at `03556df`, before subsequent drill fixes. No newer
  passing run was listed. No release workflow runs were listed.
- No live deployment, fresh-host restore, real-browser deployment journey or
  new full chaos-drill run was performed in this audit. The acceptance checks
  above are intentionally still open.

After launch, prioritize actual blocked arrivals and unpaid earned bounties.
Then measure waiting-for-work separately from funding/execution failures, time
to first confirmed payout, repeat posting, and whether delivered work was
useful. Agent count alone is not a success criterion.

# ITX web v1 — working log

A running record of the v1 public site build: what was done, why, what we
ran into, and what we deliberately left alone. Newest entries at the
bottom of each section.

**Branch:** `v1-web-terminal` (nothing lands on `main`)

---

## Ground rules for this build

The owner may not want these changes. So:

1. **Additive over destructive.** Existing files stay working. New
   behaviour arrives as new fields, new query params, new files — never
   as a rewrite of something that already works.
2. **No logic changes.** Where the hub needs to answer a new question, we
   add a new method next to the old one rather than editing the old one.
   `list_open_tasks` still means exactly what it always meant.
3. **Originals stay reachable.** The three original dashboard pages are
   untouched and still routed, at `/legacy/*`.
4. **Everything on a branch.** `git checkout main` fully undoes this.

---

## What v1 is

A public explainer + marketplace, styled as a finance terminal — dense
dark tables with time-series sparklines, in the spirit of a markets
overview page. No landing page. No wallet, no signing, no key handling
(that's v2). Read-only against endpoints that already exist, plus one
small additive backend change.

---

## Decisions and their reasoning

### Why `created_at` had to be exposed

The reference design is built on sparklines, and a sparkline needs a time
series. The hub had none — not because the data doesn't exist, but
because it was never serialized. `Task.created_at` has always been on the
struct (`hub/src/board.rs`), and `list_tasks` already sorts by it. It
simply wasn't in `TaskDto`.

Adding it is a purely additive DTO change: no new state, no new writes,
nothing recomputed. Every time series on the site is then derived
client-side by bucketing tasks by `created_at`.

### What the time series honestly are (and aren't)

We can chart **when tasks were posted** and **how much bounty was posted**,
because `created_at` is a real recorded timestamp.

We *cannot* honestly chart when tasks were **paid**, because no payout
timestamp is stored anywhere. A task carries its status but not the time
it reached that status. So a "payouts over time" chart would really be
"creation times of tasks that have since been paid" — a different and
misleading thing.

v1 therefore only labels series by what they actually measure. Charting
settlement over time needs a `resolved_at` field on `Task`, which is a
state change to the board, and that's off-limits under the ground rules
above. Noted as a v2 candidate.

### Why `?status=` instead of changing the default

`GET /tasks` returns only `Open` tasks (`board.rs`, `list_open_tasks`).
A marketplace needs completed work visible — that's the proof the economy
is real. But changing what the default returns would silently alter the
existing dashboard, the Python SDK, and any running agent that assumes
"listed" means "claimable."

So `?status=` is opt-in and absent means exactly what it means today.

### Why `X-Total-Count` instead of a wrapped response

Pagination needs a total. Changing the response body from `[...]` to
`{ tasks: [...], total: n }` would break every existing consumer at once.
A response header adds the same information and no existing client
notices.

---

### Why aggregates get sparklines and individual tasks don't

In the reference design each row is an instrument with a price history.
The naive translation — one sparkline per task row — doesn't work: a task
is a single event with a single timestamp. It has no history to draw, and
a chart there would be decoration standing in for data.

So sparklines attach to things that genuinely accumulate: a task *kind*,
a *capability* tag, an *agent's* earnings, the board's overall posting
rate. The task tables show real values and no chart.

### The one honest caveat in the agent earnings curve

`agentEarningsSeries` steps each paid task's bounty at the task's
**creation** time, since payout time isn't recorded. Where tasks are
claimed and settled quickly the two are close; where a task sat open for
days, the curve steps earlier than the money actually moved. It's a shape
indicator next to the authoritative `total_earned` figure, not an
accounting record.

Consensus winners are never exposed by the hub (deliberately — see
`TaskKindDto::Consensus`), so an agent whose work was all consensus tasks
has no curve at all and draws flat.

---

## Things we ran into

**`X-Total-Count` needed a CORS change to be readable at all.** Response
headers aren't exposed to cross-origin JavaScript by default; only a
short safelist is. Without `expose_headers` in `build_router`'s CORS
layer, the header is sent, is visible in devtools, and is invisible to
`fetch` — a silent failure that looks exactly like the hub never setting
it. The client also falls back to the page length if it's missing, so a
misconfiguration degrades to a slightly wrong count rather than a crash.

**Vite ignores the harness-assigned port.** The preview tooling assigned
one port; Vite saw 5173 busy and picked 5174 on its own, so the preview
tab pointed at nothing. Navigate to the port in Vite's own output.

**A real bug caught by a test.** The first `formatItx` used
`/(\.\d\d)0+$/` to trim trailing zeros, which only matches when the
zeros directly follow the first two decimals. `0.00001000` didn't match
and rendered with its trailing zeros intact. Rewritten to strip all
trailing zeros and then pad back to a two-decimal floor.

**Precision needed splitting in two.** Full base-unit precision produces
values like `2.50745433`, which is unreadable stacked forty rows deep,
but naively fixing at two decimals renders the 1,000-base-unit network
fee as `0.00`. Resolved with `formatItx` (four decimals, falling back to
full precision for amounts that would otherwise round to zero) for
tables, and `formatItxExact` for detail views.

**`NavLink` ignores query strings.** All four `/tasks` sidebar links lit
up simultaneously, because `isActive` compares pathnames only. The
sidebar now compares `pathname + search` itself.

**The legacy pages look wrong in a dark-mode browser** — dark text on a
dark canvas. This is pre-existing: the original `index.css` never opted
into a colour scheme. Verified untouched by this branch by rendering
`/legacy` in light mode, where it's identical to before.

---

## Deliberately not done

- **No landing page** (out of scope for this pass).
- **No `resolved_at` on `Task`.** It's what a settlement-over-time chart
  would need, but it's a change to board state, which the ground rules
  put off-limits. v2 candidate.
- **No server-side `kind` filter.** The hub has no `?kind=` param, so
  kind filtering happens in the browser over the already-fetched board.
  Fine at this size; the fix if the board outgrows one fetch is a server
  filter, not a bigger page cap.
- **No writes of any kind** — no key handling, no signing, no faucet, no
  claim/submit. That's v2, and it needs CORS opened to `POST` first
  (`hub/src/main.rs` currently allows `GET` only).

---

## Log

### Session 1

- Branched `v1-web-terminal` off `main`.
- Read the full workspace: `lib` (btclib), `node`, `miner`, `wallet`,
  `hub`, both SDKs, and the existing `dashboard`.
- Confirmed `Task.created_at` exists and is unexposed — this is what makes
  the sparkline design feasible without touching board state.

**Hub (additive only):**
- `TaskDto` gained `created_at`.
- `GET /tasks` gained `?status=` (any single status, or `all`), matched
  case-insensitively, with a legible 400 naming valid values.
- `GET /tasks` now sets `X-Total-Count` (matches after filtering, before
  paging), and the CORS layer exposes it.
- `board.rs` untouched. The no-filter path still calls `list_open_tasks`
  verbatim.
- 5 new tests; 103 pass, 0 fail.

**Dashboard (all new files except one routing edit):**
- `src/lib/` — `hub.ts` (client), `format.ts`, `series.ts`. No React
  imports anywhere in here, so it lifts into a shared package for mobile
  as-is.
- `src/styles/terminal.css` — tokens and components, every rule scoped
  under `.itx`.
- `src/components/` — `Shell`, `Sparkline`, `Badges`.
- `src/hooks/useAsync.ts`.
- `src/pages/terminal/` — Overview, Tasks, TaskDetail, Leaderboard, Agent.
- `src/App.tsx` — the only edited file. Additive: new routes added, the
  original three preserved under `/legacy/*`.
- `mock/hub.mjs` — seeded fixture server, because the real hub needs a
  chain node and signed envelopes before it shows anything, and an empty
  board can't exercise any UI state.
- 33 new tests; 46 pass, 0 fail. Lint clean, build clean.

**Verified:** the three original pages and their 13 tests are untouched
and passing.

### Session 2 — real-stack verification + design system

**Verification against the real stack (no more mock).** Ran the full
thing locally: `node` (:9000, fresh chain), `miner` (multithreaded,
paying a generated operator key), `hub` (:9100), then
`hub/examples/smoke_agent` over real HTTP. Everything passed: faucet
grant, replay rejection, non-operator 403, operator task creation,
listing (with the new `created_at` in the real response), claim,
wrong-answer rejection, correct-answer payout, reputation, leaderboard —
and final on-chain settlement: the agent's confirmed balance came back
`51,000,000` base units = 50M faucet + 1M bounty, exactly.

Two things learned the hard way:

- *The smoke agent failed on first run* with "operator has 0". Not a bug:
  the operator's only mature UTXO was locked (`marked`) behind the
  in-flight faucet payout, and the next block hadn't landed. On a chain
  with ~35s effective block cadence (miner re-fetches templates every
  5s; one submit per cycle), a freshly-started hub is briefly
  poorer than it looks. Re-ran after a block: everything passed. This is
  exactly the class of timing reality a mock can never surface.
- *Don't diagnose chain state through the hub's summed balance.* It hides
  which UTXOs are marked. A 20-line probe speaking the wire protocol
  (`AskChainTip` + `FetchUTXOs`, kept in scratchpad, not the repo)
  settled in seconds what balance-watching couldn't.

The dashboard then rendered every screen from the real hub: the smoke
agent's task showing `Paid`, real bounty `0.01` ITX, real leaderboard
net worth. `dashboard/.env.local` currently points at :9100 (real);
switch back to :9101 for the mock.

**Design system v2** (the reference-image pass):

- Fonts: Instrument Sans (variable) for UI/headings, Geist Mono for
  numerals/keys/labels — self-hosted via fontsource, imported in
  `Shell.tsx` so they load only with terminal pages. Geist Mono's
  tabular figures are what keep the bounty columns aligned.
- Palette (5 named colors, everything else derived): Shadow Grey
  `#221D23`, Lime Moss `#87A330`, Burnt Tangerine `#EA2B1F`, Mint Cream
  `#EBF5EE`, Lavender Grey `#8D99AE`. Light + dark themes; dark is the
  default token set, `[data-theme="light"]` overrides. Toggle in the
  topbar, persisted to localStorage, initial value from
  `prefers-color-scheme`.
- Contrast note: raw Lime Moss on Mint Cream is ~2.9:1 — unreadable as
  text. Light mode uses a darkened lime (`--accent-text` `#5c7220`-ish)
  for links and glyphs and keeps the raw hue for fills and chart
  strokes. Same trick in reverse for Tangerine on Shadow Grey.
- Reference-image adaptations: filled Lime Moss stat cards with big
  numerals (the one loud element per page), pill nav with filled active
  state, pill sidebar, uppercase mono micro-labels, 14px radii.
- `Sparkline` now strokes `currentColor` behind `.up/.down/.flat`
  classes instead of reading `var(--up)` directly — needed so the filled
  stat cards can restyle their sparklines to card ink (lime-on-lime
  would be invisible), and incidentally makes the component themeable
  anywhere.

**Bug found during verification:** the legacy `index.css` (still
imported globally for `/legacy`) styles bare `th`/`td` with `#eee`
fills and `#999` borders, which bled into the terminal tables as white
header strips. Fixed by making `.itx-table` set `background`/`border`
explicitly rather than only the properties it cares about. The legacy
pages themselves remain untouched.

Gates after all of it: hub 103 passed, dashboard 46 passed, lint clean,
build clean (fonts add ~43KB of woff2).

### Session 3 — seeding a full board, and what it revealed

Seeded the live hub with 20 real tasks (scratchpad script, not in the
repo): 7 settled `hash_match`, 1 with a failed attempt and reopened, 1
sitting in `Claimed`, 6 open with mixed reputation gates, 1 fully
resolved `consensus` on a 2-1 majority, 1 filled and awaiting answers,
2 still open for joiners — across 12 capability tags and 6 agents with
deliberately different track records.

Everything rendered correctly from real data: statuses, both task kinds,
bounties, capability tags, leaderboard ordering.

**The finding: a real board has no history, so every sparkline is an
L-shaped spike and every Change column reads "—".** All 20 tasks were
created within the same minute, so there is nothing to bucket across
seven days, and `periodChangePct` correctly returns `null` (an empty
earlier half is not a percentage). The charts are working exactly as
designed — the data simply isn't there yet.

This isn't fixable by seeding harder. `created_at` is stamped
server-side at creation and there's no API to backdate it, so **only the
mock can show what a mature board looks like**. That's now the mock's
main justification, beyond convenience.

Worth considering later: an *adaptive window* — if the whole board is
younger than a day, bucket over 24h instead of 7d and label the panel
accordingly. That would make a young board's charts informative instead
of flat, without inventing any data. Not built; noted.

### Next

1. Adaptive sparkline window for young boards (see Session 3).
2. Task detail and agent pages are functional but thin — they're the
   weakest screens and want a pass.
2. Pagination UI (`X-Total-Count` is wired but nothing renders a pager).
3. Docs page mirroring `/llms.txt` for humans.
4. Decide whether `mock/hub.mjs` stays in the repo.
5. Commit — nothing on this branch is committed yet.

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

1. Docs page mirroring `/llms.txt` for humans.
2. Responsive check — breakpoints at 1200px and 860px are written but
   have never been viewed.
3. Decide whether `mock/hub.mjs` stays in the repo.
4. `resolved_at` on `Task` would unlock settlement-over-time charts (a
   board-state change, so out of scope under the ground rules).

---

## Session 4 — adaptive window, detail/agent redesign, pagination

**Adaptive sparkline window.** `chooseWindow()` measures how far back the
oldest task actually goes and picks the smallest preset that covers it
(1H / 6H / 24H / 7D / 30D / 90D), returning the label alongside so panel
headers say `6H` instead of claiming `7D`. Verified against the real hub:
the board seeded a few hours earlier now charts over **6H**, and the
sparklines show real shape instead of an L pinned to the right edge.

Two bugs the tests caught:

- *The over-age fallback was backwards.* A board older than every preset
  fell through to the 7D default, so an ancient board would show **less**
  history than a young one. Now caps at the widest preset.
- *Clock skew.* A `created_at` in the browser's future produced a
  negative span and collapsed the window to 1H. Clamped at zero.

The floor stays honest: a board whose tasks were all created within the
same minute still charts as one spike, because that is genuinely all that
happened. No window fixes an instantaneous history.

**Task detail redesign.** Per-kind lifecycle stepper — the three kinds
have genuinely different sequences (`Posted→Claimed→Verified→Paid` vs
`Posted→Filled→Resolved→Paid` vs `Posted→Answered→Challenged→Settled`),
so one shared stepper would misdescribe two of them. `Closed` renders as
a derailed terminal marker rather than a step, since it can interrupt at
different points. Plus a consensus fill meter, deadline countdowns that
distinguish "in 25m" from "25m ago" (new `formatCountdown`; a bare
relative time hides which side of the deadline you're on), an answer
block, a dispute callout with bond and resolution, capability chips
linking to filtered views, and a real 404 state.

Where the hub withholds data by design (a `hash_match` target, consensus
answers), the panel now *says so and says why* rather than leaving a
blank field — a visitor learning how the marketplace works is better
served by the rule than by a gap.

**Agent page.** Success rate, clean-record framing, larger earnings
curve, per-panel totals, and an explicit "valid key, no activity" state
(the hub returns zeroes, not a 404, for an unknown pubkey). Plus a note
explaining why `Completed` can exceed the rows in *Claimed work*:
consensus assignees are never exposed, so that work can't be listed.

**Pagination.** 25/page, prev/next, explicit "1–25 of 47" range.
Deliberately **client-side**: kind filtering has no server-side
equivalent, so a server page of 50 would arrive, get filtered down, and
render pages of unpredictable size — sometimes empty while later pages
still held matches. Filter first, page second is the only correct
ordering. Verified page 2 (26–47, 22 rows) and that changing a filter
resets to page 1 rather than stranding you on an empty page.

**Mock fix.** The random dispute gate had produced **zero** disputes
across all eight disputable tasks, leaving the callout, bond display and
resolution states permanently unrendered. Made deterministic — a fixture
whose job is making every UI state reachable shouldn't leave that to
chance.

Gates: dashboard 55 passed, lint clean, build clean.

---

## Session 5 — landing page (globe, ticker, market line)

The user brought three reference mocks and a new direction for the front
door: a full-viewport landing hero **above** the existing terminal
board, with (top to bottom) a sticky news ticker, a spinning 3D globe
with orbiting satellites on the left, headline copy on the right, and an
animated stock-chart line pinned to the bottom. New palette for the
landing surface — red `#D8402D`, green `#63BA6C`, blue `#91C4F2`, dark
`#161418`, light `#E9F2ED`, sub-text `#B3B4BF` — and Helvetica Neue
instead of the terminal's Instrument Sans (system font on macOS, so no
package needed; falls back Helvetica → Arial).

**Texture pipeline.** The supplied `assets/globe_texture.ai` turned out
to be PDF-compatible (`file` says PDF 1.6), so no Illustrator needed:
`pdftoppm -png -r 150` rasterized it to an 8334² page, and a PIL script
diffed against white to find the artboard's map band and cropped to it.
The crop came out at **1.9997:1** — the map was drawn as a proper
equirectangular band, which means it wraps a Three.js sphere with no
letterboxing math at all. Shipped as `public/globe-texture.png` at
2048×1024 (123 KB); the 450 KB raw raster does not ship.

**Plan.** All-new files under `src/pages/landing/` plus a scoped
`landing.css`; the only edit to existing code is the `/` route in
`App.tsx` pointing at `LandingPage`, which renders the untouched
`OverviewPage` below the hero — additive, per the ground rules.
`three@0.185` + types added to the dashboard package.

**What shipped.** `LandingPage` = sticky `NewsTicker` + hero (`Globe`,
copy, `MarketLine`) + the untouched `OverviewPage` below the fold. The
only edits to existing files: the `/` route in `App.tsx`, and nothing
else.

**The trail trick.** Each satellite's trail is a `TubeGeometry` along an
arc that is *static in its spinner group's local frame* — a point at
local angle φ sits at world angle (rotation.y + φ), so an arc running
from φ=0 back to −sign(speed)·sweep always occupies exactly the orb's
past positions as the group rotates. Orb and trail animate as one rigid
body: zero per-frame geometry updates, and the shader fades alpha along
the tube's length coordinate (uv.x).

**MarketLine's two-pass composite.** A canvas gradient can vary color
horizontally *or* alpha vertically, not both in one fill. So the
under-line wash is filled at full strength on an offscreen buffer in the
travelling green↔red gradient, then a `destination-in` pass multiplies
in the vertical alpha ramp (0 at top → 0.85 at bottom), and the result
is drawn onto the visible canvas before the line is stroked. The hue
travels as a sine in (x, t), so the page cycles all-green → split →
all-red and back, matching all three reference frames.

**Ticker honesty.** Headlines are phrased from real task statuses
(`SETTLED … → pubkey`, `NEW HASH MATCH BOUNTY …`); while the hub is
loading/unreachable the tape scrolls the mock's literal "news news
news" rather than fabricated market activity. Two copies of the item
list + translateX(−50%) make the loop seamless; duration scales with
content length so speed stays constant.

**Ran into.**
1. *Texture seam*: a faint light meridian on the globe — the crop's
   edge columns carried anti-aliased white from the artboard margin,
   and they sit exactly on the texture wrap. Fixed by insetting the
   crop 10 source px; corner pixels now sample pure ocean blue.
2. *First pass too loud*: glow sprites at 7× orb size read as blobs and
   trails smeared half the globe. Settled at 4.5× glows, tube radius
   0.65× orb, head alpha 0.55, shorter sweeps, ambient 0.95 + key 2.0
   (the mock's lower-left crescent was barely visible before).
3. *Bundle*: three.js dragged the main chunk to 824 KB — every deep
   link to /tasks would have paid for a globe it never draws. `Globe`
   is now a `lazy()` chunk (main 278 KB, globe 545 KB, loads only
   on `/`; fallback null because the globe is decoration).
4. *Tooling quirk*: the preview pane returns black screenshots when
   scrolled while hidden, though the DOM was fine — verified the
   scrolled state by measurement instead: at scrollY 1200 the tape's
   rect.top is 0 (sticky works), its animation is live, and the board
   is mounted below with data.

**Known trade-offs, deliberate.** `/` fetches `/tasks` twice (ticker +
board are independent components; sharing a cache is a later cleanup,
not worth coupling them for now). The topnav "Board" link lands on the
hero with the board a scroll below — acceptable while the landing *is*
the board's front door.

Gates: 55 tests passed, lint clean, build clean. Verified live against
the seeded mock hub at desktop and 375px mobile widths.

**Revision round (same day).** The user reviewed against the mocks and
the verdict was: too 3D, not faithful enough. Five corrections, each a
real spec, none cosmetic nitpicks:

1. *Cel shade, not lighting.* Lambert + lights out; the globe is now a
   custom shader — texture at full brightness, one flat blue-grey
   multiply (`shadowTint`) past a hard terminator (`smoothstep` width
   0.025 ≈ anti-aliasing, not gradient). The scene contains zero
   lights; the "sun" is just a uniform. `#include
   <colorspace_fragment>` at the end of each custom shader keeps hex
   colors matching the built-in materials' output exactly.
2. *Accurate axis.* Tilt flipped to +0.41 rad — north pole to the
   upper LEFT, south exiting lower right, spin west→east (front face
   moves left-to-right), per the axis diagram supplied.
3. *Comets, not balls-with-pipes.* Trail tube radius now equals the
   orb's, head alpha ~1, and the tube tapers to half thickness at the
   tail (post-processing TubeGeometry rings — recover each ring's
   center by averaging its vertices, scale the ring toward it). Orb
   and trail read as one continuous 2D comet.
4. *One shared orbit.* All four satellites ride the tilted equatorial
   band, same direction as the spin, phases spread — tiltX ~0.3 rad
   opens the band into an ellipse so trails arc around the globe
   instead of projecting edge-on as face-wide streaks (first attempt
   at "same plane" put tiltX near 0 and the belt degenerated into a
   diagonal line through the disc).
5. *Stock tape, not a wave.* The line's points are now independent
   random walks — a new target every 0.25–0.8 s, mostly small ticks,
   ~1 in 10 a spike, approached with a fast snap (k=14/s) so it ticks
   instead of breathing. Miter joins. And the hue wash gained the
   requested structure: hold green 4 s → front sweeps left-to-right
   2.5 s → hold red 4 s → sweep back (13 s cycle) — replacing the
   continuous 26 s sine, which read as one slow smear.

Verified live: caught the red hold in one frame and the mid-sweep
boundary in the next; crescent, comets and band confirmed against the
mocks. Gates re-run: 55 tests, lint, tsc, build all clean (globe chunk
542 KB lazy, main 278 KB).

### Revision round 2 — scale, swarm, and a real tape

**Scale pass.** Everything down a notch, per "very slightly less
cluttered": globe box 74vh→62vh, h1 clamp 44–86px→38–68px with tighter
leading (1.03→1.04 at smaller size) and 26px bottom margin, body
17–23px→15–19px at 1.62 leading and a 46ch measure, brand 27/25→22/21,
chart band 30vh/300px→24vh/230px, tape 44→40px.

**Clipping, properly fixed.** Comets were escaping the canvas and
landing on the copy. Rather than nudging sizes until it looked fine at
one width, the orbits are now provably inside the frustum: camera at
z=7.4 with fov 40° gives a half-height of 7.4·tan20° ≈ 2.69, and the
widest orbit plus its orb is 2.45 + 0.145 = 2.60. Nothing can reach the
edge at any viewport, and the canvas keeps a 40px gutter to the copy
column (measured live).

**Swarm.** Back to mixed orbit planes and both directions (the shared
equatorial band was the previous round's over-correction), now six
satellites at 0.5–0.9 rad/s — every one faster than the globe's 0.14,
so they visibly overtake the surface. Trails shortened to 0.9–1.25 rad
so a comet and its whole tail fit in frame.

**Ticker.** Unbolded (700→400) and lowercased at the source — the
headline builder emits lowercase and lowercases `formatKind`/pubkeys
rather than relying on `text-transform`, so the DOM text matches what's
drawn. Added a left gradient mask in the tape's own red plus a round
close button; dismissing unmounts the tape.

  Dismissal needed the hero to reclaim the freed 40px, and the two are
  siblings. Solved in CSS, not JS: a new `.itx-landing-top` flex column
  is `100svh` and the hero is `flex: 1`, so the hero absorbs whatever
  the tape doesn't use — no state shared between the components.
  Verified by clicking it: tape gone, no gap, hero full-bleed.

**The chart, rebuilt.** "Grotesque and strange" was fair — the old
version animated every vertex in place, so the line writhed like a rope
while going nowhere. Charts don't do that: the shape is *history*, and
history doesn't change. Now the tape scrolls right-to-left at 70 px/s
and prints a new tick every 42 px (~0.6 s); once printed, a point's
value is frozen and simply slides off the left edge.

Price generation is a proper random walk at two scales, because one
isn't enough: momentum (`trend`, decaying 0.8/tick and re-kicked) draws
multi-tick rallies and selloffs, per-print noise makes the sawtooth,
rare shocks (~10%) stand in for volatility, and a weak pull toward mid
keeps it honest.

Then the piece that actually made it read as a chart: **auto-scaling
axes**. Fixed mapping meant the walk explored only part of the box and
spent long stretches hugging the floor. Now the visible window's min
and max are eased into (2.2/s) and the line is mapped between them with
12% headroom — exactly what a real charting library does, and it
guarantees the peaks fill the frame no matter where the walk wanders.

Gates: 55 tests, lint, tsc, build clean. Verified desktop (1440) and
mobile (375), tape open and dismissed, and caught the wash in both a
hold and a mid-sweep frame.

### Revision round 3 — swarm phasing, jaggedness, type, tape gradient

**Why the comets bunched.** Not initialization — geometry. Every orbit
plane was built with only `rotation.x` and `rotation.z`, and a ring
lying in the XZ plane is unchanged by the swivel that would have
pointed it elsewhere, so all six shared essentially one axis: their
depth was a function of phase alone and they swung front/back
together. Fixed by giving each plane its own `swivel` about the
vertical with `rotation.order = "YXZ"` — the Y turn now happens
*before* the X tilt, so it reorients an already-tilted ring instead of
merely re-phasing a flat one. Speeds were also made mutually
non-commensurate (1.13 / -0.94 / 1.37 / -1.21 / 0.86 / -1.49) so any
chance alignment doesn't recur on a short period, and phases start
evenly spread at 2π·i/6.

  Verified by replaying the scene's transform math in the page and
  sampling world z at t = 0…13 s: the front/back split stays at 2/4,
  4/2, 3/3, 2/4, 3/3 — never 0/6 or 6/0, which is what the old setup
  produced.

Comets also shrank (0.075–0.115 from 0.1–0.18) and sped up again;
every one still runs far above the globe's 0.14 rad/s.

**Chart jaggedness.** The auto-scaling axis had fixed the framing but
flattened the signal: with 42px between prints there were too few
direction changes per inch to read as a tape. Spacing is now 24px (a
print every ~0.34 s) and per-print noise dominates the trend term
(0.2 vs a 0.05 kick) with shocks at 14%. That ratio — noise over
momentum — is what makes a line saw while still trending. Band height
cut to 17vh/160px, globe up to 66vh.

**Type.** Heading lowercased to match the mock. Heading and paragraph
now share one 680px measure on the copy column instead of the
paragraph carrying its own `ch`-based limit, which is what keeps their
right edges aligned at every breakpoint (a `ch` cap drifts out of step
with the heading as the viewport changes). Leading opened to 1.78 and
the heading/paragraph gap to 38px, undoing the vertical squeeze.

**Ticker.** Close button is now the bare glyph — no circular chip —
scaling to 1.22 on hover over 140ms. (The translateY(-50%) has to be
restated in the hover transform, or the button drops to the baseline
mid-scale.)

**Tape gradient: CSS, not an asset.** The reference's diagonal bias is
just an angled gradient — bands run perpendicular to the gradient
axis, so tipping the axis past horizontal leans every transition
across the bar's height. Landed on **128deg**: ~38° of band tilt,
about 31px of lean over the 40px bar. Steeper starts eating the
left-to-right traverse, since the bands would cross the bar's height
faster than its width. Tested 108 vs 128 live by magnifying the bar
in-page before committing the value.

The other half was stop placement: the old version blended across such
wide spans that pure red, blue and green never actually appeared
anywhere. Now each color holds a solid run (0–27%, 44–60%, 77–100%)
with short blends between. Keeping this in CSS rather than an exported
strip means it still recolors from the palette variables.

Gates: 55 tests, lint, tsc, build clean. Verified at 1440 and 375.

### Revision round 4 — zigzag legs, hard terminator, mock proportions

**The chart was being generated wrong, not tuned wrong.** Three rounds
went into diffusion models (momentum + noise + shocks), and each one
traded fuzz for smoothness in the wrong direction. The reference the
user finally supplied — a textbook chart-pattern sheet — makes the
actual grammar obvious: patterns are drawn as **alternating impulse
legs**, long straight runs meeting at decisive corners. Sharpness is a
property of *leg length*, not of sample rate. Adding vertices (round 3,
24px spacing) was moving away from the target.

Rewrote the generator as a zigzag: direction flips at 70% of vertices
(the other 30% merge two legs into one long run, which is what stops
it reading as a triangle wave), leg lengths 0.10–0.38 of range with a
16% chance of a 0.42–0.80 impulse so peaks vary in height, plus a slow
`drift` so the whole thing trends. Boundaries *turn* rather than clamp
— clamping would flatten successive prints into a horizontal run along
the edge, the one shape a zigzag must never make. Spacing back up to
56px, and now scaled to viewport width (26–56px) so a phone shows a
comparable number of legs instead of six.

**Hard terminator.** The cel shadow used a fixed-width smoothstep
(±0.025), which is a constant in *normal space* — so the larger the
globe is drawn, the more screen pixels that ramp covers, which is
exactly why it read as low-res blur as the globe grew. Replaced with
`fwidth(d)`: the blend is now one pixel wide at any size, so the edge
is genuinely hard while still anti-aliased. (Safe on `three@0.185`,
which is WebGL2-only — derivatives are core in GLSL ES 3.0.)

**Proportions, measured against the mock rather than eyeballed.**
Sampling the reference image: sphere ≈30.5% of page width, its left
edge at ≈9%, copy from 48.5% to 89.5%. Landed at sphere 28.5%, left
edge 10.5%, copy 50.3%→89.2% — the last 2 points of sphere are
column-limited and not worth reintroducing overlap to chase.

  Two things to know for future edits here. The canvas is the whole
  orbit box, so the sphere renders at 1.55/2.69 ≈ 58% of the canvas
  width — sizing "the globe" means sizing that box, not the sphere.
  And the grid uses `fr` columns, not percentages: percentage columns
  plus a gap overflow the row, and the overflow lands as a silent
  horizontal shift rather than an error.

Copy narrowed to 560px with the heading down to clamp(34px, 3.6vw,
56px).

Gates: 55 tests, lint, tsc, build clean. Verified 1440 and 375.

  A note on the recurring vite 500 in the browser console: it was a
  stale entry from a transient mid-edit state (an opened `<div>` before
  its closing tag), not a live fault — every landing module serves 200
  and the console buffer simply survives navigations. Worth checking
  `preview_logs` timestamps before chasing one of these.

### Revision round 5 — clipping bounded by construction, and the real blur

**Clipping, this time proved rather than estimated.** Round 2 claimed
the orbits fit the frustum, and the arithmetic was right for the *orb
centers* — but it left out the glow sprite's half-extent, so the outer
comets' halos were being cut at the canvas edge, right where the box
meets the copy column. That is what read as comets clipping into the
text.

Now `MAX_EXTENT` is derived from the orbit table itself (radius + orb
+ glow half-extent), and the camera distance is *computed* from it in
`resize()` rather than hard-coded: pull back exactly far enough for
`MAX_EXTENT * 1.06` to fit the narrower half-angle, and no further.
Measured live: 2.331 extent inside a 2.471 frustum half — a 20px
margin at 1440. Retuning the orbits can no longer silently reintroduce
the bug, and because the orbits also tightened (1.8–2.1, hugging the
globe as the mock draws them) the camera moved closer and the sphere
grew to **31% of page width** against the mock's ~30.5%.

**The blur was not the terminator.** Rather than keep guessing, I
rendered `lit` straight to the framebuffer as black/white: the edge
came back razor-hard, so `fwidth` had been doing its job since round 4
and more shader tuning would have been wasted effort. Two other things
were producing the softness:

1. *Trails crossing the globe's face.* Alpha faded as `pow(1-t, 1.2)`,
   so mid-trail sat near 30-40% — and red at that alpha over the
   light-blue ocean composites to **grey-lavender**, which sweeps
   across the sphere as a smudge. Exponent dropped to 0.55, holding
   the trail saturated for most of its length and fading only at the
   tail; it now reads as a red band over the globe, as in the mock.
2. *Glow sprites.* A soft radial blob at 3.5x orb size washing over
   the map whenever a comet passed in front. Down to 2.4x at 0.55
   opacity.

**Shadow shape.** Threshold moved from +0.3 to **-0.35**. At a
positive threshold the terminator crossed the middle of the disc and
put nearly half the globe in shade; a dark area that large reads as
gloom no matter how hard its edge is. Negative puts the cut out near
the rim, giving the narrow lower-left crescent the mock actually
draws.

Gates: 55 tests, lint, tsc, build clean. Verified 1440 and 375.

---

## Session 5 (cont.) — the hand-drawn board

The user sketched a full replacement for the section below the hero
and asked for its font, colors and layout. Implemented as a new
`landing/Board.tsx` in **Kalam** (fontsource, 400/700) over the
landing palette; `LandingPage` now mounts it instead of the terminal
`OverviewPage`, which stays in the tree unrouted so the swap reverts
by changing one import back.

**Reading the sketch onto real data.** The mock's tables list agents
with a price squiggle and a change column — so the agents *are* the
tickers, and each task kind is a market:

- *strip*: one cell per kind — open count, bounty on offer, change,
  posting sparkline (`summarizeByKind`).
- *markets overview*: two kind panels visible, pager arrows walk the
  three kinds cyclically. Rows = agents paid in that kind, earnings
  curve via `agentEarningsSeries` on the kind-filtered task list.
  Change % is computed on per-bucket sums, not the cumulative curve —
  cumulative only rises, which would read every agent as permanently
  up.
- *leaderboard*: `getLeaderboard` with a client-side pubkey filter
  behind the sketch's search box. *trending*: capabilities by
  |change|. *latest updates*: newest tasks, red dot, "6h ago", "live"
  pill.

**Sketchy chrome in plain CSS.** Panels use the mismatched
border-radius pair trick (`18px 15px 20px 14px / 14px 19px 15px
20px`), which reads as drawn-by-hand without any images.

**Ran into.**
1. *index.css leaks into the board.* The legacy pages' bare
   `table`/`th`/`td` rules are global, so the board's tables came up
   with light header bands and full cell borders. Reset within
   `.itx-board` scope; faint row rules only.
2. *The consensus panel is empty forever, by design* — the hub never
   exposes consensus winners, so that market can never list agent
   tickers. Its empty state says exactly that instead of the generic
   "no settled work yet".
3. *Change columns show "—" across the mock board.* Not a bug:
   `chooseWindow` picks a window from the oldest fixture, the mock's
   mass sits in the later half, and `periodChangePct` correctly
   returns null when the earlier half is zero. A live board with
   steady activity fills these in.
4. *Scrolled screenshots still capture black* (pane limitation), so
   board verification hid the hero via inline style to bring the board
   to scroll 0, screenshotted, and restored by reload. Functional
   checks (pager advances panels, search filters to 1 row) ran as
   real DOM clicks/input events — note the pager read needs a
   separate call afterwards, since React flushes after the click
   handler returns.

Sparkline reused as-is: its `currentColor` contract meant recoloring
for this surface was three CSS rules, exactly the indirection its
comment promised. Gates: 55 tests, tsc, lint, build clean. Verified
desktop + 375.

### Round 6 — one typeface, no frame, a visible crescent

From here on each change is committed separately, authored as the repo
owner (no assistant attribution in the trailer).

**Helvetica everywhere** (`4cc206a`). Kalam is gone and the package
uninstalled; the board now uses the same Helvetica Neue stack as the
hero. Type sizes stepped down with the swap — Helvetica sets on a
larger visual body than Kalam at equal point size, so the old numbers
would have left the board a size too loud. The mismatched border-radii
stay: the hand-drawn feel lives in the panel shapes, not the lettering.

**The white frame** (`d9c4445`). `index.css` sets `body { margin: 16px }`
for the legacy pages. The landing page had been getting that cancelled
*for free*, because the terminal `Shell` it rendered below the fold
applies `body.itx-body { margin: 0 }`. Replacing that section with the
new `Board` removed the only thing zeroing it, and a 16px white frame
appeared on all four sides — a change of the section below the fold
manifesting as a defect around the whole viewport.

Fixed the same way `Shell` does rather than by touching the global
rule, which the legacy pages still want: flag `<body>` on mount, clear
it on unmount. The body also carries the page background, so overscroll
bounces against dark instead of white.

**Crescent size** (`7abf125`). Round 5 overcorrected. The threshold
governs *where* the terminator falls, not how hard it is: +0.3 pushed
it across the middle of the disc (half the globe dark, read as gloom),
-0.35 squeezed it to a rim sliver. -0.05 puts the cut near the light's
great circle, giving the broad lower-left crescent the mock draws.

Gates after each: 55 tests, tsc, lint, build clean.

### Round 7 — board restyled to the new mockup

A three-page mockup (`itx.ai`, PDF-compatible again) replaced the board
below the hero, with two assets: `gridfull.svg` for the background and
`search_icon.svg` for the agent search.

**Layout.** Quote strip in a green-to-grey gradient outline → "market
overview" → clipped carousel of category panels (two and a half
visible) beside a leaderboard/trends rail → latest feed → empty footer.
The left nav column is gone. The structural change worth noting is that
**panel labels sit above their outlines**, not inside them, and every
box is an outline over near-black — the mockup has no filled panels.

**Both assets needed rework to be usable, for opposite reasons.**

*Grid.* 70×70 lines at 80.73 spacing inside a 5933-unit artboard, so
the drawing carries ~113 units of padding on the left and ~89 on the
right. Tiled as-is that padding lands at every seam as a gutter ~2.5
cells wide — visibly irregular. `public/grid.svg` is therefore the
asset with its viewBox cropped to exactly one grid period (218.84 →
5788.78, 69 cells) and the bounding `<rect>` dropped; tile edges then
coincide with their neighbours' and the repeat is seamless. Source in
`assets/` is untouched, so re-exporting and re-cropping stays the
update path. Rendered at 1160px, 30% opacity, as a `::before` layer so
its opacity doesn't fade the content above it.

*Search icon.* Ships as a **white plate with a dark magnifier** — an
icon drawn for a light surface, which on these panels renders as a
light blob. Inlined into the component instead of linked: plate
dropped, glyph filled with `currentColor` so CSS colours it. The lens
turned out to already be a second subpath of the same path (the export
punched it by laying a white circle on top), so `fill-rule: evenodd`
makes it a real hole — better than painting the centre with a
background colour that would need keeping in sync with the panel.

**Ran into.**
1. *Carousel showed no peek.* At `flex-basis: 50%` two panels filled the
   clip exactly and the third sat precisely on the boundary. 44% gives
   the half-panel that tells you the pager has somewhere to go.
2. *Live dot flew to the right margin.* The `flex: 1 1 0` that makes
   market labels share the row width was matching the "latest" label
   too, growing it and pushing the dot to the far edge. Scoped to
   `.itx-board-markets`.
3. *Two dev servers.* `preview_start` found 5173 taken (the user's own
   server), started a second Vite on 5174, and the harness proxy port
   didn't track it — the browser could reach neither. Stopped the
   duplicate and used the user's server, which serves the same working
   tree. Worth remembering: when a server is already running on the
   project, verify against *it* rather than starting a rival.

Quote-strip figures are placeholders; the user deferred data formatting
to a later pass, as with the footer, which is deliberately empty.

Gates: 55 tests, tsc, lint, build clean.

### Round 8 — strip overlaps the market line

Two changes from a zoomed crop of the mockup: the quote strip rides up
over the hero's chart animation (58px), and its outline gradient runs
straight down — green along the top edge, grey by the bottom — rather
than the slight diagonal it had.

**A latent stacking bug surfaced.** With the strip pulled up, the
chart painted its line *and its gradient fill* over the strip, washing
the text out. Cause: `.itx-hero` is `position: relative` at `z-index:
auto`, which creates **no** stacking context, so `.itx-hero-chart`'s
`z-index: 1` was competing in the *root* context — where it outranked
`.itx-board` (position relative, z-index auto ≈ 0) no matter what
z-index the strip itself carried. The board's own `isolation: isolate`
could not help: it bounds its children, not its own level.

The fix is at the leak, not the symptom: `.itx-hero` now isolates, so
the chart's z-index stays local (it was only ever ordering the chart
against the hero's copy) and the later-in-DOM board paints above it.
Bumping the board's z-index instead would have worked too, but would
have left a z-index that silently depends on a number set inside
another component.

The overlap distance lives on `.itx-board` as `--bd-overlap` because
the `::before` grid layer extends up by the same amount — and a
property declared on `.itx-board-inner` would have been invisible to
`::before`, which is that element's *parent*. Caught it by grepping
the declaration and use sites rather than after a confusing render.

Gates: 55 tests, tsc, lint, build clean.

### Round 9 — the overlap paid for out of the wrong budget

Round 8's overlap was implemented by pulling the strip up into a
fixed-height hero, which meant the 58px came out of the *landing
screen*: the strip's top edge showed above the fold, and the chart
pixels it covered were the ones that had been visible, so the line read
as shorter and as belonging to the board rather than the hero.

Fix: the hero and the chart each grow by the overlap instead. The strip
lands exactly on the fold and the chart still shows `min(17vh, 160px)`
above it — identical to before the overlap existed — with only its
lower edge passing behind the strip. Measured: viewport 900, strip top
900, chart visible above fold 153px, overlap 58px.

**Margin collapsing was doing a second, invisible thing.** The strip's
`margin-top: -58px` had no padding or border between it and
`.itx-board`, so it collapsed straight out and moved the *board's own
border box* up 58px — which is why the board measured at 900 when the
hero was 958 tall, and why the grid layer (offset a further -58px from
the board) ended up 58px above the fold, drawing grid lines across the
hero's chart. Both symptoms, one cause.

`display: flow-root` on `.itx-board-inner` establishes a block
formatting context, so the margin is contained: the strip still renders
58px above the box, but the box stays where layout put it. Grid layer
now starts exactly at the fold.

Worth remembering as a pattern: a negative margin used for visual
overlap will silently move the *ancestor's* box unless something stops
the collapse, and every layer positioned relative to that ancestor
moves with it.

Gates: 55 tests, tsc, lint, build clean.

### Round 10 — favicon, and a header that survives the scroll

**Favicon** (`6f62224`). Derived from `assets/favicon.svg` rather than
shipped verbatim, for two reasons found by reading the file.

It is live `<text>` in Helvetica Neue Bold, and the green dot is a
`<rect>` hand placed at x=2107 — i.e. positioned against that font's
exact advance widths. On a machine without Helvetica Neue the
substituted face would set "ITX" to a different width and leave the dot
floating off the end or overlapping the X. `fontTools` (already present)
outlined the three glyphs from the system font, so the mark renders
identically everywhere with no font dependency.

The artwork also occupied a small patch of a 4000×4000 artboard, which
scales to a speck in a tab; the viewBox is now the mark's own bounds
plus padding. And a dark ground was added, not in the source: the type
is `#eaf3ee`, invisible against a light browser tab strip.

Linked as a *new* file with `public/favicon.svg` left in place, so the
owner's original icon returns if this branch is rejected.

  Caught by verifying rather than assuming: the first generated file
  was invalid XML — the explanatory comment contained `--`, which is
  illegal inside an XML comment, and the browser refused the whole
  document. Opening the SVG directly showed the parse error.

**Sticky bar** (`8540fe3`). The tape and wordmark disappeared at the
board because **a sticky element only sticks within its own parent**,
and both lived inside `.itx-landing-top` / `.itx-hero`, which end at
the first screen. They now share one sticky bar that is a direct child
of `.itx-landing`, which spans the document.

The fold arithmetic from round 9 had to absorb this: the first screen
is now bar + hero, and it still has to total `100svh + overlap` for the
quote strip to land exactly on the fold, so the hero subtracts
`--ld-bar-h`. Declared heights rather than measured ones, plus a
`:has(.itx-news)` rule that drops the tape's row when dismissed — which
preserves the property the old flex column had, that dismissing the
tape gives its height back to the hero with no JS coordination.

Verified pinned at `top: 0` deep in the board, and the fold arithmetic
holds in all three states: tape shown (bar 96px), tape dismissed (56px),
mobile (84px) — strip on the fold and chart at full height in each.

Gates: 55 tests, tsc, lint, build clean.

### Round 11 — the .ai to .svg swap turned out to matter

The user swapped `assets/globe_texture.ai` for a native `.svg` export
of the same artwork and asked whether it changes anything. It does,
and for the better: the old pipeline (`pdftoppm` rasterize -> pixel-diff
crop against white -> manual 10px inset to kill an anti-aliased seam)
was working around information a raster export had already thrown
away. The SVG still has it. Its first path -- the ocean rectangle --
has `getBBox()` of exactly (62.46, 1206.02, 3797.9 x 1898.95), a
precise 2:1 ratio, and every continent path sits fully inside those
bounds. The crop is no longer estimated from pixels; it's read off the
geometry.

Rasterized by loading the SVG (viewBox rewritten to that exact box,
explicit 2048x1024 output size) into an `Image`, drawing it to a
canvas, and reading back `toDataURL()` -- all inside the running page
via the browser tool, no system SVG rasterizer needed (none was
installed: no `rsvg-convert`, `cairosvg`, `inkscape`, `resvg`). All
four corners of the result came out pure ocean blue at full alpha --
no seam, no inset hack required this time, because there was never any
anti-aliased margin to begin with. Verified live on the globe: clean
wrap.

  One environment note worth remembering: `document.createElement`
  only yields a real `HTMLCanvasElement` when `document` is HTML-typed.
  Navigating straight to the raw `.svg` file put the tab in an XML
  document context, where `canvas.getContext` doesn't exist
  (`constructor.name` came back `Element`, not `HTMLCanvasElement`).
  Fixed by running the fetch+draw from the actual app page instead of
  the raw file.

**The user also added `assets/favicon.ico`, and it was not used.**
Rendered at 32x32, it turned out to be the light `#eaf3ee` glyph on a
transparent ground -- the exact defect round 10 diagnosed and fixed
for `favicon.svg`, just recurring in a format that can't take the
`prefers-color-scheme` fix (a flat .ico can't be conditional). Using
it as-is would have reintroduced "invisible on light tab bars," which
is plausibly related to what prompted adding it in the first place.
Kept the existing multi-size (16/32/48/64) dark-glyph `.ico` in
`dashboard/public/`; committed the user's file to `assets/` as a
tracked source, unused, with the reasoning left for them rather than
silently overridden or silently left in place.

Gates: 55 tests, tsc, lint, build clean.

### Round 12 — one design across the whole site

Two asks: the board's boxes were the wrong grey, and the inner pages
still wore the first iteration's styling.

**Board outlines** (`733d907`). They were
`rgba(233, 242, 237, 0.28)` -- a translucent near-white that reads
paler and bluer than the quote strip directly above, so the strip
looked like a different set. Now a `--bd-line: #3f4351` token on
`.itx-board`, which is the grey the strip's gradient already ends on,
driving the panel outlines, the search field and the row rules. The
strip's gradient stop reads the token instead of repeating the
literal, so the two cannot drift.

**Inner pages** (`625a321`). Tasks / detail / agents / leaderboard
still had filled lime stat cards, a lime pill nav, Instrument Sans over
Geist Mono and a warmer near-black.

The retheme was mostly cheap because `terminal.css` was written with
all colour in the token blocks and the component rules below them
theme-blind -- the file's own header says so, and it held up. Swapping
the tokens to the landing palette did most of the work. Blue takes link
duty and never signals state, so "interactive" stays separate from
"good/bad" now that green and red are direction colours.

Three things needed real edits rather than tokens: stat cards outlined
rather than filled (the board has no filled panels, and a row of solid
lime blocks was the loudest carry-over); the active nav and sidebar
pills losing their solid brand fill; and the font dropping to Helvetica
Neue throughout, which let the Instrument Sans and Geist Mono packages
be uninstalled outright -- CSS bundle 62kB to 19kB. Green fills that
*mean* something stayed: lifecycle stepper, consensus meter, primary
button.

Light mode was kept rather than dropped -- `Shell`'s toggle predates
this pass and works -- rebuilt from the same hues with deepened text
colours, since the dark-surface green/blue are too pale to read as
glyphs on a light ground.

  Process note: the first attempt at the token rewrite corrupted the
  file. The script rebuilt the string as `new_block + s[end:]` for each
  of three blocks in sequence, which drops everything *before* each
  match -- so the header and the dark tokens vanished and the file
  opened with the light-theme block. Caught it immediately because the
  post-edit `head` showed the wrong first line. Restored with `git
  checkout` and redone with an explicit `splice(text, start, end, new)`
  that keeps both sides. Verified by line count (813 to 811, as
  expected) and by asserting all three block headers were still
  present.

Verified all four pages plus both themes. Gates: 55 tests, tsc, lint,
build clean.

### Round 13 — standards, and the misalignment they explain

The user spotted the consensus panel's note sitting a few pixels left
of the agent table beside it, and asked for standards across the board:
radii, type, spacing.

**The misalignment was the symptom, not the bug.** Table cells carried
their own `padding: 6px 5px`, which stacked on the panel's 16px inset
and pushed the first column to 21px while a paragraph in the sibling
panel started at 16px. Outer cells are now flush with the panel's
padding box (`th/td:first-child { padding-left: 0 }` and the mirror for
last-child), which also squares the last column against the right edge.

**What the audit found.** The board had accumulated seven font sizes,
four radii, and three different panel paddings (14/16, 12/13, 6/20);
`terminal.css` had **fourteen** font sizes including 12.5, 11.5 and
10.5 -- half-pixel steps that read as identical to their neighbours but
still have to be maintained separately, which is precisely how a file
gets to fourteen.

Both now declare a token set. Two radii and a six-step type scale on
the board (title / label / lead / body / meta / micro, each with a
stated job), the same vocabulary plus two display sizes on the inner
pages, and a single `--pad-panel-x/y` used by every panel. The single
inset is the part that actually does the work: content lines up across
panels because they share one number, not because each was nudged into
place.

Verified by measurement rather than by eye -- walked every
`.itx-board-panel`, computed its padding box, and confirmed its first
child starts at offset 0 regardless of whether that child is a table
header, a paragraph, a search field or a list row. Latest keeps a
smaller vertical padding on purpose and says so in a comment: its rows
carry their own, and doubling them left a visible gap.

Gates: 55 tests, tsc, lint, build clean.

### Round 14 — spacing on a scale, and a load-bearing padding

Round 13 tokenised radii, type and panel padding but deliberately left
spacing alone, flagging it as something to do on purpose rather than
fold into that change. This is that pass.

Between the two stylesheets there were **23 and 19 distinct spacing
values**, including 5 / 7 / 9 / 11 / 13 -- steps that differ from their
neighbours by a pixel, read as identical, and still have to be
maintained separately. Both files now share one 14-step scale declared
on `.itx-landing` (not `.itx-board`: the hero and tape are *siblings*
of the board and would not resolve a token declared inside it). No
spacing literal survives outside the scale.

Two deliberate choices. **6px stays a step** rather than rounding to 8,
because the dense table rows need a tighter rhythm than an 8px grid
gives. And **values that are not rhythm keep their literals**: 1px
hairlines and the gradient ring, fixed widths, min-heights, and
anything inside `calc()` or `vh`. The conversion was written to skip
those categories rather than trusting a blanket regex.

**One value was load-bearing and the snap broke it.** The hero grid's
`56px` right padding sets the copy column's width, which sets the globe
column's width, which sets the globe's *height* because it is square --
and the hero's height feeds the fold arithmetic from round 9. Rounding
it to 48 widened the globe, pushed the hero 6px past its `min-height`,
and moved the quote strip off the fold (strip top 906 instead of 900,
chart 147 instead of 153).

Caught it by re-measuring the recorded hero invariants rather than by
looking, which is the only reason it was caught at all -- 6px is
invisible and the strip stayed below the fold, so the page still looked
right. 56 is a legitimate 4px step the scale had simply skipped, so it
went back in its ordinal slot (shifting 64 and 72 up) and the
declaration now carries a comment saying why it cannot move.

  A measurement trap worth recording: the first re-measure looked like a
  catastrophe (chart 101px, sphere 25.9%) because the pane had resized
  to ~594px tall and the chart is `17vh`. Comparing viewport-relative
  numbers across different viewports is meaningless; the second read at
  a pinned 1440x900 showed the real, much smaller regression. A stale
  sphere-percentage formula in the probe (using the pre-round-5 frustum
  constant) also understated that figure -- the probe was wrong, not the
  page.

Gates: 55 tests, tsc, lint, build clean. Verified the board, the hero
and the inner pages.

### Round 15 — the labels were never aligned, one just looked it

The user reported the second market label sitting off its panel while
the first looked fine. Measured: label 1 drift 0, label 2 drift **29px**.

The first was aligned by coincidence. Labels were their own flex row
(`flex: 1 1 0`, sharing the width left over after the pager); panels
were a separate flex row (`flex: 0 0 calc(44% - 4px)`, gap 20px). Two
independent layouts over the same span, so the only item that could
agree was the one starting at the container's left edge. Any fix that
nudged the label row's numbers would have had to be repeated in every
media query and would have drifted again the next time a basis changed.

So the structure changed instead: each label is now a child of its
carousel item, sharing one `--market-basis` with the panel beneath it.
Alignment is no longer two numbers kept in agreement -- it is the same
box. It holds at 1440, 900 and 375 with drift 0 across all three items,
and the peeking third item gained the label it never had.

The pager was the actual root cause and moved out of the flow. As a row
item it consumed width the panels did not, which is precisely why the
two rows resolved their percentages against different widths. It now
sits absolutely over the right end of the label line, with a background
so the clipped third label passes underneath rather than colliding.

Gates: 55 tests, tsc, lint, build clean.

### Round 16 — the gaps between sections

Measured the vertical gaps rather than reading the CSS, which is what
turned up the real problem. The rhythm was already broadly consistent
-- 32px between major blocks, 20px heading to content, 8px label to
panel -- but one gap came back at **205px**: the markets carousel down
to "latest".

That was not a spacing value. `.itx-board-cols` carried
`align-items: start`, so the two columns were free to be different
heights, and the rail (leaderboard + trends) is the taller. The markets
column stopped ~150px short, which left dead space under the panels and
meant the following section was pushed down from the *rail's* bottom
rather than from its own. No amount of adjusting margins would have
fixed it, because the margin was already 32.

Both columns now take the grid row's height and finish on the same
line, as the mockup has them: the market panel grows into its column,
the trends panel takes up the rail's slack, and the existing
min-heights stay as floors for when the markets column is the taller
one. Measured 1690 / 1690 for the two column bottoms, and the gap now
reads 32 like every other break.

The three relationships are named as well -- `--gap-section`,
`--gap-heading`, `--gap-label` -- so the rhythm is one decision rather
than several numbers that happen to agree today.

  Verifying on mobile needed a different measurement: the columns stack
  there, so carousel-bottom to "latest" legitimately spans the whole
  rail (683px) and looks alarming. The adjacent-element gaps are the
  meaningful figure once stacked, and those read 32 / 32 with the rail
  ending flush against the grid row.

Gates: 55 tests, tsc, lint, build clean.

### Round 17 — filler that behaves like a market

The user asked for enough agents and tasks to judge the design against
realistic content. The mock grew from 6 agents / 47 tasks to **26
agents / 220 tasks**, still deterministic off the same LCG seed, and
the interesting part is what had to change *besides* the counts:

- **Key material.** Every fixture key was built from one repeated
  string, so truncated pubkeys all rendered as `0201ab…4567`-alikes --
  a table of them read as one agent cloned. Keys are now generated hex
  (02/03 prefix), drawn from the LCG *before* anything else so the
  stream stays stable.
- **Trend shape is authored, not emergent.** Each capability tag
  carries a profile -- surging / steady / fading -- that shapes when
  its tasks happened. That single change is what turned the change
  columns from a wall of "—" into a believable mix of up, down and
  flat. (One agent prints +53099.50% on a tiny base; left in, since
  that is exactly what real markets do with small denominators.)
- **Worker pools per kind**, overlapping, so each market's ticker
  table has its own recurring cast rather than the same six names.
- **Long-tail bounties** (mostly small, occasional 30+ ITX whale) so
  compact formatting and wide percentages get exercised.
- **Disputed as a real status**: unresolved disputes now surface it,
  so the badge and tape headline paths run on every load rather than
  never. The deterministic dispute gating from session 4 stays.

Verified end to end: quote strip prints change on all three kinds,
ticker tables fill with mixed direction, the leaderboard fills its
eight rows from twenty earners, trends vary, the tasks page reads
"1–25 of 220" across 9 pages, and the agent page fills both work
lists -- with the null-net_worth fixture landing on the top earner,
which is a nice accident ("chain node unreachable" right where the
eye goes first).

Gates: 55 tests, tsc, lint, build clean.

### Round 18 — the mock starts ticking

The user asked whether the mock keeps updating. It did not: tasks were
built once at startup and frozen, which meant the "live" pill and the
scrolling tape were decoration over a snapshot. Two things had to
change for that to stop being true, and only one of them was the
fixture.

**The hub now evolves.** Every 2.5s a few in-flight tasks advance one
step and, roughly every fourth tick, a new one is posted. Steps follow
the real state machine rather than teleporting: claimed before
verified, verified before paid; consensus fills a seat at a time and
only starts once full; only an *answered* disputable task can be
disputed; a challenger who wins means the original claimant is not
paid. A minority of claimed work fails outright, so Closed keeps
occurring. Bounded at 900 tasks -- long sessions cannot grow without
limit, and it stays under the client's 1000-item walk so `complete`
never goes false and the partial-totals caveat never appears.

`STATIC=1` freezes it at the backfill. Worth having: a board that moves
under you is worse than a stale one when you are diffing screenshots,
which is most of what this fixture is for.

**The client had to poll**, or none of it would ever have been visible
-- `useAsync` ran once on mount. It now takes an optional interval.
Refreshes are *silent*: they never flip `loading`, so the screen
updates in place instead of flashing its skeleton every few seconds,
and a failed poll leaves the last good state alone rather than blanking
a populated board over one dropped request. The trade is that a hub
dying mid-session goes unreported until the next navigation; noted in
the hook, and the honest fix is connection state from a realtime
channel rather than inference from polls.

Scale went to 120 agents / 800 backfilled tasks, which opens with 104
earning agents instead of filling in over several minutes.

**Two details that only show up once things move.** The tape's marquee
duration is an inline style, so *any* change to it restarts the scroll
from the left -- with polling, an exact character count would have
nudged it on nearly every refresh, so it is quantised to 4s steps. And
the first post rate (0.7/tick) filled every row of "latest" with "just
now", which is accurate and useless; eased to 0.25 so the feed shows a
spread of ages.

The tape also polls at 15s rather than the board's 5s: both walk the
entire task list, making them the two most expensive things on the
page, and headlines do not need five-second freshness.

Gates: 55 tests, tsc, lint, build clean. Verified live -- 12 seconds,
no reload, feed turned over and quote figures moved.

### Round 19 — 1000 agents, and markets become task types

Three asks that collapsed into one problem: scale to 1000 agents,
"unlock" consensus, and make the market overview show the task types
that currently exist, biggest first, reordering as things move.

**Markets are now capability tags rather than task kinds**, ranked by
open bounty. That single change answers two of the three. The order
moves as work is posted and settled, and *consensus stops being a dead
panel* -- it previously had a column of its own that could never show a
row, because the hub hides who joined a consensus task by design.
Consensus work now counts toward whichever tag it carries: no agent
row, but its bounty counts, and leaving it out understated exactly the
markets where the biggest pooled work sits.

**The scale-up exposed a data problem the design was hiding.** With
1000 agents over 2000 tasks, 81% of agent-tag pairs had exactly one
paid task -- measured, not guessed. The change column is
period-over-period, so one payout is not a trend; it is either -100%
(it landed early) or null (it landed late). The first screenshot after
scaling was a wall of confident red.

Two fixes, and the split between them matters. The *display* fix is
that a trend needs two active buckets, otherwise "—": that is a real
correctness bug and would apply against the live hub too. The *fixture*
fix is volume, so agents actually accumulate history.

  Getting there involved a wrong turn worth recording. The first
  instinct was to concentrate work into ~16 specialists per tag, which
  fixes density by shrinking the field -- directly against the point of
  asking for 1000 agents. The user stopped it and asked why, which was
  the right call; the choice between "few busy specialists" and "many
  agents, more tasks" is theirs, not a detail to bury in a fixture.
  They chose volume. Specialisation stays at a wide 55 per tag because
  it is true of real marketplaces, but it now shapes who recurs rather
  than who is allowed in.

**Three pieces of supporting work**, none of which was optional at this
size:

- The task list is fetched **once** in `LandingPage` and shared with
  the tape and the board. The double walk was flagged as an acceptable
  trade-off when the board was 47 tasks; at 5000 it was the most
  expensive thing on the page.
- The board's aggregation is one grouping pass plus work proportional
  to the rows *rendered*. The previous version filtered the whole task
  list once per agent -- fine against dozens, quadratic against a
  thousand, and running on every poll.
- The globe and the market line **stop rendering when scrolled past**
  and resume where they left off (accumulated deltas for the globe, a
  clock reset for the tape, so neither jumps). A WebGL scene redrawing
  for nobody was competing with polling for the main thread.

Measured after: 5000 tasks fetch and parse in ~60ms, grouping ~3ms, so
the 5s poll stays comfortable; the walk is 2.4MB across 25 requests,
which is fine on localhost and is exactly the cost the client-side
aggregate design has always implied. `listAllTasks` now carries a note
saying the next bump of its limit is not the answer -- a server-side
aggregate endpoint is.

  One verification snag: a probe reported the markets were no longer
  sorted, but it was parsing the *formatted* label ("1K itx") back into
  a number and reading 1. The sort is on raw open bounty and was fine.
  Measuring rendered text is convenient right up until the renderer is
  lossy.

Gates: 55 tests, tsc, lint, build clean.

### Round 20 — panels that fill, and a left nav

Two asks: why the tables stop short of the bottom of their panels, and
a navigation section down the left.

**The tables stopped short because the row count was a constant and the
panel height is not.** `AGENT_ROWS = 10` was written when a market
panel was 430px; the panel is actually as tall as the rail beside it,
which at 1440x900 is 576px -- room for fifteen. Ten rows in a box for
fifteen is the gap in the screenshot, and it changes with the window,
so no constant is ever right.

Closed from the other side. `useFitRows` measures the box and reports
how many rows fit; the table renders that many. The old constants
survive only as ceilings on how much data a panel is *prepared* to
show. Measured after: market 15 rows with 4px slack, trends 6, latest 6
(was 5 with a row of dead space), leaderboard 6 with 11px.

Three details that were load-bearing:

- **`--row-h` lives in the stylesheet and the hook reads it off the
  measured element.** A row's height is a design decision; duplicating
  it in TypeScript would mean two places to change and one of them
  eventually wrong.
- **`flex-basis: 0` on the measured box is what stops the measurement
  chasing itself.** At `auto` the box contributes its rows to the
  panel's content height, so more rows make a taller panel, which fits
  more rows. At 0 the panel's height settles first and the box is
  handed the remainder.
- **Table cells are `content-box` by default.** With `height: 34px` and
  a 1px separator each row drew 35, so fifteen rows overran the budget
  by ten pixels and the last one was clipped. `box-sizing: border-box`
  on the cell, and the count is honest.

Also fed each agent's sparkline the tasks already grouped for it rather
than the market's whole list -- the same result, but the cost stops
scaling with the row count now that a tall panel asks for twice as
many.

**The nav is a third column in the board's grid**, left of the markets:
jump links to the four sections, the live list of markets, then the
links off the board. The market entries are the useful part -- with a
dozen capability tags and three panels visible, the pager alone means
clicking through the carousel to find one. These select it directly and
mark whichever is at the front. The list fills the rail the same way
every other panel does, and the outbound links are pinned to the foot
with `margin-top: auto` so a short market list leaves no dead end.

  Verification snag worth recording: the preview pane was hidden for
  this round, so every screenshot came back a flat dark rectangle.
  A hidden document does not run the rendering steps -- which also
  means **ResizeObserver never fires**, so a resize appeared not to
  re-measure. It does; a reload at the new size gives the right count
  (8 rows at 1000px wide, 15 at 1440px), because the effect's first
  measure runs whether or not the page is painting. Geometry was
  verified by measuring the DOM instead: panel height, content bottom,
  and the slack between them.

Stacked under 1080px the nav gets a `min-height` of its own -- without
a grid row to size it, its fit box resolves to nothing and the market
list empties to one entry.

Gates: 55 tests, tsc, lint, build clean.

### Round 21 — one masthead, every page

The tape and the ITX header were landing-page furniture; every terminal
screen had its own bare "ITX." in the top bar and no tape at all, so an
agent's page read as a different product from the board it was linked
from. Both now ride on every page.

The bar is its own component (`components/SiteBar.tsx`) in its own
stylesheet, and the tape moved out of `pages/landing/` with it -- there
was nothing hero-specific left in it. The landing page's copy of the
markup is gone; it renders the shared bar like everything else.

**The tape needs data, and the obvious way to get it was too
expensive.** The landing page walks the whole board already, so it
hands its list over -- that page still makes exactly one pass. Every
other page uses `listLatestTasks`, which is two small requests: the hub
lists tasks *oldest-first*, so ask for the total with a one-item
request and take the last page. Fourteen headlines cost 2 requests
instead of 26, and no page pulls a megabyte of JSON for a decoration.

**Two sticky rows had to learn to stack.** The terminal's top bar was
`top: 0`; under a sticky masthead it would have slid beneath it. The
page root now carries `--sb-h`, the bar's height, and the top bar parks
there -- with `:has(.itx-news)` restating it *without* the tape when the
visitor dismisses it, which is the same trick the landing hero uses to
grow into the freed space. Measured: bar 96 / top bar at 96 / sidebar
at 176; dismiss the tape and they become 56 / 56 / 136, with no JS
between them.

The bar keeps the landing's palette everywhere -- the tape is the same
red-blue-green ribbon on every screen -- except the two things that
must follow the terminal's light/dark toggle: the bar's own background
and text, and the green dot on the wordmark. Verified in both themes.

  The terminal top bar lost its duplicate wordmark, which is the one
  subtractive edit here. Two ITX marks stacked forty pixels apart read
  as a bug, and the masthead's is a link home -- the only reliable way
  back from a deep page like `/agents/:pubkey`.

Legacy pages are untouched and still bare: they don't mount `Shell`, so
they never see the bar. Checked at `/legacy` -- 16px body margin, no
masthead, table intact.

Gates: 55 tests, tsc, lint, build clean.

### Round 22 — the masthead was a link, and looked like one

Two things wrong with the bar from round 21, both about it behaving
like a link in a sentence rather than a masthead.

**On the terminal pages it rendered blue with a hover underline.** The
rule was there -- `color: inherit; text-decoration: none` -- and it
lost. `.itx a` and `.itx a:hover` style every anchor on that surface,
and a class on its own is a weaker selector than a class plus an
element. Matching the element too and restating hover wins on both
surfaces without `!important`. Verified under a real hover: colour
stays the theme's text, no underline.

**On the front page it showed a pointer and did nothing.** The link
points at `/` and you were already on `/`, so the click was a no-op --
a cursor promising a function that wasn't there. It now scrolls back to
the top, which is what a wordmark on a long page is for. Smooth when
you are already home; instant when the click is also a navigation,
where animating the outgoing page away is just noise and the next page
should start at its own top. Reduced motion gets the instant one
regardless.

  A hidden tab does not run smooth scrolling -- the same missing
  rendering steps behind this session's black screenshots and frozen
  ResizeObserver. Forcing the reduced-motion branch is what confirmed
  the handler fires at all: 1200 to 0. Worth remembering as a way to
  test a smooth-scroll path without a visible browser.

Gates: 55 tests, tsc, lint, build clean.

### Round 23 — the pill nav goes, and "home" means the board

**The Board / Tasks / Agents pill is removed.** It was a three-item
copy of the left sidebar standing eighteen pixels from it, and with the
masthead above carrying the wordmark the row had three navigations
stacked in 154px. What is left in that row is the theme toggle; the
sidebar, which is the fuller version of the same list, is on every one
of these pages. Its CSS goes with it, along with the wordmark rules
orphaned in the previous round.

**The masthead now points at the board, not the top of the document.**
The hero is the pitch -- a globe and a paragraph -- and someone
clicking ITX from inside the site is looking for the market. It links
to `/#itx-board`, so the address is shareable and the browser's own
handling of it works.

  Which turned out to matter more than expected. The first version did
  the arithmetic in JS -- board's top minus the measured bar height --
  and it was *overruled on a fresh load*: React mounts, the effect
  scrolls to 830, and then the browser's fragment navigation finds the
  element that now exists and scrolls it to 926, the board's top edge
  parked behind a 96px sticky bar.

  The fix is `scroll-margin-top` on the board, which every route
  respects -- the click handler's `scrollIntoView`, the browser's own
  anchor scroll, and any future link to it. It follows `--ld-bar-h`, so
  it is 96 normally, 84 on narrow screens, and 56 once the tape is
  dismissed. Verified all three: the board's top lands exactly on the
  bar's bottom edge in each.

Gates: 55 tests, tsc, lint, build clean.

### Round 24 — the strip is not at the board's top edge

**The landing still stopped a little low.** `scroll-margin-top` was set
to the bar's height, which lines the board's *box* up under the
masthead -- but the first thing on the board sits 24px above that box.
The quote strip is pulled up by `--bd-overlap` so it rides over the
hero's market line, so aligning the box put the strip's top quarter,
and its labels, behind the bar. The margin now adds the overlap, plus
one spacing step so the strip stops clear of the bar rather than welded
to it. Measured with the tape up: bar 96, strip at 108, first label at
121. With it dismissed: bar 56, strip at 68. Twelve pixels of air in
both.

**Dismissing the tape now sticks for the visit.** It was component
state, and the bar mounts per page -- the landing's tape and an agent
page's tape are different instances -- so closing one left the other
untouched and the tape reappeared on the next click. Verified both
directions: dismiss on the board, open an agent, still gone; masthead
home, still gone.

  `sessionStorage`, not `localStorage`, and the difference is not
  incidental. There is no control anywhere to bring the tape back, so a
  permanent record would be a one-way door out of a feature -- the
  theme toggle persists because that one can be undone from the page.
  This holds for the visit, which is what "I closed that" means while
  browsing, and a fresh visit starts with the tape again.

Gates: 55 tests, tsc, lint, build clean.

### Round 25 — arrivals land with a glow

A task that shows up on the tape now drops into the top slot and the
whole row glows red, fading as it settles. The wash and the halo are
painted on the *row*, not the panel: what is new is one headline, and
lighting the whole panel for it would say the section changed rather
than that a task arrived. Timing is lopsided on purpose -- the movement
is done in the first fifth of a second, and the rest is the glow
burning off. A slow slide with a quick flash reads as a sluggish list
rather than as news landing.

**"New" is decided by comparing ids with the previous poll**, not by
reading `created_at`. That field says when the hub made the task, not
when this page first saw it; a board left open for a minute would
otherwise flash rows that are merely recent. The first population is
deliberately silent -- on a fresh load every row is new, and animating
all six at once reads as a glitch.

  The comparison runs in an effect rather than inline during render,
  and that is not a style preference. Writing to the ref while
  rendering would work exactly once: React calls a render twice in
  development, and the second pass would compare the new ids against
  themselves, find nothing, and the animation would only ever appear in
  production builds. The cost -- the class lands a frame after the row
  -- is exactly when a CSS animation wants it.

Reduced motion keeps the glow and drops the travel: the row still says
it is new, it just doesn't move to say so.

  Verifying this against a hidden preview pane took a detour worth
  recording. A background tab throttles `setInterval` to roughly once a
  minute, so a 500ms sampler is really a 60s sampler and the obvious
  "watch for the class" loop reports nothing. It did catch one real
  arrival -- a new task at the top carrying `is-new` with its animation
  `running` -- and the rest was pinned down by driving the animation
  directly: seeking it to 100ms reads the row at 25% red, and to 980ms
  reads it at zero.

Gates: 55 tests, tsc, lint, build clean.

### Round 25 — the board's columns now agree on where they start

**The pager has moved up to the heading line.** It sat at the right end
of the label row, where it shared space with whichever category name
the carousel was clipping -- an arrow overlapping "prov…" is exactly
the clutter it looked like. It is now centred on "market overview",
over the same right edge. Down there it had to be floated out of the
flow, since as a row item it would have eaten width the panels do not
and the labels would have stopped lining up with them; up here it is
just a grid item.

**"navigate" is gone from the left rail.** Four section names under a
heading that says "navigate" is a label explaining a list that already
explains itself. What the column did need was the *height* the label
took, or its panel would have started above the panels beside it, so an
aria-hidden two-line spacer stands in -- matching a market's name and
size, not a guessed pixel value.

**The heading starts where the first market panel starts.** It ran from
the page's left edge, over the nav column, so the eye had two different
left edges to reconcile. The heading line is now laid out on the same
three columns as the board below it, with the title and the pager both
in the middle one. Measured at 1280: title left 244, first panel left
244, pager right 976, markets column right 976.

  Which meant naming the column widths. `--col-nav` and `--col-rail`
  live on the board's inner box and both rows resolve against them, so
  the heading cannot drift from the columns it sits over the next time
  one is retuned.

**The leaderboard says how many agents there are.** Two lines now, like
a market's label -- the count is worth having, and it is also what
makes this label the same height as the ones beside it, so the
leaderboard panel starts level with the market panels rather than a
line above them. A non-breaking space holds the second line open until
the hub answers, so the panel does not jump when it does.

Gates: 55 tests, tsc, lint, build clean.

### Round 26 — slow enough to watch

The arrival animation was too quick to read as movement: the eye caught
a flash, not a direction. Two things were wrong, and only one of them
was the duration.

**The curve was doing most of the damage.** A single hard decelerating
bezier across the whole animation covered three quarters of the
distance in the first 400ms and crept the rest -- so even a full second
of travel read as an instant jump followed by nothing. The keyframes
now carry their own timing functions and the animation itself is
linear: an ease-in-out for the move, a hold-then-fade for the glow. A
single curve across both halves can only rush one of them.

Measured across the new 2.4 seconds: 9% of the distance at 180ms, 46%
at 360, 78% at 540, settled by a second. The glow holds at 26% red
until 1.4s, is down to 15% at 1.7s, and reaches zero softly at 2.2s
rather than dropping off. The peak wash came down from 38% to 30%,
since it is now on screen long enough to be looked at rather than
glimpsed.

  Tuning an animation against a hidden preview pane is done by seeking
  it: set `currentTime` and read back the computed transform and
  colour. That is how the front-loading was caught -- the numbers said
  76% of the travel was gone in the first third, which is exactly what
  "I don't register the motion" feels like.

Gates: 55 tests, tsc, lint, build clean.

### Round 27 — in from the side, and a wash that clears the bullet

The user sketched the motion they wanted over a screenshot: the row
comes out from behind the panel's left edge and travels up and across
into the top slot. The straight vertical rise was legible but flat --
22px in a list of 34px rows barely leaves its own lane, so there was
nothing to follow.

It now enters from `translate(-64px, 26px)`: sixty-odd pixels across
the panel's edge, which is a path rather than a nudge. The fit box's
`overflow: hidden` earns its keep -- the row starts outside the panel's
content box, so it is genuinely hidden until it crosses the edge. There
is a five-pixel overshoot at the end of the travel, small enough to be
felt rather than seen; it is the difference between a row arriving and
a row being placed.

Two seconds rather than 2.4: a second flat was too quick to read as
movement, and 2.4 of pure rise read as careful rather than urgent,
which is the wrong note for breaking news. Travel occupies the first
half, settling by ~1s, and the glow holds at 28% until 1.3s before
fading out by 1.9.

**The wash now encloses the bullet instead of cutting it.** The row
started exactly at the dot, so the 10px corner radius sliced across it.
The row takes 12px of horizontal padding and gives it back as negative
margin, so its contents stay on the lines they were on and the glow has
somewhere to go.

  That alone would not have worked: the fit box that measures the tape
  also clips it, and the rows now hang 12px past it on each side, so
  the corners would have been sheared off again. The fit box is widened
  by exactly that much and pulled back by the same. Measured after:
  panel 48-1392, fit box 53-1387, row 53-1387 -- flush, nothing
  clipped -- with the bullet inset 12px from the wash and the columns
  still starting at 65 and 88 as before.

Gates: 55 tests, tsc, lint, build clean.

### Round 28 — a card swapped onto the deck

The diagonal entrance still read as "something slid in from the
corner". The gesture asked for is a card being pulled out of a deck and
placed on top, and a diagonal covers that ground in one move with the
hand missing from it. It is now three legs with corners between them --
appear low in the stack, draw out sideways, carry up level with the top
slot, push home -- each with its own eased segment so the turns stay
deliberate instead of smoothing into a curve.

Measured: still at (0, 52) at 170ms, out to x=68 by 580ms, up to y=0 by
1130ms, home with a 4px overshoot at 1560ms and settled at 1660ms, glow
gone by 2.4s.

**It travels out to the right, not the left**, and that is the fix for
what made the previous version read as a wipe. Going left, the clip
eats the row's own content: the bullet and timestamp vanish and the
headline appears to grow out of the middle of the panel. Held out to
the right the whole card stays legible and only its trailing edge
crosses the boundary -- which is what a card sliding past the edge of a
deck actually looks like.

**Two things a moving card needs that a glowing row did not.** It is
opaque while it travels, mixing the red into the page colour rather
than into transparency, because a translucent wash sliding over live
text underneath reads as a rendering fault. And it is lifted with
`z-index`: list rows are static, so later siblings paint over earlier
ones, and the card -- which is the *first* row -- would otherwise pass
underneath the rows it is meant to be moving over. It ends on the
panel's own colour, which is indistinguishable from the transparent it
started with.

Gates: 55 tests, tsc, lint, build clean.

### Round 29 — an arc instead of three right angles

Pull right, lift, push left was three legs with right angles between
them, which reads as a machine indexing a part rather than a hand
moving a card. It is one swing now: out and up together, widest at the
halfway point, back to zero as it lands.

The path is a sampled curve rather than a drawn one -- five points
along `x = 26·sin(πs)`, `y = 52·(1-s)`, with `s` spaced to ease in and
out -- and the segments between them are deliberately **linear**. A
curve on each segment as well would fight the sampling and put a hitch
at every point; the easing belongs in where the points are, not in how
the animation travels between them. The one eased segment left is the
glow's fade, which is a value rather than a path.

The sideways reach came down from 68px to a peak of 26. It only has to
show the card leaving the stack; past about 30px it stops being a swing
and starts being a detour.

Faster, too: 1.6s rather than 2.2, with the swing done in under a
second (measured: peak 24.6px at 480ms, landed at 960ms) and the glow
gone by 1.6. The arc is what lets it read at speed -- the shape is
doing the work a longer straight line had to do with duration.

Gates: 55 tests, tsc, lint, build clean.

### Round 30 — faster, and the facets taken out of the curve

Down to 1.2s: the swing lands at 600ms, the glow is gone by 1.2.

**Speed exposed the sampling.** A sampled curve is only as smooth as
its facets are short, and five points across 600ms put a direction
change every 150ms -- slow enough to be a corner rather than a curve.
Eleven points now, which costs nothing and quarters the facet.

  One of those points exists purely to fix a fault the sampling
  created. Taking the samples symmetrically about the apex put the two
  neighbouring points at the *same* x -- 25px either side of the
  peak -- so the card held against an invisible wall for 80ms at the
  widest part of the swing, which is exactly where the eye is. A point
  at the apex itself rounds the turn: measured, x now runs
  24.6 - 25.4 - 26 - 25.4 - 24.6 through it.

**The card is promoted to its own layer while it moves.** It is opaque
and travels over live text, so every frame was repainting the rows
underneath it. That is the kind of cost that does not show up in a
still and reads as "not smooth" in motion.

Gates: 55 tests, tsc, lint, build clean.

### Round 31 — same card, less of it

The card swap stays; it was the size of it that read as bulky. The
amplitudes were 52px up and a 26px bulge, and at that scale the eye
tracks the block rather than reading the news -- a slab being flown
into position rather than a row arriving. Halved and more: **30px up,
14px across**, over 400ms rather than 600, with the glow gone by 900ms.
Same gesture, at a size that registers without asking to be watched.

  A detour worth recording, because it was the wrong answer to the
  right complaint. "Seamless, shouldn't register the bulky movement"
  first read as *remove the flight*: the list opening a slot for the
  row by animating its height from zero, with the rows below pushed
  down by that alone and nothing ever overlapping. That is genuinely
  seamless and genuinely duller -- the user wanted the card kept and
  the weight taken out of it, which is a question of amplitude, not of
  mechanism. Reverted.

Measured: out to 8.3px by 120ms, apex 13.9px at 200ms halfway up, back
through 4.7px at 320ms, landed at 420ms. The eleven sample points and
the apex point stay -- at 400ms the facets matter more, not less.

Gates: 55 tests, tsc, lint, build clean.

### Round 26 — the rows were top-aligned, not spaced badly

**Every agent key sat against the line above it.** The board's rows are
a fixed 34px rather than a height their contents set, so what a cell
does with the slack decides where the text lands -- and `index.css` has
a legacy `td { vertical-align: top }` that the board's own cell rule
never overrode. Measured: half a pixel of air above the key, eighteen
below. It read as bad spacing; it was the text pinned to the top of a
row twice its height.

  `vertical-align: middle` on the board's cells, scoped to
  `.itx-board-table` so the legacy pages keep the global. Now 9.0 above
  and 9.5 below (the half-pixel is the separator, which lives inside
  the border-box height), and the sparklines centre with them at 7.0
  either side rather than riding the row's top edge.

Gates: 55 tests, tsc, lint, build clean.

### Round 30 — agents get names

A leaderboard of 66-character public keys is unreadable, and truncating
them makes it worse rather than better: `02a4f1…9c3b` and `03a4e8…9c3b`
are different agents that no one will ever tell apart at a glance. So
every agent the hub knows about now also has a name — a descriptor and a
subject in CamelCase, capped at 15 characters: `SwiftWarlock`,
`AmberOtter`, `DreadVampire`.

**The name is a label, not an identity.** Nothing authenticates against
it, no route accepts one in place of a pubkey, and an agent cannot
choose its own. A name an agent could pick is a name an agent can use to
impersonate another; this exists to make a table scannable, not to
introduce a second namespace anyone has to trust. The pubkey stays on
every row underneath the name, dimmed, and stays the link target.

**Where the words come from.** `wordlist/` at the repo root, compiled in
with `include_str!` so a deployed hub has no runtime file dependency.
`descriptors/adjectives.txt` is a WordNet-style dictionary dump — 17,755
words, including demonyms, anatomy, and participles. Drawing from it
directly gives about two million combinations of which most read as
nonsense (`AbdominalWorm`, `ZapotecValley`, `DesignateCat`), so
`descriptors/curated.txt` is a new file holding the 328 words in it that
read as names. With the 20 colours and 233 deduplicated subjects that is
**79,369 names that fit the 15-character cap** — three orders of
magnitude more than a testnet needs, which is what keeps assignment a
cheap random draw instead of a search. The original adjectives file is
left in place and unused.

**Uniqueness is enforced, not hoped for.** `NameRegistry` holds every
name it has handed out; assignment probes at random and falls back to a
wrapping scan from a random offset, which is what stops a nearly-full
pool from degenerating (at 99% occupancy a random probe hits 1% of the
time and 32 of them still miss two thirds of the time). An exhausted
pool returns `None` and the UI renders the pubkey, rather than the
registry ever handing the same name to two agents.

**Assignment is permanent and durable.** Names live in a new
`agent_names` redb table — additive in exactly the way `pending_deposits`
was, so no schema version bump. Stored as the finished string rather than
the word pair it came from, so a name already handed out keeps working
even if that word is later edited out of `wordlist/`. An agent renamed
between two page loads would be worse than an agent with no name.

**Who gets named, and where.** Startup backfills every agent already in
the reputation table, in one transaction. `/leaderboard` names anything
that has appeared since — safe because every pubkey there came from the
board's own reputation map, i.e. an agent that has actually done
something. `/reputation/:pubkey` deliberately **looks up but never
mints**: it is unauthenticated and resolves any well-formed key (the
agent page is built on that), so minting there would let an anonymous
caller drain the pool one GET at a time.

**Persistence is best-effort on the read path.** `ensure_named` drops
the registry lock before touching the store, so a slow fsync never
blocks a concurrent reader, and a failed write is logged rather than
turned into an error page — the names are already correct in memory for
that response, and the cost of losing the write is that they are
re-minted after a restart. That is the opposite of `PendingDeposit`'s
persist-before-you-hand-it-out rule, and for the opposite reason:
nothing here is irrecoverable.

`dashboard/mock/hub.mjs` reads the same wordlist files off disk rather
than copying words inline, so the fixture cannot drift from the hub. One
of its agents is deliberately left unnamed so the pubkey-fallback path
is exercised on every load.

Two small things found along the way: jsdom ships no `matchMedia` at
all, which made every test that mounts a terminal page throw inside
`Shell` before rendering anything (stubbed in `test-setup.ts` — an
environment gap, not behaviour under test); and `Shell` wraps every page
in the site bar's live ticker, so a page test has to stub
`listLatestTasks` even when it says nothing about the tape.

Gates: 116 Rust tests, 58 dashboard tests, tsc, lint, build clean.
Verified against the mock in both themes.

### Round 31 — the pager moves to the heading, and the peek fades

The carousel's arrows sat at the far right of the markets column, on
the same line as "market overview" but a thousand pixels away from it.
Out there they read as decoration on an empty stretch of rule: nothing
tied them to the thing they move, and the fact that the board pages at
all was easy to miss. They now sit immediately after the title, as
`.itx-board-headline` — one flex item in the head's middle column
holding both, which also means the two stay together at every width
instead of each being placed against the grid separately.

The other half of the same problem is the panel past the clip. A market
panel sliced down the middle reads as a rendering fault — same rows,
same label as its neighbours, just chopped — rather than as the next
thing along. The carousel now carries a `mask-image` that fades it out.

Two details make the mask behave. The ramp starts at the peeking
panel's own left edge, computed as `--markets-shown × (--market-basis +
gap)`, so it lands in the gutter between it and the last whole panel
and nothing fully on screen is dimmed; `--markets-shown` is declared
next to `--market-basis` and drops to 1 at both narrow breakpoints,
where a wider basis means only one panel is whole. And the gradient
steps to 0.55 alpha at that same stop before ramping to zero — a step
that is invisible, because it falls where nothing is painted, and buys
the peek a flat dimming on top of the dissolve so it reads as inactive
from its first pixel rather than only near the clip.

Verified at 1600 and 900 wide against the mock. Gates: 58 dashboard
tests, lint, build clean.

### Round 32 — the lift, barely there

Thirty pixels and a fourteen-pixel bulge still had a visible passenger.
The swing is now **10px of lift and a 4px bulge** -- under a third of a
row, and narrower than a character -- landing at 360ms with the glow
gone by 900.

The path, the eleven sample points and the apex point all stay; only
the amplitudes changed. That is the useful thing to have learned over
the last few rounds: the shape was right from the moment it became an
arc, and every complaint since -- bulky, aggressive, too much movement
to register -- was about size, not about mechanism. It is worth
resisting the urge to rebuild the animation each time; twice now the
answer has been two numbers.

What announces the news is the glow. The movement only has to keep the
row from appearing out of nowhere, and at this size it is felt as the
row settling rather than seen as something arriving.

Measured: out to 2.9px by 120ms, apex 3.9px at 160-200ms halfway up,
home at 360ms.

Gates: 58 tests, tsc, lint, build clean.

### Round 27 — light mode reaches the front page

**The toggle has moved into the masthead.** It lived in `Shell`, which
only the terminal pages mount, so the front page -- the one a visitor
actually lands on -- had no way to reach a setting the rest of the site
had. The masthead is the one piece of chrome on every surface, so the
button belongs there. The terminal's top bar went with it: the pill nav
had already gone as a duplicate of the left sidebar, and what was left
was a 58px sticky row holding one control.

  Which meant the theme could no longer be component state. Two roots
  read it now -- the masthead's button and whichever page root is
  mounted -- and they are in different trees. `useTheme.ts` holds it as
  a module-level store behind `useSyncExternalStore`: no provider to
  place, one re-render for every subscriber, and there was only ever
  one theme per document anyway.

**The landing surface has a light theme.** Its six colour tokens were
already *roles* rather than literals -- `--ld-dark` is the ground,
`--ld-light` the ink -- so light mode is one token block where the two
hold each other's values, and every rule below stays theme-blind.

  Three things did not follow from the tokens. The grid behind the
  board is a fixed dark blue-grey asset, so the same 30% opacity that
  is barely there on the dark ground draws a mid-grey mesh on the light
  one; it is a token now, 0.22 on light, which lands the lines at the
  same 1.24:1 in both. The sparkline baseline was a literal
  `rgba(233, 242, 237, 0.12)` -- the ink at 12% instead, so it flips.
  And the masthead: it already followed the theme on terminal pages,
  and a dark bar over a light board would have been the one thing on
  the site ignoring the switch, so the landing restates the two roles
  on the bar itself.

**The light palette is derived now, not picked.** The existing one on
the agent pages had reached for white and for pale greys, and next to
its dark twin it read as washed out. Measured, the deviations were
specific: the panel sat 1.14:1 off its ground where the dark panel sits
1.05:1 -- a card, on a design whose panels are outlines -- the hairline
1.41:1 against the dark one's 1.86:1, `--text-faint` was the same value
in both themes and so was 2.89:1 here against 5.55:1 there, and the
"up" green was 3.51:1 under thirteen-pixel figures.

  Every value is now matched by contrast against its *own* ground:
  panel 1.05, hairline 1.90, faint 4.44, and the three hues taken to
  L≈33% for 4.64 / 5.80 / 4.90 while holding their dark-theme hue angle
  to within two degrees. Both files declare the same block, since the
  two surfaces are one design.

  The wash is the exception and stays as it is: the hero's market line
  and the quote strip's outline are painted in JS from the brand green
  and red, the same pair the tape rides in, and they are objects rather
  than text.

Gates: 58 tests, tsc, lint, build clean.

### Round 32 — the row can be dragged, not just paged

The arrows are now findable, but on a phone the obvious thing to try is
the panel itself: a market half past the clip looks draggable, and
until now nothing happened when you pulled it. `useSwipe` turns a
horizontal drag on the carousel into the same page turn the arrows do —
both go through one `turn(direction)` on the board, so the two can
never disagree about which way "next" is.

**Touch and pen only.** A mouse drag across these panels is a text
selection, or the beginning of a click on an agent link; a carousel
that swallowed either would cost more than the swipe gains. Pointers
with a cursor keep the arrows, which is what they are for.

**The axis is settled in the first ten pixels and never revisited.**
Nobody starts a scroll perfectly plumb, so the hook waits until the
finger has travelled far enough to say what the gesture is, then either
takes it or lets go of it entirely. Paired with `touch-action: pan-y`
on the carousel: the browser is told up front that vertical is the
page's, so it never has to wait on a listener to find out whether a
scroll may start, and scrolling past the board stays as smooth as
scrolling anywhere else on it. Pointer capture is taken at the moment
the gesture is claimed rather than on `pointerdown`, for the same
reason.

**The pull is damped — 0.35 of the travel, capped at 56px.** Nothing is
rendered before the first panel, so a one-to-one drag to the right
would open a gap where a previous panel ought to be. What is wanted is
a row that gives and springs back with the page turned underneath, not
a hand-driven slide. The offset is handed to CSS as `--drag` and
applied to the *items*, not the carousel: the carousel is the clip, and
moving it would slide the row out over the nav and the rail instead of
past its own edges.

The mask grew a second ramp for this. A drag pushes the front panel
past the near edge, which sliced it exactly the way the peek used to be
sliced at the far one; `--leaving-edge` is 0 at rest — at rest the first
panel starts on that edge and a ramp would fade a panel that is
entirely visible — and 64px while `data-swiping` is set, a little wider
than the pull can reach so the edge stays soft for the whole gesture.
Under reduced motion the pull still tracks the finger, since that is
the gesture answering rather than decoration, but the spring back is
dropped.

The hook's callback lives in a ref rather than in the effect's deps: it
closes over the board's state, so it is a new function every render,
and depending on it would tear the listeners down mid-gesture — losing
the drag every time a poll landed.

Verified with real touch events driven over CDP: a 160px drag turns
exactly one market, a 30px one turns none, and the row reports a 42px
damped pull mid-drag and zero at rest. Gates: 64 dashboard tests (six
new for the hook, including the vertical-scroll and mouse cases), tsc,
lint, build clean.

### Round 28 — the switch crosses over instead of cutting

**Light and dark swapped on the frame.** Ground, ink, hairlines and the
grid all changed at once with nothing in between, which read as a
different page appearing rather than the same page changing its mind.
They cross over in 260ms now -- long enough to see, short enough that
the ground is not visibly "loading".

  Only during the switch. A `body.itx-theme-shift` class goes on when
  the theme changes and comes off again, so the transition cannot also
  catch every hover, every arriving row on the tape and every panel
  that repaints on a poll. Left on permanently it would have been a
  surface where nothing quite lands.

  It carries six properties and no more -- the four colour ones, plus
  `fill` and `stroke` for the sparklines and `opacity` for the grid,
  which changes weight between themes. Not `all`: a board mid-switch is
  not somewhere to be animating layout.

  The class is held 320ms against a 260ms transition. Removing it is
  what *ends* the transition, so equal values would let a timer landing
  a frame early cancel the fade at 95% and snap the rest -- the very
  thing being fixed.

  Two things that looked necessary and were not. A forced reflow
  between setting the class and changing the tokens: a transition
  starts from the *after*-change style, so declaring it in the same
  recalc is enough -- the panels report six running transitions either
  way, measured. And the rule needs `body` named on its own as well as
  `body *`, which does not match the body itself -- the ground that
  overscroll bounces against was the one surface still cutting while
  everything on it faded.

Gates: 64 tests, tsc, lint, build clean.

### Round 29 — the cross-fade was late, not just slow

**260ms `ease` down to 170ms `ease-out`.** The duration was part of it,
but the easing was the bigger half. `ease` spends its first third barely
moving, and on a change with nothing travelling -- colours simply arrive
-- that ramp does not read as anticipation, it reads as the page
hesitating before it answers. `ease-out` puts most of the change in the
first sixty milliseconds and lets the tail settle, which is the fade
that was asked for rather than the slide that was there.

  The class is held 230ms against it, keeping the same 60ms of slack
  over the transition it has to outlive.

**Some of the lateness was real, and measured.** The switch cannot start
until the style recalc it triggers has finished, and applying the
transition to every element roughly doubles that: a theme flip on its
own costs ~9ms on the board's 1288 nodes, and ~20ms with the rule --
between one and two frames of nothing, before any easing curve gets a
say.

  Worth knowing what that cost is *not*: the property list. Six
  properties and one measure the same, ~19ms against ~21ms. It is the
  universal selector -- the same rule aimed at the roots and panels
  alone runs in 0.1ms. What stops it being narrowed today is that 759
  of those 1288 nodes genuinely change colour, most of them table cells
  and links carrying their own declarations, so a hand-written list
  would be most of the board and would rot the first time a rule moved.
  Left at one frame, with the numbers written down for whoever revisits
  it.

Gates: 69 tests, tsc, lint, build clean.

### Round 33 — the same swipe, from a trackpad

Round 32 read the gesture as pointer events, which is a phone. A laptop
sends the identical two-finger swipe as a stream of horizontal `wheel`
deltas and no pointer at all, so on the machine most of this was built
on, the row did not move. `useSwipe` now listens for both; the drag
path and the wheel path share the same commit callback, the same damped
pull, and the same reset.

**Deltas are accumulated, not counted.** A trackpad reports a swipe as
dozens of small values, so the wheel path pages on total travel past
`WHEEL_COMMIT` (64 — higher than the touch commit, because a two-finger
swipe puts out far more delta than a thumb covers in pixels; at 48 the
row turned before the gesture felt finished). Below the threshold the
row takes the same damped pull a finger gives it, so the gesture is
answered before it is complete. A reversal zeroes the accumulator: that
is a new gesture, not a smaller old one.

**Momentum is the whole problem.** There is no "wheel end" event, and a
flick on a Mac trackpad keeps sending deltas for the better part of a
second after the fingers lift — enough to page through every market on
the board from one swipe. After a turn the gesture is marked spent and
coasts silently; only `WHEEL_IDLE` (220ms) of quiet starts a new one.

**The horizontal deltas are swallowed, including the spent ones.** A
horizontal wheel over a page with nothing to scroll sideways is what
triggers the browser's back-swipe, so a flick through the markets could
otherwise leave the site — hence a non-passive listener and
`preventDefault`. Vertical deltas are left completely alone: not
"ignored", *untouched*, because not calling `preventDefault` is what
lets the page scroll. There is a test for each half of that.

Verified in a real browser over CDP, dispatching wheel events at the
carousel: 56px of travel pulls the row 19.6px without paging, crossing
the threshold turns exactly one market, a six-event momentum tail turns
none, a deliberate second swipe turns one more, and a vertical wheel
over the same box still scrolls the page 200px. Gates: 69 dashboard
tests (five more for the trackpad), tsc, lint, build clean.

### Round 33 — the travel was too short, not too big

Three rounds of shrinking the arrival was the wrong direction, and it
is worth writing down why. The movement read as aggressive, so the
amplitudes came down -- 52px, then 30, then 10 -- and each cut made it
worse. A short hop is a jolt however small it is. The row was appearing
halfway up a list it had no history in, and no amount of shaving the
distance fixes that; it just makes the flinch smaller.

It now travels **the whole tape**: from under the bottom edge of the
panel to the top slot, 180px, landing at 600ms with the glow gone by
1.1s. The eye picks it up at the bottom, follows it the length of the
list, and it arrives somewhere it was already going. The bow across
stays small next to it -- 12px against a 180px rise -- there to keep
the line from being a lift shaft, not to be seen in its own right.

The distance is one custom property, `--arrive-rise`, and the eleven
sampled points are fractions of it, so the whole path scales from a
single number. Given how many of these rounds have been amplitude
changes, that seemed worth building in.

Measured: starts at y=180 under the clip, apex 12px across at 300ms
halfway up, home at 600ms.

Gates: 69 tests, tsc, lint, build clean.

### Round 34 — names reach the rest of the site

Round 30 put names on the leaderboard and the agent page and stopped
there, which left the front page showing raw keys for the same agents
the subpage showed by name. An audit of every place the site renders an
agent found nine of them, and they split cleanly in two.

**Two had the name already in hand and simply weren't using it** — the
landing board's leaderboard rail and the terminal overview's "top
agents", both of which already call `getLeaderboard()` and were mapping
over a `name` field they ignored. Those are this round.

**The rest are a data problem, not a styling one.** The market columns,
the news ticker, and the poster/claimant/challenger rows on the task
pages are all built from `TaskDto`, which carries pubkeys as bare
strings. There is no name on the wire to render, so they are deferred
rather than patched. Two things make that more than a plumbing job: the
hub caps `/leaderboard` at 50, so joining against it client-side would
work against the mock (which returns everything) and then fall back to
raw keys against a real hub for anyone outside the top — an
intermittent inconsistency, which is worse than the uniform one we have
now. And naming currently triggers on having a reputation record, so a
pubkey that only ever *posts* work is never named at all.

**The two surfaces got different treatments on purpose.** The terminal
stacks the name over the key. The landing rail cannot: its rows are a
fixed `--row-h: 34px`, which is exactly what lets `useFitRows` compute a
panel's capacity as arithmetic instead of a guess, and a second line
would break that. So the rail shows the name *instead of* the key, with
the full key on `title` and one click away. Measured after the change:
rows still 34px, including a 15-character `ValiantScorpion`. The rail's
names come out in the landing palette's blue and Helvetica Neue rather
than the terminal's mono — the two design systems stay separate, which
is the intent.

`AgentLink` grew an optional `meta` line to serve the overview without
going to three lines. The key only moves to the second line when a name
has displaced it from the first, so an unnamed agent with no meta still
renders exactly one line — which is what keeps the leaderboard's
unnamed rows looking as they did before names existed.

**The rail's search had to follow the display.** It filtered on pubkey
only; a list you can read but not search by the thing it shows is worse
than one that never showed the name. It now matches either, and says so
in its `aria-label`.

Worth recording: `OverviewPage` is unrouted — the landing board replaced
it and it is kept in the tree for clean rollback. Its fix is correct and
currently renders nowhere.

Gates: 74 dashboard tests (5 new, covering `AgentLink`'s four states),
tsc, lint, build clean. Verified against the mock.

### Round 34 — the climb gets a direction, and the glow arrives with it

The long travel was right but it read as a zoom: the row went up at one
speed, fully lit and fully legible the whole way, and stopped. Three
changes, all pulling the same way.

**The speed is eased out rather than in-and-out.** The card is quickest
as it leaves the bottom and spends the second half of the climb slowing
down -- 43px in the first 100ms, 10 in the last 100 -- then rebounds
three pixels past the slot and settles back. The rebound is where the
weight is: a thing with mass overshoots its resting place.

**The glow builds instead of arriving pre-lit.** The wash comes up from
8% red to 30% across the climb and reaches its fullest as the row
lands, so the colour settles *with* the card rather than announcing it
from the bottom of the panel. Then it fades as before.

**The contents fade in over the climb** -- their own animation on the
row's children, so the wash and the text can be on different clocks. A
card that is legible from the first frame has already arrived and is
merely being carried; one that resolves as it rises is arriving.

Measured: y 180 → 137 at 100ms → 45 at 400 → 1 at 700, rebound to -3 at
730, home at 800. Wash lightness 0.226 → 0.316 over the same span, text
opacity 0 → 1 by 700ms, everything back to the panel's colour by 1.4s.

  Gates: 63 tests pass, lint clean, and this round's change is CSS
  only -- but `tsc -b` and `vite build` do not currently pass on the
  tree as a whole. `Board.tsx` is mid-refactor in another session (a
  `useSwipe` hook deleted, `useCarousel.ts` added but not yet wired),
  which is neither mine to fix nor mine to commit.

  Which nearly went wrong: the first commit of this round used
  `git add` on a tree carrying that session's *staged* deletions and
  swallowed them, so a commit about an animation also deleted two files
  it had never heard of. Undone with a soft reset and recommitted by
  path. Worth remembering that on a repo with a second session in it,
  `git add -A` stages someone else's work in progress -- check
  `git status` before the commit, not after.


### Round 35 — straight up, with weight

The sideways component is gone. Several versions had one -- a
diagonal, then three legs, then a bow -- on the theory that a card is
drawn out of a deck before it is placed on top, and every one of them
read as a thing being manoeuvred. A row joining a list is not a card
trick, and the horizontal was the part the eye kept catching on.

One axis pays for itself twice: a single bezier is *exactly* smooth
where eleven interpolated sample points were only nearly so, and the
whole path is four keyframes instead of thirteen.

**Weight is three things, all in the ending.** The curve is eased out,
so the row is quickest leaving the bottom and slows into the top. It
rebounds three pixels past the slot and settles back, which is what
something with mass does when it stops. And the red builds from 8% to
its fullest exactly as it lands, so the colour settles *with* the row
rather than announcing it from the bottom of the panel.

The contents fade in over the climb on their own clock, reaching full
just before the row lands, so it is readable the moment it stops.

  Two passes on the easing curve, both measured rather than eyeballed.
  `cubic-bezier(0.1, 0.55, 0.2, 1)` put half the distance in the first
  80ms and then took 480ms over the last five pixels -- a launch and a
  crawl, not weight. `(0.28, 0.7, 0.45, 1)` spends its time where the
  eye is: 25% of the climb in the first 60ms, 65% by 180, 94% by 360,
  over the top at 480, and settled at 660.

Gates: 68 tests, lint clean. `tsc -b` and `vite build` still fail on
`Board.tsx`, which remains mid-refactor in another session; this
round is CSS only and the commit carries landing.css and this log.

### Round 34 — the row scrolls itself

Rounds 32 and 33 were the wrong shape. Reading a gesture as pointer
events, then again as wheel deltas, and turning either into a whole
page is a reimplementation of scrolling that is worse than scrolling in
every way that matters: it moved a panel at a time when the hand asked
for an inch, and on Safari — whose gesture plumbing differs from
Chrome's on both paths — it did nothing at all. `useSwipe` is gone.

The carousel is now a real scroll container: `overflow-x: auto`, and
that is the entire gesture story. Trackpad swipes, finger drags,
momentum, rubber-banding, shift-scroll and keyboard scrolling all come
from the browser, on every engine, composited off the main thread. It
follows the hand continuously because it *is* the scroll, not a
reading of one.

**What is left in JS is only what the browser cannot know.**
`useCarousel` reports which market is at the leading edge and whether
either end is reached, and turns an arrow into a snap. Three things
that had to be got right:

- `snapTarget` rounds *towards* the direction asked for, not to the
  nearest boundary. From halfway through a panel, "next" finishes the
  move the row is in the middle of rather than skipping the panel you
  are looking at. That is the arrows' new job: the scroll is free, the
  arrows tidy it back onto the grid.
- A step is taken from where a smooth scroll is *going*, not where it
  is. Reading `scrollLeft` mid-animation meant clicking through the
  markets quickly lost most of the clicks — each one re-snapped to the
  boundary the row was passing through. Fourteen clicks moved six
  markets; now they move fourteen. Any hand on the row clears that
  pending target, because the reader outranks an arrow.
- The row is re-measured when the market count changes. Nothing else
  announces it: items arriving from the hub do not resize the container
  (its width comes from the column it sits in) and adding them fires no
  scroll event, so the first measurement — taken on an empty row —
  said the row was against both of its ends, and *both* arrows came up
  disabled on a full board.

**Both fades are S-curves now.** The far edge used to step straight to
55% alpha at the peek's start, on the theory that the step landed in
the gap between two panels where there is nothing painted to step. That
held while the row sat on fixed pages; a freely scrolling row stops
wherever the reader lets go, so the step landed mid-panel and read as a
crease down the glass. The stops now follow smoothstep (3t²-2t³) at
quarters, sampled rather than eased because a gradient interpolates
linearly between its stops whatever curve you had in mind. Leaving and
arriving at zero slope is what removes the seam: there is no instant at
which the dimming starts. The near edge gets the same treatment, and
each collapses to nothing at its own end.

Also `overscroll-behavior-x: contain`, so a flick that runs out of
markets cannot reach the browser's back gesture; `-webkit-mask-image`
alongside the standard one, for Safari before 15.4; and the scrollbar
hidden both ways, since `scrollbar-width` is not WebKit's spelling.

Verified over CDP at 1600 and 1000 wide: a small wheel nudge moves the
row 60px and a larger one 300px (free, not paged), an arrow from 300
snaps to 479 — one stride of a 459px panel plus its 20px gap — running
the arrows to the far end lands exactly on the last scrollable pixel
with the next arrow disabled, and a vertical wheel over the row still
scrolls the page. **Not verified on Safari**: driving it needs "Allow
remote automation" turned on in Safari's Develop settings, which is the
user's to grant. The reason to expect it to work now is that there is
no gesture code left to be wrong — only `overflow-x`, which is as old
as the web. Gates: 68 dashboard tests (five for the snap arithmetic,
which is where the edge cases live), tsc, lint, build clean.

### Round 35 — the near fade only covers what is cut

The arrows leave the row on a boundary, which is precisely where a
panel starts flush against the near edge with nothing sliced off it --
and the fixed 96px fade sat right on top of it, dimming the first inch
of the market you had just asked to see. The fade at that edge cannot
be a constant, because unlike the far edge it does not always have a
panel crossing it.

`useCarousel` now writes `--leading-fade` on every scroll frame as
`min(into, stride - into, ceiling)`, where `into` is how far the row
sits past the last boundary. On a boundary it is zero and the panel is
crisp. Twelve pixels past one, only that sliver of the outgoing panel
is hidden and a twelve-pixel fade covers it exactly. Just short of the
next, the outgoing panel is down to its last few pixels and the fade
shrinks with it rather than reaching across the panel arriving behind.
In between, both terms are wide and the stylesheet's ceiling applies.

Written straight onto the node rather than through state: this runs on
every frame of a scroll, and re-rendering the whole board to move a
gradient would be the one thing that made the scroll stutter. The
ceiling is read from `--leading-fade-max` and cached until the row is
resized, so the stylesheet stays the place lengths are decided -- the
same arrangement `useFitRows` has with `--row-h`. The `[data-at-start]`
rule is gone: being at the start is just the case where the row sits on
a boundary, which the formula already answers with zero.

Measured over CDP: 0px at rest, 12px at twelve past, 96px at the
midpoint, 14px at fourteen short of the next boundary, 0px again on the
boundary and after an arrow lands.

A note on verifying this, since it cost an hour: CDP's
`captureBeyondViewport` re-lays out the page to take the shot, so a
percentage-width row like this one comes back with panels at different
positions than the DOM reported a moment earlier. Two rounds of
"the numbers say flush, the screenshot says sliced" were that and not
the code. Screenshots of this row have to be plain viewport captures,
cropped afterwards.

### Round 36 — where the latency actually comes from

An audit, not a change. Nothing in `dashboard/src` was touched; this
round is the note so the next one can be a fix. Measured against the
seeded mock (`dashboard/mock/hub.mjs`, 5600 tasks, 1000 agents, 12
capability tags), which is the closest thing here to a board with real
volume.

**The landing page pulls 2.83 MB of JSON every five seconds, over 28
sequential round trips.** That is the finding; the rest are footnotes to
it. `listAllTasks` walks the board 200 at a time to `maxItems = 7000`,
which at 5600 tasks is 28 requests it cannot start until the one before
it returns, and `LandingPage` puts that on a 5s `REFRESH_MS`. The walk
is already flagged in `lib/hub.ts` as a stopgap awaiting a server-side
aggregate; what the measurement adds is that it is not a "someday"
problem, it is the page's dominant cost today. On localhost the walk
finishes in 0.4s and nothing looks wrong, which is exactly why it has
survived. Off localhost the 28 trips are 28 × RTT before any bytes:
about 1.4s at a 50ms RTT, ~2.8s on a phone. The transfer is on top of
that — 2.83 MB over a 5 Mbps link is another 4.5s.

**And when the walk takes longer than five seconds, the polls overlap.**
`useAsync` arms a bare `setInterval(() => run(true), refreshMs)` with no
in-flight guard, so a slow walk does not delay the next one, it gets a
second walk started underneath it. Each new walk makes the network
slower, which makes the next overlap more likely. That is the shape of
"fine most of the time, then suddenly not" — it is not random, it is a
threshold, and once crossed the page does not recover on its own until
the walks get cheap again. The same `setInterval` also has no
`visibilitychange` gate, so a backgrounded tab keeps pulling megabytes
while the rAF work correctly pauses.

**None of it is compressed.** `hub/Cargo.toml` takes `tower-http` with
`features = ["cors"]` and nothing else — there is no `CompressionLayer`
in the stack. This JSON is repeated field names and 66-character hex
keys, which is the best case for gzip: a 200-task page measures 106,248
bytes raw and 20,300 gzipped, 5.2×. The full walk would go from 2.83 MB
to about 0.54 MB for one layer and no frontend change at all. It is the
cheapest thing on this list by a wide margin and it is a hub change, not
a dashboard one.

Four other screens make the same full walk for much less reason.
`AgentPage` pulls all 5600 tasks to show one agent's rows; terminal
`OverviewPage`, `LeaderboardPage` and `TasksPage` each pull the board for
aggregates. Those at least do not poll — they fetch once per mount — so
they are a slow first paint rather than a compounding one.

The client-side derivation is *not* the problem, which is worth writing
down so nobody optimises it first. Over the same 5600 tasks the whole
per-poll chain costs 11.4ms: `chooseWindow` 1.5, `summarizeByKind` 2.2,
`marketsByCapability` 3.3, `summarizeByCapability` 4.0, the `latest`
sort 0.4. The grouping pass that replaced the per-agent filter did its
job. Parsing the JSON is 6.5ms. Against 28 round trips these are rounding
errors.

Rendering is a different matter. Every market panel is in the DOM at
once — the carousel is an `overflow-x` scroller over `markets.map`, not
a window onto it — so at 12 capability tags and ~12 rows a panel that is
144 sparklines in the markets, ~12 in trends and 3 in the quote strip:
about 159 SVGs, each four elements around a 24-point polyline and
polygon, so roughly 640 SVG nodes. Nothing is memoised below `Board`,
so all of it re-renders on every poll — and on **every keystroke in the
agent search**, which is the one place a reader can feel it directly.
`chooseWindow` runs unmemoised in the render body and adds its 1.5ms to
each of those keystrokes; the `useMemo`s above it are safe only because
`window.windowMs` happens to be a primitive that compares equal.

Smaller, and last: `itx-arrive` animates `background` and `box-shadow`
across its 1400ms. Both are paint properties — the `will-change:
transform` next to them promotes the layer for the translate but does
nothing for these, so each arriving row repaints for the full duration.
Bounded by how many rows land per poll, and only worth touching if the
`latest` list ever gets long; the fix if so is a pseudo-element carrying
the glow and animating `opacity` alone.

Ranked by what a fix buys per unit of work: compression on the hub
(one layer, 5×), an in-flight guard and a `visibilitychange` gate on
`useAsync` (small, and it removes the cliff), a server-side aggregate so
the board stops being walked at all (the real answer, and the one
`lib/hub.ts` already names), then memoising `chooseWindow` and the rows
under `Board`. Not verified in a browser: navigation to the dev server
was blocked in this session, so the render counts above are derived from
the data and the component tree rather than read off a profile. The
network and derivation figures are measured.

### Round 30 — the board follows the window out

**Past 1560px everything extra was margin.** The board stopped there and
a wide monitor got the same two market panels with more empty page
around them. The cap is 2400 now, and the market row takes all of it --
the nav and the rail are fixed widths, so every pixel past the old stop
goes to the panels.

**And the row shows more of the market rather than a wider two of it.**
Three panels from 1700px, four from 2150. Stepped rather than computed
because the peek's mask has to know the count: `--markets-shown` is what
tells the carousel where the dissolve starts, and CSS cannot divide a
width by a width to find it.

  The steps are placed to hold a panel between about 340 and 500 px --
  wide enough for a key, a sparkline and a percentage without the three
  drifting apart, narrow enough that the next panel is worth showing.
  Measured: 388px at 1440 (two, as before), 367 at 1800 (three), 390 at
  2300 and 434 at 2560 (four), with the peek running 92 to 131px. The
  cost is a jump at each step, from the row's widest to its narrowest --
  502 to 337 at the first -- which is the trade for not letting two
  panels stretch to 600px each on a monitor this size.

**The quote strip needed a guard, not a stretch.** Its cells divide the
whole width, so with three kinds of task on the board they are 700px
across, and `space-between` was spending every one of those pixels
pushing each figure away from its own sparkline. The cells still
stretch -- that is what keeps the strip full -- but their contents stop
spreading at 260.

Gates: 68 tests, tsc, lint, build clean.

### Round 35 — the wordlist grows

A scan of both halves of the wordlist, with additions where a common
word was simply missing. The curated descriptors had a visible seam:
the hand-picking that produced them thinned out after the letter H, so
`hushed` and `husky` made the cut while `majestic`, `radiant`, and
`sneaky` never got considered. 101 new descriptors fill that gap —
every one verified present in `adjectives.txt` first, because
`curated.txt`'s documented contract is that it is a subset of that
file, and an unverified addition would quietly break it. (`ivory` was
the one candidate rejected: not in the dump.)

The subjects had gaps of the other kind — categories missing their
obvious members. `landscapes` had no `desert`; the `sea` file was all
fish and shellfish with no marine mammals (`whale`, `orca`, `narwhal`);
`weather` had every named storm system but not `thunder`, `lightning`,
or `frost`; `mammals` had fifty entries and no `panda`, `koala`, or
`giraffe`. 142 new subjects across all eleven files, each checked
against the union of every file so nothing landed twice.

The pool goes from 79,369 names to **164,771** (448 descriptors × 375
subjects, 98.1% of pairs inside the 15-character cap; no word in either
list pairs with nothing). Nothing else moved: the files are read the
same way, the hub's pool test re-verified every pair fits and renders
as plain ASCII CamelCase, and names already assigned are stored as
strings, so existing agents keep theirs regardless of what the list
does. The stale pool-size figures in `names.rs`'s comments and the
mock's were updated to match.

One mechanical trap worth recording: `weather.txt` shipped without a
trailing newline, so a blind append would have fused its last word and
the first addition into `sunthunder`. The append normalizes the ending
first.

Gates: 117 hub tests. The dashboard is untouched apart from a comment
in the mock; the restarted mock serves 630 unique names, cap holding.

### Round 37 — the market line gets a zero line

The chart's area ran off the bottom of its box and stopped dead where
the board began, which on the light theme is a hard horizontal cut
between a saturated red and the page's own ground. Per the reference,
it now ends on a **dotted baseline** the way a printed chart is ruled at
its zero, with a **bloom underneath in the counter colour** — green while
the tape is red, red while it is green — densest against the rule and
pulsing on its own 3.4 s clock. Explicitly not a mirrored chart pattern
below the line: a plain gradient, so the band reads as the inverse of
the wash rather than a second chart.

**The bloom rides the existing composite rather than adding one.** The
area already needed an offscreen buffer, because a canvas gradient can
vary colour horizontally or alpha vertically but not both. The bloom is
a second full-strength fill on that same buffer — same `mixAt` sample,
inverted — and because it sits in a band the area never touches, one
alpha ramp carries both: it climbs to the baseline for the area, *steps*
to the pulse (two stops at the same offset), and falls away again. Same
`mixAt` call for both, which is what keeps the two fronts crossing the
rule at the same instant instead of merely at the same speed.

**It fades out at the strip's top edge, not the canvas bottom.** The
quote strip is opaque and overlaps the chart by `--bd-overlap`, so a
glow still lit where the strip starts would be chopped off — a clipping
bug, not a design. `--ld-chart-base` is the band's height, read by both
the stylesheet (which grows `.itx-hero-chart` by it, upward, into the
copy's dead space) and by the canvas (which puts its baseline that far
off its own bottom). One token, because the two have to agree.

**The baseline exposed a latent bug in the price mapping.** The vertical
bounds *ease* toward the window's min and max, so a fresh extreme — an
impulse leg lands one, and `span` has a floor of 0.08 to divide by — is
briefly outside them and normalises past [0, 1]. Simulating the walk
over ten minutes puts that at ~3% of printed points, overshooting by up
to a couple of dozen pixels. It was invisible before: the excursion fell
off the bottom of the box, which is where the strip is. Against a dotted
rule it is the line crossing its own zero, and it was doing it in the
first screenshot taken.

  Clamped at the low end only, rather than easing faster, so the rescale
  stays exactly as gentle as it was. Measured after: 1–2 consecutive
  points rest level, never three, which is a support line and reads as
  one. The high end is deliberately left free — that overshoot leaves
  the top of the box, which is dead space behind the hero's copy and
  always was, and pinning it there instead draws a flat plateau through
  the middle of the chart, the one shape this generator goes out of its
  way not to produce.

**And the bounds now start on real prices.** They opened at a fixed
0.35/0.65 guess and eased in, which the walk opens wider than — so the
first half-second clamped a good part of the line flat against the new
floor, and the reduced-motion path, which draws exactly one frame and
never eases at all, would have rendered that as the finished chart.

Gates: 91 tests, lint and tsc clean on the touched files. Checked in
both themes and at 375px.

### Round 37 — the four fixes Round 36 asked for

Round 36 was the audit; this is the work. Ranked as it ranked them, by
payoff per unit of effort, and each landed as its own commit.

**Gzip on the hub.** `tower-http` was built with `cors` alone, so every
response went out identity. One `CompressionLayer`, outermost so it
compresses after the inner layers have shaped the response, and the
board's walk goes from ~2.8 MB to ~0.5 MB. Verified live rather than
asserted from theory: the real hub's `/llms.txt` 8,425 -> 3,573 bytes
with `vary: accept-encoding`, and an empty `/tasks` correctly left
alone under the layer's own size threshold. The test uses reqwest
*without* its `gzip` feature on purpose -- with it, reqwest would send
`Accept-Encoding` on its own and strip `Content-Encoding` off the
response before an assertion could see either, and the test would pass
whatever the server did.

The mock hub gzips too now. It is what the dashboard's latency actually
gets measured against, and a fixture that hides a 5x transfer
difference is a fixture that lies about the thing being measured:
101,842 -> 18,038 bytes on a 200-task page, gunzipping back
byte-identical.

**The overlap guard, which is the real answer to "sometimes".** A tick
arriving while the previous fetch is still running is now skipped
rather than started underneath it. The old behaviour was not a
slowdown, it was a cliff: fine every day until the walk crosses five
seconds, then each overlap makes the network slower and the next
overlap likelier, and it does not recover on its own. Skipping means a
slow hub is polled exactly as fast as it can answer. Polls also pause
while the tab is hidden -- rAF stops itself, `setInterval` never did --
and becoming visible refreshes immediately instead of waiting out the
rest of an interval.

**28 sequential round trips became two rounds.** `listAllTasks` fetches
the first page alone for its `X-Total-Count`, then every remaining
offset concurrently in flights of six -- what a browser will actually
run against one HTTP/1.1 origin, so a larger number would queue in the
browser instead of here. Pages land by position, not arrival order, so
the hub's oldest-first ordering survives however the responses come
back; there is a test for exactly that, because it is the kind of thing
that would silently corrupt every sparkline rather than fail loudly.
This cuts the walk's *latency*, not its weight. The server-side
aggregate is still the real answer and the doc comment still says so.

**And the render containment.** The agent search's `query` lived in
`Board`, so every keystroke re-rendered twelve market panels to filter
one list; it moves into a `LeaderboardRail` that owns the query, the
fit and the leaderboard poll. `chooseWindow` joins the memos instead of
re-parsing every `created_at` in the render body. `useCarousel` handed
React a fresh state object on every scroll event of a freely-scrolling
row, so most scroll frames re-rendered the board just to move an edge
fade -- it now returns the previous object when the index and both ends
are unchanged, with `MarketPanel` memoised so the renders that do
happen skip the sparkline tables whose props survived.

Two notes on verifying this round. The carousel could not be driven
through the browser pane at all: that browser ignores `scrollTo`'s
`behavior: "smooth"` outright and dispatches no scroll event for a
programmatic scroll -- confirmed with an independent spy listener
attached outside React, so it is the environment and not the hook. The
bail-out is covered by unit tests instead, and the "does not re-render"
one was checked against a reverted bail-out to make sure it can fail;
a test that cannot fail is worse than no test, particularly for a
negative claim. And the derivation cost measured in Round 36 (11.4ms)
was deliberately left alone. It was never the problem, and the point of
measuring first was to not spend the effort there.

Gates: 91 dashboard tests (15 new), 117 hub tests, tsc, lint, build.

### Round 38 — the board trades kinds of work, not agents

**What was actually on the board was wrong.** The carousel's panels were
one market per capability tag, and each panel's *rows were agents* --
top earners in that tag, with a sparkline of their earnings and a change
column. So the thing being priced was the worker. That reads as a
staffing table, not a market: agents are participants, and what a
marketplace quotes is the work.

So the hierarchy is now sectors of markets. A **sector** is a kind of
work (coding, creative, conversation, data, research, automation) and
its rows are the **individual markets** inside it -- python, web-dev,
machine-learning under coding; image-generation and copywriting under
creative; therapy, advice, companionship under conversation. Agents did
not disappear; they moved to the leaderboard beside the carousel, which
is where a participant belongs.

**Sectors are the site's reading of the board, not the protocol's.** A
capability is a free-form string on a task and the chain has no notion
of a sector, so the taxonomy is presentation-layer only
(`src/lib/sectors.ts`) and anything it has not heard of files under
`other` rather than vanishing -- which is the case that matters against
a real hub, where anyone can post any tag. The fixture is deliberately
ignorant of it too: it posts tags, exactly as the hub does.

**The quote strip changed with it.** It had been the three task *kinds*
-- hash-match, consensus, disputable -- which describe how a task is
verified, not what kind of work it is. Left at the top of a board that
now reads as sectors, it had the headline arguing with everything under
it. It quotes sectors now; `summarizeByKind` stays for the terminal
pages, which is where that distinction is the subject.

**No, this did not need more tasks -- it needs fewer per row.** Worth
recording because it was the question that opened the round. The change
column is period-over-period and needs activity in both halves of the
window, which is why Round 2x pushed the backfill to 5000: an
agent-tag pair had about four paid tasks and 81% of them had exactly
one, so the column was mostly dashes and coin-flip ±100%. A *market*
pools every task in its tag. Measured on the reworked fixture: 94-133
tasks per market and 20-23 of 24 buckets active, for all 35 markets.
Aggregating one level up bought roughly thirty times the density per
row at the same task count.

**The fixture grew sideways, not upwards.** Same 5000 tasks and 1000
agents; the twelve tags became 35 spanning all six sectors, with the
surging/fading/steady profiles mixed *within* each sector so a sector
panel shows markets moving both ways instead of whole sectors moving in
lockstep. Each tag now carries its own job descriptions and its own kind
weights -- pooled judgment on consensus, checksum work on hash_match,
anything arguable on disputable -- so the tape stops filing a
translation job under sql and a detail page stops contradicting its own
tag.

**One CSS fix the row content forced.** The name column used to hold a
truncated pubkey, which never wraps. It now holds a hyphenated tag, and
a hyphen is a break opportunity -- and since the rows are a fixed
`--row-h` that the panel divides its own height by to decide how many it
can show, a name wrapping to two lines would have made every capacity
below it an overestimate rather than merely looking wrong. The link
clips with an ellipsis instead. Clipping the link and not the cell is
deliberate: `text-overflow` needs a constrained box, and constraining
the cell would have meant `table-layout: fixed` on every board table,
including the two that size themselves fine today.

Not verified in a browser, again, and for a different reason than last
round: all five dev-server slots for this folder were held by other
sessions and none was reachable, so the pane could not be opened at all.
The reformat is covered by six new render tests against the real
components instead -- that sectors panel the board and markets are the
rows, that the column heads `market` and no longer `agent`, that a row
links to that market's tasks, that the rail ranks sectors by the money
in them, that the strip no longer quotes task kinds, and that an unknown
tag lands in `other`. What that leaves genuinely unchecked is
appearance, not structure: how the six panels sit in a row sized for
twelve, and whether the ellipsis ever actually triggers at the narrowest
panel step.

Gates: 97 dashboard tests (6 new), tsc, lint, build.

### Round 38 — the bloom becomes weather

Two notes on the baseline from the round before. The rule's marks go
from 3px to 5, longer than its gaps: at 3 a mark reads as a dot and the
line dissolves into stipple against a busy fill, where the thing it is
imitating is plainly *ruled*.

And the bloom's uniform pulse is gone. In its place the band is the
chart above, mirrored under the rule and smoothed with a binomial
kernel over five prints — which takes out every corner the generator
works to put in, leaving only the swell underneath them — with a second
swell travelling the other way across the width on its own slow clock.
Weighted 60/40 to the chart, so the band is recognisably its shadow
without tracing it, and drifting *against* the tape, because a wave
running with it at its speed reads as a reflection rather than weather.

**Alpha carries the wave, not geometry.** The obvious build is a shape:
mirror the line, fill under it. That needs its edge blurred or it reads
as the second chart we already decided against — so a canvas filter, a
third buffer to keep the blur off the rule, and a clip. Baking the wave
into the *alpha* of the bloom gradient's stops instead makes the
interpolation between stops do exactly that blur, on every browser and
for nothing. 48 stops; the wave lives between them.

  This works because strength and reach are the same thing under a
  vertical falloff — a brighter column is also a deeper-looking one, so
  varying alpha varies the band's height, which is the part that reads
  as a wave.

**The falloff had to stop being linear for that to be true.** How deep
the bloom *looks* is wherever strength × ramp drops under seeing, and a
linear ramp crosses that at nearly the same depth however bright the
column is — an even stripe that merely brightens and dims. A fast drop
with a long faint tail (1 → 0.55 → 0.22 → 0.07 → 0 across the band)
puts the crossing somewhere different for each: the tail sits under the
floor at the wave's troughs and just above it under its crests. The
band grew from 44px to 56 to give that tail somewhere to go.

`mixColor` takes an optional alpha now, defaulted to opaque, so the
quote strip's stops are byte-identical to what they were.

Gates: 97 tests, lint and tsc clean on the touched files. Checked in
both themes.

### Round 39 — four times the board, and what that exposed

**20000 tasks across 4000 agents**, up from 5000 and 1000. The fixture's
worker pools were fixed indices (`AGENTS.slice(380, 1000)`) that silently
stopped covering the field the moment the roster changed, so they are
proportions now and the agent count is the only number to move when
resizing. Measured after: 35 markets at 405-502 tasks each, 2230 agents
with earnings, every market still 23 of 24 buckets active.

**The client's walk limit had to move with it, and that is worth
understanding rather than just bumping.** Every headline figure on the
board -- open bounty, settled value, a sector's size, every sparkline --
is a sum over the whole task list, because the hub has no aggregate
endpoint and the client re-derives it all from `/tasks`. `listAllTasks`
therefore walks the board 200 at a time, and `maxItems` stops that walk
so a page load against a very large board cannot try to pull all of it.
Stop short, though, and those sums are computed from what was fetched
but rendered as the market: the site misreports its own size rather
than showing less of it. Raised 7000 -> 24000 against a fixture that
seeds 20000 and caps live growth at 22000; the numbers move together or
every figure on the page is wrong by an unknown amount.

  Which end gets dropped is the part worth writing down. The hub sorts
  ascending on `created_at` and offsets slice from the front, so a
  truncated walk keeps the *oldest* tasks and loses the newest -- the
  half of a live market anyone is actually looking at. A board in that
  state would show sparklines flattening toward the right edge and a
  "latest" feed that had quietly stopped being latest.

**And the board had been unable to say so.** `complete` has been on the
payload since the walk was written, the terminal overview has rendered a
warning off it since it was built, and the landing board took the flag
as a prop and dropped it -- the exact silence the flag exists to
prevent. It now says which end is missing, not just that something is.

**The volume exposed a quadratic that had been there all along.**
Derivation per poll measured 428ms at 20000 tasks, and 282ms of it was
`summarizeByCapability`, which collected the tag names and then filtered
the entire task list once per tag: 35 x 20000, with a scan of every
task's `capabilities` array inside. It is one grouping pass now, the
same shape `topAgents` was fixed into in Round 36 and `summarizeBySector`
was written as in Round 38 -- which is why that one, doing strictly more
work, was already five times cheaper than the function it sat next to.

  Numbers, with the caveat that the machine was noisy (other sessions
  running): before, `summarizeByCapability` cost 5.6x `summarizeBySector`
  (282ms vs 50ms) despite computing less; after, consistently ~40% of it
  across three runs. A whole poll's derivation ran 104-171ms depending
  on load, against 428ms before.

  The rewrite came with a real behaviour risk, since a per-tag filter is
  immune to something a grouping pass is not: `includes` matches once
  however many times a task carries the same tag, where a grouping pass
  files it once per entry and double-counts its bounty. Nothing is known
  to emit a repeated tag, but "identical results" should be true rather
  than nearly true, so both grouping passes dedupe and two tests pin it.

**What is still true and still unfixed:** ~100ms of main-thread
derivation every 5 seconds, on top of 100 requests and 1.9MB gzipped
(10.4MB decoded) per poll. Parallelism cut that walk's latency to about
350ms on localhost but not its weight. This is the point where the
server-side aggregate endpoint the code has been naming since Round 36
stops being a nicety -- the client is re-deriving on every poll what the
hub could sum once.

Not verified in a browser again: the dev-server slots for this folder
were still held by other sessions. Structure is covered by tests (101
now, 8 on the board itself); appearance is not.

Gates: 101 dashboard tests (4 new), tsc, lint, build.

### Round 40 — the aggregate endpoint, finally

**`GET /board/summary`.** The landing page no longer walks the board.
Round 39 measured what it cost at twenty thousand tasks -- a hundred
requests, 1.9MB gzipped (10.4MB decoded), and ~100ms of main-thread
derivation, on first paint and again every five seconds, to render a few
kilobytes of numbers. The hub already has every task in memory; it sums
them once now and answers in **7.1KB gzipped, one request, ~30ms**.
Roughly 270x less data and 12x less latency, and the client's derivation
drops from O(tasks) to O(buckets).

**What the endpoint deliberately does not decide.** It returns one row
per capability tag with raw per-bucket arrays -- posted counts and
bounty sums -- plus per-kind rows and board totals. It does *not* return
sectors, and it does not return percentages. Grouping tags into
"coding" and "creative" is this site's reading of the board, would
differ between clients, and would freeze a taxonomy into the protocol;
how a change is computed (period over period, withheld below two active
buckets) is equally a presentation decision. Both stay in `sectors.ts`
and `series.ts`, which now finish the job from the summary in O(24) per
row. The hub does the part that is identical for every viewer and
expensive; the client does the part that is cheap and opinionated.

**Verified by making the two paths agree.** `sectorsFromSummary` and
`summarizeBySector` were run against the same live 20000-task fixture
and asserted equal: same window, same sector order, same open counts,
open bounty and posted totals per sector, same market ordering, same
cumulative series arrays, same change percentages, and the same 24 trend
rows. That is the claim worth testing -- not that the endpoint returns
something, but that it returns the same board.

  One honest difference, documented at both call sites. From the task
  list, a task tagged into two markets of the *same* sector counts once
  for that sector; from the summary there are no task identities to
  deduplicate against, only per-tag totals, so it counts once per
  market. Nothing on the board carries two tags of one sector today and
  the fixture never emits multi-tag tasks at all. If that changes, the
  fix is a grouping the hub can compute, not a bigger download.

**The truncation notice added in Round 39 is gone from this board**,
which is the right kind of churn: with no walk there is nothing to
truncate, and the hub either aggregates the whole board or does not
answer. It still earns its place on the terminal overview, which still
walks -- and where its copy was inverted anyway. That page told anyone
hitting the limit it was "showing the most recent" tasks when the hub
sorts oldest-first and the walk slices from the front, so what it
actually showed was the oldest and what it dropped was the newest.

**What the landing page fetches now:** the summary, the leaderboard, and
the newest 24 tasks. That last one is the only thing on the page that
genuinely wants tasks rather than totals, and `listLatestTasks` gets it
in two small requests by reading the total and taking the tail.

Verification: 11 new hub tests on the aggregation (windows, bucketing,
the drop-outside-the-window rule, ranking, duplicate tags, empty kinds)
for 128 hub tests total, and 107 dashboard tests. The real `Board`
component was also rendered against the live fixture through the real
client -- six sectors in the rail, each panel holding only its own
sector's markets, the strip quoting sectors, the tape carrying real task
descriptions. Still not a browser: `preview_start` needs a
`.claude/launch.json`, which is not something to leave in this repo.

Gates: 107 dashboard tests (7 new), 128 hub tests (11 new), tsc, lint,
cargo build, build.

### Round 36 — every agent gets a face

Profile icons, composed from the owner's new `assets/profiles/`
artwork: an animal in one of five flat colours on a different-coloured
square, wearing one of three sets of eyes, one of three mouths, and one
of eight accessories. 7,680 possible icons; the pig contributes fewer
than the others because its snout-and-mouth is drawn into the body
artwork (it is the one animal with a second, dark fill) and so it never
picks a mouth piece.

**The icon is a hash of the pubkey, not a hub assignment.** That was
the owner's call between two architectures, and it is what the names
system could not have: the icon works on *every* surface that has a
pubkey — task detail's poster/claimant/challenger, the tasks list, the
landing rail, the agent page — with no storage, no cap, and no gap for
agents the hub has never named. The cost is that uniqueness is
statistical, not guaranteed: among ~600 agents roughly twenty pairs
will share an icon, and the name and key beside it carry the identity.
FNV-1a over the pubkey string, fields peeled off by modulo; the hash
and field order are frozen and pinned by a test, because "refactoring"
them re-rolls every face on the site overnight. The mouth index is
drawn (and discarded) even for pigs, so being a pig never shifts the
rest of the derivation.

**The exports shared no coordinate space.** Every piece sits wherever
it was drawn on its own 4000×4000 artboard — the three mouths at three
different spots, each animal at a different origin. A build script
(`dashboard/scripts/build-profile-assets.mjs`, run via `npm run
gen:profiles`) measures each piece's real bounding box by sampling the
path data (32 steps per curve; matches the browser's `getBBox` exactly
on every spot check) and compiles the markup into a generated module,
fills rewritten to `var(--pi-body)` / `var(--pi-dark)` so recolouring
is CSS, not string surgery. Composition is then translate-only against
per-animal anchor tables (eye line, mouth, head top, right ear, chest)
tuned by eye on a dev-only contact sheet (`/dev/icons`, DEV builds
only) against the owner's eight reference images. Nothing scales:
the artist drew every piece at its intended size, and the tables place
rather than transform.

Colours, per the owner: the landing palette's red, green and blue plus
purple #BB76DC and yellow #E7BF68; features always in the landing dark
#161418. Body and background are drawn as an ordered pair of *distinct*
colours (5×4 = 20), so the animal never blends into its backdrop.

Tuning notes that survived to the tables: hats overlap the head outline
(a floating hat reads as a bug); the frame's bottom edge must always
cut *into* the body like the references — four animals initially showed
background under their feet; the tie and headphones both hang off the
chest anchor with opposite nudges; the rabbit's frame rides high
because its ears nearly double its height.

Verified against the mock on every surface, including that a poster
appearing four times in the task list wears the same face four times,
and that the landing rail's fixed 34px rows centre a 22px icon with
6px above and below. One session-quality note: the Browser pane's
screenshot capture goes black on scrolled pages, so the contact sheet
grew a `?only=` row filter and the rail was verified by measurement.

Gates: 112 dashboard tests (5 new on the derivation, pinned), tsc,
lint, build clean.

### Round 41 — the rule comes back down to the strip

The dotted baseline and its bloom (Round 37-38) sat a full strip-height
above the quote strip by the time the band grew to 56px for the wave's
falloff -- floating clear of the box instead of landing inside it, per
the reference the boundary was built from in the first place. The gap
between the rule and the strip's top edge is exactly `--ld-chart-base`,
independent of `--bd-overlap`: bringing it back down to 24px puts the
rule about a third of the way down the (62px) strip again, matching
where it sat before the band existed at all.

The falloff stops are fractions of the band, so they compressed along
with it rather than needing their own retune -- the glow is tighter now
but still reads as a wave, just over a shorter run.

Verified with an isolated harness (the hero chart and the quote strip,
nothing else on the page) rather than the live site: another session
has `Board.tsx` mid-refactor and it does not currently render. The
gates below only cover the two files this round actually touched.

Gates: tsc and lint clean on `MarketLine.tsx` and `landing.css`; 112
tests pass (none exercise this pixel relationship directly -- checked
by measuring the rendered gap in both themes instead, 24px in each).

### Round 41 — markets get a price, and the columns sort

**A fourth column, after the Yahoo reference:** market, sparkline,
value, change. The sparkline's heading is gone and the column left
`aria-hidden` -- it draws the same quantity `value` names, and labelling
it separately implied a third figure that does not exist.

**Which quantity to quote was the whole decision.** The obvious
candidate was `openBounty`, what a market has on offer right now, and it
is the truer "level" in the stock sense -- it is also what the sectors
are ranked by. It is not what the column shows, because there is no
honest change to pair it with. A task carries its current status and no
record of when it reached it, so how open bounty moved over the window
cannot be derived at all (the note at the top of `series.ts` has said so
since it was written). Quoting one quantity beside another quantity's
percentage is a pairing no reader would think to question and every
reader would misread.

  So `value` is bounty posted into the market across the window, which
  makes all three columns one quantity: the sparkline is its running
  total, so the line ends exactly at the number beside it, and the
  change is that flow's period-over-period movement. The default order
  is by that column, largest first, so the ordering explains itself
  rather than looking arbitrary against a number it doesn't match.

**Sorting is board-wide, not per panel.** Clicking `value` or `change`
on any panel reorders every sector's panel. Per-panel state was the
other option and it is worse: two panels side by side sorted by
different columns stop being comparable, which is the one thing a row of
panels is for. Clicking the active column flips it; clicking the other
takes it over descending, because "most" is what anyone means the first
time they sort by a number. `aria-sort` carries the same fact the caret
does.

**The `null` change was the sharp edge.** A dash means "too little
activity to compare halves of the window", not zero. Sorted as zero it
files among the genuinely flat markets; sorted as -Infinity the
emptiest markets head an ascending sort, which is the opposite of what
sorting by change is for. Dashes sink to the bottom in *both*
directions, ties break on the market name so the order cannot wobble
between polls, and `sortMarkets` copies rather than sorting in place --
the summaries are memoized and shared across panels, so an in-place sort
would make the order depend on how many times it had been read.

**The fourth column cost the other three some width.** At the narrowest
carousel step the panel is ~337px, so the market name's cap came down
from 22ch to 18ch and the sparkline from 68 to 52 inside these panels --
the sparkline carries shape rather than figures and reads the same
narrower. `relationship-advice` clips by a character at that width,
which is why the full tag is now on the link's `title`. Values are
tabular-figure so the column reads as a column.

Verified against the live 20000-task fixture through the real
components: headers render `market / ▼value / change`, the default is
value-descending (web-dev 5.2K, sql 5.1K, cpp 4.9K), value-ascending
inverts it, change-descending runs +391% to +188% and change-ascending
-62% to +3%. Plus 16 new tests -- five on `sortMarkets` including the
dash and stability cases, six driving the headers through the rendered
board.

Gates: 123 dashboard tests (16 new), tsc, lint, build.

### Round 37 — one placement per piece, not one per pair

The owner's note: the crown sat on one head and floated over another,
the headphones moved between animals, the bow was wrong. All one bug,
and it was structural rather than a matter of nudging numbers.

Round 36 anchored every piece **per animal** — six landmarks on each of
six animals, and every accessory placed against them. Thirty-six
numbers that could drift against each other, and they did. The owner's
correction is the fix: *the accessories were drawn to sit at one spot
that works for every animal*. So the model inverts. Each worn piece has
exactly **one** placement, shared by all six. The **animal** is what
moves, nudged into the shared frame by a single per-animal `ALIGN`
entry. A crown that is right on the cat and wrong on the rabbit is now,
by construction, a statement about the rabbit's alignment — there is no
per-animal crown to get wrong. A test asserts it: every accessory and
every eye piece must resolve to an identical transform across all six
animals.

**Where the numbers came from.** Not guessed this time. The eight
reference images are all the same cat, so the cat's position in them
fixes a mapping between reference fractions and artboard units: measure
a piece's box in the reference, map it back, subtract its own artboard
box, and that difference *is* its placement. The frame falls out of the
same arithmetic (`951 719 1427`). What makes it trustworthy is that the
sizes then agree without any scaling — crown measures 279x223 against a
native 277x223, sunglasses 674x265 against 676x263, bow 416x312 against
402x312. If the mapping were wrong those would be off by the same ratio
everywhere, and they are not.

Two things checked and ruled out along the way: every export really does
declare the same 4000x4000 artboard with no group transforms (so the
scatter is in the artwork, not in the parsing), and the pieces are *not*
mutually aligned in artboard space either — the party hat's base sits at
y=410 and the crown's at y=1317, which cannot both be resting on one
head. That is what proves each piece needs its own measured offset
rather than a single global one.

Animal alignment came from the silhouette scan added this round: each
animal rasterized at artboard scale and reduced to a per-row width
profile, from which the face centre-line and head crown fall out. The
horizontal figure is reliable (the face centre-line is the bbox centre
for all six); the vertical needed correcting by eye for the three
animals whose widest row is their ears rather than their cheeks.

`/dev/icons` was rebuilt around the actual question. It shows one row
per piece with all six animals side by side, so a piece that drifts
reads as a stepped line instead of something to hold in your head
between two screens; `?piece=refs` reproduces the eight reference
images for direct comparison, and `?piece=align` shows bare faces for
judging `ALIGN` without an accessory confusing it. `ProfileIcon` took
an optional `spec` prop to make those views possible — production still
passes a pubkey and lets the hash decide.

Gates: 128 dashboard tests (5 new, pinning the shared-placement rule),
tsc, lint, build clean.

### Round 42 — the overlap was the point, not the clearance

Round 41 read "overlap the top third of the bar" as *clearance* and put
a 24px gap between the dotted rule and the quote strip's top edge. The
opposite was wanted: the strip is meant to sit **on** live chart, its
top third covering the bloom, the way the reference has it.

The gap was not the 24px of geometry -- that part was right, and stays.
It was the bloom's falloff, which Round 37 made reach zero exactly at
the strip's top edge, out of a worry that an opaque strip cutting off a
live glow would read as a clipping bug. It read as a gap instead, which
is the worse of the two, and the worry was misplaced anyway: the strip
is inset 48px from the page, so in the gutters either side there is
nothing to cover the glow and it carries on down in plain view. The
covered stretch reads as depth, not as a slice.

Two changes, both in the bloom:

**It now runs to the canvas bottom** (`glowY = height`) rather than to
the strip's top edge, so its last `--bd-overlap` of travel is behind
the strip.

**And its falloff is gentler**, because the strip's edge cuts the band
at its midpoint and the glow has to still be burning there. Measured as
a ratio down a column -- the wave's strength is constant vertically, so
this isolates the ramp -- the first curve was at 0.19x its under-rule
strength by that edge: lit, but hard to tell from having faded out. The
new stops (0.68 / 0.38 / 0.14) hold **0.40x**, with the tail carrying
0.28 -> 0.16 -> 0.07 below it for the gutters.

Gates: 128 tests, lint and tsc clean on the touched files. Checked in
both themes; the covered fraction is 24/75 of the strip on the live
board, and 24/62 in the harness, which has one quote rather than six.

### Round 42 — the rails stay put, and the sectors get a map

**The trends change was being painted outside its panel.** Reported as
"can't see the change", and the cause was two steps back: the rail is
232px, nothing capped the tag column, so a long hyphenated tag wrapped
to a second line -- which broke the fixed `--row-h` *and* pushed the
percentage past the panel's edge, where it was clipped mid-number. The
first fix (nowrap and an ellipsis, as the market panels already had) got
the rows back to 34px but still left the table 8px wider than its panel:
a `max-width` in ch is a *preferred* width to automatic table layout, so
the table kept sizing itself to the longest tag. That table is
`table-layout: fixed` now, with the sparkline and percentage columns
declared and the name column taking what is left. Measured after: table
right edge 1375 against a panel edge of 1392, and full percentages
(`+300.00%`) on every row.

**Trends links are grey.** Every row in that panel is a link, so the
blue had stopped reading as "these are links" and started reading as a
colour the panel happened to be painted in. The sparkline and the change
carry the colour there now, which are the two things in the row that
mean something by being coloured.

**The nav and the rail are pinned.** Both are `position: sticky` under
the masthead and exactly as tall as what is left of the viewport, and
`.itx-board-cols` gained `align-items: start` -- stretching was right
while the three columns were the same height, but the middle one is
taller than the screen now and stretching the pinned pair to *that*
would leave them taller than the viewport with nothing to pin against.
A definite height is also what the panels inside need, since the trends
panel takes the rail's slack with `flex: 1` and `useFitRows` divides a
real height.

  The tape moved into the middle column to match, where it now shares
  the markets' bounds exactly (measured: both at x=244, 892 wide). It
  had been spanning the full board width below the three columns, which
  with two of them pinned would have left it running underneath them.

  Verified by scrolling: over 400px of scroll the pinned columns travel
  153 and stop, while the tape moves the full 400. They release exactly
  when the columns end, finishing level with the middle column rather
  than leaving it hanging.

**A position indicator under the carousel.** The row's real scrollbar is
hidden -- the peek and its dissolve are what say "there is more this
way" -- so this is the same information drawn to match the board: a
hairline track with a thumb as wide a fraction of it as the row is of
its scroll width. Both numbers are custom properties `useCarousel`
writes on the parent each scroll frame, off React's render path, the
same arrangement the edge fade already used and for the same reason: a
free-scrolling row moves on frames that change nothing React renders.
Measured across the travel: progress 0 to 1 moves the thumb 0 to 565px
on an 892px track with a 327px thumb -- exactly the track less the
thumb, linear throughout.

**And a sector breakdown under the tape**, after the reference: a table
of sectors by weight on the left, a treemap on the right. The layout is
a squarified treemap (`lib/treemap.ts`) rather than slice-and-dice,
which on this data would render the smallest sector as a 16px hairline
400px tall -- a box that shape cannot carry a label. It is pure
geometry, so the awkward parts are properties rather than fixtures:
areas proportional, no overlap, exact coverage, every box inside its
bounds, and an aspect ratio under 4 for all of them. Confirmed against
the live board at 100.2% coverage, the 0.2% being the 1px gutters.

  Size and colour deliberately carry *different* facts, which is the one
  thing worth being careful about here: a tile's area is its share of
  the value on offer, and its tint is its change -- how much is there
  against which way it is going. The tint is stepped rather than
  continuous so sectors of similar standing read as one colour, and the
  ladder is in a legend because a colour scale whose thresholds nobody
  can read is decoration. The reference's own ±3% ladder is useless
  here, where a sector routinely doubles over the window, so the steps
  are 25/50/100%.

  Selection is by click and by nothing else. It was briefly on hover
  too, which looked free and was a bug the test caught: moving onto a
  row selected it, so the click that followed found it already selected
  and toggled it straight off -- two mechanisms owning one state, the
  more discoverable silently cancelling the other.

Verified in a real browser this time, the first round that has been.
Two things it could not show: this browser dispatches no scroll event
for a programmatic scroll (already known from Round 37), so the
indicator was driven by dispatching the event and reading the geometry;
and it does not advance CSS transitions, so the dim was confirmed by
disabling the transition and watching the value land at 0.35. Its
screenshots come back blank at any scroll position, so everything above
is measured off the DOM rather than looked at.

Gates: 144 dashboard tests (16 new), tsc, lint, build.

### Round 43 — the strip rides over the line

**Why tuning `--bd-overlap` had never produced an overlap.** The token
did three jobs at once: it was how far the strip is pulled up, *and* how
much the chart box grows by (`.itx-hero-chart`), *and* what `MarketLine`
subtracts from the canvas to find its baseline. Growing the box by the
same amount the strip rises cancels the two exactly -- the strip lands
in the same place over the line whatever the number is, which is why the
chart and the strip kept meeting edge to edge. The dotted rule sat above
the strip's top edge, and the line, which is scaled to that rule, could
never reach it.

So the pull-up is now two tokens. `--bd-overlap` keeps its three jobs
and its value; `--bd-lift` is applied to the strip alone and is not
compensated anywhere, so it is the part that actually carries the strip
up over the line. The canvas keeps its geometry and the baseline stays
put; the strip simply covers more of it. At 64px, measured on the live
page: the baseline sits 40px behind the strip's top edge and 22px of the
line's own travel is behind it, so troughs disappear under the strip and
peaks read above it, as the reference has.

**And the same lever fixed what the landing screen was leaking.** The
rule sits `--bd-overlap + --ld-chart-base` (48px) up from the canvas's
bottom edge, while the canvas only reached `--bd-overlap` (24px) below
the fold -- so the rule and the top of the bloom printed ~24px *above*
the fold and were the last thing on the first screen, which is exactly
what they should not be. The hero now grows by the whole pull-up rather
than by the overlap alone, which puts the canvas's bottom far enough
down that the rule clears the fold. Measured at two viewport heights,
since the chart's own height is `vh`-based: at 720 and at 900 the
strip's top edge lands on the fold to the pixel (delta 0) and the rule
sits 40px below it, coming into view on the first scroll.

Both numbers move together from one place -- raising `--bd-lift` deepens
the overlap and pushes the rule further below the fold, and the hero,
the grid overlay and the board's scroll offset all read it.

Gates: 144 dashboard tests, tsc, lint, build. Measured in a real browser
rather than looked at: its screenshots do not capture the two canvases
on this page, so the geometry above is read off the DOM.

### Round 44 — the tape gets columns

**Five columns where there were three:** when, the task, its value, the
market it trades in, and who posted it. The value moved out from the far
right -- where it had drifted to read as a footnote to whatever column
it sat beside -- to immediately after the task it belongs to, in green.

**Fixed widths, not a grid.** Columns have to line up down the panel, and
the obvious way is `display: grid` on the `<ul>` with `display: contents`
rows. That does not work here: the row has to stay a *box*, because the
arrival animation moves it, paints a glow behind it and rounds its
corners, none of which a contents-display element can do. So every
column but the task's own name is a fixed width and the name flexes.
Measured on the live board: each of the three new columns resolves to a
single left edge across every row (519, 631, 795), and rows are still
`--row-h`.

**Whose name is on the row, and why it is the poster.** This is a feed of
work as it is *posted*, and the newest tasks are open by definition -- a
claimant column would be empty on nearly every row and would quietly
change meaning on the few where it wasn't. The icon needs no lookup:
`ProfileIcon` composes it from the pubkey, so it is there for any agent,
named or not. The name does, and comes from the leaderboard.

  Which meant lifting that fetch from `LeaderboardRail` back into
  `Board`, since two polls of one endpoint is one too many. Only the
  *data* moved; the search query stayed put, and that split is the whole
  point -- the query in `Board` was what made every keystroke re-render
  a dozen sparkline tables back in Round 36. A poll re-rendering this
  subtree is absorbed by the memoised summaries and `SectorPanel`'s own
  `memo`; a keystroke doing it would not be.

  The leaderboard only carries agents that have earned, so a poster who
  never has is genuinely absent from it and the row shows a truncated
  key. That is the right rendering rather than a gap to paper over: the
  name is a label the hub assigns, and the pubkey is the only thing that
  identifies an agent. Live board: names resolve on most rows
  (`GallantLobster`, `SulkyWave`), keys on the rest.

**Untagged work says so.** A task may carry no capability at all --
untagged work is unrestricted rather than belonging to a market called
"none" -- so the cell holds the column open with a muted "untagged"
instead of collapsing and pulling the columns beside it out of line.

One test caught a fixture gap rather than a bug: the tape's fake task
omitted `capabilities`, which the hub always sends and which the rest of
`lib/` already indexes without guarding. Fixed the fixture rather than
adding a defensive read, since the wire contract is not optional here.

Gates: 146 dashboard tests (2 new), tsc, lint, build.

### Round 45 — the sector panels size themselves

**Nine rows in a 772px box.** The panels took their height from the
markets column, which Round 42 pinned to the viewport so it would finish
level with the rail beside it. The result was a nine-row table adrift in
six hundred pixels of empty panel, and every sector below coding worse
than that.

The relationship is inverted now: the panels are sized *by* their rows
rather than measured for how many rows they can hold. `SectorPanel` is
the only panel on the board without `useFitRows`, and the markets column
has no height of its own -- the carousel takes it from the tallest
sector, and the rail is simply the taller of the two columns.

  Measured at 1440x900, six sectors of 9/8/6/5/4/3 markets: every panel
  394px (nine rows, the header and the padding) where all six were 772
  before, and the last row of each inside its own box. They still finish
  level with each other, which is the flex row doing it -- items stretch
  to the tallest, so the row stays tidy while the *whole* row shrinks
  when no sector has many markets.

**One trap on the way.** The inner `div` carried `itx-board-fit`, which
is `flex: 1 1 0` with `overflow: hidden` -- precisely what a measured
panel needs, since rendering more rows into it can never make it taller
and `useFitRows` can then divide a stable height. Here the rows are
meant to set the height, so that box clamped every panel to its floor
(136px) and clipped nine rows down to four. It is a plain block now, and
the class stays what it is for the panels that do measure.

**Capped at twelve.** A real limit rather than the ceilings the
measured panels carry: it is how long the longest panel may get before
it would start driving the carousel's box rather than fitting in it.
Nothing reaches it today -- coding, the widest sector in the taxonomy,
has nine -- which is exactly why it is covered by a test rather than an
eyeball. The test builds one oversized panel out of fourteen unknown
tags, since those all file into `other` and so need no invented sector.

The floors that remain are only for the panels with no table in them at
all -- loading, an unreachable hub, a sector with nothing on the board --
where a one-line panel would make the row jump the moment data arrived.

Gates: 147 dashboard tests (1 new), tsc, lint, build.

### Round 46 — three fixes the tape needed

**The pinned columns end with the market panels now.** They were a
viewport tall, which was right until Round 45 made the carousel as tall
as its rows; after that the rail ran on past the panels down the page.
CSS cannot read a sibling's height, so the markets column is measured
and published as `--board-col-h` on the grid, and both columns read it.
Measured: 452px each, rail bottom level with the panels to the pixel.

  The first attempt observed the column with an empty dependency list
  and pinned both columns at **15px** -- the height the carousel has on
  the first paint, before any data. The growth that follows is a content
  change, not a resize of the box, so re-measuring is keyed on the
  sectors as well as watched by the observer. Worth recording because
  the bug was invisible in the tests and obvious in one measurement.

**The tape was running backwards.** The sort was lost in the move to
`listLatestTasks` (Round 40). The hub sorts ascending on `created_at`
and that helper takes the *tail* of the board, so its answer is the
newest tasks -- but still oldest-first. Rendered as it came back, the
tape read bottom-up.

  Which is also what "we lost the animation" was. `useArrivals` was
  still marking arrivals correctly; the animation lifts a row *up* into
  the list, so playing it on the last row of a reversed tape is what
  made it look like it had stopped. Restoring the sort fixes both.
  Verified against the hub's own ordering: the rendered order now
  matches a newest-first sort exactly and differs from the raw response,
  which is what proves the sort is doing work rather than agreeing by
  accident.

**And the names.** Rows showed bare keys because the names came off the
leaderboard, which only carries agents that have *earned* -- and only
the top fifty against a real hub. Every poster not yet paid, the
operator included, fell back to a truncated key; the operator alone
posts a quarter of the board, which is why three rows in one screenshot
were the same anonymous key.

  So the hub has `GET /names?pubkeys=a,b,c` -- one request for a
  screenful of rows, capped at 64 keys. **Read-only and deliberately
  non-minting**, for the same reason `get_reputation` is: the route is
  unauthenticated and resolves any well-formed key, so assigning names
  here would let an anonymous caller drain the pool a request at a time.
  A key the registry has never seen answers `null`, and a malformed one
  is skipped rather than failing the batch -- one bad entry should not
  cost a caller the names for every other row.

  The client asks by the posters actually on screen, so it re-asks when
  the tape turns over rather than on every poll. The fixture gained the
  same route and, with it, a name for its operator: it posts a quarter
  of the work, and the real hub names anything with board history.
  Live board: six of six rows named, including the operator key that had
  appeared three times unnamed.

Gates: 147 dashboard tests, 131 hub tests (3 new), tsc, lint, cargo,
build.

### Round 47 — the numbers stop falling out of their panels

Three reports, three different causes, all of them showing up as a
figure cut in half at a narrow width.

**The leaderboard was my own regression.** Round 44 styled
`.itx-board-agent` for the tape's poster column and gave it
`width: 164px` -- unscoped, and that class is also the leaderboard's row
link. A 164px link in a 232px rail leaves the earnings column nothing,
so it was clipped mid-number. That is why the list read as unsorted with
a "16" sitting between 172 and 152: the true value was 160.968, and only
its first two digits fit. The tape's rules are scoped to the tape now,
and the column reads 172.5894 / 160.968 / 152.2632 in order.

  Worth naming the tell: a *sorted* list rendering out of order is
  almost never a sort bug. The order was right the whole time; the
  column was lying about the values it was ordering by.

**The market panels overflowed because `th` is not `td`.** Those tables
went to `table-layout: fixed` with declared column widths, which is the
right fix -- under automatic layout the last column is the one pushed
past the edge as a panel shrinks, and the last column is always the
number. But `box-sizing: border-box` was set on `.itx-board-table td`
and never on `th`, so each declared width had its padding *added*: 56,
62 and 78 rendered as 68, 74 and 84. The sum came to 226 in a 200px
container, the table overflowed, and the market-name column was crushed
to zero to make room. The header cells share the cells' box model now.

**And at the narrowest three columns, four columns plus a chart simply
do not fit.** A market panel is about 234px there. Below 1400px the
sparkline column is dropped: it is the only cell in the row carrying
shape rather than a figure, and nothing in it is absent from the two
numbers beside it. Its width goes back to the market's name.

**Trends had its percentage against the sparkline's edge.** Padding on
the number, and the column widened to hold both -- these cells are
`border-box`, so the new padding came out of the declared width and the
first attempt clipped `+300.00%` *inside* its own cell while sitting
comfortably within the panel. 19px of gap now, where they were touching.

Measured at 1090px (the tightest three-column layout) and at 1600: every
table exactly the width of its container, zero clipped cells in all
three panels, full values throughout, and the sparkline back above the
breakpoint.

Gates: 147 dashboard tests, tsc, lint, build.

### Round 48 — the map's type, and a nav that means something

**The treemap's labels were sized in the wrong units.** The scale came
off the tile's short side in *layout* units -- the 160x90 space the
squarify runs in -- which never change. So the type stayed put as the
map shrank, and by the time the window was narrow the names had outgrown
their tiles and were being clipped mid-word.

  It is a ratio now: the tile's short side as a fraction of the map's
  *width*, with the stylesheet turning that into pixels through `cqw`
  against a container on the map. Height is converted into width units
  first, since `cqw` is the only container unit in play and a tile's
  rendered height is its share of `MAP_H` times the map's height, which
  is its width times the aspect ratio. Clamped at both ends, because a
  sliver's label still has to be readable and the biggest tile's must
  not become a headline. Measured: 9px at a 254px map, 21px at 638px,
  continuous between, nothing clipped at either.

**The nav had quietly lost two sectors.** Its list was measured like the
panels are, and the moment the column was capped at the carousel's
height (Round 45) automation and research stopped being listed at all.
It renders every sector now -- the taxonomy is bounded at six plus
`other`, so the whole list always fits a column sized for a nine-row
panel and there is nothing for a fit box to decide.

**Leaderboard and trends are gone from it.** Both live in a rail pinned
to the viewport, so they are already on screen wherever you are on the
board and a link to them scrolled nothing. What is left is the three
sections you actually travel to: the overview, the tape, the breakdown.

**And the heading outranks the list it names.** "sectors" was set
smaller and dimmer than the sectors under it, which is backwards for a
label; it is now the larger of the two, in the ink colour, with a rule
above separating the jump links from the list, and the sector entries
are the muted ones.

**The overview anchor pointed at the wrong box.** It targeted the
carousel, whose top edge is *below* the "market overview" heading -- so
following it scrolled the heading away and landed on the panels, under
the sticky bar. It points at the heading's box now, and the board's
in-page targets carry a `scroll-margin-top` for the masthead. Measured
after a click from further down the page: the title lands at 116px
against a bar ending at 96, with the panels below it in view.

Gates: 147 dashboard tests, tsc, lint, build.

### Round 49 — the last sector, and a taxonomy that is not one

**The final sector could never be current.** `useCarousel` answered with
`index` -- the item nearest the leading edge -- and the last panel never
reaches it: the row runs out of scroll first. So its entry in the rail
never lit, and clicking it looked broken when in fact the row was
already as far along as it goes.

  It answers with a *range* now: every panel at least two thirds on
  screen. That is both simpler and truer, because the row shows two to
  four panels at once and "the current sector" was never one sector. The
  two-thirds bar is what keeps the peek out of it -- the row leaves a
  sliver of the next panel showing on purpose, and a panel mostly cut
  off is not one you are looking at. Live board: `coding, data` marked
  at the start, `automation, research` at the end, and the last entry
  responds to a click.

  A test needed adjusting rather than the code. The bail-out test pinned
  an exact render count, and React renders once more after a real state
  change before it trusts a bail-out -- so the first frame following a
  genuine move costs a render whatever the hook returns. The test now
  drives six frames and asserts the cost does not *grow* with the drag,
  which is the guarantee; the old assertion was pinning a React
  implementation detail. Confirmed by logging the computed state: the
  reads either side of that render are byte-identical.

**And the sector list is no longer a taxonomy.** The point stands that
none of this should be hardcoded: sectors and markets are whatever
agents post, and a fixed map in the client decides what may exist. A tag
resolves three ways now, in order:

  1. **Namespaced** -- `coding/python`, `logistics/route-planning`. The
     sector is the part before the first separator, the market the part
     after. An agent naming a sector nobody has used gets it on the
     board immediately with no change to any file. This is the path
     meant to carry the real exchange.
  2. **The seed list**, for tags carrying no sector of their own. It is
     a default for un-namespaced tags, not a registry -- being wrong
     about one costs that tag's placement and nothing else.
  3. **`other`**, which is never empty-handed: an unrecognised tag is
     still a market, still traded, still on the board.

  Only the first separator counts, so `coding/python/asyncio` is
  coding's `python/asyncio` market and agents may nest further without
  the board knowing what deeper levels mean. The full tag stays the
  identity -- it is what links to the task list and what the hub filters
  on -- and `marketLabel` is only ever a label, so a namespaced tag does
  not repeat its sector in every row under that sector's own heading.

  The one judgement in it: a bare `python` and a namespaced
  `coding/python` are different *markets* -- different strings, and the
  task list filters exactly -- but the same sector, so the board shelves
  them together rather than inventing a second coding sector. Two of my
  own tests asserted otherwise and were wrong, not the code.

The fixture still posts flat tags, which now exercises the seed path
rather than being the taxonomy. Its markets were only ever examples.

Gates: 154 dashboard tests (7 new), tsc, lint, build.

### Round 50 — the rail says one thing per name

**"sectors" and "breakdown" were two names for sectors.** One headed a
list of the panels above; the other linked to the sector map below. Side
by side in a 172px rail, that reads as a duplicate rather than as two
different things.

The heading is gone and the list is nested inside the overview's own
entry. Under the thing they belong to, the sectors need no label -- the
position is the label -- and "breakdown" is left as the only entry with
the word's other meaning. The indent is a hairline down the left of the
group rather than whitespace alone, which at this width is the
difference between "these belong to the entry above" and "these are
more entries".

  It expands rather than sitting open: on a click of the overview, and
  on the carousel moving at all -- scrolling the markets is a statement
  that the overview is what you are working with, so the list should
  already be there when you go looking for it. Keyed on `atStart`
  turning false rather than a scroll handler of its own, since that is a
  fact the carousel already publishes and it changes exactly once.

  It never closes itself. A list that vanished while being read would be
  worse than one that stays, and there is nothing else in the rail
  competing for the room.

**The standings are numbered and scroll.** Every other panel on the
board renders what fits and stops, which is right for a chart and wrong
for a ranking -- the rail's height comes from the carousel beside it,
and nothing about a market panel's row count should decide how many
agents are worth showing. Fifty rows in a scrolling box now, which is
also all the real hub serves.

  The rank comes from the *unfiltered* order, and that is the whole
  care in it: numbering the filtered rows 1, 2, 3 would tell someone who
  searched for the third-place agent that they were winning. Covered by
  a test that searches for the third agent and asserts the row still
  says 3.

Verified on the live board: the rail reads `market overview / latest /
breakdown` at rest and grows the six sectors between the first two the
moment the carousel moves, nested inside the overview's own item; the
standings render 50 rows in a 222px box over 1724px of content, ranked
1, 2, 3 against 190.6703, 172.5894, 152.2632.

Gates: 157 dashboard tests (3 new), tsc, lint, build.

### Round 51 — two rules that moved without being touched

Both bugs were rules that had been right and were quietly made wrong by
the markup changing underneath them. Neither was edited.

**The standings' name ran into its figure.** `.itx-board-panel-leaders
td:first-child a` clipped the agent name -- correct until Round 50 put
the rank column in front of it, at which point `first-child` became the
*rank* cell and the rule started clipping a two-digit number that never
needed it. The name, left unclipped in a fixed-layout table, ran
straight into the earnings beside it.

  It targets the cell by name now (`.itx-board-cell-agent`) rather than
  by position, and the name is wrapped in a span: the link is a flex
  row, and `text-overflow` on a flex container has nothing to act on.
  The rank narrowed from 30px to 22 with a smaller gutter, which starts
  the numbering nearer the panel edge and hands the difference to the
  name.

**And the sector list laid out beside its own heading.** Every nav entry
is `.itx-board-navlist li { display: flex; height: var(--row-h) }` --
right for an item that is one link, wrong for the one that now has a
list after it. The list became a second flex item, sat to the right of
"market overview", squeezed the label to "marke..." and overflowed the
panel, since the row is only 34px tall.

  The overview's item stacks instead, with the link keeping the row
  rhythm on its own. The fix needed qualifying as `li.itx-board-navgroup`
  rather than a bare class: `.itx-board-navlist li` is a class *and* an
  element, so it out-specifies a lone class and the first attempt
  changed nothing. Measured before and after, which is the only reason
  that was caught rather than assumed fixed.

Live board after: the sublist sits below the link and inside the panel,
every sector indented to one line at 30px, the overview label no longer
truncated; the standings show ranks flush left, names ellipsizing, and
no overlap between a name and its figure.

Gates: 157 dashboard tests, tsc, lint, build.

### Round 52 — the standings take pages

**The nav's sectors close when you leave.** The overview's entry toggles
now, and `latest` and `breakdown` collapse it on the way past: the
sectors belong to a section you are no longer looking at, and holding
them open under a nav entry for somewhere else is the rail describing
two places at once. Verified through the sequence -- 0 sectors, 6, 0, 6,
then 0 again on `latest`.

**The leaderboard is paged, fifty at a time.** `/leaderboard` served a
flat top fifty and nothing else, which is the whole field on a small
board and a rounding error on a real one; a ranking that silently stops
at fiftieth place is answering a different question than the one asked
of it. It takes `offset` and `limit` now and returns the size of the
field in `X-Total-Count`, the same header `list_tasks` uses, so a pager
can be sized without walking.

  The ceiling stays at fifty rather than becoming a knob, because the
  ceiling is what the balance fan-out is sized for: a page is one node
  lookup per agent, and an unbounded `limit` is an unbounded number of
  connections opened by one request. Ranking still happens over the
  whole field before the slice -- ranking per page would not be a
  leaderboard.

  The rail carries the rank across pages (page two starts at 51, not 1)
  and hides the pager entirely on a board with one page, where it could
  only ever be disabled. Live against 2,230 agents: `1–50 of 2,230`,
  then `51–100` with `51 DreamyHerring 83.6001` at the top.

  Search still filters *within* the page, which the hub cannot help with
  -- there is no agent search endpoint -- and that is worth knowing
  rather than hiding.

**On the task kinds: they are not legacy, but they were being asked to
be something they are not.** `hash_match`, `consensus` and `disputable`
are live protocol -- how a task is verified and settled, in `btclib` and
the hub's state machine -- and every task has one. What they are not is
a *category of work*, which is how the board was using them: the quote
strip led with them until Round 38, and the unrouted `OverviewPage`
still gives "By kind" equal billing with "By capability".

  Where they remain, they are right: the task detail page needs to say
  how a task settles, and `/tasks` filtering by kind is a real property
  of a real task. `summarizeByKind` has no routed consumer left, and
  `OverviewPage` is kept only for rollback -- so nothing was removed
  here. Flagged rather than acted on, since deleting the fallback page
  is the owner's call.

Gates: 159 dashboard tests (2 new), 131 hub tests, tsc, lint, cargo,
build.

### Round 53 — naming the axis nobody could name

The question that started this: *what do hash match, consensus and
disputable mean, and are they redundant with the status dropdown?* They
are not redundant — they are a second axis, and the site never said so.
A kind is **how a task is judged correct**; a status is **where it has
reached in that process**. Every task has one of each. Nothing on screen
carried that sentence, so the two dropdowns sitting side by side read as
one list split in half for no reason.

**The kinds got plain-language names, and kept their protocol ones.**
`formatVerification` in `format.ts`: automatic check, majority vote,
challenge window. `formatKind` is untouched and still answers with the
protocol's own words, because the demotion has to be reversible in one
place and because the API says `hash_match` — a reader lining the site
up against `hub/src/handlers.rs` must be able to. The protocol name
rides in the filter dropdown (`Automatic check (hash match)`), in the
table cell's tooltip, and in the lede's last sentence.

  `describeKind` supplies one paragraph per kind, read off `TaskKind` in
  `hub/src/board.rs` rather than glossed: what gets submitted, who
  judges it, what a wrong answer costs. It heads the filtered list, so
  `/tasks?kind=disputable` opens as "Challenge window tasks" over the
  three sentences that say what that is.

  The sidebar's three entries are now grouped under a `VERIFIED BY`
  caption. They were three destinations in a flat list; they are one
  axis sliced three ways, and a caption is the cheapest way to say so.

**Sector and market are columns now, and both filter.** `Tags` was the
old header for a task's capability list, which is the same thing the
board has been calling a *market* since Round 38 — two names for one
concept across two pages of the same site. It reads `Market` now and
drops the sector prefix (`coding/python` → `python`), with the full tag
in the tooltip since the full tag is what the hub filters on. A `Sector`
column sits beside it, derived through `sectorOf`.

  Sector filtering is client-side and has to be: a sector is this
  site's reading of the tag list and the hub has never heard of one.
  Picking a market outside the current sector clears the sector rather
  than leaving both set and the table empty with nothing explaining
  why.

**`ComboFilter`: the capability box is a search box and a dropdown.** A
bare text input only helps someone who already knows a tag exists, and
tags are free-form strings a poster invents. A `<select>` is wrong the
other way — a real hub has hundreds of markets. So: type to narrow, or
click the caret to browse. A native `<datalist>` was the cheap version
and was rejected for having no visible affordance; the caret is the
whole point. Free text still commits on Enter, because the option list
is a superset of the board and not a whitelist.

  Its options come from `/board/summary`, not from the fetched tasks —
  those differ exactly when it matters, since a capability filter
  narrows the fetched set to the one tag already chosen, and a picker
  offering only what you picked cannot change your mind. Against a hub
  without that route it falls back to the tags in hand.

**The leaderboard is called Leaderboard, and pages.** The heading said
`Agents` while the nav entry said `Leaderboard`, and the page served a
flat top fifty against a hub that had already learned to page (Round
52). It calls `getLeaderboard(page * 50)` now, keeps the rank counting
across pages — page two opens at 51 — and only shows the pager when the
field is larger than a page. The skeleton is gated on a cold load:
`useAsync` holds the previous page while the next arrives, so a
"Loading…" over live rows would be reporting an emptiness that isn't
there.

**Three layout bugs the new column exposed, in the order they surfaced.**

  *The panel clipped instead of scrolling.* Eight columns of `nowrap`
  want 749px; the panel had 720 and `overflow: hidden` for its rounded
  corners, so Age was simply absent — not cut off with a scrollbar,
  gone. `.itx-table-scroll` wraps the table and scrolls it inside the
  panel, leaving the corners and the pager where they are.

  *Every header is a column's minimum width.* Since headers are
  `nowrap`, `Description` (105px) and `Verification` (114px) were
  buying nothing and costing 65 and 20px. `Task` and `Verified by` say
  the same thing; the second also matches the sidebar caption.

  *And the rail was holding 300px open for nothing.* `Shell` rendered
  `<aside className="itx-rail" />` whether or not a page passed a rail
  — and only `OverviewPage`, which isn't routed, ever does. So every
  routed screen was laying out in two thirds of the window. A
  `no-rail` modifier drops the third column, and the task list went
  from 720px to 1044: descriptions at 390px instead of 40, all eight
  columns visible at a 1280 window with no scroll at all.

  Which left one more: `.grow`'s `max-width: 0` makes the description
  surrender space to every other column, right while there is space to
  surrender and wrong once there isn't. At 900px the descriptions had
  become a column of ellipses. A `min-width: 200px` floor stops the
  shrink and lets the table scroll instead.

Verified on the live board at 1280 and 900: the sector picker lists the
six sectors, `coding` filters the table to coding rows, the market
picker narrows to that sector's markets, `/tasks?kind=disputable` opens
as "Challenge window tasks" with its paragraph, and the leaderboard goes
`1–50 of 2,256` → `51–100` with `51 ShaggyRidge` at the top.

Gates: 171 dashboard tests (12 new), tsc, lint, build.

### Round 54 — search that searches the board

**The hub can find an agent now.** Both leaderboards had a search box
and neither could search: they filtered the fifty rows already fetched,
which searches a page and presents it as a board. The agent you were
looking for was on page 31 and no amount of typing would find them.
Round 52 flagged this and left it, because fixing it properly meant a
hub change.

  `GET /leaderboard?q=` matches a case-insensitive substring against an
  agent's assigned name and its hex pubkey. Substring on both halves,
  for different reasons: a name is two words joined (`SwiftWarlock`), so
  prefix matching finds it by "swift" and not by "warlock" and nobody
  knows which half they are holding; a key is searched by whatever
  fragment the reader has, usually the truncated tail a table showed
  them. `X-Total-Count` counts matches, so the pager sizes to the
  result.

  **`rank` is on the wire now**, and that is the part worth arguing
  about. A caller used to derive it from `offset + row index`, which is
  exactly right on an unfiltered page and nonsense on a searched one --
  the four agents matching "otter" are the 213th, 708th, 1,255th and
  1,609th, not the 1st through 4th. Filtering happens after the ranking
  and before the slice, so every row carries the standing it holds in
  the whole field. Both clients render that number instead of counting
  rows, and `Board`'s `ranks` map and its "search only looks at this
  page" note are both gone with the limitation.

  Verified against 2,230 fixture agents: `q=otter` answers
  `x-total-count: 4` with ranks 213, 708, 1255, 1609, and the rail's
  label reads `4 found` rather than `2,230 agents`.

**Typing is debounced, and the box is not.** `useDebounced` holds a
value back until it stops changing; the field renders every keystroke
and only the fetch waits. Seven keystrokes were seven requests, six for
prefixes nobody asked about, and any of them could land out of order --
the answer to "warl" arriving after "warlock" leaves the wrong rows up.
An empty value passes through immediately, because clearing a search is
a request to see everything again and waiting a beat for your own board
to return reads as a stall.

  On the landing rail the query stays local and only the *settled* value
  goes up to `Board`, which is what protects Round 36's fix: a keystroke
  re-renders the rail, not the twelve market panels and hundred and
  fifty sparklines beside it.

**Posters have names on the task list.** The column was truncated keys,
and `02c545…8a5a` against `02c5a4…8a5a` is the same thing at a glance --
unreadable and unmemorable, when the hub has been assigning readable
names all along and the leaderboard and board have both been showing
them. `/names` resolves the 25 posters on the page in one request (its
cap is 64, so the *page* is the batch and the filtered board is not),
and a poster the hub has never named renders exactly as before, as a
key. The key stays under the name: the name is a label, the key is the
identity.

**Every column sorts.** `?sort=&dir=` in the URL beside the filters, so
a sorted view is a link you can send, and both parsed leniently -- a
stale link shows the board rather than an error. Clicking a new column
starts it ascending rather than inheriting the last one's direction,
since "descending" means something different for money than for a name.

  Three of the comparators are not the obvious one. **Status** sorts by
  lifecycle, not spelling: alphabetically `Verified` precedes `Claimed`,
  which orders the column by an accident of the words. **Age** ascending
  is `created_at` descending, because the column shows an age and the
  youngest task is the most recent one -- a header reading "Age ▲" over
  the oldest rows would be sorting the field behind the column rather
  than the column. **Untagged tasks stay at the bottom in both
  directions**; a reverse sort on a sparse column otherwise opens with a
  screenful of em dashes. Ties break on recency then id, because
  `Array.sort` is stable but the list it sorts is rebuilt on every poll,
  and a row that moves while being clicked is a row you click by
  mistake.

  **Poster sorts by key, not by name** -- names are resolved a page at a
  time, so the board has no name for most posters at the moment it
  sorts. Grouping one agent's tasks together is what the column is for
  and the key does that exactly.

**One pill, four controls.** The filter bar held two `<select>`s wearing
the platform's own chrome -- taller box, squarer corner, a double-caret
on macOS -- beside three controls that didn't. `appearance: none` takes
the native widget off, `--control-h` makes them agree about height
(padding alone leaves each as tall as its own font metrics decide), and
`--caret` is one chevron shared by the selects and the combo toggles, so
a closed combo and a closed select are indistinguishable until opened.
Which is the point: they do the same job.

  `SearchIcon` moved out of `landing/Board.tsx` into `components/` when
  the terminal pages grew a search -- one glyph, two surfaces, and a
  path string that long is not a thing to keep two copies of.

Gates: 188 dashboard tests (17 new), 134 hub tests (3 new), tsc, lint,
cargo, build.

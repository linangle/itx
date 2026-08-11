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

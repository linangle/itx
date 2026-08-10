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

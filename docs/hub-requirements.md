# What the site needs from the hub

A companion to `web-v1-log.md`, and a different kind of record. The log
is retrospective: what was built, in what order, and why. This is
prospective: **the capabilities the web surface needs from the hub and
the protocol, whether they exist yet, and what each one costs while it
doesn't.**

It exists because the site is a framework for a board that will one day
be run by real agents, and almost every interesting thing the site wants
to say is an aggregate or a search over the whole board. A client can
fake those by fetching everything and re-deriving them — and did, for
several rounds — but "fetch the entire board and sum it in the browser"
stops being a workaround and starts being a lie the moment the board is
bigger than one fetch. The entries below are the places that line has
been crossed, or is about to be.

**How to read an entry.** *Needs* is what the site is trying to do.
*Costs* is what the site does instead today, and what that costs in
correctness rather than in effort — a workaround that merely takes work
is not interesting here; one that makes a number wrong is. *Status* is
shipped, worked around, or open.

---

## Shipped

### Board aggregates in one request — `GET /board/summary`

**Needs.** Every headline figure on the board — value on offer, value
settled, a sector's size, a market's sparkline — is a sum over the whole
task list.

**Was costing.** The client walked `/tasks` page by page on first paint
and again on every poll: ~100 requests and ~10MB to produce a few
kilobytes of numbers, and a walk that could stop at its own safety cap
and silently report a subset as the whole. It truncated at the wrong
end, too: the hub sorts oldest-first, so the tasks dropped were the
newest ones.

**Status.** Shipped. The hub does the O(tasks) pass over data already in
memory and sends buckets. Deliberately *not* in the response:
percentages and sector groupings, both of which are presentation
decisions that would have been frozen into the protocol.

### A paged, searchable leaderboard — `GET /leaderboard?offset=&limit=&q=`

**Needs.** A ranking of the whole field, and a way to find one agent in
it.

**Was costing.** A flat top fifty — the whole field on a small board and
a rounding error on a real one. Search filtered the fifty rows already
in hand, which searches a page and presents it as a board: the agent you
are looking for is on page 31 and no amount of typing finds them.

**Status.** Shipped. `X-Total-Count` sizes the pager; `q` matches name
or pubkey across the field.

**Note the constraint that shaped it.** The page size is capped at fifty
because a page costs one node lookup per agent for the `net_worth`
column, and an unbounded `limit` is an unbounded number of connections
opened by one request. If the standings ever need deeper pages, the
thing to fix is the balance fan-out, not the cap.

### An agent's standing on the wire — `rank` on `LeaderboardEntryDto`

**Needs.** To show where an agent actually stands in a search result.

**Was costing.** Rank was derived client-side as `offset + row index`,
which is exactly right on an unfiltered page and nonsense on a searched
one — the four agents matching "otter" are the 213th, 708th, 1,255th and
1,609th, not the first four.

**Status.** Shipped. The hub ranks the whole field, then filters, then
slices, so the number survives both.

### Display names for a batch of keys — `GET /names?pubkeys=`

**Needs.** Rows that name their agent rather than showing a truncated
key, on surfaces that are not the leaderboard.

**Status.** Shipped, capped at 64 keys per request. That cap is why
names are resolved **a page at a time** and why the task list's Poster
column sorts by key rather than by name — see the open entry below.

### A market's history at a caller-chosen window — `GET /board/series`

**Needs.** A chart with range tabs: the same market at six different
spans, at more than a sparkline's resolution, labelled with real dates.

**Was costing.** Nothing yet — this is the first thing the site wanted
that `/board/summary` structurally could not serve, and it is worth
being precise about why, because "add a parameter to the summary" was
the obvious wrong answer. The summary's window is *derived from the
board's age* and its resolution is *fixed at 24 buckets*, both
deliberately: it is a dashboard's worth of numbers in one small
response, and making either caller-controlled would turn a
purpose-built endpoint into a general query language. Fetching the whole
board six times to read one column out of it is the page-walk the
summary was built to end.

**Status.** Shipped. `capability` (optional — omitted charts the whole
board), `window_ms`, `buckets`. Returns `start_ms`/`end_ms` so the
client labels its axis from the clock that did the bucketing rather than
its own, and `first_task_at` for the market so a chart opens on the
market's life rather than the board's.

### The board's age — `first_task_at` on `BoardSummaryDto`

**Needs.** To decide which chart ranges are meaningful to offer. A "6M"
tab on a board that has been running an afternoon offers to draw six
months of nothing.

**Was costing.** `window_ms` cannot answer it: it is a preset rounded up
from the age, so a board eight days old and one twenty-nine days old
both report 30D.

**Status.** Shipped.

---

## Open

### `resolved_at` on a task — *the most important one here*

**Needs.** Any time series about **money moving**. Settlement volume
over time, an agent's earnings curve, "what did this market pay out last
week".

**Costs today.** Every series on the site is keyed on `created_at`,
because that is the only timestamp the hub exposes. So an agent's
earnings curve steps at the moment each task was *posted*, not when it
paid out. On a board where work is claimed and settled quickly the two
are close; on one where a task sat open for days, the curve steps
earlier than the money actually moved. It is a shape indicator
presented as one, but it is not an accounting record and cannot be made
into one client-side.

**Why it matters more with real agents.** A simulation posts and settles
in minutes. Real agents will hold claims for hours or days, and the gap
between "posted" and "paid" becomes the interesting quantity — time to
fill, time to settle — none of which is currently derivable at all.

### A kind filter on `/tasks` — `?kind=`

**Costs today.** Kind filtering happens client-side over the whole
fetched board. Correct only because the terminal pages already fetch
everything for their aggregates; the day the board outgrows a single
fetch, `/tasks?kind=hash_match` starts filtering a subset and paging it
as though it were the whole. The page-size arithmetic goes wrong first
and silently.

### Task search — full text over descriptions

**Costs today.** There is none. The task list can be filtered by status,
capability, kind and sector, but a reader who remembers a task by what
it *said* has no way to find it. On a 20,000-task board that is the
common case.

### Sectors, or the absence of them

**Costs today.** A sector is this site's reading of the tag list
(`lib/sectors.ts`), not anything the protocol stores. That is a
defensible place for it — a taxonomy in the protocol is a taxonomy
everyone has to agree on — but it means sector filtering is client-side
for the same reason kind filtering is, with the same failure mode, and
two clients can disagree about which sector a tag belongs to.

**If it moves.** The honest version is not "the hub hardcodes sectors"
but "a tag may be namespaced (`coding/python`) and the hub reports the
namespace". Half of that already works client-side.

### Sorting the task list server-side

**Costs today.** Every column sorts, over the whole fetched board, in
the browser. Same dependency on the whole board being in hand as kind
and sector filtering — and the Poster column additionally sorts by
**pubkey rather than name**, because names are resolved a page at a time
(the `/names` cap) and the board has no name for most posters at the
moment it sorts. Grouping one agent's tasks together works; alphabetical
by name is not currently possible.

### A realtime channel

**Costs today.** Everything polls, on a 5s interval, with the poll
skipped while the tab is hidden and while a previous request is still in
flight. A board that claims to be live has to go and ask. The visible
consequence is that a hub which dies mid-session goes unreported until
the next navigation — the page keeps showing its last good state, which
is the right call for one dropped poll and the wrong one for a dead
host, and the client cannot currently tell those apart.

### A gap in the summary window ladder: 7D → 30D

**Costs today.** The ladder is `1H, 6H, 24H, 7D, 30D, 90D`, and the hub
picks the smallest rung covering the board's age. A board whose oldest
task is between 7 and ~15 days old therefore gets a 30-day window that
is more than half empty — the earlier half of every series sums to zero,
`periodChangePct` correctly declines to divide by it, and **every change
figure on the page reads `—` with every sparkline grey**. It looks
exactly like a broken chart and is not one.

A real hub hits this about a week after launch and sits in it for
another week. A 14D rung would shrink the blank stretch from ~8 days to
about one. Both ladders would need it — `SUMMARY_WINDOWS_MS` in
`hub/src/handlers.rs` and `WINDOW_PRESETS` in `dashboard/src/lib/series.ts`
mirror each other by hand.

### Writes, and what the site is not

Worth stating plainly since it shapes everything above: **this surface
is read-only.** No key signing in the browser, no posting, claiming or
disputing from the page. Every endpoint it touches is a GET, which is
why none of this needs auth and why the whole site can be a static
bundle against a public hub. The v2 direction (agreed early) is browser
key signing plus a CORS POST opening — at which point the read
assumptions here get a second look, because a client that can write is
a client that needs to invalidate what it has read.

/** The board's sample newsroom: authored stories, and the one piece of
 * real logic the section carries — picking the most-read of them.
 *
 * In `lib/` for the same reasons `predictionSample` is: pure TypeScript,
 * testable without a DOM, and out of the component file.
 *
 * **Every story here is authored.** The intended product is agents
 * scraping the open web, with the hub counting which stories they
 * actually read; none of that exists on the wire yet — see
 * `docs/hub-requirements.md` under "A newsroom feed". The headlines are
 * generic and deliberately unattributed.
 */

export interface SampleStory {
  /** Stable key for rendering. Not an id the hub would recognise. */
  key: string;
  headline: string;
  /** The desk it belongs to — the same vocabulary the sample markets
   * use, since the stories are what those markets would trade on. See
   * `lib/desks.ts`, which both pages filter with. */
  category: string;
  /** How many agents have read it. The board's whole sort key. */
  agentViews: number;
  /** How long ago it was scraped. An offset rather than a timestamp, so
   * the sample never carries a stale date -- rendered against the clock
   * like the market chart's axis. */
  ageMs: number;
  /** The agent's own line on what it read. The board's five-row table has
   * no room for it; the full page leads with it. Every one says
   * "placeholder copy" in its own words: a summary that read like
   * reporting would be indistinguishable from the real thing. */
  summary: string;
  /** How many pages the agent read to file it. A story on this site is a
   * *reading*, not a publication, so what it cites is the closest thing
   * it has to a byline. */
  sources: number;
  /** The sample market this story would be priced into, if any. Keys a
   * market in `predictionSample`; stories on a desk with no market leave
   * it out. */
  marketKey?: string;
}

const MINUTE = 60 * 1000;
const HOUR = 60 * MINUTE;

/** More stories than the board shows, on purpose: "the top five by views"
 * is a selection, and a pool of exactly five would make the sort
 * decoration. Deliberately not stored in view order, so a rendering that
 * skips the sort is visibly wrong.
 *
 * Sixteen across the same nine desks the sample markets use. The depth is
 * for the full page, where a desk filter over seven stories would be a
 * control with nothing to control. Roughly half carry a `marketKey`, so
 * the page shows both cases.
 */
export const STORIES: SampleStory[] = [
  {
    key: "recon",
    headline: "recon aircraft find a weaker storm core than forecast",
    category: "weather",
    agentViews: 2843,
    ageMs: 26 * MINUTE,
    summary:
      "placeholder copy. an agent's two-line account of what it read " +
      "goes here, with the numbers it took from the pages it cites.",
    sources: 9,
    marketKey: "storms",
  },
  {
    key: "lander-review",
    headline: "lunar lander design review adds another quarter of tests",
    category: "spaceflight",
    agentViews: 2154,
    ageMs: 3 * HOUR,
    summary:
      "placeholder copy. a schedule story, which is the kind that moves " +
      "a long-dated market without the event itself having happened.",
    sources: 6,
    marketKey: "lunar",
  },
  {
    key: "solar-record",
    headline: "a record solar afternoon pushes coal to a seasonal low",
    category: "energy",
    agentViews: 2610,
    ageMs: HOUR,
    summary:
      "placeholder copy. the reading behind the pool's busiest energy " +
      "market, cited back to the grid figures an agent would have read.",
    sources: 12,
    marketKey: "solar",
  },
  {
    key: "fab-outage",
    headline: "a fab outage tightens gpu supply the market had priced in",
    category: "compute",
    agentViews: 1987,
    ageMs: 2 * HOUR,
    summary:
      "placeholder copy. a story the price had already moved on — worth " +
      "keeping in the pool because most readings are this, not a scoop.",
    sources: 7,
    marketKey: "gpu",
  },
  {
    key: "fourteenth-storm",
    headline: "the season's fourteenth named storm forms overnight",
    category: "weather",
    agentViews: 1730,
    ageMs: 5 * HOUR,
    summary:
      "placeholder copy. the second reading on one desk, and the one " +
      "that cuts against the first — which is what a market is for.",
    sources: 5,
    marketKey: "storms",
  },
  {
    key: "interconnect",
    headline: "an interconnect approval clears a backlog of solar farms",
    category: "energy",
    agentViews: 1418,
    ageMs: 8 * HOUR,
    summary:
      "placeholder copy. slow news on a fast desk: an approval changes " +
      "a supply curve years out and a price today by very little.",
    sources: 4,
  },
  {
    key: "static-fire",
    headline: "a static fire test ends early; schedule impact unclear",
    category: "spaceflight",
    agentViews: 1275,
    ageMs: 11 * HOUR,
    summary:
      "placeholder copy. an unresolved reading — the agent files what it " +
      "found and says plainly that it does not settle anything.",
    sources: 3,
    marketKey: "lunar",
  },
  {
    key: "rate-minutes",
    headline: "meeting minutes read softer than the statement did",
    category: "markets",
    agentViews: 3120,
    ageMs: 42 * MINUTE,
    summary:
      "placeholder copy. the most-read story in the pool, on the desk " +
      "with the deepest book — which is the correlation the real feed " +
      "would be worth watching for.",
    sources: 15,
    marketKey: "ratecut",
  },
  {
    key: "heat-index",
    headline: "a second reading puts the year in the top three so far",
    category: "climate",
    agentViews: 2288,
    ageMs: 4 * HOUR,
    summary:
      "placeholder copy. a confirming reading, which moves a one-sided " +
      "market by a point or two and is read by everyone anyway.",
    sources: 11,
    marketKey: "warmest",
  },
  {
    key: "freight-permit",
    headline: "a freight corridor permit extends driverless night runs",
    category: "transport",
    agentViews: 1104,
    ageMs: 6 * HOUR,
    summary:
      "placeholder copy. a permit is a threshold market's raw material: " +
      "it changes the miles that can be driven, not the miles driven.",
    sources: 5,
    marketKey: "freight",
  },
  {
    key: "replication",
    headline: "a third lab reports no signal in the replication attempt",
    category: "science",
    agentViews: 1642,
    ageMs: 9 * HOUR,
    summary:
      "placeholder copy. long odds getting longer — the reading that " +
      "explains why the thinnest market in the pool sits where it does.",
    sources: 8,
    marketKey: "supercon",
  },
  {
    key: "trial-enrolment",
    headline: "an enrolment update pulls a readout back into the autumn",
    category: "health",
    agentViews: 1533,
    ageMs: 7 * HOUR,
    summary:
      "placeholder copy. schedule news again, on the health desk — the " +
      "same shape of reading the spaceflight desk filed this morning.",
    sources: 6,
    marketKey: "readout",
  },
  {
    key: "cluster-power",
    headline: "a datacentre cluster signs for its own generation",
    category: "compute",
    // Deliberately the most-read story on its desk *and* one with no
    // market behind it: filtering to compute is how the page shows a
    // lead that priced nothing, which is what most reading is.
    agentViews: 2050,
    ageMs: 13 * HOUR,
    summary:
      "placeholder copy. a story that sits between two desks — compute " +
      "by subject, energy by consequence — and is filed under one.",
    sources: 7,
  },
  {
    key: "grid-curtail",
    headline: "curtailment records fall for a third week running",
    category: "energy",
    agentViews: 968,
    ageMs: 16 * HOUR,
    summary:
      "placeholder copy. the tail of the feed: read by fewer agents than " +
      "anything above it, and kept because a feed with no tail is a " +
      "front page rather than a feed.",
    sources: 3,
  },
  {
    key: "orbit-debris",
    headline: "a debris conjunction moves a launch window by a day",
    category: "spaceflight",
    agentViews: 842,
    ageMs: 19 * HOUR,
    summary:
      "placeholder copy. a day is inside the noise of a market that " +
      "settles in 2028, and the agents read it anyway.",
    sources: 4,
  },
  {
    key: "port-throughput",
    headline: "port throughput holds despite the routing change",
    category: "transport",
    agentViews: 727,
    ageMs: 22 * HOUR,
    summary:
      "placeholder copy. the oldest reading the page carries, which is " +
      "what the timestamp column is there to make obvious.",
    sources: 5,
  },
];

/** The stories the board shows: the most-read first, cut to `count`.
 *
 * The section's contract with the future feed — when the hub grows
 * `GET /news?sort=views&limit=5`, the server does exactly this.
 * Non-mutating, because `STORIES` is module state. */
export function topStories(count = 5, stories: SampleStory[] = STORIES): SampleStory[] {
  return [...stories].sort((a, b) => b.agentViews - a.agentViews).slice(0, count);
}

/** When a story was scraped, as an ISO timestamp against the given
 * clock — the shape `formatRelative` reads. */
export function scrapedAtIso(ageMs: number, now: number = Date.now()): string {
  return new Date(now - ageMs).toISOString();
}

/** How the full page may order the feed. Two keys, because a feed has
 * exactly two honest orders: most read and newest. */
export type NewsOrder = "views" | "latest";

/** The feed in the given order. Non-mutating, for the reason `topStories`
 * is. "latest" sorts on `ageMs` ascending — the field is an offset from
 * now, so smaller is newer, and ordering on it never needs a clock. */
export function orderStories(stories: SampleStory[], order: NewsOrder): SampleStory[] {
  const sorted = [...stories];
  if (order === "views") return sorted.sort((a, b) => b.agentViews - a.agentViews);
  return sorted.sort((a, b) => a.ageMs - b.ageMs);
}

/** The feed's totals, for the page's header strip: how much has been
 * read, and how much reading it took. */
export function newsTotals(stories: SampleStory[] = STORIES) {
  return {
    count: stories.length,
    reads: stories.reduce((sum, s) => sum + s.agentViews, 0),
    sources: stories.reduce((sum, s) => sum + s.sources, 0),
  };
}

/** The board's sample newsroom: authored stories, and the one piece of
 * real logic the section carries — picking the most-read of them.
 *
 * In `lib/` for the same reasons `predictionSample` is: pure
 * TypeScript, testable without a DOM, and out of the component file so
 * fast refresh keeps working.
 *
 * **Every story here is authored.** The intended product is agents
 * scraping the open web, with the hub counting which stories the agents
 * actually read — the board then shows the five most-read. None of that
 * exists on the wire yet; what it needs is recorded in
 * `docs/hub-requirements.md` under "A newsroom feed". The headlines are
 * generic and deliberately unattributed — no real outlet's name goes on
 * copy it never wrote.
 */

export interface SampleStory {
  /** Stable key for rendering. Not an id the hub would recognise. */
  key: string;
  headline: string;
  /** The desk it belongs to — the same vocabulary the sample markets
   * use, since the stories are what those markets would trade on. */
  category: string;
  /** How many agents have read it. The board's whole sort key. */
  agentViews: number;
  /** How long ago it was scraped. An offset rather than a timestamp, so
   * the sample never carries a stale date -- rendered against the clock
   * like the market chart's axis. */
  ageMs: number;
}

const MINUTE = 60 * 1000;
const HOUR = 60 * MINUTE;

/** More stories than the board shows, on purpose: "the top five by
 * views" is a selection, and a pool of exactly five would make the sort
 * decoration. Deliberately not stored in view order, so a rendering
 * that skips the sort is visibly wrong (and caught by the tests). */
export const STORIES: SampleStory[] = [
  {
    key: "recon",
    headline: "recon aircraft find a weaker storm core than forecast",
    category: "weather",
    agentViews: 2843,
    ageMs: 26 * MINUTE,
  },
  {
    key: "lander-review",
    headline: "lunar lander design review adds another quarter of tests",
    category: "spaceflight",
    agentViews: 2154,
    ageMs: 3 * HOUR,
  },
  {
    key: "solar-record",
    headline: "a record solar afternoon pushes coal to a seasonal low",
    category: "energy",
    agentViews: 2610,
    ageMs: HOUR,
  },
  {
    key: "fab-outage",
    headline: "a fab outage tightens gpu supply the market had priced in",
    category: "compute",
    agentViews: 1987,
    ageMs: 2 * HOUR,
  },
  {
    key: "fourteenth-storm",
    headline: "the season's fourteenth named storm forms overnight",
    category: "weather",
    agentViews: 1730,
    ageMs: 5 * HOUR,
  },
  {
    key: "interconnect",
    headline: "an interconnect approval clears a backlog of solar farms",
    category: "energy",
    agentViews: 1418,
    ageMs: 8 * HOUR,
  },
  {
    key: "static-fire",
    headline: "a static fire test ends early; schedule impact unclear",
    category: "spaceflight",
    agentViews: 1275,
    ageMs: 11 * HOUR,
  },
];

/** The stories the board shows: the most-read first, cut to `count`.
 *
 * This is the section's contract with the future feed — when the hub
 * grows `GET /news?sort=views&limit=5`, the server does exactly this
 * and the client half of it goes away. Non-mutating, because `STORIES`
 * is module state and a sort in place would reorder it for every other
 * reader. */
export function topStories(count = 5, stories: SampleStory[] = STORIES): SampleStory[] {
  return [...stories].sort((a, b) => b.agentViews - a.agentViews).slice(0, count);
}

/** When a story was scraped, as an ISO timestamp against the given
 * clock — the shape `formatRelative` reads. */
export function scrapedAtIso(ageMs: number, now: number = Date.now()): string {
  return new Date(now - ageMs).toISOString();
}

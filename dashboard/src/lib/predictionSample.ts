/** The board's sample prediction markets: their copy, their authored
 * price histories, and the arithmetic the cards read off them.
 *
 * Here rather than in the component for the reason the rest of `lib/`
 * exists — pure TypeScript with no DOM in it, so it can be tested against
 * the numbers instead of against a chart jsdom cannot lay out.
 *
 * **Everything here is authored.** The protocol has no outcome markets,
 * no odds and no settlement yet; what it would need is recorded in
 * `docs/hub-requirements.md` under "Prediction markets".
 */

export interface Outcome {
  label: string;
  /** The outcome's odds, in percent. The two sum to 100 — these are
   * binary markets, so one price implies the other. */
  pct: number;
}

export interface SampleMarket {
  /** Stable key for the carousel. Not an id the hub would recognise —
   * there is nothing on the hub to recognise it. */
  key: string;
  category: string;
  title: string;
  yes: Outcome;
  no: Outcome;
  /** In whole itx, not base units — authored copy, not a hub figure
   * passing through the usual formatters. */
  volumeItx: number;
  /** How many agents hold a position. The full page's header counts
   * these; the board's card has no room for it and does not read it. */
  traders: number;
  settles: string;
  news: string;
  /** The `yes` odds over the span, one point per step. The `no` series
   * is never stored: in a binary market it is 100 minus this. */
  series: number[];
}

/** How many points a history holds, and how long it claims to span. */
export const STEPS = 84;
export const SPAN_MS = 7 * 24 * 60 * 60 * 1000;

/** mulberry32: a tiny deterministic PRNG, so a sample's history is the
 * same on every load. `Math.random` would redraw the market's past on
 * every visit, which even for a sample is the one thing a price history
 * must not do. */
function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), a | 1);
    t = (t + Math.imul(t ^ (t >>> 7), t | 61)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** A week of odds that ticks discretely, like the reference. A seeded walk
 * rather than a hand-authored array, then eased onto the quoted price so
 * the line ends exactly where the card's pill says the market stands.
 *
 * `volatility` is per market so the samples do not read as one series
 * drawn nine times — a market that has barely moved all week looks
 * different from one that has been argued over. */
export function walk(seed: number, from: number, to: number, volatility: number): number[] {
  const rnd = mulberry32(seed);
  const points: number[] = [];
  let v = from;
  for (let i = 0; i < STEPS; i++) {
    v = Math.min(92, Math.max(8, v + (rnd() - 0.5) * volatility));
    points.push(v);
  }
  const drift = to - points[points.length - 1];
  return points.map((p, i) => p + (drift * i) / (points.length - 1));
}

/** The sample markets, in one pool.
 *
 * They price **world events**, not the ITX board itself, and that is the
 * point of the placeholder: the intended product is agents scraping the
 * open web and pricing what they find there.
 *
 * The events are generic and deliberately unattributed, the news lines
 * say plainly that they are placeholders, and each card carries a "sample
 * market" line — so nothing here can be mistaken for a real quote or for
 * reporting from a real outlet.
 *
 * Nine markets across nine desks, which is what makes the desk filter on
 * the full page a filter rather than a decoration. The board's row shows
 * the first few (see `boardMarkets`) because every card draws a live
 * chart. The desks match the newsroom's on purpose: one vocabulary
 * across the two pages is what lets a reader move between them.
 */
export const SAMPLES: SampleMarket[] = [
  {
    key: "storms",
    category: "weather",
    title: "atlantic season closes under 15 named storms",
    yes: { label: "under 15", pct: 72 },
    no: { label: "15 or more", pct: 28 },
    volumeItx: 84_200,
    traders: 412,
    settles: "settles dec 1",
    news:
      "placeholder copy. this line is where an agent's summary of what " +
      "it scraped will sit — the story behind the price, cited back to " +
      "the sources the agent actually read.",
    series: walk(11, 58, 72, 7),
  },
  {
    key: "lunar",
    category: "spaceflight",
    title: "a crewed lunar landing slips past 2027",
    yes: { label: "slips past 2027", pct: 61 },
    no: { label: "lands by 2027", pct: 39 },
    volumeItx: 45_800,
    traders: 268,
    settles: "settles jan 1, 2028",
    news:
      "placeholder copy. a long-dated market moves on schedule news " +
      "rather than on the event, which is the kind of thing an agent " +
      "watching launch manifests would be first to price.",
    series: walk(29, 44, 61, 4),
  },
  {
    key: "solar",
    category: "energy",
    title: "solar out-generates coal worldwide this year",
    yes: { label: "solar leads", pct: 37 },
    no: { label: "coal holds", pct: 63 },
    volumeItx: 128_400,
    traders: 594,
    settles: "settles feb 15",
    news:
      "placeholder copy. the widest swing on the board's own row, and " +
      "deliberately so — a market where the agents disagree is the one " +
      "worth reading.",
    // The widest walk in the pool, but not so wide that the line reads as
    // static rather than as a market changing its mind.
    series: walk(53, 52, 37, 6.5),
  },
  {
    key: "gpu",
    category: "compute",
    title: "gpu spot prices close the quarter under the spring floor",
    yes: { label: "under the floor", pct: 44 },
    no: { label: "at or above", pct: 56 },
    volumeItx: 96_500,
    traders: 508,
    settles: "settles sep 30",
    news:
      "placeholder copy. a market whose two sides have traded places " +
      "twice this week — the shape a chart takes when the agents are " +
      "reading the same supply notes and disagreeing about them.",
    series: walk(71, 51, 44, 8),
  },
  {
    key: "warmest",
    category: "climate",
    title: "the year closes among the three warmest on record",
    yes: { label: "top three", pct: 84 },
    no: { label: "outside top three", pct: 16 },
    volumeItx: 61_300,
    traders: 221,
    settles: "settles jan 15",
    news:
      "placeholder copy. the most one-sided of the pool, and worth " +
      "keeping for that: a card at 84/16 shows what the odds column and " +
      "the payout column do at the ends of their range.",
    series: walk(97, 77, 84, 3),
  },
  {
    key: "ratecut",
    category: "markets",
    title: "a major central bank cuts rates before july",
    yes: { label: "cuts", pct: 56 },
    no: { label: "holds", pct: 44 },
    volumeItx: 152_700,
    traders: 733,
    settles: "settles jul 1",
    news:
      "placeholder copy. the deepest book in the pool. a market this " +
      "close to even is where a scraping agent's edge would show first, " +
      "since both sides are already priced by everyone else.",
    series: walk(131, 49, 56, 5.5),
  },
  {
    key: "freight",
    category: "transport",
    title: "driverless freight clears a million road miles this quarter",
    yes: { label: "clears 1m", pct: 33 },
    no: { label: "falls short", pct: 67 },
    volumeItx: 38_900,
    traders: 174,
    settles: "settles oct 1",
    news:
      "placeholder copy. a threshold market: it resolves on a number " +
      "somebody publishes, which is the kind an oracle can settle " +
      "without a dispute round.",
    series: walk(157, 41, 33, 4.5),
  },
  {
    key: "supercon",
    category: "science",
    title: "a superconductor claim replicates in a second lab",
    yes: { label: "replicates", pct: 17 },
    no: { label: "fails to", pct: 83 },
    volumeItx: 27_400,
    traders: 149,
    settles: "settles dec 31",
    news:
      "placeholder copy. thin volume against long odds — the tail of " +
      "the pool, and the case the page has to render as readably as it " +
      "renders the busy ones.",
    series: walk(179, 24, 17, 6),
  },
  {
    key: "readout",
    category: "health",
    title: "a phase three readout lands before winter",
    yes: { label: "lands", pct: 68 },
    no: { label: "slips", pct: 32 },
    volumeItx: 71_600,
    traders: 305,
    settles: "settles dec 1",
    news:
      "placeholder copy. a schedule market again, on a different desk — " +
      "the pool carries two so the page shows the desk filter cutting " +
      "across the kind of question rather than along it.",
    series: walk(211, 59, 68, 5),
  },
];

/** What a winning stake returns per unit staked: the reciprocal of the
 * odds. 72% pays 1.39x, 28% pays 3.57x — a card's two columns stay
 * consistent by construction rather than by proofreading. */
export function paysOut(pct: number): string {
  return `${(100 / pct).toFixed(2)}x`;
}

/** The point nearest a fraction of the way across the plot, clamped to
 * the series. Everything a hover draws sits on a data point, so the
 * snap happens once, here. */
export function snapIndex(fraction: number): number {
  if (!Number.isFinite(fraction)) return 0;
  return Math.max(0, Math.min(STEPS - 1, Math.round(fraction * (STEPS - 1))));
}

/** The quote at point `i`: both prices and the moment they stood at.
 * Percentages are rounded because the cards quote whole numbers — 71.6%
 * beside a pill saying 72% reads as two different figures. */
export function quoteAt(series: number[], i: number, now: number = Date.now()) {
  const yesPct = series[i];
  const at = now - ((STEPS - 1 - i) / (STEPS - 1)) * SPAN_MS;
  return {
    yesPct: Math.round(yesPct),
    noPct: Math.round(100 - yesPct),
    /** Exact, for plotting: the rounded pair above is for reading, and
     * rounding the line would make it step in whole percents. */
    yesExact: yesPct,
    at,
    when: momentLabel(at),
  };
}

/** "aug 9 at 2 pm", as the reference reads it. Lowercase, like the rest
 * of this surface. */
export function momentLabel(ms: number): string {
  return new Date(ms)
    .toLocaleString("en-US", { month: "short", day: "numeric", hour: "numeric" })
    .toLowerCase()
    .replace(", ", " at ");
}

/** The dates under the axis: four across the span, ending now. Derived
 * rather than authored, so a sample never carries a stale "aug 12" into
 * september. */
export function axisDates(now: number = Date.now()): { index: number; label: string }[] {
  return [0, 1, 2, 3].map((k) => {
    const index = Math.round((k / 3) * (STEPS - 1));
    const at = now - ((STEPS - 1 - index) / (STEPS - 1)) * SPAN_MS;
    return {
      index,
      label: new Date(at)
        .toLocaleDateString("en-US", { month: "short", day: "numeric" })
        .toLowerCase(),
    };
  });
}

/** How many of the pool the board's row carries. Three, not nine: every
 * card draws its own chart, measured and re-rendered on resize, and the
 * board is already the heaviest page on the site. */
export const BOARD_MARKETS = 3;

/** The markets the board's carousel shows — the head of the pool. The head
 * rather than a random pick or a `featured` flag: the pool is authored,
 * so its order *is* the editorial choice. */
export function boardMarkets(
  count: number = BOARD_MARKETS,
  markets: SampleMarket[] = SAMPLES,
): SampleMarket[] {
  return markets.slice(0, count);
}

/** How far a market's odds have travelled over the span, in points. The
 * distance between where the week opened and where it stands, not the
 * width of the swing between them — a market that wandered and came back
 * has not moved. */
export function movedPct(market: SampleMarket): number {
  return Math.abs(market.series[market.series.length - 1] - market.series[0]);
}

/** How the full page may order the pool. Three keys rather than a column
 * per field: this is a page of cards, not a table, so ordering is a
 * question about the markets themselves rather than about a column. */
export type MarketOrder = "volume" | "close" | "moved";

/** The pool in the given order, most-interesting first. Non-mutating:
 * `SAMPLES` is module state, and a sort in place would reorder it for
 * the board too. */
export function orderMarkets(
  markets: SampleMarket[],
  order: MarketOrder,
): SampleMarket[] {
  const sorted = [...markets];
  if (order === "volume") return sorted.sort((a, b) => b.volumeItx - a.volumeItx);
  // Closest to even first: a market at 51/49 is the one the agents are
  // actually arguing over, and it sorts ahead of one at 84/16.
  if (order === "close") {
    return sorted.sort((a, b) => Math.abs(a.yes.pct - 50) - Math.abs(b.yes.pct - 50));
  }
  return sorted.sort((a, b) => movedPct(b) - movedPct(a));
}

/** The pool's totals, for the page's header strip. In whole itx and
 * whole agents, like the fields they add up. */
export function marketTotals(markets: SampleMarket[] = SAMPLES) {
  return {
    count: markets.length,
    volumeItx: markets.reduce((sum, m) => sum + m.volumeItx, 0),
    traders: markets.reduce((sum, m) => sum + m.traders, 0),
  };
}

/** The id of a market's card on the full page, and the fragment the
 * newsroom links to it by. One function so the page that writes the id
 * and the page that writes the link cannot drift. */
export function marketAnchor(key: string): string {
  return `market-${key}`;
}

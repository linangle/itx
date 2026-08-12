/** The board's sample prediction markets: their copy, their authored
 * price histories, and the arithmetic the cards read off them.
 *
 * Here rather than in the component for the reason the rest of `lib/`
 * exists — it is pure TypeScript with no DOM in it, so it can be tested
 * against the numbers instead of against a chart jsdom cannot lay out
 * (see the `ResizeObserver` note in `test-setup.ts`). It also keeps
 * `PredictionMarket.tsx` a component file that only exports a
 * component, which is what fast refresh wants.
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
 * same on every load. `Math.random` here would redraw the market's past
 * on every visit, which even for a sample is the one thing a price
 * history must not do. */
function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), a | 1);
    t = (t + Math.imul(t ^ (t >>> 7), t | 61)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** A week of odds that ticks discretely, like the reference. A seeded
 * walk rather than a hand-authored array (84 points is too many literals
 * to review), then eased onto the quoted price so the line ends exactly
 * where the card's pill says the market stands.
 *
 * `volatility` is how far a step may move. It is per market so the three
 * samples do not read as one series drawn three times — a market that
 * has barely moved all week looks different from one that has been
 * argued over, and that difference is most of what a price chart says. */
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

/** The three sample markets.
 *
 * They price **world events**, not the ITX board itself, and that is the
 * point of the placeholder: the intended product is agents scraping the
 * open web and pricing what they find there. Markets about the board's
 * own sectors would have shown the wrong idea in the right format.
 *
 * The events are generic and deliberately unattributed, the news lines
 * say plainly that they are placeholders, and each card carries a
 * "sample market" line — so nothing here can be mistaken for a real
 * quote or for reporting from a real outlet.
 */
export const SAMPLES: SampleMarket[] = [
  {
    key: "storms",
    category: "weather",
    title: "atlantic season closes under 15 named storms",
    yes: { label: "under 15", pct: 72 },
    no: { label: "15 or more", pct: 28 },
    volumeItx: 84_200,
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
    settles: "settles feb 15",
    news:
      "placeholder copy. the busiest of the three samples, and " +
      "deliberately the one whose odds have moved most — a market where " +
      "the agents disagree is the one worth reading.",
    // The widest of the three, but not so wide that the line reads as
    // static rather than as a market changing its mind: at 11 the walk
    // crossed the plot several times a day and the shape stopped
    // carrying any information.
    series: walk(53, 52, 37, 6.5),
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
 *
 * Percentages are rounded because the cards quote whole numbers — a
 * readout saying 71.6% beside a pill saying 72% reads as two different
 * figures rather than one at two precisions. */
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
      label: new Date(at).toLocaleDateString("en-US", { month: "short", day: "numeric" }),
    };
  });
}

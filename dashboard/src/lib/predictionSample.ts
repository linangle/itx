/** The board's sample prediction market: its copy, its authored price
 * history, and the arithmetic the card reads off it.
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
  /** The outcome's current odds, in percent. The two sum to 100 — this
   * is a binary market, so one price implies the other. */
  pct: number;
}

/** The sample market.
 *
 * A market on a **world event**, not on the ITX board itself, and that
 * is the whole point of the placeholder: the intended product is agents
 * scraping the open web and pricing what they find there. A market
 * about the board's own sectors would have shown the wrong idea in the
 * right format.
 *
 * The event is generic and deliberately unattributed, and the card
 * wears a "sample" line, so nothing here can be mistaken for a real
 * quote or for reporting from a real outlet.
 */
export const SAMPLE = {
  category: "weather",
  title: "atlantic season closes under 15 named storms",
  yes: { label: "under 15", pct: 72 } as Outcome,
  no: { label: "15 or more", pct: 28 } as Outcome,
  /** In whole itx, not base units — authored copy, not a hub figure
   * passing through the usual formatters. */
  volumeItx: 84_200,
  settles: "settles dec 1",
  news:
    "placeholder copy. this line is where an agent's summary of what it " +
    "scraped will sit — the story behind the price, cited back to the " +
    "sources the agent actually read.",
};

/** What a winning stake returns per unit staked: the reciprocal of the
 * odds. 72% pays 1.39x, 28% pays 3.57x — the two columns of the card
 * stay consistent by construction rather than by proofreading. */
export function paysOut(pct: number): string {
  return `${(100 / pct).toFixed(2)}x`;
}

/** How many points the sample's history holds, and how long it claims
 * to span. */
export const YES_STEPS = 84;
export const SPAN_MS = 7 * 24 * 60 * 60 * 1000;

/** mulberry32: a tiny deterministic PRNG, so the sample's history is the
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

/** A week of odds for the `yes` outcome, ticking discretely like the
 * reference. A seeded walk rather than a hand-authored array (84 points
 * is too many literals to review), then eased onto the quoted 72% so the
 * line ends exactly where the pill says the market stands. The `no`
 * series is not stored at all: in a binary market it is 100 minus this.
 */
export const YES_SERIES: number[] = (() => {
  const rnd = mulberry32(11);
  const walk: number[] = [];
  let v = 58;
  for (let i = 0; i < YES_STEPS; i++) {
    v = Math.min(88, Math.max(12, v + (rnd() - 0.5) * 7));
    walk.push(v);
  }
  const drift = SAMPLE.yes.pct - walk[walk.length - 1];
  return walk.map((p, i) => p + (drift * i) / (walk.length - 1));
})();

/** The point nearest a fraction of the way across the plot, clamped to
 * the series. Everything a hover draws sits on a data point, so the
 * snap happens once, here. */
export function snapIndex(fraction: number): number {
  if (!Number.isFinite(fraction)) return 0;
  return Math.max(0, Math.min(YES_STEPS - 1, Math.round(fraction * (YES_STEPS - 1))));
}

/** The quote at point `i`: both prices and the moment they stood at.
 *
 * Percentages are rounded because the card quotes whole numbers — a
 * readout saying 71.6% beside a pill saying 72% reads as two different
 * figures rather than one at two precisions. */
export function quoteAt(i: number, now: number = Date.now()) {
  const yesPct = YES_SERIES[i];
  const at = now - ((YES_STEPS - 1 - i) / (YES_STEPS - 1)) * SPAN_MS;
  return {
    yesPct: Math.round(yesPct),
    noPct: Math.round(100 - yesPct),
    /** Exact, for plotting: the rounded pair above is for reading. */
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
 * rather than authored, so the sample never carries a stale "aug 12"
 * into september. */
export function axisDates(now: number = Date.now()): { index: number; label: string }[] {
  return [0, 1, 2, 3].map((k) => {
    const index = Math.round((k / 3) * (YES_STEPS - 1));
    const at = now - ((YES_STEPS - 1 - index) / (YES_STEPS - 1)) * SPAN_MS;
    return {
      index,
      label: new Date(at).toLocaleDateString("en-US", { month: "short", day: "numeric" }),
    };
  });
}

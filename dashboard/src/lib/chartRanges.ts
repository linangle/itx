// Which spans a chart may offer, given how long the board has actually
// been running.
//
// Nothing here imports React -- same rule as the rest of `src/lib/`.

export interface ChartRange {
  /** URL-safe and stable: it goes in `?range=`. */
  key: string;
  /** What the tab says. Lowercase, like everything else on the site. */
  label: string;
  /** How far back it reaches, or `null` for "all of it", whose span is
   * only knowable from the board's age at the moment it is asked. */
  windowMs: number | null;
}

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/** The ladder, shortest first.
 *
 * Yahoo's is `1D 5D 1M 6M YTD 1Y 5Y All`, which is the reference for the
 * look but the wrong ladder for this board: a simulation is minutes old
 * when it first renders, and a chart whose narrowest range is a day
 * would open on a single point for the first day of its life. The short
 * end is added; the long end is kept, because a real board will get
 * there.
 *
 * `ytd` is deliberately absent. It is a meaningful range for an
 * instrument that has been trading for years and a strange one for a
 * board that may have started this morning -- on 2 January it is a
 * two-day window that happens to be called "year to date". When there
 * is a board old enough for it to mean something, it earns its tab. */
export const CHART_RANGES: ChartRange[] = [
  { key: "1h", label: "1h", windowMs: HOUR },
  { key: "6h", label: "6h", windowMs: 6 * HOUR },
  { key: "1d", label: "1d", windowMs: DAY },
  { key: "5d", label: "5d", windowMs: 5 * DAY },
  { key: "1m", label: "1m", windowMs: 30 * DAY },
  { key: "6m", label: "6m", windowMs: 182 * DAY },
  { key: "1y", label: "1y", windowMs: 365 * DAY },
  { key: "5y", label: "5y", windowMs: 5 * 365 * DAY },
  { key: "all", label: "all", windowMs: null },
];

/** How much of a range must have elapsed before it is worth offering.
 *
 * Not 100%: a range appears once the board covers a *usable fraction* of
 * it, because the alternative is that a tab pops into existence at the
 * exact moment it stops being empty and shows a line pinned to the right
 * edge. At 60% the first thing a new tab shows is a chart with most of
 * its width used.
 *
 * The consequence to be aware of: a range can be offered whose earlier
 * portion predates the board, so the line starts partway across. That is
 * honest -- the board did not exist there -- and is better than hiding a
 * span the reader can see is nearly available. */
const OFFER_THRESHOLD = 0.6;

/** The ranges worth showing for a board of this age.
 *
 * `all` is always present and always last: whatever the board's age, the
 * whole of it is a meaningful thing to look at, and on a board minutes
 * old it is the *only* meaningful thing.
 *
 * `ageMs` is the age of the thing being charted -- the market's own
 * first trade for a market chart, the board's first task for the board's.
 * Charting a tag that first appeared yesterday against a two-month-old
 * board is a flat run of nothing followed by a day of data.
 */
export function rangesForAge(ageMs: number | null): ChartRange[] {
  const all = CHART_RANGES[CHART_RANGES.length - 1];
  if (ageMs === null || !Number.isFinite(ageMs) || ageMs <= 0) return [all];
  return CHART_RANGES.filter(
    (r) => r.windowMs === null || ageMs >= r.windowMs * OFFER_THRESHOLD,
  );
}

/** The range a chart opens on: the widest that is not `all`, or `all` on
 * a board too young to have one.
 *
 * Widest rather than narrowest because the question a market chart is
 * opened to answer is "what has this done", and the longest view the
 * board can honestly show answers it best. `all` is not the default
 * where a fixed range exists, so that the axis means the same thing from
 * one visit to the next -- `all` silently rescales as the board ages. */
export function defaultRange(ageMs: number | null): ChartRange {
  const offered = rangesForAge(ageMs);
  const fixed = offered.filter((r) => r.windowMs !== null);
  return fixed.length > 0 ? fixed[fixed.length - 1] : offered[offered.length - 1];
}

/** A `?range=` value, or the default for this age. A stale link -- one
 * kept from a board that had grown a `1y` tab it has since... well, it
 * cannot shrink, but a hand-edited or mistyped one -- resolves to
 * something showable rather than to an error. */
export function parseRange(raw: string | null, ageMs: number | null): ChartRange {
  const offered = rangesForAge(ageMs);
  return offered.find((r) => r.key === raw) ?? defaultRange(ageMs);
}

/** How wide a window to actually ask the hub for.
 *
 * `all` has no fixed span, so it becomes the board's age plus a little
 * air -- without the padding the oldest point sits exactly on the left
 * edge, where half its marker is clipped and it reads as though the
 * series continues off-screen. */
export function windowForRange(range: ChartRange, ageMs: number | null): number | undefined {
  if (range.windowMs !== null) return range.windowMs;
  if (ageMs === null || !Number.isFinite(ageMs) || ageMs <= 0) return undefined;
  return Math.ceil(ageMs * 1.02);
}

/** How many points to ask for at this width.
 *
 * One bucket per ~6px of chart, bounded by what the hub will serve
 * (`MAX_SERIES_BUCKETS`, 240) and by what is worth drawing at all. Below
 * about 24 the line stops reading as a line and starts reading as a
 * histogram of a handful of bars. */
export function bucketsForWidth(width: number): number {
  return Math.max(24, Math.min(240, Math.round(width / 6)));
}

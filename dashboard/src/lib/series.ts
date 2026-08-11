// Deriving time series from the board.
//
// ## What we can and cannot honestly chart
//
// The hub exposes exactly one timestamp per task: `created_at`. There is
// no record anywhere of when a task was claimed, verified, or paid -- a
// task carries its current status but not the moment it arrived there.
//
// So this module only produces series keyed on **when work was posted**.
// A chart labelled "payouts over time" would really be plotting the
// creation times of tasks that have since been paid, which is a
// different and misleading quantity, and we don't build one. Charting
// settlement over time needs a `resolved_at` on `Task`, which is a change
// to board state -- see `docs/web-v1-log.md`.
//
// Everything here is pure: tasks in, numbers out, `now` passed
// explicitly rather than read from the clock, so tests are deterministic.
// That mirrors how the Rust side threads `now` through `TaskBoard`.

import type { TaskDto } from "./hub";
import { sectorOf } from "./sectors";

export interface BucketOptions {
  /** How many buckets to divide the window into. Each becomes one point
   * on a sparkline. */
  buckets?: number;
  /** How far back the window reaches, in milliseconds. */
  windowMs?: number;
  /** End of the window. Passed in rather than read from `Date.now()` so
   * results are reproducible under test. */
  now?: number;
}

export const DAY_MS = 24 * 60 * 60 * 1000;
const HOUR_MS = 60 * 60 * 1000;

const DEFAULT_BUCKETS = 24;
const DEFAULT_WINDOW_MS = 7 * DAY_MS;

/** A charting window and the label a panel header shows for it. */
export interface SeriesWindow {
  windowMs: number;
  /** Short header label: `1H`, `24H`, `7D`. */
  label: string;
}

/** Smallest first -- `chooseWindow` takes the first one wide enough. */
const WINDOW_PRESETS: SeriesWindow[] = [
  { windowMs: HOUR_MS, label: "1H" },
  { windowMs: 6 * HOUR_MS, label: "6H" },
  { windowMs: DAY_MS, label: "24H" },
  { windowMs: 7 * DAY_MS, label: "7D" },
  { windowMs: 30 * DAY_MS, label: "30D" },
  { windowMs: 90 * DAY_MS, label: "90D" },
];

const DEFAULT_WINDOW: SeriesWindow = { windowMs: DEFAULT_WINDOW_MS, label: "7D" };

/** Picks a charting window that actually fits the board's age.
 *
 * A fixed 7-day window is wrong at both ends of a board's life. On a
 * board seeded an hour ago every task lands in the final bucket and each
 * sparkline is a flat line with one spike at the right edge -- the chart
 * is technically correct and tells you nothing. On a board a year old,
 * seven days hides almost all of its history.
 *
 * So: measure how far back the oldest task actually goes and take the
 * smallest preset that covers it. This never invents data -- it only
 * stops stretching a short history across a long axis. The label travels
 * with the window so a panel header can say `1H` rather than claiming
 * `7D` for something that spans an hour.
 *
 * Note the floor: a board whose tasks were all created within the same
 * minute still charts as a single spike, because that is genuinely all
 * that happened. No window can fix an instantaneous history.
 */
export function chooseWindow(tasks: TaskDto[], now: number = Date.now()): SeriesWindow {
  let oldest = Number.POSITIVE_INFINITY;
  for (const task of tasks) {
    const at = new Date(task.created_at).getTime();
    if (Number.isFinite(at) && at < oldest) oldest = at;
  }
  if (!Number.isFinite(oldest)) return DEFAULT_WINDOW;

  // Clamp: a task timestamped in the future (clock skew between hub and
  // browser) must not produce a negative span and collapse to the
  // narrowest window.
  const span = Math.max(0, now - oldest);
  // Nothing wide enough means the board is older than every preset, so
  // cap at the widest rather than falling back to the 7D default -- an
  // ancient board should show *more* history than a young one, not less.
  return (
    WINDOW_PRESETS.find((preset) => preset.windowMs >= span) ??
    WINDOW_PRESETS[WINDOW_PRESETS.length - 1]
  );
}

/** Bucketed counts of tasks by creation time, oldest bucket first.
 *
 * Tasks older than the window are dropped rather than piled into the
 * first bucket -- a leading spike made entirely of ancient history would
 * flatten everything recent into an unreadable baseline. */
export function countByCreatedAt(tasks: TaskDto[], options: BucketOptions = {}): number[] {
  return sumByCreatedAt(tasks, () => 1, options);
}

/** As `countByCreatedAt`, but summing an arbitrary per-task quantity --
 * bounty value being the one we actually use. */
export function sumByCreatedAt(
  tasks: TaskDto[],
  valueOf: (task: TaskDto) => number,
  options: BucketOptions = {},
): number[] {
  const buckets = options.buckets ?? DEFAULT_BUCKETS;
  const windowMs = options.windowMs ?? DEFAULT_WINDOW_MS;
  const now = options.now ?? Date.now();
  const start = now - windowMs;
  const bucketMs = windowMs / buckets;

  const series = new Array<number>(buckets).fill(0);
  for (const task of tasks) {
    const at = new Date(task.created_at).getTime();
    if (!Number.isFinite(at) || at < start || at > now) continue;
    // `now` itself lands in the final bucket rather than one past the end.
    const index = Math.min(buckets - 1, Math.floor((at - start) / bucketMs));
    series[index] += valueOf(task);
  }
  return series;
}

/** Running total of a series. Turns "tasks posted per bucket" into
 * "tasks posted so far", which is the shape that actually reads like a
 * price line rather than a noisy bar chart. */
export function cumulative(series: number[]): number[] {
  let total = 0;
  return series.map((value) => (total += value));
}

/** Change in activity between the two halves of the window, as a
 * percentage.
 *
 * This is period-over-period, not first-point-to-last-point: for counts
 * of discrete events, comparing two equal spans is meaningful where
 * comparing two individual buckets is mostly noise.
 *
 * Returns `null` when there's nothing to compare against -- an empty
 * earlier half means any activity at all is an increase from zero, which
 * is not a percentage. `null` renders as an em dash, deliberately
 * distinct from a real 0.00%. */
export function periodChangePct(series: number[]): number | null {
  if (series.length < 2) return null;
  const midpoint = Math.floor(series.length / 2);
  const earlier = series.slice(0, midpoint).reduce((a, b) => a + b, 0);
  const later = series.slice(midpoint).reduce((a, b) => a + b, 0);
  if (earlier === 0) return null;
  return ((later - earlier) / earlier) * 100;
}

/** An agent's cumulative earnings curve, derived from the tasks they were
 * paid for.
 *
 * The honest caveat: each task contributes its bounty at its **creation**
 * time, not its payout time, because payout time isn't recorded. On a
 * board where tasks are claimed and settled quickly the two are close;
 * on one where a task sat open for days before someone took it, the
 * curve steps earlier than the money actually moved. It's a shape
 * indicator, not an accounting record -- which is all a sparkline ever
 * is, but worth saying out loud.
 *
 * Consensus tasks split their bounty between winners, and the board
 * never exposes who those winners were (`TaskKindDto::Consensus` hides
 * assignees deliberately), so only tasks with a visible `claimant`
 * contribute. */
export function agentEarningsSeries(
  tasks: TaskDto[],
  pubkey: string,
  options: BucketOptions = {},
): number[] {
  const theirs = tasks.filter((t) => t.claimant === pubkey && t.status === "Paid");
  return cumulative(sumByCreatedAt(theirs, (t) => t.bounty, options));
}

export interface KindSummary {
  kind: TaskDto["kind"];
  /** Tasks of this kind currently open. */
  open: number;
  /** Total bounty currently on offer across those open tasks. */
  openBounty: number;
  /** All tasks of this kind in the window, whatever their status. */
  posted: number;
  series: number[];
  changePct: number | null;
}

/** One row per task kind, which is what the overview's three panels
 * render. Kinds with no tasks at all are still included -- an empty
 * marketplace should show three empty categories, not vanish. */
export function summarizeByKind(tasks: TaskDto[], options: BucketOptions = {}): KindSummary[] {
  const kinds: TaskDto["kind"][] = ["hash_match", "consensus", "disputable"];
  return kinds.map((kind) => {
    const ofKind = tasks.filter((t) => t.kind === kind);
    const open = ofKind.filter((t) => t.status === "Open");
    const series = countByCreatedAt(ofKind, options);
    return {
      kind,
      open: open.length,
      openBounty: open.reduce((sum, t) => sum + t.bounty, 0),
      posted: ofKind.length,
      series,
      changePct: periodChangePct(series),
    };
  });
}

export interface CapabilitySummary {
  capability: string;
  open: number;
  openBounty: number;
  series: number[];
  changePct: number | null;
}

/** Top capability tags by open task count -- the closest thing this board
 * has to sectors. Untagged tasks are unrestricted rather than belonging
 * to a tag called "none", so they're simply absent here. */
export function summarizeByCapability(
  tasks: TaskDto[],
  limit = 8,
  options: BucketOptions = {},
): CapabilitySummary[] {
  const tags = new Set<string>();
  for (const task of tasks) {
    for (const capability of task.capabilities) tags.add(capability);
  }

  return [...tags]
    .map((capability) => {
      const tagged = tasks.filter((t) => t.capabilities.includes(capability));
      const open = tagged.filter((t) => t.status === "Open");
      const series = countByCreatedAt(tagged, options);
      return {
        capability,
        open: open.length,
        openBounty: open.reduce((sum, t) => sum + t.bounty, 0),
        series,
        changePct: periodChangePct(series),
      };
    })
    .sort((a, b) => b.open - a.open || b.openBounty - a.openBounty)
    .slice(0, limit);
}

export interface MarketSummary {
  /** The capability tag this market trades in. */
  capability: string;
  open: number;
  openBounty: number;
  /** Cumulative bounty posted over the window -- the shape that reads
   * as a price line. Same honesty caveat as every series here: keyed on
   * `created_at`, because that is the only timestamp there is. */
  series: number[];
  changePct: number | null;
}

export interface SectorSummary {
  /** Sector name from `sectors.ts`, or `OTHER_SECTOR`. */
  name: string;
  open: number;
  openBounty: number;
  /** All tasks filed under this sector in the window. */
  posted: number;
  /** The sector's individual markets, biggest open bounty first. */
  markets: MarketSummary[];
  /** Tasks posted per bucket across the whole sector -- the quote
   * strip's sparkline. */
  series: number[];
  changePct: number | null;
}

/** The board grouped as sectors of individual markets: one entry per
 * sector with tagged work on it, each carrying one market per capability
 * tag, biggest first at both levels.
 *
 * Ranked by open bounty rather than task count, same as the market
 * carousel always was: a sector is "big" when there is real money on
 * offer in it, and the order is recomputed from whatever the hub last
 * returned, so sectors genuinely move around as work is posted and
 * settled.
 *
 * A market's change column carries the same guard the agent tickers
 * had: below two active buckets there is no trend to report, only a
 * single payout landing in one half of the window and masquerading as
 * +100% or -100%, so it reports `null` and the UI shows a dash.
 *
 * Untagged tasks are unrestricted rather than belonging to a sector, so
 * they are absent here -- consistent with `summarizeByCapability`. A
 * task tagged into two sectors counts once in each; one tagged twice
 * into the *same* sector counts once, which is why the grouping walks a
 * per-task set of sector names rather than pushing per tag. */
export function summarizeBySector(
  tasks: TaskDto[],
  options: BucketOptions = {},
): SectorSummary[] {
  const bySector = new Map<string, TaskDto[]>();
  const byCapability = new Map<string, TaskDto[]>();
  for (const task of tasks) {
    if (task.capabilities.length === 0) continue;
    const sectors = new Set<string>();
    for (const capability of task.capabilities) {
      const list = byCapability.get(capability);
      if (list) list.push(task);
      else byCapability.set(capability, [task]);
      sectors.add(sectorOf(capability));
    }
    for (const name of sectors) {
      const list = bySector.get(name);
      if (list) list.push(task);
      else bySector.set(name, [task]);
    }
  }

  const markets = new Map<string, MarketSummary[]>();
  for (const [capability, tagged] of byCapability) {
    const open = tagged.filter((t) => t.status === "Open");
    const perBucket = sumByCreatedAt(tagged, (t) => t.bounty, options);
    const active = perBucket.filter((v) => v > 0).length;
    const market: MarketSummary = {
      capability,
      open: open.length,
      openBounty: open.reduce((sum, t) => sum + t.bounty, 0),
      series: cumulative(perBucket),
      changePct: active >= 2 ? periodChangePct(perBucket) : null,
    };
    const name = sectorOf(capability);
    const list = markets.get(name);
    if (list) list.push(market);
    else markets.set(name, [market]);
  }

  return [...bySector.entries()]
    .map(([name, ofSector]) => {
      const open = ofSector.filter((t) => t.status === "Open");
      const series = countByCreatedAt(ofSector, options);
      return {
        name,
        open: open.length,
        openBounty: open.reduce((sum, t) => sum + t.bounty, 0),
        posted: ofSector.length,
        markets: (markets.get(name) ?? []).sort(
          (a, b) => b.openBounty - a.openBounty || b.open - a.open,
        ),
        series,
        changePct: periodChangePct(series),
      };
    })
    .sort((a, b) => b.openBounty - a.openBounty || b.open - a.open);
}

export interface BoardTotals {
  openTasks: number;
  openBounty: number;
  paidTasks: number;
  paidBounty: number;
  postedSeries: number[];
  postedChangePct: number | null;
}

/** Headline figures for the top of the overview. `paidBounty` is lifetime
 * settled value across every task the board still remembers, which is the
 * single most persuasive number a marketplace can show. */
export function boardTotals(tasks: TaskDto[], options: BucketOptions = {}): BoardTotals {
  const open = tasks.filter((t) => t.status === "Open");
  const paid = tasks.filter((t) => t.status === "Paid");
  const postedSeries = countByCreatedAt(tasks, options);
  return {
    openTasks: open.length,
    openBounty: open.reduce((sum, t) => sum + t.bounty, 0),
    paidTasks: paid.length,
    paidBounty: paid.reduce((sum, t) => sum + t.bounty, 0),
    postedSeries,
    postedChangePct: periodChangePct(postedSeries),
  };
}

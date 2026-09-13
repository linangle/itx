/** What the board is actually doing, derived from one `/board/series`
 * response.
 *
 * The market charts answer "how much itx has been posted into this kind
 * of work", which is a question about demand and only about demand --
 * `created_at` was the only timestamp the hub had, so it was the only
 * question anything on this site could ask. It got drawn as a rising
 * line with a percentage beside it, which is the shape of a price, and
 * it is not one: nothing here is quoted, nothing is traded, and the
 * number does not go down.
 *
 * These tiles are the rest of the picture, and they are deliberately
 * about completion rather than motion. Posted work that is never done is
 * not a marketplace; agents who arrive and never earn are not users. So
 * every tile that has a counterpart is placed next to it -- posted
 * against paid, tasks against completions -- because the interesting
 * number is almost always the gap.
 */
import type { MarketSeriesDto } from "./hub";
import { cumulative, periodChangePct } from "./series";

/** How a tile's value should be read. The component formats; this module
 * only says which kind of quantity it produced, so a count is never
 * printed with an `itx` suffix and a rate is never compacted to `1.2K`. */
export type ActivityUnit = "itx" | "count" | "pct";

export interface ActivityTile {
  key: string;
  label: string;
  /** One sentence saying what the number counts, in terms a reader can
   * check against the board. Every tile has one: a figure whose meaning
   * has to be guessed is how "cumulative bounty posted" came to be read
   * as a share price. */
  note: string;
  value: number;
  unit: ActivityUnit;
  /** The line to draw: one point per bucket, oldest first, already in
   * the shape the tile means. A flow -- bounty posted, tasks completed,
   * fees -- is accumulated here into the same rising curve the market
   * chart draws, so its last point is the figure above it. A reading --
   * agents active, the average bounty -- is left as read, since summing
   * the same agent forty-eight times is not a count of agents. Decided
   * here rather than by the panel because "tasks posted" and "active
   * agents" are both counts and only this module knows which is which.
   *
   * `null` where the hub has not served what the curve needs -- see
   * `open bounty` -- rather than a curve invented to fill the space. */
  curve: number[] | null;
  /** First half of the window against the second, or `null` when there
   * is nothing to compare. Never shown for a tile whose movement has no
   * good or bad direction. */
  changePct: number | null;
}

/** Mean of `total / count` per bucket, skipping empty buckets rather
 * than charting them as zero. A bucket with no tasks has no average
 * bounty; drawing it as 0 would put a trough in the line every quiet
 * hour and make the series look like a collapse in price. */
function meanSeries(totals: number[], counts: number[]): number[] {
  return totals.map((total, i) => {
    const n = counts[i] ?? 0;
    return n > 0 ? total / n : 0;
  });
}

/** Whether this hub serves the fields the activity panel is built from.
 *
 * `/board/series` grew the five per-bucket series and their totals after
 * the site was already shipping. A hub that predates them answers 200
 * with a body that simply lacks them, and `MarketSeriesDto` -- a
 * compile-time shape, erased at run time -- promises they are there.
 *
 * The consequence was not a wrong number. `s.bounty_series` came back
 * `undefined`, `Sparkline` read `.length` off it, that threw during
 * render, and React unmounted the whole root: a blank white page, not a
 * broken panel. Reachable by upgrading the site before the hub, which is
 * the ordinary order for anyone who deploys the static files first.
 *
 * Checked rather than defaulted, deliberately. Coercing the missing
 * series to `[]` and the missing totals to `0` would render a confident
 * board of zeros on a hub that is merely older than the page, and a
 * fabricated zero is worse than a blank: it is indistinguishable from a
 * quiet market, which is the same failure §5.1 spent a day removing from
 * the landing page. */
/** Open bounty laid out by posting time, ending at the figure itself.
 *
 * Nothing records when a task was claimed, so what was open at any past
 * instant is unknowable and a history is off the table. What the hub
 * serves instead is the present by posting bucket, which accumulates
 * left to right -- and open work posted *before* the window is in the
 * total but in no bucket, so the curve starts from that backlog rather
 * than from zero. Read it as "of what is open now, how old is it": a
 * curve rising to the right is fresh work waiting, a flat one is a
 * backlog nobody is taking. */
function openBountyCurve(byBucket: number[], total: number): number[] {
  const inWindow = byBucket.reduce((a, b) => a + b, 0);
  const before = Math.max(0, total - inWindow);
  return cumulative(byBucket).map((v) => v + before);
}

/** The completion rate so far in the window, bucket by bucket. A
 * per-bucket ratio of two sets that do not correspond would swing
 * wildly and mean nothing; the running ratio settles, and its last point
 * is the window's own rate -- the figure on the tile. Empty until the
 * first task is posted, since a rate of nothing is not zero. */
function runningRate(settled: number[], posted: number[]): number[] {
  let done = 0;
  let asked = 0;
  return posted.map((p, i) => {
    asked += p;
    done += settled[i] ?? 0;
    return asked > 0 ? (done / asked) * 100 : 0;
  });
}

export function hasActivitySeries(s: MarketSeriesDto): boolean {
  return Array.isArray(s.bounty_series) && Array.isArray(s.settled_series);
}

export function activityTiles(s: MarketSeriesDto): ActivityTile[] {
  const completion = s.posted > 0 ? (s.settled / s.posted) * 100 : null;
  const averageBounty = s.posted > 0 ? s.bounty / s.posted : 0;

  return [
    {
      key: "bounty-posted",
      label: "bounty posted",
      note: "itx attached to tasks posted in this window, whether or not the work has been done.",
      value: s.bounty,
      unit: "itx",
      curve: cumulative(s.bounty_series),
      changePct: periodChangePct(s.bounty_series),
    },
    {
      key: "bounty-paid",
      label: "bounty paid",
      note: "itx that actually reached a worker, counted when the chain confirmed the payout rather than when the task was posted.",
      value: s.paid_bounty,
      unit: "itx",
      curve: cumulative(s.paid_bounty_series),
      changePct: periodChangePct(s.paid_bounty_series),
    },
    {
      key: "open-bounty",
      label: "open bounty",
      note: "itx on tasks nobody has claimed, as of right now, laid out by when each was posted. Not a history: nothing records when a task was claimed, so the curve is today's open bounty by age. It starts from what was posted before this window and is still waiting, and ends at the figure above.",
      value: s.open_bounty,
      unit: "itx",
      // Absent rather than invented on a hub that predates the series:
      // a history rebuilt from posted minus paid would start at zero at
      // the window's left edge whatever the backlog really was.
      curve: Array.isArray(s.open_bounty_series)
        ? openBountyCurve(s.open_bounty_series, s.open_bounty)
        : null,
      changePct: null,
    },
    {
      key: "tasks-posted",
      label: "tasks posted",
      note: "tasks created in this window, of every kind.",
      value: s.posted,
      unit: "count",
      curve: cumulative(s.posted_series),
      changePct: periodChangePct(s.posted_series),
    },
    {
      key: "tasks-completed",
      label: "tasks completed",
      note: "tasks whose last payout confirmed in this window. Some of them were posted before it.",
      value: s.settled,
      unit: "count",
      curve: cumulative(s.settled_series),
      changePct: periodChangePct(s.settled_series),
    },
    {
      key: "completion-rate",
      label: "completion rate",
      note: "completions in this window against tasks posted in it. Not a cohort: the two sets overlap but are not the same tasks, so a busy settlement week can read above 100%. The curve is the rate so far in the window, bucket by bucket, settling on the figure above.",
      value: completion ?? 0,
      unit: "pct",
      curve: runningRate(s.settled_series, s.posted_series),
      changePct: null,
    },
    {
      key: "average-bounty",
      label: "average bounty",
      note: "posted bounty divided by tasks posted. Quiet buckets are skipped rather than charted as zero.",
      value: averageBounty,
      unit: "itx",
      curve: meanSeries(s.bounty_series, s.posted_series),
      changePct: null,
    },
    {
      key: "active-agents",
      label: "active agents",
      note: "distinct agent keys that posted a task or received a confirmed payout in this window. one person or organization may run many agents, so this is not a count of people or of independent operators. counted once per bucket and once over the window, so the bars do not add up to the total.",
      value: s.agents,
      unit: "count",
      curve: s.agents_series,
      changePct: periodChangePct(s.agents_series),
    },
    {
      key: "faucet-grants",
      label: "faucet grants",
      note: "starting grants issued, board-wide and unfiltered by capability. The faucet issues against a key, not against a kind of work.",
      value: s.faucet_grants,
      unit: "count",
      curve: cumulative(s.faucet_series),
      changePct: periodChangePct(s.faucet_series),
    },
    {
      key: "chain-fees",
      label: "chain fees",
      note: "itx the hub spent on chain fees settling tasks, one per settlement transaction. an escrow-funded consensus task pays all its winners in one transaction and so costs one fee, however many winners it had. excludes rebuilt payouts and dispute bonds, so this is what settlement recorded rather than total network spend.",
      value: s.fees,
      unit: "itx",
      curve: cumulative(s.fees_series),
      changePct: null,
    },
  ];
}

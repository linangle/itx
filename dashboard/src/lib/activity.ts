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
import { periodChangePct } from "./series";

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
  /** Per bucket, oldest first, for the sparkline. `null` where a series
   * would be a lie rather than merely absent -- see `open bounty` and
   * `completion rate` below. */
  series: number[] | null;
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
      series: s.bounty_series,
      changePct: periodChangePct(s.bounty_series),
    },
    {
      key: "bounty-paid",
      label: "bounty paid",
      note: "itx that actually reached a worker, counted when the chain confirmed the payout rather than when the task was posted.",
      value: s.paid_bounty,
      unit: "itx",
      series: s.paid_bounty_series,
      changePct: periodChangePct(s.paid_bounty_series),
    },
    {
      key: "open-bounty",
      label: "open bounty",
      note: "itx on tasks nobody has claimed, as of right now. A fact about the present, so it has no history to chart.",
      value: s.open_bounty,
      unit: "itx",
      // Deliberately no series. The hub reports this as a scalar because
      // it is a fact about now; reconstructing a history from posted
      // minus paid would draw a curve that starts at zero at the window's
      // left edge whatever the backlog really was.
      series: null,
      changePct: null,
    },
    {
      key: "tasks-posted",
      label: "tasks posted",
      note: "tasks created in this window, of every kind.",
      value: s.posted,
      unit: "count",
      series: s.posted_series,
      changePct: periodChangePct(s.posted_series),
    },
    {
      key: "tasks-completed",
      label: "tasks completed",
      note: "tasks whose last payout confirmed in this window. Some of them were posted before it.",
      value: s.settled,
      unit: "count",
      series: s.settled_series,
      changePct: periodChangePct(s.settled_series),
    },
    {
      key: "completion-rate",
      label: "completion rate",
      note: "completions in this window against tasks posted in it. Not a cohort: the two sets overlap but are not the same tasks, so a busy settlement week can read above 100%.",
      value: completion ?? 0,
      unit: "pct",
      // No series for the same reason the note gives: a per-bucket ratio
      // of two sets that do not correspond would swing wildly and mean
      // nothing at all.
      series: null,
      changePct: null,
    },
    {
      key: "average-bounty",
      label: "average bounty",
      note: "posted bounty divided by tasks posted. Quiet buckets are skipped rather than charted as zero.",
      value: averageBounty,
      unit: "itx",
      series: meanSeries(s.bounty_series, s.posted_series),
      changePct: null,
    },
    {
      key: "active-agents",
      label: "active agents",
      note: "keys that posted a task or were paid for one. Counted once per bucket and once over the window, so the bars do not add up to the total -- an agent working every day is one agent.",
      value: s.agents,
      unit: "count",
      series: s.agents_series,
      changePct: periodChangePct(s.agents_series),
    },
    {
      key: "faucet-grants",
      label: "faucet grants",
      note: "starting grants issued, board-wide and unfiltered by capability. The faucet issues against a key, not against a kind of work.",
      value: s.faucet_grants,
      unit: "count",
      series: s.faucet_series,
      changePct: periodChangePct(s.faucet_series),
    },
    {
      key: "chain-fees",
      label: "chain fees",
      note: "itx the hub spent settling, one network fee per payout. A consensus task with three winners costs three.",
      value: s.fees,
      unit: "itx",
      series: s.fees_series,
      changePct: null,
    },
  ];
}

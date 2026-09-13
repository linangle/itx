import { useMemo } from "react";
import { useAsync } from "../../hooks/useAsync";
import type { AsyncState } from "../../hooks/useAsync";
import { getMarketSeries } from "../../lib/hub";
import type { MarketSeriesDto } from "../../lib/hub";
import { bucketsForWidth, parseRange, rangesForAge, windowForRange } from "../../lib/chartRanges";
import type { ChartRange } from "../../lib/chartRanges";

const REFRESH_MS = 5000;

/** A history at a window and resolution the reader chooses -- one
 * market's, or the whole board's when `capability` is undefined.
 *
 * Shared by the market chart and the stat chart, which draw different
 * lines from the same endpoint and must offer the same ranges the same
 * way. **The range tabs are derived from the series' own age**, not the
 * board's and not a fixed list -- see `chartRanges`.
 *
 * The age, and so which ranges can be offered, comes from the hub -- but
 * the hub only reports it *in* a series response. So the first request
 * goes out with no window at all and every later one is sized from the
 * `first_task_at` that came back. The cost is one request at a
 * possibly-wrong window on first open. */
export function useRangedSeries(
  capability: string | undefined,
  range: string | null,
  width: number,
): { ranges: ChartRange[]; active: ChartRange; series: AsyncState<MarketSeriesDto> } {
  const probe = useAsync(() => getMarketSeries({ capability, buckets: 24 }), [capability]);
  const ageMs = useMemo(() => {
    const first = probe.data?.first_task_at;
    return first ? Date.now() - new Date(first).getTime() : null;
  }, [probe.data]);

  const ranges = useMemo(() => rangesForAge(ageMs), [ageMs]);
  const active = useMemo(() => parseRange(range, ageMs), [range, ageMs]);

  const buckets = bucketsForWidth(width || 600);
  const windowMs = windowForRange(active, ageMs);
  const series = useAsync(
    () => getMarketSeries({ capability, windowMs, buckets }),
    // `probe.data` is in the deps so the first real fetch happens once
    // the age is known and the default range has settled -- without it
    // the chart would draw at the pre-age default and then jump.
    [capability, windowMs, buckets, probe.data],
    REFRESH_MS,
  );

  return { ranges, active, series };
}

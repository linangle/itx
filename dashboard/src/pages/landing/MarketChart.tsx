import { useMemo } from "react";
import Triangle from "../../components/Triangle";
import TimeSeriesChart from "../../components/TimeSeriesChart";
import { useAsync } from "../../hooks/useAsync";
import { useElementWidth } from "../../hooks/useElementWidth";
import { getMarketSeries } from "../../lib/hub";
import { marketLabel, sectorOf } from "../../lib/sectors";
import { bucketsForWidth, parseRange, rangesForAge, windowForRange } from "../../lib/chartRanges";
import { cumulative, periodChangePct } from "../../lib/series";
import { directionOf, formatCompactItx, formatCount, formatPct } from "../../lib/format";

const REFRESH_MS = 5000;

interface Props {
  /** The full capability tag — the market's identity, not its label. */
  capability: string;
  /** `?range=`, and the setter that writes it back. Held in the URL so a
   * chart someone is looking at is a link they can send. */
  range: string | null;
  onRange: (key: string) => void;
  onClose: () => void;
}

/** One market's history, opened in place of the carousel.
 *
 * Deliberately *not* a route. The board's left nav and its
 * leaderboard/trends rail stay exactly where they are — this replaces
 * the middle column's contents and nothing else, so opening a market is
 * a change of focus rather than a change of page. The state still lives
 * in the URL (`?market=`), so it survives a reload and can be linked;
 * what it does not do is unmount the board around it.
 *
 * **Range tabs are derived from the market's own age**, not the board's
 * and not a fixed list. A tag that first traded yesterday offers `1h`,
 * `6h`, `1d` and `all`; the same page six months into a real run offers
 * `6m` as well, without anyone adding it. See `chartRanges`.
 */
export default function MarketChart({ capability, range, onRange, onClose }: Props) {
  const [box, width] = useElementWidth<HTMLDivElement>();

  /** The market's age, and so which ranges it can offer, comes from the
   * hub — but the hub only reports it *in* a series response. So the
   * first request goes out with no window at all, which the hub answers
   * at its own default for this market, and every later one is sized
   * from the `first_task_at` that came back. The cost is one request at
   * a possibly-wrong window on first open; the alternative is a second
   * endpoint that answers only "how old is this tag". */
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

  const data = series.data;
  const line = useMemo(() => cumulative(data?.bounty_series ?? []), [data]);
  const changePct = useMemo(
    () => (data ? periodChangePct(data.bounty_series) : null),
    [data],
  );

  return (
    <div className="itx-chart-panel" ref={box}>
      <div className="itx-chart-head">
        <button type="button" className="itx-chart-back" onClick={onClose}>
          <Triangle direction="left" />
          market overview
        </button>
      </div>

      <div className="itx-chart-title">
        <div>
          <h3>{marketLabel(capability)}</h3>
          {/* The full tag under the label, since the label drops the
              sector prefix and the tag is what the hub knows this market
              by. */}
          <span className="itx-chart-sub">
            {sectorOf(capability)}
            {marketLabel(capability) !== capability && ` · ${capability}`}
          </span>
        </div>

        <div className="itx-chart-figure">
          <span className="itx-chart-value">
            {data ? `${formatCompactItx(data.bounty)} itx` : "—"}
          </span>
          <span className={`itx-chart-change ${directionOf(changePct)}`}>
            {formatPct(changePct)}
          </span>
          <span className="itx-chart-sub">
            {data ? `${formatCount(data.posted)} posted · ${formatCount(data.open)} open` : " "}
          </span>
        </div>
      </div>

      <div className="itx-chart-ranges" role="group" aria-label="Chart range">
        {ranges.map((r) => (
          <button
            key={r.key}
            type="button"
            className={r.key === active.key ? "is-active" : undefined}
            aria-pressed={r.key === active.key}
            onClick={() => onRange(r.key)}
          >
            {r.label}
          </button>
        ))}
      </div>

      <div className="itx-chart-plot">
        {/* `width` is 0 until the box has been measured, and a chart
            drawn at zero width is a chart drawn wrong -- so the first
            frame renders nothing rather than a collapsed axis. */}
        {series.error && <div className="itx-chart-empty">couldn&apos;t reach the hub.</div>}
        {!series.error && data && width > 0 && (
          <TimeSeriesChart
            values={line}
            volume={data.posted_series}
            startMs={data.start_ms}
            endMs={data.end_ms}
            width={width}
            direction={directionOf(changePct)}
          />
        )}
        {!series.error && !data && <div className="itx-chart-empty">loading history…</div>}
      </div>
    </div>
  );
}

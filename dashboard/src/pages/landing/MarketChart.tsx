import { useMemo } from "react";
import TimeSeriesChart from "../../components/TimeSeriesChart";
import { useElementWidth } from "../../hooks/useElementWidth";
import { marketLabel, sectorOf } from "../../lib/sectors";
import { cumulative, periodChangePct } from "../../lib/series";
import { directionOf, formatCompactItx, formatCount, formatPct } from "../../lib/format";
import { useRangedSeries } from "./useRangedSeries";

interface Props {
  /** The full capability tag — the market's identity, not its label. */
  capability: string;
  /** `?range=`, and the setter that writes it back. Held in the URL so a
   * chart someone is looking at is a link they can send. */
  range: string | null;
  onRange: (key: string) => void;
  onClose: () => void;
  /** An id for the heading, so the dialog it opens in is named by it. */
  titleId?: string;
}

/** One market's history, opened over the board in a `ChartDialog`.
 *
 * Deliberately *not* a route: the board stays exactly where it was
 * underneath. The state still lives in the URL (`?market=`), so it
 * survives a reload and can be linked.
 *
 * **Range tabs are derived from the market's own age**, not the board's
 * and not a fixed list — see `useRangedSeries`, which the stat chart
 * shares.
 */
export default function MarketChart({ capability, range, onRange, onClose, titleId }: Props) {
  const [box, width] = useElementWidth<HTMLDivElement>();
  const { ranges, active, series } = useRangedSeries(capability, range, width);

  const data = series.data;
  const line = useMemo(() => cumulative(data?.bounty_series ?? []), [data]);
  const changePct = useMemo(
    () => (data ? periodChangePct(data.bounty_series) : null),
    [data],
  );

  return (
    <>
      {/* The same label a sector's panel wears, above the panel. */}
      <div className="itx-chart-head">
        <h3 className="itx-board-label itx-chart-label" id={titleId}>
          {marketLabel(capability)}
          {/* The sector, and only the sector: the title above it is the
              market, so `software · software/rust` said both twice. The
              full tag is still the market's identity -- it is in the URL
              and in the task-list link -- it just does not need a third
              printing here. */}
          <span className="itx-board-label-sub">{sectorOf(capability)}</span>
        </h3>
        <button type="button" className="itx-chart-close" onClick={onClose} aria-label="Close the chart">
          ×
        </button>
      </div>

      <div className="itx-board-panel itx-chart-panel" ref={box}>
        {/* Labelled, and the label is not decoration. A bare compact
            figure with a percentage beside it is the shape of a quote,
            and this is not one: the line is bounty *posted* into this
            kind of work over the window, it only ever goes up, and the
            percentage compares the window's second half with its first.
            Saying so costs one line and is the difference between a
            chart someone can read and one they will misread. */}
        <div className="itx-chart-figure">
          <span className="itx-chart-figure-label">bounty posted</span>
          <span className="itx-chart-value">
            {data ? `${formatCompactItx(data.bounty)} itx` : "—"}
          </span>
          <span
            className={`itx-chart-change ${directionOf(changePct)}`}
            title="bounty posted in the second half of this window against the first half"
          >
            {formatPct(changePct)}
          </span>
          <span className="itx-chart-sub">
            {data
              ? `${formatCount(data.posted)} posted · ${formatCount(data.settled)} completed · ${formatCompactItx(data.paid_bounty)} itx paid · ${formatCount(data.open)} open`
              : " "}
          </span>
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
    </>
  );
}

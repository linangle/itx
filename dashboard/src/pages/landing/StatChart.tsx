import { useMemo } from "react";
import TimeSeriesChart from "../../components/TimeSeriesChart";
import { useElementWidth } from "../../hooks/useElementWidth";
import { activityTiles, formatStatTick, formatStatValue, hasActivitySeries } from "../../lib/activity";
import { directionOf, formatPct } from "../../lib/format";
import { useRangedSeries } from "./useRangedSeries";

interface Props {
  /** The stat's key, as `activityTiles` names it -- `bounty-posted`. */
  statKey: string;
  /** `?range=`, and the setter that writes it back. Held in the URL so a
   * chart someone is looking at is a link they can send. */
  range: string | null;
  onRange: (key: string) => void;
  onClose: () => void;
}

/** One of the board's own figures, opened in place of the carousel --
 * the stats rail's entries lead here. The same shell as a market's chart,
 * drawing the tile's own curve rather than a market's bounty, and its
 * definition where the market chart puts its totals: this is where the
 * note under every figure moved to when the figures moved to the rail.
 */
export default function StatChart({ statKey, range, onRange, onClose }: Props) {
  const [box, width] = useElementWidth<HTMLDivElement>();
  const { ranges, active, series } = useRangedSeries(undefined, range, width);
  const data = series.data;
  const stale = data ? !hasActivitySeries(data) : false;
  const tile = useMemo(
    () => (data && hasActivitySeries(data) ? (activityTiles(data).find((t) => t.key === statKey) ?? null) : null),
    [data, statKey],
  );
  const direction = directionOf(tile?.changePct ?? null);

  return (
    <>
      <div className="itx-chart-head">
        {/* The same label a sector's panel wears, on the same line -- it
            carries the line's height, so the panel below starts level
            with the leaderboard's. The key stands in until the hub has
            named the tile. */}
        <h3 className="itx-board-label itx-chart-label">
          {tile?.label ?? statKey.replace(/-/g, " ")}
          {/* The second line held open, not captioned: a market's chart
              names its sector here, and a board-wide figure has no such
              parent to name. */}
          <span className="itx-board-label-sub" aria-hidden="true">
            {"\u00a0"}
          </span>
        </h3>
        <button type="button" className="itx-chart-close" onClick={onClose} aria-label="Close the chart">
          ×
        </button>
      </div>

      <div className="itx-board-panel itx-chart-panel" ref={box}>
        <div className="itx-chart-figure">
          <span className="itx-chart-figure-label">{tile?.label ?? " "}</span>
          <span className="itx-chart-value">{tile ? formatStatValue(tile.unit, tile.value) : "—"}</span>
          {tile && tile.changePct !== null && (
            <span
              className={`itx-chart-change ${direction}`}
              title="the second half of this window against the first"
            >
              {formatPct(tile.changePct)}
            </span>
          )}
          {/* The definition, where a market chart puts its totals. Every
              figure here is one definition away from being misread. */}
          <span className="itx-chart-sub">{tile?.note ?? " "}</span>
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
          {series.error && <div className="itx-chart-empty">couldn&apos;t reach the hub.</div>}
          {!series.error && stale && (
            <div className="itx-chart-empty">this hub is older than this page, and doesn&apos;t serve stats yet.</div>
          )}
          {!series.error && data && !stale && !tile && (
            <div className="itx-chart-empty">no such figure on this board.</div>
          )}
          {!series.error && tile && tile.curve === null && (
            <div className="itx-chart-empty">this hub doesn&apos;t serve a history for this figure.</div>
          )}
          {/* `width` is 0 until the box has been measured, and a chart
              drawn at zero width is a chart drawn wrong. */}
          {!series.error && data && tile && tile.curve && width > 0 && (
            <TimeSeriesChart
              values={tile.curve}
              startMs={data.start_ms}
              endMs={data.end_ms}
              width={width}
              direction={direction}
              valueNoun={tile.label}
              formatValue={(v) => formatStatValue(tile.unit, v)}
              formatTick={(v) => formatStatTick(tile.unit, v)}
            />
          )}
          {!series.error && !data && <div className="itx-chart-empty">loading history…</div>}
        </div>
      </div>
    </>
  );
}

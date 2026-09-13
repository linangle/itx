import { useMemo, useState } from "react";
import TimeSeriesChart from "../../components/TimeSeriesChart";
import { useAsync } from "../../hooks/useAsync";
import { useElementWidth } from "../../hooks/useElementWidth";
import { getMarketSeries } from "../../lib/hub";
import {
  activityTiles,
  hasActivitySeries,
  type ActivityTile,
  type ActivityUnit,
} from "../../lib/activity";
import { cumulative } from "../../lib/series";
import { directionOf, formatCompactItx, formatCount, formatPct } from "../../lib/format";

const REFRESH_MS = 5000;
/** Enough points for the curve to have a shape and few enough that a
 * chart a few hundred pixels wide is not drawing sub-pixel steps. The
 * market chart sizes its buckets to its measured width; these are all
 * one fixed height and a fraction of its width, so a constant is honest
 * here where it would not be there. */
const BUCKETS = 48;
/** The tiles' charts, in pixels. The market chart is 300 with a volume
 * strip beneath; these carry no strip and sit beside their figure, and
 * at this height a row of two is about as tall as one sector panel. Kept
 * in step with `.itx-activity-plot`'s `min-height`, which holds the
 * space for the tiles that have no series to draw. */
const CHART_H = 132;

/** The board's own numbers, each with a chart of its own.
 *
 * Board-wide on purpose: this asks `/board/series` with no capability,
 * so it is the whole marketplace rather than one kind of work. The
 * market chart above it is the per-capability view, and the two are
 * deliberately different questions -- which is why this does not simply
 * become another tab on that chart.
 */
export default function ActivityPanel() {
  const series = useAsync(() => getMarketSeries({ buckets: BUCKETS }), [], REFRESH_MS);
  // Three states, not two. A hub older than this page answers 200 with a
  // body that lacks the series, and reading them unguarded threw during
  // render and took the whole React root down with it -- a blank page,
  // reachable by upgrading the site before the hub.
  const stale = series.data ? !hasActivitySeries(series.data) : false;
  const tiles = useMemo(
    () => (series.data && hasActivitySeries(series.data) ? activityTiles(series.data) : null),
    [series.data],
  );

  return (
    <section aria-label="Activity" className="itx-activity">
      <div className="itx-board-labels">
        <span className="itx-board-label">activity</span>
      </div>
      <div className="itx-board-panel itx-activity-panel" id="itx-board-activity">
        {series.error && <div className="itx-activity-empty">couldn&apos;t reach the hub.</div>}
        {/* Named rather than blank: an operator seeing this has upgraded
            the site ahead of the hub, and the fix is to upgrade the hub.
            §5.2 states that order. */}
        {!series.error && stale && (
          <div className="itx-activity-empty">
            this hub is older than this page, and doesn&apos;t serve activity yet.
          </div>
        )}
        {!series.error && !stale && !tiles && (
          <div className="itx-activity-empty">loading activity…</div>
        )}
        {tiles && series.data && (
          <ul className="itx-activity-grid">
            {tiles.map((tile) => (
              <Tile
                key={tile.key}
                tile={tile}
                startMs={series.data!.start_ms}
                endMs={series.data!.end_ms}
              />
            ))}
          </ul>
        )}
      </div>
    </section>
  );
}

/** A count is never printed with an `itx` suffix and a rate is never
 * compacted, which is the whole reason `ActivityUnit` exists. Takes the
 * unit rather than the tile because the chart calls it too, for the
 * hovered bucket. */
function formatValue(unit: ActivityUnit, value: number): string {
  switch (unit) {
    case "itx":
      return `${formatCompactItx(value)} itx`;
    case "pct":
      // One decimal and no sign: this is a level, not a movement, and
      // `formatPct` would render it as "+64.0%" as though it had risen.
      return `${value.toFixed(1)}%`;
    case "count":
      return formatCount(value);
  }
}

/** The value axis, which has no room for a suffix. Counts stay whole
 * numbers; itx is compacted the way the market chart's axis is. */
function formatTick(unit: ActivityUnit, value: number): string {
  return unit === "count" ? formatCount(value) : unit === "pct" ? `${value.toFixed(0)}%` : formatCompactItx(value);
}

/** One figure, its chart beside it, and its definition on the back.
 *
 * The definition used to be a caption under every tile, which made the
 * panel mostly prose. It is still on the tile -- every figure here is
 * one definition away from being misread -- but behind it, a flip away,
 * where it costs nothing until it is wanted. The label sits above the
 * card rather than on it, so it stays put while the card turns and the
 * back does not have to repeat it.
 */
function Tile({ tile, startMs, endMs }: { tile: ActivityTile; startMs: number; endMs: number }) {
  const direction = directionOf(tile.changePct);
  const [flipped, setFlipped] = useState(false);
  const [plot, width] = useElementWidth<HTMLDivElement>();

  // A flow accumulates into the market chart's rising curve; a level is
  // drawn as read. See `ActivityTile.shape`.
  const values = useMemo(
    () =>
      tile.series === null ? null : tile.shape === "flow" ? cumulative(tile.series) : tile.series,
    [tile.series, tile.shape],
  );

  return (
    <li className={flipped ? "itx-activity-tile is-flipped" : "itx-activity-tile"}>
      <div className="itx-activity-head">
        {/* No `title` on the label, though the note would make an obvious
            tooltip. A `title` becomes the element's accessible name, so
            the label announced itself as its own footnote and the words
            "bounty posted" were never spoken at all. The note is on the
            back instead, reachable by the button beside it. */}
        <span className="itx-activity-label">{tile.label}</span>
        <button
          type="button"
          className="itx-activity-flip"
          aria-pressed={flipped}
          aria-label={flipped ? `Back to the ${tile.label} figure` : `What ${tile.label} counts`}
          onClick={() => setFlipped((f) => !f)}
        >
          {flipped ? "×" : "?"}
        </button>
      </div>

      <div className="itx-activity-card">
        {/* Whichever face is turned away is hidden from assistive tech
            as well as from sight, or a screen reader would read both. */}
        <div className="itx-activity-face itx-activity-front" aria-hidden={flipped || undefined}>
          <div className="itx-activity-figure">
            <span className="itx-activity-value">{formatValue(tile.unit, tile.value)}</span>
            {tile.changePct !== null && (
              <span
                className={`itx-activity-change ${direction}`}
                title="the second half of this window against the first"
              >
                {formatPct(tile.changePct)}
              </span>
            )}
          </div>
          {/* Measured like the market chart's box: `width` is 0 until the
              plot has been laid out, and a chart drawn at zero width is a
              chart drawn wrong. No series is a deliberate answer rather
              than a loading state -- see the tiles' own notes for which
              ones and why -- and the box keeps its height so the grid
              stays a grid. */}
          <div className="itx-activity-plot" ref={plot}>
            {values && width > 0 && (
              <TimeSeriesChart
                values={values}
                startMs={startMs}
                endMs={endMs}
                width={width}
                height={CHART_H}
                direction={direction}
                valueNoun={tile.label}
                formatValue={(v) => formatValue(tile.unit, v)}
                formatTick={(v) => formatTick(tile.unit, v)}
              />
            )}
          </div>
        </div>
        <div className="itx-activity-face itx-activity-back" aria-hidden={!flipped || undefined}>
          <p className="itx-activity-note">{tile.note}</p>
        </div>
      </div>
    </li>
  );
}

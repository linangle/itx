import { useMemo } from "react";
import Sparkline from "../../components/Sparkline";
import { useAsync } from "../../hooks/useAsync";
import { getMarketSeries } from "../../lib/hub";
import { activityTiles, hasActivitySeries, type ActivityTile } from "../../lib/activity";
import { directionOf, formatCompactItx, formatCount, formatPct } from "../../lib/format";

const REFRESH_MS = 5000;
/** Enough points for a sparkline to have a shape and few enough that a
 * 64px-wide one is not drawing sub-pixel steps. The market chart sizes
 * its buckets to its measured width; these are all the same small size,
 * so a constant is honest here where it would not be there. */
const BUCKETS = 48;

/** The board's own numbers, as a grid rather than as a line.
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
    <section aria-label="Activity">
      <div className="itx-board-labels">
        <span className="itx-board-label">
          activity
          <span className="itx-board-label-sub">
            the whole board · what was posted, and what was finished
          </span>
        </span>
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
        {tiles && (
          <ul className="itx-activity-grid">
            {tiles.map((tile) => (
              <Tile key={tile.key} tile={tile} />
            ))}
          </ul>
        )}
      </div>
    </section>
  );
}

/** A count is never printed with an `itx` suffix and a rate is never
 * compacted, which is the whole reason `ActivityUnit` exists. */
function formatValue(tile: ActivityTile): string {
  switch (tile.unit) {
    case "itx":
      return `${formatCompactItx(tile.value)} itx`;
    case "pct":
      // One decimal and no sign: this is a level, not a movement, and
      // `formatPct` would render it as "+64.0%" as though it had risen.
      return `${tile.value.toFixed(1)}%`;
    case "count":
      return formatCount(tile.value);
  }
}

function Tile({ tile }: { tile: ActivityTile }) {
  const direction = directionOf(tile.changePct);
  return (
    <li className="itx-activity-tile">
      {/* No `title` here, though the note would make an obvious tooltip.
          A `title` becomes the element's accessible name, so the label
          announced itself to a screen reader as its own footnote and the
          word "bounty posted" was never spoken at all. The note is
          visible text below anyway, which is the better place for it:
          every figure here is one definition away from being misread,
          and a definition only sighted users can hover is half a fix. */}
      <span className="itx-activity-label">{tile.label}</span>
      <span className="itx-activity-value">{formatValue(tile)}</span>
      <span className="itx-activity-foot">
        {tile.series ? (
          <Sparkline
            values={tile.series}
            direction={direction}
            label={`${tile.label} per bucket`}
          />
        ) : (
          /* No series is a deliberate answer, not a loading state -- see
             the tiles' own notes for which ones and why. */
          <span className="itx-activity-nochart" aria-hidden="true" />
        )}
        {tile.changePct !== null && (
          <span className={`itx-activity-change ${direction}`}>{formatPct(tile.changePct)}</span>
        )}
      </span>
      <p className="itx-activity-note">{tile.note}</p>
    </li>
  );
}

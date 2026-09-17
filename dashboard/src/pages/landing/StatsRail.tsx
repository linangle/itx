import { useMemo } from "react";
import { Link } from "react-router-dom";
import Sparkline from "../../components/Sparkline";
import { useAsync } from "../../hooks/useAsync";
import { getMarketSeries } from "../../lib/hub";
import { activityTiles, formatStatValue, hasActivitySeries } from "../../lib/activity";
import { directionOf, formatPct } from "../../lib/format";

const REFRESH_MS = 5000;
/** Enough points for a sparkline to have a shape and few enough that a
 * 44px-wide one is not drawing sub-pixel steps. The stat chart sizes its
 * buckets to its measured width; these are all the same small size, so a
 * constant is honest here where it would not be there. */
const BUCKETS = 48;

/** The board's own numbers, as the left rail: the same figures the
 * activity panel used to grid across the middle, each with a sparkline
 * the size of the trends rail's, and each a button that opens the full
 * chart.
 *
 * Board-wide on purpose: this asks `/board/series` with no capability,
 * so it is the whole marketplace rather than one kind of work. The
 * market activity section is the per-market view, and the two are
 * deliberately different questions.
 */
export default function StatsRail({
  open,
  onOpen,
}: {
  /** Which stat's chart is open, if any -- marked here so the rail says
   * which figure the chart is of. */
  open: string | null;
  onOpen: (key: string) => void;
}) {
  const series = useAsync(() => getMarketSeries({ buckets: BUCKETS }), [], REFRESH_MS);
  // Three states, not two. A hub older than this page answers 200 with a
  // body that lacks the series, and reading them unguarded threw during
  // render and took the whole React root down with it.
  const stale = series.data ? !hasActivitySeries(series.data) : false;
  const tiles = useMemo(
    () => (series.data && hasActivitySeries(series.data) ? activityTiles(series.data) : null),
    [series.data],
  );

  return (
    <>
      {/* Two lines, like the leaderboard's label across the board, which
          is what keeps the panel below level with the market panels -- the
          second held open by a non-breaking space rather than a caption. */}
      <span className="itx-board-label">
        stats
        <span className="itx-board-label-sub" aria-hidden="true">
          {"\u00a0"}
        </span>
      </span>
      <div className="itx-board-panel itx-board-panel-stats">
        {series.error && <p className="itx-board-note">couldn&apos;t reach the hub.</p>}
        {/* Named rather than blank: an operator seeing this has upgraded
            the site ahead of the hub, and the fix is to upgrade the hub. */}
        {!series.error && stale && (
          <p className="itx-board-note">this hub is older than this page, and doesn&apos;t serve stats yet.</p>
        )}
        {!series.error && !stale && !tiles && <p className="itx-board-note">loading stats…</p>}
        {tiles && (
          /* `itx-rail-*` rather than `itx-stat-*`: the terminal pages have a
             stat strip of that name in their own stylesheet, and both
             stylesheets are loaded here, so the rail's figures came out at
             the strip's 16px bold and on its grid. */
          <ul className="itx-rail-stats">
            {tiles.map((tile) => {
              const direction = directionOf(tile.changePct);
              const active = tile.key === open;
              return (
                <li key={tile.key}>
                  {/* The whole entry is the control, and its name is its
                      contents: the label first, so a screen reader hears
                      "bounty posted" before the figure. The figures on the
                      left, the graph on the right with the entry's whole
                      height -- a line of text is not enough room for a
                      line of data. */}
                  <button
                    type="button"
                    className={active ? "itx-rail-stat is-active" : "itx-rail-stat"}
                    aria-pressed={active}
                    onClick={() => onOpen(tile.key)}
                  >
                    <span className="itx-rail-stat-text">
                      <span className="itx-rail-stat-label">{tile.label}</span>
                      <span className="itx-rail-stat-figures">
                        <span className="itx-rail-stat-value">
                          {formatStatValue(tile.unit, tile.value)}
                        </span>
                        {tile.changePct !== null && (
                          <span className={`itx-rail-stat-change ${direction}`}>
                            {formatPct(tile.changePct)}
                          </span>
                        )}
                      </span>
                    </span>
                    <span className="itx-rail-stat-spark">
                      {tile.curve ? (
                        /* Sized by the stylesheet, not by these: the
                           sparkline stretches its one viewBox to whatever
                           the column gives it, so the width here only sets
                           the aspect it is drawn at. */
                        <Sparkline
                          values={tile.curve}
                          width={76}
                          height={24}
                          direction={direction}
                          label={`${tile.label} over the window`}
                        />
                      ) : (
                        /* No curve is a deliberate answer, not a loading
                           state -- a hub that does not serve what it needs,
                           see `ActivityTile.curve`. */
                        <span aria-hidden="true" />
                      )}
                    </span>
                  </button>
                </li>
              );
            })}
          </ul>
        )}
        {/* The pages that carry on past the board, which the nav this
            rail replaced used to carry. The masthead links the
            leaderboard already; nothing else on the board links the
            task list. */}
        <ul className="itx-board-navlist itx-board-navlist-pages">
          <li>
            <Link to="/tasks">all tasks</Link>
          </li>
          <li>
            <Link to="/leaderboard">full leaderboard</Link>
          </li>
        </ul>
      </div>
    </>
  );
}

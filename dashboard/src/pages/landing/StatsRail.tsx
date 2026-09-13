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
 * chart in the middle column.
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
  /** Which stat's chart is open in the middle, if any -- marked here so
   * the rail says what the middle is showing. */
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
          is what keeps the panel below level with the market panels. */}
      <span className="itx-board-label">
        stats
        <span className="itx-board-label-sub">the whole board</span>
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
          <ul className="itx-stats">
            {tiles.map((tile) => {
              const direction = directionOf(tile.changePct);
              const active = tile.key === open;
              return (
                <li key={tile.key}>
                  {/* The whole entry is the control, and its name is its
                      contents: the label first, so a screen reader hears
                      "bounty posted" before the figure. */}
                  <button
                    type="button"
                    className={active ? "itx-stat is-active" : "itx-stat"}
                    aria-pressed={active}
                    onClick={() => onOpen(tile.key)}
                  >
                    <span className="itx-stat-head">
                      <span className="itx-stat-label">{tile.label}</span>
                      {tile.curve ? (
                        <Sparkline
                          values={tile.curve}
                          width={44}
                          direction={direction}
                          label={`${tile.label} over the window`}
                        />
                      ) : (
                        /* No curve is a deliberate answer, not a loading
                           state -- a hub that does not serve what it needs,
                           see `ActivityTile.curve`. */
                        <span className="itx-stat-nochart" aria-hidden="true" />
                      )}
                    </span>
                    <span className="itx-stat-foot">
                      <span className="itx-stat-value">{formatStatValue(tile.unit, tile.value)}</span>
                      {tile.changePct !== null && (
                        <span className={`itx-stat-change ${direction}`}>{formatPct(tile.changePct)}</span>
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

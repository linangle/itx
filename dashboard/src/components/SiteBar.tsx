import { Link, useLocation } from "react-router-dom";
import NewsTicker from "./NewsTicker";
import { useAsync } from "../hooks/useAsync";
import type { AsyncState } from "../hooks/useAsync";
import { BOARD_ANCHOR, scrollToBoard } from "./siteNav";
import { listLatestTasks, type TaskDto } from "../lib/hub";
import { toggleTheme, useTheme } from "../hooks/useTheme";
import "../styles/sitebar.css";

/** How often the bar re-asks the hub for headlines. Matches the board's
 * poll, so a task that appears on one appears on the other. */
const REFRESH_MS = 5000;

/** The site's masthead: the market tape, then the wordmark. Sticky, so
 * it stays with you down any page.
 *
 * It exists as its own component because it is now on *every* screen,
 * not just the landing hero it started in -- the terminal pages had
 * their own bare "ITX." in the top bar and no tape at all, which made
 * the board and the rest of the site read as two different products.
 *
 * The wordmark is a link home. It is the only thing on a deep page like
 * an agent's profile that reliably goes back to the front. */
export function SiteBar({ tasks }: { tasks: AsyncState<{ items: TaskDto[] }> }) {
  const { pathname } = useLocation();

  /** "Home" means the board, not the top of the document. The hero is
   * the pitch -- a globe and a paragraph -- and someone clicking the
   * masthead from inside the site is looking for the market, so the
   * link lands on the data and the pitch stays where a first-time
   * visitor meets it.
   *
   * Already on the landing page, this scrolls; from anywhere else the
   * hash rides along with the navigation and `LandingPage` acts on it
   * once the board has rendered. */
  function toBoard() {
    if (pathname !== "/") return;
    scrollToBoard({ smooth: true });
  }

  return (
    <div className="itx-sitebar">
      <NewsTicker tasks={tasks} />

      <header className="itx-sitebar-brand">
        <Link to={`/#${BOARD_ANCHOR}`} className="itx-sitebar-home" onClick={toBoard}>
          <span className="itx-sitebar-mark">
            ITX<span className="itx-sitebar-dot">.</span>
          </span>
          <span className="itx-sitebar-tag">internet traffic exchange</span>
        </Link>

        <ThemeToggle />
      </header>
    </div>
  );
}

/** Light/dark, at the right end of the masthead.
 *
 * It lives here rather than in `Shell` because the masthead is the one
 * piece of chrome on every surface -- the landing page has no Shell, and
 * a toggle the front page cannot reach is not a site-wide setting. The
 * terminal's top bar existed only to hold this button, and is gone with
 * it.
 *
 * The glyph is the mode you would switch *to*, matching the label the
 * button used to carry ("Light" while dark). Drawn inline rather than
 * loaded: two shapes at 15px, on the one piece of chrome that stays dark
 * in both themes, so they only ever need one colour. */
function ThemeToggle() {
  const theme = useTheme();
  const toLight = theme === "dark";

  return (
    <button
      type="button"
      className="itx-sitebar-theme"
      onClick={toggleTheme}
      aria-label={`Switch to ${toLight ? "light" : "dark"} mode`}
      title={`Switch to ${toLight ? "light" : "dark"} mode`}
    >
      {toLight ? (
        <svg viewBox="0 0 24 24" width="15" height="15" aria-hidden="true">
          <circle cx="12" cy="12" r="4.6" fill="currentColor" />
          {/* Eight rays, generated so they are exactly evenly spaced --
              by hand this is sixteen coordinates to keep in step. */}
          {Array.from({ length: 8 }, (_, i) => {
            const a = (i * Math.PI) / 4;
            const [cos, sin] = [Math.cos(a), Math.sin(a)];
            return (
              <line
                key={i}
                x1={12 + cos * 7.6}
                y1={12 + sin * 7.6}
                x2={12 + cos * 10}
                y2={12 + sin * 10}
                stroke="currentColor"
                strokeWidth="1.8"
                strokeLinecap="round"
              />
            );
          })}
        </svg>
      ) : (
        <svg viewBox="0 0 24 24" width="15" height="15" aria-hidden="true">
          {/* One path, not a disc with a disc punched out of it: a
              crescent cut by a second arc keeps its horns sharp. */}
          <path
            fill="currentColor"
            d="M20.3 14.7A9 9 0 0 1 9.3 3.7a9 9 0 1 0 11 11Z"
          />
        </svg>
      )}
    </button>
  );
}

/** The same bar, fetching its own headlines.
 *
 * For pages that aren't already holding the task list. It asks for the
 * newest dozen rather than walking the board -- see `listLatestTasks`.
 * The landing page uses `SiteBar` directly and hands over what its board
 * already fetched, so that page still makes exactly one pass. */
export default function LiveSiteBar() {
  const tasks = useAsync(
    () => listLatestTasks().then((items) => ({ items })),
    [],
    REFRESH_MS,
  );
  return <SiteBar tasks={tasks} />;
}

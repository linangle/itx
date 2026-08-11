import { Link, useLocation } from "react-router-dom";
import NewsTicker from "./NewsTicker";
import { useAsync } from "../hooks/useAsync";
import type { AsyncState } from "../hooks/useAsync";
import { listLatestTasks, type TaskDto } from "../lib/hub";
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

  /** The masthead shows a pointer, so it has to do something on every
   * page -- including the front page, where the link home is a no-op
   * and clicking it appeared to do nothing at all. It scrolls back to
   * the top instead, which is what the wordmark on a long page is for.
   *
   * Smooth only when you are already where the link points; on the way
   * to another page the outgoing page animating away is just noise, and
   * an instant reset means the next page starts at its top rather than
   * wherever this one was left. Reduced-motion gets the instant one
   * either way. */
  function toTop() {
    const stay = pathname === "/";
    const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    window.scrollTo({ top: 0, behavior: stay && !reduce ? "smooth" : "auto" });
  }

  return (
    <div className="itx-sitebar">
      <NewsTicker tasks={tasks} />

      <header className="itx-sitebar-brand">
        <Link to="/" className="itx-sitebar-home" onClick={toTop}>
          <span className="itx-sitebar-mark">
            ITX<span className="itx-sitebar-dot">.</span>
          </span>
          <span className="itx-sitebar-tag">internet traffic exchange</span>
        </Link>
      </header>
    </div>
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

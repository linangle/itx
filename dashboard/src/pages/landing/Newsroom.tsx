import { Link } from "react-router-dom";
import { formatCount, formatRelative } from "../../lib/format";
import { scrapedAtIso, topStories } from "../../lib/newsroomSample";

/** The board's newsroom section: the five stories the agents have read
 * most, under the prediction market they would be trading on, with the
 * way to the full page.
 *
 * **The stories are authored examples** — see `lib/newsroomSample.ts`,
 * and the label's own sub-line says so on the page. What is real is the
 * selection: the section shows the top five by agent views, which is
 * the contract the future feed serves (`GET /news?sort=views&limit=5`
 * in `docs/hub-requirements.md`). When the wire exists, the sample pool
 * is swapped for a fetch and nothing about this component's shape
 * changes. */
const SHOWN = 5;

export default function Newsroom() {
  const stories = topStories(SHOWN);

  return (
    <section className="itx-nr" aria-label="Newsroom">
      <div className="itx-board-labels">
        <span className="itx-board-label">
          newsroom
          {/* The honesty line, where every label keeps its sub-line:
              these five are examples until agents are actually reading
              the web through the hub. */}
          <span className="itx-board-label-sub">
            what the agents read most — sample stories, nothing scraped yet
          </span>
        </span>
        <Link
          className="itx-pm-open"
          to="/newsroom"
          aria-label="Open the full newsroom"
          title="full newsroom"
        >
          <svg viewBox="0 0 16 16" width="16" height="16" aria-hidden="true">
            <path
              d="M2 8h11M9 3.5 13.5 8 9 12.5"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.8"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          </svg>
        </Link>
      </div>

      {/* The jump link's target, on the panel like every other section's
          -- see `--anchor-top`. */}
      <div className="itx-board-panel itx-nr-panel" id="itx-board-newsroom">
        <table className="itx-board-table itx-nr-table">
          <tbody>
            {stories.map((story, i) => (
              <tr key={story.key}>
                {/* Rank in the reading, like the leaderboard's: the
                    order is the point of the section, so it is worth a
                    number rather than leaving the sort implicit. */}
                <td className="itx-board-rank">{i + 1}</td>
                {/* Titled because the cell clips: a long headline loses
                    its tail at narrow widths. Not a link yet — there is
                    no story page to go to, and a dead link is worse
                    than plain text. The full newsroom is the arrow
                    above. */}
                <td className="itx-nr-headline" title={story.headline}>
                  {story.headline}
                </td>
                <td className="itx-nr-cat">{story.category}</td>
                <td
                  className="right itx-nr-views"
                  title={`read by ${formatCount(story.agentViews)} agents`}
                >
                  <svg
                    viewBox="0 0 16 16"
                    width="14"
                    height="14"
                    aria-hidden="true"
                    className="itx-nr-eye"
                  >
                    <path
                      d="M1.5 8s2.4-4.2 6.5-4.2S14.5 8 14.5 8s-2.4 4.2-6.5 4.2S1.5 8 1.5 8Z"
                      fill="none"
                      stroke="currentColor"
                      strokeWidth="1.3"
                    />
                    <circle cx="8" cy="8" r="2" fill="currentColor" />
                  </svg>
                  {formatCount(story.agentViews)}
                </td>
                <td className="right itx-nr-when">
                  {formatRelative(scrapedAtIso(story.ageMs))} ago
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </section>
  );
}

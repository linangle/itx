import { formatCount, formatRelative } from "../../lib/format";
import { scrapedAtIso, topStories } from "../../lib/newsroomSample";
import SectionLink from "./SectionLink";

/** The board's newsroom section: the five stories the agents have read
 * most, under the prediction market they would be trading on, with the
 * way to the full page.
 *
 * **The stories are authored examples** — see `lib/newsroomSample.ts`.
 * The section carried a line saying so on the page; the owner asked for
 * it gone twice, so the record of it lives here, in that module, and in
 * `docs/hub-requirements.md` rather than on the board. Worth knowing
 * before anyone screenshots this: a ranked feed with view counts reads
 * as live whether or not it is.
 *
 * What is real is the selection: the section shows the top five by
 * agent views, which is the contract the future feed serves (`GET
 * /news?sort=views&limit=5`). When the wire exists, the sample pool is
 * swapped for a fetch and nothing about this component's shape
 * changes. */
const SHOWN = 5;

export default function Newsroom() {
  const stories = topStories(SHOWN);

  return (
    <section className="itx-nr" aria-label="Newsroom">
      {/* Label and door as one link, exactly as the prediction market
          above it. The sub-line that used to hang under the name is
          gone: it made this the only two-line label on the board, and
          what it said belongs where the market card says the same
          thing -- inside the panel, with the rows it qualifies. */}
      <div className="itx-board-labels">
        <SectionLink to="/newsroom" label="newsroom" describedAs="open the full newsroom" />
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

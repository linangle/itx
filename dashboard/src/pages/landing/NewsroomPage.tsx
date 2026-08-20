import { useMemo, useState } from "react";
import { Link } from "react-router-dom";
import LiveSiteBar from "../../components/SiteBar";
import FilterPills from "./FilterPills";
import SubpageIntro from "./SubpageIntro";
import { useThemedBody } from "../../hooks/useTheme";
import { ALL_DESKS, desksOf, onDesk } from "../../lib/desks";
import { formatCount, formatRelative } from "../../lib/format";
import { SAMPLES, marketAnchor } from "../../lib/predictionSample";
import {
  STORIES,
  newsTotals,
  orderStories,
  scrapedAtIso,
  type NewsOrder,
  type SampleStory,
} from "../../lib/newsroomSample";
import "../../styles/landing.css";

/** The newsroom, reached from the masthead and from the board's own
 * newsroom section.
 *
 * **Every story on it is authored** -- the pool the board takes its top
 * five from, all sixteen of it, in `lib/newsroomSample`. Nothing on the
 * wire files stories yet; the note at the foot says so, and headlines
 * stay generic and unattributed.
 *
 * What the page is proposing is the *unit*: a story here is a
 * **reading**, not a publication. It carries who read it, how much
 * reading went into it, and what it moved. That is why the page leads
 * with one story rather than opening on a table -- a lead is where the
 * summary and the market link have room to be seen.
 */
const ORDERS: { value: NewsOrder; label: string }[] = [
  { value: "views", label: "most read" },
  { value: "latest", label: "newest" },
];

/** The markets a story can be priced into, by key. A story's `marketKey`
 * is resolved against this rather than rendered raw -- a key that no
 * longer names a market should draw no link at all. */
const MARKETS = new Map(SAMPLES.map((market) => [market.key, market]));

export default function NewsroomPage() {
  const theme = useThemedBody("itx-landing-body");
  const [desk, setDesk] = useState<string>(ALL_DESKS);
  const [order, setOrder] = useState<NewsOrder>("views");

  const desks = useMemo(() => desksOf(STORIES), []);
  const shown = useMemo(() => orderStories(onDesk(STORIES, desk), order), [desk, order]);
  const totals = newsTotals(shown);

  /** The lead is whatever the reader's own ordering put first, not a
   * story flagged as the lead. The rest fall into the table below,
   * numbered from two, so the lead keeps its place in the count.
   *
   * Guarded at the render below rather than here: every desk in the
   * filter comes from a story in the pool, so the empty case cannot be
   * reached today -- but a lead read off an empty list is a crashed
   * page. */
  const [lead, ...rest] = shown;

  return (
    <div className="itx-landing" data-theme={theme}>
      <LiveSiteBar />
      <main className="itx-board itx-subpage" aria-label="Newsroom">
        <SubpageIntro
          title="newsroom"
          lede={
            <>
              what the agents are reading, ranked by how many of them read it.{" "}
              <strong>every story below is authored</strong> — the headlines, the counts
              and the summaries stand in for the feed this page is being built for.
            </>
          }
          stats={[
            { value: String(totals.count), label: "stories" },
            { value: formatCount(totals.reads), label: "agent reads" },
            { value: formatCount(totals.sources), label: "sources cited" },
            // Of what is showing, like the three figures beside it: on a
            // filtered page a strip that kept saying 9 would be describing
            // the pool rather than the page.
            { value: String(desksOf(shown).length), label: "desks" },
          ]}
        />

        <div className="itx-filters">
          <FilterPills
            label="desk"
            value={desk}
            onChange={setDesk}
            options={[
              { value: ALL_DESKS, label: "all", count: STORIES.length },
              ...desks.map((d) => ({ value: d.name, label: d.name, count: d.count })),
            ]}
          />
          <FilterPills label="order" value={order} onChange={setOrder} options={ORDERS} />
        </div>

        {lead && <Lead story={lead} />}

        {/* The rest of the feed, in the board's own table -- same rank
            column, same eye, same relative timestamp. A reader who came
            from the board's five rows should recognise this as the
            whole of what those five were the top of. */}
        <div className="itx-board-panel itx-nrpage-panel">
          <table className="itx-board-table itx-nr-table">
            <thead>
              <tr>
                <th className="itx-board-rank">#</th>
                <th>story</th>
                <th>desk</th>
                <th className="right">sources</th>
                <th className="right">read by</th>
                <th className="right">scraped</th>
              </tr>
            </thead>
            <tbody>
              {rest.map((story, i) => (
                <tr key={story.key}>
                  {/* Numbered from two: the lead above is one, and
                      restarting the count here would say the page holds
                      two feeds. */}
                  <td className="itx-board-rank">{i + 2}</td>
                  <td className="itx-nr-headline" title={story.headline}>
                    {story.headline}
                  </td>
                  <td className="itx-nr-cat">{story.category}</td>
                  <td className="right itx-nrpage-sources">{story.sources}</td>
                  <td
                    className="right itx-nr-views"
                    title={`read by ${formatCount(story.agentViews)} agents`}
                  >
                    <EyeIcon />
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

        <p className="itx-sub-note">
          <strong>no agent filed any of this.</strong> a real newsroom needs agents
          publishing what they scrape, the hub counting reads per story, and a feed
          endpoint to serve them in this order — the pool below the board is authored
          until it has all three.
        </p>
      </main>
    </div>
  );
}

/** The story at the top of the feed, given the room a table row cannot
 * give it: the summary the agent filed, what it read to file it, and
 * the market it is priced into. */
function Lead({ story }: { story: SampleStory }) {
  const market = story.marketKey ? MARKETS.get(story.marketKey) : undefined;

  return (
    <article className="itx-board-panel itx-nrpage-lead">
      <div className="itx-nrpage-leadtop">
        <span className="itx-pm-cat">{story.category}</span>
        <span className="itx-nrpage-leadtag">lead</span>
      </div>

      <h2 className="itx-nrpage-headline">{story.headline}</h2>
      <p className="itx-nrpage-summary">{story.summary}</p>

      <div className="itx-nrpage-leadmeta">
        <span className="itx-nr-views">
          <EyeIcon />
          {formatCount(story.agentViews)} agent reads
        </span>
        <span>{story.sources} sources</span>
        <span>{formatRelative(scrapedAtIso(story.ageMs))} ago</span>
      </div>

      {/* Only when the story has a market. A "priced into" line on every
          story would be a promise the pool does not keep -- half of it
          is reading that moved nothing, which is the honest ratio. */}
      {market && (
        <Link className="itx-nrpage-priced" to={`/predictions#${marketAnchor(market.key)}`}>
          priced into <strong>{market.title}</strong> — {market.yes.label}{" "}
          {market.yes.pct}%
        </Link>
      )}
    </article>
  );
}

/** The board newsroom's eye, on the two counts this page shows. Drawn
 * here rather than imported because the board's copy sits inline in a
 * table cell. */
function EyeIcon() {
  return (
    <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true" className="itx-nr-eye">
      <path
        d="M1.5 8s2.4-4.2 6.5-4.2S14.5 8 14.5 8s-2.4 4.2-6.5 4.2S1.5 8 1.5 8Z"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.3"
      />
      <circle cx="8" cy="8" r="2" fill="currentColor" />
    </svg>
  );
}

import { memo, useEffect, useMemo, useRef, useState } from "react";
import { Link } from "react-router-dom";
import Sparkline from "../../components/Sparkline";
import { sweepColors } from "./marketHue";
import { useAsync } from "../../hooks/useAsync";
import type { AsyncState } from "../../hooks/useAsync";
import { useFitRows } from "../../hooks/useFitRows";
import { useCarousel } from "../../hooks/useCarousel";
import { BOARD_ANCHOR } from "../../components/siteNav";
import { getLeaderboard } from "../../lib/hub";
import type { Page, TaskDto } from "../../lib/hub";
import {
  directionOf,
  formatCompactItx,
  formatCount,
  formatPct,
  formatRelative,
  truncatePubkey,
} from "../../lib/format";
import { chooseWindow, summarizeByCapability, summarizeBySector } from "../../lib/series";
import type { SectorSummary } from "../../lib/series";

/** Ceilings, not row counts. Every table on the board now renders as
 * many rows as its panel has room for -- see `useFitRows` -- so these
 * only cap how much a panel is *prepared* to show, which is what stops
 * a tall window from asking for a sparkline per market on the board. */
const MAX_MARKET_ROWS = 24;
const MAX_TRENDING_ROWS = 24;
const MAX_UPDATE_ROWS = 24;
const MAX_LEADER_ROWS = 24;
/** How often the board re-asks the hub. Slow enough to be cheap, quick
 * enough that a settling task shows up while you are still looking. */
const REFRESH_MS = 5000;

/** The supplied search glyph (`assets/search_icon.svg`), inlined.
 *
 * Two changes from the file as exported. It ships as a *white plate
 * with a dark magnifier*, which on these dark panels renders as a light
 * blob -- so the plate is dropped and the glyph takes `currentColor`,
 * letting CSS colour it. And the lens is already a second subpath of
 * the same path, punched in the export by laying a white circle over
 * it; `fill-rule: evenodd` makes it a real hole instead, so the ring is
 * transparent in the middle rather than painted with a background
 * colour that would have to be kept in sync with the panel. */
function SearchIcon() {
  return (
    <svg viewBox="1703 1453 1440 1440" width="13" height="13" aria-hidden="true">
      <path
        fill="currentColor"
        fillRule="evenodd"
        d="M2623.4,2271.99l113.55,157.33c21.1,29.23,18.55,68.17-10.72,90.24s-67.87,14.28-88.75-14.9l-112.75-157.51c-140.53,71.5-311.67,35.57-412.75-82.72-101.65-118.96-108.93-293.97-16.89-420.92,90.78-125.22,256.91-175.28,402.97-116.29,42.46,16.45,81.15,42.65,113.18,74.47,129.02,128.13,134.42,335.62,12.16,470.31ZM2587.59,2043.22c0-119.56-96.92-216.48-216.48-216.48s-216.48,96.92-216.48,216.48,96.92,216.48,216.48,216.48,216.48-96.92,216.48-216.48Z"
      />
    </svg>
  );
}

/** "3m" -> "3m ago"; "just now" stays as is. */
function ago(iso: string): string {
  const rel = formatRelative(iso);
  return rel === "just now" ? rel : `${rel} ago`;
}

/** Which of the tape's rows landed on this poll, so they can be marked
 * as arrivals and animated in.
 *
 * The comparison is against the *previous* set of ids rather than a
 * timestamp: a task's `created_at` says when the hub made it, not when
 * this page first saw it, and a board that has been open for a minute
 * would otherwise flash rows that are merely recent.
 *
 * Done in an effect rather than during render. Working it out inline
 * would mean writing to the ref while rendering, and React calls a
 * render twice in development -- the second pass would compare the new
 * ids against themselves and find nothing new, so the animation would
 * only ever appear in production. The cost is that the class lands a
 * frame after the row does, which is exactly when a CSS animation
 * wants it anyway.
 *
 * The first population is deliberately silent: on a fresh load every
 * row is new, and animating all six at once reads as a glitch rather
 * than as news arriving. */
function useArrivals(latest: TaskDto[]): Set<string> {
  const seen = useRef<Set<string> | null>(null);
  const [arrivals, setArrivals] = useState<Set<string>>(() => new Set());

  useEffect(() => {
    const ids = latest.map((t) => t.id);
    const previous = seen.current;
    seen.current = new Set(ids);
    if (!previous) return;

    const fresh = ids.filter((id) => !previous.has(id));
    // Replaces rather than adds: last poll's arrivals have finished
    // animating and should drop the class, or a row that stays at the
    // top would glow again every time the list is rebuilt.
    if (fresh.length > 0 || arrivals.size > 0) setArrivals(new Set(fresh));
    // `arrivals` is read to decide whether clearing is needed; adding it
    // to the deps would re-run this on its own state change.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [latest]);

  return arrivals;
}

/** What the board trades in.
 *
 * The panels used to be one market per capability tag with that
 * market's *top-earning agents* as its rows -- agents were the tickers.
 * They are not any more: a sector is a kind of work (coding, creative,
 * conversation) and its rows are the individual markets inside it
 * (python, image-generation, therapy). Agents are participants in this
 * market, not the thing being priced, and the leaderboard beside the
 * carousel is where they belong.
 *
 * The derivation itself lives in `lib/series.ts` -- see
 * `summarizeBySector` -- since it is pure task-list arithmetic and the
 * rest of `lib/` is what a future mobile client would share. What is
 * left here is only how it is rendered. */

/** The board below the hero, laid out from the user's mockup: a quote
 * strip in a green-to-grey gradient outline, "market overview" with a
 * clipped carousel of sector panels, a leaderboard/trends rail, a
 * latest feed, and a footer placeholder -- all over a faint grid.
 *
 * Panel *labels sit outside their panels* here, above the outline,
 * which is the main structural difference from the previous version. */
export default function Board({
  tasks,
}: {
  /** Fetched and polled by LandingPage and shared with the tape, so the
   * task list is walked once per refresh rather than once per consumer. */
  tasks: AsyncState<Page<TaskDto> & { complete: boolean }>;
}) {
  const items = useMemo(() => tasks.data?.items ?? [], [tasks.data]);
  // Memoized like the summaries below it, and for the same reason: it
  // parses every task's created_at, and the render body is the wrong
  // place for that to happen on renders whose data hasn't changed.
  const window = useMemo(() => chooseWindow(items), [items]);

  const sectors = useMemo(
    () => summarizeBySector(items, { windowMs: window.windowMs }),
    [items, window.windowMs],
  );
  const trending = useMemo(
    () =>
      summarizeByCapability(items, MAX_TRENDING_ROWS, { windowMs: window.windowMs })
        .slice()
        .sort((a, b) => Math.abs(b.changePct ?? -Infinity) - Math.abs(a.changePct ?? -Infinity)),
    [items, window.windowMs],
  );
  const latest = useMemo(
    () =>
      [...items]
        .sort((a, b) => b.created_at.localeCompare(a.created_at))
        .slice(0, MAX_UPDATE_ROWS),
    [items],
  );

  const arrivals = useArrivals(latest);

  // The row scrolls itself -- see useCarousel and `overflow-x` in the
  // stylesheet. This is only what the browser cannot work out on its
  // own: which sector is at the front, whether either end is reached,
  // and where the arrows should land.
  const [carouselRef, carousel] = useCarousel<HTMLDivElement>(sectors.length);

  // One of these per table: the panel measures itself and says how many
  // rows it has room for, and the table renders that many.
  const [trendFit, trendRows] = useFitRows();
  const [latestFit, latestRows] = useFitRows();

  return (
    // The masthead's link home targets this, not the top of the
    // document -- see SiteBar. The hero is the pitch; this is the site.
    <section className="itx-board" id={BOARD_ANCHOR} aria-label="Market board">
      <div className="itx-board-inner">
        <QuoteStrip sectors={sectors} windowLabel={window.label} />

        {/* Said out loud when the client's page walk stopped before the
         * end of the board (`listAllTasks`'s `maxItems`). Every figure
         * above and below -- sector sizes, open bounty, every sparkline
         * -- is a sum over the task list, so a truncated walk does not
         * show less of the market, it misstates the market. The terminal
         * overview has said so since it was built; this board took the
         * flag as a prop and rendered nothing, which is the one outcome
         * the flag exists to prevent.
         *
         * Worth being precise about which end is missing: the hub sorts
         * oldest-first and the walk slices from the front, so what is
         * absent is the newest work, not the oldest. */}
        {tasks.data && !tasks.data.complete && (
          <p className="itx-board-note itx-board-partial" role="status">
            showing the oldest {formatCount(items.length)} of{" "}
            {formatCount(tasks.data.total)} tasks — figures on this board cover only
            these, and the newest work is missing.
          </p>
        )}

        {/* Laid out on the same three columns as the board below, with the
         * heading in the middle one: the title starts where the first
         * market panel starts. The pager sits immediately after the
         * title rather than out at the carousel's right end -- there it
         * was easy to miss, and nothing tied it to the thing it moves.
         * Keeping it up here rather than on the label line is what stops
         * it colliding with the category names it used to sit among. */}
        <div className="itx-board-head">
          <div className="itx-board-headline">
            <h2 className="itx-board-title">market overview</h2>

            {/* Disabled at the ends rather than wrapping. The row is a
             * scroll now, and a control that jumps the whole way back
             * would contradict what dragging it does. */}
            <div className="itx-board-pager">
              <button
                type="button"
                aria-label="Previous category"
                disabled={carousel.atStart}
                onClick={() => carousel.step(-1)}
              >
                <svg viewBox="0 0 12 14" width="12" height="14" aria-hidden="true">
                  <path d="M11 1 L2 7 L11 13 Z" fill="currentColor" />
                </svg>
              </button>
              <button
                type="button"
                aria-label="Next category"
                disabled={carousel.atEnd}
                onClick={() => carousel.step(1)}
              >
                <svg viewBox="0 0 12 14" width="12" height="14" aria-hidden="true">
                  <path d="M1 1 L10 7 L1 13 Z" fill="currentColor" />
                </svg>
              </button>
            </div>
          </div>
        </div>

        <div className="itx-board-cols">
          <BoardNav
            sectors={sectors}
            current={sectors[carousel.index]?.name}
            onSelect={carousel.to}
          />

          <div className="itx-board-markets" id="itx-board-markets">
            {/* Which end the row is against is handed to CSS as a pair
             * of flags: whether an edge is fading, and how, is the
             * stylesheet's business. */}
            <div
              className="itx-board-carousel"
              ref={carouselRef}
              data-at-start={carousel.atStart || undefined}
              data-at-end={carousel.atEnd || undefined}
            >
              {sectors.map((s) => (
                // Label and panel are one item, so the label cannot drift
                // from the panel it names at any width.
                <div className="itx-board-market" key={s.name}>
                  <span className="itx-board-label">
                    {s.name}
                    {/* The market count is the useful half of this line
                     * now: it says how wide a sector is, which is the
                     * one thing the panel below cannot show once its
                     * rows run past the fold. */}
                    <span className="itx-board-label-sub">
                      {formatCount(s.markets.length)} markets ·{" "}
                      {formatCompactItx(s.openBounty)} itx
                    </span>
                  </span>
                  <SectorPanel
                    sector={s}
                    windowLabel={window.label}
                    loading={tasks.loading}
                    error={tasks.error}
                  />
                </div>
              ))}
            </div>
          </div>

          <div className="itx-board-rail">
            <LeaderboardRail />
            <span className="itx-board-label">trends</span>
            <div className="itx-board-panel itx-board-panel-trends" id="itx-board-trends">
              <div className="itx-board-fit" ref={trendFit}>
                {trending.length === 0 ? (
                  <p className="itx-board-note">no markets trading yet.</p>
                ) : (
                  <table className="itx-board-table">
                    <tbody>
                      {trending.slice(0, trendRows).map((row) => (
                        <tr key={row.capability}>
                          <td>
                            <Link to={`/tasks?capability=${encodeURIComponent(row.capability)}`}>
                              {row.capability}
                            </Link>
                          </td>
                          <td className="itx-board-cell-spark">
                            <Sparkline
                              values={row.series}
                              direction={directionOf(row.changePct)}
                              label={`${row.capability} tasks posted over the last ${window.label}`}
                            />
                          </td>
                          <td className={`right ${directionOf(row.changePct)}`}>
                            {formatPct(row.changePct)}
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                )}
              </div>
            </div>
          </div>
        </div>

        <div className="itx-board-labels itx-board-labels-latest">
          <span className="itx-board-label">latest</span>
          <span className="itx-board-live-dot" aria-label="live" title="live" />
        </div>
        <div className="itx-board-panel itx-board-panel-latest" id="itx-board-latest">
          <div className="itx-board-fit" ref={latestFit}>
            <ul className="itx-board-updates">
              {latest.length === 0 && !tasks.loading && (
                <li className="itx-board-note">nothing on the tape yet.</li>
              )}
              {latest.slice(0, latestRows).map((t) => (
                <li key={t.id} className={arrivals.has(t.id) ? "is-new" : undefined}>
                  <span className="itx-board-dot" aria-hidden="true" />
                  <span className="itx-board-when">{ago(t.created_at)}</span>
                  <Link to={`/tasks/${t.id}`}>{t.description}</Link>
                  <span className="itx-board-amt">{formatCompactItx(t.bounty)} itx</span>
                </li>
              ))}
            </ul>
          </div>
        </div>

        {/* Deliberately empty for now, per the mockup -- the outline is
         * the deliverable at this stage. */}
        <footer className="itx-board-panel itx-board-footer" aria-label="Footer" />
      </div>
    </section>
  );
}

/** How many colour stops the strip's outline samples across its width.
 * The wash's front is soft (it spans most of the width), so the colour
 * changes slowly in space and five stops resolve it without banding. */
const SWEEP_STOPS = 5;

/** The Yahoo-Finance-style quote strip: one cell per sector, in the
 * gradient outline from the mockup -- the indices above the board, with
 * the constituents in the carousel below.
 *
 * It used to show the three *task kinds* (hash-match, consensus,
 * disputable). Those are protocol mechanics -- how a task is verified,
 * not what kind of work it is -- and leaving them in the headline strip
 * of a board that now reads as sectors of work made the top of the page
 * argue with the rest of it. They still have a home on the terminal
 * pages, which is why `summarizeByKind` stays.
 *
 * The outline's colour is driven from the same wash as the hero's market
 * line, sampled across the strip's width, so the two sweep together. It
 * writes CSS custom properties rather than a whole gradient string,
 * which keeps the gradient's geometry in the stylesheet and lets this
 * only supply colours. */
function QuoteStrip({
  sectors,
  windowLabel,
}: {
  sectors: SectorSummary[];
  windowLabel: string;
}) {
  const stripRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const el = stripRef.current;
    if (!el) return;

    const paint = (t: number) => {
      const colors = sweepColors(t, SWEEP_STOPS);
      for (let i = 0; i < colors.length; i++) el.style.setProperty(`--q${i}`, colors[i]);
    };

    // Reading performance.now() is what locks this to the market line:
    // both sample the wash at the same absolute time, so they agree in
    // phase rather than merely sharing a period.
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      paint(2);
      return;
    }

    // Paint once here rather than waiting for the first frame. rAF does
    // not run while the document is hidden, so a tab opened in the
    // background would otherwise sit on the CSS fallback colour until it
    // was focused, then jump.
    paint(performance.now() / 1000);

    let raf = 0;
    const loop = () => {
      paint(performance.now() / 1000);
      raf = requestAnimationFrame(loop);
    };
    raf = requestAnimationFrame(loop);
    return () => cancelAnimationFrame(raf);
  }, []);

  return (
    <div className="itx-board-quotes" ref={stripRef}>
      <div className="itx-board-quotes-inner">
        {sectors.map((s) => (
          <div className="itx-board-quote" key={s.name}>
            <div className="itx-board-quote-name">{s.name}</div>
            <div className="itx-board-quote-row">
              <div>
                <div className="itx-board-quote-value">{formatCompactItx(s.openBounty)}</div>
                <div className={`itx-board-quote-change ${directionOf(s.changePct)}`}>
                  {s.open} open {formatPct(s.changePct)}
                </div>
              </div>
              <Sparkline
                values={s.series}
                direction={directionOf(s.changePct)}
                width={60}
                height={22}
                label={`${s.name} tasks posted over the last ${windowLabel}`}
              />
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

/** The leaderboard label, search box and table, owning its own fetch and
 * its own query state.
 *
 * Its own component rather than more JSX in `Board` because of what
 * typing in the search used to cost: `query` lived in `Board`, so every
 * keystroke re-rendered the whole board -- twelve market panels and some
 * hundred and fifty sparklines re-reconciled to filter one list (Round
 * 36 measured it). With the state down here, a keystroke re-renders
 * exactly this panel. The leaderboard poll moves down with it, so the
 * board also stops re-rendering when only the leaders answer changed. */
function LeaderboardRail() {
  const leaders = useAsync(() => getLeaderboard(), [], REFRESH_MS);
  const [query, setQuery] = useState("");
  const [fitRef, leaderRows] = useFitRows();

  // Matches the name as well as the key, because the rail now shows the
  // name -- a list you can read but not search by the thing it displays
  // is worse than one that never showed the name at all.
  const needle = query.trim().toLowerCase();
  const found = (leaders.data ?? []).filter(
    (l) =>
      l.pubkey.toLowerCase().includes(needle) ||
      (l.name?.toLowerCase().includes(needle) ?? false),
  );

  return (
    <>
      {/* Two lines, like a market's label: the count is worth having
       * on its own, and it is also what keeps this label the same
       * height as the ones beside it -- so the leaderboard panel
       * starts level with the market panels. A non-breaking space
       * holds the second line open until the hub answers. */}
      <span className="itx-board-label">
        leaderboard
        <span className="itx-board-label-sub">
          {leaders.data ? `${formatCount(leaders.data.length)} agents` : "\u00a0"}
        </span>
      </span>
      <div className="itx-board-panel itx-board-panel-leaders" id="itx-board-leaders">
        <div className="itx-board-search">
          <SearchIcon />
          <input
            type="search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="agent search"
            aria-label="Search agents by name or public key"
          />
        </div>
        <div className="itx-board-fit" ref={fitRef}>
          {leaders.data === null ? (
            <p className="itx-board-note">loading agents…</p>
          ) : found.length === 0 ? (
            <p className="itx-board-note">
              {query ? "no agent matches that." : "no agents have earned yet."}
            </p>
          ) : (
            <table className="itx-board-table">
              <tbody>
                {found.slice(0, Math.min(leaderRows, MAX_LEADER_ROWS)).map((agent) => (
                  <tr key={agent.pubkey}>
                    {/* Name *instead of* the key, not above it: these
                        rows are a fixed 34px (`--row-h`, which is
                        what lets the panel compute its own capacity)
                        and the terminal's stacked treatment would
                        not fit. The full key stays reachable on
                        hover and one click away on the agent page. */}
                    <td>
                      <Link to={`/agents/${agent.pubkey}`} title={agent.pubkey}>
                        {agent.name ?? truncatePubkey(agent.pubkey)}
                      </Link>
                    </td>
                    <td className="right">{formatCompactItx(agent.total_earned)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      </div>
    </>
  );
}

/** One sector's individual markets, as its tickers.
 *
 * Memoized because the carousel re-renders `Board` every time the front
 * sector changes -- which during a drag is every panel boundary the row
 * crosses. The panels' own props all survive those renders (`sectors` is
 * memoized on the task list, the rest are primitives), so `memo` lets a
 * wall of sparkline tables sit out a render that only moved the nav
 * highlight. After a poll the sectors really are new objects and every
 * panel re-renders, which is exactly right. */
const SectorPanel = memo(function SectorPanel({
  sector,
  windowLabel,
  loading,
  error,
}: {
  sector: SectorSummary;
  windowLabel: string;
  loading: boolean;
  error: Error | null;
}) {
  const markets = sector.markets;
  // The panel is as tall as the row it sits in, which is set by the rail
  // beside it -- so how many markets belong here is a question only the
  // rendered box can answer. The header row is measured out of the
  // budget rather than assumed, since it is inside the same box.
  const [fitRef, rows] = useFitRows();
  return (
    <section className="itx-board-panel itx-board-panel-market">
      <div className="itx-board-fit" ref={fitRef}>
        {loading ? (
          <p className="itx-board-note">loading the board…</p>
        ) : error ? (
          <p className="itx-board-note">couldn&apos;t reach the hub. {error.message}</p>
        ) : markets.length === 0 ? (
          <p className="itx-board-note">no {sector.name} work on the board yet.</p>
        ) : (
          <table className="itx-board-table">
            <thead data-fit-fixed>
              <tr>
                <th>market</th>
                <th>value</th>
                <th className="right">change</th>
              </tr>
            </thead>
            <tbody>
              {markets.slice(0, Math.min(rows, MAX_MARKET_ROWS)).map((m) => (
                <tr key={m.capability}>
                  {/* Straight to that market's tasks. A market is a
                      capability tag on the wire, and the task list
                      already filters by exactly that. */}
                  <td className="itx-board-cell-market">
                    <Link to={`/tasks?capability=${encodeURIComponent(m.capability)}`}>
                      {m.capability}
                    </Link>
                  </td>
                  <td className="itx-board-cell-spark">
                    <Sparkline
                      values={m.series}
                      direction={directionOf(m.changePct)}
                      label={`bounty posted in ${m.capability} over the last ${windowLabel}`}
                    />
                  </td>
                  <td className={`right ${directionOf(m.changePct)}`}>{formatPct(m.changePct)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </section>
  );
});

/** The board's left rail: jump links to the four sections, then the live
 * list of sectors, then the pages that carry on past the board.
 *
 * The sector entries are the useful part -- with three panels visible at
 * a time, the pager alone means clicking through the carousel to find
 * one. These select it directly, and the one currently at the front of
 * the carousel is marked.
 *
 * The list fills the rail the same way every other panel does, so a tall
 * window shows more sectors rather than more empty rail -- though with
 * six of them it now usually shows the lot, where the old per-tag list
 * ran past the fold. */
function BoardNav({
  sectors,
  current,
  onSelect,
}: {
  sectors: SectorSummary[];
  /** Name of the sector at the front of the carousel, or none while the
   * board is still empty. Passed by name rather than index because that
   * is what the list matches on, and the list re-sorts under it. */
  current: string | undefined;
  onSelect: (index: number) => void;
}) {
  const [fitRef, rows] = useFitRows();

  return (
    <nav className="itx-board-nav" aria-label="Board sections">
      {/* Where the other columns have a label. A list of section names
       * needs no heading -- but it does need the height one takes, or
       * this panel would start above the panels it sits beside. Two
       * lines, matching a market's name-and-size label. */}
      <span className="itx-board-label itx-board-label-spacer" aria-hidden="true">
        {"\u00a0"}
        <span className="itx-board-label-sub">{"\u00a0"}</span>
      </span>
      <div className="itx-board-panel itx-board-panel-nav">
        <ul className="itx-board-navlist">
          <li>
            <a href="#itx-board-markets">market overview</a>
          </li>
          <li>
            <a href="#itx-board-leaders">leaderboard</a>
          </li>
          <li>
            <a href="#itx-board-trends">trends</a>
          </li>
          <li>
            <a href="#itx-board-latest">latest</a>
          </li>
        </ul>

        <span className="itx-board-navhead">sectors</span>
        <div className="itx-board-fit" ref={fitRef}>
          <ul className="itx-board-navlist">
            {sectors.slice(0, rows).map((s, index) => (
              <li key={s.name}>
                <button
                  type="button"
                  className={s.name === current ? "is-active" : undefined}
                  aria-current={s.name === current ? "true" : undefined}
                  onClick={() => onSelect(index)}
                >
                  {s.name}
                </button>
              </li>
            ))}
          </ul>
        </div>

        <ul className="itx-board-navlist itx-board-navlist-pages">
          <li>
            <Link to="/tasks">all tasks</Link>
          </li>
          <li>
            <Link to="/leaderboard">full leaderboard</Link>
          </li>
        </ul>
      </div>
    </nav>
  );
}

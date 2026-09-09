import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import Sparkline from "../../components/Sparkline";
import ProfileIcon from "../../components/ProfileIcon";
import SearchIcon from "../../components/SearchIcon";
import Triangle from "../../components/Triangle";
import SectorBreakdown from "./SectorBreakdown";
import ActivityPanel from "./ActivityPanel";
import MarketChart from "./MarketChart";
import { sweepColors } from "./marketHue";
import { useAsync } from "../../hooks/useAsync";
import type { AsyncState } from "../../hooks/useAsync";
import { useColumnWidth } from "../../hooks/useColumnWidth";
import type { ColumnWidth } from "../../hooks/useColumnWidth";
import { useDebounced } from "../../hooks/useDebounced";
import { useFitRows } from "../../hooks/useFitRows";
import { useCarousel } from "../../hooks/useCarousel";
import { BOARD_ANCHOR } from "../../components/siteNav";
import { marketLabel } from "../../lib/sectors";
import { LEADERBOARD_PAGE_SIZE, getLeaderboard, getNames } from "../../lib/hub";
import type { BoardSummaryDto, LeaderboardEntryDto, Page, TaskDto } from "../../lib/hub";
import {
  directionOf,
  formatCompactItx,
  formatCount,
  formatPct,
  formatRelative,
  lowerFirst,
  truncatePubkey,
} from "../../lib/format";
import {
  DEFAULT_MARKET_SORT,
  capabilitiesFromSummary,
  sectorsFromSummary,
  sortMarkets,
  windowFromSummary,
} from "../../lib/series";
import type {
  MarketSort,
  MarketSortKey,
  SectorSummary,
  SeriesWindow,
} from "../../lib/series";

/** Ceilings for the tables that measure themselves -- see `useFitRows`.
 * `MAX_MARKET_ROWS` is the exception and a real limit: the sector panels
 * are sized by their rows rather than measured, so it caps how long the
 * longest may get before the carousel's box scrolls instead of growing. */
const MAX_MARKET_ROWS = 12;
const MAX_TRENDING_ROWS = 24;
const MAX_UPDATE_ROWS = 20;
/** The hub only serves the top fifty. */
const MAX_LEADER_ROWS = 50;
/** How often the board re-asks the hub. */
const REFRESH_MS = 5000;

/** What the two side columns may be dragged between, and what they are
 * before anyone has dragged them.
 *
 * The starting widths have to stay in step with the stylesheet's own
 * `--col-nav` / `--col-rail`: those are what the board draws with until a
 * stored width is applied, and a mismatch shows as the columns jumping on
 * the first paint after a reload. The floors are what the columns'
 * contents need; below them there is no narrower column to offer, so that
 * is where dragging further shuts it instead. */
const NAV_WIDTH = { initial: 172, min: 132, max: 320 };
const RAIL_WIDTH = { initial: 232, min: 190, max: 420 };

/** "3m" -> "3m ago"; "just now" stays as is. */
function ago(iso: string): string {
  const rel = formatRelative(iso);
  return rel === "just now" ? rel : `${rel} ago`;
}

/** Which of the tape's rows landed on this poll, so they can be marked
 * as arrivals and animated in.
 *
 * Compared against the previous set of ids rather than a timestamp: a
 * task's `created_at` says when the hub made it, not when this page first
 * saw it. Done in an effect rather than during render -- writing to the
 * ref while rendering would mean React's double render in development
 * compares the new ids against themselves and finds nothing new.
 *
 * The first population is deliberately silent: on a fresh load every row
 * is new, and animating all of them at once reads as a glitch. */
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
    // animating and should drop the class.
    if (fresh.length > 0 || arrivals.size > 0) setArrivals(new Set(fresh));
    // `arrivals` is read to decide whether clearing is needed; adding it
    // to the deps would re-run this on its own state change.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [latest]);

  return arrivals;
}

/** The board below the hero: a quote strip in a gradient outline,
 * "market overview" with a clipped carousel of sector panels, a
 * leaderboard/trends rail, a latest feed, and a footer -- all over a
 * faint grid. Panel labels sit outside their panels, above the outline.
 *
 * The derivation of what is shown lives in `lib/series.ts` (see
 * `summarizeBySector`), since it is pure task-list arithmetic; what is
 * left here is only how it is rendered. */

/** Window shown before the hub has answered. Only ever on screen for the
 * first paint, but a panel header has to say something. */
const PENDING_WINDOW: SeriesWindow = { windowMs: 7 * 24 * 60 * 60 * 1000, label: "7D" };

export default function Board({
  summary,
  latest,
}: {
  /** The hub's own aggregates, polled by LandingPage. The hub sums the
   * board once (`/board/summary`) and this finishes the presentation
   * arithmetic in O(buckets). */
  summary: AsyncState<BoardSummaryDto>;
  /** The tape's headlines, fetched separately because they are the one
   * thing on this page that needs actual tasks rather than totals. */
  latest: AsyncState<{ items: TaskDto[] }>;
}) {
  const window = useMemo(
    () => (summary.data ? windowFromSummary(summary.data) : PENDING_WINDOW),
    [summary.data],
  );
  const sectors = useMemo(
    () => (summary.data ? sectorsFromSummary(summary.data) : []),
    [summary.data],
  );
  const trending = useMemo(
    () =>
      (summary.data ? capabilitiesFromSummary(summary.data, MAX_TRENDING_ROWS) : []).sort(
        (a, b) => Math.abs(b.changePct ?? -Infinity) - Math.abs(a.changePct ?? -Infinity),
      ),
    [summary.data],
  );
  /** Newest first. The hub sorts ascending on `created_at` and
   * `listLatestTasks` takes the *tail* of the board, so its answer is the
   * newest tasks but still oldest-first -- rendered as it came back, the
   * tape read bottom-up and arrivals animated on the last row. */
  const updates = useMemo(
    () =>
      [...(latest.data?.items ?? [])]
        .sort((a, b) => b.created_at.localeCompare(a.created_at))
        .slice(0, MAX_UPDATE_ROWS),
    [latest.data],
  );

  const arrivals = useArrivals(updates);

  /** Ends the two pinned columns level with the market panels. CSS
   * cannot read a sibling's height, so the markets column is measured and
   * its height published as a property both columns read.
   *
   * An observer rather than a one-off read: the height changes with the
   * widest sector, and with the breakpoint that changes how many panels
   * are shown. */
  const colsRef = useRef<HTMLDivElement | null>(null);
  const marketsRef = useRef<HTMLDivElement | null>(null);

  /** Both widths are published on `.itx-board-inner`, where the
   * stylesheet declares them, because two grids resolve against the same
   * pair: the columns themselves and the heading line above them. Sizing
   * the columns directly would leave the heading measuring against the
   * old numbers. */
  const innerRef = useRef<HTMLDivElement | null>(null);
  const nav = useColumnWidth({
    host: innerRef,
    property: "--col-nav",
    attribute: "data-nav-shut",
    storageKey: "itx-board-nav-w",
    side: "left",
    label: "the board nav",
    controls: "itx-board-nav",
    ...NAV_WIDTH,
  });
  const rail = useColumnWidth({
    host: innerRef,
    property: "--col-rail",
    attribute: "data-rail-shut",
    storageKey: "itx-board-rail-w",
    side: "right",
    label: "the leaderboard and trends",
    controls: "itx-board-rail",
    ...RAIL_WIDTH,
  });
  /** Which market's chart is open, and at what range -- in the URL, so a
   * chart is a link someone can send and a reload lands back on it. A
   * search param rather than a route: opening a market must not unmount
   * the board around it. */
  const [params, setParams] = useSearchParams();
  const openCapability = params.get("market");

  const openMarket = useCallback(
    (capability: string) => {
      const next = new URLSearchParams(params);
      next.set("market", capability);
      // A range carried over from the last market is meaningless for this
      // one -- markets have different ages, so `5d` may not be on offer.
      next.delete("range");
      setParams(next);
    },
    [params, setParams],
  );

  const closeMarket = useCallback(() => {
    const next = new URLSearchParams(params);
    next.delete("market");
    next.delete("range");
    setParams(next);
  }, [params, setParams]);

  const setRange = useCallback(
    (key: string) => {
      const next = new URLSearchParams(params);
      next.set("range", key);
      setParams(next);
    },
    [params, setParams],
  );

  useEffect(() => {
    const cols = colsRef.current;
    const markets = marketsRef.current;
    if (!cols || !markets || typeof ResizeObserver === "undefined") return;
    const sync = () =>
      cols.style.setProperty(
        "--board-col-h",
        `${Math.round(markets.getBoundingClientRect().height)}px`,
      );
    sync();
    const observer = new ResizeObserver(sync);
    observer.observe(markets);
    return () => observer.disconnect();
    // Re-measured on content changes, not only on resizes: on the first
    // paint the board has no data, so the column is a few pixels tall and
    // that is what the observer recorded. `openCapability` because opening
    // a market replaces the measured box.
  }, [sectors, openCapability]);

  // One ordering for every panel -- see `SectorPanel`'s `sort` prop.
  const [sort, setSort] = useState<MarketSort>(DEFAULT_MARKET_SORT);

  /** Whether the nav is showing the overview's sectors. Opened by
   * clicking the overview's entry, and by moving the carousel. Closed
   * only to begin with: a list that vanished mid-read would be worse. */
  const [sectorsOpen, setSectorsOpen] = useState(false);

  /** Which page of the standings the rail is showing, held here with the
   * fetch it keys so the poll and the pager cannot disagree. */
  const [leaderPage, setLeaderPage] = useState(0);
  /** The *committed* agent search -- what the hub is being asked for, not
   * what is in the box. It keys the fetch, so it lives up here; the box's
   * own text deliberately stays down in the rail, so a keystroke
   * re-renders the rail and not the twelve market panels beside it. */
  const [leaderQuery, setLeaderQuery] = useState("");
  const leaders = useAsync(
    () => getLeaderboard(leaderPage * LEADERBOARD_PAGE_SIZE, LEADERBOARD_PAGE_SIZE, leaderQuery),
    [leaderPage, leaderQuery],
    REFRESH_MS,
  );

  /** Pubkey to hub-assigned name, for the tape's poster column. Asked for
   * by key rather than read off the leaderboard, which only carries agents
   * that have *earned* -- every poster who had not yet been paid rendered
   * as a bare key. Keyed on the posters actually on screen, so it re-asks
   * when the tape turns over rather than on every poll; `null` for a key
   * the hub has never named is a normal answer. */
  const posters = useMemo(
    () => [...new Set(updates.map((t) => t.poster))].sort().join(","),
    [updates],
  );
  const names = useAsync(
    () => getNames(posters ? posters.split(",") : []),
    [posters],
  );

  // The row scrolls itself -- see useCarousel and `overflow-x` in the
  // stylesheet. This is only what the browser cannot work out on its own:
  // which sector is at the front, and whether either end is reached.
  const [carouselRef, carousel] = useCarousel<HTMLDivElement>(sectors.length);

  // Moving the row opens the sector list, so it is already there when the
  // reader looks for it. Keyed on `atStart`, which changes exactly once,
  // so this costs nothing per frame.
  useEffect(() => {
    if (!carousel.atStart) setSectorsOpen(true);
  }, [carousel.atStart]);

  // One of these per table: the panel measures itself and says how many
  // rows it has room for, and the table renders that many.
  const [trendFit, trendRows] = useFitRows();


  return (
    // The masthead's link home targets this, not the top of the
    // document -- see SiteBar. The hero is the pitch; this is the site.
    <section className="itx-board" id={BOARD_ANCHOR} aria-label="Market board">
      <div className="itx-board-inner" ref={innerRef}>
        <QuoteStrip sectors={sectors} windowLabel={window.label} />


        {/* Laid out on the same three columns as the board below, with the
          * heading in the middle one: the title starts where the first
          * market panel starts. The line stays in both modes because it
          * carries the section's top margin -- hiding it when a market
          * opened pulled the whole board up by 71px. Only its contents
          * swap. */}
        <div className="itx-board-head" id="itx-board-overview">
          <div className="itx-board-headline">
            {openCapability ? (
              <button type="button" className="itx-chart-back" onClick={closeMarket}>
                <Triangle direction="left" />
                market overview
              </button>
            ) : (
            <>
            <h2 className="itx-board-title">market overview</h2>

            {/* Disabled at the ends rather than wrapping: the row is a
             * scroll, and a control that jumped the whole way back would
             * contradict what dragging it does. */}
            <div className="itx-board-pager">
              <button
                type="button"
                aria-label="Previous category"
                disabled={carousel.atStart}
                onClick={() => carousel.step(-1)}
              >
                <Triangle direction="left" />
              </button>
              <button
                type="button"
                aria-label="Next category"
                disabled={carousel.atEnd}
                onClick={() => carousel.step(1)}
              >
                <Triangle direction="right" />
              </button>
            </div>
            </>
            )}
          </div>
        </div>

        <div className="itx-board-cols" ref={colsRef}>
          <BoardNav
            sectors={sectors}
            firstVisible={carousel.firstVisible}
            lastVisible={carousel.lastVisible}
            expanded={sectorsOpen}
            setExpanded={setSectorsOpen}
            onSelect={carousel.to}
            column={nav}
          />

          {/* The middle column: the carousel, its position indicator and
            * the tape -- the one column here that actually scrolls. */}
          <div className="itx-board-mid">
          {/* `marketsRef` measures *whichever* of the two is showing, not
              the carousel -- it publishes `--board-col-h`, which is how the
              pinned columns either side know how tall to be. On
              `#itx-board-markets`, which unmounts when a chart opens, the
              measurement went stale. */}
          <div className="itx-board-feature" ref={marketsRef}>
          {openCapability ? (
            <MarketChart
              capability={openCapability}
              range={params.get("range")}
              onRange={setRange}
            />
          ) : (
          <div className="itx-board-markets" id="itx-board-markets">
            {/* Which end the row is against, as a pair of flags: whether
             * an edge fades, and how, is the stylesheet's business. */}
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
                    <span className="itx-board-label-sub">
                      {formatCount(s.markets.length)} markets ·{" "}
                      {formatCompactItx(s.openBounty)} itx
                    </span>
                  </span>
                  <SectorPanel
                    sector={s}
                    windowLabel={window.label}
                    loading={summary.loading}
                    error={summary.error}
                    sort={sort}
                    onSort={setSort}
                    onOpen={openMarket}
                  />
                </div>
              ))}
            </div>

            {/* Where the row sits, as a track under it. Driven from custom
              * properties `useCarousel` writes on each scroll frame -- a
              * free-scrolling row moves on frames that change nothing React
              * renders, so state here would re-render the board behind
              * every one of them. */}
            <div className="itx-board-slider" aria-hidden="true">
              <span />
            </div>
          </div>
          )}
          </div>

          <section aria-label="Latest">
          <div className="itx-board-labels itx-board-labels-latest">
            <span className="itx-board-label">latest</span>
            <span className="itx-board-live-dot" aria-label="live" title="live" />
          </div>
          {/* The anchor sits on the panel, with an offset a label taller
            * than the masthead (`--anchor-top`), so every section parks its
            * panel level with the leaderboard's and its own label still
            * clears the bar. */}
          <div className="itx-board-panel itx-board-panel-latest" id="itx-board-latest">
            <div className="itx-board-fit">
              <ul className="itx-board-updates">
                {updates.length === 0 && !latest.loading && (
                  <li className="itx-board-note">nothing on the tape yet.</li>
                )}
                {updates.map((t) => (
                  <li key={t.id} className={arrivals.has(t.id) ? "is-new" : undefined}>
                    <span className="itx-board-dot" aria-hidden="true" />
                    <span className="itx-board-when">{ago(t.created_at)}</span>
                    <Link className="itx-board-what" to={`/tasks/${t.id}`}>
                      {/* The site is set in lower case, so the leading
                          capital comes off -- acronyms survive, see
                          `lowerFirst`. */}
                      {lowerFirst(t.description)}
                    </Link>
                    <span className="itx-board-amt">{formatCompactItx(t.bounty)} itx</span>
                    <span className="itx-board-cat">
                      {/* Untagged work is unrestricted rather than
                          belonging to a market called "none", so the cell
                          holds the column open rather than collapsing the
                          row's alignment. */}
                      {t.capabilities[0] ? (
                        <Link
                          to={`/tasks?capability=${encodeURIComponent(t.capabilities[0])}`}
                          title={t.capabilities[0]}
                        >
                          {marketLabel(t.capabilities[0])}
                        </Link>
                      ) : (
                        <span className="itx-board-untagged">untagged</span>
                      )}
                    </span>
                    {/* The poster, not the claimant: this is a feed of work
                        as it is *posted*, and the newest tasks are open by
                        definition, so a claimant column would be empty on
                        most rows. */}
                    <TapeAgent pubkey={t.poster} name={names.data?.get(t.poster) ?? null} />
                  </li>
                ))}
              </ul>
            </div>
          </div>
          </section>

          {/* Between the tape and the breakdown on purpose. Latest is
            * what just happened, the breakdown is where the work is, and
            * this is whether any of it is finishing -- the question the
            * board could not ask while `created_at` was the only
            * timestamp the hub had. */}
          <ActivityPanel />

          <SectorBreakdown sectors={sectors} />

          </div>

          {/* The column is an ordinary grid item; what pins is the box
            * inside it -- see `.itx-board-pin`. A sticky element that is
            * also a grid item is the case engines disagree about. */}
          <div className="itx-board-rail" id="itx-board-rail">
            <ColumnGrip column={rail} />
            <div className="itx-board-pin">
            <LeaderboardRail
              leaders={leaders}
              page={leaderPage}
              onPage={setLeaderPage}
              onQuery={(q) => {
                // A new search has no page 4. Resetting here rather than in
                // an effect keeps the two in one state update.
                setLeaderPage(0);
                setLeaderQuery(q);
              }}
            />
            <span className="itx-board-label">trends</span>
            <div className="itx-board-panel itx-board-panel-trends" id="itx-board-trends">
              <div className="itx-board-fit" ref={trendFit}>
                {trending.length === 0 ? (
                  <p className="itx-board-note">no work posted yet.</p>
                ) : (
                  <table className="itx-board-table">
                    <tbody>
                      {trending.slice(0, trendRows).map((row) => (
                        <tr key={row.capability}>
                          {/* Clipped like the market panels'. Untreated, a
                              long hyphenated tag wrapped to a second line in
                              a rail this narrow, which broke the fixed row
                              height and pushed the percentage past the
                              panel's edge. */}
                          <td className="itx-board-cell-market">
                            <Link
                              to={`/tasks?capability=${encodeURIComponent(row.capability)}`}
                              title={row.capability}
                            >
                              {marketLabel(row.capability)}
                            </Link>
                          </td>
                          <td className="itx-board-cell-spark">
                            <Sparkline
                              values={row.series}
                              width={44}
                              direction={directionOf(row.changePct)}
                              label={`${row.capability} tasks posted over the last ${window.label}`}
                            />
                          </td>
                          <td className={`right itx-board-cell-pct ${directionOf(row.changePct)}`}>
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
        </div>


        {/* Deliberately empty for now, per the mockup -- the outline is
         * the deliverable at this stage. */}
        <footer className="itx-board-panel itx-board-footer" aria-label="Footer" />
      </div>
    </section>
  );
}

/** The edge of a side column, as something to take hold of.
 *
 * Absolutely positioned into the grid's gutter rather than laid out as a
 * track of its own, which keeps the three-column template -- and the
 * heading line that has to match it -- exactly as it was. The thumb
 * inside it is a separate element only so it can be `sticky`: the column
 * is as tall as the board, so a marker placed anywhere in it is off
 * screen from almost everywhere on the page.
 *
 * A shut column is nothing but this strip, so the strip is also the way
 * back: it stays visible, says so on hover, and a click reopens it. */
function ColumnGrip({ column }: { column: ColumnWidth }) {
  return (
    <div className="itx-board-grip" data-shut={column.shut || undefined} {...column.grip}>
      <span className="itx-board-grip-thumb" aria-hidden="true" />
    </div>
  );
}

/** The agent at the right of a tape row: their icon and their name.
 *
 * The icon needs no lookup -- `ProfileIcon` composes it from the pubkey
 * itself -- and the name falls back to a truncated key, since a name is
 * a label the hub assigns only to agents it has seen earn. */
function TapeAgent({ pubkey, name }: { pubkey: string; name: string | null }) {
  return (
    <Link
      className="itx-board-agent"
      to={`/agents/${pubkey}`}
      // The name the hub assigned, not the key: the reader is pointing at
      // a row because the name is what they are reading by. The key is
      // still what the tooltip says for an agent the hub has never named.
      title={name ? `posted by ${name}` : `posted by ${truncatePubkey(pubkey)}`}
    >
      <ProfileIcon pubkey={pubkey} size={20} className="itx-avatar" />
      <span>{name ?? truncatePubkey(pubkey, 4, 4)}</span>
    </Link>
  );
}

/** How many colour stops the strip's outline samples across its width.
 * The wash's front is soft, so five resolve it without banding. */
const SWEEP_STOPS = 5;

/** The quote strip: one cell per sector, in a gradient outline -- the
 * indices above the board, with the constituents in the carousel below.
 *
 * The outline's colour is driven from the same wash as the hero's market
 * line, sampled across the strip's width, so the two sweep together. It
 * writes CSS custom properties rather than a whole gradient string, which
 * keeps the gradient's geometry in the stylesheet. */
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

    // Paint once rather than waiting for the first frame: rAF does not run
    // while the document is hidden, so a background tab would sit on the
    // CSS fallback colour until focused, then jump.
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
 * Its own component because of what typing in the search used to cost:
 * with `query` in `Board`, every keystroke re-rendered twelve market
 * panels and some hundred and fifty sparklines to filter one list. */
function LeaderboardRail({
  leaders,
  page,
  onPage,
  onQuery,
}: {
  /** One page of the standings, fetched by `Board` and handed down. The
   * *query* is the half that has to stay down here -- see above. */
  leaders: AsyncState<Page<LeaderboardEntryDto>>;
  /** Zero-based, and owned by `Board` because it keys the fetch. */
  page: number;
  onPage: (page: number) => void;
  /** The committed search, handed up once the typing settles. `Board`
   * owns it for the same reason it owns the page: it keys the fetch. */
  onQuery: (query: string) => void;
}) {
  const [query, setQuery] = useState("");
  // The box updates on every keystroke, the hub hears about it once the
  // typing stops. Through an effect rather than the change handler because
  // the *debounced* value settles on a timer, not on an event.
  const settled = useDebounced(query);
  useEffect(() => {
    onQuery(settled);
    // `onQuery` is an inline closure in `Board` and so a new function
    // every render; depending on it would fire this on every poll.
  }, [settled]); // eslint-disable-line react-hooks/exhaustive-deps

  const total = leaders.data?.total ?? 0;
  const pages = Math.ceil(total / LEADERBOARD_PAGE_SIZE);
  // No client-side filter left: the hub searches the whole field, and each
  // row carries the rank it holds among all agents.
  const found = leaders.data?.items ?? [];

  return (
    <>
      {/* Two lines, like a market's label -- which is also what keeps this
       * label the same height as the ones beside it, so the leaderboard
       * panel starts level with the market panels. A non-breaking space
       * holds the second line open until the hub answers. */}
      <span className="itx-board-label">
        leaderboard
        <span className="itx-board-label-sub">
          {/* Under a search the total counts matches, not the field, so
              the label has to say which it is reporting. */}
          {leaders.data
            ? `${formatCount(leaders.data.total)} ${settled ? "found" : "agents"}`
            : "\u00a0"}
        </span>
      </span>
      <div className="itx-board-panel itx-board-panel-leaders" id="itx-board-leaders">
        <div className="itx-board-search">
          <SearchIcon />
          <input
            type="search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="search agents"
            aria-label="Search agents by name or public key"
          />
        </div>
        {/* Scrolls rather than fitting: a leaderboard that stops at the
            sixth agent is answering a different question, and the rail's
            height is set by the carousel beside it. */}
        <div className="itx-board-scroll">
          {/* The error arm comes first, and it is the reason this is not
              a two-way branch any more. `leaders.data` is null both while
              the request is in flight and after it failed, so an
              unreachable hub left this rail saying "loading agents…"
              indefinitely -- the one panel on the board that claimed to
              be making progress. Every other panel here branches on
              `error`; this one had been missed. */}
          {leaders.error ? (
            <p className="itx-board-note">couldn&apos;t reach the hub.</p>
          ) : leaders.data === null ? (
            <p className="itx-board-note">loading agents…</p>
          ) : found.length === 0 ? (
            <p className="itx-board-note">
              {query ? "no agent matches that." : "no agents have earned yet."}
            </p>
          ) : (
            <table className="itx-board-table">
              <tbody>
                {found.slice(0, MAX_LEADER_ROWS).map((agent) => (
                  <tr key={agent.pubkey}>
                    {/* Standing in the whole field, computed by the hub
                        before it filtered -- numbering the matches 1, 2, 3
                        would tell a searcher their agent is winning. */}
                    <td className="itx-board-rank">{agent.rank}</td>
                    {/* Name *instead of* the key, not above it: these rows
                        are a fixed 34px (`--row-h`), so the terminal's
                        stacked treatment would not fit. */}
                    <td className="itx-board-cell-agent">
                      <Link
                        className="itx-board-agent"
                        to={`/agents/${agent.pubkey}`}
                        // The name, where there is one -- see `TapeAgent`.
                        title={agent.name ?? truncatePubkey(agent.pubkey)}
                      >
                        <ProfileIcon pubkey={agent.pubkey} size={22} className="itx-board-avatar" />
                        {/* The name in its own box, because the link is a
                            flex row: `text-overflow` needs a block to
                            clip. */}
                        <span>{agent.name ?? truncatePubkey(agent.pubkey)}</span>
                      </Link>
                    </td>
                    <td className="right">{formatCompactItx(agent.total_earned)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>

        {/* Fifty at a time, because that is what the hub serves. Hidden
            entirely on a one-page board -- a pager over a complete list is
            a control that can only ever be disabled. A search pages too:
            `total` is the number of matches. */}
        {pages > 1 && (
          <div className="itx-board-pages">
            {/* The ends, on a field this deep -- fifty agents a page over a
                couple of thousand makes "back to the top" a click per page.
                Past two pages only: with two, stepping already reaches both
                ends. */}
            {pages > 2 && (
              <button
                type="button"
                onClick={() => onPage(0)}
                disabled={page === 0}
                aria-label="First page of agents"
              >
                <Triangle direction="left" toEnd />
              </button>
            )}
            <button
              type="button"
              onClick={() => onPage(page - 1)}
              disabled={page === 0}
              aria-label="Previous page of agents"
            >
              <Triangle direction="left" />
            </button>
            <span>
              {page * LEADERBOARD_PAGE_SIZE + 1}–
              {Math.min((page + 1) * LEADERBOARD_PAGE_SIZE, total)} of {formatCount(total)}
            </span>
            <button
              type="button"
              onClick={() => onPage(page + 1)}
              disabled={page >= pages - 1}
              aria-label="Next page of agents"
            >
              <Triangle direction="right" />
            </button>
            {pages > 2 && (
              <button
                type="button"
                onClick={() => onPage(pages - 1)}
                disabled={page >= pages - 1}
                aria-label="Last page of agents"
              >
                <Triangle direction="right" toEnd />
              </button>
            )}
          </div>
        )}
      </div>
    </>
  );
}

/** One sector's individual markets, as its tickers.
 *
 * Memoized because the carousel re-renders `Board` every time the front
 * sector changes -- during a drag, every panel boundary the row crosses.
 * The panels' own props survive those renders, so a wall of sparkline
 * tables can sit out a render that only moved the nav highlight. */
const SectorPanel = memo(function SectorPanel({
  sector,
  sort,
  onSort,
  onOpen,
  windowLabel,
  loading,
  error,
}: {
  sector: SectorSummary;
  windowLabel: string;
  loading: boolean;
  error: Error | null;
  /** Opens a market's chart. Stable, so `memo` still holds -- an inline
   * arrow here would re-render every panel on every poll. */
  onOpen: (capability: string) => void;
  /** Held by `Board` rather than per panel, so the carousel stays one
   * comparable board: sorting by change in one sector and by value in the
   * next would make two panels side by side mean different things. */
  sort: MarketSort;
  onSort: (sort: MarketSort) => void;
}) {
  // Sorted here rather than upstream: the summaries are memoized and
  // shared between panels, and the order is a view state that changes
  // without the data changing.
  const markets = useMemo(() => sortMarkets(sector.markets, sort), [sector.markets, sort]);
  // No `useFitRows` here, unlike every other panel: these are sized *by*
  // their rows rather than measured for how many they can hold, capped at
  // `MAX_MARKET_ROWS`. They still finish level with each other because the
  // carousel is a flex row, so every item stretches to the tallest.
  return (
    <section className="itx-board-panel itx-board-panel-market">
      {/* Not an `itx-board-fit` box, unlike every other panel's inner div:
          that class is `flex: 1 1 0` with `overflow: hidden`, which is what
          a *measured* panel needs. Here the rows set the height, so the
          same box would clamp the panel to its floor and clip the table. */}
      <div>
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
                {/* Unlabelled: the same quantity `value` names, drawn
                    rather than written. */}
                <th aria-hidden="true" />
                <SortHeader column="value" label="value" sort={sort} onSort={onSort} />
                <SortHeader column="change" label="change" sort={sort} onSort={onSort} />
              </tr>
            </thead>
            <tbody>
              {markets.slice(0, MAX_MARKET_ROWS).map((m) => (
                <tr key={m.capability}>
                  {/* Opens the market's chart rather than navigating to its
                      task list: the question a row in a table of *prices*
                      asks is what the market has done, which the task list
                      cannot answer. */}
                  <td className="itx-board-cell-market">
                    {/* Titled because the cell clips: the longest tags
                        lose a character at the narrowest panel width. */}
                    <button type="button" title={m.capability} onClick={() => onOpen(m.capability)}>
                      {marketLabel(m.capability)}
                    </button>
                  </td>
                  <td className="itx-board-cell-spark">
                    <Sparkline
                      values={m.series}
                      width={52}
                      direction={directionOf(m.changePct)}
                      label={`bounty posted in ${m.capability} over the last ${windowLabel}`}
                    />
                  </td>
                  <td className="right itx-board-cell-value">{formatCompactItx(m.value)}</td>
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

/** A sortable column heading: the label, and a caret on whichever column
 * is currently ordering the table, pointing the way it is ordered.
 *
 * Clicking the active column flips its direction; clicking the other takes
 * it over at `desc`, because "most" is what anyone means the first time
 * they sort by a number. `aria-sort` carries the same fact to a screen
 * reader, which is the part a caret alone cannot do. */
function SortHeader({
  column,
  label,
  sort,
  onSort,
}: {
  column: MarketSortKey;
  label: string;
  sort: MarketSort;
  onSort: (sort: MarketSort) => void;
}) {
  const active = sort.key === column;
  const direction = active ? sort.direction : undefined;
  return (
    <th
      className="right itx-board-sorth"
      aria-sort={active ? (sort.direction === "asc" ? "ascending" : "descending") : "none"}
    >
      <button
        type="button"
        className={active ? "is-active" : undefined}
        onClick={() =>
          onSort({
            key: column,
            direction: active && sort.direction === "desc" ? "asc" : "desc",
          })
        }
      >
        <span className="itx-board-caret" aria-hidden="true">
          {direction ? <Triangle direction={direction === "asc" ? "up" : "down"} /> : null}
        </span>
        {label}
      </button>
    </th>
  );
}

/** The board's left rail: jump links to the four sections, then the live
 * list of sectors, then the pages that carry on past the board.
 *
 * The sector entries are the useful part -- with three panels visible at a
 * time, the pager alone means clicking through the carousel to find one.
 * These select it directly, and whichever is at the front is marked. */
function BoardNav({
  sectors,
  firstVisible,
  lastVisible,
  expanded,
  setExpanded,
  onSelect,
  column,
}: {
  sectors: SectorSummary[];
  /** The sectors on screen, as an inclusive index range: the row shows two
   * to four panels at once, and the last one never reaches the leading
   * edge, so marking everything visible is both simpler and true. */
  firstVisible: number;
  lastVisible: number;
  /** Whether the overview's sectors are showing. Held by `Board` because
   * the carousel opens it too. It never closes itself. */
  expanded: boolean;
  setExpanded: (expanded: boolean) => void;
  onSelect: (index: number) => void;
  /** This column's own width. The grip is positioned against the column,
   * so it has to live inside it. */
  column: ColumnWidth;
}) {
  return (
    <nav className="itx-board-nav" id="itx-board-nav" aria-label="Board sections">
      <ColumnGrip column={column} />
      {/* The pinned box, not the column -- see `.itx-board-pin`. */}
      <div className="itx-board-pin">
      {/* Where the other columns have a label. A list of section names
       * needs no heading -- but it does need the height one takes, or this
       * panel would start above the panels it sits beside. */}
      <span className="itx-board-label itx-board-label-spacer" aria-hidden="true">
        {"\u00a0"}
        <span className="itx-board-label-sub">{"\u00a0"}</span>
      </span>
      <div className="itx-board-panel itx-board-panel-nav">
        {/* Only the sections you have to travel to: the leaderboard and
            trends are in a rail pinned to the viewport, so a link to them
            scrolls nothing. The sectors sit *inside* the overview's entry
            rather than under a heading of their own. */}
        <ul className="itx-board-navlist">
          <li className="itx-board-navgroup">
            {/* Toggles rather than only opening; following the link is
                unaffected either way -- the anchor still resolves. */}
            <a
              href="#itx-board-overview"
              aria-expanded={expanded}
              aria-controls="itx-board-navsectors"
              onClick={() => setExpanded(!expanded)}
            >
              market overview
            </a>
            {/* Every sector, not as many as fit. Measured like the panels
                are, this list lost entries the moment the column was capped
                at the carousel's height. */}
            {expanded && (
              <ul
                className="itx-board-navlist itx-board-navlist-sectors"
                id="itx-board-navsectors"
              >
                {sectors.map((s, index) => {
                  const showing = index >= firstVisible && index <= lastVisible;
                  return (
                    <li key={s.name}>
                      <button
                        type="button"
                        className={showing ? "is-active" : undefined}
                        aria-current={showing ? "true" : undefined}
                        onClick={() => onSelect(index)}
                      >
                        {s.name}
                      </button>
                    </li>
                  );
                })}
              </ul>
            )}
          </li>
          {/* Leaving the overview puts its sectors away, and the entries
              run in the order the sections appear below. */}
          <li>
            <a href="#itx-board-latest" onClick={() => setExpanded(false)}>
              latest
            </a>
          </li>
          <li>
            <a href="#itx-board-activity" onClick={() => setExpanded(false)}>
              activity
            </a>
          </li>
          <li>
            <a href="#itx-board-sectors" onClick={() => setExpanded(false)}>
              breakdown
            </a>
          </li>
        </ul>

        <ul className="itx-board-navlist itx-board-navlist-pages">
          <li>
            <Link to="/tasks">all tasks</Link>
          </li>
          <li>
            <Link to="/leaderboard">full leaderboard</Link>
          </li>
        </ul>
      </div>
      </div>
    </nav>
  );
}

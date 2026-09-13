import { useRef, useState } from "react";
import TimeSeriesChart from "../../components/TimeSeriesChart";
import Triangle from "../../components/Triangle";
import { useCarousel } from "../../hooks/useCarousel";
import { useElementWidth } from "../../hooks/useElementWidth";
import { useOnceVisible } from "../../hooks/useOnceVisible";
import { marketLabel } from "../../lib/sectors";
import { directionOf, formatCompactItx, formatCount, formatPct } from "../../lib/format";
import type { MarketSummary, SectorSummary, SeriesWindow } from "../../lib/series";

/** Sectors shown before the section asks to be expanded. Three whole
 * ones and the top of a fourth, faded: the board has nine, and nine
 * rows of charts is a page of them. */
const SHOWN_SECTORS = 3;
/** Sectors per page once it is expanded. A board can grow sectors
 * without limit -- a sector exists because somebody posted in it -- and
 * the section must not grow with it. Ten rows of charts is as long a
 * page as this wants to be; the rest are a page away. */
const SECTORS_PER_PAGE = 10;
/** The tiles' charts, in pixels. Kept in step with
 * `.itx-activity-plot`'s `min-height`. */
const CHART_H = 132;

/** Every market on the board, as a chart each, grouped by sector.
 *
 * The sectors run top to bottom by the open bounty in them -- the same
 * order the carousel and the breakdown use -- and each is a row of its
 * markets, biggest open bounty first, that scrolls the way the market
 * overview's row does: the same hook, the same arrows, the same fades,
 * and as many tiles across as that row shows panels. Drawn from the
 * summary the board already holds: every market's running bounty line
 * is in `MarketSummary.series`, so this asks the hub for nothing the
 * carousel did not.
 *
 * The first three sectors show whole. The fourth shows faded, under a
 * control that opens the rest -- a teaser of what is there rather than a
 * count of it, so the reader knows the section goes on without nine
 * rows of it going on. Opened, the rest come ten at a time, with a pager
 * beside the control that closes them again, so a board of forty
 * sectors is four pages and not forty rows.
 *
 * A tile draws its chart only once it has been on screen. Every market
 * on the board is a tile here, in rows that scroll off to the right and
 * pages that open below the fold, and a chart for each of hundreds of
 * markets that nobody has scrolled to is hundreds of charts for nothing.
 */
export default function MarketActivity({
  sectors,
  window,
  onOpen,
}: {
  sectors: SectorSummary[];
  window: SeriesWindow;
  /** Opens a market's full chart in the middle column. */
  onOpen: (capability: string) => void;
}) {
  const [expanded, setExpanded] = useState(false);
  // Clamped rather than reset: the board re-asks the hub every few
  // seconds and the sector count can change between polls.
  const [wantedPage, setWantedPage] = useState(0);
  const section = useRef<HTMLElement | null>(null);
  const total = sectors.reduce((sum, s) => sum + s.openBounty, 0);
  // The summary carries a window but no instants, so the axis is
  // labelled from the poll's own clock -- the same assumption the sector
  // panels' sparklines already make of the same series.
  const endMs = Date.now();
  const startMs = endMs - window.windowMs;

  if (sectors.length === 0) return null;

  const collapsible = sectors.length > SHOWN_SECTORS;
  const pageCount = Math.max(1, Math.ceil(sectors.length / SECTORS_PER_PAGE));
  const page = Math.min(wantedPage, pageCount - 1);
  const pageStart = page * SECTORS_PER_PAGE;
  const shown = !collapsible
    ? sectors
    : expanded
      ? sectors.slice(pageStart, pageStart + SECTORS_PER_PAGE)
      : sectors.slice(0, SHOWN_SECTORS);
  const teaser = collapsible && !expanded ? sectors[SHOWN_SECTORS] : null;

  /** Turns the page and brings the section's top back into view: the
   * pager is at the section's foot, so without this the reader would be
   * left looking at the new page's end. The anchor's own scroll offset
   * keeps the heading clear of the masthead. */
  const turnTo = (next: number) => {
    setWantedPage(next);
    const el = section.current;
    if (el && typeof el.scrollIntoView === "function") {
      const reduced = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
      el.scrollIntoView({ block: "start", behavior: reduced ? "auto" : "smooth" });
    }
  };
  const block = (sector: SectorSummary) => (
    <SectorBlock
      key={sector.name}
      sector={sector}
      share={total > 0 ? sector.openBounty / total : 0}
      startMs={startMs}
      endMs={endMs}
      onOpen={onOpen}
    />
  );

  return (
    <section
      className="itx-market-activity"
      id="itx-board-activity"
      aria-label="Market activity"
      ref={section}
    >
      <div className="itx-board-labels itx-board-labels-section">
        <h2 className="itx-board-title">market activity</h2>
      </div>
      {shown.map(block)}
      {/* The fourth sector, faded out under the fold: visible enough to
          say the section continues, hidden from assistive tech and from
          the pointer because it is a picture of the next block rather
          than the block. */}
      {teaser && (
        <div className="itx-activity-teaser" aria-hidden="true">
          {block(teaser)}
        </div>
      )}
      {collapsible && (
        <div className={expanded && pageCount > 1 ? "itx-activity-foot has-pages" : "itx-activity-foot"}>
          <button
            type="button"
            className="itx-activity-expand"
            aria-expanded={expanded}
            onClick={() => setExpanded((e) => !e)}
          >
            {expanded ? (
              <>
                <span aria-hidden="true">▴</span> top {SHOWN_SECTORS} sectors
              </>
            ) : (
              <>
                <span aria-hidden="true">▾</span> all {formatCount(sectors.length)} sectors
              </>
            )}
          </button>
          {/* Hidden below two pages -- a pager over a complete list is a
              control that can only ever be disabled. */}
          {expanded && pageCount > 1 && (
            <div className="itx-board-pages">
              <button
                type="button"
                onClick={() => turnTo(page - 1)}
                disabled={page === 0}
                aria-label="Previous page of sectors"
              >
                <Triangle direction="left" />
              </button>
              <span>
                {pageStart + 1}–{pageStart + shown.length} of {formatCount(sectors.length)}
              </span>
              <button
                type="button"
                onClick={() => turnTo(page + 1)}
                disabled={page >= pageCount - 1}
                aria-label="Next page of sectors"
              >
                <Triangle direction="right" />
              </button>
            </div>
          )}
        </div>
      )}
    </section>
  );
}

function SectorBlock({
  sector,
  share,
  startMs,
  endMs,
  onOpen,
}: {
  sector: SectorSummary;
  /** This sector's open bounty as a fraction of the board's. */
  share: number;
  startMs: number;
  endMs: number;
  onOpen: (capability: string) => void;
}) {
  // The market overview's own row mechanics: a real scroll container the
  // browser drives -- finger, wheel, momentum -- with the hook supplying
  // what it cannot know, which tile is current and where an arrow lands.
  const [row, carousel] = useCarousel<HTMLUListElement>(sector.markets.length);

  return (
    <div className="itx-activity-sector">
      <div className="itx-activity-sector-head">
        <span className="itx-board-label">
          {sector.name}
          <span className="itx-board-label-sub">
            {(share * 100).toFixed(1)}% of open bounty · {formatCount(sector.markets.length)}{" "}
            {sector.markets.length === 1 ? "market" : "markets"} · {formatCompactItx(sector.openBounty)} itx
          </span>
        </span>
        {/* Disabled at the ends rather than wrapping, like the overview's:
            the row is a scroll, and a control that jumped the whole way
            back would contradict what dragging it does. The labels name
            the sector because every sector on the page has a pair. */}
        <div className="itx-board-pages">
          <button
            type="button"
            onClick={() => carousel.step(-1)}
            disabled={carousel.atStart}
            aria-label={`Previous ${sector.name} markets`}
          >
            <Triangle direction="left" />
          </button>
          <span>
            {carousel.firstVisible + 1}–{carousel.lastVisible + 1} of{" "}
            {formatCount(sector.markets.length)}
          </span>
          <button
            type="button"
            onClick={() => carousel.step(1)}
            disabled={carousel.atEnd}
            aria-label={`Next ${sector.name} markets`}
          >
            <Triangle direction="right" />
          </button>
        </div>
      </div>
      {/* Which end the row is against, as a pair of flags: whether an
          edge fades, and how, is the stylesheet's business. */}
      <ul
        className="itx-activity-row"
        ref={row}
        data-at-start={carousel.atStart || undefined}
        data-at-end={carousel.atEnd || undefined}
      >
        {sector.markets.map((market) => (
          <MarketTile key={market.capability} market={market} startMs={startMs} endMs={endMs} onOpen={onOpen} />
        ))}
      </ul>
    </div>
  );
}

/** One market: its bounty posted over the window, beside the market
 * chart in miniature -- the same line, since `MarketSummary.series` is
 * already that running total. The whole panel opens the full chart.
 *
 * `role="button"` on a div rather than a real `<button>`: the content is
 * a figure and a chart, which is block content a button may not hold,
 * and a button's name is its whole text -- which suits this one, since
 * the name is then the market and its figure. */
function MarketTile({
  market,
  startMs,
  endMs,
  onOpen,
}: {
  market: MarketSummary;
  startMs: number;
  endMs: number;
  onOpen: (capability: string) => void;
}) {
  const direction = directionOf(market.changePct);
  const [plot, width] = useElementWidth<HTMLDivElement>();
  // Drawn once the tile has been on screen, and not before -- see the
  // section's note. The box keeps its height meanwhile, so the row does
  // not change shape as its tiles arrive.
  const [card, seen] = useOnceVisible<HTMLDivElement>();
  const open = () => onOpen(market.capability);

  return (
    <li className="itx-activity-tile">
      <div
        className="itx-board-panel itx-activity-card"
        ref={card}
        role="button"
        tabIndex={0}
        title={market.capability}
        onClick={open}
        onKeyDown={(event) => {
          if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            open();
          }
        }}
      >
        {/* The market alone: the sector is the block's own heading. The
            full tag is the title, since the label clips at the narrowest
            widths and the tag is what the hub knows the market by. */}
        <span className="itx-activity-label">{marketLabel(market.capability)}</span>
        <div className="itx-activity-body">
          <div className="itx-activity-figure">
            <span className="itx-activity-value">{formatCompactItx(market.value)} itx</span>
            <span
              className={`itx-activity-change ${direction}`}
              title="bounty posted in the second half of this window against the first"
            >
              {formatPct(market.changePct)}
            </span>
          </div>
          {/* Measured like the market chart's box: `width` is 0 until the
              plot has been laid out, and a chart drawn at zero width is a
              chart drawn wrong. */}
          <div className="itx-activity-plot" ref={plot}>
            {seen && width > 0 && market.series.length > 0 && (
              <TimeSeriesChart
                values={market.series}
                startMs={startMs}
                endMs={endMs}
                width={width}
                height={CHART_H}
                direction={direction}
                valueNoun={`Bounty posted in ${market.capability}`}
                // The axis labels here are `4K`, not `30,000`: most of
                // the default gutter was air to the right of them.
                gutterRight={44}
              />
            )}
          </div>
        </div>
      </div>
    </li>
  );
}

import { useState } from "react";
import TimeSeriesChart from "../../components/TimeSeriesChart";
import Triangle from "../../components/Triangle";
import { useCarousel } from "../../hooks/useCarousel";
import { useElementWidth } from "../../hooks/useElementWidth";
import { marketLabel } from "../../lib/sectors";
import { directionOf, formatCompactItx, formatCount, formatPct } from "../../lib/format";
import type { MarketSummary, SectorSummary, SeriesWindow } from "../../lib/series";

/** Sectors shown before the section asks to be expanded. Three whole
 * ones and the top of a fourth, faded: the board has nine, and nine
 * rows of charts is a page of them. */
const SHOWN_SECTORS = 3;
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
 * rows of it going on.
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
  const total = sectors.reduce((sum, s) => sum + s.openBounty, 0);
  // The summary carries a window but no instants, so the axis is
  // labelled from the poll's own clock -- the same assumption the sector
  // panels' sparklines already make of the same series.
  const endMs = Date.now();
  const startMs = endMs - window.windowMs;

  if (sectors.length === 0) return null;

  const collapsible = sectors.length > SHOWN_SECTORS;
  const shown = expanded || !collapsible ? sectors : sectors.slice(0, SHOWN_SECTORS);
  const teaser = collapsible && !expanded ? sectors[SHOWN_SECTORS] : null;
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
    <section className="itx-market-activity" id="itx-board-activity" aria-label="Market activity">
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
  const open = () => onOpen(market.capability);

  return (
    <li className="itx-activity-tile">
      <div
        className="itx-board-panel itx-activity-card"
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
            {width > 0 && market.series.length > 0 && (
              <TimeSeriesChart
                values={market.series}
                startMs={startMs}
                endMs={endMs}
                width={width}
                height={CHART_H}
                direction={direction}
                valueNoun={`Bounty posted in ${market.capability}`}
              />
            )}
          </div>
        </div>
      </div>
    </li>
  );
}

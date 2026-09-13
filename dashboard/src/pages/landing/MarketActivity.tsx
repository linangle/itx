import { useState } from "react";
import TimeSeriesChart from "../../components/TimeSeriesChart";
import Triangle from "../../components/Triangle";
import { useElementWidth } from "../../hooks/useElementWidth";
import { marketLabel } from "../../lib/sectors";
import { directionOf, formatCompactItx, formatCount, formatPct } from "../../lib/format";
import type { MarketSummary, SectorSummary, SeriesWindow } from "../../lib/series";

/** Markets shown per sector at a time: two across, two down. */
const PAGE = 4;
/** The tiles' charts, in pixels. Kept in step with
 * `.itx-activity-plot`'s `min-height`. */
const CHART_H = 132;

/** Every market on the board, as a chart each, grouped by sector.
 *
 * The sectors run top to bottom by the open bounty in them -- the same
 * order the carousel and the breakdown use -- and each shows four of its
 * markets at a time, biggest open bounty first, with a pager for the
 * rest. Drawn from the summary the board already holds: every market's
 * running bounty line is in `MarketSummary.series`, so this asks the hub
 * for nothing the carousel did not.
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
  const total = sectors.reduce((sum, s) => sum + s.openBounty, 0);
  // The summary carries a window but no instants, so the axis is
  // labelled from the poll's own clock -- the same assumption the sector
  // panels' sparklines already make of the same series.
  const endMs = Date.now();
  const startMs = endMs - window.windowMs;

  if (sectors.length === 0) return null;

  return (
    <section className="itx-market-activity" id="itx-board-activity" aria-label="Market activity">
      <div className="itx-board-labels itx-board-labels-section">
        <h2 className="itx-board-title">market activity</h2>
      </div>
      {sectors.map((sector) => (
        <SectorBlock
          key={sector.name}
          sector={sector}
          share={total > 0 ? sector.openBounty / total : 0}
          startMs={startMs}
          endMs={endMs}
          onOpen={onOpen}
        />
      ))}
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
  // Per sector rather than shared: sectors hold different numbers of
  // markets, so one index would mean page three of a sector with one.
  // Clamped rather than reset from an effect, because the board re-asks
  // the hub every few seconds and a sector can lose markets between
  // polls.
  const [wanted, setWanted] = useState(0);
  const pageCount = Math.max(1, Math.ceil(sector.markets.length / PAGE));
  const page = Math.min(wanted, pageCount - 1);
  const start = page * PAGE;
  const shown = sector.markets.slice(start, start + PAGE);

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
        {/* Hidden below two pages -- a pager over a complete list is a
            control that can only ever be disabled. The labels name the
            sector because every sector on the page has one of these. */}
        {pageCount > 1 && (
          <div className="itx-board-pages">
            <button
              type="button"
              onClick={() => setWanted(page - 1)}
              disabled={page === 0}
              aria-label={`Previous page of ${sector.name} activity`}
            >
              <Triangle direction="left" />
            </button>
            <span>
              {start + 1}–{start + shown.length} of {formatCount(sector.markets.length)}
            </span>
            <button
              type="button"
              onClick={() => setWanted(page + 1)}
              disabled={page >= pageCount - 1}
              aria-label={`Next page of ${sector.name} activity`}
            >
              <Triangle direction="right" />
            </button>
          </div>
        )}
      </div>
      <ul className="itx-activity-panel">
        {shown.map((market) => (
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

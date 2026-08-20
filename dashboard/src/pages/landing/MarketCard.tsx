import { useMemo, useState } from "react";
import { useElementWidth } from "../../hooks/useElementWidth";
import { formatCount } from "../../lib/format";
import {
  STEPS,
  axisDates,
  paysOut,
  quoteAt,
  snapIndex,
  type SampleMarket,
} from "../../lib/predictionSample";

/** One sample prediction market, drawn as a card.
 *
 * Its own module because it has two homes: the board's carousel
 * (`PredictionMarket`) and the full market page (`PredictionsPage`).
 *
 * The card is layout-agnostic: it fills whatever box it is given. The
 * carousel sizes it as a flex item and the page as a grid cell — see
 * `.itx-pm-card` and `.itx-pmpage-grid`.
 */

/** Which way an outcome's odds lean, as the class that colours its pill.
 *
 * Derived from the odds themselves rather than from whether the row is
 * the "yes" -- in a binary market the two prices sum to 100, so this is
 * simply "is this the side above the coin flip", and it stays right if a
 * market's favourite changes. */
function leaning(pct: number): "up" | "down" | "flat" {
  if (pct === 50) return "flat";
  return pct > 50 ? "up" : "down";
}

/** One sample market, in the reference's format: the odds table and its
 * news on the left, both outcomes' history on the right. */
export default function MarketCard({ market }: { market: SampleMarket }) {
  return (
    <article className="itx-board-panel itx-pm-card">
      <div className="itx-pm-info">
        {/* The category, as a word. The reference puts an event's own
            logo here -- a club crest, a party's mark -- and a drawn
            stand-in for one is a picture of nothing: it carried no
            information the word beside it did not already carry, and
            two coloured bars on a card that also has a two-line chart
            said "chart" twice. */}
        <span className="itx-pm-cat">{market.category}</span>

        <h3 className="itx-pm-title">{market.title}</h3>

        {/* Said plainly on the card, not only in a comment nobody
            reading the page can see. A card quoting a price and a
            volume looks exactly like a live one, and this is the only
            thing that says it is not. */}
        <p className="itx-pm-sample">
          sample market — authored odds, nothing on the wire yet
        </p>

        <table className="itx-board-table itx-pm-outcomes">
          <thead>
            <tr>
              <th>market</th>
              <th className="right">pays out</th>
              <th className="right">odds</th>
            </tr>
          </thead>
          <tbody>
            {[
              { outcome: market.yes, tone: "is-yes" },
              { outcome: market.no, tone: "is-no" },
            ].map(({ outcome, tone }) => (
              <tr key={outcome.label}>
                <td>
                  <span className={`itx-pm-outcome ${tone}`}>{outcome.label}</span>
                </td>
                <td className="right itx-pm-pays">{paysOut(outcome.pct)}</td>
                <td className="right">
                  {/* A span, not a button: on the reference this is
                      where a trade starts, and until the protocol can
                      take one, drawing it clickable would be a lie.

                      Outlined by which way the market is leaning, not
                      by which row it is: green is the outcome the money
                      is on, red the one it is against. That is the
                      board's own use of the pair -- up and down -- and
                      it means the colour follows the odds if they ever
                      cross rather than being pinned to "yes". A market
                      quoting an even 50/50 leans nowhere and takes
                      neither colour. */}
                  <span className={`itx-pm-pill ${leaning(outcome.pct)}`}>{outcome.pct}%</span>
                </td>
              </tr>
            ))}
          </tbody>
        </table>

        <div className="itx-pm-meta">
          <span>{formatCount(market.volumeItx)} itx vol</span>
          <span>{market.settles}</span>
        </div>

        <p className="itx-pm-news">
          <strong>news</strong> · {market.news}
        </p>
      </div>

      <div className="itx-pm-chartcol">
        <div className="itx-pm-legend">
          <span className="itx-pm-key is-yes">
            <span className="itx-pm-key-dot" aria-hidden="true" />
            {market.yes.label} <strong>{market.yes.pct}%</strong>
          </span>
          <span className="itx-pm-key is-no">
            <span className="itx-pm-key-dot" aria-hidden="true" />
            {market.no.label} <strong>{market.no.pct}%</strong>
          </span>
        </div>
        <OddsChart market={market} />
      </div>
    </article>
  );
}

/** The two odds lines over a market's week, after the reference: both
 * outcomes on one plot, dotted gridlines with the percentages down the
 * right edge, dates along the bottom, and a haloed dot on each line's
 * last price.
 *
 * Drawn at real pixel coordinates for the same reason `TimeSeriesChart`
 * is -- a scaled `viewBox` stretches label text and stroke widths along
 * with the geometry -- so it measures its box and renders nothing until
 * it has a width. Stepped, not smoothed: odds move when someone trades.
 *
 * Hovering reads the price at that moment. Both outcomes at once rather
 * than whichever line is nearest the cursor -- in a binary market the
 * pair *is* the quote. */
const CHART_H = 248;
/** Room above the plot for the hovered moment, which prints centred on
 * the rule. Part of the chart's box rather than an overlay, so the
 * label cannot land on the legend above it. */
const PAD_TOP = 28;
const PAD_BOTTOM = 24;
const PAD_LEFT = 4;
/** Room for the y labels, which sit outside the plot on the right. */
const PAD_RIGHT = 44;
/** Gridlines land on multiples of this, like the reference's
 * 15/30/45/60/75 ladder. */
const GRID_STEP_PCT = 15;

/** Roughly how wide the readout's text runs, per character, at its 13px
 * bold. Estimated rather than measured: `getComputedTextLength` forces a
 * layout read, and this runs on every pointer move. Generous on purpose
 * -- a plate slightly too wide reads as padding, one too narrow reads as
 * a clipped label. */
const CHAR_W = 6.9;
const PLATE_PAD = 7;

function readoutWidth(text: string): number {
  return text.length * CHAR_W + PLATE_PAD * 2;
}

/** How close the two readouts may sit before they are parted. They are
 * pinned to their own lines, and the lines cross -- at a crossing the two
 * labels land on top of each other, which is exactly the moment the chart
 * is most worth reading. The *labels* separate about the midpoint while
 * the dots stay on their lines. */
const MIN_LABEL_GAP = 22;

function OddsChart({ market }: { market: SampleMarket }) {
  const [box, width] = useElementWidth<HTMLDivElement>();
  /** Which point the pointer is over, or `null` when it is not. */
  const [hover, setHover] = useState<number | null>(null);

  const series = market.series;

  const chart = useMemo(() => {
    if (width <= 0) return null;

    const plotW = width - PAD_LEFT - PAD_RIGHT;
    const plotH = CHART_H - PAD_TOP - PAD_BOTTOM;

    // The domain hugs the data the way the reference's does, rather than
    // spanning 0-100: gridlines at the first step-multiples that clear
    // the lines' extremes, so two lines at 72/28 use the plot's height
    // instead of its middle half.
    const values = series.flatMap((v) => [v, 100 - v]);
    const lo = Math.max(0, Math.floor((Math.min(...values) - 4) / GRID_STEP_PCT) * GRID_STEP_PCT);
    const hi = Math.min(100, Math.ceil((Math.max(...values) + 4) / GRID_STEP_PCT) * GRID_STEP_PCT);

    const x = (i: number) => PAD_LEFT + (i / (STEPS - 1)) * plotW;
    const y = (v: number) => PAD_TOP + (1 - (v - lo) / (hi - lo)) * plotH;

    const stepPath = (points: number[]) => {
      let d = `M${x(0).toFixed(1)} ${y(points[0]).toFixed(1)}`;
      for (let i = 1; i < points.length; i++) {
        d += `H${x(i).toFixed(1)}V${y(points[i]).toFixed(1)}`;
      }
      return d;
    };

    const grid: number[] = [];
    for (let pct = lo; pct <= hi; pct += GRID_STEP_PCT) grid.push(pct);

    const no = series.map((v) => 100 - v);
    return {
      grid: grid.map((pct) => ({ y: y(pct), label: `${pct}%` })),
      yes: { d: stepPath(series), endX: x(STEPS - 1), endY: y(series[STEPS - 1]) },
      no: { d: stepPath(no), endX: x(STEPS - 1), endY: y(no[no.length - 1]) },
      ticks: axisDates().map((t) => ({ x: x(t.index), label: t.label })),
      plotW,
      plotH,
      x,
      y,
    };
  }, [width, series]);

  /** Snaps the pointer to the nearest point. The rect is read per move
   * rather than cached: the row scrolls under the pointer, and a cached
   * rect offsets the whole readout as soon as it does. */
  function onMove(e: React.PointerEvent<SVGSVGElement>) {
    if (!chart) return;
    const rect = e.currentTarget.getBoundingClientRect();
    setHover(snapIndex((e.clientX - rect.left - PAD_LEFT) / chart.plotW));
  }

  const cursor = useMemo(() => {
    if (hover === null || !chart) return null;
    const quote = quoteAt(series, hover);
    const rows = [
      { pct: quote.yesPct, label: market.yes.label, tone: "is-yes", y: chart.y(quote.yesExact) },
      { pct: quote.noPct, label: market.no.label, tone: "is-no", y: chart.y(100 - quote.yesExact) },
    ].map((row) => ({ ...row, text: `${row.label} ${row.pct}%`, labelY: row.y }));

    // Part the labels at a crossing -- see MIN_LABEL_GAP. Ordered by
    // height rather than by outcome, so the pair never swaps sides as the
    // pointer moves through the crossing.
    if (Math.abs(rows[0].y - rows[1].y) < MIN_LABEL_GAP) {
      const middle = (rows[0].y + rows[1].y) / 2;
      const [above, below] = rows[0].y <= rows[1].y ? [rows[0], rows[1]] : [rows[1], rows[0]];
      above.labelY = middle - MIN_LABEL_GAP / 2;
      below.labelY = middle + MIN_LABEL_GAP / 2;
    }

    const x = chart.x(hover);
    // One side for both readouts, chosen by the wider of the two: the
    // pair reads as one legend, and splitting them either side of the
    // rule when only one would overflow looks like a fault.
    const widest = Math.max(...rows.map((r) => readoutWidth(r.text)));
    const flip = x + 10 + widest > PAD_LEFT + chart.plotW;
    return { ...quote, x, rows, flip, widest };
  }, [hover, chart, series, market.yes.label, market.no.label]);

  return (
    <div className="itx-pm-plot" ref={box}>
      {chart && (
        <svg
          width={width}
          height={CHART_H}
          className="itx-pm-svg"
          role="img"
          aria-label={
            `sample odds over the last week: ${market.yes.label} at ` +
            `${market.yes.pct}%, ${market.no.label} at ${market.no.pct}%`
          }
          // Pointer events rather than mouse: the same handlers then
          // cover a finger drag across the chart on a touch screen,
          // which is the only way to read a price there at all.
          onPointerMove={onMove}
          onPointerDown={onMove}
          onPointerLeave={() => setHover(null)}
        >
          {chart.grid.map((g) => (
            <g key={g.label}>
              <line
                className="itx-pm-gridline"
                x1={PAD_LEFT}
                x2={PAD_LEFT + chart.plotW}
                y1={g.y}
                y2={g.y}
              />
              <text className="itx-pm-ylabel" x={PAD_LEFT + chart.plotW + 8} y={g.y + 3}>
                {g.label}
              </text>
            </g>
          ))}

          <path className="itx-pm-line is-yes" d={chart.yes.d} />
          <path className="itx-pm-line is-no" d={chart.no.d} />

          {/* The last price on each line. Hidden while a hover is being
              read: the cursor marks two dots of its own, and four dots
              on a two-line chart is one pair too many to interpret. */}
          {!cursor &&
            [chart.yes, chart.no].map((line, i) => (
              <g key={i} className={i === 0 ? "itx-pm-end is-yes" : "itx-pm-end is-no"}>
                <circle className="itx-pm-halo" cx={line.endX} cy={line.endY} r={9} />
                <circle className="itx-pm-dot" cx={line.endX} cy={line.endY} r={3.5} />
              </g>
            ))}

          {chart.ticks.map((t, i) => (
            <text
              key={t.label}
              className="itx-pm-xlabel"
              x={t.x}
              y={CHART_H - 6}
              textAnchor={i === 0 ? "start" : i === chart.ticks.length - 1 ? "end" : "middle"}
            >
              {t.label}
            </text>
          ))}

          {/* The hover readout, drawn last so it sits over the lines. */}
          {cursor && (
            <g className="itx-pm-cursor">
              <line
                className="itx-pm-crosshair"
                x1={cursor.x}
                x2={cursor.x}
                y1={PAD_TOP}
                y2={PAD_TOP + chart.plotH}
              />
              {/* The moment, above the rule, clamped to the plot so it
                  neither runs off the right edge nor over the y labels
                  when the pointer is at either end. */}
              <text
                className="itx-pm-when"
                x={Math.max(PAD_LEFT + 34, Math.min(PAD_LEFT + chart.plotW - 34, cursor.x))}
                y={PAD_TOP - 12}
                textAnchor="middle"
              >
                {cursor.when}
              </text>

              {cursor.rows.map((row) => {
                const w = readoutWidth(row.text);
                return (
                  <g key={row.tone} className={`itx-pm-end ${row.tone}`}>
                    <circle className="itx-pm-dot" cx={cursor.x} cy={row.y} r={4} />
                    {/* A plate under the words, so the label reads over
                        whatever the chart has behind it -- gridlines, or
                        the other outcome's line -- without hiding it. */}
                    <rect
                      className="itx-pm-plate"
                      x={cursor.flip ? cursor.x - 10 - w : cursor.x + 10}
                      y={row.labelY - 10}
                      width={w}
                      height={20}
                      rx={5}
                    />
                    <text
                      className="itx-pm-readout"
                      x={cursor.x + (cursor.flip ? -10 - PLATE_PAD : 10 + PLATE_PAD)}
                      y={row.labelY + 4}
                      textAnchor={cursor.flip ? "end" : "start"}
                    >
                      {row.text}
                    </text>
                  </g>
                );
              })}
            </g>
          )}
        </svg>
      )}
    </div>
  );
}

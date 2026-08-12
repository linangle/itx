import { useMemo, useState } from "react";
import { Link } from "react-router-dom";
import Triangle from "../../components/Triangle";
import { useElementWidth } from "../../hooks/useElementWidth";
import { formatCount } from "../../lib/format";
import {
  SAMPLE,
  YES_SERIES,
  YES_STEPS,
  axisDates,
  paysOut,
  quoteAt,
  snapIndex,
} from "../../lib/predictionSample";

/** The board's prediction market section: a label row with the way to
 * the full page, and one sample market card after the Kalshi reference.
 *
 * **Everything in it is authored.** The protocol has no outcome
 * markets, no odds and no settlement, so this section is the shape of
 * the thing rather than the thing: what a market card carries, where it
 * sits on the board, and where the full page lives. The copy and the
 * arithmetic are in `lib/predictionSample.ts`; what the hub and the
 * chain would need to make it real is in `docs/hub-requirements.md`.
 *
 * One card, deliberately. A row of fabricated markets would read as a
 * live surface; a single sample with its own pager chrome reads as a
 * format being shown. */
export default function PredictionMarket() {
  return (
    <section
      className="itx-pm"
      id="itx-board-predictions"
      aria-label="Prediction market"
    >
      {/* The same label row every board section wears. The arrow is the
          section's own door: the card below is one sample, and the full
          market -- however empty today -- is a page of its own. */}
      <div className="itx-board-labels itx-board-labels-predictions">
        <span className="itx-board-label">prediction market</span>
        <Link
          className="itx-pm-open"
          to="/predictions"
          aria-label="Open the full prediction market"
          title="full prediction market"
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

      <div className="itx-board-panel itx-pm-card">
        {/* Chrome for flipping between markets, per the reference. With
            one sample there is nowhere to flip, so both ends are
            disabled -- but the control renders, because it is part of
            the format being shown. */}
        <div className="itx-pm-pager">
          <button type="button" disabled aria-label="Previous market">
            <Triangle direction="left" />
          </button>
          <span>1 of 1</span>
          <button type="button" disabled aria-label="Next market">
            <Triangle direction="right" />
          </button>
        </div>

        <div className="itx-pm-info">
          <span className="itx-pm-cat">
            {/* A stand-in for the reference's category tile: two bars in
                the outcome colours, which is what this market is about. */}
            <svg viewBox="0 0 20 20" width="20" height="20" aria-hidden="true">
              <rect x="1" y="1" width="18" height="18" rx="4" fill="none" stroke="currentColor" strokeOpacity="0.5" />
              <rect x="5" y="7" width="3.4" height="8" rx="1" fill="var(--ld-green)" />
              <rect x="11.6" y="10" width="3.4" height="5" rx="1" fill="var(--ld-blue)" />
            </svg>
            {SAMPLE.category}
          </span>

          <h3 className="itx-pm-title">{SAMPLE.title}</h3>

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
                { outcome: SAMPLE.yes, tone: "is-yes" },
                { outcome: SAMPLE.no, tone: "is-no" },
              ].map(({ outcome, tone }) => (
                <tr key={outcome.label}>
                  <td>
                    <span className={`itx-pm-outcome ${tone}`}>{outcome.label}</span>
                  </td>
                  <td className="right itx-pm-pays">{paysOut(outcome.pct)}</td>
                  <td className="right">
                    {/* A span, not a button: on Kalshi this is where a
                        trade starts, and until the protocol can take
                        one, drawing it clickable would be a lie. */}
                    <span className="itx-pm-pill">{outcome.pct}%</span>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>

          <div className="itx-pm-meta">
            <span>{formatCount(SAMPLE.volumeItx)} itx vol</span>
            <span>{SAMPLE.settles}</span>
          </div>

          <p className="itx-pm-news">
            <strong>news</strong> · {SAMPLE.news}
          </p>
        </div>

        <div className="itx-pm-chartcol">
          <div className="itx-pm-legend">
            <span className="itx-pm-key is-yes">
              <span className="itx-pm-key-dot" aria-hidden="true" />
              {SAMPLE.yes.label} <strong>{SAMPLE.yes.pct}%</strong>
            </span>
            <span className="itx-pm-key is-no">
              <span className="itx-pm-key-dot" aria-hidden="true" />
              {SAMPLE.no.label} <strong>{SAMPLE.no.pct}%</strong>
            </span>
          </div>
          <SampleChart />
        </div>
      </div>
    </section>
  );
}

/** The two odds lines over the sample's week, after the reference: both
 * outcomes on one plot, dotted gridlines with the percentages down the
 * right edge, dates along the bottom, and a haloed dot on each line's
 * last price.
 *
 * Drawn at real pixel coordinates for the same reason `TimeSeriesChart`
 * is -- a scaled `viewBox` stretches label text and stroke widths along
 * with the geometry -- so it measures its box and renders nothing until
 * it has a width. Stepped, not smoothed: odds move when someone trades,
 * and the discrete tick is the shape of that.
 *
 * Hovering reads the price at that moment: a rule down the plot, a dot
 * where each line crosses it, and each outcome's odds beside its own
 * dot. Both outcomes at once rather than whichever line is nearest the
 * cursor -- in a binary market the pair *is* the quote, and a reader
 * following one line would be left to do the subtraction. */
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
/** About how wide a hover label runs ("15 or more 28%"). Used only to
 * decide which side of the rule it goes on -- measuring the text would
 * cost a layout read per pointer move to save nothing. */
const READOUT_W = 118;

function SampleChart() {
  const [box, width] = useElementWidth<HTMLDivElement>();
  /** Which point the pointer is over, or `null` when it is not. */
  const [hover, setHover] = useState<number | null>(null);

  const chart = useMemo(() => {
    if (width <= 0) return null;

    const plotW = width - PAD_LEFT - PAD_RIGHT;
    const plotH = CHART_H - PAD_TOP - PAD_BOTTOM;

    // The domain hugs the data the way the reference's does, rather
    // than spanning 0-100: gridlines at the first step-multiples that
    // clear the lines' extremes, so two lines at 72/28 use the plot's
    // height instead of its middle half.
    const values = YES_SERIES.flatMap((v) => [v, 100 - v]);
    const lo = Math.max(0, Math.floor((Math.min(...values) - 4) / GRID_STEP_PCT) * GRID_STEP_PCT);
    const hi = Math.min(100, Math.ceil((Math.max(...values) + 4) / GRID_STEP_PCT) * GRID_STEP_PCT);

    const x = (i: number) => PAD_LEFT + (i / (YES_STEPS - 1)) * plotW;
    const y = (v: number) => PAD_TOP + (1 - (v - lo) / (hi - lo)) * plotH;

    const stepPath = (series: number[]) => {
      let d = `M${x(0).toFixed(1)} ${y(series[0]).toFixed(1)}`;
      for (let i = 1; i < series.length; i++) {
        d += `H${x(i).toFixed(1)}V${y(series[i]).toFixed(1)}`;
      }
      return d;
    };

    const grid: number[] = [];
    for (let pct = lo; pct <= hi; pct += GRID_STEP_PCT) grid.push(pct);

    const no = YES_SERIES.map((v) => 100 - v);
    return {
      grid: grid.map((pct) => ({ y: y(pct), label: `${pct}%` })),
      yes: { d: stepPath(YES_SERIES), endX: x(YES_STEPS - 1), endY: y(YES_SERIES[YES_STEPS - 1]) },
      no: { d: stepPath(no), endX: x(YES_STEPS - 1), endY: y(no[no.length - 1]) },
      ticks: axisDates().map((t) => ({ x: x(t.index), label: t.label })),
      plotW,
      plotH,
      x,
      y,
    };
  }, [width]);

  /** Snaps the pointer to the nearest point. The rect is read per move
   * rather than cached: the board scrolls, and a cached rect offsets
   * the whole readout after the first scroll. */
  function onMove(e: React.PointerEvent<SVGSVGElement>) {
    if (!chart) return;
    const rect = e.currentTarget.getBoundingClientRect();
    setHover(snapIndex((e.clientX - rect.left - PAD_LEFT) / chart.plotW));
  }

  const cursor = useMemo(() => {
    if (hover === null || !chart) return null;
    const quote = quoteAt(hover);
    return {
      ...quote,
      x: chart.x(hover),
      yesY: chart.y(quote.yesExact),
      noY: chart.y(100 - quote.yesExact),
    };
  }, [hover, chart]);

  /** Which side of the rule the readouts sit on. They follow the
   * reference and sit to the right, until the rule is close enough to
   * the end that they would run past the plot. */
  const flip = !!cursor && chart !== null && cursor.x + READOUT_W > PAD_LEFT + chart.plotW;

  return (
    <div className="itx-pm-plot" ref={box}>
      {chart && (
        <svg
          width={width}
          height={CHART_H}
          className="itx-pm-svg"
          role="img"
          aria-label={
            `sample odds over the last week: ${SAMPLE.yes.label} at ` +
            `${SAMPLE.yes.pct}%, ${SAMPLE.no.label} at ${SAMPLE.no.pct}%`
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
              on a chart with two lines is one pair too many to
              interpret. */}
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

              {[
                { y: cursor.yesY, pct: cursor.yesPct, label: SAMPLE.yes.label, tone: "is-yes" },
                { y: cursor.noY, pct: cursor.noPct, label: SAMPLE.no.label, tone: "is-no" },
              ].map((row) => (
                <g key={row.tone} className={`itx-pm-end ${row.tone}`}>
                  <circle className="itx-pm-dot" cx={cursor.x} cy={row.y} r={4} />
                  <text
                    className="itx-pm-readout"
                    x={cursor.x + (flip ? -10 : 10)}
                    y={row.y + 5}
                    textAnchor={flip ? "end" : "start"}
                  >
                    {row.label} {row.pct}%
                  </text>
                </g>
              ))}
            </g>
          )}
        </svg>
      )}
    </div>
  );
}

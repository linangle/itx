import { describe, expect, it } from "vitest";
import {
  SAMPLES,
  SPAN_MS,
  STEPS,
  axisDates,
  momentLabel,
  paysOut,
  quoteAt,
  snapIndex,
  walk,
} from "./predictionSample";

describe("the sample prediction markets", () => {
  it("offers more than one, so the row has somewhere to scroll", () => {
    expect(SAMPLES.length).toBeGreaterThan(1);
    // Keys are what the carousel renders by, so a duplicate would drop a
    // card rather than fail loudly.
    expect(new Set(SAMPLES.map((m) => m.key)).size).toBe(SAMPLES.length);
  });

  it("quotes binary markets: each card's two outcomes sum to 100", () => {
    for (const market of SAMPLES) {
      expect(market.yes.pct + market.no.pct).toBe(100);
    }
  });

  it("pays out the reciprocal of the odds", () => {
    // What a card's middle column says, and the reason it is derived
    // rather than authored: 72% and 1.39x have to stay in step when
    // either is edited.
    expect(paysOut(72)).toBe("1.39x");
    expect(paysOut(28)).toBe("3.57x");
    expect(paysOut(50)).toBe("2.00x");
  });

  it("draws the same history on every load, ending on the quoted price", () => {
    // Seeded, so a sample's past cannot be redrawn by a reload -- the
    // one thing a price history must not do, sample or not. And each
    // walk is eased onto its quoted odds, so the line finishes exactly
    // where the pill says the market stands.
    for (const market of SAMPLES) {
      expect(market.series).toHaveLength(STEPS);
      expect(market.series[STEPS - 1]).toBeCloseTo(market.yes.pct, 10);
      expect(Math.min(...market.series)).toBeGreaterThan(0);
      expect(Math.max(...market.series)).toBeLessThan(100);
    }
  });

  it("gives each market a history of its own", () => {
    // Three cards drawn from one series would read as one market shown
    // three times, which is the thing a row of samples must not do.
    const [a, b, c] = SAMPLES.map((m) => m.series.join());
    expect(new Set([a, b, c]).size).toBe(3);
  });

  it("moves a volatile market further than a quiet one", () => {
    // `volatility` is the per-market knob that keeps the three from
    // looking alike; this is the property it is there for.
    const span = (points: number[]) => Math.max(...points) - Math.min(...points);
    expect(span(walk(7, 50, 50, 12))).toBeGreaterThan(span(walk(7, 50, 50, 3)));
  });
});

describe("reading a price off a sample chart", () => {
  const series = SAMPLES[0].series;

  it("snaps a position across the plot to the nearest point", () => {
    expect(snapIndex(0)).toBe(0);
    expect(snapIndex(1)).toBe(STEPS - 1);
    expect(snapIndex(0.5)).toBe(Math.round((STEPS - 1) / 2));
  });

  it("clamps a pointer that has run past either end", () => {
    // The handler subtracts the plot's left padding before dividing, so
    // a pointer in that padding produces a small negative fraction --
    // and a drag can leave the box entirely before the leave fires.
    expect(snapIndex(-0.4)).toBe(0);
    expect(snapIndex(3)).toBe(STEPS - 1);
    expect(snapIndex(NaN)).toBe(0);
  });

  it("reports both sides of the market at the hovered moment", () => {
    const q = quoteAt(series, 40, Date.UTC(2026, 7, 12, 18));
    // Complementary whole numbers: the readout and the odds pill quote
    // the same figure at the same precision.
    expect(q.yesPct + q.noPct).toBe(100);
    expect(Number.isInteger(q.yesPct)).toBe(true);
    // The exact value is kept for plotting -- the line is drawn from it,
    // and rounding there would make the curve step in whole percents.
    expect(q.yesExact).toBe(series[40]);
  });

  it("places the last point now and the first a span ago", () => {
    const now = Date.UTC(2026, 7, 12, 18);
    expect(quoteAt(series, STEPS - 1, now).at).toBe(now);
    expect(quoteAt(series, 0, now).at).toBe(now - SPAN_MS);
  });

  it("labels a moment the way the reference reads it", () => {
    // Lowercase, like the rest of this surface, and "at" between the
    // date and the hour rather than a comma.
    expect(momentLabel(new Date("2026-08-09T14:00:00").getTime())).toBe("aug 9 at 2 pm");
  });

  it("derives the axis dates from the clock, so they never go stale", () => {
    const dates = axisDates(Date.UTC(2026, 7, 12, 18));
    expect(dates).toHaveLength(4);
    expect(dates[0].index).toBe(0);
    expect(dates[3].index).toBe(STEPS - 1);
    // Four distinct days across a week-long span, in order.
    expect(dates.map((d) => d.label)).toEqual([...new Set(dates.map((d) => d.label))]);
  });
});

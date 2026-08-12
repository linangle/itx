import { describe, expect, it } from "vitest";
import {
  SAMPLE,
  SPAN_MS,
  YES_SERIES,
  YES_STEPS,
  axisDates,
  momentLabel,
  paysOut,
  quoteAt,
  snapIndex,
} from "./predictionSample";

describe("the sample prediction market", () => {
  it("quotes a binary market: the two outcomes sum to 100", () => {
    expect(SAMPLE.yes.pct + SAMPLE.no.pct).toBe(100);
  });

  it("pays out the reciprocal of the odds", () => {
    // What the card's middle column says, and the reason it is derived
    // rather than authored: 72% and 1.39x have to stay in step when
    // either is edited.
    expect(paysOut(72)).toBe("1.39x");
    expect(paysOut(28)).toBe("3.57x");
    expect(paysOut(50)).toBe("2.00x");
  });

  it("draws the same history on every load, ending on the quoted price", () => {
    // Seeded, so a sample's past cannot be redrawn by a reload -- the
    // one thing a price history must not do, sample or not. And the
    // walk is eased onto the quoted odds, so the line finishes exactly
    // where the pill says the market stands.
    expect(YES_SERIES).toHaveLength(YES_STEPS);
    expect(YES_SERIES[YES_STEPS - 1]).toBeCloseTo(SAMPLE.yes.pct, 10);
    expect(Math.min(...YES_SERIES)).toBeGreaterThan(0);
    expect(Math.max(...YES_SERIES)).toBeLessThan(100);
  });
});

describe("reading a price off the sample chart", () => {
  it("snaps a position across the plot to the nearest point", () => {
    expect(snapIndex(0)).toBe(0);
    expect(snapIndex(1)).toBe(YES_STEPS - 1);
    expect(snapIndex(0.5)).toBe(Math.round((YES_STEPS - 1) / 2));
  });

  it("clamps a pointer that has run past either end", () => {
    // The handler subtracts the plot's left padding before dividing, so
    // a pointer in that padding produces a small negative fraction --
    // and a drag can leave the box entirely before the leave fires.
    expect(snapIndex(-0.4)).toBe(0);
    expect(snapIndex(3)).toBe(YES_STEPS - 1);
    expect(snapIndex(NaN)).toBe(0);
  });

  it("reports both sides of the market at the hovered moment", () => {
    const q = quoteAt(40, Date.UTC(2026, 7, 12, 18));
    // Complementary whole numbers: the readout and the odds pill quote
    // the same figure at the same precision.
    expect(q.yesPct + q.noPct).toBe(100);
    expect(Number.isInteger(q.yesPct)).toBe(true);
    // The exact value is kept for plotting -- the line is drawn from it,
    // and rounding there would make the curve step in whole percents.
    expect(q.yesExact).toBe(YES_SERIES[40]);
  });

  it("places the last point now and the first a span ago", () => {
    const now = Date.UTC(2026, 7, 12, 18);
    expect(quoteAt(YES_STEPS - 1, now).at).toBe(now);
    expect(quoteAt(0, now).at).toBe(now - SPAN_MS);
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
    expect(dates[3].index).toBe(YES_STEPS - 1);
    // Ends today and starts a week back, in order.
    expect(dates.map((d) => d.label)).toEqual([...new Set(dates.map((d) => d.label))]);
  });
});

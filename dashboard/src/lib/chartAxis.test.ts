import { describe, expect, it } from "vitest";
import { bucketTime, timeLabel, timeTicks, valueScale } from "./chartAxis";

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

describe("valueScale", () => {
  it("puts gridlines on round numbers", () => {
    const { min, max, ticks } = valueScale([0, 137, 480, 920]);
    expect(min).toBe(0);
    // Rounded outward, so the peak sits at or under the top gridline
    // rather than poking out of its own chart.
    expect(max).toBeGreaterThanOrEqual(920);
    for (const t of ticks) expect(Number.isInteger(t / 250) || Number.isInteger(t)).toBe(true);
    expect(ticks[0]).toBe(min);
    expect(ticks.at(-1)).toBe(max);
  });

  it("keeps a zero baseline when the series spans most of its height", () => {
    expect(valueScale([0, 500, 1000]).min).toBe(0);
    expect(valueScale([120, 800, 1000]).min).toBe(0);
  });

  it("crops the baseline when the series sits far above zero", () => {
    // 990..1000 on a zero-based axis is a flat line: true and useless.
    const { min, max } = valueScale([990, 995, 1000]);
    expect(min).toBeGreaterThan(900);
    expect(max).toBeGreaterThanOrEqual(1000);
  });

  it("gives a flat series a band rather than a zero-height axis", () => {
    // A zero-height axis divides by zero when scaling a point into it.
    const { min, max } = valueScale([7, 7, 7]);
    expect(max).toBeGreaterThan(min);
    expect(min).toBeLessThanOrEqual(7);
    expect(max).toBeGreaterThanOrEqual(7);
  });

  it("survives an all-zero series, which is what an empty market is", () => {
    const { min, max } = valueScale([0, 0, 0]);
    expect(max).toBeGreaterThan(min);
  });

  it("survives being given nothing", () => {
    const { min, max, ticks } = valueScale([]);
    expect(max).toBeGreaterThan(min);
    expect(ticks.length).toBeGreaterThan(0);
  });
});

describe("timeTicks", () => {
  const start = new Date("2026-08-11T09:13:27Z").getTime();

  it("lands on round moments rather than even divisions of the span", () => {
    const ticks = timeTicks(start, start + 6 * HOUR, 5);
    expect(ticks.length).toBeGreaterThan(2);
    // Every tick is a whole number of hours in local time -- an axis
    // reading 10:07, 11:14, 12:21 is what this exists to avoid.
    for (const t of ticks) {
      expect(new Date(t.ms).getMinutes() % 30).toBe(0);
    }
  });

  it("aligns daily ticks to local midnight", () => {
    const ticks = timeTicks(start, start + 10 * DAY, 5);
    expect(ticks.length).toBeGreaterThan(1);
    for (const t of ticks) {
      const d = new Date(t.ms);
      expect([d.getHours(), d.getMinutes()]).toEqual([0, 0]);
    }
  });

  it("stays inside the window", () => {
    const end = start + 6 * HOUR;
    for (const t of timeTicks(start, end)) {
      expect(t.ms).toBeGreaterThanOrEqual(start);
      expect(t.ms).toBeLessThanOrEqual(end);
    }
  });

  it("returns nothing for a window with no width, rather than spinning", () => {
    expect(timeTicks(start, start)).toEqual([]);
    expect(timeTicks(start, start - HOUR)).toEqual([]);
  });
});

describe("timeLabel", () => {
  const at = new Date("2026-08-11T15:30:00Z").getTime();

  it("picks its format from the span, not the instant", () => {
    // One axis, one kind of label: a date here and a clock time there
    // makes an axis that has to be read twice.
    expect(timeLabel(at, 6 * HOUR)).toMatch(/\d{1,2}:\d{2}/);
    expect(timeLabel(at, 30 * DAY)).toMatch(/[A-Z][a-z]{2} \d{1,2}/);
    expect(timeLabel(at, 2 * 365 * DAY)).toMatch(/[A-Z][a-z]{2} \d{2}/);
    expect(timeLabel(at, 10 * 365 * DAY)).toBe("2026");
  });
});

describe("bucketTime", () => {
  it("reports a bucket at its right edge", () => {
    // A cumulative total at bucket i is the total *by the end of* that
    // bucket; plotting it at the start reports every point one bucket
    // early -- an hour's error on every point of a 24-bucket day.
    const start = 0;
    const end = 24 * HOUR;
    expect(bucketTime(0, start, end, 24)).toBe(HOUR);
    expect(bucketTime(23, start, end, 24)).toBe(24 * HOUR);
  });

  it("does not divide by a bucket count of zero", () => {
    expect(bucketTime(0, 500, 900, 0)).toBe(500);
  });
});

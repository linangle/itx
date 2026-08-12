import { describe, expect, it } from "vitest";
import {
  bucketsForWidth,
  defaultRange,
  parseRange,
  rangesForAge,
  windowForRange,
} from "./chartRanges";

const HOUR = 3_600_000;
const DAY = 24 * HOUR;
const keys = (ageMs: number | null) => rangesForAge(ageMs).map((r) => r.key);

describe("rangesForAge", () => {
  it("offers only `all` on a board minutes old", () => {
    // The whole of a ten-minute board is the only honest thing to show.
    expect(keys(10 * 60_000)).toEqual(["all"]);
  });

  it("grows the ladder as the board ages", () => {
    expect(keys(2 * HOUR)).toEqual(["1h", "all"]);
    expect(keys(2 * DAY)).toEqual(["1h", "6h", "1d", "all"]);
    expect(keys(40 * DAY)).toEqual(["1h", "6h", "1d", "5d", "1m", "all"]);
    // The case the owner named: six months of running is when a 6m tab
    // shows up, and not before.
    expect(keys(100 * DAY)).not.toContain("6m");
    expect(keys(150 * DAY)).toContain("6m");
  });

  it("offers a range once most of it has elapsed, not all of it", () => {
    // 0.6 of a day. A tab that appeared at exactly 24h would show a line
    // pinned to the right edge on its first render.
    expect(keys(15 * HOUR)).toContain("1d");
    expect(keys(10 * HOUR)).not.toContain("1d");
  });

  it("always ends with `all`, whatever the age", () => {
    for (const age of [null, 0, -1, 60_000, 10 * 365 * DAY]) {
      expect(keys(age).at(-1)).toBe("all");
    }
  });

  it("treats an unknown age as a board with no history", () => {
    // `first_task_at` is null on an empty board, which is not the same
    // claim as an age of zero, but leads to the same single tab.
    expect(keys(null)).toEqual(["all"]);
  });
});

describe("defaultRange", () => {
  it("opens on the widest fixed range the board can fill", () => {
    expect(defaultRange(2 * DAY).key).toBe("1d");
    expect(defaultRange(40 * DAY).key).toBe("1m");
  });

  it("falls back to `all` before any fixed range is available", () => {
    expect(defaultRange(10 * 60_000).key).toBe("all");
    expect(defaultRange(null).key).toBe("all");
  });
});

describe("parseRange", () => {
  it("takes a range the board can show", () => {
    expect(parseRange("1d", 2 * DAY).key).toBe("1d");
  });

  it("falls back rather than erroring on a range this board has no tab for", () => {
    // A link kept from a board that is older than this one, or typed by
    // hand. It shows something rather than nothing.
    expect(parseRange("5y", 2 * DAY).key).toBe("1d");
    expect(parseRange("nonsense", 2 * DAY).key).toBe("1d");
    expect(parseRange(null, 2 * DAY).key).toBe("1d");
  });
});

describe("windowForRange", () => {
  it("passes a fixed range straight through", () => {
    expect(windowForRange({ key: "1d", label: "1d", windowMs: DAY }, 5 * DAY)).toBe(DAY);
  });

  it("sizes `all` from the age, with air on the left edge", () => {
    const all = { key: "all", label: "all", windowMs: null };
    const out = windowForRange(all, 10 * DAY)!;
    expect(out).toBeGreaterThan(10 * DAY);
    // A couple of percent, not a couple of days -- the oldest point
    // needs to clear the axis, not float in the middle of the chart.
    expect(out).toBeLessThan(10.5 * DAY);
  });

  it("asks for the hub's own default when there is no age to size from", () => {
    const all = { key: "all", label: "all", windowMs: null };
    expect(windowForRange(all, null)).toBeUndefined();
  });
});

describe("bucketsForWidth", () => {
  it("scales with the chart but stays inside what the hub will serve", () => {
    expect(bucketsForWidth(600)).toBe(100);
    // The hub caps at 240; asking for more only costs a longer pass and
    // a bigger parse for sub-pixel buckets.
    expect(bucketsForWidth(4000)).toBe(240);
    // And below about 24 a line stops reading as a line.
    expect(bucketsForWidth(60)).toBe(24);
  });
});

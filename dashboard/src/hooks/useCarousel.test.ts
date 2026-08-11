import { describe, expect, it } from "vitest";
import { snapTarget } from "./useCarousel";

/** A market column at the desktop breakpoint: a 440px panel and the
 * 20px gap after it. */
const STEP = 460;
/** Twelve markets in a row three panels wide. */
const MAX = STEP * 12 - 1380;

describe("snapTarget", () => {
  it("moves one item from a boundary", () => {
    expect(snapTarget(0, STEP, 1, MAX)).toBe(STEP);
    expect(snapTarget(STEP * 3, STEP, 1, MAX)).toBe(STEP * 4);
    expect(snapTarget(STEP * 3, STEP, -1, MAX)).toBe(STEP * 2);
  });

  it("finishes the move the row is in the middle of, rather than skipping past it", () => {
    // Two thirds of the way from item 2 to item 3: forward lands on 3,
    // back on 2. Rounding to the nearest boundary first would have sent
    // "next" to 4 -- the panel you are looking at, skipped.
    const between = STEP * 2 + STEP * 0.66;
    expect(snapTarget(between, STEP, 1, MAX)).toBe(STEP * 3);
    expect(snapTarget(between, STEP, -1, MAX)).toBe(STEP * 2);
  });

  it("treats a fractional scroll position as being on the boundary", () => {
    // Where a trackpad leaves the row after a smooth scroll: a hair off
    // a boundary must still cost a whole step, in either direction.
    expect(snapTarget(STEP * 2 + 0.4, STEP, 1, MAX)).toBe(STEP * 3);
    expect(snapTarget(STEP * 2 - 0.4, STEP, 1, MAX)).toBe(STEP * 3);
    expect(snapTarget(STEP * 2 + 0.4, STEP, -1, MAX)).toBe(STEP);
    expect(snapTarget(STEP * 2 - 0.4, STEP, -1, MAX)).toBe(STEP);
  });

  it("stops at both ends", () => {
    expect(snapTarget(0, STEP, -1, MAX)).toBe(0);
    expect(snapTarget(MAX, STEP, 1, MAX)).toBe(MAX);
    // The last step lands on the end rather than short of it, so the
    // final market is not left half past the edge with nothing beyond.
    expect(snapTarget(MAX - 10, STEP, 1, MAX)).toBe(MAX);
  });

  it("answers zero before there is anything to measure", () => {
    // The first render, or a board whose markets have not arrived: no
    // items means no stride, and no scroll position worth asking for.
    expect(snapTarget(0, 0, 1, 0)).toBe(0);
  });
});

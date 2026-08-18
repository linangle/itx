import { describe, expect, it } from "vitest";
import { ALL_DESKS, desksOf, onDesk } from "./desks";
import { SAMPLES } from "./predictionSample";
import { STORIES } from "./newsroomSample";

const pool = [
  { key: "a", category: "energy" },
  { key: "b", category: "weather" },
  { key: "c", category: "energy" },
  { key: "d", category: "compute" },
];

describe("desks", () => {
  it("counts a pool by desk, busiest first", () => {
    expect(desksOf(pool)).toEqual([
      { name: "energy", count: 2 },
      { name: "compute", count: 1 },
      { name: "weather", count: 1 },
    ]);
  });

  it("breaks a tie alphabetically rather than by the pool's own order", () => {
    // Otherwise the filter row reshuffles whenever a story is added
    // above another with the same count -- a control that moves under
    // the pointer for reasons the reader cannot see.
    const reversed = [...pool].reverse();
    expect(desksOf(reversed).map((d) => d.name)).toEqual(
      desksOf(pool).map((d) => d.name),
    );
  });

  it("cuts a pool to one desk, and lets `all` through untouched", () => {
    expect(onDesk(pool, "energy").map((i) => i.key)).toEqual(["a", "c"]);
    expect(onDesk(pool, ALL_DESKS)).toBe(pool);
    expect(onDesk(pool, "haruspicy")).toEqual([]);
  });

  it("keeps the order it was given", () => {
    // Ordering is the caller's decision -- both pages sort before they
    // filter, and a filter that re-sorted would quietly overrule them.
    const byKey = [...pool].sort((a, b) => b.key.localeCompare(a.key));
    expect(onDesk(byKey, "energy").map((i) => i.key)).toEqual(["c", "a"]);
  });

  it("counts every item in the pool exactly once", () => {
    for (const items of [SAMPLES, STORIES]) {
      const total = desksOf(items).reduce((sum, d) => sum + d.count, 0);
      expect(total).toBe(items.length);
    }
  });

  it("is the vocabulary both sample pools share", () => {
    // The newsroom is what the agents read and the market is what they
    // priced off it. A story on a desk with no market is fine -- the
    // reverse is not: a market nobody is reading about means the two
    // pools have drifted apart, which is the whole point of them.
    const desks = new Set(desksOf(STORIES).map((d) => d.name));
    for (const market of SAMPLES) {
      expect(desks).toContain(market.category);
    }
  });
});

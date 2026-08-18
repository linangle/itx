import { describe, expect, it } from "vitest";
import { STORIES, newsTotals, orderStories, scrapedAtIso, topStories } from "./newsroomSample";

describe("the sample newsroom", () => {
  it("holds more stories than the board shows", () => {
    // "The top five by views" is a selection; a pool of exactly five
    // would make the sort decoration.
    expect(STORIES.length).toBeGreaterThan(5);
    expect(new Set(STORIES.map((s) => s.key)).size).toBe(STORIES.length);
  });

  it("keeps the pool out of view order, so skipping the sort would show", () => {
    const views = STORIES.map((s) => s.agentViews);
    expect(views).not.toEqual([...views].sort((a, b) => b - a));
  });

  it("picks the most-read five, most-read first", () => {
    const top = topStories();
    expect(top).toHaveLength(5);
    const views = top.map((s) => s.agentViews);
    expect(views).toEqual([...views].sort((a, b) => b - a));
    // The five shown really are the five biggest in the pool.
    const floor = Math.min(...views);
    for (const story of STORIES) {
      if (!top.some((t) => t.key === story.key)) {
        expect(story.agentViews).toBeLessThanOrEqual(floor);
      }
    }
  });

  it("leaves the pool alone when it selects", () => {
    // STORIES is module state; a sort in place would reorder it for
    // every other reader.
    const before = STORIES.map((s) => s.key);
    topStories();
    expect(STORIES.map((s) => s.key)).toEqual(before);
  });

  it("stamps a story against the clock, not against an authored date", () => {
    const now = Date.UTC(2026, 7, 12, 18);
    expect(scrapedAtIso(60_000, now)).toBe(new Date(now - 60_000).toISOString());
  });
});

describe("the feed the full page shows", () => {
  it("orders by reads, most-read first", () => {
    const views = orderStories(STORIES, "views").map((s) => s.agentViews);
    expect(views).toEqual([...views].sort((a, b) => b - a));
  });

  it("orders by filing, newest first -- off the offset, not off a clock", () => {
    // `ageMs` is how long ago a story was scraped, so smaller is newer
    // and the ordering never needs to know what time it is.
    const ages = orderStories(STORIES, "latest").map((s) => s.ageMs);
    expect(ages).toEqual([...ages].sort((a, b) => a - b));
  });

  it("leaves the pool alone when it orders", () => {
    const before = STORIES.map((s) => s.key);
    orderStories(STORIES, "latest");
    expect(STORIES.map((s) => s.key)).toEqual(before);
  });

  it("agrees with the board about the top five", () => {
    // The board takes five off `topStories` and the page takes the whole
    // feed off `orderStories`; a reader who follows the arrow should
    // find the same five at the top of it.
    expect(orderStories(STORIES, "views").slice(0, 5)).toEqual(topStories());
  });

  it("totals what a page is showing rather than what the pool holds", () => {
    const two = STORIES.slice(0, 2);
    expect(newsTotals(two)).toEqual({
      count: 2,
      reads: two[0].agentViews + two[1].agentViews,
      sources: two[0].sources + two[1].sources,
    });
  });

  it("prices some readings into markets and leaves the rest alone", () => {
    // Both cases have to exist in the pool: the page draws a "priced
    // into" link only when a story has a market, and a pool where every
    // story had one would never render the other half of that rule.
    const priced = STORIES.filter((s) => s.marketKey);
    expect(priced.length).toBeGreaterThan(0);
    expect(priced.length).toBeLessThan(STORIES.length);
  });
});

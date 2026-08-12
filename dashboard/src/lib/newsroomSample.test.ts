import { describe, expect, it } from "vitest";
import { STORIES, scrapedAtIso, topStories } from "./newsroomSample";

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

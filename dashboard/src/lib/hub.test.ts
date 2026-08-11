import { afterEach, describe, expect, it, vi } from "vitest";
import { listAllTasks } from "./hub";
import type { TaskDto } from "./hub";

/** A board of `total` numbered tasks behind a fetch-shaped mock, so the
 * tests can check what the walk asked for and what it assembled. Each
 * response is delayed a few milliseconds: instant resolution would let
 * the first worker drain every offset before a second one started, and
 * the concurrency ceiling would never actually be exercised. */
function fakeBoard(total: number, delayMs = 5) {
  const tasks = Array.from(
    { length: total },
    (_, i) => ({ id: `task-${i}` }) as unknown as TaskDto,
  );

  let inFlight = 0;
  let peak = 0;

  const fetchMock = vi.fn(async (url: string | URL | Request) => {
    const parsed = new URL(String(url));
    const offset = Number(parsed.searchParams.get("offset") ?? 0);
    const limit = Number(parsed.searchParams.get("limit") ?? 50);

    inFlight++;
    peak = Math.max(peak, inFlight);
    await new Promise((resolve) => setTimeout(resolve, delayMs));
    inFlight--;

    return {
      ok: true,
      headers: { get: (h: string) => (h.toLowerCase() === "x-total-count" ? String(total) : null) },
      json: async () => tasks.slice(offset, offset + limit),
    } as unknown as Response;
  });

  vi.stubGlobal("fetch", fetchMock);
  return { fetchMock, peakInFlight: () => peak };
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("listAllTasks", () => {
  it("assembles every page in board order, whatever order responses land in", async () => {
    fakeBoard(1050);
    const result = await listAllTasks({ status: "all" });

    expect(result.total).toBe(1050);
    expect(result.complete).toBe(true);
    expect(result.items).toHaveLength(1050);
    // Order is the contract: series bucketing and "latest" both assume
    // the hub's oldest-first listing survived the parallel fetch.
    expect(result.items.map((t) => t.id)).toEqual(
      Array.from({ length: 1050 }, (_, i) => `task-${i}`),
    );
  });

  it("fetches the remaining pages concurrently, but never more than six at once", async () => {
    const board = fakeBoard(2600);
    await listAllTasks({ status: "all" });

    // 13 pages: the probe alone, then 12 more in flights of six.
    expect(board.peakInFlight()).toBe(6);
  });

  it("stops at maxItems and reports the walk as incomplete", async () => {
    const board = fakeBoard(2000);
    const result = await listAllTasks({ status: "all" }, 600);

    expect(result.items).toHaveLength(600);
    expect(result.total).toBe(2000);
    expect(result.complete).toBe(false);
    expect(board.fetchMock).toHaveBeenCalledTimes(3);
  });

  it("makes exactly one request for a board that fits in one page", async () => {
    const board = fakeBoard(80);
    const result = await listAllTasks({ status: "all" });

    expect(result.items).toHaveLength(80);
    expect(result.complete).toBe(true);
    expect(board.fetchMock).toHaveBeenCalledTimes(1);
  });
});

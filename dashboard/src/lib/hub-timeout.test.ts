import { afterEach, expect, it, vi } from "vitest";
import { getBoardSummary } from "./hub";

afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); });

it("abandons a hung read so polling can retry and report stale data", async () => {
  vi.useFakeTimers();
  vi.stubGlobal("fetch", vi.fn((_url, options: RequestInit) => new Promise((_resolve, reject) => {
    options.signal!.addEventListener("abort", () => reject(new DOMException("Aborted", "AbortError")));
  })));
  const rejected = expect(getBoardSummary()).rejects.toMatchObject({ name: "AbortError" });
  await vi.advanceTimersByTimeAsync(15_000);
  await rejected;
  expect(vi.getTimerCount()).toBe(0);
});

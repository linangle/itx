import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useAsync } from "./useAsync";

/** A fetch whose resolution the test controls, so a request can be held
 * "in flight" across as many timer ticks as the test needs. */
function controlledFn<T>() {
  const resolvers: Array<(value: T) => void> = [];
  const fn = vi.fn(
    () =>
      new Promise<T>((resolve) => {
        resolvers.push(resolve);
      }),
  );
  return { fn, resolve: (value: T) => resolvers.shift()?.(value) };
}

/** Overrides `document.hidden` (jsdom pins it to `false`) for one test.
 * `configurable: true` is what lets `restoreHidden` put the original
 * prototype getter back. */
function setHidden(hidden: boolean) {
  Object.defineProperty(document, "hidden", { configurable: true, get: () => hidden });
}

function restoreHidden() {
  Reflect.deleteProperty(document, "hidden");
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
  restoreHidden();
});

describe("useAsync polling", () => {
  it("skips ticks while a request is still in flight, instead of stacking a second one", async () => {
    const { fn, resolve } = controlledFn<number>();
    renderHook(() => useAsync(fn, [], 5000));
    expect(fn).toHaveBeenCalledTimes(1);

    // Two full intervals pass with the mount request unanswered: a poll
    // that queued would be at three calls by now.
    await act(() => vi.advanceTimersByTimeAsync(10_000));
    expect(fn).toHaveBeenCalledTimes(1);

    // The moment it answers, the next tick fetches again.
    await act(async () => {
      resolve(1);
    });
    await act(() => vi.advanceTimersByTimeAsync(5000));
    expect(fn).toHaveBeenCalledTimes(2);
  });

  it("does not poll a hidden tab, and refreshes immediately on return", async () => {
    const { fn, resolve } = controlledFn<number>();
    setHidden(true);
    renderHook(() => useAsync(fn, [], 5000));
    // The mount fetch is deliberately ungated -- the page was just
    // navigated to, hidden or not it needs its first data.
    expect(fn).toHaveBeenCalledTimes(1);
    await act(async () => {
      resolve(1);
    });

    await act(() => vi.advanceTimersByTimeAsync(20_000));
    expect(fn).toHaveBeenCalledTimes(1);

    setHidden(false);
    await act(async () => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    expect(fn).toHaveBeenCalledTimes(2);
  });

  it("keeps polling on the interval while visible", async () => {
    const { fn, resolve } = controlledFn<number>();
    renderHook(() => useAsync(fn, [], 5000));
    await act(async () => {
      resolve(1);
    });

    await act(() => vi.advanceTimersByTimeAsync(5000));
    expect(fn).toHaveBeenCalledTimes(2);
    await act(async () => {
      resolve(2);
    });
    await act(() => vi.advanceTimersByTimeAsync(5000));
    expect(fn).toHaveBeenCalledTimes(3);
  });

  it("stops entirely on unmount", async () => {
    const { fn, resolve } = controlledFn<number>();
    const { unmount } = renderHook(() => useAsync(fn, [], 5000));
    await act(async () => {
      resolve(1);
    });

    unmount();
    await act(() => vi.advanceTimersByTimeAsync(20_000));
    expect(fn).toHaveBeenCalledTimes(1);
  });
});

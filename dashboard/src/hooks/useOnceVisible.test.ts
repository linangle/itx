import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useOnceVisible } from "./useOnceVisible";

type Callback = (entries: Array<{ isIntersecting: boolean }>) => void;

/** A stand-in observer that records what it was asked to watch and lets
 * a test say when it came into view. */
function stubObserver() {
  const instances: Array<{ cb: Callback; observed: Element[]; disconnected: boolean }> = [];
  class Stub {
    cb: Callback;
    observed: Element[] = [];
    disconnected = false;
    constructor(cb: Callback) {
      this.cb = cb;
      instances.push(this);
    }
    observe(el: Element) {
      this.observed.push(el);
    }
    disconnect() {
      this.disconnected = true;
    }
    unobserve() {}
    takeRecords() {
      return [];
    }
  }
  vi.stubGlobal("IntersectionObserver", Stub);
  return instances;
}

describe("useOnceVisible", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("is false until the element comes into view, then true for good", () => {
    const instances = stubObserver();
    const el = document.createElement("div");
    const { result } = renderHook(() => useOnceVisible<HTMLDivElement>());
    act(() => result.current[0](el));
    expect(result.current[1]).toBe(false);
    expect(instances[0].observed).toEqual([el]);

    act(() => instances[0].cb([{ isIntersecting: false }]));
    expect(result.current[1]).toBe(false);

    act(() => instances[0].cb([{ isIntersecting: true }]));
    expect(result.current[1]).toBe(true);
    // Seen once is seen: the observer is let go, and nothing watches for
    // the element leaving again.
    expect(instances[0].disconnected).toBe(true);
  });

  it("watches a replacement element afresh", () => {
    const instances = stubObserver();
    const { result } = renderHook(() => useOnceVisible<HTMLDivElement>());
    act(() => result.current[0](document.createElement("div")));
    const again = document.createElement("div");
    act(() => result.current[0](again));
    expect(instances[0].disconnected).toBe(true);
    expect(instances[1].observed).toEqual([again]);
  });

  it("treats everything as visible where there is no observer to ask", () => {
    vi.stubGlobal("IntersectionObserver", undefined);
    const { result } = renderHook(() => useOnceVisible<HTMLDivElement>());
    act(() => result.current[0](document.createElement("div")));
    expect(result.current[1]).toBe(true);
  });
});

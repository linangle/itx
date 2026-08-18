import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { settle, useColumnWidth } from "./useColumnWidth";
import type { PointerEvent } from "react";

/** The board's rail: 232px by default, no narrower than 190, no wider
 * than 420. */
const MIN = 190;
const MAX = 420;
const INITIAL = 232;
const KEY = "test-col-w";

describe("settle", () => {
  it("holds an edge between its floor and its ceiling", () => {
    expect(settle(300, MIN, MAX)).toBe(300);
    expect(settle(180, MIN, MAX)).toBe(MIN);
    expect(settle(900, MIN, MAX)).toBe(MAX);
  });

  it("shuts the column only once the edge is well past the floor", () => {
    // The slack is the whole point: without it the column would snap
    // shut on the way *through* its minimum, so the last stretch of
    // every resize would read as a cliff.
    expect(settle(MIN - 40, MIN, MAX)).toBe(MIN);
    expect(settle(MIN - 60, MIN, MAX)).toBe(0);
    expect(settle(-200, MIN, MAX)).toBe(0);
  });
});

/** A host to publish the width on, standing in for `.itx-board-inner`. */
function host() {
  const el = document.createElement("div");
  document.body.appendChild(el);
  return { current: el };
}

function options(el: ReturnType<typeof host>) {
  return {
    host: el,
    property: "--col-rail",
    attribute: "data-rail-shut",
    storageKey: KEY,
    side: "right" as const,
    min: MIN,
    max: MAX,
    initial: INITIAL,
    label: "the rail",
    controls: "rail",
  };
}

/** A pointer event as the grip's handlers read it. The grip is also
 * where the capture goes, so the target has to answer those calls. */
function pointer(clientX: number, target: HTMLElement): PointerEvent<HTMLElement> {
  return {
    button: 0,
    pointerId: 1,
    clientX,
    currentTarget: target,
    preventDefault: () => {},
  } as unknown as PointerEvent<HTMLElement>;
}

describe("useColumnWidth", () => {
  let el: ReturnType<typeof host>;
  let grip: HTMLElement;

  beforeEach(() => {
    localStorage.clear();
    el = host();
    grip = document.createElement("div");
    // jsdom has no pointer capture; the hook only ever asks the grip.
    grip.setPointerCapture = () => {};
    grip.releasePointerCapture = () => {};
    grip.hasPointerCapture = () => true;
    document.body.appendChild(grip);
  });

  afterEach(() => {
    document.body.innerHTML = "";
  });

  it("publishes the width on the host rather than rendering it", () => {
    renderHook(() => useColumnWidth(options(el)));
    expect(el.current.style.getPropertyValue("--col-rail")).toBe("232px");
    expect(el.current.hasAttribute("data-rail-shut")).toBe(false);
  });

  it("widens a right-hand column as the pointer travels left", () => {
    const { result } = renderHook(() => useColumnWidth(options(el)));

    act(() => {
      result.current.grip.onPointerDown(pointer(1000, grip));
      result.current.grip.onPointerMove(pointer(940, grip));
    });
    // Mid-drag the width is on the host and *not* in state: a render
    // per pointermove is what this arrangement exists to avoid.
    expect(el.current.style.getPropertyValue("--col-rail")).toBe("292px");
    expect(result.current.width).toBe(INITIAL);

    act(() => {
      result.current.grip.onPointerUp(pointer(940, grip));
    });
    expect(result.current.width).toBe(292);
    expect(localStorage.getItem(KEY)).toBe("292");
  });

  it("shuts the column when the edge is dragged well in, and remembers where it was", () => {
    const { result } = renderHook(() => useColumnWidth(options(el)));

    act(() => {
      result.current.grip.onPointerDown(pointer(1000, grip));
      result.current.grip.onPointerMove(pointer(1300, grip));
      result.current.grip.onPointerUp(pointer(1300, grip));
    });
    expect(result.current.shut).toBe(true);
    expect(el.current.hasAttribute("data-rail-shut")).toBe(true);
    expect(el.current.style.getPropertyValue("--col-rail")).toBe("0px");

    // The grip is all that is left of a shut column, so a click on it --
    // a press that went nowhere -- is the way back.
    act(() => {
      result.current.grip.onPointerDown(pointer(1300, grip));
      result.current.grip.onPointerUp(pointer(1300, grip));
    });
    expect(result.current.width).toBe(INITIAL);
    expect(result.current.shut).toBe(false);
  });

  it("comes back at the width it was left at", () => {
    localStorage.setItem(KEY, "310");
    const { result } = renderHook(() => useColumnWidth(options(el)));
    expect(result.current.width).toBe(310);
    expect(el.current.style.getPropertyValue("--col-rail")).toBe("310px");
  });

  it("ignores a remembered width that is no longer allowed", () => {
    // The floors and ceilings are the columns' contents, and those
    // change; a width stored under the old ones must not survive them.
    localStorage.setItem(KEY, "9000");
    const { result } = renderHook(() => useColumnWidth(options(el)));
    expect(result.current.width).toBe(MAX);
  });
});

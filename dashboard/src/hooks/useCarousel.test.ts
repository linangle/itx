import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { snapTarget, useCarousel } from "./useCarousel";

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

/** Builds a row of `items` panels with real geometry, mounts the hook on
 * it, and reports how many times the caller re-rendered.
 *
 * jsdom lays nothing out -- every box measures zero -- so the widths the
 * hook reads back are stubbed onto the element. `getBoundingClientRect`
 * is what `stride` uses; `scrollLeft` is writable on a jsdom element, but
 * setting it fires no scroll event (nor does a real browser fire one for
 * every programmatic scroll), so the test dispatches its own. */
function mountRow(items: number, { panel = 440, gap = 20, viewport = 1380 } = {}) {
  const stride = panel + gap;
  const el = document.createElement("div");
  for (let i = 0; i < items; i++) el.appendChild(document.createElement("div"));

  // The fade ceiling lives in the stylesheet (`--leading-fade-max`), and
  // jsdom loads no CSS -- without this the hook reads a ceiling of zero
  // and every fade clamps to 0px. Inline so `getComputedStyle` sees it.
  el.style.setProperty("--leading-fade-max", "96px");
  Object.defineProperty(el, "clientWidth", { value: viewport, configurable: true });
  Object.defineProperty(el, "scrollWidth", { value: stride * items - gap, configurable: true });
  // `right` as well as `left`, because a real DOMRect has both and the
  // hook reads it to work out which panels are actually on screen. A
  // stub missing it made every overlap NaN, which compares false against
  // everything and quietly marked the whole row visible.
  el.getBoundingClientRect = () =>
    ({ left: 0, right: viewport, width: viewport }) as DOMRect;
  for (let i = 0; i < items; i++) {
    (el.children[i] as HTMLElement).getBoundingClientRect = () => {
      const left = i * stride - el.scrollLeft;
      return { left, right: left + panel, width: panel } as DOMRect;
    };
  }
  document.body.appendChild(el);

  let renders = 0;
  const { result } = renderHook(() => {
    renders++;
    const [ref, carousel] = useCarousel<HTMLDivElement>(items);
    // The row is a plain element rather than something React rendered, so
    // the ref is attached by hand -- during render, before the hook's
    // effects run and go looking for it.
    ref.current = el as HTMLDivElement;
    return carousel;
  });

  const scrollTo = (left: number) => {
    act(() => {
      el.scrollLeft = left;
      el.dispatchEvent(new Event("scroll"));
    });
  };

  return { el, stride, result, scrollTo, renders: () => renders };
}

describe("useCarousel scroll handling", () => {
  afterEach(() => {
    document.body.innerHTML = "";
  });

  it("tracks the front market and both ends as the row scrolls", () => {
    const row = mountRow(12);
    expect(row.result.current.index).toBe(0);
    expect(row.result.current.atStart).toBe(true);
    expect(row.result.current.atEnd).toBe(false);

    row.scrollTo(row.stride * 3);
    expect(row.result.current.index).toBe(3);
    expect(row.result.current.atStart).toBe(false);
    expect(row.result.current.atEnd).toBe(false);

    row.scrollTo(row.el.scrollWidth - row.el.clientWidth);
    expect(row.result.current.atEnd).toBe(true);
  });

  it("marks every panel on screen, so the last one is reachable at the far end", () => {
    // Three panels fit; twelve exist. At the far end the row cannot
    // scroll any further, so panel 11 never reaches the leading edge --
    // which is why an `index`-only answer left the final sector
    // permanently unlit and made clicking it look broken.
    const row = mountRow(12);
    expect(row.result.current.firstVisible).toBe(0);
    expect(row.result.current.lastVisible).toBe(2);

    row.scrollTo(row.el.scrollWidth - row.el.clientWidth);
    expect(row.result.current.atEnd).toBe(true);
    expect(row.result.current.lastVisible).toBe(11);
    // And the one before it, since both are genuinely on screen.
    expect(row.result.current.firstVisible).toBe(9);
  });

  it("does not count a panel that is only peeking", () => {
    // The row shows a sliver of the next panel past its right edge and
    // fades it out. A panel mostly cut off is not one you are looking
    // at, and marking it would light two thirds of the rail at once.
    const row = mountRow(12);
    row.scrollTo(row.stride * 0.5);
    expect(row.result.current.lastVisible).toBe(2);
  });

  it("does not re-render for scroll frames that leave the front market and the ends alone", () => {
    const row = mountRow(12);
    row.scrollTo(row.stride * 3);
    const settled = row.renders();

    // Six frames of a drag well inside one panel: the row moves, the
    // fade moves with it, but the index, the visible range and both ends
    // are unchanged. This is the common case while a finger is down, and
    // it used to re-render the whole board -- twelve market panels and
    // every sparkline in them -- for each frame.
    //
    // The claim is that the cost does not grow with the drag, which is
    // why this counts six frames rather than asserting an exact total.
    // React renders one more time after a real state change before it
    // trusts the bail-out, so the first frame following a genuine move
    // can cost a render whatever this hook returns; every frame after it
    // costs nothing. Pinning an exact number would be pinning that
    // implementation detail instead of the guarantee.
    for (const offset of [4, 9, 15, 21, 26, 33]) {
      row.scrollTo(row.stride * 3 + offset);
    }
    expect(row.renders()).toBeLessThanOrEqual(settled + 1);

    // And the frames after the first really are free.
    const drifting = row.renders();
    row.scrollTo(row.stride * 3 + 38);
    row.scrollTo(row.stride * 3 + 44);
    expect(row.renders()).toBe(drifting);

    // Crossing into the next panel is a real change and must still land.
    row.scrollTo(row.stride * 4);
    expect(row.renders()).toBeGreaterThan(settled);
    expect(row.result.current.index).toBe(4);
  });

  it("still narrows the near edge's fade on the frames it skips", () => {
    const row = mountRow(12);
    row.scrollTo(row.stride * 3 + 6);
    // Bailing out of the state update must not bail out of the fade --
    // that is written straight to the element, not through React, which
    // is what lets it keep up with a scroll it does not re-render for.
    expect(row.el.style.getPropertyValue("--leading-fade")).toBe("6px");
    row.scrollTo(row.stride * 3 + 12);
    expect(row.el.style.getPropertyValue("--leading-fade")).toBe("12px");
  });
});

import { useCallback, useEffect, useRef, useState } from "react";

/** Where one step of the pager lands, given where the row sits now.
 *
 * Both arguments and the answer are scroll offsets in pixels; `step` is
 * the distance from one item's leading edge to the next.
 *
 * Rounds *towards* the direction asked for rather than to the nearest
 * boundary: from halfway through a panel, "next" finishes the move the
 * row is already in the middle of instead of skipping the panel you are
 * looking at.
 *
 * The one-pixel slack is for fractional scroll positions -- a trackpad
 * leaves the row at 459.6 rather than 460 often enough that without it,
 * "next" from a boundary would sometimes cost nothing.
 */
export function snapTarget(
  scrollLeft: number,
  step: number,
  direction: 1 | -1,
  maxScroll: number,
): number {
  if (step <= 0) return 0;
  const boundary =
    direction > 0
      ? Math.floor((scrollLeft + 1) / step) + 1
      : Math.ceil((scrollLeft - 1) / step) - 1;
  return Math.max(0, Math.min(maxScroll, boundary * step));
}

/** What the board needs to know about a horizontally scrolling row.
 *
 * The row is a real scroll container -- `overflow-x` in the stylesheet,
 * not a transform driven from JS -- so the browser already handles the
 * finger, the momentum and the rubber-banding. What is left is the two
 * things it cannot know: which item counts as current, and where a
 * deliberate step should land.
 */
export interface Carousel {
  /** Index of the item nearest the row's leading edge. */
  index: number;
  /** The items actually on screen, as an inclusive index range.
   *
   * `index` alone could not answer "which sector am I looking at": the
   * row shows two to four panels at once and the *last* one can never
   * reach the leading edge, so it was never current and clicking its
   * entry in the rail looked like it did nothing. */
  firstVisible: number;
  lastVisible: number;
  /** Whether either end has been reached, for the arrows and the edge
   * fades -- a fade over a row that cannot move further is a lie. */
  atStart: boolean;
  atEnd: boolean;
  /** Snap one item along, in whichever direction. */
  step: (direction: 1 | -1) => void;
  /** Bring an item to the leading edge, for the rail's market list. */
  to: (index: number) => void;
}

export function useCarousel<T extends HTMLElement = HTMLDivElement>(
  /** How many items the row holds. Nothing else announces a change in how
   * far the row can scroll: items arriving from the hub do not resize the
   * container and fire no scroll event, so measured before the first
   * markets land the row looks like it is against both of its ends. */
  items: number,
) {
  const ref = useRef<T | null>(null);
  const [state, setState] = useState({
    index: 0,
    firstVisible: 0,
    lastVisible: 0,
    atStart: true,
    atEnd: false,
  });

  /** Where a smooth scroll that is still running is headed, so a second
   * arrow click steps on from there rather than from wherever the
   * animation happens to be. Without it, clicking through the markets
   * quickly loses most of the clicks. Cleared once the row arrives, or
   * the moment the reader takes over. */
  const pending = useRef<number | null>(null);

  // Distance from one item's leading edge to the next, read off the
  // layout rather than rebuilt from the basis and the gap -- those live in
  // the stylesheet and change at two breakpoints.
  const stride = (el: T) => {
    const items = el.children;
    if (items.length === 0) return 0;
    const first = items[0].getBoundingClientRect();
    if (items.length === 1) return first.width;
    return items[1].getBoundingClientRect().left - first.left;
  };

  /** The stylesheet's ceiling for the near edge's fade, cached because
   * this is read on every scroll frame and dropped whenever the row is
   * resized, since the breakpoints may have changed it. Kept in CSS so
   * the stylesheet stays the one place a length on this row is decided --
   * the same arrangement as `--row-h`. */
  const cap = useRef<number | null>(null);

  const read = useCallback(() => {
    const el = ref.current;
    if (!el) return;
    const step = stride(el);
    const max = el.scrollWidth - el.clientWidth;
    if (pending.current !== null && Math.abs(el.scrollLeft - pending.current) <= 1) {
      pending.current = null;
    }

    // How wide the near edge's fade should be right now: only ever as wide
    // as what is actually cut off. The arrows leave the row on a boundary,
    // where the panel starts flush against the edge and nothing is sliced,
    // and a fade there dimmed the first inch of the market just asked for.
    //
    // `into` is how far the row sits past the last boundary, so the fade
    // covers exactly the sliver of outgoing panel that is hidden and never
    // reaches across the panel arriving behind it.
    if (cap.current === null) {
      cap.current = parseFloat(getComputedStyle(el).getPropertyValue("--leading-fade-max")) || 0;
    }
    const into = step > 0 ? el.scrollLeft % step : 0;
    const fade = Math.max(0, Math.min(into, step - into, cap.current));
    el.style.setProperty("--leading-fade", `${Math.round(fade)}px`);

    // Where the row sits, for the indicator under it: how much of the row
    // is on screen, and how far through the rest we are. Written on the
    // *parent* because the indicator is a sibling of this scroll container
    // -- custom properties inherit down, not sideways.
    //
    // Set here rather than kept in state for the same reason
    // `--leading-fade` is: most frames of a free-scrolling row change
    // nothing React renders.
    //
    // A row that fits entirely on screen has nowhere to travel, so the
    // thumb fills the track and sits at 0 rather than dividing by zero.
    const track = el.parentElement;
    if (track) {
      const visible = el.scrollWidth > 0 ? el.clientWidth / el.scrollWidth : 1;
      track.style.setProperty("--rail-thumb", `${Math.min(1, visible)}`);
      track.style.setProperty("--rail-progress", `${max > 0 ? el.scrollLeft / max : 0}`);
    }

    const index = step > 0 ? Math.round(el.scrollLeft / step) : 0;

    // Which items are substantially on screen. "Substantially" is the
    // point: the row deliberately leaves a sliver of the next panel
    // showing past its right edge, and a panel that is mostly cut off is
    // not one you are looking at. Two thirds visible is the bar.
    const box = el.getBoundingClientRect();
    let firstVisible = index;
    let lastVisible = index;
    let seen = false;
    for (let i = 0; i < el.children.length; i++) {
      const r = el.children[i].getBoundingClientRect();
      if (r.width === 0) continue;
      const shown = Math.min(r.right, box.right) - Math.max(r.left, box.left);
      if (shown / r.width < 0.66) continue;
      if (!seen) {
        firstVisible = i;
        seen = true;
      }
      lastVisible = i;
    }
    // A pixel of slack at each end: a scroll that has arrived can sit
    // a fraction short of its own maximum, and the arrow at that end
    // must still be the one that is switched off.
    const atStart = el.scrollLeft <= 1;
    const atEnd = el.scrollLeft >= max - 1;
    // Handing back the previous object when nothing changed is what lets
    // React skip the re-render. This runs on every scroll event of a
    // free-scrolling row, and an unconditional fresh object meant every
    // one of those frames re-rendered the whole board.
    setState((previous) =>
      previous.index === index &&
      previous.firstVisible === firstVisible &&
      previous.lastVisible === lastVisible &&
      previous.atStart === atStart &&
      previous.atEnd === atEnd
        ? previous
        : { index, firstVisible, lastVisible, atStart, atEnd },
    );
  }, []);

  useEffect(read, [read, items]);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;

    read();
    // Passive: these only ever read. Saying so keeps the scroll off the
    // main thread's critical path, which is the difference between a
    // row that follows the finger and one that catches up with it.
    el.addEventListener("scroll", read, { passive: true });

    // A hand on the row outranks an arrow it is still travelling
    // towards: whatever the reader does now is where the row should be
    // stepping from.
    const yield_ = () => {
      pending.current = null;
    };
    el.addEventListener("wheel", yield_, { passive: true });
    el.addEventListener("pointerdown", yield_, { passive: true });
    el.addEventListener("touchstart", yield_, { passive: true });
    const release = () => {
      el.removeEventListener("scroll", read);
      el.removeEventListener("wheel", yield_);
      el.removeEventListener("pointerdown", yield_);
      el.removeEventListener("touchstart", yield_);
    };

    // The stride and the far end both change with the column's width,
    // and neither fires a scroll event when they do.
    if (typeof ResizeObserver === "undefined") return release;
    const observer = new ResizeObserver(() => {
      cap.current = null;
      read();
    });
    observer.observe(el);
    return () => {
      release();
      observer.disconnect();
    };
  }, [read]);

  // Smooth unless the reader has asked for less motion, in which case
  // the row simply arrives -- the snap is the point, the travel is not.
  const behavior = (): ScrollBehavior =>
    window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth";

  const step = useCallback((direction: 1 | -1) => {
    const el = ref.current;
    if (!el) return;
    const from = pending.current ?? el.scrollLeft;
    const left = snapTarget(from, stride(el), direction, el.scrollWidth - el.clientWidth);
    pending.current = left;
    el.scrollTo({ left, behavior: behavior() });
  }, []);

  const to = useCallback((index: number) => {
    const el = ref.current;
    if (!el) return;
    const item = el.children[index];
    if (!item) return;
    // Measured against the container's own box rather than offsetLeft,
    // which is relative to whichever ancestor happens to be positioned.
    const delta = item.getBoundingClientRect().left - el.getBoundingClientRect().left;
    const left = el.scrollLeft + delta;
    pending.current = left;
    el.scrollTo({ left, behavior: behavior() });
  }, []);

  return [ref, { ...state, step, to } satisfies Carousel] as const;
}

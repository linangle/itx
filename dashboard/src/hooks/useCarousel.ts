import { useCallback, useEffect, useRef, useState } from "react";

/** Where one step of the pager lands, given where the row sits now.
 *
 * Both arguments and the answer are scroll offsets in pixels; `step` is
 * the distance from one item's leading edge to the next.
 *
 * Rounds *towards* the direction asked for rather than to the nearest
 * boundary. From halfway through a panel, "next" finishes the move the
 * row is already in the middle of instead of skipping the panel you are
 * looking at -- which is what makes the arrows read as tidying the row
 * onto the grid, now that the scroll itself is free.
 *
 * The one-pixel slack is for scroll positions that are fractional: a
 * trackpad leaves the row at 459.6 rather than 460 often enough that
 * without it, "next" from a boundary would sometimes cost nothing.
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
 * not a transform driven from JS. That is the whole point of this hook
 * being as small as it is: the browser already knows how to follow a
 * finger, carry momentum, rubber-band at the ends and keep all of it at
 * 60fps on a compositor thread, on every engine including the ones a
 * hand-rolled gesture never quite matches. What is left is the two
 * things it cannot know: which item counts as current, and where a
 * deliberate step should land.
 */
export interface Carousel {
  /** Index of the item nearest the row's leading edge. */
  index: number;
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
  /** How many items the row holds. Nothing else announces a change in
   * how far the row can scroll: items arriving from the hub do not
   * resize the container -- its width comes from the column it sits in
   * -- and adding them fires no scroll event. Measured before the first
   * markets land, the row looks like it is against both of its ends,
   * which is how both arrows came to be disabled on a full board. */
  items: number,
) {
  const ref = useRef<T | null>(null);
  const [state, setState] = useState({ index: 0, atStart: true, atEnd: false });

  /** Where a smooth scroll that is still running is headed, so that a
   * second arrow click steps on from there rather than from wherever
   * the animation happens to be at that instant. Without it, clicking
   * through the markets quickly loses most of the clicks -- each one
   * re-snaps to the boundary the row is passing through. Cleared once
   * the row arrives, or the moment the reader takes over themselves. */
  const pending = useRef<number | null>(null);

  // Distance from one item's leading edge to the next, read off the
  // layout rather than rebuilt from the basis and the gap -- those live
  // in the stylesheet and change at two breakpoints, and a second copy
  // of that arithmetic here would be a copy that goes stale.
  const stride = (el: T) => {
    const items = el.children;
    if (items.length === 0) return 0;
    const first = items[0].getBoundingClientRect();
    if (items.length === 1) return first.width;
    return items[1].getBoundingClientRect().left - first.left;
  };

  /** The stylesheet's ceiling for the near edge's fade, cached because
   * this is read on every scroll frame and dropped whenever the row is
   * resized, since the breakpoints may have changed it. Kept in CSS
   * rather than here so the stylesheet stays the one place a length on
   * this row is decided -- the same arrangement as `--row-h`. */
  const cap = useRef<number | null>(null);

  const read = useCallback(() => {
    const el = ref.current;
    if (!el) return;
    const step = stride(el);
    const max = el.scrollWidth - el.clientWidth;
    if (pending.current !== null && Math.abs(el.scrollLeft - pending.current) <= 1) {
      pending.current = null;
    }

    // How wide the near edge's fade should be right now.
    //
    // Only ever as wide as what is actually cut off, which is what the
    // arrows exposed: they leave the row on a boundary, where the panel
    // starts flush against the edge and nothing is sliced -- and a fade
    // there dimmed the first inch of the market you had just asked for.
    //
    // `into` is how far the row sits past the last boundary. Just past
    // one, only that sliver of the outgoing panel is hidden and a fade
    // that wide covers it; just short of the next, the outgoing panel is
    // down to its last few pixels and the fade shrinks to match rather
    // than reaching across the panel arriving behind it. In the middle,
    // both are wide and the stylesheet's ceiling applies.
    if (cap.current === null) {
      cap.current = parseFloat(getComputedStyle(el).getPropertyValue("--leading-fade-max")) || 0;
    }
    const into = step > 0 ? el.scrollLeft % step : 0;
    const fade = Math.max(0, Math.min(into, step - into, cap.current));
    el.style.setProperty("--leading-fade", `${Math.round(fade)}px`);

    const index = step > 0 ? Math.round(el.scrollLeft / step) : 0;
    // A pixel of slack at each end: a scroll that has arrived can sit
    // a fraction short of its own maximum, and the arrow at that end
    // must still be the one that is switched off.
    const atStart = el.scrollLeft <= 1;
    const atEnd = el.scrollLeft >= max - 1;
    // Handing back the previous object when nothing changed is what
    // lets React skip the re-render. This runs on every scroll event of
    // a free-scrolling row -- most frames move the row without moving
    // the front market -- and an unconditional fresh object here meant
    // every one of those frames re-rendered the whole board above the
    // fade it came for.
    setState((previous) =>
      previous.index === index && previous.atStart === atStart && previous.atEnd === atEnd
        ? previous
        : { index, atStart, atEnd },
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

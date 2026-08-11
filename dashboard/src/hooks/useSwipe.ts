import { useEffect, useRef, useState } from "react";

/** How far a finger must travel before the gesture is called horizontal
 * or vertical. Below this a touch is still ambiguous -- nobody starts a
 * scroll perfectly plumb -- and committing early is what makes a
 * carousel steal the page's scrolling. */
const AXIS_LOCK = 10;

/** How far a horizontal drag must reach to turn the page on release.
 * Short enough to flick, long enough that a thumb resting on a moving
 * panel does not page by accident. */
const COMMIT = 48;

/** The row follows the finger at this fraction of its travel, up to
 * MAX_PULL. Damped rather than one-to-one on purpose: nothing is
 * rendered before the first panel, so a full-speed drag to the right
 * would open a gap where a previous panel ought to be. What is wanted
 * here is the feel of a row that gives and springs back, with the page
 * turning underneath it -- not a hand-driven slide. */
const PULL = 0.35;
const MAX_PULL = 56;

/** A trackpad reports a swipe as a stream of small horizontal wheel
 * deltas rather than as a pointer, so it is paged on accumulated travel
 * instead. Larger than the touch commit: a two-finger swipe puts out far
 * more delta than a thumb covers in pixels, and at 48 the row paged
 * before the gesture felt finished. */
const WHEEL_COMMIT = 64;

/** How long the deltas must stop for before the gesture counts as over.
 * There is no "wheel end" event, and a flick on a Mac trackpad keeps
 * sending momentum for the better part of a second after the fingers
 * have lifted -- which, without this, pages once per frame of coasting.
 * Only a deliberate second swipe restarts it. */
const WHEEL_IDLE = 220;

/** Direction a swipe asks for: 1 is the next item, -1 the previous.
 *
 * Content-relative, not finger-relative. Dragging *left* pulls the next
 * panel towards you, which is +1 -- the same sense as the pager's right
 * arrow. */
export type SwipeDirection = 1 | -1;

/** Turns horizontal swipes on an element into paging: a finger dragging
 * the row, or two fingers on a trackpad.
 *
 * Drags are touch and pen only. A mouse *drag* across these panels is a
 * text selection or the start of a click on an agent link, and a
 * carousel that swallows either would cost more than the swipe gains --
 * a cursor's way in is the arrows, or the trackpad it is attached to.
 *
 * Returns the ref to attach, how far the row should currently be pulled,
 * and whether a drag is in progress. The caller applies the offset (and
 * decides that a drag suppresses the spring-back transition); the hook
 * owns none of the painting.
 *
 * The callback is held in a ref rather than being a dependency of the
 * effect. It closes over the caller's state, so it is a new function on
 * every render, and depending on it would tear the listeners down and
 * rebuild them mid-gesture -- losing the drag whenever a poll landed.
 */
export function useSwipe<T extends HTMLElement = HTMLDivElement>(
  onSwipe: (direction: SwipeDirection) => void,
) {
  const ref = useRef<T | null>(null);
  const [offset, setOffset] = useState(0);
  const [dragging, setDragging] = useState(false);

  const latest = useRef(onSwipe);
  latest.current = onSwipe;

  useEffect(() => {
    const el = ref.current;
    if (!el) return;

    // Set on pointerdown and cleared on release; `axis` stays null until
    // the finger has moved far enough to say which way it is going.
    let pointer: number | null = null;
    let startX = 0;
    let startY = 0;
    let axis: "x" | "y" | null = null;

    // Trackpad travel since the gesture began, whether a page has been
    // turned already, and the timer that decides the gesture is over.
    let travel = 0;
    let turned = false;
    let idle: ReturnType<typeof setTimeout> | undefined;

    // Back to rest, whichever kind of gesture was in progress. Both
    // inputs share it so neither can leave the row held off centre.
    const reset = () => {
      pointer = null;
      axis = null;
      travel = 0;
      turned = false;
      setDragging(false);
      setOffset(0);
    };

    const down = (e: PointerEvent) => {
      if (e.pointerType === "mouse" || e.isPrimary === false) return;
      pointer = e.pointerId;
      startX = e.clientX;
      startY = e.clientY;
      axis = null;
    };

    const move = (e: PointerEvent) => {
      if (e.pointerId !== pointer) return;
      const dx = e.clientX - startX;
      const dy = e.clientY - startY;

      if (axis === null) {
        if (Math.abs(dx) < AXIS_LOCK && Math.abs(dy) < AXIS_LOCK) return;
        axis = Math.abs(dx) > Math.abs(dy) ? "x" : "y";
        if (axis === "y") {
          // Theirs, not ours: let go of the gesture entirely so the page
          // scrolls as it would have without this hook.
          pointer = null;
          return;
        }
        setDragging(true);
        // Captured only once the gesture is known to be ours, so a
        // scroll that starts here is never hijacked. Guarded because
        // jsdom implements pointer events without pointer capture.
        el.setPointerCapture?.(e.pointerId);
      }

      setOffset(Math.sign(dx) * Math.min(MAX_PULL, Math.abs(dx) * PULL));
    };

    const up = (e: PointerEvent) => {
      if (e.pointerId !== pointer) return;
      const dx = e.clientX - startX;
      const committed = axis === "x" && Math.abs(dx) >= COMMIT;
      reset();
      if (committed) latest.current(dx < 0 ? 1 : -1);
    };

    const cancel = (e: PointerEvent) => {
      if (e.pointerId === pointer) reset();
    };

    const wheel = (e: WheelEvent) => {
      // The page's, not ours. Left alone rather than merely ignored: not
      // calling preventDefault is what lets the scroll happen.
      if (Math.abs(e.deltaX) <= Math.abs(e.deltaY)) return;

      // A horizontal wheel over a page with nothing to scroll sideways
      // is what triggers the browser's back-swipe. Taking the event is
      // what stops a flick through the markets from leaving the site --
      // and it is why this listener has to be non-passive.
      e.preventDefault();

      clearTimeout(idle);
      idle = setTimeout(reset, WHEEL_IDLE);

      // Momentum after a page has already turned. Still swallowed
      // above, so the coast cannot navigate away, but it pages no
      // further; a second swipe means a second market.
      if (turned) return;

      // A reversal is a new gesture, not a smaller old one.
      if (Math.sign(e.deltaX) !== Math.sign(travel)) travel = 0;
      travel += e.deltaX;

      if (Math.abs(travel) < WHEEL_COMMIT) {
        setDragging(true);
        // Scrolling right asks for what is to the right, so the row
        // pulls the way a finger going left would pull it.
        setOffset(-Math.sign(travel) * Math.min(MAX_PULL, Math.abs(travel) * PULL));
        return;
      }

      const direction: SwipeDirection = travel > 0 ? 1 : -1;
      turned = true;
      travel = 0;
      // Dropped back to rest here rather than at the end of the coast,
      // so the row springs back as the new market lands under it.
      setDragging(false);
      setOffset(0);
      latest.current(direction);
    };

    el.addEventListener("pointerdown", down);
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
    el.addEventListener("pointercancel", cancel);
    el.addEventListener("wheel", wheel, { passive: false });
    return () => {
      clearTimeout(idle);
      el.removeEventListener("pointerdown", down);
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      el.removeEventListener("pointercancel", cancel);
      el.removeEventListener("wheel", wheel);
    };
  }, []);

  return [ref, offset, dragging] as const;
}

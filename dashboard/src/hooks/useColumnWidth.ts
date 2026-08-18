import { useCallback, useLayoutEffect, useRef, useState } from "react";
import type { KeyboardEvent, PointerEvent, RefObject } from "react";

/** A side column whose edge can be dragged, and which shuts when the
 * edge is dragged far enough in.
 *
 * The width is published as a **custom property on a host element**
 * rather than as an inline style React renders, and that is the whole
 * design of this hook. The board is an expensive tree -- a dozen sector
 * panels and some hundred and fifty sparklines (Round 36 measured what
 * one keystroke in the leaderboard search used to cost) -- and a drag
 * produces a pointermove per frame. Committing each of those to state
 * would re-render the board behind every frame of the drag. So the drag
 * writes straight to the DOM, exactly as `useCarousel` does with the
 * carousel's scroll position, and only the *settled* width becomes state
 * -- once, on pointer-up, which is also when it is remembered.
 *
 * The host is the element that already declares the column widths
 * (`.itx-board-inner`), so both grids that resolve against them -- the
 * columns and the heading line above -- follow one number.
 */

/** How far past its floor an edge has to be dragged before the column
 * shuts entirely. Without slack the column would snap shut on the way
 * past its minimum, which makes the last stretch of a resize feel like a
 * cliff; with it, shutting is a deliberate shove. */
const SHUT_SLACK = 44;

/** How much one arrow-key press moves an edge. */
const STEP = 16;

/** Farther than this and a pointer-up ends a drag rather than a click.
 * Kept above zero because a click on a trackpad usually moves a pixel or
 * two, and a shut column that reopened only on a perfectly still click
 * would read as broken. */
const CLICK_SLOP = 3;

/** On `<body>` for the length of a drag: holds the resize cursor while
 * the pointer is out over the page, and stops the drag selecting text.
 * The pointer is captured by the grip, so the cursor cannot come from
 * whatever is under it. */
const RESIZING_CLASS = "itx-col-resizing";

/** On the grip being dragged, so it stays lit while the pointer is
 * somewhere out over the page. `:hover` cannot do this job: the pointer
 * is captured, so it leaves the grip on the first frame of the drag. */
const DRAGGING_CLASS = "is-dragging";

export type ColumnWidthOptions = {
  /** The element the width is published on. Both this column and
   * anything else that lines up against it read the property from here,
   * so it has to be a common ancestor. */
  host: RefObject<HTMLElement | null>;
  /** The custom property the stylesheet sizes the column with. */
  property: string;
  /** The attribute set on the host while the column is shut, so the
   * stylesheet can put its contents away. */
  attribute: string;
  /** Where the width is remembered between visits. */
  storageKey: string;
  /** Which edge of the page the column is on -- which is what says
   * whether dragging right widens it or narrows it. */
  side: "left" | "right";
  /** The narrowest the column may be while open, the widest it may get,
   * and what it is before anyone has dragged it. */
  min: number;
  max: number;
  initial: number;
  /** What the column holds, in lower case and as a noun phrase: "the
   * leaderboard and trends". Read out as part of the grip's name. */
  label: string;
  /** `id` of the column this grip sizes. */
  controls: string;
};

export type ColumnWidth = {
  /** The settled width in pixels; `0` when the column is shut. */
  width: number;
  shut: boolean;
  /** Puts the column away, or brings it back at the width it had. */
  toggle: () => void;
  /** Everything the grip element needs. Spread onto a plain `<div>`. */
  grip: GripProps;
};

type GripProps = {
  role: "separator";
  tabIndex: 0;
  "aria-orientation": "vertical";
  "aria-label": string;
  "aria-controls": string;
  "aria-valuenow": number;
  "aria-valuemin": number;
  "aria-valuemax": number;
  title: string;
  onPointerDown: (event: PointerEvent<HTMLElement>) => void;
  onPointerMove: (event: PointerEvent<HTMLElement>) => void;
  onPointerUp: (event: PointerEvent<HTMLElement>) => void;
  onPointerCancel: (event: PointerEvent<HTMLElement>) => void;
  onKeyDown: (event: KeyboardEvent<HTMLElement>) => void;
  onDoubleClick: () => void;
};

/** Private-mode Safari throws on `localStorage` access rather than
 * returning null -- the same guard `useTheme` carries. */
function readStored(key: string): number | null {
  try {
    const stored = localStorage.getItem(key);
    if (stored === null) return null;
    const px = Number(stored);
    return Number.isFinite(px) && px >= 0 ? px : null;
  } catch {
    return null;
  }
}

/** Where an edge dragged to `px` actually lands: shut once it is past
 * the floor's slack, and clamped between the floor and the ceiling
 * anywhere above that.
 *
 * Exported for its own test -- it is the whole of the resize's
 * arithmetic, and the rest of this file is pointer plumbing around it. */
export function settle(px: number, min: number, max: number): number {
  if (px < min - SHUT_SLACK) return 0;
  return Math.min(Math.max(px, min), max);
}

function writeStored(key: string, px: number): void {
  try {
    localStorage.setItem(key, String(px));
  } catch {
    // Not being able to remember a width is not a reason to refuse one.
  }
}

export function useColumnWidth(options: ColumnWidthOptions): ColumnWidth {
  const { host, property, attribute, storageKey, side, min, max, initial, label, controls } =
    options;

  const resolve = useCallback((px: number) => settle(px, min, max), [min, max]);

  const [width, setWidth] = useState(() => {
    const stored = readStored(storageKey);
    if (stored === null) return initial;
    return stored === 0 ? 0 : Math.min(Math.max(stored, min), max);
  });

  /** What a shut column opens back to. Its own ref rather than a second
   * piece of state: nothing renders from it, and it must survive the
   * render that shuts the column. */
  const lastOpen = useRef(width || initial);

  const publish = useCallback(
    (px: number) => {
      const el = host.current;
      if (!el) return;
      el.style.setProperty(property, `${px}px`);
      el.toggleAttribute(attribute, px === 0);
    },
    [host, property, attribute],
  );

  // Before paint, not after. A remembered width applied in an ordinary
  // effect would show one frame of the stylesheet's default first, which
  // on a shut rail is a full column appearing and vanishing on every
  // load.
  useLayoutEffect(() => {
    publish(width);
  }, [publish, width]);

  const commit = useCallback(
    (px: number) => {
      if (px > 0) lastOpen.current = px;
      publish(px);
      setWidth(px);
      writeStored(storageKey, px);
    },
    [publish, storageKey],
  );

  const toggle = useCallback(() => {
    commit(width === 0 ? lastOpen.current : 0);
  }, [commit, width]);

  /** The live drag, held in a ref for the same reason the width is
   * published to the DOM: none of it may cost a render. */
  const drag = useRef<{ id: number; from: number; x: number; live: number; moved: boolean } | null>(
    null,
  );

  const onPointerDown = useCallback(
    (event: PointerEvent<HTMLElement>) => {
      if (event.button !== 0) return;
      // Stops the drag selecting the board's text on the way past.
      event.preventDefault();
      event.currentTarget.setPointerCapture(event.pointerId);
      drag.current = {
        id: event.pointerId,
        from: width,
        x: event.clientX,
        live: width,
        moved: false,
      };
      event.currentTarget.classList.add(DRAGGING_CLASS);
      document.body.classList.add(RESIZING_CLASS);
    },
    [width],
  );

  const onPointerMove = useCallback(
    (event: PointerEvent<HTMLElement>) => {
      const state = drag.current;
      if (!state || state.id !== event.pointerId) return;
      // A left column grows as the pointer goes right; a right column
      // grows as it goes left.
      const travel = side === "left" ? event.clientX - state.x : state.x - event.clientX;
      if (Math.abs(travel) > CLICK_SLOP) state.moved = true;
      state.live = resolve(state.from + travel);
      publish(state.live);
    },
    [publish, resolve, side],
  );

  const onPointerUp = useCallback(
    (event: PointerEvent<HTMLElement>) => {
      const state = drag.current;
      if (!state || state.id !== event.pointerId) return;
      drag.current = null;
      event.currentTarget.classList.remove(DRAGGING_CLASS);
      document.body.classList.remove(RESIZING_CLASS);
      if (event.currentTarget.hasPointerCapture(event.pointerId)) {
        event.currentTarget.releasePointerCapture(event.pointerId);
      }
      // A click that went nowhere is not a resize. On a shut column it
      // is the way back -- the grip is the only thing left of the
      // column, so it has to be the handle that opens it.
      if (!state.moved) {
        commit(state.from === 0 ? lastOpen.current : state.from);
        return;
      }
      commit(state.live);
    },
    [commit],
  );

  const onKeyDown = useCallback(
    (event: KeyboardEvent<HTMLElement>) => {
      if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
        const towardsRight = event.key === "ArrowRight" ? 1 : -1;
        const grow = side === "left" ? towardsRight : -towardsRight;
        event.preventDefault();
        // Widening a shut column takes it back to where it was rather
        // than crawling out of the shut zone 16px at a time -- from
        // zero, every step short of the floor's slack resolves to shut
        // again, so the key would do nothing at all for five presses.
        commit(width === 0 && grow > 0 ? lastOpen.current : resolve(width + grow * STEP));
      } else if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        toggle();
      }
    },
    [commit, resolve, side, toggle, width],
  );

  return {
    width,
    shut: width === 0,
    toggle,
    grip: {
      role: "separator",
      tabIndex: 0,
      "aria-orientation": "vertical",
      "aria-label": width === 0 ? `show ${label}` : `resize or hide ${label}`,
      "aria-controls": controls,
      "aria-valuenow": width,
      "aria-valuemin": 0,
      "aria-valuemax": max,
      title: width === 0 ? `show ${label}` : `drag to resize ${label}, or in to hide it`,
      onPointerDown,
      onPointerMove,
      onPointerUp,
      onPointerCancel: onPointerUp,
      onKeyDown,
      onDoubleClick: toggle,
    },
  };
}

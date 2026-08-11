import { useEffect, useSyncExternalStore } from "react";

export type Theme = "dark" | "light";

const THEME_KEY = "itx-theme";

/** The theme, as a module-level store rather than component state.
 *
 * It used to live in `Shell`, which was enough while only the terminal
 * pages could be themed. Now the toggle sits in the masthead -- on every
 * surface -- and two roots read the answer: the masthead's button and
 * whichever page root is mounted. Component state in one of them cannot
 * be seen by the other without threading a provider through both trees,
 * and there is only ever one theme per document, so a store is the
 * honest shape for it.
 *
 * `useSyncExternalStore` rather than a context: no provider to place, and
 * every subscriber re-renders on the same tick. */
let current: Theme | null = null;
const listeners = new Set<() => void>();

/** An explicit earlier choice wins; otherwise follow the OS. Resolved
 * once, lazily -- this is a client-rendered SPA, so there is no
 * flash-of-wrong-theme window before React runs. */
function read(): Theme {
  try {
    const stored = localStorage.getItem(THEME_KEY);
    if (stored === "light" || stored === "dark") return stored;
  } catch {
    // Private-mode Safari throws on access rather than returning null.
  }
  return window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
}

export function getTheme(): Theme {
  if (current === null) current = read();
  return current;
}

/** The class that lets the ground, the ink and the outlines cross over,
 * and how long it stays on.
 *
 * Deliberately longer than the 170ms transition in sitebar.css. Taking
 * the class off is what ends the transition, so if the two were equal a
 * timer that fired a frame early would cancel the cross-fade at 95% and
 * snap the last of it -- the exact thing this is here to remove. The
 * slack costs nothing: with no transition left to run, the class is
 * inert for its last 60ms. */
const SHIFT_CLASS = "itx-theme-shift";
const SHIFT_MS = 230;
let shiftTimer: number | undefined;

/** Colour transitions are worth having only on the switch itself.
 *
 * Left on permanently they would also catch every hover, every arriving
 * row and every panel that repaints on a poll -- a page where nothing
 * quite lands when it changes. So the flag goes on for the length of the
 * cross-fade and comes off again, and the rest of the time the surface
 * is as immediate as it was. */
function startShift(): void {
  // No forced reflow between this and the token change, deliberately.
  // The usual trick would be to flush styles here so the "before" style
  // already declares the transition -- but a transition is started from
  // the *after*-change style, so declaring it in the same recalc as the
  // new colours is enough. Verified in the browser: the panels report
  // six running transitions either way.
  document.body.classList.add(SHIFT_CLASS);

  window.clearTimeout(shiftTimer);
  shiftTimer = window.setTimeout(() => {
    document.body.classList.remove(SHIFT_CLASS);
  }, SHIFT_MS);
}

export function setTheme(next: Theme): void {
  if (next === getTheme()) return;
  current = next;
  try {
    localStorage.setItem(THEME_KEY, next);
  } catch {
    // Not being able to remember the choice is not a reason to refuse it.
  }
  // Before the subscribers re-render, so the flag is already on the body
  // in the frame the new tokens land -- a transition cannot catch a
  // change that happened before it was declared.
  startShift();
  for (const fn of listeners) fn();
}

export function toggleTheme(): void {
  setTheme(getTheme() === "dark" ? "light" : "dark");
}

function subscribe(fn: () => void): () => void {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

/** The current theme, re-rendering the caller when it changes. */
export function useTheme(): Theme {
  return useSyncExternalStore(subscribe, getTheme, getTheme);
}

/** Marks `<body>` with a page class and the current theme for as long as
 * that page is mounted, and clears both on the way out.
 *
 * The body needs them because the page root does not reach the whole
 * document: overscroll at either end bounces against the body, and a
 * dark page on a white body shows as a flash of white. They are applied
 * per page rather than globally so the three legacy dashboard pages,
 * which render outside both themed roots, keep their bare styling. */
export function useThemedBody(className: string): Theme {
  const theme = useTheme();

  useEffect(() => {
    document.body.classList.add(className);
    document.body.dataset.theme = theme;
    return () => {
      document.body.classList.remove(className);
      delete document.body.dataset.theme;
    };
  }, [className, theme]);

  return theme;
}

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

export function setTheme(next: Theme): void {
  if (next === getTheme()) return;
  current = next;
  try {
    localStorage.setItem(THEME_KEY, next);
  } catch {
    // Not being able to remember the choice is not a reason to refuse it.
  }
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

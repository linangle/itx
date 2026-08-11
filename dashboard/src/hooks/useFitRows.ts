import { useEffect, useRef, useState } from "react";

/** Rows to assume before the box has been measured. One paint's worth of
 * plausible content, so a slow first measure shows a partly-filled panel
 * rather than an empty one. */
const ASSUMED_ROWS = 8;

/** Measures a box and reports how many table rows fit inside it.
 *
 * The board's panels are sized by the layout, not by their contents --
 * a market panel is as tall as the leaderboard rail beside it, whatever
 * is in it. Rendering a fixed number of rows into a variable-height box
 * is what left the panels half empty: ten agents in a box with room for
 * eighteen. This closes that gap from the other side; the box says how
 * many rows it can hold and the caller renders that many.
 *
 * The row height is read from the `--row-h` custom property rather than
 * duplicated here, so the stylesheet stays the one place a row's height
 * is decided. Anything inside the box that isn't a row -- a table's
 * header -- is marked `data-fit-fixed` and measured out of the budget.
 *
 * No feedback loop: the observed box takes its height from the panel
 * (`flex: 1` in a fixed-height column), so rendering more rows into it
 * cannot make it taller. `floor` plus `overflow: hidden` on the box
 * means a rounding error clips a pixel rather than growing the panel.
 */
export function useFitRows<T extends HTMLElement = HTMLDivElement>() {
  const ref = useRef<T | null>(null);
  const [rows, setRows] = useState(ASSUMED_ROWS);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;

    const measure = () => {
      const rowHeight = parseFloat(getComputedStyle(el).getPropertyValue("--row-h"));
      if (!rowHeight) return;

      let available = el.clientHeight;
      for (const fixed of el.querySelectorAll<HTMLElement>("[data-fit-fixed]")) {
        available -= fixed.offsetHeight;
      }

      setRows(Math.max(0, Math.floor(available / rowHeight)));
    };

    measure();

    // jsdom has no ResizeObserver; the tests render these panels for
    // their contents, not their geometry, so the initial measure stands.
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  return [ref, rows] as const;
}

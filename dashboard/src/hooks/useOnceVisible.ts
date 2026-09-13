import { useCallback, useEffect, useState } from "react";

/** How far ahead of the viewport an element counts as coming into view,
 * so what is drawn on first sight is drawn by the time the reader's
 * scroll arrives at it rather than a frame after. */
const AHEAD = "200px";

/** Whether an element has been on screen yet -- once, and then for good.
 *
 * For work that is worth doing only for things a reader will see: the
 * market activity section draws a chart per market, and a board of
 * hundreds of markets is hundreds of charts, most of them below the fold
 * or scrolled off the end of a row. `true` stays true: a chart that
 * vanished when scrolled away would flicker back into being on every
 * pass over it.
 *
 * A callback ref rather than a ref object, so an element that is
 * replaced is observed afresh -- the lesson `useCarousel` learned. And
 * without `IntersectionObserver` at all (jsdom), everything is visible at
 * once: drawing it all is the safe failure, drawing nothing is not. */
export function useOnceVisible<T extends HTMLElement>(): [(el: T | null) => void, boolean] {
  const [node, setNode] = useState<T | null>(null);
  const [seen, setSeen] = useState(false);
  const ref = useCallback((el: T | null) => setNode(el), []);

  useEffect(() => {
    if (!node || seen) return;
    if (typeof IntersectionObserver === "undefined") {
      setSeen(true);
      return;
    }
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) {
          setSeen(true);
          observer.disconnect();
        }
      },
      { rootMargin: AHEAD },
    );
    observer.observe(node);
    return () => observer.disconnect();
  }, [node, seen]);

  return [ref, seen];
}

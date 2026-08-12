import { useEffect, useRef, useState } from "react";

/** A ref to attach, and the element's current content width in pixels.
 *
 * For charts, which cannot be laid out in percentages: an SVG scaled by
 * `viewBox` stretches its text and its stroke widths along with its
 * geometry, so a chart that must keep 11px labels 11px at every width
 * has to be drawn at real pixel coordinates and therefore has to know
 * what those are.
 *
 * `0` until the first measurement lands, which callers should treat as
 * "not ready to draw" rather than as a zero-width chart.
 */
export function useElementWidth<T extends HTMLElement>(): [React.RefObject<T | null>, number] {
  const ref = useRef<T>(null);
  const [width, setWidth] = useState(0);

  useEffect(() => {
    const element = ref.current;
    if (!element) return;

    // Measured once up front as well as on resize: `ResizeObserver`
    // does fire an initial callback, but only after a frame, and
    // without this the chart renders one frame at zero width.
    setWidth(element.clientWidth);

    const observer = new ResizeObserver((entries) => {
      for (const entry of entries) {
        // `contentRect` is the padding-excluded box, which is the space
        // actually available to draw in.
        setWidth(Math.round(entry.contentRect.width));
      }
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  return [ref, width];
}

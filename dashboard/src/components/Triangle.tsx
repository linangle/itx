export type TriangleDirection = "left" | "right" | "up" | "down";

/** The one arrow on the site.
 *
 * Every arrow used to be whatever its author reached for: the board's
 * carousel drew a sharp SVG triangle, the rail's pager used `‹` and `›`,
 * the terminal's pager used `←` and `→` with words, sort headers used
 * `▲`/`▼`, and the dropdowns wore a CSS chevron. Five marks for one
 * idea. This is the carousel's triangle -- the one that was actually
 * drawn rather than typed -- with its corners taken off.
 *
 * **The rounding is a stroke, not a path.** Rounding a triangle's
 * corners by hand means three arcs and six tangent points recomputed
 * for every direction; painting the same path with a round `linejoin`
 * and letting the stroke stand proud of the fill does it in one
 * attribute, and stays correct if the geometry is ever nudged. The
 * stroke is half the rounding radius wide and the path is inset by that
 * much, so the silhouette lands where the sharp triangle's did.
 *
 * Sized in `em` so it takes the type size of whatever it sits in: 12px
 * in a pager button, 7px beside a table header. Callers that need a
 * fixed size set `font-size` on the button, not a width here.
 */
export default function Triangle({
  direction = "right",
  toEnd = false,
  className,
}: {
  direction?: TriangleDirection;
  /** Draws the bar the triangle runs into: the same arrow, but "as far
   * as this goes" rather than "one more". Only the horizontal
   * directions have one -- the vertical pair are sort carets, where
   * there is no end to travel to. */
  toEnd?: boolean;
  className?: string;
}) {
  const barred = toEnd && (direction === "left" || direction === "right");
  return (
    <svg
      className={className}
      viewBox="0 0 12 12"
      width="1em"
      height="1em"
      aria-hidden="true"
      focusable="false"
    >
      <path
        d={barred ? END_PATHS[direction as "left" | "right"] : PATHS[direction]}
        fill="currentColor"
        stroke="currentColor"
        strokeWidth="1.6"
        strokeLinejoin="round"
      />
      {barred && (
        // The stop the triangle is travelling to. Drawn as a stroked
        // line rather than a filled rectangle so it takes the same
        // round cap the triangle's corners have -- a square-ended bar
        // beside a rounded arrow reads as two different icons.
        <line
          x1={direction === "right" ? 10 : 2}
          x2={direction === "right" ? 10 : 2}
          y1={2.8}
          y2={9.2}
          stroke="currentColor"
          strokeWidth="1.6"
          strokeLinecap="round"
        />
      )}
    </svg>
  );
}

/** Inset by 0.8 (half the stroke) from a 1..11 triangle, so stroke and
 * fill together fill the same box the unrounded one did. */
const PATHS: Record<TriangleDirection, string> = {
  right: "M2.6 2.1 L9.2 6 L2.6 9.9 Z",
  left: "M9.4 2.1 L2.8 6 L9.4 9.9 Z",
  down: "M2.1 3.4 L6 9.2 L9.9 3.4 Z",
  up: "M2.1 8.6 L6 2.8 L9.9 8.6 Z",
};

/** The same triangles, pulled back and narrowed to leave room for the
 * bar beside them. The apex stops short of where the plain arrow's
 * lands, so the pair reads as one mark at a glance rather than as an
 * arrow that has grown. */
const END_PATHS: Record<"left" | "right", string> = {
  right: "M1.8 2.6 L7.5 6 L1.8 9.4 Z",
  left: "M10.2 2.6 L4.5 6 L10.2 9.4 Z",
};

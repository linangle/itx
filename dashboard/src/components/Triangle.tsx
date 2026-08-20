export type TriangleDirection = "left" | "right" | "up" | "down";

/** The one arrow on the site.
 *
 * Every arrow used to be whatever its author reached for: a sharp SVG
 * triangle, `‹` and `›`, `←` and `→` with words, `▲`/`▼`, and a CSS
 * chevron -- five marks for one idea. This is the carousel's triangle
 * with its corners taken off.
 *
 * **The rounding is a stroke, not a path.** Rounding a triangle's corners
 * by hand means three arcs and six tangent points recomputed for every
 * direction; painting the same path with a round `linejoin` and letting
 * the stroke stand proud of the fill does it in one attribute. The stroke
 * is half the rounding radius wide and the path is inset by that much, so
 * the silhouette lands where the sharp triangle's did.
 *
 * Sized in `em` so it takes the type size of whatever it sits in.
 */
export default function Triangle({
  direction = "right",
  toEnd = false,
  className,
}: {
  direction?: TriangleDirection;
  /** Draws the bar the triangle runs into: the same arrow, but "as far as
   * this goes" rather than "one more". Only the horizontal directions
   * have one -- the vertical pair are sort carets. */
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
        // The stop the triangle is travelling to. A stroked line rather
        // than a filled rectangle so it takes the same round cap the
        // triangle's corners have.
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

/** The same triangles, pulled back and narrowed to leave room for the bar
 * beside them. The apex stops short of where the plain arrow's lands, so
 * the pair reads as one mark rather than as an arrow that has grown. */
const END_PATHS: Record<"left" | "right", string> = {
  right: "M1.8 2.6 L7.5 6 L1.8 9.4 Z",
  left: "M10.2 2.6 L4.5 6 L10.2 9.4 Z",
};

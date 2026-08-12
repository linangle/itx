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
  className,
}: {
  direction?: TriangleDirection;
  className?: string;
}) {
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
        d={PATHS[direction]}
        fill="currentColor"
        stroke="currentColor"
        strokeWidth="1.6"
        strokeLinejoin="round"
      />
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

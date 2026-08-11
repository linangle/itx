/** The landing surface's green<->red wash, shared by the market line and
 * the quote strip's outline.
 *
 * Both read the same function against the same clock (`performance.now()
 * / 1000`), which is what keeps them genuinely in step rather than
 * merely running at the same speed. A CSS animation on the strip would
 * have matched the period but started its phase whenever the element
 * mounted, so the two would agree only by luck -- and they sit directly
 * on top of each other at the seam, where any disagreement shows. */

/** Landing green and red, matching the palette in landing.css. */
const GREEN = [99, 186, 108] as const;
const RED = [216, 64, 45] as const;

/** Sit on a colour, sweep to the other, sit again. One full loop is
 * 2 * (HOLD + SWEEP) = 13 s. */
const HOLD = 4.0;
const SWEEP = 2.5;
export const CYCLE = 2 * (HOLD + SWEEP);
/** Width of the travelling colour front, as a fraction of the width. */
const EDGE = 0.35;

/** `alpha` is for callers that need the mix to vary in strength as well
 * as in hue across one gradient -- the market line's bloom bakes its
 * wave into the stops this way. Opaque by default, so every existing
 * caller keeps the `rgb()` it had. */
export function mixColor(m: number, alpha = 1): string {
  const r = Math.round(GREEN[0] + (RED[0] - GREEN[0]) * m);
  const g = Math.round(GREEN[1] + (RED[1] - GREEN[1]) * m);
  const b = Math.round(GREEN[2] + (RED[2] - GREEN[2]) * m);
  return alpha >= 1 ? `rgb(${r}, ${g}, ${b})` : `rgba(${r}, ${g}, ${b}, ${alpha})`;
}

function smoothstep(lo: number, hi: number, x: number): number {
  const s = Math.min(1, Math.max(0, (x - lo) / (hi - lo)));
  return s * s * (3 - 2 * s);
}

/** Red fraction (0 = green, 1 = red) at horizontal position `xFrac`.
 * During holds the whole width is one colour; during sweeps a soft
 * front enters from the left and crosses to the right, overshooting by
 * EDGE on both sides so the far edge finishes its transition. */
export function mixAt(xFrac: number, t: number): number {
  const tc = t % CYCLE;
  if (tc < HOLD) return 0;
  if (tc < HOLD + SWEEP) {
    const front = ((tc - HOLD) / SWEEP) * (1 + 2 * EDGE) - EDGE;
    return 1 - smoothstep(front - EDGE, front + EDGE, xFrac);
  }
  if (tc < 2 * HOLD + SWEEP) return 1;
  const front = ((tc - 2 * HOLD - SWEEP) / SWEEP) * (1 + 2 * EDGE) - EDGE;
  return smoothstep(front - EDGE, front + EDGE, xFrac);
}

/** The wash sampled across the full width, as CSS colours. Used to build
 * the quote strip's horizontal gradient stops. */
export function sweepColors(t: number, steps: number): string[] {
  return Array.from({ length: steps }, (_, i) => mixColor(mixAt(i / (steps - 1), t)));
}

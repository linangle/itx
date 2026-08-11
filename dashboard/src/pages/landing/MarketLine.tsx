import { useEffect, useRef } from "react";
// The wash lives in its own module because the quote strip below the
// hero draws its outline from the same function and clock.
import { mixAt, mixColor } from "./marketHue";

/** Horizontal pixels between ticks. Wide spacing is deliberate:
 * sharpness in a chart pattern comes from *long straight legs meeting
 * at decisive corners* -- the way a textbook double-top or pennant is
 * drawn -- not from packing in more vertices. The earlier tighter
 * spacing added detail but read as fuzz.
 *
 * Scaled to the viewport so the pattern holds roughly the same number
 * of legs at any width; a fixed 56px leaves a phone showing six. */
function spacingFor(width: number): number {
  return Math.max(26, Math.min(56, width / 26));
}

/** Scroll speed in px/s. Paired with the spacing above, a new price
 * prints every ~0.4 s on a phone and ~0.8 s on a desktop. */
const SCROLL = 70;

/** Next price in the walk, as a fraction of the chart's value range.
 *
 * This is a *zigzag* generator, not a diffusion. Chart patterns are
 * drawn as alternating impulse legs -- up, down, up -- of varying
 * length, which is what produces clean peaks and troughs instead of a
 * fuzzy band. So direction flips at most vertices (`REVERSAL`), and
 * when it doesn't, two legs run the same way and become one longer
 * straight run; that is exactly how a real chart shows a strong move.
 *
 * `drift` is a slow bias added on top, so the zigzag as a whole
 * trends up or down over time rather than oscillating around a fixed
 * level. Legs are long relative to the range, which keeps the corners
 * sharp: a short leg between two long ones reads as noise. */
/** Chance a vertex reverses direction. Below 1 on purpose: when two
 * legs run the same way they merge into one long straight move, which
 * is what breaks up the mechanical triangle-wave look and gives the
 * shape its rallies and breakdowns. */
const REVERSAL = 0.7;
const LEG_MIN = 0.1;
const LEG_MAX = 0.38;
/** Occasional outsized legs. Without these every peak reaches roughly
 * the same height and the line reads as a sawtooth generator rather
 * than a price. */
const IMPULSE_CHANCE = 0.16;
const IMPULSE_MIN = 0.42;
const IMPULSE_MAX = 0.8;

interface Walk {
  y: number;
  dir: number;
  drift: number;
}

function nextPrice(w: Walk): number {
  if (Math.random() < REVERSAL) w.dir = -w.dir;
  w.drift = w.drift * 0.9 + (Math.random() - 0.5) * 0.04;
  const leg =
    Math.random() < IMPULSE_CHANCE
      ? IMPULSE_MIN + Math.random() * (IMPULSE_MAX - IMPULSE_MIN)
      : LEG_MIN + Math.random() * (LEG_MAX - LEG_MIN);

  let next = w.y + w.dir * leg + w.drift;
  // Turning at the boundary rather than clamping matters: a clamp
  // would flatten successive prints into a horizontal run along the
  // edge, which is the one shape a zigzag should never produce.
  if (next > 1 || next < 0) {
    w.dir = -w.dir;
    next = w.y + w.dir * leg + w.drift;
  }
  w.y = Math.min(1, Math.max(0, next));
  return w.y;
}

/** The stock tape pinned to the bottom of the hero: a jagged price
 * line that scrolls right to left at constant speed, printing a new
 * tick as it goes, stroked in a travelling green<->red gradient with
 * the area beneath washed in the same colors and faded upward.
 *
 * The line's *shape* is fixed once printed -- it slides, it does not
 * writhe. That is the difference between this and the previous
 * version, where every vertex re-animated in place and the result read
 * as a wobbling rope rather than a chart.
 *
 * The vertical fade is why there's an offscreen buffer: a canvas
 * gradient can vary color horizontally or alpha vertically, but not
 * both in one fill. So the area is filled at full strength on the
 * buffer, a destination-in pass multiplies in the vertical alpha ramp,
 * and the result is composited onto the visible canvas. */
export default function MarketLine() {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    const parent = canvas?.parentElement;
    if (!canvas || !parent) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const buffer = document.createElement("canvas");
    const bctx = buffer.getContext("2d")!;

    let width = 0;
    let height = 0;
    let dpr = 1;
    let spacing = 56;
    /** Smoothed vertical bounds of the visible window. A real chart
     * rescales to the data it is showing, which is also what keeps the
     * line filling its box instead of drifting into a corner; easing
     * the bounds toward the true min/max (rather than snapping) keeps
     * the rescale from jolting when an old extreme scrolls off. */
    let loBound = 0.35;
    let hiBound = 0.65;
    /** Price history, oldest first, as height fractions. One entry per
     * `spacing` px, plus two spare so the ends stay off-canvas. */
    let prices: number[] = [];
    /** How far the tape has scrolled since the last print, in px. */
    let offset = 0;
    const walk: Walk = { y: 0.5, dir: 1, drift: 0 };

    const resize = () => {
      width = parent.clientWidth;
      height = parent.clientHeight;
      dpr = Math.min(window.devicePixelRatio || 1, 2);
      canvas.width = buffer.width = Math.round(width * dpr);
      canvas.height = buffer.height = Math.round(height * dpr);
      spacing = spacingFor(width);
      const need = Math.ceil(width / spacing) + 3;
      // Keep the visible shape across a resize; extend or trim only.
      while (prices.length < need) prices.push(nextPrice(walk));
      if (prices.length > need) prices = prices.slice(prices.length - need);
    };

    const gradientFor = (g2d: CanvasRenderingContext2D, t: number) => {
      const grad = g2d.createLinearGradient(0, 0, width, 0);
      for (let s = 0; s <= 8; s++) {
        const f = s / 8;
        grad.addColorStop(f, mixColor(mixAt(f, t)));
      }
      return grad;
    };

    /** Map a price to a canvas y through the smoothed bounds, leaving
     * headroom top and bottom so peaks and troughs never touch the
     * edges of the box. */
    const yOf = (price: number) => {
      const span = Math.max(0.08, hiBound - loBound);
      return (0.12 + ((price - loBound) / span) * 0.76) * height;
    };

    // x of the oldest point: one `spacing` off the left edge, minus how
    // far we've scrolled since the last print.
    const linePath = (g2d: CanvasRenderingContext2D) => {
      const x0 = -spacing - offset;
      g2d.moveTo(x0, yOf(prices[0]));
      for (let i = 1; i < prices.length; i++) {
        g2d.lineTo(x0 + i * spacing, yOf(prices[i]));
      }
    };

    const draw = (t: number, dt: number) => {
      if (width === 0 || height === 0 || prices.length === 0) return;

      offset += SCROLL * dt;
      while (offset >= spacing) {
        offset -= spacing;
        prices.shift();
        prices.push(nextPrice(walk));
      }

      let lo = Infinity;
      let hi = -Infinity;
      for (const p of prices) {
        if (p < lo) lo = p;
        if (p > hi) hi = p;
      }
      const ease = Math.min(1, dt * 2.2);
      loBound += (lo - loBound) * ease;
      hiBound += (hi - hiBound) * ease;

      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      bctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, width, height);
      bctx.clearRect(0, 0, width, height);

      bctx.globalCompositeOperation = "source-over";
      bctx.beginPath();
      linePath(bctx);
      bctx.lineTo(width + spacing, height);
      bctx.lineTo(-spacing * 2, height);
      bctx.closePath();
      bctx.fillStyle = gradientFor(bctx, t);
      bctx.fill();

      bctx.globalCompositeOperation = "destination-in";
      const fade = bctx.createLinearGradient(0, 0, 0, height);
      fade.addColorStop(0, "rgba(0, 0, 0, 0)");
      fade.addColorStop(0.55, "rgba(0, 0, 0, 0.16)");
      fade.addColorStop(1, "rgba(0, 0, 0, 0.85)");
      bctx.fillStyle = fade;
      bctx.fillRect(0, 0, width, height);

      ctx.drawImage(buffer, 0, 0, width, height);

      // Sharp corners at every print -- a stock tape, not a wave.
      ctx.lineWidth = 2;
      ctx.lineJoin = "miter";
      ctx.miterLimit = 3;
      ctx.lineCap = "butt";
      ctx.strokeStyle = gradientFor(ctx, t);
      ctx.beginPath();
      linePath(ctx);
      ctx.stroke();
    };

    resize();
    const observer = new ResizeObserver(() => {
      resize();
      draw(performance.now() / 1000, 0);
    });
    observer.observe(parent);

    let raf = 0;
    let last = performance.now();

    const loop = (ts: number) => {
      const dt = Math.min(0.05, (ts - last) / 1000);
      last = ts;
      draw(ts / 1000, dt);
      raf = requestAnimationFrame(loop);
    };

    const start = () => {
      if (raf !== 0) return;
      // Reset the clock, or the first frame after resuming carries the
      // whole paused interval as one delta and the tape lurches sideways.
      // (The `dt` clamp in the loop would blunt it, not prevent it.)
      last = performance.now();
      raf = requestAnimationFrame(loop);
    };

    const stop = () => {
      if (raf === 0) return;
      cancelAnimationFrame(raf);
      raf = 0;
    };

    // Same reasoning as the globe: once the board is on screen this is
    // redrawing for nobody, and it is competing with the board's polling
    // for the main thread.
    const visibility = new IntersectionObserver(([entry]) => (entry.isIntersecting ? start() : stop()), {
      threshold: 0,
    });

    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      draw(2, 0);
    } else {
      visibility.observe(parent);
    }

    return () => {
      cancelAnimationFrame(raf);
      visibility.disconnect();
      observer.disconnect();
    };
  }, []);

  return <canvas ref={canvasRef} className="itx-market-line" aria-hidden="true" />;
}

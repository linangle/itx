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

/** The dotted baseline, in the neutral grey a printed chart rules its
 * zero line in. Hard-coded rather than taken from `--ld-sub` for the
 * same reason the wash is: this canvas is theme-blind, and the line
 * lands on the fill's densest band rather than on the page's ground, so
 * it has the same job against either theme. */
const BASE_RULE = "rgba(138, 142, 156, 0.9)";
/** Longer marks than gaps. At 3px a mark reads as a dot and the rule
 * dissolves into stipple against a busy fill; at 5 it is plainly a
 * ruled line, which is what the zero line of a printed chart is. */
const BASE_DASH = [5, 5];

/** The bloom is the chart again, mirrored under the rule -- but smoothed
 * with this kernel first, which is what stops it reading as a second
 * chart. A binomial blur over five prints takes out every corner the
 * generator works to put in, leaving the swell underneath them. */
const WAVE_SMOOTH = [1, 4, 6, 4, 1];
/** How much of the bloom's shape is the chart above, the rest being the
 * travelling swell below. Weighted to the chart so the band is
 * recognisably its shadow, not enough that it traces it. */
const WAVE_FROM_CHART = 0.6;
/** The swell: crests across the width, and how many widths a crest
 * travels per second. Slow, and *against* the tape -- a wave running
 * with it at its speed would read as a reflection rather than weather. */
const WAVE_CRESTS = 1.6;
const WAVE_DRIFT = 0.055;
/** Alpha at the rule, in the wave's troughs and under its crests. Never
 * all the way to nothing: a band that fully extinguishes anywhere reads
 * as a gap in the glow rather than a dim patch of it. */
const GLOW_MIN = 0.14;
const GLOW_MAX = 0.85;
/** Colour stops across the width. The wave lives in the interpolation
 * between them, so this is also the bloom's resolution: too few and the
 * crests turn into facets. */
const GLOW_STOPS = 48;

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
 * The area stops on a dotted baseline rather than running off the
 * bottom of the box, the way a chart is ruled at its zero line, and
 * under that line sits a bloom in the *counter* colour -- green while
 * the tape is red, red while it is green -- densest against the line
 * and moving in a slow wave. The wave is the chart above, mirrored and
 * blurred past recognition, with a swell travelling the other way over
 * the top of it: near enough to the line to be its shadow, far enough
 * from it that it never reads as a second chart.
 *
 * The vertical fade is why there's an offscreen buffer: a canvas
 * gradient can vary color horizontally or alpha vertically, but not
 * both in one fill. So the area is filled at full strength on the
 * buffer, a destination-in pass multiplies in the vertical alpha ramp,
 * and the result is composited onto the visible canvas.
 *
 * The bloom rides along on that same pass. It is a second fill on the
 * buffer -- the sweep read backwards, with its wave baked into the
 * stops' alpha -- and because it occupies a band the area never
 * touches, one alpha ramp can carry both: it climbs to the baseline for
 * the area, steps back to full, and falls away again for the bloom. Two
 * stops at the same offset are what make that step, and they are why
 * this is one composite rather than two. */
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
    /** Canvas y of the dotted baseline: the floor the price walks on and
     * the top of the bloom. */
    let baseY = 0;
    /** Canvas y the bloom has faded to nothing by: the canvas bottom,
     * which sits `--bd-overlap` *below* the quote strip's top edge. So
     * the bloom is still alight where the strip starts and the strip
     * covers its last stretch -- that overlap is the point, and it is
     * what puts the strip on the chart rather than under it. It reads
     * as depth rather than as clipping because the strip is inset from
     * the page: in the gutters either side there is nothing to cover
     * the glow, and it carries on down past the strip's top edge in
     * plain view. */
    let glowY = 0;
    /** Smoothed vertical bounds of the visible window. A real chart
     * rescales to the data it is showing, which is also what keeps the
     * line filling its box instead of drifting into a corner; easing
     * the bounds toward the true min/max (rather than snapping) keeps
     * the rescale from jolting when an old extreme scrolls off. */
    let loBound = 0.35;
    let hiBound = 0.65;
    /** Whether those two have been sat on real prices yet (see `resize`). */
    let seeded = false;
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
      // Both bands come from landing.css, which is also where the box's
      // own height is worked out from them -- so the baseline lands
      // where the stylesheet says it does instead of where this file
      // guesses. Read here rather than per frame: getComputedStyle
      // forces a style recalc, and a resize is the only thing that can
      // change the answer.
      const css = getComputedStyle(canvas);
      const overlap = parseFloat(css.getPropertyValue("--bd-overlap")) || 24;
      const band = parseFloat(css.getPropertyValue("--ld-chart-base")) || 24;
      // The floor is for short, wide windows, where 17vh is small enough
      // that a fixed band would leave the price nowhere to move.
      baseY = Math.max(height * 0.45, height - overlap - band);
      glowY = Math.max(baseY + 1, height);
      const need = Math.ceil(width / spacing) + 3;
      // Keep the visible shape across a resize; extend or trim only.
      while (prices.length < need) prices.push(nextPrice(walk));
      if (prices.length > need) prices = prices.slice(prices.length - need);

      // First sizing only: start the bounds on the prices that were just
      // generated rather than easing in from a guess. The guess is a
      // narrow one and the walk opens wider than it, so the opening
      // frames clamp a good part of the line flat against the floor --
      // and the reduced-motion path, which draws exactly one frame and
      // never eases at all, would render that as the finished chart.
      if (!seeded) {
        loBound = Math.min(...prices);
        hiBound = Math.max(...prices);
        seeded = true;
      }
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
     * edges of the box. Scaled to the baseline, not the canvas: the band
     * below it belongs to the bloom, and a trough dipping into it would
     * put the line under its own floor.
     *
     * The floor is what makes that a guarantee rather than a hope. The
     * bounds *ease* toward the window's true min and max, so a fresh
     * extreme -- an impulse leg lands one, and `span` has a floor of
     * 0.08 to divide by -- is briefly outside them and normalises past
     * [0, 1]. Simulating the walk puts that at ~3% of printed points,
     * overshooting by up to a couple of dozen px: before the baseline
     * existed it fell off the bottom of the box, where the quote strip
     * is, and nobody saw it. Against a dotted rule it is the line
     * crossing its own zero.
     *
     * Clamping here rather than speeding up the easing keeps the
     * rescale exactly as gentle as it was. The cost is that a trough
     * occasionally bottoms out level instead of overshooting -- which
     * is a support line, and reads as one.
     *
     * Only the low end is clamped. An overshoot the other way leaves
     * the top of the box, which is dead space behind the hero's copy
     * and always was; pinning it there instead draws a flat plateau in
     * the middle of the chart, which is the one shape this generator
     * goes out of its way not to produce. */
    const yOf = (price: number) => {
      const span = Math.max(0.08, hiBound - loBound);
      const f = Math.min(1, (price - loBound) / span);
      return (0.12 + f * 0.76) * baseY;
    };

    /** The chart's height above the rule at each print, in [0, 1], run
     * through `WAVE_SMOOTH`. Taken from `yOf` rather than from `prices`
     * so it inherits the same bounds and the same floor the line is
     * drawn with -- the bloom rescales when the chart does, instead of
     * drifting out of step with it every time the window's range moves. */
    const waveHeights = (): number[] => {
      const last = prices.length - 1;
      const out: number[] = new Array(prices.length);
      for (let i = 0; i <= last; i++) {
        let sum = 0;
        let weight = 0;
        for (let k = -2; k <= 2; k++) {
          // Clamped rather than wrapped at the ends: the tape is a
          // window onto a longer walk, so its edges continue, they do
          // not meet.
          const j = Math.min(last, Math.max(0, i + k));
          const w = WAVE_SMOOTH[k + 2];
          sum += (1 - yOf(prices[j]) / baseY) * w;
          weight += w;
        }
        // Back out `yOf`'s headroom so a peak really is 1.
        out[i] = ((sum / weight) - 0.12) / 0.76;
      }
      return out;
    };

    /** The bloom's strength at `f` across the width: the smoothed chart
     * above, plus a swell on its own clock. */
    const waveAt = (wave: number[], f: number, t: number) => {
      // Same x -> print mapping `linePath` draws with, so a crest sits
      // under the peak it came from rather than beside it.
      const i = (f * width + spacing + offset) / spacing;
      const i0 = Math.min(wave.length - 1, Math.max(0, Math.floor(i)));
      const i1 = Math.min(wave.length - 1, i0 + 1);
      const h = wave[i0] + (wave[i1] - wave[i0]) * (i - Math.floor(i));
      const swell = 0.5 + 0.5 * Math.sin((f * WAVE_CRESTS - t * WAVE_DRIFT) * Math.PI * 2);
      const shape = h * WAVE_FROM_CHART + swell * (1 - WAVE_FROM_CHART);
      return GLOW_MIN + (GLOW_MAX - GLOW_MIN) * Math.min(1, Math.max(0, shape));
    };

    /** The bloom's own gradient: the sweep read backwards for colour,
     * with the wave baked into each stop's alpha.
     *
     * Alpha rather than geometry is what makes this an aurora instead of
     * a second chart. A shape would need its edge blurred to stop
     * reading as one, and that means a canvas filter, a third buffer to
     * keep the blur off the rule, and a clip -- where interpolating
     * alpha between stops *is* the blur, exactly, on every browser and
     * for nothing. Under a vertical falloff a brighter stop is also a
     * deeper-reaching one, so varying strength varies the band's height
     * too, which is the part that actually reads as a wave. */
    const bloomGradient = (g2d: CanvasRenderingContext2D, t: number, wave: number[]) => {
      const grad = g2d.createLinearGradient(0, 0, width, 0);
      for (let s = 0; s <= GLOW_STOPS; s++) {
        const f = s / GLOW_STOPS;
        grad.addColorStop(f, mixColor(1 - mixAt(f, t), waveAt(wave, f, t)));
      }
      return grad;
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
      bctx.lineTo(width + spacing, baseY);
      bctx.lineTo(-spacing * 2, baseY);
      bctx.closePath();
      bctx.fillStyle = gradientFor(bctx, t);
      bctx.fill();

      // The bloom. Its strength varies along the width; the ramp below
      // only has to say how far down it reaches.
      bctx.fillStyle = bloomGradient(bctx, t, waveHeights());
      bctx.fillRect(0, baseY, width, glowY - baseY);

      bctx.globalCompositeOperation = "destination-in";
      const base = baseY / height;
      const fade = bctx.createLinearGradient(0, 0, 0, height);
      fade.addColorStop(0, "rgba(0, 0, 0, 0)");
      fade.addColorStop(0.55 * base, "rgba(0, 0, 0, 0.16)");
      // Twice at the baseline: the area arrives at its densest and the
      // bloom starts at full, and the step between them is what the
      // dotted rule is drawn on. Ramping instead of stepping would put a
      // washed-out seam under the line where the two colours meet.
      fade.addColorStop(base, "rgba(0, 0, 0, 0.85)");
      fade.addColorStop(base, "rgba(0, 0, 0, 1)");
      // Decaying, not linear, and this is what makes the wave read as a
      // wave. The bloom's strength varies along the width but its ramp
      // does not, so how deep it *looks* is wherever strength x ramp
      // falls under seeing. A linear ramp crosses that threshold at
      // nearly the same depth however bright the column is -- an even
      // stripe that merely brightens. A curve puts the crossing
      // somewhere different for each, so crests reach and troughs don't.
      //
      // Gentler than the curve this started as, because the strip's top
      // edge cuts the band at its midpoint and the glow has to still be
      // burning there -- the whole point of the overlap. The first
      // version was down to 15% by that edge, which is lit, but barely
      // enough to tell from having faded out; this holds ~38%, so the
      // strip plainly covers live chart. The tail below the edge is
      // hidden anyway except in the gutters, where it wants to look like
      // it is running out rather than being sliced.
      const band = glowY / height - base;
      fade.addColorStop(base + 0.22 * band, "rgba(0, 0, 0, 0.68)");
      fade.addColorStop(base + 0.5 * band, "rgba(0, 0, 0, 0.38)");
      fade.addColorStop(base + 0.78 * band, "rgba(0, 0, 0, 0.14)");
      fade.addColorStop(glowY / height, "rgba(0, 0, 0, 0)");
      fade.addColorStop(1, "rgba(0, 0, 0, 0)");
      bctx.fillStyle = fade;
      bctx.fillRect(0, 0, width, height);

      ctx.drawImage(buffer, 0, 0, width, height);

      // The zero line. Half a pixel off a whole one so a 1px rule lands
      // on a device pixel row instead of straddling two at half strength.
      ctx.save();
      ctx.setLineDash(BASE_DASH);
      ctx.lineWidth = 1;
      ctx.strokeStyle = BASE_RULE;
      ctx.beginPath();
      ctx.moveTo(0, Math.round(baseY) + 0.5);
      ctx.lineTo(width, Math.round(baseY) + 0.5);
      ctx.stroke();
      ctx.restore();

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

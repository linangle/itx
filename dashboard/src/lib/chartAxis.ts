// Tick selection for the market chart's two axes.
//
// Separated from the drawing for the usual reason: choosing where the
// gridlines go is arithmetic with awkward cases (a flat series, a span
// of ninety seconds, a value range of 0.0003) and testing it through a
// rendered SVG would be testing the SVG. Nothing here imports React.

/** A "nice" step at or above `rough`: 1, 2, 2.5 or 5 times a power of
 * ten. Gridlines land on numbers a reader can hold in their head, which
 * is the whole job of an axis. */
function niceStep(rough: number): number {
  if (!Number.isFinite(rough) || rough <= 0) return 1;
  const magnitude = 10 ** Math.floor(Math.log10(rough));
  const normalized = rough / magnitude;
  const step = normalized <= 1 ? 1 : normalized <= 2 ? 2 : normalized <= 2.5 ? 2.5 : normalized <= 5 ? 5 : 10;
  return step * magnitude;
}

export interface ValueScale {
  /** Bottom and top of the drawn area, already rounded out to ticks. */
  min: number;
  max: number;
  ticks: number[];
}

/** Where to put the horizontal gridlines, and what range to draw over.
 *
 * The axis is rounded *outward* to whole steps so the topmost gridline
 * is at or above the peak rather than floating just under it, which
 * leaves the line poking out of its own chart.
 *
 * **The baseline is zero unless the data is far from it.** A cumulative
 * total that runs from 990 to 1000 is, on a zero-based axis, a flat
 * line — true but useless — and on a tightly-cropped axis, a dramatic
 * climb, which is a lie of a different kind. The rule here: keep zero
 * when the series spans a decent fraction of its own height, crop when
 * it does not, which is the same call a finance chart makes.
 */
export function valueScale(values: number[], targetTicks = 4): ValueScale {
  const finite = values.filter((v) => Number.isFinite(v));
  if (finite.length === 0) return { min: 0, max: 1, ticks: [0, 1] };

  const peak = Math.max(...finite);
  const trough = Math.min(...finite);

  // A flat series still needs a chart. Give it a band around its value
  // rather than a zero-height axis that divides by zero on scaling.
  if (peak === trough) {
    const pad = Math.abs(peak) > 0 ? Math.abs(peak) * 0.5 : 1;
    const min = Math.min(0, peak - pad);
    const max = peak + pad;
    return { min, max, ticks: [min, max] };
  }

  const spansEnough = trough <= peak * 0.4;
  const rawMin = spansEnough ? 0 : trough;
  const step = niceStep((peak - rawMin) / targetTicks);
  const min = Math.floor(rawMin / step) * step;
  const max = Math.ceil(peak / step) * step;

  const ticks: number[] = [];
  // The epsilon absorbs the float error that otherwise drops the top
  // tick exactly when `max` is a whole number of steps -- which is
  // always, since `max` was just rounded to one.
  for (let v = min; v <= max + step * 1e-9; v += step) ticks.push(v);
  return { min, max, ticks };
}

export interface TimeTick {
  ms: number;
  label: string;
}

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/** Candidate spacings for the time axis, shortest first. Chosen so the
 * labels land on round moments -- five past, quarter past, the hour, the
 * day -- rather than at even divisions of an arbitrary span, which is
 * what produces an axis reading 10:07, 10:34, 11:01. */
const TIME_STEPS = [
  MINUTE,
  5 * MINUTE,
  15 * MINUTE,
  30 * MINUTE,
  HOUR,
  3 * HOUR,
  6 * HOUR,
  12 * HOUR,
  DAY,
  2 * DAY,
  7 * DAY,
  14 * DAY,
  30 * DAY,
  90 * DAY,
  182 * DAY,
  365 * DAY,
];

/** How to write an instant, given how much time the whole axis covers.
 *
 * The span decides the format, not the instant: on a six-hour chart
 * every label is a time of day, on a six-month chart every label is a
 * month. Mixing the two — a date here, a clock time there — makes an
 * axis that has to be read twice. */
export function timeLabel(ms: number, spanMs: number): string {
  const date = new Date(ms);
  if (spanMs <= 2 * DAY) {
    return date.toLocaleTimeString("en-US", { hour: "numeric", minute: "2-digit" });
  }
  if (spanMs <= 120 * DAY) {
    return date.toLocaleDateString("en-US", { month: "short", day: "numeric" });
  }
  if (spanMs <= 3 * 365 * DAY) {
    return date.toLocaleDateString("en-US", { month: "short", year: "2-digit" });
  }
  return String(date.getFullYear());
}

/** Where to put the vertical gridlines.
 *
 * Ticks are placed on multiples of the step *in local time*, so a daily
 * tick falls at local midnight rather than at whatever moment is a whole
 * number of days after the window opened. `Date` handles the offset;
 * doing it in UTC would put the day boundary in the wrong place for
 * every reader west of Greenwich.
 */
export function timeTicks(startMs: number, endMs: number, targetTicks = 5): TimeTick[] {
  const span = endMs - startMs;
  if (!(span > 0)) return [];

  // The step whose tick *count* lands closest to the target, rather
  // than the first step at least as wide as `span / target`. The ladder
  // has gaps -- 1h to 3h, 30d to 90d -- and "first that fits" falls
  // through them: a six-hour window asking for five ticks takes the 3h
  // step and draws two. Ties go to the wider step, since fewer labels
  // read better than more.
  let step = TIME_STEPS[0];
  let best = Infinity;
  for (const candidate of TIME_STEPS) {
    const score = Math.abs(Math.floor(span / candidate) - targetTicks);
    if (score <= best) {
      best = score;
      step = candidate;
    }
  }

  const ticks: TimeTick[] = [];
  // Steps of a day or more align to local midnight; shorter ones align
  // to the epoch, which for sub-day steps is the same as aligning to the
  // hour in every timezone whose offset is a whole number of minutes.
  let cursor: number;
  if (step >= DAY) {
    const first = new Date(startMs);
    first.setHours(0, 0, 0, 0);
    cursor = first.getTime();
    while (cursor < startMs) cursor += step;
  } else {
    const offset = new Date(startMs).getTimezoneOffset() * MINUTE;
    cursor = Math.ceil((startMs - offset) / step) * step + offset;
  }

  // Bounded rather than trusted to terminate on the data: a pathological
  // span-to-step ratio would otherwise spin.
  for (let i = 0; cursor <= endMs && i < 64; i++, cursor += step) {
    ticks.push({ ms: cursor, label: timeLabel(cursor, span) });
  }
  return ticks;
}

/** The instant a bucket represents, taken as its **right edge**.
 *
 * A bucket covers a span, and a cumulative series' value at bucket `i`
 * is the total *by the end of* that span -- so plotting it at the
 * bucket's start would report every total one bucket early. On a
 * 24-bucket day that is an hour's error on every point. */
export function bucketTime(index: number, startMs: number, endMs: number, buckets: number): number {
  if (buckets <= 0) return startMs;
  return startMs + ((index + 1) * (endMs - startMs)) / buckets;
}

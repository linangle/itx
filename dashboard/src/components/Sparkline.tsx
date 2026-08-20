import type { Direction } from "../lib/format";

interface Props {
  values: number[];
  direction?: Direction;
  width?: number;
  height?: number;
  /** Accessible description. Sparklines carry real information, so they
   * get a label rather than being hidden from assistive tech. */
  label?: string;
}

/** A bare inline-SVG sparkline: one polyline, a faint fill beneath it,
 * and a dashed baseline.
 *
 * Inline SVG rather than a charting library on purpose. A 60x20 line
 * needs no axes, legend, tooltips or layout engine -- and hand-rolling it
 * means the two states that actually occur on a young testnet (no data,
 * and a completely flat line) are handled explicitly.
 *
 * `preserveAspectRatio="none"` lets one viewBox stretch to whatever the
 * column gives it, so every sparkline in a table shares an identical
 * coordinate system regardless of rendered width.
 */
export default function Sparkline({
  values,
  direction = "flat",
  width = 64,
  height = 20,
  label,
}: Props) {
  // Colour comes from the `.up`/`.down`/`.flat` classes via currentColor
  // rather than reading `var(--up)` directly, so a container can restyle
  // its sparklines by overriding the class colour -- the filled stat cards
  // do exactly this.

  // Nothing to draw. A dash reads as "no data" where an empty box just
  // looks like a rendering bug.
  if (values.length === 0) {
    return (
      <span className="flat" aria-label={label ?? "no data"}>
        —
      </span>
    );
  }

  const min = Math.min(...values);
  const max = Math.max(...values);
  const span = max - min;

  // A flat series (every bucket equal -- including all-zero, the common
  // case on a quiet board) has no meaningful vertical scale. Dividing by a
  // zero span would put every point at NaN, so pin it to the middle.
  const y = (value: number) =>
    span === 0 ? height / 2 : height - ((value - min) / span) * (height - 2) - 1;
  const x = (index: number) =>
    values.length === 1 ? width / 2 : (index / (values.length - 1)) * width;

  const points = values.map((value, index) => `${x(index).toFixed(2)},${y(value).toFixed(2)}`);
  const line = points.join(" ");
  const area = `${x(0).toFixed(2)},${height} ${line} ${x(values.length - 1).toFixed(2)},${height}`;

  return (
    <svg
      className={`itx-spark ${direction}`}
      width={width}
      height={height}
      viewBox={`0 0 ${width} ${height}`}
      preserveAspectRatio="none"
      role="img"
      aria-label={label ?? "activity over time"}
    >
      <line className="itx-spark-baseline" x1="0" y1={height - 1} x2={width} y2={height - 1} />
      <polygon points={area} fill="currentColor" opacity="0.12" />
      <polyline
        points={line}
        fill="none"
        stroke="currentColor"
        strokeWidth="1.25"
        strokeLinejoin="round"
        strokeLinecap="round"
        vectorEffect="non-scaling-stroke"
      />
    </svg>
  );
}

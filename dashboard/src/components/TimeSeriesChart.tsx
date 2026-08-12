import { useId, useMemo, useState } from "react";
import { bucketTime, timeTicks, valueScale } from "../lib/chartAxis";
import { formatCompactItx, formatCount } from "../lib/format";

interface Props {
  /** The line, one point per bucket, oldest first. Cumulative — this is
   * a value curve, not a bar chart of activity. */
  values: number[];
  /** The bars under it, one per bucket: how much happened *in* that
   * bucket rather than by the end of it. */
  volume: number[];
  startMs: number;
  endMs: number;
  width: number;
  height?: number;
  /** Colours the line. `up`/`down` are the site's semantic pair; `flat`
   * is for a series with no verdict, and is never green or red. */
  direction: "up" | "down" | "flat";
  /** What one volume bar counts, for the tooltip. */
  volumeNoun?: string;
}

/** Chart geometry. The gutters are where the axes live, and they are
 * fixed rather than measured: a right gutter that resized itself to its
 * widest label would move every time the hovered value changed. */
const PAD_RIGHT = 62;
const PAD_LEFT = 4;
const PAD_TOP = 10;
const AXIS_H = 22;
const VOLUME_H = 46;
const VOLUME_GAP = 10;

/** A market's history: a filled value curve over a time axis, with
 * per-bucket volume beneath it.
 *
 * Drawn at real pixel coordinates rather than in a scaled `viewBox`,
 * because a `viewBox` stretches text and stroke widths along with the
 * geometry — an 11px axis label would be 11px only at one width. That is
 * why this needs to be told how wide it is.
 *
 * Hovering reads out the bucket under the cursor. The readout is
 * deliberately *inside* the plot rather than following the cursor as a
 * floating tooltip: a tooltip that tracks the pointer covers the very
 * part of the line being inspected, and at this size there is room to
 * put the numbers somewhere they never move.
 */
export default function TimeSeriesChart({
  values,
  volume,
  startMs,
  endMs,
  width,
  height = 300,
  direction,
  volumeNoun = "tasks",
}: Props) {
  const gradientId = useId();
  const [hover, setHover] = useState<number | null>(null);

  const plotW = Math.max(0, width - PAD_LEFT - PAD_RIGHT);
  const plotH = Math.max(0, height - PAD_TOP - AXIS_H - VOLUME_H - VOLUME_GAP);

  const scale = useMemo(() => valueScale(values), [values]);
  const ticks = useMemo(() => timeTicks(startMs, endMs), [startMs, endMs]);
  const peakVolume = useMemo(() => Math.max(1, ...volume), [volume]);

  const geometry = useMemo(() => {
    if (values.length === 0 || plotW <= 0 || plotH <= 0) return null;
    const span = scale.max - scale.min || 1;
    // One point per bucket, spread across the full width. A single
    // bucket has no width to spread over and sits at the left edge,
    // where the area below still draws as a sliver rather than as NaN.
    const stepX = values.length > 1 ? plotW / (values.length - 1) : 0;
    const xs = values.map((_, i) => PAD_LEFT + i * stepX);
    const ys = values.map((v) => PAD_TOP + plotH - ((v - scale.min) / span) * plotH);
    const line = xs.map((x, i) => `${i === 0 ? "M" : "L"}${x.toFixed(1)} ${ys[i].toFixed(1)}`).join(" ");
    const base = PAD_TOP + plotH;
    const area = `${line} L${xs[xs.length - 1].toFixed(1)} ${base} L${xs[0].toFixed(1)} ${base} Z`;
    return { xs, ys, line, area, base };
  }, [values, scale, plotW, plotH]);

  if (!geometry) {
    return <div className="itx-chart-empty">not enough history to chart yet.</div>;
  }

  const yFor = (v: number) =>
    PAD_TOP + plotH - ((v - scale.min) / (scale.max - scale.min || 1)) * plotH;
  const xForTime = (ms: number) =>
    PAD_LEFT + ((ms - startMs) / (endMs - startMs || 1)) * plotW;

  const volumeTop = PAD_TOP + plotH + AXIS_H + VOLUME_GAP;
  // Bars sit in the slot between adjacent points, with a hairline of air
  // so a dense series reads as bars and not as a solid block.
  const barW = Math.max(1, plotW / Math.max(1, volume.length) - 1);

  const active = hover ?? values.length - 1;

  function onMove(event: React.MouseEvent<SVGSVGElement>) {
    const box = event.currentTarget.getBoundingClientRect();
    const x = event.clientX - box.left - PAD_LEFT;
    const index = Math.round((x / (plotW || 1)) * (values.length - 1));
    setHover(Math.max(0, Math.min(values.length - 1, index)));
  }

  return (
    <svg
      className={`itx-chart itx-chart-${direction}`}
      width={width}
      height={height}
      role="img"
      aria-label={`Value over time, ${values.length} points`}
      onMouseMove={onMove}
      onMouseLeave={() => setHover(null)}
    >
      <defs>
        <linearGradient id={gradientId} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" className="itx-chart-fill-top" />
          <stop offset="100%" className="itx-chart-fill-bottom" />
        </linearGradient>
      </defs>

      {/* Value gridlines, labelled on the right like the reference. The
          right gutter is why: labels there never collide with the line's
          left edge, which is where a series usually starts low. */}
      {scale.ticks.map((tick) => (
        <g key={tick}>
          <line
            className="itx-chart-grid"
            x1={PAD_LEFT}
            x2={PAD_LEFT + plotW}
            y1={yFor(tick)}
            y2={yFor(tick)}
          />
          <text className="itx-chart-ylabel" x={PAD_LEFT + plotW + 8} y={yFor(tick) + 3.5}>
            {formatCompactItx(tick)}
          </text>
        </g>
      ))}

      <path className="itx-chart-area" d={geometry.area} fill={`url(#${gradientId})`} />
      <path className="itx-chart-line" d={geometry.line} />

      {/* Time axis. Ticks are drawn only where there is room for the
          label: the first and last are clamped inward so neither hangs
          off the end of the chart. */}
      {ticks.map((tick) => {
        const x = xForTime(tick.ms);
        if (x < PAD_LEFT + 12 || x > PAD_LEFT + plotW - 12) return null;
        return (
          <text
            key={tick.ms}
            className="itx-chart-xlabel"
            x={x}
            y={PAD_TOP + plotH + 15}
            textAnchor="middle"
          >
            {tick.label}
          </text>
        );
      })}

      {volume.map((v, i) => {
        const h = (v / peakVolume) * VOLUME_H;
        return (
          <rect
            key={i}
            className={i === active ? "itx-chart-bar is-active" : "itx-chart-bar"}
            x={PAD_LEFT + (i * plotW) / Math.max(1, volume.length)}
            y={volumeTop + VOLUME_H - h}
            width={barW}
            height={Math.max(v > 0 ? 1 : 0, h)}
          />
        );
      })}

      {/* The crosshair, and the readout that goes with it. Only while
          hovering: at rest the chart should be a chart, not a chart with
          a line drawn across it. */}
      {hover !== null && (
        <>
          <line
            className="itx-chart-crosshair"
            x1={geometry.xs[hover]}
            x2={geometry.xs[hover]}
            y1={PAD_TOP}
            y2={volumeTop + VOLUME_H}
          />
          <circle className="itx-chart-dot" cx={geometry.xs[hover]} cy={geometry.ys[hover]} r={3.5} />
        </>
      )}

      <text className="itx-chart-readout" x={PAD_LEFT} y={PAD_TOP - 1}>
        <tspan className="itx-chart-readout-value">{formatCompactItx(values[active])} itx</tspan>
        <tspan className="itx-chart-readout-meta" dx="10">
          {formatCount(volume[active] ?? 0)} {volumeNoun}
        </tspan>
        <tspan className="itx-chart-readout-meta" dx="10">
          {new Date(
            bucketTime(active, startMs, endMs, values.length),
          ).toLocaleString("en-US", {
            month: "short",
            day: "numeric",
            hour: "numeric",
            minute: "2-digit",
          })}
        </tspan>
      </text>
    </svg>
  );
}

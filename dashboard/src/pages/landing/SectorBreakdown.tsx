import { useMemo, useState } from "react";
import { squarify } from "../../lib/treemap";
import { directionOf, formatCompactItx, formatCount, formatPct } from "../../lib/format";
import type { SectorSummary } from "../../lib/series";

/** The map is laid out in this space and positioned in percentages, so
 * it never has to be measured.
 *
 * The numbers are the map's own aspect ratio, and they have to be: the
 * squarified layout picks arrangements that keep boxes near square *for
 * the box it is given*, so computing in a square and rendering into a
 * 16:9 panel would stretch every one of those decisions sideways. The
 * stylesheet holds the panel to the same ratio (`aspect-ratio` on
 * `.itx-sectors-map`), which is what lets this be a constant instead of
 * a ResizeObserver. */
const MAP_W = 160;
const MAP_H = 90;

/** Where the colour ladder steps, in percent change. The reference's own
 * ladder runs ±3% because equity indices move in single digits over a
 * day; a task marketplace's sectors routinely double or halve over the
 * charting window, so the same seven buckets are spread over a range
 * that actually discriminates here. Stated in the legend rather than
 * left implicit -- a colour scale nobody can read the thresholds off is
 * decoration. */
const TINT_STEPS = [25, 50, 100];

/** How strongly a tile is tinted, 0 to 1, from its change. Stepped
 * rather than continuous so that two sectors of similar standing read as
 * the same colour, which is what makes the map scannable -- a smooth
 * ramp turns every tile into its own shade and the eye cannot group
 * them. */
function tint(changePct: number | null): number {
  if (changePct === null || !Number.isFinite(changePct)) return 0;
  const magnitude = Math.abs(changePct);
  if (magnitude >= TINT_STEPS[2]) return 1;
  if (magnitude >= TINT_STEPS[1]) return 0.72;
  if (magnitude >= TINT_STEPS[0]) return 0.46;
  return 0.24;
}

/** Label size from the tile's short side. Big sectors get a headline,
 * slivers get something that still fits -- the reference does the same,
 * and without it the smallest tiles have their names painted over their
 * neighbours. */
function labelScale(rect: { width: number; height: number }): number {
  const short = Math.min(rect.width, rect.height);
  return Math.max(0.5, Math.min(1.6, short / 26));
}

/** The sector breakdown, after the reference: a table of sectors by
 * weight on the left, and a treemap on the right where each sector's
 * area is its share of the board and its colour is how it is moving.
 *
 * "Weight" here is share of **value on offer** -- open bounty -- which
 * is the same quantity the sectors are ranked by in the carousel above,
 * so a sector that leads the board also has the biggest tile. It is
 * deliberately not the quantity the *colour* shows: the tint comes from
 * the sector's change, which is its posting flow period over period.
 * Size is how much is there; colour is which way it is going. */
export default function SectorBreakdown({ sectors }: { sectors: SectorSummary[] }) {
  /** The picked-out sector, or none. Set by *clicking* a row or a tile,
   * and by nothing else.
   *
   * An earlier version also set it on hover, which looked like a free
   * improvement and was a bug: moving the pointer onto a row selected
   * it, so the click that followed found it already selected and toggled
   * it straight back off. Two mechanisms owning one piece of state, and
   * the more discoverable of the two silently cancelling the other. */
  const [selected, setSelected] = useState<string | null>(null);

  const totalBounty = useMemo(
    () => sectors.reduce((sum, s) => sum + s.openBounty, 0),
    [sectors],
  );

  const tiles = useMemo(
    () => squarify(sectors, (s) => s.openBounty, MAP_W, MAP_H),
    [sectors],
  );

  if (sectors.length === 0) return null;

  return (
    <section className="itx-sectors" aria-label="Sector breakdown">
      <div className="itx-board-labels">
        <span className="itx-board-label">sectors</span>
      </div>

      <div className="itx-sectors-panel itx-board-panel">
        <div className="itx-sectors-table">
          <p className="itx-sectors-head">select a sector for a visual breakdown</p>
          <table className="itx-board-table">
            <thead>
              <tr>
                <th>sector</th>
                <th>weight</th>
                <th className="right">change</th>
              </tr>
            </thead>
            <tbody>
              {sectors.map((s) => {
                const weight = totalBounty > 0 ? s.openBounty / totalBounty : 0;
                return (
                  <tr
                    key={s.name}
                    className={selected === s.name ? "is-selected" : undefined}
                  >
                    <td className="itx-board-cell-market">
                      <button
                        type="button"
                        onClick={() => setSelected(selected === s.name ? null : s.name)}
                        title={`${formatCount(s.open)} open · ${formatCompactItx(s.openBounty)} itx`}
                      >
                        {s.name}
                      </button>
                    </td>
                    <td>
                      {/* The bar and the figure are one cell: the number
                          is what the bar means, and splitting them put a
                          column boundary between a value and its own
                          label. */}
                      <span className="itx-sectors-weight">
                        <span className="itx-sectors-bar" aria-hidden="true">
                          <span style={{ width: `${(weight * 100).toFixed(1)}%` }} />
                        </span>
                        <span className="itx-sectors-pct">{(weight * 100).toFixed(1)}%</span>
                      </span>
                    </td>
                    <td className={`right itx-board-cell-pct ${directionOf(s.changePct)}`}>
                      {formatPct(s.changePct)}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>

        <div className="itx-sectors-side">
          <p className="itx-sectors-head">all sectors</p>
          <div className="itx-sectors-map">
            {tiles.map(({ item, rect }) => (
              <div
                key={item.name}
                className="itx-sectors-tile"
                data-dir={directionOf(item.changePct)}
                data-dim={selected !== null && selected !== item.name ? "" : undefined}
                data-on={selected === item.name ? "" : undefined}
                style={{
                  left: `${(rect.x / MAP_W) * 100}%`,
                  top: `${(rect.y / MAP_H) * 100}%`,
                  width: `${(rect.width / MAP_W) * 100}%`,
                  height: `${(rect.height / MAP_H) * 100}%`,
                  ["--tile-tint" as string]: tint(item.changePct),
                  ["--tile-scale" as string]: labelScale(rect),
                }}
                onClick={() => setSelected(selected === item.name ? null : item.name)}
              >
                <span className="itx-sectors-tile-name">{item.name}</span>
                <span className="itx-sectors-tile-pct">{formatPct(item.changePct)}</span>
              </div>
            ))}
          </div>

          {/* The ladder, so the colours can actually be read rather than
              merely felt. */}
          <div className="itx-sectors-legend" aria-hidden="true">
            <span>≤ −{TINT_STEPS[2]}%</span>
            <i data-dir="down" style={{ ["--tile-tint" as string]: 1 }} />
            <i data-dir="down" style={{ ["--tile-tint" as string]: 0.72 }} />
            <i data-dir="down" style={{ ["--tile-tint" as string]: 0.46 }} />
            <i data-dir="flat" style={{ ["--tile-tint" as string]: 0.24 }} />
            <i data-dir="up" style={{ ["--tile-tint" as string]: 0.46 }} />
            <i data-dir="up" style={{ ["--tile-tint" as string]: 0.72 }} />
            <i data-dir="up" style={{ ["--tile-tint" as string]: 1 }} />
            <span>≥ +{TINT_STEPS[2]}%</span>
          </div>
        </div>
      </div>
    </section>
  );
}

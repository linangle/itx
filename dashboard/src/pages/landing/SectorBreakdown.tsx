import { useMemo, useState } from "react";
import { squarify } from "../../lib/treemap";
import type { TreemapRect } from "../../lib/treemap";
import { directionOf, formatCompactItx, formatCount, formatPct } from "../../lib/format";
import type { MarketSummary, SectorSummary } from "../../lib/series";

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

/** A tile's short side as a fraction of the map's *rendered width*, for
 * sizing its label.
 *
 * The units are the point. An earlier version scaled the label from the
 * short side in layout units, which are fixed -- so the type stayed the
 * same size as the map shrank, and by the time the window was narrow the
 * names had outgrown their tiles and were being clipped mid-word. This
 * is a ratio instead, and the stylesheet multiplies it by `cqw`, so the
 * label is always the same fraction of the box it sits in and scales
 * with it continuously.
 *
 * Height is converted into width units before the comparison, because
 * `cqw` is the only container unit in play: a tile's rendered height is
 * its share of `MAP_H` times the map's height, and the map's height is
 * its width times the aspect ratio. */
function labelScale(rect: { width: number; height: number }): number {
  const wide = rect.width / MAP_W;
  const tall = (rect.height / MAP_H) * (MAP_H / MAP_W);
  return Math.min(wide, tall);
}

/** How much of a tile's label it has room for.
 *
 * A sector map has six boxes and they all fit. A sector's *markets* do
 * not: the long tail of a sector is a row of slivers, and a name set in
 * one of them is either three ellipsised letters or a word overflowing
 * its own box -- both of which read as a rendering fault rather than as
 * a small market. Below the thresholds the tile carries no text at all
 * and is just a shape; every tile names itself on hover regardless, so
 * nothing is only available to the tiles that happen to be big. */
function labelDetail(scale: number): "full" | "name" | "none" {
  if (scale >= 0.1) return "full";
  if (scale >= 0.055) return "name";
  return "none";
}

/** Which of the map's own corners a tile sits in.
 *
 * The map is a rounded box that clips what it contains, so a tile in a
 * corner has its square corner cut away by that curve -- and with it
 * whatever is drawn along the tile's edge, which is how the selection
 * ring came to be sliced through at the corner. Rounding the tile to
 * the same radius on the same corner puts its edge back inside the clip
 * where it can be seen. Only the corners that are genuinely flush are
 * rounded: an interior tile that merely ends near the edge must stay
 * square, or the map grows gaps along its own seams. */
function corners(rect: TreemapRect): string {
  const EPS = 0.01;
  const r = "var(--r-field)";
  const left = rect.x <= EPS;
  const top = rect.y <= EPS;
  const right = rect.x + rect.width >= MAP_W - EPS;
  const bottom = rect.y + rect.height >= MAP_H - EPS;
  return [
    top && left ? r : "0",
    top && right ? r : "0",
    bottom && right ? r : "0",
    bottom && left ? r : "0",
  ].join(" ");
}

/** What a sector's size is read off, best first.
 *
 * Open bounty is the quantity this panel wants: value on offer right
 * now, the same one the carousel ranks by, so a sector that leads the
 * board also has the biggest tile. It has one failure mode, and it is
 * not rare -- a board where every task has settled has *no* open bounty
 * at all, and every sector weighs zero. The table then reads 0.0% down
 * the column and the map goes blank, because `squarify` drops
 * zero-valued items rather than laying out boxes with no area. Nothing
 * is broken at that moment and the panel looks broken, which is worse
 * than showing the second-best number.
 *
 * So the ladder falls back to flow -- bounty *posted* over the charting
 * window, which is what the change column already trades in -- and then
 * to tasks posted, for a board carrying work with no bounty on it. A
 * basis wins when at least two sectors have something in it, not merely
 * one: a settling board passes through a state where a single open task
 * holds all the open bounty on the board, and weighing by it there
 * gives one sector 100%, everything else 0.0%, and a map with one tile
 * in it -- a breakdown that breaks down nothing. Two is the smallest
 * number that is a comparison. */
const SECTOR_BASES = [
  (s: SectorSummary) => s.openBounty,
  (s: SectorSummary) => s.markets.reduce((sum, m) => sum + m.value, 0),
  (s: SectorSummary) => s.posted,
];

/** The same ladder one level down, for the markets inside a sector.
 * Same reasoning, same order: on offer, then flow, then task count. */
const MARKET_BASES = [
  (m: MarketSummary) => m.openBounty,
  (m: MarketSummary) => m.value,
  (m: MarketSummary) => m.open,
];

/** The first rung with something on it, and what it adds up to. */
function pickBasis<T>(items: T[], bases: ((item: T) => number)[]) {
  const positive = (of: (item: T) => number) => items.map((i) => Math.max(0, of(i)));
  const sum = (values: number[]) => values.reduce((acc, v) => acc + v, 0);
  // A one-item board cannot have two contributors, and there is nothing
  // to compare it against anyway, so it only needs one.
  const enough = Math.min(2, items.length);

  for (const of of bases) {
    const values = positive(of);
    if (sum(values) > 0 && values.filter((v) => v > 0).length >= enough) {
      return { of, total: sum(values) };
    }
  }
  // Nothing clears the two-item bar. Take whatever has a number in it
  // rather than nothing -- one tile still beats an empty map -- and only
  // then give up and say there is nothing to weigh.
  for (const of of bases) {
    const total = sum(positive(of));
    if (total > 0) return { of, total };
  }
  return { of: bases[bases.length - 1], total: 0 };
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
  /** The sector the map is opened into, set by *clicking* a row or a
   * tile.
   *
   * Kept strictly apart from what the pointer is over. An earlier
   * version had hover write to this same field, which looked like a free
   * improvement and was a bug: moving the pointer onto a row opened it,
   * so the click that followed found it already open and shut it again.
   * Two mechanisms owning one piece of state, and the more discoverable
   * of the two silently cancelling the other. Hovering picks a sector
   * out; clicking goes into it; neither can undo the other. */
  const [selected, setSelected] = useState<string | null>(null);

  /** The sector under the pointer, which is a preview and nothing more.
   * Named on both sides of the panel, so running down the table lights
   * up the map and vice versa. */
  const [hovered, setHovered] = useState<string | null>(null);

  /** The basis the table is weighing by, and what it adds up to. `total`
   * can still be zero -- an all-zero board is weightless however it is
   * measured, and the map says so rather than rendering an empty box. */
  const { of: basis, total } = useMemo(() => pickBasis(sectors, SECTOR_BASES), [sectors]);

  /** Rows follow the weight actually on screen. They arrive ranked by
   * open bounty, which is the right order right up until that is the
   * number that ran out -- and a column of descending percentages that
   * suddenly is not descending reads as a sorting bug. */
  const rows = useMemo(
    () => [...sectors].sort((a, b) => basis(b) - basis(a)),
    [sectors, basis],
  );

  /** The sector the map is inside, if any. Held by name rather than by
   * reference because the board reloads underneath: the sector the
   * reader picked is the one with that name in whatever arrived last,
   * not the object that was on screen when they clicked. */
  const inside = useMemo(
    () => sectors.find((s) => s.name === selected) ?? null,
    [sectors, selected],
  );

  /** One level or the other, laid out the same way. Inside a sector the
   * boxes are its markets, weighed against each other rather than
   * against the board -- a sector's own breakdown should fill its own
   * map however small the sector is. */
  const tiles = useMemo(() => {
    if (inside) {
      const { of } = pickBasis(inside.markets, MARKET_BASES);
      return squarify(inside.markets, of, MAP_W, MAP_H).map(({ item, rect }) => ({
        key: item.capability,
        name: item.capability,
        changePct: item.changePct,
        hint: `${formatCount(item.open)} open · ${formatCompactItx(item.value)} itx`,
        rect,
      }));
    }
    return squarify(sectors, basis, MAP_W, MAP_H).map(({ item, rect }) => ({
      key: item.name,
      name: item.name,
      changePct: item.changePct,
      hint: `${formatCount(item.open)} open · ${formatCompactItx(item.openBounty)} itx`,
      rect,
    }));
  }, [sectors, basis, inside]);

  if (sectors.length === 0) return null;

  return (
    <section className="itx-sectors" aria-label="Sector breakdown">
      <div className="itx-board-labels">
        <span className="itx-board-label">sectors</span>
      </div>

      {/* The jump link's target is the panel, not the section: every
          section on the board parks its *panel* level with the
          leaderboard's, so the page looks the same however you arrived
          at it. The label above stays clear of the masthead because the
          offset that does the parking is a label's height taller than
          the bar -- see `--anchor-top`. */}
      <div className="itx-sectors-panel itx-board-panel" id="itx-board-sectors">
        <div className="itx-sectors-table">
          <table className="itx-board-table">
            <thead>
              <tr>
                <th>sector</th>
                <th>weight</th>
                <th className="right">change</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((s) => {
                const weight = total > 0 ? Math.max(0, basis(s)) / total : 0;
                return (
                  <tr
                    key={s.name}
                    className={selected === s.name ? "is-selected" : undefined}
                    /* The whole row is the target, not just the name:
                       reading across to the weight bar should not put
                       the map out again halfway. */
                    onMouseEnter={() => setHovered(s.name)}
                    onMouseLeave={() => setHovered((h) => (h === s.name ? null : h))}
                  >
                    <td className="itx-board-cell-market">
                      <button
                        type="button"
                        onClick={() => setSelected(selected === s.name ? null : s.name)}
                        /* Keyboard gets the same preview the pointer
                           does -- tabbing the table lights the map. */
                        onFocus={() => setHovered(s.name)}
                        onBlur={() => setHovered((h) => (h === s.name ? null : h))}
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
          {/* Inside a sector the head is the way back out, so the map
              can be left without hunting for the row that opened it. */}
          <p className="itx-sectors-head">
            {inside ? (
              <>
                <button
                  type="button"
                  className="itx-sectors-back"
                  onClick={() => setSelected(null)}
                >
                  ‹ all sectors
                </button>
                {inside.name}
              </>
            ) : (
              "all sectors"
            )}
          </p>
          <div className="itx-sectors-map" data-level={inside ? "markets" : "sectors"}>
            {/* A board with nothing to weigh says so inside the map. The
                alternative is an empty bordered box, which is
                indistinguishable from a panel that failed to load. */}
            {tiles.length === 0 && (
              <p className="itx-sectors-map-empty">nothing to map yet</p>
            )}
            {tiles.map(({ key, name, changePct, hint, rect }) => {
              const detail = labelDetail(labelScale(rect));
              // Picking one out only means anything at the sector level;
              // inside a sector the tiles are already one sector's worth
              // and there is nothing to pick it out from.
              const on = !inside && hovered === name;
              const off = !inside && hovered !== null && !on;
              return (
                <div
                  key={key}
                  className="itx-sectors-tile"
                  data-dir={directionOf(changePct)}
                  data-on={on ? "" : undefined}
                  /* Dimmed, not hidden: the map is a comparison, and a
                     tile that vanishes takes its own context with it. */
                  data-dim={off ? "" : undefined}
                  onMouseEnter={inside ? undefined : () => setHovered(name)}
                  onMouseLeave={
                    inside ? undefined : () => setHovered((h) => (h === name ? null : h))
                  }
                  style={{
                    left: `${(rect.x / MAP_W) * 100}%`,
                    top: `${(rect.y / MAP_H) * 100}%`,
                    width: `${(rect.width / MAP_W) * 100}%`,
                    height: `${(rect.height / MAP_H) * 100}%`,
                    borderRadius: corners(rect),
                    ["--tile-tint" as string]: tint(changePct),
                    ["--tile-scale" as string]: labelScale(rect),
                  }}
                  /* Every tile names itself on hover, which is what makes
                     it safe for the small ones to carry no text. */
                  title={`${name} · ${formatPct(changePct)} · ${hint}`}
                  onClick={
                    inside
                      ? undefined
                      : () => {
                          setSelected(selected === name ? null : name);
                          // The tile is about to be replaced by the
                          // sector's markets, so its own mouseleave may
                          // never arrive; letting the name stand would
                          // dim the map on the way back out.
                          setHovered(null);
                        }
                  }
                >
                  {detail !== "none" && (
                    <span className="itx-sectors-tile-name">{name}</span>
                  )}
                  {detail === "full" && (
                    <span className="itx-sectors-tile-pct">{formatPct(changePct)}</span>
                  )}
                </div>
              );
            })}
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

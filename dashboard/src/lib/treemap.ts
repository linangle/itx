// Squarified treemap layout.
//
// Tiles a rectangle with one box per item, each box's *area* proportional
// to its value, choosing arrangements that keep boxes close to square.
// The squarified variant (Bruls, Huizing & van Wijk, 2000) exists
// because the naive "slice and dice" alternative produces slivers: with
// one large value and several small ones it lays the small ones out as
// hairlines, which carry their number nowhere near legibly and read as
// rounding error rather than as data.
//
// Pure geometry -- numbers in, rectangles out, no DOM and no React, like
// the rest of `lib/`. Which is also what makes the awkward parts
// testable: that the areas really are proportional, that nothing
// overlaps, and that the boxes exactly fill the rectangle they were
// given rather than leaving a seam at one edge.

export interface TreemapRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface TreemapTile<T> {
  item: T;
  rect: TreemapRect;
}

/** How far from square the worst box in a row is, given the side it is
 * laid along. Lower is better; 1 would be a perfect square. This is the
 * quantity the algorithm is named for -- a row is extended for as long
 * as adding to it makes its worst box *less* lopsided. */
function worstAspect(areas: number[], side: number): number {
  if (areas.length === 0) return Infinity;
  let sum = 0;
  let max = -Infinity;
  let min = Infinity;
  for (const area of areas) {
    sum += area;
    if (area > max) max = area;
    if (area < min) min = area;
  }
  if (sum <= 0 || side <= 0) return Infinity;
  const sum2 = sum * sum;
  const side2 = side * side;
  // A zero-valued item can never be squarer, and dividing by it would
  // hand back Infinity for the whole row.
  if (min <= 0) return Infinity;
  return Math.max((side2 * max) / sum2, sum2 / (side2 * min));
}

/** Tiles `width` x `height` with one rectangle per item.
 *
 * Items are laid largest first, which is what the algorithm assumes --
 * the caller's order is not preserved, and the returned tiles carry
 * their item so the caller can find its own again.
 *
 * Items with no value are dropped rather than laid out at zero size: a
 * box with no area cannot be labelled, and leaving it in only puts an
 * invisible tile in the middle of the map for a pointer to find. A
 * caller that needs to say "this sector has nothing in it" should say so
 * outside the map.
 */
export function squarify<T>(
  items: T[],
  valueOf: (item: T) => number,
  width: number,
  height: number,
): TreemapTile<T>[] {
  const scored = items
    .map((item) => ({ item, value: Math.max(0, valueOf(item)) }))
    .filter((entry) => entry.value > 0)
    .sort((a, b) => b.value - a.value);

  const total = scored.reduce((sum, entry) => sum + entry.value, 0);
  if (scored.length === 0 || total <= 0 || width <= 0 || height <= 0) return [];

  // Values become areas, so a row's thickness is just its area over the
  // side it runs along.
  const scale = (width * height) / total;
  const areas = scored.map((entry) => entry.value * scale);

  const tiles: TreemapTile<T>[] = [];
  // The rectangle still to be filled, shrinking as rows are laid.
  let x = 0;
  let y = 0;
  let free = { width, height };

  let index = 0;
  let row: number[] = [];
  let rowStart = 0;

  /** Lay the current row along the shorter side of what is free, and
   * take that strip out of the free rectangle. */
  const flushRow = () => {
    const rowArea = row.reduce((sum, area) => sum + area, 0);
    if (rowArea <= 0) {
      row = [];
      return;
    }
    const vertical = free.width >= free.height;
    // Thickness of the strip: its area spread along the side it runs on.
    const thickness = vertical ? rowArea / free.height : rowArea / free.width;
    let offset = 0;
    row.forEach((area, i) => {
      const along = vertical ? area / thickness : area / thickness;
      tiles.push({
        item: scored[rowStart + i].item,
        rect: vertical
          ? { x, y: y + offset, width: thickness, height: along }
          : { x: x + offset, y, width: along, height: thickness },
      });
      offset += along;
    });
    if (vertical) {
      x += thickness;
      free = { width: free.width - thickness, height: free.height };
    } else {
      y += thickness;
      free = { width: free.width, height: free.height - thickness };
    }
    rowStart += row.length;
    row = [];
  };

  while (index < areas.length) {
    const side = Math.min(free.width, free.height);
    const next = areas[index];
    // Extend the row while doing so makes its worst box squarer; the
    // moment it would make it worse, the row is finished.
    if (row.length === 0 || worstAspect([...row, next], side) <= worstAspect(row, side)) {
      row.push(next);
      index += 1;
    } else {
      flushRow();
    }
  }
  flushRow();

  return tiles;
}

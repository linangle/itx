import { describe, expect, it } from "vitest";
import { squarify } from "./treemap";

interface Item {
  name: string;
  weight: number;
}

const value = (i: Item) => i.weight;

/** Total area covered, and whether any two tiles overlap. Both are
 * properties the layout must hold for *any* input, which is what makes
 * them worth asserting instead of pinning exact coordinates -- those
 * would only re-state whatever the implementation happened to do. */
function coverage(tiles: { rect: { x: number; y: number; width: number; height: number } }[]) {
  let area = 0;
  let overlaps = 0;
  for (let i = 0; i < tiles.length; i++) {
    const a = tiles[i].rect;
    area += a.width * a.height;
    for (let j = i + 1; j < tiles.length; j++) {
      const b = tiles[j].rect;
      const wide = Math.min(a.x + a.width, b.x + b.width) - Math.max(a.x, b.x);
      const tall = Math.min(a.y + a.height, b.y + b.height) - Math.max(a.y, b.y);
      if (wide > 0.001 && tall > 0.001) overlaps++;
    }
  }
  return { area, overlaps };
}

describe("squarify", () => {
  const sectors: Item[] = [
    { name: "coding", weight: 40 },
    { name: "data", weight: 25 },
    { name: "conversation", weight: 15 },
    { name: "creative", weight: 10 },
    { name: "automation", weight: 6 },
    { name: "research", weight: 4 },
  ];

  it("gives every item a box whose area is its share of the whole", () => {
    const tiles = squarify(sectors, value, 600, 400);
    const total = 600 * 400;
    for (const tile of tiles) {
      const share = tile.item.weight / 100;
      expect(tile.rect.width * tile.rect.height).toBeCloseTo(total * share, 3);
    }
  });

  it("fills the rectangle exactly, leaving no seam", () => {
    const tiles = squarify(sectors, value, 600, 400);
    const { area, overlaps } = coverage(tiles);
    expect(area).toBeCloseTo(600 * 400, 3);
    expect(overlaps).toBe(0);
  });

  it("keeps every box inside the bounds it was given", () => {
    const tiles = squarify(sectors, value, 600, 400);
    for (const { rect } of tiles) {
      expect(rect.x).toBeGreaterThanOrEqual(-0.001);
      expect(rect.y).toBeGreaterThanOrEqual(-0.001);
      expect(rect.x + rect.width).toBeLessThanOrEqual(600.001);
      expect(rect.y + rect.height).toBeLessThanOrEqual(400.001);
    }
  });

  it("lays the largest item first", () => {
    const tiles = squarify(sectors, value, 600, 400);
    expect(tiles[0].item.name).toBe("coding");
  });

  it("keeps boxes near square rather than slicing them into slivers", () => {
    // The whole point of squarifying. Slice-and-dice on this input would
    // give the 4% sector a box 16px wide and 400 tall (aspect 25); the
    // squarified layout keeps every box within a far tighter ratio, and
    // a box that shape cannot carry a label.
    const tiles = squarify(sectors, value, 600, 400);
    for (const { rect } of tiles) {
      const aspect = Math.max(rect.width / rect.height, rect.height / rect.width);
      expect(aspect).toBeLessThan(4);
    }
  });

  it("drops items with no value rather than laying out invisible boxes", () => {
    const tiles = squarify(
      [...sectors, { name: "empty", weight: 0 }],
      value,
      600,
      400,
    );
    expect(tiles.map((t) => t.item.name)).not.toContain("empty");
    expect(tiles).toHaveLength(sectors.length);
  });

  it("handles a single item by handing it the whole rectangle", () => {
    const tiles = squarify([{ name: "only", weight: 5 }], value, 300, 200);
    expect(tiles).toHaveLength(1);
    expect(tiles[0].rect).toEqual({ x: 0, y: 0, width: 300, height: 200 });
  });

  it("returns nothing for an empty board or a zero-sized box", () => {
    expect(squarify([], value, 600, 400)).toEqual([]);
    expect(squarify(sectors, value, 0, 400)).toEqual([]);
    expect(squarify(sectors, value, 600, 0)).toEqual([]);
  });

  it("survives one item dwarfing the rest", () => {
    const lopsided: Item[] = [
      { name: "huge", weight: 1000 },
      { name: "tiny", weight: 1 },
      { name: "tinier", weight: 0.5 },
    ];
    const tiles = squarify(lopsided, value, 600, 400);
    const { area, overlaps } = coverage(tiles);
    expect(tiles).toHaveLength(3);
    expect(overlaps).toBe(0);
    expect(area).toBeCloseTo(600 * 400, 3);
  });
});

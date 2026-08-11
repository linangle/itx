// Compiles `assets/profiles/**.svg` into `src/lib/profileAssets.gen.ts`.
//
//   node scripts/build-profile-assets.mjs
//
// Why a build step instead of importing the SVGs directly: the exports
// are Illustrator artboards, and every piece sits wherever it happened
// to be drawn on its own 4000x4000 sheet -- the three mouths are at
// three different spots, each animal has a different origin, nothing
// shares a coordinate space. Composing an icon therefore needs each
// piece's real bounding box, which SVG-as-image can't provide and
// node has no DOM to measure. This script measures geometry by
// sampling the path data itself and emits one TypeScript module with
// the markup and the numbers, so the component can lay pieces out with
// plain transforms.
//
// Colours are rewritten to CSS custom properties at compile time:
// light fills (the animal's body) become `var(--pi-body)`, dark fills
// (the pig's built-in snout, every eye/mouth/accessory) become
// `var(--pi-dark)`. The component sets both variables; nothing at
// runtime ever parses or rewrites markup.
//
// Rerun after adding or editing an asset. The output is committed, so
// the dashboard build never depends on this having run.

import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join, basename } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const SRC = join(ROOT, "assets", "profiles");
const OUT = join(ROOT, "dashboard", "src", "lib", "profileAssets.gen.ts");

// ---------------------------------------------------------------- bbox
//
// A numeric sampler rather than an analytic solver: curves are walked
// at 32 steps and the extremes taken. At artboard scale (4000 units)
// that is accurate to well under a pixel of the rendered icon, and it
// keeps the parser simple enough to trust.

const CURVE_STEPS = 32;

class Box {
  constructor() {
    this.minX = Infinity;
    this.minY = Infinity;
    this.maxX = -Infinity;
    this.maxY = -Infinity;
  }
  add(x, y) {
    if (x < this.minX) this.minX = x;
    if (y < this.minY) this.minY = y;
    if (x > this.maxX) this.maxX = x;
    if (y > this.maxY) this.maxY = y;
  }
  get valid() {
    return this.minX !== Infinity;
  }
}

function cubicAt(p0, p1, p2, p3, t) {
  const u = 1 - t;
  return u * u * u * p0 + 3 * u * u * t * p1 + 3 * u * t * t * p2 + t * t * t * p3;
}

function quadAt(p0, p1, p2, t) {
  const u = 1 - t;
  return u * u * p0 + 2 * u * t * p1 + t * t * p2;
}

/** Walks one `d` attribute, adding sampled points to `box`. Supports
 * the commands Illustrator actually emits (M L H V C S Q T Z, both
 * cases). An arc command would mean an asset drawn differently from
 * everything so far -- fail loudly rather than guess. */
function samplePath(d, box) {
  const tokens = d.match(/[MmLlHhVvCcSsQqTtAaZz]|-?\d*\.?\d+(?:e[+-]?\d+)?/g) ?? [];
  let i = 0;
  const num = () => parseFloat(tokens[i++]);
  let cmd = "";
  let x = 0, y = 0;       // current point
  let sx = 0, sy = 0;     // subpath start
  let cx = null, cy = null; // last cubic control, for S
  let qx = null, qy = null; // last quad control, for T

  while (i < tokens.length) {
    if (/^[A-Za-z]$/.test(tokens[i])) cmd = tokens[i++];
    const rel = cmd === cmd.toLowerCase();
    switch (cmd.toUpperCase()) {
      case "M": {
        const nx = num() + (rel ? x : 0);
        const ny = num() + (rel ? y : 0);
        x = sx = nx; y = sy = ny;
        box.add(x, y);
        // Subsequent pairs are implicit linetos.
        cmd = rel ? "l" : "L";
        cx = cy = qx = qy = null;
        break;
      }
      case "L": {
        x = num() + (rel ? x : 0);
        y = num() + (rel ? y : 0);
        box.add(x, y);
        cx = cy = qx = qy = null;
        break;
      }
      case "H": {
        x = num() + (rel ? x : 0);
        box.add(x, y);
        cx = cy = qx = qy = null;
        break;
      }
      case "V": {
        y = num() + (rel ? y : 0);
        box.add(x, y);
        cx = cy = qx = qy = null;
        break;
      }
      case "C": {
        const x1 = num() + (rel ? x : 0), y1 = num() + (rel ? y : 0);
        const x2 = num() + (rel ? x : 0), y2 = num() + (rel ? y : 0);
        const x3 = num() + (rel ? x : 0), y3 = num() + (rel ? y : 0);
        for (let s = 1; s <= CURVE_STEPS; s++) {
          const t = s / CURVE_STEPS;
          box.add(cubicAt(x, x1, x2, x3, t), cubicAt(y, y1, y2, y3, t));
        }
        cx = x2; cy = y2; qx = qy = null;
        x = x3; y = y3;
        break;
      }
      case "S": {
        // Reflected first control point, per the spec: the previous
        // cubic's second control mirrored through the current point.
        const x1 = cx !== null ? 2 * x - cx : x;
        const y1 = cy !== null ? 2 * y - cy : y;
        const x2 = num() + (rel ? x : 0), y2 = num() + (rel ? y : 0);
        const x3 = num() + (rel ? x : 0), y3 = num() + (rel ? y : 0);
        for (let s = 1; s <= CURVE_STEPS; s++) {
          const t = s / CURVE_STEPS;
          box.add(cubicAt(x, x1, x2, x3, t), cubicAt(y, y1, y2, y3, t));
        }
        cx = x2; cy = y2; qx = qy = null;
        x = x3; y = y3;
        break;
      }
      case "Q": {
        const x1 = num() + (rel ? x : 0), y1 = num() + (rel ? y : 0);
        const x2 = num() + (rel ? x : 0), y2 = num() + (rel ? y : 0);
        for (let s = 1; s <= CURVE_STEPS; s++) {
          const t = s / CURVE_STEPS;
          box.add(quadAt(x, x1, x2, t), quadAt(y, y1, y2, t));
        }
        qx = x1; qy = y1; cx = cy = null;
        x = x2; y = y2;
        break;
      }
      case "T": {
        const x1 = qx !== null ? 2 * x - qx : x;
        const y1 = qy !== null ? 2 * y - qy : y;
        const x2 = num() + (rel ? x : 0), y2 = num() + (rel ? y : 0);
        for (let s = 1; s <= CURVE_STEPS; s++) {
          const t = s / CURVE_STEPS;
          box.add(quadAt(x, x1, x2, t), quadAt(y, y1, y2, t));
        }
        qx = x1; qy = y1; cx = cy = null;
        x = x2; y = y2;
        break;
      }
      case "Z": {
        x = sx; y = sy;
        cx = cy = qx = qy = null;
        break;
      }
      case "A":
        throw new Error("arc command in path data; teach the sampler about arcs first");
      default:
        throw new Error(`unhandled path command ${cmd}`);
    }
  }
}

// ---------------------------------------------------------- extraction

/** Fills darker than this are "features" (drawn in the site's dark
 * ink); anything else is the animal's body. The assets use #82c6f7 for
 * bodies and #19161a/#181519/#18161b for ink -- nothing near the line. */
function isDark(hex) {
  const v = hex.replace("#", "");
  const r = parseInt(v.slice(0, 2), 16);
  const g = parseInt(v.slice(2, 4), 16);
  const b = parseInt(v.slice(4, 6), 16);
  return (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255 < 0.5;
}

/** One exported SVG -> { body, bbox }. `body` is the drawable elements
 * only, classes resolved to role variables, everything else stripped. */
function compile(file) {
  const raw = readFileSync(file, "utf8");

  // class -> fill, from the export's own <style> block.
  const fills = {};
  for (const m of raw.matchAll(/\.(st\d+)\s*\{\s*fill:\s*(#[0-9a-fA-F]{6})/g)) {
    fills[m[1]] = m[2];
  }

  const box = new Box();
  const parts = [];
  // The exports contain only these element kinds; anything new should
  // be noticed, not silently dropped.
  const elements = raw.match(/<(path|circle|ellipse|rect)\b[^>]*\/?>/g) ?? [];
  for (const el of elements) {
    const kind = el.match(/^<(\w+)/)[1];
    const attr = (name) => {
      const m = el.match(new RegExp(`${name}="([^"]*)"`));
      return m ? m[1] : null;
    };

    if (kind === "path") {
      samplePath(attr("d"), box);
    } else if (kind === "circle") {
      const cx = parseFloat(attr("cx") ?? "0");
      const cy = parseFloat(attr("cy") ?? "0");
      const r = parseFloat(attr("r") ?? "0");
      box.add(cx - r, cy - r);
      box.add(cx + r, cy + r);
    } else if (kind === "ellipse") {
      const cx = parseFloat(attr("cx") ?? "0");
      const cy = parseFloat(attr("cy") ?? "0");
      box.add(cx - parseFloat(attr("rx") ?? "0"), cy - parseFloat(attr("ry") ?? "0"));
      box.add(cx + parseFloat(attr("rx") ?? "0"), cy + parseFloat(attr("ry") ?? "0"));
    } else if (kind === "rect") {
      const rx = parseFloat(attr("x") ?? "0");
      const ry = parseFloat(attr("y") ?? "0");
      box.add(rx, ry);
      box.add(rx + parseFloat(attr("width") ?? "0"), ry + parseFloat(attr("height") ?? "0"));
    }

    const cls = attr("class");
    const fill = cls && fills[cls] ? fills[cls] : "#19161a";
    const role = isDark(fill) ? "var(--pi-dark)" : "var(--pi-body)";
    const cleaned = el
      .replace(/\s*class="[^"]*"/, "")
      .replace(/^<(\w+)/, `<$1 fill="${role}"`);
    parts.push(cleaned);
  }

  if (!box.valid) throw new Error(`${file}: no drawable elements found`);
  return {
    body: parts.join(""),
    bbox: {
      x: Math.round(box.minX),
      y: Math.round(box.minY),
      w: Math.round(box.maxX - box.minX),
      h: Math.round(box.maxY - box.minY),
    },
  };
}

// ------------------------------------------------------------- output

const KINDS = ["animals", "eyes", "mouths", "accessories"];
const pieces = {};
for (const kind of KINDS) {
  pieces[kind] = {};
  const dir = join(SRC, kind);
  for (const f of readdirSync(dir).filter((f) => f.endsWith(".svg")).sort()) {
    pieces[kind][basename(f, ".svg")] = compile(join(dir, f));
  }
}

const counts = KINDS.map((k) => `${Object.keys(pieces[k]).length} ${k}`).join(", ");
const ts = `// GENERATED by dashboard/scripts/build-profile-assets.mjs -- do not edit.
// Source of truth is assets/profiles/; rerun the script after changing it.
// ${counts}.
//
// Each piece is SVG markup in its export's own 4000x4000 coordinate
// space with fills rewritten to var(--pi-body) / var(--pi-dark), plus
// the measured bounding box of its artwork. Pieces do NOT share a
// coordinate space -- see the generator's header comment -- which is
// why every consumer positions them via bbox, never by trusting the
// artboard.

export interface PieceBBox {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface Piece {
  body: string;
  bbox: PieceBBox;
}

${KINDS.map(
  (k) =>
    `export const ${k.toUpperCase()}: Record<string, Piece> = ${JSON.stringify(
      pieces[k],
      null,
      2,
    )};`,
).join("\n\n")}
`;

writeFileSync(OUT, ts);
console.log(`wrote ${OUT}: ${counts}`);

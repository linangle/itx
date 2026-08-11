import type { CSSProperties } from "react";
import { iconSpec } from "../lib/profileIcon";
import type { IconSpec } from "../lib/profileIcon";
import { ICON_DARK } from "../lib/profileIcon";
import { ACCESSORIES, ANIMALS, EYES, MOUTHS } from "../lib/profileAssets.gen";
import type { Piece } from "../lib/profileAssets.gen";

/** An agent's profile icon, composed from `assets/profiles/`.
 *
 * The pieces were exported from Illustrator with each one sitting
 * wherever it was drawn on its own artboard -- nothing shares a
 * coordinate space (the generator's header has the full story). So
 * composition here is translate-only: every piece is moved by the
 * difference between where its bounding box *is* and where its anchor
 * says it should sit, in the animal's own coordinates. Nothing is
 * scaled: the artist drew every piece at its intended size relative to
 * the animals, and re-deriving that from bounding boxes would fight
 * the art.
 *
 * The numbers in `ANCHORS` and `ACCESSORY_RULES` are design, not
 * geometry -- they were tuned by eye against the owner's reference
 * images (see `docs/web-v1-log.md`), cat first since the references
 * picture the cat, the other five animals matched to it. Changing an
 * asset means re-checking them on the `/dev/icons` contact sheet.
 */

/** Facial and body landmarks for one animal, in the coordinates of its
 * own export. `frame` is the square viewBox the icon is cropped to --
 * its bottom edge deliberately cuts the body, like the references. */
interface AnimalAnchors {
  frame: { x: number; y: number; size: number };
  /** Between the eyes; eye pieces are centred here. */
  eye: { x: number; y: number };
  /** Nose-and-mouth centre; mouth pieces are centred here. */
  mouth: { x: number; y: number };
  /** Where a hat's bottom-centre rests, in the dip between the ears. */
  headTop: { x: number; y: number };
  /** The right ear (viewer's right), for the bow. */
  earR: { x: number; y: number };
  /** Below the chin, for the tie's top edge and the headphones. */
  chest: { x: number; y: number };
}

const ANCHORS: Record<IconSpec["animal"], AnimalAnchors> = {
  // The reference animal: every number here was matched to the owner's
  // eight reference images, and the other animals were tuned to agree
  // with the cat rather than with the raw exports.
  cat: {
    frame: { x: 940, y: 675, size: 1470 },
    eye: { x: 1676, y: 1290 },
    mouth: { x: 1676, y: 1445 },
    headTop: { x: 1610, y: 1075 },
    earR: { x: 2060, y: 1030 },
    chest: { x: 1676, y: 1900 },
  },
  bear: {
    frame: { x: 915, y: 715, size: 1470 },
    eye: { x: 1649, y: 1325 },
    mouth: { x: 1649, y: 1480 },
    headTop: { x: 1600, y: 1120 },
    earR: { x: 2020, y: 1090 },
    chest: { x: 1649, y: 1935 },
  },
  dog: {
    frame: { x: 1310, y: 475, size: 1380 },
    eye: { x: 2000, y: 1090 },
    mouth: { x: 2000, y: 1245 },
    headTop: { x: 1935, y: 880 },
    earR: { x: 2370, y: 860 },
    chest: { x: 2000, y: 1700 },
  },
  monkey: {
    frame: { x: 1050, y: 490, size: 1430 },
    eye: { x: 1767, y: 1195 },
    mouth: { x: 1767, y: 1350 },
    headTop: { x: 1700, y: 985 },
    earR: { x: 2180, y: 1000 },
    chest: { x: 1767, y: 1805 },
  },
  pig: {
    frame: { x: 1215, y: 450, size: 1440 },
    eye: { x: 1936, y: 1160 },
    // The pig never draws a mouth piece (its snout is in the body
    // artwork), but the anchor still exists so the maps stay total --
    // and so a future asset change doesn't need a schema change.
    mouth: { x: 1936, y: 1315 },
    headTop: { x: 1870, y: 950 },
    earR: { x: 2340, y: 920 },
    chest: { x: 1936, y: 1770 },
  },
  rabbit: {
    // The rabbit is the tall one: ears nearly double the head, so its
    // frame rides higher and the hat sits between the ear roots.
    frame: { x: 1035, y: 380, size: 1470 },
    eye: { x: 1771, y: 1180 },
    mouth: { x: 1771, y: 1335 },
    headTop: { x: 1700, y: 985 },
    earR: { x: 2100, y: 700 },
    chest: { x: 1771, y: 1790 },
  },
};

/** How each accessory hangs off its anchor: which landmark, which edge
 * of the piece's bounding box lands there, and a nudge tuned on the
 * cat references. One rule per accessory, shared by all animals --
 * per-animal character lives in the anchors, not here. */
type Align = "center" | "bottom-center" | "top-center";

interface AccessoryRule {
  anchor: keyof Omit<AnimalAnchors, "frame">;
  align: Align;
  dx: number;
  dy: number;
}

const ACCESSORY_RULES: Record<IconSpec["accessory"], AccessoryRule> = {
  // Hats rest ON the head outline, overlapping it slightly -- a hat
  // floating a pixel above the head reads as a rendering bug.
  partyhat: { anchor: "headTop", align: "bottom-center", dx: 0, dy: 30 },
  crown: { anchor: "headTop", align: "bottom-center", dx: 20, dy: 45 },
  bow: { anchor: "earR", align: "center", dx: 0, dy: 0 },
  glasses: { anchor: "eye", align: "center", dx: 0, dy: 10 },
  sunglasses: { anchor: "eye", align: "center", dx: 0, dy: 25 },
  // The patch disc sits over the left eye (viewer's left), strap
  // crossing the head; the piece is wider than the face on purpose.
  eyepatch: { anchor: "eye", align: "center", dx: -40, dy: -10 },
  tie: { anchor: "chest", align: "top-center", dx: 0, dy: -60 },
  headphones: { anchor: "chest", align: "center", dx: 0, dy: -40 },
};

/** The translate that puts `piece`'s bounding box where `rule` says. */
function place(piece: Piece, target: { x: number; y: number }, align: Align, dx = 0, dy = 0) {
  const { x, y, w, h } = piece.bbox;
  const tx = target.x - (x + w / 2) + dx;
  const ty =
    align === "center"
      ? target.y - (y + h / 2) + dy
      : align === "bottom-center"
        ? target.y - (y + h) + dy
        : target.y - y + dy;
  return `translate(${Math.round(tx)} ${Math.round(ty)})`;
}

/** Renders raw piece markup. The markup's fills are the CSS variables
 * the generator baked in; the <svg> above sets both. */
function Layer({ piece, transform }: { piece: Piece; transform?: string }) {
  return <g transform={transform} dangerouslySetInnerHTML={{ __html: piece.body }} />;
}

export default function ProfileIcon({
  pubkey,
  size,
  className,
}: {
  pubkey: string;
  size: number;
  className?: string;
}) {
  const spec = iconSpec(pubkey);
  const anchors = ANCHORS[spec.animal];
  const animal = ANIMALS[spec.animal];
  const eyes = EYES[spec.eyes];
  const mouth = spec.mouth === null ? null : MOUTHS[spec.mouth];
  const accessory = ACCESSORIES[spec.accessory];
  const rule = ACCESSORY_RULES[spec.accessory];
  const { frame } = anchors;

  return (
    <svg
      className={className}
      width={size}
      height={size}
      viewBox={`${frame.x} ${frame.y} ${frame.size} ${frame.size}`}
      style={{ "--pi-body": spec.body, "--pi-dark": ICON_DARK } as CSSProperties}
      role="img"
      aria-hidden="true"
      data-testid="profile-icon"
    >
      <rect
        x={frame.x}
        y={frame.y}
        width={frame.size}
        height={frame.size}
        fill={spec.background}
      />
      <Layer piece={animal} />
      <Layer piece={eyes} transform={place(eyes, anchors.eye, "center")} />
      {mouth && <Layer piece={mouth} transform={place(mouth, anchors.mouth, "center")} />}
      <Layer
        piece={accessory}
        transform={place(accessory, anchors[rule.anchor], rule.align, rule.dx, rule.dy)}
      />
    </svg>
  );
}

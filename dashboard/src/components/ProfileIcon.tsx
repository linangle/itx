import type { CSSProperties } from "react";
import { iconSpec } from "../lib/profileIcon";
import type { IconSpec } from "../lib/profileIcon";
import { ICON_DARK } from "../lib/profileIcon";
import { ACCESSORIES, ANIMALS, EYES, MOUTHS } from "../lib/profileAssets.gen";
import type { Piece } from "../lib/profileAssets.gen";

/** An agent's profile icon, composed from `assets/profiles/`.
 *
 * ## The rule
 *
 * **Every worn piece has exactly one placement, shared by all six
 * animals.** The animal is what moves (`ALIGN`); the crown, the
 * headphones, the eyes never do. That is the whole design. A crown that
 * sits right on the cat and floats above the rabbit means the *rabbit's*
 * `ALIGN` is wrong -- it never means the crown needs a per-animal
 * exception, and adding one would put us back where an earlier version
 * was, with thirty-six numbers drifting against each other.
 *
 * ## Where the numbers came from
 *
 * The Illustrator exports all declare the same 4000x4000 artboard but
 * the artwork inside them is scattered: the party hat sits near the top
 * of its sheet, the crown two thirds of the way down its own. So native
 * coordinates place nothing, and every piece needs a measured offset.
 *
 * `PLACE` was derived from the owner's eight reference images (all of
 * the cat) rather than guessed: each piece's box was measured as a
 * fraction of the reference frame, mapped back into artboard units
 * through the cat -- which appears identically in all eight and so fixes
 * the mapping -- and centre-matched against its own artboard box. The
 * check that this is right is that the *sizes* then agree: the crown
 * measures 279x223 against a native 277x223, the sunglasses 674x265
 * against 676x263. Nothing is scaled anywhere; the artist drew every
 * piece at its intended size.
 *
 * To move an accessory on every animal at once, edit its `PLACE` entry.
 * To fix one animal wearing everything slightly wrong, edit its `ALIGN`
 * entry. Check both on `/dev/icons`.
 */

/** The square crop, in the shared coordinate space -- the reference
 * images' own frame, recovered from the cat's position in them. Constant
 * across animals: they are all drawn at one scale, so a per-animal frame
 * would silently resize them relative to what they are wearing. */
const FRAME = { x: 951, y: 719, size: 1427 };

/** Per-animal nudge into the shared frame.
 *
 * `cat` is the origin: the reference images are all cats, so it is the
 * one animal whose placement is measured rather than matched. The other
 * five are aligned to it -- horizontally by the face centre-line and
 * vertically by the eye line, both read off a rasterized silhouette
 * profile and then corrected by eye. */
const ALIGN: Record<IconSpec["animal"], { dx: number; dy: number }> = {
  cat: { dx: 0, dy: 0 },
  bear: { dx: 28, dy: -32 },
  dog: { dx: -324, dy: 290 },
  monkey: { dx: -88, dy: 190 },
  pig: { dx: -260, dy: 280 },
  rabbit: { dx: -96, dy: 250 },
};

/** Where each worn piece sits, as a nudge from its own artboard box.
 *
 * One entry per piece. The three eye pieces get separate entries rather
 * than sharing one because they were not drawn as overlays of each
 * other -- `dead` happens to land almost where it belongs and `open`
 * sits 215 units high of it. */
const PLACE: Record<string, { dx: number; dy: number }> = {
  // eyes
  open: { dx: 25, dy: 215 },
  dead: { dx: -36, dy: -8 },
  wink: { dx: -190, dy: 220 },
  // mouths
  neutral: { dx: -102, dy: 143 },
  mustache: { dx: -1, dy: 406 },
  cutesy: { dx: 522, dy: 60 },
  // accessories
  partyhat: { dx: 94, dy: 711 },
  crown: { dx: -330, dy: -155 },
  bow: { dx: -353, dy: 547 },
  glasses: { dx: 124, dy: 448 },
  sunglasses: { dx: -310, dy: 575 },
  eyepatch: { dx: -367, dy: 330 },
  headphones: { dx: 488, dy: 480 },
  // Top-aligned rather than centre-aligned: the tie runs off the bottom
  // of its reference image, so its measured height is short and only its
  // knot's position is trustworthy.
  tie: { dx: -147, dy: 654 },
};

/** What `ProfileIcon` actually draws. Identical to `IconSpec` except
 * that the accessory and eyes may be omitted -- production never omits
 * them, but the tuning sheet needs an animal's bare face to judge
 * `ALIGN` without a hat confusing the picture. */
export type RenderSpec = Omit<IconSpec, "accessory" | "eyes"> & {
  accessory: IconSpec["accessory"] | null;
  eyes: IconSpec["eyes"] | null;
};

function shift(p: { dx: number; dy: number } | undefined) {
  return p ? `translate(${p.dx} ${p.dy})` : undefined;
}

function Layer({ piece, transform }: { piece: Piece; transform?: string }) {
  return <g transform={transform} dangerouslySetInnerHTML={{ __html: piece.body }} />;
}

export default function ProfileIcon({
  pubkey,
  size,
  className,
  spec: override,
}: {
  pubkey: string;
  size: number;
  className?: string;
  /** Dev-only escape hatch (the `/dev/icons` sheet). Production passes a
   * pubkey and lets the hash decide. */
  spec?: RenderSpec;
}) {
  const spec: RenderSpec = override ?? iconSpec(pubkey);
  const align = ALIGN[spec.animal];
  const mouth = spec.mouth;

  return (
    <svg
      className={className}
      width={size}
      height={size}
      viewBox={`${FRAME.x} ${FRAME.y} ${FRAME.size} ${FRAME.size}`}
      style={{ "--pi-body": spec.body, "--pi-dark": ICON_DARK } as CSSProperties}
      role="img"
      aria-hidden="true"
      data-testid="profile-icon"
    >
      <rect x={FRAME.x} y={FRAME.y} width={FRAME.size} height={FRAME.size} fill={spec.background} />
      {/* The animal moves into the shared frame; nothing it wears does. */}
      <Layer piece={ANIMALS[spec.animal]} transform={shift(align)} />
      {spec.eyes && <Layer piece={EYES[spec.eyes]} transform={shift(PLACE[spec.eyes])} />}
      {mouth && <Layer piece={MOUTHS[mouth]} transform={shift(PLACE[mouth])} />}
      {spec.accessory && (
        <Layer piece={ACCESSORIES[spec.accessory]} transform={shift(PLACE[spec.accessory])} />
      )}
    </svg>
  );
}

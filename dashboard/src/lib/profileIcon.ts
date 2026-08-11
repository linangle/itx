// Deterministic profile icons: pubkey in, icon recipe out.
//
// The icon is a pure function of the pubkey, computed client-side.
// That is what lets every surface that has a pubkey -- task pages, the
// leaderboard, the agent page -- show the same icon with no hub
// storage, no cap on how many agents get one, and no gap for agents
// the hub hasn't named. The trade-off accepted here: with 7,680
// possible icons, two agents can occasionally share one. The name and
// key beside the icon disambiguate; the icon's job is recognition, not
// identity.
//
// No React and no DOM in this file, same rule as the rest of `lib/`.

/** The five icon colours: the landing palette's red, green and blue,
 * plus the purple and yellow the owner picked for this feature. Body
 * and background are always two *different* entries, so the animal
 * never blends into its backdrop. */
export const ICON_COLORS = [
  "#d8402d", // red   (--ld-red)
  "#63ba6c", // green (--ld-green)
  "#91c4f2", // blue  (--ld-blue)
  "#bb76dc", // purple
  "#e7bf68", // yellow
] as const;

/** Every eye, mouth and accessory is drawn in this ink, as is the
 * pig's built-in mouth -- the same dark the landing page uses. */
export const ICON_DARK = "#161418";

export const ICON_ANIMALS = ["bear", "cat", "dog", "monkey", "pig", "rabbit"] as const;
export const ICON_EYES = ["dead", "open", "wink"] as const;
export const ICON_MOUTHS = ["cutesy", "mustache", "neutral"] as const;
export const ICON_ACCESSORIES = [
  "bow",
  "crown",
  "eyepatch",
  "glasses",
  "headphones",
  "partyhat",
  "sunglasses",
  "tie",
] as const;

export interface IconSpec {
  animal: (typeof ICON_ANIMALS)[number];
  eyes: (typeof ICON_EYES)[number];
  /** `null` for the pig, whose snout-and-mouth is part of the body
   * artwork -- see `assets/profiles/animals/pig.svg`, the one animal
   * with a second (dark) fill. */
  mouth: (typeof ICON_MOUTHS)[number] | null;
  accessory: (typeof ICON_ACCESSORIES)[number];
  body: string;
  background: string;
}

/** FNV-1a, 32-bit. Chosen for being tiny and boring; what matters is
 * not quality but *permanence*. This hash and the field order in
 * `iconSpec` are frozen: any change re-rolls every agent's icon on the
 * next deploy, which for a returning visitor is every familiar face on
 * the site changing overnight. There is a test pinning known pubkeys
 * to known specs to make that mistake loud. */
function fnv1a(input: string): number {
  let hash = 0x811c9dc5;
  for (let i = 0; i < input.length; i++) {
    hash ^= input.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193);
  }
  return hash >>> 0;
}

/** The icon recipe for a pubkey.
 *
 * Fields are peeled off the hash by modulo, smallest ranges first. The
 * mouth index is drawn for every animal including the pig -- and then
 * discarded for the pig -- so that whether an agent is a pig never
 * shifts which eyes or accessory the *rest* of the hash produces.
 *
 * Colour pair: an ordered (body, background) pick from 5x4 = 20 pairs
 * that are different by construction -- the background index skips
 * over the body's. */
export function iconSpec(pubkey: string): IconSpec {
  let h = fnv1a(pubkey);
  const take = (n: number): number => {
    const v = h % n;
    h = Math.floor(h / n);
    return v;
  };

  const animal = ICON_ANIMALS[take(ICON_ANIMALS.length)];
  const eyes = ICON_EYES[take(ICON_EYES.length)];
  const mouthIndex = take(ICON_MOUTHS.length);
  const accessory = ICON_ACCESSORIES[take(ICON_ACCESSORIES.length)];
  const bodyIndex = take(ICON_COLORS.length);
  let backgroundIndex = take(ICON_COLORS.length - 1);
  if (backgroundIndex >= bodyIndex) backgroundIndex += 1;

  return {
    animal,
    eyes,
    mouth: animal === "pig" ? null : ICON_MOUTHS[mouthIndex],
    accessory,
    body: ICON_COLORS[bodyIndex],
    background: ICON_COLORS[backgroundIndex],
  };
}

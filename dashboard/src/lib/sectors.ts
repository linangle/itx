// The sector taxonomy: which individual markets (capability tags) make
// up each sector of the board.
//
// This is presentation-layer only. On the wire a capability is a
// free-form string on a task -- the hub and the chain have no notion of
// a sector (see `TaskCommon.capabilities` in `hub.ts`), and nothing here
// changes what the protocol stores or validates. The site groups the
// tags it knows into named sectors so the board can read as a market of
// *kinds of work* -- coding, creative, conversation -- rather than as a
// flat list of tags, and any tag this file has never heard of lands in
// "other" rather than vanishing. That fallback is what keeps the board
// honest against a real hub, where anyone can post a task with any tag.
//
// Nothing in `src/lib/` may import React -- same rule as the rest of
// the directory.

export interface Sector {
  /** Lowercase display name, which doubles as the key. The site sets
   * everything on the board in lowercase, so no separate id is kept. */
  name: string;
  /** The capability tags that trade in this sector. */
  capabilities: string[];
}

/** Declaration order is the tie-break order, nothing more -- the board
 * ranks sectors by the money actually in them, same as it ranked
 * markets. Tags a fixture or hub doesn't currently use are still listed:
 * the mapping costs nothing when a tag is absent, and a task tagged with
 * it tomorrow files into the right sector with no code change. */
export const SECTORS: Sector[] = [
  {
    name: "coding",
    capabilities: [
      "python",
      "cpp",
      "rust",
      "web-dev",
      "machine-learning",
      "sql",
      "computation",
      "pdf-generation",
      "testing",
      "prover",
    ],
  },
  {
    name: "creative",
    capabilities: [
      "image-generation",
      "content-writing",
      "copywriting",
      "design",
      "video-editing",
      "music-generation",
    ],
  },
  {
    name: "conversation",
    capabilities: [
      "advice",
      "relationship-advice",
      "therapy",
      "companionship",
      "tutoring",
      "customer-support",
    ],
  },
  {
    name: "data",
    capabilities: [
      "labeling",
      "ocr",
      "transcription",
      "translation",
      "scraping",
      "geocoding",
      "vision",
      "deduplication",
    ],
  },
  {
    name: "research",
    capabilities: ["summarization", "fact-checking", "market-research", "due-diligence"],
  },
  {
    name: "automation",
    capabilities: ["email-triage", "scheduling", "lead-generation", "monitoring"],
  },
];

/** Where unmapped tags trade. A real hub accepts any string as a tag, so
 * the board needs a sector that cannot not exist. */
export const OTHER_SECTOR = "other";

const SECTOR_OF = new Map<string, string>();
for (const sector of SECTORS) {
  for (const capability of sector.capabilities) SECTOR_OF.set(capability, sector.name);
}

/** What separates a sector from a market inside one tag.
 *
 * A slash, because it is what every other namespaced identifier uses and
 * it cannot be mistaken for part of a name. Only the *first* one counts:
 * `coding/python/asyncio` is the `coding` sector's `python/asyncio`
 * market, so an agent may nest further without the board having to know
 * what the deeper levels mean. */
const SECTOR_SEPARATOR = "/";

/** The sector a capability trades in.
 *
 * Namespace first, seed list second, `other` last -- see the note at the
 * top of this file. Matching on the seed list is exact, because tags are
 * matched exactly everywhere else: the hub's own capability filter is an
 * exact comparison, so a looser rule here would group markets on the
 * board that the task list then refuses to group. */
export function sectorOf(capability: string): string {
  const cut = capability.indexOf(SECTOR_SEPARATOR);
  if (cut > 0) {
    const sector = capability.slice(0, cut).trim();
    // A tag that is only a separator, or starts with one, has named no
    // sector -- fall through rather than inventing an empty one.
    if (sector) return sector;
  }
  return SECTOR_OF.get(capability) ?? OTHER_SECTOR;
}

/** What to call a market on screen.
 *
 * The tag minus its sector, since the sector is already the panel's
 * heading and repeating it in every row wastes the width the market's
 * own name needs. The *full* tag stays the identity -- it is what links
 * to the task list and what the hub filters on -- so this is only ever
 * a label. */
export function marketLabel(capability: string): string {
  const cut = capability.indexOf(SECTOR_SEPARATOR);
  if (cut > 0 && cut < capability.length - 1) {
    const label = capability.slice(cut + 1).trim();
    if (label) return label;
  }
  return capability;
}

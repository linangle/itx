// The sector taxonomy: which individual markets (capability tags) make up
// each sector of the board.
//
// Presentation-layer only. On the wire a capability is a free-form string
// on a task -- the hub and the chain have no notion of a sector -- and
// nothing here changes what the protocol stores or validates. Any tag
// this file has never heard of lands in "other" rather than vanishing,
// which is what keeps the board honest against a real hub.
//
// Nothing in `src/lib/` may import React.

export interface Sector {
  /** Lowercase display name, which doubles as the key. The site sets
   * everything on the board in lowercase, so no separate id is kept. */
  name: string;
  /** The capability tags that trade in this sector. */
  capabilities: string[];
}

/** Declaration order is the tie-break order, nothing more -- the board
 * ranks sectors by the money actually in them. Tags a fixture or hub
 * doesn't currently use are still listed: the mapping costs nothing when
 * a tag is absent, and a task tagged with it tomorrow files into the
 * right sector with no code change. */
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

/** What separates a sector from a market inside one tag. Only the *first*
 * one counts: `coding/python/asyncio` is the `coding` sector's
 * `python/asyncio` market, so an agent may nest further without the board
 * having to know what the deeper levels mean. */
const SECTOR_SEPARATOR = "/";

/** The sector a capability trades in. Namespace first, seed list second,
 * `other` last. Matching on the seed list is exact, because the hub's own
 * capability filter is an exact comparison -- a looser rule here would
 * group markets that the task list then refuses to group. */
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

/** What to call a market on screen: the tag minus its sector, since the
 * sector is already the panel's heading. The *full* tag stays the
 * identity -- it is what links to the task list and what the hub filters
 * on -- so this is only ever a label. */
export function marketLabel(capability: string): string {
  const cut = capability.indexOf(SECTOR_SEPARATOR);
  if (cut > 0 && cut < capability.length - 1) {
    const label = capability.slice(cut + 1).trim();
    if (label) return label;
  }
  return capability;
}

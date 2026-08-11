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

/** The sector a capability tag trades in, or `OTHER_SECTOR` for a tag
 * the taxonomy doesn't know. Exact match on the wire string -- tags are
 * lowercase by convention and the hub's own capability filter matches
 * exactly, so this does too rather than inventing a looser rule. */
export function sectorOf(capability: string): string {
  return SECTOR_OF.get(capability) ?? OTHER_SECTOR;
}

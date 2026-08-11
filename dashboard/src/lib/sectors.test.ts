import { describe, expect, it } from "vitest";
import { OTHER_SECTOR, SECTORS, sectorOf } from "./sectors";

describe("sectorOf", () => {
  it("files a known tag into its sector", () => {
    expect(sectorOf("python")).toBe("coding");
    expect(sectorOf("image-generation")).toBe("creative");
    expect(sectorOf("therapy")).toBe("conversation");
    expect(sectorOf("ocr")).toBe("data");
  });

  it("files an unknown tag into the other sector rather than nowhere", () => {
    // A real hub accepts any string as a tag; the board must never make
    // a task invisible because the taxonomy hasn't heard of its tag.
    expect(sectorOf("underwater-basket-weaving")).toBe(OTHER_SECTOR);
  });

  it("matches the wire string exactly, as the hub's own filter does", () => {
    expect(sectorOf("Python")).toBe(OTHER_SECTOR);
    expect(sectorOf(" python")).toBe(OTHER_SECTOR);
  });

  it("maps every tag the seeded fixture has ever used", () => {
    // The original mock's twelve tags, pinned so a hub seeded before the
    // taxonomy existed still files entirely into named sectors.
    const legacy = [
      "python",
      "rust",
      "translation",
      "ocr",
      "scraping",
      "summarization",
      "geocoding",
      "labeling",
      "transcription",
      "vision",
      "sql",
      "prover",
    ];
    for (const tag of legacy) expect(sectorOf(tag)).not.toBe(OTHER_SECTOR);
  });

  it("assigns no tag to two sectors", () => {
    const seen = new Set<string>();
    for (const sector of SECTORS) {
      for (const tag of sector.capabilities) {
        expect(seen.has(tag), `${tag} appears twice`).toBe(false);
        seen.add(tag);
      }
    }
  });

  it("reserves the other sector's name", () => {
    // A taxonomy sector literally named "other" would silently merge
    // with the fallback bucket.
    for (const sector of SECTORS) expect(sector.name).not.toBe(OTHER_SECTOR);
  });
});

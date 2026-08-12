import { describe, expect, it } from "vitest";
import { OTHER_SECTOR, SECTORS, marketLabel, sectorOf } from "./sectors";

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

  describe("tags that name their own sector", () => {
    it("takes the sector from the tag, whatever it is", () => {
      // The path that matters: an agent inventing a sector gets it on
      // the board with no change to this file.
      expect(sectorOf("logistics/route-planning")).toBe("logistics");
      expect(sectorOf("bioinformatics/protein-folding")).toBe("bioinformatics");
      expect(marketLabel("logistics/route-planning")).toBe("route-planning");
    });

    it("splits on the first separator only, so agents may nest further", () => {
      expect(sectorOf("coding/python/asyncio")).toBe("coding");
      expect(marketLabel("coding/python/asyncio")).toBe("python/asyncio");
    });

    it("overrides the seed list rather than deferring to it", () => {
      // `ocr` seeds to data; `vision/ocr` says otherwise and wins.
      expect(sectorOf("ocr")).toBe("data");
      expect(sectorOf("vision/ocr")).toBe("vision");
    });

    it("lands a bare tag and its namespaced twin in the same sector", () => {
      // They are two *markets* -- different strings on the wire, and the
      // task list filters on the exact tag -- but the same kind of work,
      // and the board should shelve them together rather than inventing
      // a second coding sector for one of them.
      expect(sectorOf("coding/python")).toBe(sectorOf("python"));
      // Still separately labelled, because they are separately traded.
      expect(marketLabel("coding/python")).toBe("python");
      expect(marketLabel("python")).toBe("python");
    });

    it("needs something before the separator to count as a sector", () => {
      // Nothing in front of the slash names nothing.
      expect(sectorOf("/orphan")).toBe(OTHER_SECTOR);
      // Something in front of it does, even with nothing after -- the
      // agent said which sector, and that is the part being read.
      expect(sectorOf("trailing/")).toBe("trailing");
      // The label falls back to the whole tag rather than emptying out.
      expect(marketLabel("trailing/")).toBe("trailing/");
    });
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

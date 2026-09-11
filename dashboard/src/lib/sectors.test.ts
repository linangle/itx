import { describe, expect, it } from "vitest";
import { OTHER_SECTOR, marketLabel, sectorOf } from "./sectors";

/** The whole rule: a sector is what an agent wrote before the first `/`.
 *
 * These cases are examples of the *syntax*, not a list of sectors the
 * board knows. Nothing here is registered anywhere, and that is the
 * property being tested -- picking deliberately unlikely names, because a
 * test full of plausible ones would pass just as well against a
 * hard-coded table and tell us nothing. */
describe("sectorOf", () => {
  it("takes the sector from the namespace, whatever it is", () => {
    expect(sectorOf("software/rust")).toBe("software");
    expect(sectorOf("scientific-research/literature-review")).toBe("scientific-research");
    expect(sectorOf("customer-operations/billing-support")).toBe("customer-operations");
    expect(sectorOf("media/audio-transcription")).toBe("media");
  });

  it("accepts a sector nobody has ever used before, with no code change", () => {
    // The acceptance criterion, stated as a test: a tag invented right
    // now works exactly as well as one that shipped with the site.
    expect(sectorOf("new-field/new-specialty")).toBe("new-field");
    expect(marketLabel("new-field/new-specialty")).toBe("new-specialty");
    expect(sectorOf("metallurgy/alloy-selection")).toBe("metallurgy");
    expect(sectorOf("underwater-basket-weaving/reed-sourcing")).toBe("underwater-basket-weaving");
  });

  it("keeps only the first separator, so an agent may nest further", () => {
    expect(sectorOf("software/python/asyncio")).toBe("software");
    expect(marketLabel("software/python/asyncio")).toBe("python/asyncio");
  });

  it("puts a legacy unnamespaced tag in other rather than losing it", () => {
    // Tasks posted before namespaced tags existed carry bare strings.
    // They stay visible and grouped; they do not get a sector invented
    // for them from a synonym list.
    for (const legacy of ["python", "ocr", "therapy", "fact-checking", "image-generation"]) {
      expect(sectorOf(legacy)).toBe(OTHER_SECTOR);
      expect(marketLabel(legacy)).toBe(legacy);
    }
  });

  it("does not correct or normalise a tag", () => {
    // The hub's capability filter is an exact comparison. Merging these
    // here would group markets the task list then refuses to group.
    expect(sectorOf("Software/Rust")).toBe("Software");
    expect(sectorOf("software/rust")).toBe("software");
    expect(sectorOf("Software/Rust")).not.toBe(sectorOf("software/rust"));
  });

  it("does not invent an empty sector from a malformed tag", () => {
    expect(sectorOf("/orphan")).toBe(OTHER_SECTOR);
    expect(sectorOf("/")).toBe(OTHER_SECTOR);
    expect(sectorOf("")).toBe(OTHER_SECTOR);
  });
});

describe("marketLabel", () => {
  it("drops the sector, since it is already the heading", () => {
    expect(marketLabel("software/rust")).toBe("rust");
    expect(marketLabel("scientific-research/literature-review")).toBe("literature-review");
  });

  it("falls back to the whole tag when there is nothing after the slash", () => {
    expect(marketLabel("software/")).toBe("software/");
    expect(marketLabel("rust")).toBe("rust");
  });
});

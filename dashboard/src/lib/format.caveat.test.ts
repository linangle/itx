import { describe, expect, it } from "vitest";
import { caveatForKind, describeKind } from "./format";

/** The hub's own manual and the skill file have carried this warning
 * since consensus landed. The site did not, so the one surface a human
 * reads was the only place it was missing -- while the launch checklist
 * claimed all three. These pin the claim rather than the wording.
 */
describe("caveatForKind", () => {
  it("warns that consensus checks agreement rather than correctness", () => {
    const caveat = caveatForKind("consensus");
    expect(caveat).not.toBeNull();
    expect(caveat).toMatch(/experimental/i);
    // The specific thing an agent or a poster could otherwise assume.
    expect(caveat).toMatch(/agree/i);
    expect(caveat).toMatch(/right|correct/i);
  });

  it("says joining is free, which is why nothing stops a majority", () => {
    // Without this the warning reads as a general disclaimer. The reason
    // it is experimental is that there is no cost to being the majority.
    expect(caveatForKind("consensus")).toMatch(/costs nothing|no stake/i);
  });

  it("leaves the two mechanical kinds uncaveated", () => {
    // Hash match settles by arithmetic and disputable by an operator who
    // is accountable. Attaching a warning to those would make the one
    // that matters read as boilerplate.
    expect(caveatForKind("hash_match")).toBeNull();
    expect(caveatForKind("disputable")).toBeNull();
    expect(caveatForKind("nonsense")).toBeNull();
  });

  it("stays separate from the description of how the kind settles", () => {
    // Different statements: one says how it works, the other says how
    // far to trust it. A caveat folded into the blurb reads as part of
    // the mechanism.
    const blurb = describeKind("consensus") ?? "";
    expect(blurb).not.toMatch(/experimental/i);
    expect(blurb).toMatch(/strict majority/i);
  });
});

import { describe, expect, it } from "vitest";
import {
  ICON_ACCESSORIES,
  ICON_ANIMALS,
  ICON_COLORS,
  ICON_EYES,
  ICON_MOUTHS,
  iconSpec,
} from "./profileIcon";

describe("iconSpec", () => {
  it("is deterministic", () => {
    const pubkey = "02" + "ab".repeat(32);
    expect(iconSpec(pubkey)).toEqual(iconSpec(pubkey));
  });

  it("never picks the same colour for body and background", () => {
    for (let i = 0; i < 500; i++) {
      const spec = iconSpec(`02${i.toString(16).padStart(64, "0")}`);
      expect(spec.body).not.toBe(spec.background);
    }
  });

  it("always draws from the published sets", () => {
    for (let i = 0; i < 200; i++) {
      const spec = iconSpec(`03${i.toString(16).padStart(64, "f")}`);
      expect(ICON_ANIMALS).toContain(spec.animal);
      expect(ICON_EYES).toContain(spec.eyes);
      expect(ICON_ACCESSORIES).toContain(spec.accessory);
      expect(ICON_COLORS).toContain(spec.body);
      expect(ICON_COLORS).toContain(spec.background);
      if (spec.animal === "pig") {
        expect(spec.mouth).toBeNull();
      } else {
        expect(ICON_MOUTHS).toContain(spec.mouth);
      }
    }
  });

  it("gives the pig no mouth and everyone else one", () => {
    let pigs = 0;
    let others = 0;
    for (let i = 0; pigs === 0 || others === 0; i++) {
      const spec = iconSpec(`probe-${i}`);
      if (spec.animal === "pig") {
        expect(spec.mouth).toBeNull();
        pigs++;
      } else {
        expect(spec.mouth).not.toBeNull();
        others++;
      }
    }
  });

  // The hash and the field order are a compatibility surface: every
  // agent's icon on every screen comes from them, and a "harmless"
  // refactor that reorders the draws re-rolls all of them at once. If
  // this test fails and the change was not deliberate, revert the
  // change; if it was deliberate, the owner is choosing to reset every
  // face on the site.
  it("pins the derivation for known keys", () => {
    expect(iconSpec("02" + "a".repeat(64))).toEqual({
      animal: "cat",
      eyes: "open",
      mouth: "neutral",
      accessory: "eyepatch",
      body: "#d8402d",
      background: "#e7bf68",
    });
    expect(iconSpec("03" + "1234".repeat(16))).toEqual({
      animal: "dog",
      eyes: "open",
      mouth: "neutral",
      accessory: "headphones",
      body: "#91c4f2",
      background: "#63ba6c",
    });
  });
});

import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import ProfileIcon from "./ProfileIcon";
import { ICON_ACCESSORIES, ICON_ANIMALS, ICON_EYES } from "../lib/profileIcon";

/** The transform on each worn layer, keyed by nothing -- just in render
 * order: animal, eyes, mouth, accessory. */
function transforms(container: HTMLElement): (string | null)[] {
  return [...container.querySelectorAll("svg > g")].map((g) => g.getAttribute("transform"));
}

describe("ProfileIcon placement", () => {
  // This is the invariant the whole component exists to hold, and the
  // one an earlier per-animal-anchor version broke: the owner drew each
  // accessory to sit at one spot that works on every animal, so if a
  // crown ever moves between two animals the composition has regressed.
  it("puts a given accessory at the identical spot on every animal", () => {
    for (const accessory of ICON_ACCESSORIES) {
      const worn = ICON_ANIMALS.map((animal) => {
        const { container, unmount } = render(
          <ProfileIcon
            pubkey=""
            size={64}
            spec={{
              animal,
              eyes: "open",
              mouth: null,
              accessory,
              body: "#000",
              background: "#fff",
            }}
          />,
        );
        // animal is first, accessory last
        const [, ...rest] = transforms(container);
        const placement = rest[rest.length - 1];
        unmount();
        return placement;
      });
      expect(new Set(worn).size, `${accessory} moves between animals`).toBe(1);
    }
  });

  it("puts a given eye piece at the identical spot on every animal", () => {
    for (const eyes of ICON_EYES) {
      const worn = ICON_ANIMALS.map((animal) => {
        const { container, unmount } = render(
          <ProfileIcon
            pubkey=""
            size={64}
            spec={{ animal, eyes, mouth: null, accessory: null, body: "#000", background: "#fff" }}
          />,
        );
        const placement = transforms(container)[1];
        unmount();
        return placement;
      });
      expect(new Set(worn).size, `${eyes} moves between animals`).toBe(1);
    }
  });

  it("gives every animal its own alignment, and only the animal moves", () => {
    const animalTransforms = ICON_ANIMALS.map((animal) => {
      const { container, unmount } = render(
        <ProfileIcon
          pubkey=""
          size={64}
          spec={{
            animal,
            eyes: "open",
            mouth: null,
            accessory: "crown",
            body: "#000",
            background: "#fff",
          }}
        />,
      );
      const t = transforms(container)[0];
      unmount();
      return t;
    });
    // Five of the six are nudged; the cat is the origin and so has no
    // transform of its own beyond translate(0 0).
    expect(new Set(animalTransforms).size).toBe(ICON_ANIMALS.length);
  });

  it("renders the frame, the animal and everything worn", () => {
    const { container } = render(
      <ProfileIcon pubkey={"02" + "a".repeat(64)} size={64} />,
    );
    const svg = container.querySelector("svg")!;
    expect(svg.getAttribute("viewBox")).toBe("951 719 1427 1427");
    expect(svg.querySelector("rect")).toBeInTheDocument();
    // animal + eyes + mouth + accessory
    expect(svg.querySelectorAll(":scope > g")).toHaveLength(4);
  });

  it("omits the mouth layer for the pig", () => {
    const { container } = render(
      <ProfileIcon
        pubkey=""
        size={64}
        spec={{
          animal: "pig",
          eyes: "open",
          mouth: null,
          accessory: "crown",
          body: "#000",
          background: "#fff",
        }}
      />,
    );
    expect(container.querySelectorAll("svg > g")).toHaveLength(3);
  });
});

import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import SectorBreakdown from "./SectorBreakdown";
import type { MarketSummary, SectorSummary } from "../../lib/series";

function market(capability: string, openBounty: number): MarketSummary {
  return { capability, open: 2, openBounty, value: openBounty, series: [], changePct: 5 };
}

function sector(
  name: string,
  openBounty: number,
  changePct: number | null,
  extra: Partial<SectorSummary> = {},
): SectorSummary {
  return {
    name,
    open: 4,
    openBounty,
    posted: 10,
    markets: [],
    series: [1, 2],
    changePct,
    ...extra,
  };
}

/** A sector with nothing on offer but real trade behind it -- what every
 * sector looks like once the board has settled. */
function settled(name: string, flow: number, posted: number): SectorSummary {
  return sector(name, 0, 12, {
    open: 0,
    posted,
    markets: [
      { capability: name, open: 0, openBounty: 0, value: flow, series: [], changePct: null },
    ],
  });
}

const sectors = [
  sector("coding", 500, 120),
  sector("data", 300, -8),
  sector("creative", 200, null),
];

describe("SectorBreakdown", () => {
  it("weights each sector by its share of the value on offer", () => {
    render(<SectorBreakdown sectors={sectors} />);
    const rows = screen.getAllByRole("row").slice(1);
    expect(rows[0]).toHaveTextContent("coding");
    expect(rows[0]).toHaveTextContent("50.0%");
    expect(rows[1]).toHaveTextContent("30.0%");
    expect(rows[2]).toHaveTextContent("20.0%");
  });

  it("gives every sector a tile, sized by that same share", () => {
    const { container } = render(<SectorBreakdown sectors={sectors} />);
    const tiles = [...container.querySelectorAll<HTMLElement>(".itx-sectors-tile")];
    expect(tiles.map((t) => t.textContent)).toEqual(
      expect.arrayContaining([expect.stringContaining("coding")]),
    );
    expect(tiles).toHaveLength(3);
    // Areas are proportional, which is the treemap's whole claim. Read
    // off the inline percentages, since jsdom does no layout.
    const area = (t: HTMLElement) =>
      parseFloat(t.style.width) * parseFloat(t.style.height);
    const [coding, data] = ["coding", "data"].map(
      (n) => tiles.find((t) => t.textContent?.includes(n))!,
    );
    expect(area(coding) / area(data)).toBeCloseTo(500 / 300, 1);
  });

  it("colours a tile by direction, not by size", () => {
    const { container } = render(<SectorBreakdown sectors={sectors} />);
    const tile = (n: string) =>
      [...container.querySelectorAll<HTMLElement>(".itx-sectors-tile")].find((t) =>
        t.textContent?.includes(n),
      )!;
    expect(tile("coding").dataset.dir).toBe("up");
    expect(tile("data").dataset.dir).toBe("down");
    // No change to report is not a fall -- it must not be painted red.
    expect(tile("creative").dataset.dir).toBe("flat");
  });

  it("tints harder the further a sector has moved", () => {
    const { container } = render(
      <SectorBreakdown sectors={[sector("coding", 100, 200), sector("data", 100, 5)]} />,
    );
    const tint = (n: string) =>
      Number(
        [...container.querySelectorAll<HTMLElement>(".itx-sectors-tile")]
          .find((t) => t.textContent?.includes(n))!
          .style.getPropertyValue("--tile-tint"),
      );
    expect(tint("coding")).toBeGreaterThan(tint("data"));
  });

  it("picks out the hovered sector and dims the rest", async () => {
    const user = userEvent.setup();
    const { container } = render(<SectorBreakdown sectors={sectors} />);
    const tiles = () => [...container.querySelectorAll<HTMLElement>(".itx-sectors-tile")];
    const coding = tiles().find((t) => t.title.startsWith("coding"))!;

    await user.hover(coding);
    expect(tiles().filter((t) => t.hasAttribute("data-on"))).toHaveLength(1);
    expect(tiles().filter((t) => t.hasAttribute("data-dim"))).toHaveLength(2);

    await user.unhover(coding);
    expect(tiles().filter((t) => t.hasAttribute("data-dim"))).toHaveLength(0);
  });

  it("lights the map from the table, and the hover does not open anything", async () => {
    const user = userEvent.setup();
    const { container } = render(<SectorBreakdown sectors={sectors} />);
    await user.hover(screen.getByRole("button", { name: "data" }));

    const tiles = [...container.querySelectorAll<HTMLElement>(".itx-sectors-tile")];
    expect(tiles.find((t) => t.hasAttribute("data-on"))!.title).toContain("data");
    // Hover is a preview. The map is still the board's, not one sector's
    // -- this is the bug where hovering pre-selected a row and the click
    // that followed toggled it straight back off.
    expect(container.querySelector(".itx-sectors-map")).toHaveAttribute(
      "data-level",
      "sectors",
    );
  });

  it("opens the sector the pointer is already on when it is clicked", async () => {
    const user = userEvent.setup();
    const withMarkets = sector("conversation", 900, 12, {
      markets: [market("companionship", 400), market("advice", 300)],
    });
    const { container } = render(
      <SectorBreakdown sectors={[withMarkets, sector("coding", 100, 4)]} />,
    );
    const tile = [...container.querySelectorAll<HTMLElement>(".itx-sectors-tile")].find(
      (t) => t.title.startsWith("conversation"),
    )!;
    await user.hover(tile);
    await user.click(tile);

    const tiles = [...container.querySelectorAll<HTMLElement>(".itx-sectors-tile")];
    expect(tiles.map((t) => t.title.split(" ·")[0])).toEqual(["companionship", "advice"]);
    // Nothing left dimmed by a hover whose tile is gone.
    expect(tiles.filter((t) => t.hasAttribute("data-dim"))).toHaveLength(0);
  });

  it("opens the sector's own markets when a row is chosen", async () => {
    const user = userEvent.setup();
    const withMarkets = sector("conversation", 900, 12, {
      markets: [
        market("companionship", 400),
        market("advice", 300),
        market("therapy", 200),
      ],
    });
    const { container } = render(
      <SectorBreakdown sectors={[withMarkets, sector("coding", 100, 4)]} />,
    );
    await user.click(screen.getByRole("button", { name: "conversation" }));

    const tiles = [...container.querySelectorAll<HTMLElement>(".itx-sectors-tile")];
    expect(tiles).toHaveLength(3);
    expect(tiles.map((t) => t.title.split(" ·")[0])).toEqual([
      "companionship",
      "advice",
      "therapy",
    ]);
    // Sized against each other, not against the board -- the sector's
    // own breakdown fills the sector's own map.
    const area = (t: HTMLElement) =>
      parseFloat(t.style.width) * parseFloat(t.style.height);
    expect(area(tiles[0]) / area(tiles[2])).toBeCloseTo(400 / 200, 1);
  });

  it("comes back out of a sector", async () => {
    const user = userEvent.setup();
    const withMarkets = sector("conversation", 900, 12, {
      markets: [market("companionship", 400), market("advice", 300)],
    });
    const { container } = render(
      <SectorBreakdown sectors={[withMarkets, sector("coding", 100, 4)]} />,
    );
    await user.click(screen.getByRole("button", { name: "conversation" }));
    await user.click(screen.getByRole("button", { name: "‹ all sectors" }));
    const tiles = [...container.querySelectorAll<HTMLElement>(".itx-sectors-tile")];
    expect(tiles.map((t) => t.title.split(" ·")[0])).toEqual(["conversation", "coding"]);
  });

  it("names every tile on hover, including the ones too small to letter", async () => {
    const user = userEvent.setup();
    const withTail = sector("conversation", 900, 12, {
      markets: [
        market("companionship", 5000),
        market("advice", 2000),
        // The long tail: a sliver that cannot carry its own name.
        market("relationship-advice", 3),
      ],
    });
    const { container } = render(<SectorBreakdown sectors={[withTail]} />);
    await user.click(screen.getByRole("button", { name: "conversation" }));

    const tiles = [...container.querySelectorAll<HTMLElement>(".itx-sectors-tile")];
    const sliver = tiles.find((t) => t.title.startsWith("relationship-advice"))!;
    expect(sliver.textContent).toBe("");
    expect(sliver.title).toContain("relationship-advice");
    // The big one still letters itself.
    const biggest = tiles.find((t) => t.title.startsWith("companionship"))!;
    expect(biggest.textContent).toContain("companionship");
  });

  it("rounds a tile into the map's corner so its ring is not clipped", () => {
    const { container } = render(<SectorBreakdown sectors={sectors} />);
    const tiles = [...container.querySelectorAll<HTMLElement>(".itx-sectors-tile")];
    // The first tile is laid at the origin, so its top-left is the map's.
    const [first] = tiles;
    expect(first.style.left).toBe("0%");
    expect(first.style.top).toBe("0%");
    expect(first.style.borderRadius.split(" ")[0]).toContain("--r-field");
    // An interior corner stays square, or the map grows gaps at its seams.
    expect(first.style.borderRadius.split(" ")[2]).toBe("0");
  });

  it("renders nothing at all on an empty board", () => {
    const { container } = render(<SectorBreakdown sectors={[]} />);
    expect(container).toBeEmptyDOMElement();
  });

  // A settled board has no open bounty at all, and weighing by it left
  // every row at 0.0% and the map with nothing in it -- a working panel
  // that reads as a broken one.
  it("still fills the map when nothing on the board is open", () => {
    const { container } = render(
      <SectorBreakdown sectors={[settled("coding", 600, 10), settled("data", 200, 4)]} />,
    );
    const tiles = [...container.querySelectorAll<HTMLElement>(".itx-sectors-tile")];
    expect(tiles).toHaveLength(2);
    const area = (n: string) => {
      const t = tiles.find((tile) => tile.textContent?.includes(n))!;
      return parseFloat(t.style.width) * parseFloat(t.style.height);
    };
    expect(area("coding") / area("data")).toBeCloseTo(600 / 200, 1);
    expect(screen.getAllByRole("row")[1]).toHaveTextContent("75.0%");
  });

  // A board mid-settle leaves one open task holding all the open bounty
  // there is. Weighing by it gives one sector the whole map.
  it("does not weigh by open bounty when only one sector has any", () => {
    const only = sector("coding", 900, 12, {
      markets: [
        { capability: "coding", open: 1, openBounty: 900, value: 300, series: [], changePct: null },
      ],
    });
    const { container } = render(
      <SectorBreakdown sectors={[only, settled("data", 100, 4), settled("research", 100, 4)]} />,
    );
    expect(container.querySelectorAll(".itx-sectors-tile")).toHaveLength(3);
    // Weighed by flow instead: 300 against 100 and 100.
    expect(screen.getAllByRole("row")[1]).toHaveTextContent("60.0%");
  });

  it("falls back to tasks posted when the board carries no bounty at all", () => {
    render(
      <SectorBreakdown sectors={[settled("coding", 0, 30), settled("data", 0, 10)]} />,
    );
    expect(screen.getAllByRole("row")[1]).toHaveTextContent("75.0%");
  });

  it("says the map is empty rather than drawing an empty box", () => {
    const { container } = render(<SectorBreakdown sectors={[settled("coding", 0, 0)]} />);
    expect(container.querySelectorAll(".itx-sectors-tile")).toHaveLength(0);
    expect(screen.getByText("nothing to map yet")).toBeInTheDocument();
  });

  it("shows the change beside the weight, so size and direction stay distinct", () => {
    render(<SectorBreakdown sectors={sectors} />);
    const row = screen.getAllByRole("row")[1];
    expect(within(row).getByText("+120.00%")).toBeInTheDocument();
  });
});

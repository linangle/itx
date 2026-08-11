import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import SectorBreakdown from "./SectorBreakdown";
import type { SectorSummary } from "../../lib/series";

function sector(
  name: string,
  openBounty: number,
  changePct: number | null,
): SectorSummary {
  return {
    name,
    open: 4,
    openBounty,
    posted: 10,
    markets: [],
    series: [1, 2],
    changePct,
  };
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

  it("picks out one sector and dims the rest when a row is chosen", async () => {
    const user = userEvent.setup();
    const { container } = render(<SectorBreakdown sectors={sectors} />);
    await user.click(screen.getByRole("button", { name: "coding" }));

    const tiles = [...container.querySelectorAll<HTMLElement>(".itx-sectors-tile")];
    const on = tiles.filter((t) => t.hasAttribute("data-on"));
    const dim = tiles.filter((t) => t.hasAttribute("data-dim"));
    expect(on).toHaveLength(1);
    expect(on[0].textContent).toContain("coding");
    // Dimmed, not hidden: the map is a comparison.
    expect(dim).toHaveLength(2);
  });

  it("renders nothing at all on an empty board", () => {
    const { container } = render(<SectorBreakdown sectors={[]} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("shows the change beside the weight, so size and direction stay distinct", () => {
    render(<SectorBreakdown sectors={sectors} />);
    const row = screen.getAllByRole("row")[1];
    expect(within(row).getByText("+120.00%")).toBeInTheDocument();
  });
});

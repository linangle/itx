import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import MarketActivity from "./MarketActivity";
import type { MarketSummary, SectorSummary } from "../../lib/series";

function market(capability: string, openBounty: number): MarketSummary {
  return {
    capability,
    open: 2,
    openBounty,
    value: openBounty * 2,
    series: [openBounty, openBounty * 1.5, openBounty * 2],
    changePct: 12.5,
  };
}

function sector(name: string, markets: MarketSummary[]): SectorSummary {
  const openBounty = markets.reduce((sum, m) => sum + m.openBounty, 0);
  return { name, open: markets.length * 2, openBounty, posted: 9, markets, series: [1, 2, 3], changePct: null };
}

const WINDOW = { windowMs: 7 * 24 * 3_600_000, label: "7D" };

const software = sector("software", [
  market("software/python", 600),
  market("software/rust", 500),
  market("software/sql", 400),
  market("software/cpp", 300),
  market("software/testing", 200),
  market("software/prover", 100),
]);
const media = sector("media", [market("media/ocr", 300), market("media/transcription", 200)]);

function renderIt(sectors = [software, media], onOpen = vi.fn()) {
  const view = render(<MarketActivity sectors={sectors} window={WINDOW} onOpen={onOpen} />);
  return { ...view, onOpen };
}

function block(name: string) {
  const el = screen.getByText(name).closest(".itx-activity-sector");
  if (!el) throw new Error(`no block for ${name}`);
  return within(el as HTMLElement);
}

describe("MarketActivity", () => {
  it("is a section titled at the size of the others, with sectors in the order given", () => {
    renderIt();
    expect(screen.getByRole("heading", { level: 2, name: "market activity" })).toBeInTheDocument();
    const names = [...document.querySelectorAll(".itx-activity-sector .itx-board-label")].map(
      (l) => l.childNodes[0]?.textContent,
    );
    expect(names).toEqual(["software", "media"]);
    // Each says its share of the board's open bounty: 2100 of 2600.
    expect(block("software").getByText(/80\.8% of open bounty/)).toBeInTheDocument();
    expect(block("media").getByText(/19\.2% of open bounty/)).toBeInTheDocument();
  });

  it("shows four markets of a sector at a time, and pages to the rest", async () => {
    const user = userEvent.setup();
    renderIt();
    // The tiles, not the pager's own two buttons, which share the block.
    const tiles = () =>
      block("software")
        .getAllByRole("button")
        .filter((b) => b.classList.contains("itx-activity-card"))
        .map((b) => b.textContent?.match(/^[a-z-]+/)?.[0]);
    expect(tiles()).toEqual(["python", "rust", "sql", "cpp"]);
    expect(block("software").getByText("1–4 of 6")).toBeInTheDocument();

    await user.click(block("software").getByRole("button", { name: /next page of software activity/i }));
    expect(tiles()).toEqual(["testing", "prover"]);
    expect(block("software").getByText("5–6 of 6")).toBeInTheDocument();

    // A sector that fits on one page has no pager at all.
    expect(block("media").queryByText(/of 2/)).toBeNull();
    expect(block("media").queryByRole("button", { name: /page of media activity/i })).toBeNull();
  });

  it("labels a tile by its market alone, keeping the full tag as the title", () => {
    renderIt();
    const tile = block("software").getByRole("button", { name: /^python/ });
    expect(tile).toHaveAttribute("title", "software/python");
    expect(tile).toHaveTextContent(/^python/);
    expect(tile.textContent).not.toContain("software/");
  });

  it("opens a market's chart from its tile, by click or by keyboard", async () => {
    const user = userEvent.setup();
    const { onOpen } = renderIt();
    await user.click(block("software").getByRole("button", { name: /^rust/ }));
    expect(onOpen).toHaveBeenLastCalledWith("software/rust");

    block("media").getByRole("button", { name: /^ocr/ }).focus();
    await user.keyboard("{Enter}");
    expect(onOpen).toHaveBeenLastCalledWith("media/ocr");
  });

  it("renders nothing for an empty board rather than an empty section", () => {
    const { container } = renderIt([]);
    expect(container.querySelector(".itx-market-activity")).toBeNull();
  });
});

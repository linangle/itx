import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
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

  it("lays every market of a sector out in one row, with the overview's arrows", () => {
    renderIt();
    // Every tile is in the row -- it scrolls, the way the market
    // overview's does, rather than paging -- biggest open bounty first.
    const tiles = block("software")
      .getAllByRole("button")
      .filter((b) => b.classList.contains("itx-activity-card"))
      .map((b) => b.textContent?.match(/^[a-z-]+/)?.[0]);
    expect(tiles).toEqual(["python", "rust", "sql", "cpp", "testing", "prover"]);
    expect(block("software").getByRole("list")).toHaveClass("itx-activity-row");

    // The arrows and the position beside them. jsdom lays nothing out,
    // so where the row sits is the hook's own tests' business; what is
    // pinned here is that each sector has its pair and says its count.
    expect(block("software").getByRole("button", { name: /previous software markets/i })).toBeInTheDocument();
    expect(block("software").getByRole("button", { name: /next software markets/i })).toBeInTheDocument();
    expect(block("software").getByText(/of 6$/)).toBeInTheDocument();
    expect(block("media").getByText(/of 2$/)).toBeInTheDocument();
  });

  it("shows the top three sectors whole, a fourth faded, and opens the rest on request", async () => {
    const user = userEvent.setup();
    const many = ["software", "media", "data", "ml", "writing"].map((name, i) =>
      sector(name, [market(`${name}/a`, 500 - i * 100), market(`${name}/b`, 100)]),
    );
    const { container } = renderIt(many);

    // Three blocks in the section proper, and the fourth inside the
    // teaser -- present to be seen, hidden from assistive tech and the
    // pointer because it is a picture of the next block, not the block.
    const whole = () =>
      [...container.querySelectorAll(".itx-market-activity > .itx-activity-sector .itx-board-label")].map(
        (l) => l.childNodes[0]?.textContent,
      );
    expect(whole()).toEqual(["software", "media", "data"]);
    const teaser = container.querySelector(".itx-activity-teaser");
    expect(teaser).toHaveAttribute("aria-hidden", "true");
    expect(teaser?.textContent).toContain("ml");
    expect(container.textContent).not.toContain("writing");

    const expand = screen.getByRole("button", { name: /all 5 sectors/i });
    expect(expand).toHaveAttribute("aria-expanded", "false");
    await user.click(expand);
    expect(whole()).toEqual(["software", "media", "data", "ml", "writing"]);
    expect(container.querySelector(".itx-activity-teaser")).toBeNull();
    // Five fit on one page, so there is no pager to offer.
    expect(screen.queryByRole("button", { name: /page of sectors/i })).toBeNull();

    // And back.
    await user.click(screen.getByRole("button", { name: /top 3 sectors/i }));
    expect(whole()).toEqual(["software", "media", "data"]);
  });

  it("opens the rest ten at a time rather than all at once", async () => {
    const user = userEvent.setup();
    const many = Array.from({ length: 23 }, (_, i) =>
      sector(`s${String(i).padStart(2, "0")}`, [market(`s${i}/a`, 1000 - i)]),
    );
    const { container } = renderIt(many);
    const whole = () =>
      [...container.querySelectorAll(".itx-market-activity > .itx-activity-sector .itx-board-label")].map(
        (l) => l.childNodes[0]?.textContent,
      );

    await user.click(screen.getByRole("button", { name: /all 23 sectors/i }));
    expect(whole()).toHaveLength(10);
    expect(whole()[0]).toBe("s00");
    expect(screen.getByText("1–10 of 23")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /previous page of sectors/i })).toBeDisabled();

    await user.click(screen.getByRole("button", { name: /next page of sectors/i }));
    expect(whole()[0]).toBe("s10");
    expect(screen.getByText("11–20 of 23")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /next page of sectors/i }));
    expect(whole()).toEqual(["s20", "s21", "s22"]);
    expect(screen.getByText("21–23 of 23")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /next page of sectors/i })).toBeDisabled();

    // Closing goes back to the top three, whatever page was open.
    await user.click(screen.getByRole("button", { name: /top 3 sectors/i }));
    expect(whole()).toEqual(["s00", "s01", "s02"]);
    expect(screen.queryByRole("button", { name: /page of sectors/i })).toBeNull();
  });

  describe("drawing charts", () => {
    afterEach(() => {
      vi.unstubAllGlobals();
    });

    it("draws a tile's chart only once the tile has been on screen", () => {
      // jsdom lays nothing out, so every element is given a width; and
      // it has no IntersectionObserver, so one is stood in that lets the
      // test say when a tile came into view.
      const widths = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientWidth");
      Object.defineProperty(HTMLElement.prototype, "clientWidth", { configurable: true, get: () => 400 });
      type Callback = (entries: Array<{ isIntersecting: boolean }>) => void;
      const seen: Callback[] = [];
      vi.stubGlobal(
        "IntersectionObserver",
        class {
          constructor(cb: Callback) {
            seen.push(cb);
          }
          observe() {}
          disconnect() {}
          unobserve() {}
          takeRecords() {
            return [];
          }
        },
      );
      try {
        const { container } = renderIt();
        // Eight tiles, eight observers, and no chart yet.
        expect(container.querySelectorAll(".itx-activity-card")).toHaveLength(8);
        expect(container.querySelectorAll("svg.itx-chart")).toHaveLength(0);

        // The first three scroll into view.
        act(() => seen.slice(0, 3).forEach((cb) => cb([{ isIntersecting: true }])));
        expect(container.querySelectorAll("svg.itx-chart")).toHaveLength(3);

        // Leaving again does not undraw them.
        act(() => seen.slice(0, 3).forEach((cb) => cb([{ isIntersecting: false }])));
        expect(container.querySelectorAll("svg.itx-chart")).toHaveLength(3);
      } finally {
        if (widths) Object.defineProperty(HTMLElement.prototype, "clientWidth", widths);
      }
    });
  });

  it("has nothing to expand on a board of three sectors or fewer", () => {
    renderIt();
    expect(screen.queryByRole("button", { name: /sectors$/i })).toBeNull();
    expect(document.querySelector(".itx-activity-teaser")).toBeNull();
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

import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";
import NewsroomPage from "./NewsroomPage";
import { STORIES, orderStories } from "../../lib/newsroomSample";
import { SAMPLES } from "../../lib/predictionSample";
import * as hub from "../../lib/hub";

vi.mock("../../lib/hub", async (importOriginal) => ({
  ...(await importOriginal<typeof hub>()),
  listLatestTasks: vi.fn().mockResolvedValue([]),
}));

function renderPage() {
  return render(
    <MemoryRouter initialEntries={["/newsroom"]}>
      <NewsroomPage />
    </MemoryRouter>,
  );
}

/** The headlines the page is showing, lead first. The lead is a heading
 * and the rest are table rows, and the point of reading them as one list
 * is that a reader sees one feed rather than a story and a table. */
function headlines(container: HTMLElement): string[] {
  const lead = container.querySelector(".itx-nrpage-headline")?.textContent ?? "";
  const rows = [...container.querySelectorAll(".itx-nr-headline")].map(
    (cell) => cell.textContent ?? "",
  );
  return [lead, ...rows];
}

describe("NewsroomPage", () => {
  it("carries the whole pool, where the board shows its top five", () => {
    const { container } = renderPage();

    const main = screen.getByRole("main", { name: "Newsroom" });
    expect(main).toHaveClass("itx-board", "itx-subpage");
    expect(headlines(container)).toHaveLength(STORIES.length);
    expect(screen.getByText(/every story below is authored/i)).toBeInTheDocument();
  });

  it("leads on the most-read story and numbers the rest under it", () => {
    const { container } = renderPage();

    const ranked = orderStories(STORIES, "views");
    expect(headlines(container)).toEqual(ranked.map((s) => s.headline));

    // The lead is rank one, so the table starts at two -- the page holds
    // one feed, not a story and a separate list.
    const ranks = [...container.querySelectorAll("tbody .itx-board-rank")].map(
      (cell) => cell.textContent,
    );
    expect(ranks[0]).toBe("2");
    expect(ranks.at(-1)).toBe(String(STORIES.length));
  });

  it("reorders to the newest filing, lead and all", async () => {
    const user = userEvent.setup();
    const { container } = renderPage();

    const order = screen.getByRole("group", { name: "order" });
    await user.click(within(order).getByRole("button", { name: "newest" }));

    const newest = orderStories(STORIES, "latest");
    expect(headlines(container)).toEqual(newest.map((s) => s.headline));
  });

  it("cuts the feed down to one desk", async () => {
    const user = userEvent.setup();
    const { container } = renderPage();

    const desks = screen.getByRole("group", { name: "desk" });
    await user.click(within(desks).getByRole("button", { name: /^weather/ }));

    const weather = orderStories(
      STORIES.filter((s) => s.category === "weather"),
      "views",
    );
    expect(headlines(container)).toEqual(weather.map((s) => s.headline));
  });

  it("walks from a reading to the price it moved", async () => {
    const user = userEvent.setup();
    renderPage();

    // The lead of the unfiltered feed carries a market, and the link
    // lands on that market's own card rather than at the top of a page
    // of nine.
    const lead = orderStories(STORIES, "views")[0];
    const market = SAMPLES.find((m) => m.key === lead.marketKey);
    expect(market).toBeDefined();
    expect(screen.getByRole("link", { name: /priced into/i })).toHaveAttribute(
      "href",
      `/predictions#market-${market!.key}`,
    );

    // A reading that priced nothing draws no link. Most reading is this,
    // and a dead "priced into" line under every story would be the page
    // promising more than the pool holds -- so the compute desk leads on
    // one, on purpose.
    const desks = screen.getByRole("group", { name: "desk" });
    await user.click(within(desks).getByRole("button", { name: /^compute/ }));
    expect(orderStories(STORIES.filter((s) => s.category === "compute"), "views")[0])
      .not.toHaveProperty("marketKey");
    expect(screen.queryByRole("link", { name: /priced into/i })).not.toBeInTheDocument();
  });
});

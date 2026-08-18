import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";
import PredictionsPage from "./PredictionsPage";
import { SAMPLES, boardMarkets, movedPct } from "../../lib/predictionSample";
import * as hub from "../../lib/hub";

// The page itself fetches nothing; the masthead it mounts asks for the
// tape's headlines, which is the one call to quiet down.
vi.mock("../../lib/hub", async (importOriginal) => ({
  ...(await importOriginal<typeof hub>()),
  listLatestTasks: vi.fn().mockResolvedValue([]),
}));

function renderPage() {
  return render(
    <MemoryRouter initialEntries={["/predictions"]}>
      <PredictionsPage />
    </MemoryRouter>,
  );
}

/** The header strip, read as label -> figure. The two counts on this
 * page can carry the same number (nine markets across nine desks), so a
 * bare `getByText("9")` would match two elements and fail on the wrong
 * thing. */
function stats(container: HTMLElement): Record<string, string> {
  const pairs = [...container.querySelectorAll(".itx-sub-stat")];
  return Object.fromEntries(
    pairs.map((pair) => [
      pair.querySelector("dt")?.textContent ?? "",
      pair.querySelector("dd")?.textContent ?? "",
    ]),
  );
}

/** The cards on the page, in the order they are laid out. Read off the
 * anchors rather than off the headings: the anchor is what the newsroom
 * links to, so a test that reads them is also checking the thing those
 * links depend on. */
function cardKeys(container: HTMLElement): string[] {
  return [...container.querySelectorAll(".itx-pmpage-cell")].map((cell) =>
    cell.id.replace(/^market-/, ""),
  );
}

describe("PredictionsPage", () => {
  it("carries the whole pool, where the board's row carries its head", () => {
    const { container } = renderPage();

    const main = screen.getByRole("main", { name: "Prediction market" });
    expect(main).toHaveClass("itx-board", "itx-subpage");
    expect(cardKeys(container)).toHaveLength(SAMPLES.length);
    expect(SAMPLES.length).toBeGreaterThan(boardMarkets().length);

    // Every card says on its face that its odds are authored, exactly as
    // the board's do -- a page of nine quoting cards looks live whether
    // or not it is.
    expect(screen.getAllByText(/sample market/i)).toHaveLength(SAMPLES.length);
    expect(screen.getByText(/every market below is authored/i)).toBeInTheDocument();
  });

  it("counts what it is showing in the strip over it", () => {
    const { container } = renderPage();

    // Of the whole pool, because the page opens unfiltered.
    const staked = SAMPLES.reduce((sum, m) => sum + m.volumeItx, 0);
    expect(stats(container)).toMatchObject({
      markets: String(SAMPLES.length),
      "itx staked": staked.toLocaleString("en-US"),
    });
  });

  it("cuts the pool down to one desk, figures and all", async () => {
    const user = userEvent.setup();
    const { container } = renderPage();

    const desks = screen.getByRole("group", { name: "desk" });
    await user.click(within(desks).getByRole("button", { name: /^energy/ }));

    const energy = SAMPLES.filter((m) => m.category === "energy");
    expect(cardKeys(container)).toEqual(energy.map((m) => m.key));
    // The strip counts the desk, not the pool: a figure that ignored the
    // filter above it would be a number about a different page.
    expect(stats(container).markets).toBe(String(energy.length));
    // The pressed pill is the one the reader chose, and it is the only
    // one pressed -- this is a filter, not a set of toggles.
    expect(within(desks).getByRole("button", { name: /^energy/ })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    expect(
      within(desks).getAllByRole("button", { pressed: true }),
    ).toHaveLength(1);
  });

  it("reorders the pool without changing what is in it", async () => {
    const user = userEvent.setup();
    const { container } = renderPage();

    // Busiest first is the page's own default, so the opening order is
    // already an assertion about `orderMarkets`.
    const byVolume = [...SAMPLES].sort((a, b) => b.volumeItx - a.volumeItx);
    expect(cardKeys(container)).toEqual(byVolume.map((m) => m.key));

    const order = screen.getByRole("group", { name: "order" });
    await user.click(within(order).getByRole("button", { name: "moved most" }));

    const byMove = [...SAMPLES].sort((a, b) => movedPct(b) - movedPct(a));
    expect(cardKeys(container)).toEqual(byMove.map((m) => m.key));
  });

  it("gives every card the anchor the newsroom links to", () => {
    // A story that moved a price links to `#market-<key>`; if the ids
    // here drift, that link silently lands at the top of the page.
    const { container } = renderPage();
    expect(new Set(cardKeys(container)).size).toBe(SAMPLES.length);
    for (const market of SAMPLES) {
      expect(container.querySelector(`#market-${market.key}`)).toBeInTheDocument();
    }
  });

  it("keeps the masthead, and every page it points at", () => {
    renderPage();

    expect(screen.getByText("internet traffic exchange")).toBeInTheDocument();
    const bar = screen.getByRole("navigation", { name: "Site pages" });
    for (const [name, href] of [
      ["main hub", "/tasks"],
      ["prediction market", "/predictions"],
      ["newsroom", "/newsroom"],
      ["leaderboard", "/leaderboard"],
    ]) {
      expect(within(bar).getByRole("link", { name })).toHaveAttribute("href", href);
    }
  });
});

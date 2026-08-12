import { render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";
import PredictionsPage from "./PredictionsPage";
import * as hub from "../../lib/hub";

// The page itself fetches nothing; the masthead it mounts asks for the
// tape's headlines, which is the one call to quiet down.
vi.mock("../../lib/hub", async (importOriginal) => ({
  ...(await importOriginal<typeof hub>()),
  listLatestTasks: vi.fn().mockResolvedValue([]),
}));

describe("PredictionsPage", () => {
  it("frames the future market: the masthead over an empty grid", () => {
    render(
      <MemoryRouter initialEntries={["/predictions"]}>
        <PredictionsPage />
      </MemoryRouter>,
    );

    // Empty on purpose -- the grid ground and the way back are the
    // deliverable at this stage, not a placeholder paragraph.
    const main = screen.getByRole("main", { name: "Prediction market" });
    expect(main).toHaveClass("itx-board", "itx-subpage");
    expect(main).toBeEmptyDOMElement();

    // The masthead rides along: the wordmark home, and the two section
    // links, so the page that is reached from the bar also carries it.
    expect(screen.getByText("internet traffic exchange")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "prediction market" })).toHaveAttribute(
      "href",
      "/predictions",
    );
    expect(screen.getByRole("link", { name: "newsroom" })).toHaveAttribute(
      "href",
      "/newsroom",
    );
  });
});

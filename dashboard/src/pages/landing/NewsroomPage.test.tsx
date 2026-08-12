import { render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";
import NewsroomPage from "./NewsroomPage";
import * as hub from "../../lib/hub";

vi.mock("../../lib/hub", async (importOriginal) => ({
  ...(await importOriginal<typeof hub>()),
  listLatestTasks: vi.fn().mockResolvedValue([]),
}));

describe("NewsroomPage", () => {
  it("frames the newsroom the same way as the market: masthead, empty grid", () => {
    render(
      <MemoryRouter initialEntries={["/newsroom"]}>
        <NewsroomPage />
      </MemoryRouter>,
    );

    const main = screen.getByRole("main", { name: "Newsroom" });
    expect(main).toHaveClass("itx-board", "itx-subpage");
    expect(main).toBeEmptyDOMElement();
    expect(screen.getByText("internet traffic exchange")).toBeInTheDocument();
  });
});

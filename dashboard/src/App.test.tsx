import { render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";
import App from "./App";
import * as hub from "./lib/hub";

// The landing page fetches the board on mount; none of that is what
// these tests are about.
vi.mock("./lib/hub", async (importOriginal) => ({
  ...(await importOriginal<typeof hub>()),
  getBoardSummary: vi.fn().mockResolvedValue({
    first_task_at: null,
    window_ms: 604_800_000,
    buckets: 24,
    total_tasks: 0,
    totals: { open_tasks: 0, open_bounty: 0, paid_tasks: 0, paid_bounty: 0, posted_series: [] },
    kinds: [],
    capabilities: [],
  }),
  listLatestTasks: vi.fn().mockResolvedValue([]),
  getLeaderboard: vi.fn().mockResolvedValue({ items: [], total: 0 }),
  getNames: vi.fn().mockResolvedValue({}),
  getMarketSeries: vi.fn().mockResolvedValue(null),
}));

function renderAt(path: string) {
  return render(
    <MemoryRouter initialEntries={[path]}>
      <App />
    </MemoryRouter>,
  );
}

describe("routing", () => {
  /** The retired sample sections were live URLs, so somebody may hold a
   * link to one. Without a catch-all an unmatched path renders literally
   * nothing -- `Routes` matches no branch and returns null, leaving a
   * blank document with no header and no way back. */
  it.each(["/predictions", "/newsroom", "/nothing-here"])(
    "sends %s to the board rather than to a blank page",
    async (path) => {
      const { container } = renderAt(path);
      await waitFor(() => expect(container.firstChild).not.toBeNull());
      expect(screen.getByText("internet traffic exchange")).toBeInTheDocument();
    },
  );

  it("still serves the pages that are meant to exist", async () => {
    renderAt("/");
    await waitFor(() => expect(screen.getByText("internet traffic exchange")).toBeInTheDocument());
    expect(screen.getByRole("heading", { name: /where machines come to work/i })).toBeInTheDocument();
  });
});

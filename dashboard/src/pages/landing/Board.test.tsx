import { render, screen, within } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";
import Board from "./Board";
import * as hub from "../../lib/hub";
import type { Page, TaskDto } from "../../lib/hub";
import type { AsyncState } from "../../hooks/useAsync";

vi.mock("../../lib/hub", async (importOriginal) => ({
  ...(await importOriginal<typeof hub>()),
  getLeaderboard: vi.fn().mockResolvedValue([]),
}));

const NOW = Date.now();
const HOUR = 3_600_000;

function task(overrides: Partial<TaskDto> & { capabilities: string[] }): TaskDto {
  return {
    id: crypto.randomUUID(),
    description: "a task",
    bounty: 1000,
    status: "Open",
    poster: "02poster",
    claimant: null,
    failed_attempts: 0,
    min_reputation: 0,
    close_reason: null,
    kind: "hash_match",
    created_at: new Date(NOW - HOUR).toISOString(),
    ...overrides,
  } as TaskDto;
}

/** Two tasks per market, an hour apart, so every market clears the
 * two-active-buckets bar and renders a real change rather than a dash. */
function market(capability: string, bounty: number): TaskDto[] {
  return [
    task({ capabilities: [capability], bounty, created_at: new Date(NOW - 3 * HOUR).toISOString() }),
    task({ capabilities: [capability], bounty, created_at: new Date(NOW - HOUR).toISOString() }),
  ];
}

function loaded(
  items: TaskDto[],
  partialOf?: number,
): AsyncState<Page<TaskDto> & { complete: boolean }> {
  return {
    data: {
      items,
      total: partialOf ?? items.length,
      complete: partialOf === undefined,
    },
    loading: false,
    error: null,
  } as AsyncState<Page<TaskDto> & { complete: boolean }>;
}

function renderBoard(items: TaskDto[], partialOf?: number) {
  const view = render(
    <MemoryRouter>
      <Board tasks={loaded(items, partialOf)} />
    </MemoryRouter>,
  );
  // A market name is on the board twice on purpose -- once as a row in
  // its sector's panel, once in the trends rail -- so anything asserting
  // about the carousel has to say so, or it matches both and throws.
  return {
    ...view,
    carousel: within(view.container.querySelector("#itx-board-markets") as HTMLElement),
  };
}

describe("Board", () => {
  const tasks = [
    ...market("python", 5000),
    ...market("web-dev", 3000),
    ...market("image-generation", 900),
    ...market("therapy", 100),
  ];

  it("panels the board by sector, with each sector's markets as its rows", async () => {
    renderBoard(tasks);

    // The sector is the panel; the markets inside it are the rows. Read
    // through the panel rather than the document, so a stray "python"
    // elsewhere on the board can't satisfy this.
    const coding = (await screen.findAllByRole("table")).find((t) =>
      within(t).queryByRole("link", { name: "python" }),
    );
    expect(coding).toBeDefined();
    expect(within(coding!).getByRole("link", { name: "web-dev" })).toBeInTheDocument();
    // Biggest market first, as the carousel orders sectors.
    const rows = within(coding!).getAllByRole("row").slice(1);
    expect(rows.map((r) => r.textContent?.match(/^[a-z-]+/)?.[0])).toEqual(["python", "web-dev"]);
  });

  it("heads the market column with the market, not the agent", async () => {
    renderBoard(tasks);
    expect(await screen.findAllByRole("columnheader", { name: "market" })).not.toHaveLength(0);
    expect(screen.queryByRole("columnheader", { name: "agent" })).not.toBeInTheDocument();
  });

  it("links a market row to that market's tasks", async () => {
    const { carousel } = renderBoard(tasks);
    expect(await carousel.findByRole("link", { name: "python" })).toHaveAttribute(
      "href",
      "/tasks?capability=python",
    );
  });

  it("lists sectors in the rail, ranked by the money in them", async () => {
    renderBoard(tasks);
    const nav = screen.getByRole("navigation", { name: /board sections/i });
    const sectors = within(nav)
      .getAllByRole("button")
      .map((b) => b.textContent);
    expect(sectors).toEqual(["coding", "creative", "conversation"]);
  });

  it("quotes sectors in the strip rather than protocol task kinds", async () => {
    renderBoard(tasks);
    // "hash match" et al describe how a task is verified, not what kind
    // of work it is, and no longer head the board.
    expect(screen.queryByText(/hash match/i)).not.toBeInTheDocument();
    expect(screen.getAllByText("coding").length).toBeGreaterThan(0);
  });

  it("says so when the page walk stopped before the end of the board", () => {
    // Every figure here is a sum over the task list, so a truncated walk
    // misstates the market rather than showing less of it. Silence is
    // the one thing this must not do.
    renderBoard(tasks, 30_000);
    const note = screen.getByRole("status");
    expect(note).toHaveTextContent(/showing the oldest 8 of 30,000 tasks/i);
    // Which end is missing is the actionable half: the hub sorts
    // oldest-first and the walk slices from the front.
    expect(note).toHaveTextContent(/newest work is missing/i);
  });

  it("stays quiet when the walk saw the whole board", () => {
    renderBoard(tasks);
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });

  it("files a tag the taxonomy doesn't know under other, rather than dropping it", async () => {
    const { carousel } = renderBoard(market("haruspicy", 100));
    const nav = screen.getByRole("navigation", { name: /board sections/i });
    expect(within(nav).getByRole("button", { name: "other" })).toBeInTheDocument();
    expect(await carousel.findByRole("link", { name: "haruspicy" })).toBeInTheDocument();
  });
});

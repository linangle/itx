import { render, screen, within } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";
import Board from "./Board";
import * as hub from "../../lib/hub";
import type { BoardSummaryDto, CapabilitySummaryDto, TaskDto } from "../../lib/hub";
import type { AsyncState } from "../../hooks/useAsync";

vi.mock("../../lib/hub", async (importOriginal) => ({
  ...(await importOriginal<typeof hub>()),
  getLeaderboard: vi.fn().mockResolvedValue([]),
}));

const BUCKETS = 24;

/** One market's row in a summary. Two active buckets, an early one and a
 * late one, so it clears the "too thin to report" bar and renders a real
 * change rather than a dash. */
function market(capability: string, openBounty: number): CapabilitySummaryDto {
  const posted = new Array(BUCKETS).fill(0);
  const bounty = new Array(BUCKETS).fill(0);
  posted[2] = 1;
  posted[BUCKETS - 2] = 3;
  bounty[2] = openBounty;
  bounty[BUCKETS - 2] = openBounty * 3;
  return {
    capability,
    open: 2,
    open_bounty: openBounty,
    posted: 4,
    posted_series: posted,
    bounty_series: bounty,
  };
}

function summary(capabilities: CapabilitySummaryDto[]): AsyncState<BoardSummaryDto> {
  return {
    data: {
      window_ms: 7 * 24 * 60 * 60 * 1000,
      buckets: BUCKETS,
      total_tasks: 100,
      totals: {
        open_tasks: 10,
        open_bounty: 1000,
        paid_tasks: 5,
        paid_bounty: 500,
        posted_series: new Array(BUCKETS).fill(1),
      },
      kinds: [],
      capabilities,
    },
    loading: false,
    error: null,
  } as AsyncState<BoardSummaryDto>;
}

function noTasks(): AsyncState<{ items: TaskDto[] }> {
  return { data: { items: [] }, loading: false, error: null } as AsyncState<{
    items: TaskDto[];
  }>;
}

function renderBoard(
  capabilities: CapabilitySummaryDto[],
  latest: AsyncState<{ items: TaskDto[] }> = noTasks(),
) {
  const view = render(
    <MemoryRouter>
      <Board summary={summary(capabilities)} latest={latest} />
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
  const capabilities = [
    market("python", 5000),
    market("web-dev", 3000),
    market("image-generation", 900),
    market("therapy", 100),
  ];

  it("panels the board by sector, with each sector's markets as its rows", async () => {
    renderBoard(capabilities);

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
    renderBoard(capabilities);
    expect(await screen.findAllByRole("columnheader", { name: "market" })).not.toHaveLength(0);
    expect(screen.queryByRole("columnheader", { name: "agent" })).not.toBeInTheDocument();
  });

  it("links a market row to that market's tasks", async () => {
    const { carousel } = renderBoard(capabilities);
    expect(await carousel.findByRole("link", { name: "python" })).toHaveAttribute(
      "href",
      "/tasks?capability=python",
    );
  });

  it("lists sectors in the rail, ranked by the money in them", () => {
    renderBoard(capabilities);
    const nav = screen.getByRole("navigation", { name: /board sections/i });
    const sectors = within(nav)
      .getAllByRole("button")
      .map((b) => b.textContent);
    expect(sectors).toEqual(["coding", "creative", "conversation"]);
  });

  it("quotes sectors in the strip rather than protocol task kinds", () => {
    renderBoard(capabilities);
    // "hash match" et al describe how a task is verified, not what kind
    // of work it is, and no longer head the board.
    expect(screen.queryByText(/hash match/i)).not.toBeInTheDocument();
    expect(screen.getAllByText("coding").length).toBeGreaterThan(0);
  });

  it("files a tag the taxonomy doesn't know under other, rather than dropping it", async () => {
    const { carousel } = renderBoard([market("haruspicy", 100)]);
    const nav = screen.getByRole("navigation", { name: /board sections/i });
    expect(within(nav).getByRole("button", { name: "other" })).toBeInTheDocument();
    expect(await carousel.findByRole("link", { name: "haruspicy" })).toBeInTheDocument();
  });

  it("takes the tape's headlines from the task feed, not from the summary", () => {
    // The summary carries totals and has no task identities in it, so
    // the "latest" panel is the one thing here that still needs tasks.
    const feed = {
      data: {
        items: [
          {
            id: "abc",
            description: "Fine-tune a sentiment classifier",
            bounty: 100,
            created_at: new Date().toISOString(),
          } as TaskDto,
        ],
      },
      loading: false,
      error: null,
    } as AsyncState<{ items: TaskDto[] }>;

    renderBoard(capabilities, feed);
    expect(
      screen.getByRole("link", { name: "Fine-tune a sentiment classifier" }),
    ).toHaveAttribute("href", "/tasks/abc");
  });
});

import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter, useLocation } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";
import Board from "./Board";
import * as hub from "../../lib/hub";
import type { BoardSummaryDto, CapabilitySummaryDto, TaskDto } from "../../lib/hub";
import type { AsyncState } from "../../hooks/useAsync";

vi.mock("../../lib/hub", async (importOriginal) => ({
  ...(await importOriginal<typeof hub>()),
  getLeaderboard: vi.fn().mockResolvedValue({ items: [], total: 0 }),
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
      first_task_at: new Date(Date.now() - 86_400_000).toISOString(),
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

/** Reports the router's current query string into the DOM.
 *
 * `MemoryRouter` keeps history in memory and never touches
 * `window.location`, so a test asserting that the board wrote `?market=`
 * has to read it back from the router rather than from the window. */
function LocationProbe() {
  return <span data-testid="search">{useLocation().search}</span>;
}

function renderBoard(
  capabilities: CapabilitySummaryDto[],
  latest: AsyncState<{ items: TaskDto[] }> = noTasks(),
) {
  const view = render(
    <MemoryRouter>
      <Board summary={summary(capabilities)} latest={latest} />
      <LocationProbe />
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

/** A board with markets in four sectors, shared by the blocks below. */
const capabilitiesFixture = [
  market("python", 5000),
  market("web-dev", 3000),
  market("image-generation", 900),
  market("therapy", 100),
];

describe("Board", () => {
  const capabilities = capabilitiesFixture;

  it("panels the board by sector, with each sector's markets as its rows", async () => {
    renderBoard(capabilities);

    // The sector is the panel; the markets inside it are the rows. Read
    // through the panel rather than the document, so a stray "python"
    // elsewhere on the board can't satisfy this.
    const coding = (await screen.findAllByRole("table")).find((t) =>
      within(t).queryByRole("button", { name: "python" }),
    );
    expect(coding).toBeDefined();
    expect(within(coding!).getByRole("button", { name: "web-dev" })).toBeInTheDocument();
    // Biggest market first, as the carousel orders sectors.
    const rows = within(coding!).getAllByRole("row").slice(1);
    expect(rows.map((r) => r.textContent?.match(/^[a-z-]+/)?.[0])).toEqual(["python", "web-dev"]);
  });

  it("caps a sector's panel at twelve markets however many it holds", () => {
    // No sector in the taxonomy is this wide -- coding, the largest, has
    // nine -- which is exactly why the cap needs a test rather than an
    // eyeball. Unknown tags all file into "other", so fourteen of them
    // build one oversized panel without inventing a sector.
    const many = Array.from({ length: 14 }, (_, i) =>
      market(`haruspicy-${String.fromCharCode(97 + i)}`, 1000 - i),
    );

    const { carousel } = renderBoard(many);
    const rows = within(carousel.getAllByRole("table")[0]).getAllByRole("row").slice(1);
    expect(rows.length).toBe(12);
  });

  it("carries the prediction market sample above the tape, with the way out", () => {
    const { container } = renderBoard(capabilities);

    const section = screen.getByRole("region", { name: "Prediction market" });
    // The two outcomes quote complementary odds and reciprocal payouts
    // -- a binary market's one price implies everything else on the
    // card. Read through the table: the legend repeats the labels.
    const table = within(section).getByRole("table");
    expect(within(table).getByText("under 15")).toBeInTheDocument();
    expect(within(table).getByText("72%")).toBeInTheDocument();
    expect(within(table).getByText("1.39x")).toBeInTheDocument();
    expect(within(table).getByText("15 or more")).toBeInTheDocument();
    expect(within(table).getByText("3.57x")).toBeInTheDocument();

    // And it says on its face that the odds are authored. A card
    // quoting a price and a volume looks live whether or not it is.
    expect(within(section).getByText(/sample market/i)).toBeInTheDocument();

    // The label row's arrow goes to the full page.
    expect(
      within(section).getByRole("link", { name: /full prediction market/i }),
    ).toHaveAttribute("href", "/predictions");

    // Above the tape, below the markets: the sample sits between the
    // carousel and the latest feed in the column that scrolls.
    const latest = container.querySelector("#itx-board-latest");
    expect(
      // eslint-disable-next-line no-bitwise
      section.compareDocumentPosition(latest!) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });

  it("keeps the sample's odds pills quiet: spans, not trade buttons", () => {
    renderBoard(capabilities);
    const section = screen.getByRole("region", { name: "Prediction market" });
    const table = within(section).getByRole("table");
    // Nothing in the protocol can take a trade, so nothing on the card
    // may offer one. The only buttons in the section are the pager's,
    // and both are disabled -- one sample, nowhere to flip.
    within(section)
      .getAllByRole("button")
      .forEach((b) => expect(b).toBeDisabled());
    expect(within(table).queryByRole("button")).not.toBeInTheDocument();
  });

  it("heads the market column with the market, not the agent", async () => {
    renderBoard(capabilities);
    expect(await screen.findAllByRole("columnheader", { name: "market" })).not.toHaveLength(0);
    expect(screen.queryByRole("columnheader", { name: "agent" })).not.toBeInTheDocument();
  });

  it("opens a market's chart in place, rather than navigating to its tasks", async () => {
    const user = userEvent.setup();
    const { carousel } = renderBoard(capabilities);

    await user.click(await carousel.findByRole("button", { name: "python" }));

    // The chart takes the middle column and the carousel goes with it.
    // The heading's accessible name carries its sub-line too ("python
    // coding"), same as a sector panel's label does.
    expect(await screen.findByRole("heading", { name: /^python/ })).toBeInTheDocument();
    expect(document.querySelector("#itx-board-markets")).toBeNull();
    // ...while the rail either side stays exactly where it was. That is
    // the whole point of doing this in place rather than as a route.
    expect(screen.getByRole("navigation", { name: /board sections/i })).toBeInTheDocument();
    expect(screen.getByLabelText(/search agents/i)).toBeInTheDocument();
  });

  it("puts the open market in the URL, so a chart is a link", async () => {
    const user = userEvent.setup();
    const { carousel } = renderBoard(capabilities);
    await user.click(await carousel.findByRole("button", { name: "python" }));
    expect(screen.getByTestId("search").textContent).toContain("market=python");
  });

  it("goes back to the carousel from the chart", async () => {
    const user = userEvent.setup();
    const { carousel } = renderBoard(capabilities);
    await user.click(await carousel.findByRole("button", { name: "python" }));

    await user.click(await screen.findByRole("button", { name: /market overview/i }));
    expect(document.querySelector("#itx-board-markets")).not.toBeNull();
  });

  it("lists sectors under the overview, ranked by the money in them", async () => {
    const user = userEvent.setup();
    renderBoard(capabilities);
    const nav = screen.getByRole("navigation", { name: /board sections/i });

    // They are a subsection of the overview, so they are not there until
    // the overview is the thing being worked with.
    expect(within(nav).queryAllByRole("button")).toHaveLength(0);

    await user.click(within(nav).getByRole("link", { name: "market overview" }));
    const sectors = within(nav)
      .getAllByRole("button")
      .map((b) => b.textContent);
    expect(sectors).toEqual(["coding", "creative", "conversation"]);
  });

  it("nests the sectors inside the overview's own entry", async () => {
    const user = userEvent.setup();
    renderBoard(capabilities);
    const nav = screen.getByRole("navigation", { name: /board sections/i });
    const overview = within(nav).getByRole("link", { name: "market overview" });
    await user.click(overview);

    // Inside the overview's list item, not a sibling list under a
    // heading of its own -- which is what made "sectors" and the
    // "breakdown" link read as two names for the same thing.
    const item = overview.closest("li")!;
    expect(within(item).getByRole("button", { name: "coding" })).toBeInTheDocument();
    expect(overview).toHaveAttribute("aria-expanded", "true");
  });

  it("quotes sectors in the strip rather than protocol task kinds", () => {
    renderBoard(capabilities);
    // "hash match" et al describe how a task is verified, not what kind
    // of work it is, and no longer head the board.
    expect(screen.queryByText(/hash match/i)).not.toBeInTheDocument();
    expect(screen.getAllByText("coding").length).toBeGreaterThan(0);
  });

  it("files a tag the taxonomy doesn't know under other, rather than dropping it", async () => {
    const user = userEvent.setup();
    const { carousel } = renderBoard([market("haruspicy", 100)]);
    const nav = screen.getByRole("navigation", { name: /board sections/i });
    await user.click(within(nav).getByRole("link", { name: "market overview" }));
    expect(within(nav).getByRole("button", { name: "other" })).toBeInTheDocument();
    expect(await carousel.findByRole("button", { name: "haruspicy" })).toBeInTheDocument();
  });

  /** A tape row's worth of task. Every field the row reads is set --
   * `capabilities` especially, which the hub always sends (it is not
   * optional in `TaskCommon`) and which the rest of `lib/` already
   * indexes without guarding. */
  function feedOf(overrides: Partial<TaskDto> = {}) {
    return {
      data: {
        items: [
          {
            id: "abc",
            description: "Fine-tune a sentiment classifier",
            bounty: 250_000_000,
            created_at: new Date().toISOString(),
            capabilities: ["machine-learning"],
            poster: "02aa11bb22cc33dd",
            status: "Open",
            claimant: null,
            ...overrides,
          } as TaskDto,
        ],
      },
      loading: false,
      error: null,
    } as AsyncState<{ items: TaskDto[] }>;
  }

  it("takes the tape's headlines from the task feed, not from the summary", () => {
    // The summary carries totals and has no task identities in it, so
    // the "latest" panel is the one thing here that still needs tasks.
    renderBoard(capabilities, feedOf());
    expect(
      screen.getByRole("link", { name: "Fine-tune a sentiment classifier" }),
    ).toHaveAttribute("href", "/tasks/abc");
  });

  it("carries the value, the market and the poster across the tape row", () => {
    const { container } = renderBoard(capabilities, feedOf());
    const row = container.querySelector(".itx-board-updates li") as HTMLElement;

    // Read off the cell rather than by text: the amount and its unit are
    // separate nodes, and how many decimals `formatItx` keeps is its
    // business, not this test's.
    expect(row.querySelector(".itx-board-amt")?.textContent).toMatch(/^2\.5\d*\s*itx$/);
    // The market it trades in, linked the same way a market row is.
    expect(within(row).getByRole("link", { name: "machine-learning" })).toHaveAttribute(
      "href",
      "/tasks?capability=machine-learning",
    );
    // And the agent who posted it, at the end of the row.
    const agent = within(row).getByTitle(/posted by 02aa11bb22cc33dd/);
    expect(agent).toHaveAttribute("href", "/agents/02aa11bb22cc33dd");
  });

  it("holds the market column open for work that carries no tag", () => {
    // Untagged work is unrestricted rather than belonging to a market
    // called "none", so the cell says so instead of collapsing and
    // pulling the columns beside it out of line.
    const { container } = renderBoard(capabilities, feedOf({ capabilities: [] }));
    const row = container.querySelector(".itx-board-updates li") as HTMLElement;
    expect(within(row).getByText("untagged")).toBeInTheDocument();
  });
});

describe("Board leaderboard", () => {
  // `rank` is the hub's, computed over the unfiltered field -- see
  // `LeaderboardEntryDto`. The rail renders it rather than counting rows.
  const agents = [
    { pubkey: "02aa", name: "CraggyGlacier", total_earned: 19_000_000_000 },
    { pubkey: "02bb", name: "RareAntelope", total_earned: 17_000_000_000 },
    { pubkey: "02cc", name: "SmoothMoth", total_earned: 15_000_000_000 },
  ].map((a, i) => ({ ...a, rank: i + 1, completed: 1, failed: 0, net_worth: 1 }));

  it("numbers the standings and keeps them in order", async () => {
    vi.mocked(hub.getLeaderboard).mockResolvedValue({ items: agents, total: agents.length } as never);
    const { container } = renderBoard(capabilitiesFixture);

    const rows = await screen.findAllByRole("row");
    const leaders = [...container.querySelectorAll(".itx-board-panel-leaders tbody tr")];
    expect(leaders.length).toBe(3);
    expect(rows.length).toBeGreaterThan(0);
    expect(leaders.map((t) => t.querySelector(".itx-board-rank")?.textContent)).toEqual([
      "1",
      "2",
      "3",
    ]);
  });

  it("pages the standings and carries the rank across pages", async () => {
    // A field bigger than one page: the pager appears, and page two's
    // first agent is 51st rather than 1st.
    const page1 = Array.from({ length: 50 }, (_, i) => ({
      pubkey: `02${i}`,
      name: `Agent${i}`,
      rank: i + 1,
      total_earned: (100 - i) * 1_000_000,
      completed: 1,
      failed: 0,
      net_worth: 1,
    }));
    vi.mocked(hub.getLeaderboard).mockResolvedValue({ items: page1, total: 120 } as never);

    const user = userEvent.setup();
    const { container } = renderBoard(capabilitiesFixture);
    await screen.findByText("Agent0");
    expect(screen.getByText(/1–50 of 120/)).toBeInTheDocument();

    vi.mocked(hub.getLeaderboard).mockResolvedValue({
      items: [{ ...page1[0], pubkey: "02x", name: "FiftyFirst", rank: 51 }],
      total: 120,
    } as never);
    await user.click(screen.getByRole("button", { name: /next page of agents/i }));

    await screen.findByText("FiftyFirst");
    const row = container.querySelector(".itx-board-panel-leaders tbody tr")!;
    expect(row.querySelector(".itx-board-rank")?.textContent).toBe("51");
    // The window the page covers, not how many rows came back -- a full
    // page two of a 120-agent field is 51 through 100.
    expect(screen.getByText(/51–100 of 120/)).toBeInTheDocument();
  });

  it("hides the pager when the whole field fits on one page", async () => {
    vi.mocked(hub.getLeaderboard).mockResolvedValue({ items: agents, total: 3 } as never);
    renderBoard(capabilitiesFixture);
    await screen.findByText("CraggyGlacier");
    // A pager over a complete list is a control that can only be
    // disabled.
    expect(screen.queryByRole("button", { name: /next page of agents/i })).not.toBeInTheDocument();
  });

  it("searches at the hub rather than filtering the page in hand", async () => {
    vi.mocked(hub.getLeaderboard).mockResolvedValue({ items: agents, total: agents.length } as never);
    const user = userEvent.setup();
    const { container } = renderBoard(capabilitiesFixture);
    await screen.findByText("SmoothMoth");

    // The hub answers with the matches and their true ranks. The trap
    // this guards: numbering the rows that came back would tell someone
    // who searched for the third-place agent that they are winning.
    vi.mocked(hub.getLeaderboard).mockResolvedValue({ items: [agents[2]], total: 1 } as never);
    await user.type(screen.getByLabelText(/search agents/i), "moth");

    await waitFor(() =>
      expect(vi.mocked(hub.getLeaderboard)).toHaveBeenLastCalledWith(0, 50, "moth"),
    );
    await waitFor(() => {
      const leaders = [...container.querySelectorAll(".itx-board-panel-leaders tbody tr")];
      expect(leaders).toHaveLength(1);
      expect(leaders[0].textContent).toContain("SmoothMoth");
      expect(leaders[0].querySelector(".itx-board-rank")?.textContent).toBe("3");
    });
  });
});

describe("Board sorting", () => {
  /** Three coding markets whose value and change orders disagree, so a
   * test cannot pass by accident on a table sorted the other way. */
  function codingMarkets(): CapabilitySummaryDto[] {
    const at = (early: number, late: number) => {
      const s = new Array(BUCKETS).fill(0);
      s[2] = early;
      s[BUCKETS - 2] = late;
      return s;
    };
    return [
      // value 400, change +300%
      { ...market("python", 1), bounty_series: at(100, 300), posted_series: at(1, 3) },
      // value 900, change +12.5%
      { ...market("rust", 1), bounty_series: at(400, 450), posted_series: at(1, 1) },
      // value 250, change -50%
      { ...market("sql", 1), bounty_series: at(200, 100), posted_series: at(2, 1) },
    ];
  }

  const names = (panel: HTMLElement) =>
    within(panel)
      .getAllByRole("row")
      .slice(1)
      .map((r) => r.textContent?.match(/^[a-z-]+/)?.[0]);

  function codingPanel(container: HTMLElement) {
    return within(container.querySelector("#itx-board-markets") as HTMLElement).getAllByRole(
      "table",
    )[0];
  }

  it("quotes each market's value beside its change", () => {
    const { container } = renderBoard(codingMarkets());
    const row = within(codingPanel(container)).getAllByRole("row")[1];
    // Biggest value first by default, so this is rust: 850 base units.
    expect(row.textContent).toContain("rust");
    expect(row.textContent).toMatch(/\+12\.50%/);
  });

  it("orders by value, largest first, before anyone touches a header", () => {
    const { container } = renderBoard(codingMarkets());
    expect(names(codingPanel(container))).toEqual(["rust", "python", "sql"]);
  });

  it("flips direction when the active column is clicked again", async () => {
    const user = userEvent.setup();
    const { container } = renderBoard(codingMarkets());
    const panel = codingPanel(container);

    await user.click(within(panel).getByRole("button", { name: /value/i }));
    expect(names(codingPanel(container))).toEqual(["sql", "python", "rust"]);

    await user.click(within(codingPanel(container)).getByRole("button", { name: /value/i }));
    expect(names(codingPanel(container))).toEqual(["rust", "python", "sql"]);
  });

  it("takes over at largest-first when the other column is clicked", async () => {
    const user = userEvent.setup();
    const { container } = renderBoard(codingMarkets());

    await user.click(within(codingPanel(container)).getByRole("button", { name: /change/i }));
    // Change order disagrees with value order, which is the point.
    expect(names(codingPanel(container))).toEqual(["python", "rust", "sql"]);

    await user.click(within(codingPanel(container)).getByRole("button", { name: /change/i }));
    expect(names(codingPanel(container))).toEqual(["sql", "rust", "python"]);
  });

  it("marks which column is sorting the table, and which way", async () => {
    const user = userEvent.setup();
    const { container } = renderBoard(codingMarkets());
    // By name rather than position: the sparkline's heading is
    // aria-hidden (it labels no figure of its own), so the columns and
    // the accessible headers are deliberately not one-to-one.
    const header = (name: RegExp) =>
      within(codingPanel(container)).getByRole("columnheader", { name });

    expect(header(/value/i)).toHaveAttribute("aria-sort", "descending");
    expect(header(/change/i)).toHaveAttribute("aria-sort", "none");

    await user.click(within(codingPanel(container)).getByRole("button", { name: /change/i }));
    expect(header(/value/i)).toHaveAttribute("aria-sort", "none");
    expect(header(/change/i)).toHaveAttribute("aria-sort", "descending");
  });

  it("sorts every sector's panel together, so the board stays comparable", async () => {
    const user = userEvent.setup();
    const { container } = renderBoard([...codingMarkets(), market("therapy", 100)]);
    const panels = () =>
      within(container.querySelector("#itx-board-markets") as HTMLElement).getAllByRole("table");

    await user.click(within(panels()[0]).getByRole("button", { name: /change/i }));
    // The conversation panel's own header reflects the same ordering,
    // rather than each panel keeping its own.
    expect(
      within(panels()[1]).getByRole("columnheader", { name: /change/i }),
    ).toHaveAttribute("aria-sort", "descending");
  });
});

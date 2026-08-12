import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router-dom";
import { beforeEach, describe, expect, it, vi } from "vitest";
import * as hub from "../../lib/hub";
import LeaderboardPage from "./LeaderboardPage";

vi.mock("../../lib/hub");

const KEY_A = "02" + "a".repeat(64);
const KEY_B = "03" + "b".repeat(64);

function entry(overrides: Partial<hub.LeaderboardEntryDto>): hub.LeaderboardEntryDto {
  return {
    pubkey: KEY_A,
    rank: 1,
    completed: 3,
    failed: 0,
    total_earned: 3_000,
    net_worth: 12_000,
    name: "SwiftWarlock",
    ...overrides,
  };
}

describe("terminal LeaderboardPage agent names", () => {
  beforeEach(() => {
    vi.mocked(hub.listAllTasks).mockResolvedValue({ items: [], total: 0, complete: true });
    // `Shell` wraps every terminal page in the site bar's live ticker,
    // so this has to be stubbed even though the test says nothing about
    // it -- `vi.mock` otherwise returns undefined and the ticker throws
    // before the table under test ever renders.
    vi.mocked(hub.listLatestTasks).mockResolvedValue([]);
  });

  it("shows the hub-assigned name alongside a truncated pubkey", async () => {
    vi.mocked(hub.getLeaderboard).mockResolvedValue({ items: [entry({})], total: 0 });

    render(
      <MemoryRouter>
        <LeaderboardPage />
      </MemoryRouter>,
    );

    expect(await screen.findByText("SwiftWarlock")).toBeInTheDocument();
    // The key stays on the row -- the name is a label, not a replacement
    // for the thing that actually identifies the agent.
    expect(screen.getByText("02aa…aaaa")).toBeInTheDocument();
    // and the full key is still recoverable, and still the link target
    const link = screen.getByRole("link", { name: /SwiftWarlock/ });
    expect(link).toHaveAttribute("title", KEY_A);
    expect(link).toHaveAttribute("href", `/agents/${KEY_A}`);
  });

  it("falls back to the pubkey for an agent the hub hasn't named", async () => {
    vi.mocked(hub.getLeaderboard).mockResolvedValue({
      items: [entry({ pubkey: KEY_B, name: null })],
      total: 1,
    });

    render(
      <MemoryRouter>
        <LeaderboardPage />
      </MemoryRouter>,
    );

    // `truncatePubkey`'s default 6/4 split, i.e. the pre-naming rendering
    expect(await screen.findByText("03bbbb…bbbb")).toBeInTheDocument();
    // Scoped to the table: `Shell` renders the site nav around every
    // terminal page, so an unscoped link query counts those too.
    const rows = within(screen.getByRole("table")).getAllByRole("link");
    expect(rows).toHaveLength(1);
    expect(rows[0]).toHaveAttribute("href", `/agents/${KEY_B}`);
  });

  it("renders a name and an unnamed agent side by side", async () => {
    vi.mocked(hub.getLeaderboard).mockResolvedValue({
      items: [
        entry({ pubkey: KEY_A, name: "AmberOtter" }),
        entry({ pubkey: KEY_B, name: null, total_earned: 10 }),
      ],
      total: 2,
    });

    render(
      <MemoryRouter>
        <LeaderboardPage />
      </MemoryRouter>,
    );

    expect(await screen.findByText("AmberOtter")).toBeInTheDocument();
    expect(screen.getByText("03bbbb…bbbb")).toBeInTheDocument();
    expect(within(screen.getByRole("table")).getAllByRole("link")).toHaveLength(2);
  });
});

describe("terminal LeaderboardPage paging", () => {
  /** A full page of distinct agents, so the pager has something to page. */
  function field(count: number, from = 0): hub.LeaderboardEntryDto[] {
    return Array.from({ length: count }, (_, i) => {
      const n = from + i;
      return entry({
        pubkey: "02" + String(n).padStart(64, "0"),
        // The hub's rank, which is what the row renders -- page two
        // arrives already numbered 51 upward.
        rank: n + 1,
        name: `Agent${n}`,
        total_earned: 100_000 - n,
      });
    });
  }

  beforeEach(() => {
    vi.mocked(hub.listAllTasks).mockResolvedValue({ items: [], total: 0, complete: true });
    vi.mocked(hub.listLatestTasks).mockResolvedValue([]);
  });

  it("is titled Leaderboard, not Agents", async () => {
    vi.mocked(hub.getLeaderboard).mockResolvedValue({ items: field(2), total: 2 });

    render(
      <MemoryRouter>
        <LeaderboardPage />
      </MemoryRouter>,
    );

    expect(await screen.findByRole("heading", { name: "Leaderboard" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Agents" })).toBeNull();
  });

  it("hides the pager when the whole field fits on one page", async () => {
    vi.mocked(hub.getLeaderboard).mockResolvedValue({ items: field(2), total: 2 });

    render(
      <MemoryRouter>
        <LeaderboardPage />
      </MemoryRouter>,
    );

    await screen.findByRole("table");
    expect(screen.queryByRole("button", { name: /Next/ })).toBeNull();
  });

  it("asks the hub for the next fifty and keeps the ranking counting up", async () => {
    const user = userEvent.setup();
    vi.mocked(hub.getLeaderboard).mockResolvedValue({
      items: field(hub.LEADERBOARD_PAGE_SIZE),
      total: 120,
    });

    render(
      <MemoryRouter>
        <LeaderboardPage />
      </MemoryRouter>,
    );

    await screen.findByRole("table");
    expect(screen.getByText("1–50 of 120")).toBeInTheDocument();
    expect(vi.mocked(hub.getLeaderboard)).toHaveBeenLastCalledWith(
      0,
      hub.LEADERBOARD_PAGE_SIZE,
      "",
    );

    vi.mocked(hub.getLeaderboard).mockResolvedValue({
      items: field(hub.LEADERBOARD_PAGE_SIZE, hub.LEADERBOARD_PAGE_SIZE),
      total: 120,
    });
    await user.click(screen.getByRole("button", { name: /Next/ }));

    // A page is a request, not a slice of something already fetched --
    // the hub ranks the whole field and serves fifty of it.
    expect(vi.mocked(hub.getLeaderboard)).toHaveBeenLastCalledWith(
      hub.LEADERBOARD_PAGE_SIZE,
      hub.LEADERBOARD_PAGE_SIZE,
      "",
    );
    expect(await screen.findByText("51–100 of 120")).toBeInTheDocument();

    // and the first row of page two is 51st, not 1st. Numbering each
    // page from one would report fifty agents as leading the board.
    const body = within(screen.getByRole("table")).getAllByRole("row")[1];
    expect(within(body).getAllByRole("cell")[0]).toHaveTextContent("51");
  });
});

describe("terminal LeaderboardPage search", () => {
  beforeEach(() => {
    vi.mocked(hub.listAllTasks).mockResolvedValue({ items: [], total: 0, complete: true });
    vi.mocked(hub.listLatestTasks).mockResolvedValue([]);
    vi.mocked(hub.getLeaderboard).mockResolvedValue({
      items: [entry({ name: "SwiftWarlock", rank: 1 })],
      total: 1,
    });
  });

  it("asks the hub rather than filtering the page it happens to hold", async () => {
    const user = userEvent.setup();
    render(
      <MemoryRouter>
        <LeaderboardPage />
      </MemoryRouter>,
    );
    await screen.findByRole("table");

    await user.type(screen.getByRole("searchbox", { name: /Search agents/ }), "otter");

    // Debounced, so this is one request for the word and not five for
    // its prefixes -- and it goes to the hub, which is the only party
    // that can see past the fifty rows in hand.
    await waitFor(() =>
      expect(vi.mocked(hub.getLeaderboard)).toHaveBeenLastCalledWith(
        0,
        hub.LEADERBOARD_PAGE_SIZE,
        "otter",
      ),
    );
  });

  it("keeps an agent's real standing in a search result", async () => {
    const user = userEvent.setup();
    render(
      <MemoryRouter>
        <LeaderboardPage />
      </MemoryRouter>,
    );
    await screen.findByRole("table");

    vi.mocked(hub.getLeaderboard).mockResolvedValue({
      items: [entry({ name: "AmberOtter", rank: 1908, pubkey: KEY_B })],
      total: 1,
    });
    await user.type(screen.getByRole("searchbox", { name: /Search agents/ }), "otter");

    expect(await screen.findByText("AmberOtter")).toBeInTheDocument();
    // The only match on screen is the 1,908th agent, not the 1st. A rank
    // is a position in the leaderboard, not in the search result.
    const row = within(screen.getByRole("table")).getAllByRole("row")[1];
    expect(within(row).getAllByRole("cell")[0]).toHaveTextContent("1,908");
  });

  it("says nothing matched rather than that nobody has earned", async () => {
    const user = userEvent.setup();
    render(
      <MemoryRouter>
        <LeaderboardPage />
      </MemoryRouter>,
    );
    await screen.findByRole("table");

    vi.mocked(hub.getLeaderboard).mockResolvedValue({ items: [], total: 0 });
    await user.type(screen.getByRole("searchbox", { name: /Search agents/ }), "zzz");

    expect(await screen.findByText(/No agent's name or key matches/)).toBeInTheDocument();
  });
});

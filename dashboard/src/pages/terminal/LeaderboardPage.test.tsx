import { render, screen, within } from "@testing-library/react";
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
    vi.mocked(hub.getLeaderboard).mockResolvedValue({ items: [entry({ pubkey: KEY_B, name: null })], total: 1 });

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
    vi.mocked(hub.getLeaderboard).mockResolvedValue({ items: [
      entry({ pubkey: KEY_A, name: "AmberOtter" }),
      entry({ pubkey: KEY_B, name: null, total_earned: 10 }),
    ], total: 2 });

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

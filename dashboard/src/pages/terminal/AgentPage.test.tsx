import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { beforeEach, describe, expect, it, vi } from "vitest";
import * as hub from "../../lib/hub";
import AgentPage from "./AgentPage";

vi.mock("../../lib/hub");

const KEY = "02" + "a".repeat(64);

/** A minimal hash-match task; overrides shape it into whatever the test
 * needs. Creation times step by the index so sort order is knowable. */
function task(n: number, overrides: Partial<hub.TaskDto> = {}): hub.TaskDto {
  return {
    id: `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`,
    description: `Task number ${n}`,
    bounty: 1_000,
    status: "Open",
    poster: KEY,
    claimant: null,
    failed_attempts: 0,
    min_reputation: 0,
    close_reason: null,
    capabilities: [],
    created_at: new Date(Date.UTC(2026, 0, 1) + n * 60_000).toISOString(),
    kind: "hash_match",
    ...overrides,
  } as hub.TaskDto;
}

function reputation(overrides: Partial<hub.ReputationDto> = {}): hub.ReputationDto {
  return {
    completed: 0,
    failed: 0,
    total_earned: 0,
    net_worth: 5_000,
    name: null,
    ...overrides,
  };
}

function renderPage() {
  return render(
    <MemoryRouter initialEntries={[`/agents/${KEY}`]}>
      <Routes>
        <Route path="/agents/:pubkey" element={<AgentPage />} />
      </Routes>
    </MemoryRouter>,
  );
}

beforeEach(() => {
  // `Shell` wraps the page in the site bar's ticker; stub it like every
  // other terminal-page test does or the ticker throws first.
  vi.mocked(hub.listLatestTasks).mockResolvedValue([]);
});

describe("AgentPage identity", () => {
  it("headlines the name for a key the leaderboard may not rank", async () => {
    // The regression this pins: a poster with no *paid* work yet. The
    // name must come from `/reputation`'s own registry lookup, not from
    // the agent's presence in any ranking -- the tape names this key,
    // so this page must too, or the site contradicts itself.
    vi.mocked(hub.getReputation).mockResolvedValue(reputation({ name: "LoftyGargoyle" }));
    vi.mocked(hub.listAllTasks).mockResolvedValue({
      items: [task(1)],
      total: 1,
      complete: true,
    });

    renderPage();

    expect(await screen.findByRole("heading", { name: "LoftyGargoyle" })).toBeInTheDocument();
    // The full key stays on the page -- a name is a label, and this is
    // the page a reader comes to *to* read the key.
    expect(screen.getByText(KEY)).toBeInTheDocument();
  });

  it("falls back to the key itself for an unnamed agent", async () => {
    vi.mocked(hub.getReputation).mockResolvedValue(reputation());
    vi.mocked(hub.listAllTasks).mockResolvedValue({
      items: [task(1)],
      total: 1,
      complete: true,
    });

    renderPage();

    expect(await screen.findByRole("heading", { name: KEY })).toBeInTheDocument();
  });
});

describe("AgentPage history paging", () => {
  beforeEach(() => {
    vi.mocked(hub.getReputation).mockResolvedValue(reputation({ name: "LoftyGargoyle" }));
  });

  it("pages a long history instead of stopping at ten", async () => {
    const user = userEvent.setup();
    vi.mocked(hub.listAllTasks).mockResolvedValue({
      items: Array.from({ length: 23 }, (_, n) => task(n)),
      total: 23,
      complete: true,
    });

    renderPage();

    // Newest first, ten to a page, and the range says where in the
    // history the reader is standing.
    const panel = (await screen.findByText("posted work")).closest("section")!;
    expect(await within(panel).findByText("1–10 of 23")).toBeInTheDocument();
    expect(within(panel).getByText("Task number 22")).toBeInTheDocument();
    expect(within(panel).queryByText("Task number 12")).toBeNull();

    await user.click(within(panel).getByRole("button", { name: "Next page" }));

    expect(within(panel).getByText("11–20 of 23")).toBeInTheDocument();
    expect(within(panel).getByText("Task number 12")).toBeInTheDocument();
    expect(within(panel).queryByText("Task number 22")).toBeNull();

    // The tail page holds the remainder, and the arrow stops there --
    // there is no page 4 of a 23-row history.
    await user.click(within(panel).getByRole("button", { name: "Next page" }));
    expect(within(panel).getByText("21–23 of 23")).toBeInTheDocument();
    expect(within(panel).getByRole("button", { name: "Next page" })).toBeDisabled();
  });

  it("shows no pager when the history fits on one page", async () => {
    vi.mocked(hub.listAllTasks).mockResolvedValue({
      items: [task(1), task(2)],
      total: 2,
      complete: true,
    });

    renderPage();

    const panel = (await screen.findByText("posted work")).closest("section")!;
    await within(panel).findByText("Task number 2");
    expect(within(panel).queryByRole("button", { name: "Next page" })).toBeNull();
  });
});

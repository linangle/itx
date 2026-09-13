import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";
import StatsRail from "./StatsRail";
import * as hub from "../../lib/hub";
import type { MarketSeriesDto } from "../../lib/hub";

vi.mock("../../lib/hub", async (importOriginal) => ({
  ...(await importOriginal<typeof hub>()),
  getMarketSeries: vi.fn(),
}));

const UNITS = 100_000_000;

function series(over: Partial<MarketSeriesDto> = {}): MarketSeriesDto {
  const zeros = [0, 0, 0, 0];
  return {
    capability: null,
    window_ms: 24 * 3_600_000,
    buckets: 4,
    start_ms: 1_000_000,
    end_ms: 1_000_000 + 24 * 3_600_000,
    posted_series: [1, 0, 2, 1],
    bounty_series: [1 * UNITS, 0, 2 * UNITS, 1 * UNITS],
    settled_series: [0, 1, 0, 2],
    paid_bounty_series: [0, 1 * UNITS, 0, 2 * UNITS],
    agents_series: [1, 2, 2, 3],
    fees_series: [...zeros],
    faucet_series: [2, 1, 0, 4],
    open_bounty_series: [0, 4 * UNITS, 0, 3 * UNITS],
    posted: 4,
    bounty: 4 * UNITS,
    settled: 3,
    paid_bounty: 3 * UNITS,
    agents: 5,
    fees: 3_000,
    faucet_grants: 7,
    faucet_itx: 7 * 50_000_000,
    open: 1,
    open_bounty: 9 * UNITS,
    first_task_at: new Date(1_000_000).toISOString(),
    ...over,
  };
}

/** A hub older than this page answers 200 without the series fields. */
function hubPredatingTheSeries(): MarketSeriesDto {
  const old = { ...series() } as Record<string, unknown>;
  for (const key of Object.keys(old)) if (key.endsWith("_series")) delete old[key];
  return old as unknown as MarketSeriesDto;
}

function renderRail(open: string | null = null, onOpen = vi.fn()) {
  const view = render(
    <MemoryRouter>
      <StatsRail open={open} onOpen={onOpen} />
    </MemoryRouter>,
  );
  return { ...view, onOpen };
}

describe("StatsRail", () => {
  it("asks the hub for the whole board, not for one kind of work", async () => {
    vi.mocked(hub.getMarketSeries).mockResolvedValue(series());
    renderRail();
    await waitFor(() => expect(hub.getMarketSeries).toHaveBeenCalled());
    const call = vi.mocked(hub.getMarketSeries).mock.calls[0][0];
    expect(call).not.toHaveProperty("capability");
  });

  it("lists every figure as an entry with its value and its change", async () => {
    vi.mocked(hub.getMarketSeries).mockResolvedValue(series());
    renderRail();
    const posted = await screen.findByRole("button", { name: /^bounty posted/ });
    expect(posted).toHaveTextContent(/4(\.0+)? itx/);
    expect(screen.getAllByRole("button", { name: /./ }).length).toBe(10);
    expect(screen.getByRole("button", { name: /^bounty paid/ })).toHaveTextContent(/3(\.0+)? itx/);
    expect(screen.getByRole("button", { name: /^tasks posted/ })).toHaveTextContent("4");
    expect(screen.getByRole("button", { name: /^completion rate/ })).toHaveTextContent("75.0%");
  });

  it("prints a count as a count, never with an itx suffix", async () => {
    vi.mocked(hub.getMarketSeries).mockResolvedValue(series());
    renderRail();
    await screen.findByRole("button", { name: /^bounty posted/ });
    for (const label of ["tasks posted", "tasks completed", "active agents", "faucet grants"]) {
      expect(screen.getByRole("button", { name: new RegExp(`^${label}`) }).textContent).not.toMatch(
        /\d\s*itx/,
      );
    }
  });

  it("opens a figure's chart by its key, and marks the one that is open", async () => {
    vi.mocked(hub.getMarketSeries).mockResolvedValue(series());
    const user = userEvent.setup();
    const { onOpen } = renderRail("bounty-paid");
    const paid = await screen.findByRole("button", { name: /^bounty paid/ });
    expect(paid).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("button", { name: /^bounty posted/ })).toHaveAttribute("aria-pressed", "false");

    await user.click(screen.getByRole("button", { name: /^tasks completed/ }));
    expect(onOpen).toHaveBeenCalledWith("tasks-completed");
  });

  it("carries the pages the nav used to, since nothing else links the task list", async () => {
    vi.mocked(hub.getMarketSeries).mockResolvedValue(series());
    renderRail();
    expect(screen.getByRole("link", { name: "all tasks" })).toHaveAttribute("href", "/tasks");
    expect(screen.getByRole("link", { name: "full leaderboard" })).toHaveAttribute("href", "/leaderboard");
  });

  it("says it could not reach the hub rather than showing zeroes", async () => {
    vi.mocked(hub.getMarketSeries).mockRejectedValue(new Error("down"));
    renderRail();
    expect(await screen.findByText(/couldn't reach the hub/i)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /^bounty posted/ })).not.toBeInTheDocument();
  });

  it("names an older hub rather than taking the page down", async () => {
    vi.mocked(hub.getMarketSeries).mockResolvedValue(hubPredatingTheSeries());
    renderRail();
    expect(await screen.findByText(/older than this page/i)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /^bounty posted/ })).not.toBeInTheDocument();
  });

  it("does not repeat the definition under every figure", async () => {
    // The notes moved to the stat chart, where there is room for them;
    // the rail is the figures.
    vi.mocked(hub.getMarketSeries).mockResolvedValue(series());
    const { container } = renderRail();
    await screen.findByRole("button", { name: /^bounty paid/ });
    expect(within(container).queryByText(/counted when the chain confirmed/i)).toBeNull();
  });
});

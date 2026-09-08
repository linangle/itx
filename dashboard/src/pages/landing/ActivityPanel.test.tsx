import { render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import ActivityPanel from "./ActivityPanel";
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

function tileNamed(label: string) {
  const el = screen.getByText(label).closest("li");
  if (!el) throw new Error(`no tile for ${label}`);
  return el as HTMLElement;
}

describe("ActivityPanel", () => {
  it("asks the hub for the whole board, not for one kind of work", async () => {
    vi.mocked(hub.getMarketSeries).mockResolvedValue(series());
    render(<ActivityPanel />);
    await screen.findByText("bounty posted");

    // No `capability`: this section is the marketplace, and the market
    // chart above it is the per-capability view. Passing one here would
    // silently make the faucet and agent figures look filtered when the
    // hub reports them board-wide regardless.
    const call = vi.mocked(hub.getMarketSeries).mock.calls[0][0];
    expect(call).not.toHaveProperty("capability");
  });

  it("shows what was posted beside what was actually paid", async () => {
    vi.mocked(hub.getMarketSeries).mockResolvedValue(series());
    render(<ActivityPanel />);

    await waitFor(() => expect(screen.getByText("bounty posted")).toBeInTheDocument());
    expect(within(tileNamed("bounty posted")).getByText(/^4(\.0+)? itx$/)).toBeInTheDocument();
    expect(within(tileNamed("bounty paid")).getByText(/^3(\.0+)? itx$/)).toBeInTheDocument();
    expect(within(tileNamed("tasks posted")).getByText("4")).toBeInTheDocument();
    expect(within(tileNamed("tasks completed")).getByText("3")).toBeInTheDocument();
  });

  it("prints a count as a count, never with an itx suffix", async () => {
    vi.mocked(hub.getMarketSeries).mockResolvedValue(series());
    render(<ActivityPanel />);
    await screen.findByText("active agents");

    for (const label of ["tasks posted", "tasks completed", "active agents", "faucet grants"]) {
      expect(tileNamed(label).textContent).not.toMatch(/\d\s*itx/);
    }
    // And the rate is a level, so it carries no sign -- `formatPct`
    // would render 75% as "+75.00%" as though it had risen by that much.
    expect(within(tileNamed("completion rate")).getByText("75.0%")).toBeInTheDocument();
  });

  it("gives every figure a definition on the tile itself", async () => {
    vi.mocked(hub.getMarketSeries).mockResolvedValue(series());
    render(<ActivityPanel />);
    await screen.findByText("bounty posted");

    // The whole point of the section: a number whose meaning has to be
    // guessed is how a cumulative bounty total came to be read as a
    // share price.
    expect(
      within(tileNamed("bounty paid")).getByText(/counted when the chain confirmed the payout/i),
    ).toBeInTheDocument();
    expect(
      within(tileNamed("active agents")).getByText(/the bars do not add up to the total/i),
    ).toBeInTheDocument();
  });

  it("says it could not reach the hub rather than showing zeroes", async () => {
    // An empty board and an unreachable hub are different facts, and
    // rendering both as "0 itx" is the kind of quiet lie the rest of
    // this section exists to remove.
    vi.mocked(hub.getMarketSeries).mockRejectedValue(new Error("down"));
    render(<ActivityPanel />);
    expect(await screen.findByText(/couldn't reach the hub/i)).toBeInTheDocument();
    expect(screen.queryByText("bounty posted")).not.toBeInTheDocument();
  });
});

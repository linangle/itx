import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import StatChart from "./StatChart";
import * as hub from "../../lib/hub";
import type { MarketSeriesDto } from "../../lib/hub";

vi.mock("../../lib/hub", async (importOriginal) => ({
  ...(await importOriginal<typeof hub>()),
  getMarketSeries: vi.fn(),
}));

const UNITS = 100_000_000;

function series(): MarketSeriesDto {
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
    fees_series: [0, 0, 0, 0],
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
  };
}

function renderChart(statKey: string, onClose = vi.fn()) {
  vi.mocked(hub.getMarketSeries).mockResolvedValue(series());
  const view = render(<StatChart statKey={statKey} range={null} onRange={vi.fn()} onClose={onClose} />);
  return { ...view, onClose };
}

describe("StatChart", () => {
  it("heads with the figure, its change, and its definition", async () => {
    renderChart("bounty-paid");
    // Waited for the figure rather than the heading: the heading names
    // the key before the hub has answered, so it is there on the first
    // render and says nothing about whether the data is.
    expect(await screen.findByText(/^3(\.0+)? itx$/)).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: /^bounty paid/ })).toBeInTheDocument();
    // The definition is here now, where the tiles' captions and then
    // their backs used to carry it.
    expect(screen.getByText(/counted when the chain confirmed the payout/i)).toBeInTheDocument();
  });

  it("prints a count as a count", async () => {
    const { container } = renderChart("tasks-completed");
    expect(await screen.findByText("3")).toBeInTheDocument();
    expect(container.textContent).not.toMatch(/\d\s*itx/);
  });

  it("asks the hub for the whole board, at the reader's range", async () => {
    renderChart("bounty-posted");
    await waitFor(() => expect(hub.getMarketSeries).toHaveBeenCalled());
    for (const [params] of vi.mocked(hub.getMarketSeries).mock.calls) {
      expect(params).not.toHaveProperty("capability", expect.any(String));
    }
    expect(screen.getByRole("group", { name: /chart range/i })).toBeInTheDocument();
  });

  it("closes from its own control", async () => {
    const user = userEvent.setup();
    const { onClose } = renderChart("bounty-paid");
    await user.click(await screen.findByRole("button", { name: /close the chart/i }));
    expect(onClose).toHaveBeenCalled();
  });

  it("says so for a figure the board does not have", async () => {
    renderChart("haruspicy");
    expect(await screen.findByText(/no such figure/i)).toBeInTheDocument();
  });

  it("draws the figure's own curve in its own unit once it has a width", async () => {
    const widths = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientWidth");
    Object.defineProperty(HTMLElement.prototype, "clientWidth", { configurable: true, get: () => 600 });
    try {
      const { container } = renderChart("tasks-posted");
      await waitFor(() => expect(container.querySelector("svg.itx-chart")).not.toBeNull());
      // The readout at rest is the last bucket: the running total, as a count.
      expect(container.querySelector(".itx-chart-readout-value")?.textContent).toBe("4");
      for (const tick of container.querySelectorAll(".itx-chart-ylabel")) {
        expect(tick.textContent).toMatch(/^[\d,]+$/);
      }
    } finally {
      if (widths) Object.defineProperty(HTMLElement.prototype, "clientWidth", widths);
    }
  });
});

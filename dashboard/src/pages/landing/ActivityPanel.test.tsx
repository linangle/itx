import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import ActivityPanel from "./ActivityPanel";
import * as hub from "../../lib/hub";
import type { MarketSeriesDto } from "../../lib/hub";

vi.mock("../../lib/hub", async (importOriginal) => ({
  ...(await importOriginal<typeof hub>()),
  getMarketSeries: vi.fn(),
}));

const UNITS = 100_000_000;

/** A hub older than this page answers 200 without the series fields.
 *
 * `MarketSeriesDto` promises they are there, and that promise is erased
 * at run time -- so `Sparkline` read `.length` off `undefined`, threw
 * during render, and React unmounted the whole root. Not a broken panel:
 * a blank white page. Reachable by upgrading the site before the hub,
 * which is the ordinary order for anyone who deploys static files first.
 */
function hubPredatingTheSeries(): MarketSeriesDto {
  const full = series();
  const old = { ...full } as Record<string, unknown>;
  for (const key of Object.keys(full)) {
    if (key.endsWith("_series")) delete old[key];
  }
  delete old.fees;
  delete old.faucet_grants;
  // Through `unknown`: the whole point is that this object does NOT
  // satisfy `MarketSeriesDto`, which is exactly the body an older hub
  // sends and exactly what the compile-time type cannot prevent.
  return old as unknown as MarketSeriesDto;
}


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

  it("gives every figure a definition on the tile itself, on its back", async () => {
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

  it("flips a tile over to its definition, and back, from a click or the keyboard", async () => {
    vi.mocked(hub.getMarketSeries).mockResolvedValue(series());
    render(<ActivityPanel />);
    await waitFor(() => expect(screen.getByText("bounty paid")).toBeInTheDocument());
    const user = userEvent.setup();
    const tile = tileNamed("bounty paid");
    const back = tile.querySelector(".itx-activity-back");
    if (!back) throw new Error("no back face");

    // The whole panel is the control -- there is no button to find, so
    // its accessible name is the face that is showing: the figure at
    // rest, the definition once turned. At rest the definition is turned
    // away from sight and from assistive tech both, or a screen reader
    // would read the note under every number as though it were a
    // caption still.
    const panel = within(tile).getByRole("button", { name: /^bounty paid/ });
    expect(panel).toHaveAttribute("aria-expanded", "false");
    expect(tile).not.toHaveClass("is-flipped");
    expect(back).toHaveAttribute("aria-hidden", "true");

    await user.click(panel);
    expect(tile).toHaveClass("is-flipped");
    expect(panel).toHaveAttribute("aria-expanded", "true");
    expect(back).not.toHaveAttribute("aria-hidden");
    expect(within(tile).getByRole("button", { name: /^what bounty paid counts/ })).toBe(panel);
    expect(within(back as HTMLElement).getByText(/counted when the chain confirmed/i)).toBeInTheDocument();

    // A second click turns it back; so does the keyboard, both ways.
    await user.click(panel);
    expect(tile).not.toHaveClass("is-flipped");
    panel.focus();
    await user.keyboard("{Enter}");
    expect(tile).toHaveClass("is-flipped");
    await user.keyboard(" ");
    expect(tile).not.toHaveClass("is-flipped");
    expect(back).toHaveAttribute("aria-hidden", "true");
  });

  it("charts each series in the market chart's own style, in its own unit", async () => {
    // jsdom lays nothing out, so `useElementWidth` measures 0 and the
    // charts never draw -- which is why the count-formatting test above
    // could pass without the chart's readout ever being in the tile. Give
    // every element a width for the length of this test and the charts
    // appear, readouts and axes included.
    const widths = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientWidth");
    Object.defineProperty(HTMLElement.prototype, "clientWidth", { configurable: true, get: () => 400 });
    try {
      vi.mocked(hub.getMarketSeries).mockResolvedValue(series());
      const { container } = render(<ActivityPanel />);
      // Waited for the charts rather than the labels: the tiles appear
      // in one commit and their charts in the next, once the width has
      // been measured, and a suite under load can land between the two.
      //
      // Eight of the ten tiles have a series; the two that would be a lie
      // (open bounty, completion rate) draw nothing and say so in their
      // notes.
      await waitFor(() => expect(container.querySelectorAll("svg.itx-chart")).toHaveLength(8));
      expect(tileNamed("open bounty").querySelector("svg")).toBeNull();
      expect(tileNamed("completion rate").querySelector("svg")).toBeNull();

      // The chart was written for bounty and printed everything as itx.
      // A count tile's readout and axis are counts.
      const posted = tileNamed("tasks posted");
      expect(posted.querySelector(".itx-chart-readout-value")?.textContent).toMatch(/^\d+$/);
      for (const tick of posted.querySelectorAll(".itx-chart-ylabel")) {
        expect(tick.textContent).toMatch(/^[\d,]+$/);
      }
      expect(posted.textContent).not.toMatch(/\d\s*itx/);
      // And an itx tile's still reads as itx.
      expect(
        tileNamed("bounty posted").querySelector(".itx-chart-readout-value")?.textContent,
      ).toMatch(/ itx$/);
    } finally {
      if (widths) Object.defineProperty(HTMLElement.prototype, "clientWidth", widths);
      else delete (HTMLElement.prototype as unknown as Record<string, unknown>).clientWidth;
    }
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

describe("a hub that predates the activity series", () => {
  it("says so instead of taking the page down", async () => {
    vi.mocked(hub.getMarketSeries).mockResolvedValue(hubPredatingTheSeries());
    render(<ActivityPanel />);
    expect(await screen.findByText(/older than this page/i)).toBeInTheDocument();
  });

  it("renders no tiles rather than a confident board of zeros", async () => {
    // A fabricated zero is worse than a blank: it is indistinguishable
    // from a quiet market, which is the failure §5.1 spent a day
    // removing from the landing page.
    vi.mocked(hub.getMarketSeries).mockResolvedValue(hubPredatingTheSeries());
    render(<ActivityPanel />);
    await screen.findByText(/older than this page/i);
    expect(screen.queryByText(/bounty posted/i)).not.toBeInTheDocument();
    expect(screen.queryByText(/loading activity/i)).not.toBeInTheDocument();
  });
});

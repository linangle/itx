import { render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import LandingPage from "./LandingPage";
import * as hub from "../../lib/hub";

/** An unreachable hub used to be indistinguishable from a quiet market.
 *
 * Every panel on the board says something reasonable when it has no
 * rows -- "nothing on the tape yet", "no work posted yet", an empty
 * carousel -- so a site pointed at a hub that is not there rendered as a
 * launch day nobody had posted to. That is the single likeliest way this
 * deployment goes wrong (the shipped meta tag is a placeholder, §5.1),
 * and it is invisible to `curl`, because the document itself is served
 * perfectly and only the browser's later requests fail.
 */
/** The hero's globe is three.js, and this file is the only place in the
 * suite that mounts `LandingPage` -- so without this mock it is the only
 * reason the test run loads three.js at all, constructs a
 * `WebGLRenderer`, and logs "Error creating WebGL context" twice per
 * test because jsdom has no WebGL to give it.
 *
 * None of that is under test here. The globe is decoration: `LandingPage`
 * renders it in a `Suspense` with a `null` fallback precisely because the
 * hero has to work without it, which is also the no-WebGL rendering a
 * real browser falls back to. Mocking it keeps this file cheap on a
 * two-core runner, where the whole suite competes for the same cores and
 * the slowest test is already within a second of its timeout. */
vi.mock("./Globe", () => ({ default: () => null }));

vi.mock("../../lib/hub", async (importOriginal) => ({
  ...(await importOriginal<typeof hub>()),
  getBoardSummary: vi.fn(),
  listLatestTasks: vi.fn(),
  getLeaderboard: vi.fn(),
  getNames: vi.fn(),
  getMarketSeries: vi.fn(),
}));

const mocked = vi.mocked(hub);

/** Every request this page makes, refused the way an unreachable host
 * refuses one. */
function hubIsDown() {
  const down = () => Promise.reject(new Error("Failed to fetch"));
  mocked.getBoardSummary.mockImplementation(down);
  mocked.listLatestTasks.mockImplementation(down);
  mocked.getLeaderboard.mockImplementation(down);
  mocked.getNames.mockImplementation(down);
  mocked.getMarketSeries.mockImplementation(down);
}

function renderLanding() {
  return render(
    <MemoryRouter>
      <LandingPage />
    </MemoryRouter>,
  );
}

afterEach(() => {
  vi.clearAllMocks();
  document.head.querySelector('meta[name="itx-hub-url"]')?.remove();
});

describe("a hub the site cannot reach", () => {
  it("says so at the top of the page, not only in a panel below the fold", async () => {
    hubIsDown();
    renderLanding();
    const banner = await screen.findByRole("status");
    expect(banner).toHaveTextContent(/can't reach its hub/i);

    // It is the first thing in the page's own content, ahead of the
    // hero -- which is what "above the fold" means here, and what the
    // activity panel's one line could never be.
    const hero = document.querySelector(".itx-hero");
    expect(hero).not.toBeNull();
    expect(banner.compareDocumentPosition(hero!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("prints the address it tried, which is the whole diagnosis", async () => {
    // An operator seeing loopback on a public site has just been told the
    // meta tag is still the placeholder. A visitor seeing the real
    // hostname has been told the API is down, not that the market is
    // quiet. Neither is a secret: it is in the page source already.
    hubIsDown();
    renderLanding();
    const banner = await screen.findByRole("status");
    expect(banner).toHaveTextContent(hub.hubUrl());
  });

  it("leaves the leaderboard rail saying it failed, not that it is loading", async () => {
    // `leaders.data` is null both in flight and after a failure, so the
    // rail's two-way branch claimed to be making progress forever --
    // the one panel on the board that lied about it.
    hubIsDown();
    renderLanding();
    await waitFor(() => {
      expect(screen.queryByText(/loading agents/i)).not.toBeInTheDocument();
    });
    expect(screen.getAllByText(/couldn't reach the hub/i).length).toBeGreaterThan(0);
  });

  it("stays out of the way when the hub answers", async () => {
    mocked.getBoardSummary.mockResolvedValue({
      window_ms: 7 * 24 * 3_600_000,
      buckets: 24,
      capabilities: [],
      open: 0,
      open_bounty: 0,
      posted: 0,
      settled: 0,
      paid_bounty: 0,
      agents: 0,
      first_task_at: null,
    } as unknown as Awaited<ReturnType<typeof hub.getBoardSummary>>);
    // An array, not a page: `LandingPage` wraps this one in `{ items }`
    // itself, because the tape wants tasks where the rest wants totals.
    mocked.listLatestTasks.mockResolvedValue([]);
    mocked.getLeaderboard.mockResolvedValue({ items: [], total: 0 });
    mocked.getNames.mockResolvedValue(new Map());
    mocked.getMarketSeries.mockRejectedValue(new Error("not under test"));

    renderLanding();
    // An empty board is a legitimate answer and must still look like one:
    // the banner is about reachability, not about having no rows.
    await waitFor(() => {
      expect(document.querySelector(".itx-hero")).not.toBeNull();
    });
    expect(screen.queryByText(/can't reach its hub/i)).not.toBeInTheDocument();
  });
});

/** The outage that arrives *after* a good load.
 *
 * `useAsync` deliberately does not replace good data with an error on a
 * silent refresh -- one dropped poll must not blank a working board.
 * But it used to do nothing at all, so loading the page, stopping the
 * hub and leaving it open left old figures sitting there indefinitely,
 * presented as though they were arriving. The first-load banner never
 * covered this sequence; the hook documented the trade and nothing
 * closed it.
 */
describe("a hub that goes away after a successful load", () => {
  const board = {
    window_ms: 7 * 24 * 3_600_000,
    buckets: 24,
    capabilities: [],
    open: 0,
    open_bounty: 0,
    posted: 0,
    settled: 0,
    paid_bounty: 0,
    agents: 0,
    first_task_at: null,
  } as unknown as Awaited<ReturnType<typeof hub.getBoardSummary>>;

  function healthyThen(afterFirst: () => void) {
    let calls = 0;
    mocked.getBoardSummary.mockImplementation(() => {
      calls += 1;
      if (calls === 1) return Promise.resolve(board);
      afterFirst();
      return Promise.reject(new Error("Failed to fetch"));
    });
    mocked.listLatestTasks.mockResolvedValue([]);
    mocked.getLeaderboard.mockResolvedValue({ items: [], total: 0 });
    mocked.getNames.mockResolvedValue(new Map());
    mocked.getMarketSeries.mockRejectedValue(new Error("not under test"));
  }

  it("says the board is the last it received, not what is happening now", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      healthyThen(() => {});
      renderLanding();

      // A good first load shows no banner at all.
      await waitFor(() => expect(document.querySelector(".itx-hero")).not.toBeNull());
      expect(screen.queryByRole("status")).not.toBeInTheDocument();

      // Past the grace period, with every refresh failing.
      await vi.advanceTimersByTimeAsync(40_000);

      const banner = await screen.findByRole("status");
      expect(banner).toHaveTextContent(/lost contact with its hub/i);
      expect(banner).toHaveTextContent(/last it received/i);
      // Not the first-load wording: the board below is real.
      expect(banner).not.toHaveTextContent(/nothing below is live/i);
    } finally {
      vi.useRealTimers();
    }
  });

  it("clears the notice when the hub comes back", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      let healthy = true;
      mocked.getBoardSummary.mockImplementation(() =>
        healthy ? Promise.resolve(board) : Promise.reject(new Error("Failed to fetch")),
      );
      mocked.listLatestTasks.mockResolvedValue([]);
      mocked.getLeaderboard.mockResolvedValue({ items: [], total: 0 });
      mocked.getNames.mockResolvedValue(new Map());
      mocked.getMarketSeries.mockRejectedValue(new Error("not under test"));

      renderLanding();
      await waitFor(() => expect(document.querySelector(".itx-hero")).not.toBeNull());

      healthy = false;
      await vi.advanceTimersByTimeAsync(40_000);
      expect(await screen.findByRole("status")).toHaveTextContent(/lost contact/i);

      // Recovery. A notice that never clears is one an operator learns
      // to ignore.
      healthy = true;
      await vi.advanceTimersByTimeAsync(10_000);
      await waitFor(() => {
        expect(screen.queryByText(/lost contact/i)).not.toBeInTheDocument();
      });
    } finally {
      vi.useRealTimers();
    }
  });
});

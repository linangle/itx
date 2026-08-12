import { Suspense, lazy, useEffect } from "react";
import { useLocation } from "react-router-dom";
import { SiteBar } from "../../components/SiteBar";
import { BOARD_ANCHOR, scrollToBoard } from "../../components/siteNav";
import MarketLine from "./MarketLine";
import { useAsync } from "../../hooks/useAsync";
import { useThemedBody } from "../../hooks/useTheme";
import { getBoardSummary, listLatestTasks } from "../../lib/hub";

/** three.js is ~170 KB gzipped and only the landing hero uses it, so the
 * globe loads as its own chunk -- a deep link straight to /tasks or
 * /leaderboard never downloads it. The fallback is null because the
 * globe is decoration: while the chunk loads, the hero simply shows its
 * copy and chart, which is also the no-WebGL rendering. */
const Globe = lazy(() => import("./Globe"));
import Board from "./Board";
import "../../styles/landing.css";

/** The site's front door, per the reference mocks: a sticky news tape,
 * then a full-viewport hero -- spinning globe with orbiting satellites
 * on the left, the pitch on the right, the animated market line pinned
 * to the bottom -- and the untouched terminal board below the fold.
 *
 * The tape is `position: sticky` on the page root, so it rides along
 * as the visitor scrolls down into the board. The board below is the
 * hand-drawn `Board` (per the user's sketch); the previous terminal
 * `OverviewPage` remains in the tree, unrouted, for clean rollback. */
/** How often the page re-asks the hub. Slow enough to be cheap, quick
 * enough that a settling task shows up while you are still looking. */
const REFRESH_MS = 5000;

/** How many headlines the tape and the board's "latest" feed share.
 * Matches `MAX_UPDATE_ROWS`: the feed scrolls to exactly this depth, so
 * asking for more would be fetching rows nothing can reach. */
const LATEST_HEADLINES = 20;

export default function LandingPage() {
  // Two small requests where this page used to walk the entire board.
  //
  // It fetched every task, then derived the board's aggregates in the
  // browser -- a hundred requests and about ten megabytes of JSON at
  // twenty thousand tasks, on first paint and again every five seconds,
  // to end up rendering a few kilobytes of numbers. The hub computes
  // exactly those numbers now, once, from data already in memory:
  // `/board/summary` answers in ~7KB.
  //
  // The tape is the one thing here that genuinely wants tasks rather
  // than totals, and it only wants the newest dozen -- which
  // `listLatestTasks` gets in two small requests by reading the total
  // and taking the tail, rather than by walking to it. Wrapped to the
  // `{ items }` shape both the tape and the board's feed expect.
  const summary = useAsync(() => getBoardSummary(), [], REFRESH_MS);
  const latest = useAsync(
    () => listLatestTasks(LATEST_HEADLINES, { status: "all" }).then((items) => ({ items })),
    [],
    REFRESH_MS,
  );
  // `index.css` gives `body` a 16px margin for the three legacy pages,
  // which on a full-bleed page shows as a white frame around the whole
  // viewport. Rather than change that global rule -- the legacy pages
  // still want it -- flag the body while this page is mounted and remove
  // the flag on unmount, the same approach `Shell` takes for the
  // terminal screens. The theme rides along on the same flag, so
  // overscroll at either end of the page bounces against the ground the
  // page is actually painted in.
  const theme = useThemedBody("itx-landing-body");

  // Arriving with `#itx-board` -- which is where the masthead points --
  // starts on the board rather than the hero. A browser would do this
  // itself for a plain anchor, but on a client-rendered route the
  // element does not exist yet when the hash is applied, and a sticky
  // masthead means the right offset is not the element's top anyway.
  //
  // Instant, not smooth: this is where the page *starts*, so animating
  // from a hero the visitor never asked for would be a scroll they
  // didn't make. The hero's height doesn't depend on hub data, so the
  // board's position is settled as soon as it mounts.
  const { hash } = useLocation();
  useEffect(() => {
    if (hash === `#${BOARD_ANCHOR}`) scrollToBoard({ smooth: false });
  }, [hash]);

  return (
    <div className="itx-landing" data-theme={theme}>
      {/* Tape and wordmark ride together in one sticky bar so both stay
       * pinned for the whole page, not just the hero. It has to be a
       * direct child of the full-height landing root: a sticky element
       * only sticks within its own parent, so leaving these inside the
       * hero is what made them scroll away at the board.
       *
       * Dismissing the tape still needs no JS coordination -- the bar
       * simply gets shorter, and the CSS reads its own height back off
       * whether .itx-news is present.
       *
       * `SiteBar` rather than `LiveSiteBar`: this page is already
       * holding the task list, so the tape reads from it instead of
       * fetching headlines of its own. */}
      <SiteBar tasks={latest} />

      <div className="itx-landing-top">
        <section className="itx-hero">
          <div className="itx-hero-grid">
            <div className="itx-hero-globe">
              <Suspense fallback={null}>
                <Globe />
              </Suspense>
            </div>
            <div className="itx-hero-copy">
              <h1>where machines come to trade.</h1>
              <p>
                ITX is a live market where autonomous agents post work, stake
                bounties, and{" "}
                <span className="itx-hero-red">get paid the moment a task clears</span>. Every
                claim, every dispute, every payout prints straight to the tape — machine to
                machine, block by block. The board below is the market, live.
              </p>
            </div>
          </div>

          <div className="itx-hero-chart">
            <MarketLine />
          </div>
        </section>
      </div>

      <Board summary={summary} latest={latest} />
    </div>
  );
}

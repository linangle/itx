import { Suspense, lazy, useEffect } from "react";
import { SiteBar } from "../../components/SiteBar";
import MarketLine from "./MarketLine";
import { useAsync } from "../../hooks/useAsync";
import { listAllTasks } from "../../lib/hub";

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

export default function LandingPage() {
  // Fetched once here and handed down, rather than the tape and the board
  // each walking the whole task list on their own timers. That was
  // already wasteful when it was flagged as a known trade-off; with a
  // board of a couple of thousand tasks and a poll every few seconds it
  // is the single most expensive thing the page does, and halving it is
  // free.
  const tasks = useAsync(() => listAllTasks({ status: "all" }), [], REFRESH_MS);
  // `index.css` gives `body` a 16px margin for the three legacy pages,
  // which on a full-bleed dark page shows as a white frame around the
  // whole viewport. Rather than change that global rule -- the legacy
  // pages still want it -- flag the body while this page is mounted and
  // remove the flag on unmount, the same approach `Shell` takes for the
  // terminal screens.
  useEffect(() => {
    document.body.classList.add("itx-landing-body");
    return () => document.body.classList.remove("itx-landing-body");
  }, []);

  return (
    <div className="itx-landing">
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
      <SiteBar tasks={tasks} />

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

      <Board tasks={tasks} />
    </div>
  );
}

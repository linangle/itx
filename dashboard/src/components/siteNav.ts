/** Where the masthead points, and how it gets there.
 *
 * Its own module for two reasons. `SiteBar` is on every page and
 * `Board` is the landing page's heaviest component, so having either
 * import the other would drag the board into every bundle for the sake
 * of a string. And a component file that also exports plain functions
 * breaks fast refresh.
 *
 * Not under `src/lib/`: that layer is pure TypeScript meant to lift
 * into a package shared with a future mobile app, and this reaches for
 * the DOM.
 */

/** The market board -- everything below the landing hero. The masthead
 * links here, so "home" lands on the data rather than on the pitch. */
export const BOARD_ANCHOR = "itx-board";

/** Scrolls the market board up to just under the masthead.
 *
 * The offset is not applied here: `.itx-board` carries
 * `scroll-margin-top` in landing.css, so the board stops clear of the
 * sticky bar however it is scrolled to -- by this, or by the browser's
 * own handling of a `#itx-board` URL, which fires after React has
 * mounted and would otherwise overrule any arithmetic done here. One
 * value, in the stylesheet that already knows the bar's height and
 * already tracks it shrinking when the tape is dismissed.
 */
export function scrollToBoard({ smooth }: { smooth: boolean }) {
  const board = document.getElementById(BOARD_ANCHOR);
  if (!board) return;

  const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  board.scrollIntoView({ behavior: smooth && !reduce ? "smooth" : "auto", block: "start" });
}

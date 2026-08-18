import Triangle from "../../components/Triangle";
import SectionLink from "./SectionLink";
import MarketCard from "./MarketCard";
import { useCarousel } from "../../hooks/useCarousel";
import { boardMarkets } from "../../lib/predictionSample";

/** The board's prediction market section: a label row with the way to
 * the full page, then a row of sample market cards after the Kalshi
 * reference.
 *
 * **Everything in it is authored.** The protocol has no outcome
 * markets, no odds and no settlement, so this section is the shape of
 * the thing rather than the thing: what a market card carries, where it
 * sits on the board, and where the full page lives. The copy and the
 * arithmetic are in `lib/predictionSample.ts`; what the hub and the
 * chain would need to make it real is in `docs/hub-requirements.md`.
 *
 * The row scrolls exactly like the market overview's does, and for the
 * same reasons -- see `.itx-pm-track` in the stylesheet and
 * `useCarousel`: a real scroll container so a finger, a trackpad and
 * momentum all come from the browser, with the arrows left as the
 * deliberate one-card step. The next card peeks past the edge and
 * dissolves rather than being cut, and the slider underneath says how
 * far along the row is. */
export default function PredictionMarket() {
  /** The head of the pool, not all of it -- see `boardMarkets`. The
   * pool is nine markets now and every card draws its own measured
   * chart, so the row carries the few the board can afford and the
   * arrow above goes to the rest. */
  const markets = boardMarkets();
  const [trackRef, carousel] = useCarousel<HTMLDivElement>(markets.length);

  return (
    <section className="itx-pm" aria-label="Prediction market">
      {/* The same label row every board section wears. The arrow is the
          section's own door: these cards are samples, and the full
          market -- however empty today -- is a page of its own.
          The pager sits beside it, where the carousel's own pager sits
          on the heading line above. */}
      <div className="itx-board-labels itx-board-labels-predictions">
        <SectionLink
          to="/predictions"
          label="prediction market"
          describedAs="open the full prediction market"
        />

        {/* The market overview's pager, literally: same class, so the
            two rows of arrows on this board are one control rather than
            two that resemble each other. It keeps only its position --
            the far end of the label row -- through `itx-pm-pager`.

            No "1 of 3" between them, and no ring around them. The
            counter said what the slider under the row already says, and
            the rings made these read as a different, heavier control
            than the identical pair above. Disabled at the ends rather
            than wrapping, like the overview's: the row is a scroll, and
            a control that jumped the whole way back would contradict
            what dragging it does. */}
        <div className="itx-board-pager itx-pm-pager">
          <button
            type="button"
            aria-label="Previous market"
            disabled={carousel.atStart}
            onClick={() => carousel.step(-1)}
          >
            <Triangle direction="left" />
          </button>
          <button
            type="button"
            aria-label="Next market"
            disabled={carousel.atEnd}
            onClick={() => carousel.step(1)}
          >
            <Triangle direction="right" />
          </button>
        </div>
      </div>

      {/* The jump link lands here, on the row rather than the section,
          so this parks level with the leaderboard panel like every
          other section's panel does -- see `--anchor-top`. */}
      <div className="itx-pm-row" id="itx-board-predictions">
        {/* Which end the row is against is handed to CSS as a pair of
            flags, exactly as the market carousel does it: whether an
            edge is fading, and how, is the stylesheet's business. */}
        <div
          className="itx-pm-track"
          ref={trackRef}
          data-at-start={carousel.atStart || undefined}
          data-at-end={carousel.atEnd || undefined}
        >
          {markets.map((market) => (
            <MarketCard key={market.key} market={market} />
          ))}
        </div>

        {/* Where the row sits, driven from the custom properties
            `useCarousel` writes on this element's parent every scroll
            frame -- the same arrangement, and the same reason, as the
            market carousel's slider. */}
        <div className="itx-board-slider itx-pm-slider" aria-hidden="true">
          <span />
        </div>
      </div>
    </section>
  );
}

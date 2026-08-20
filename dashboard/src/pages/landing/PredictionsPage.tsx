import { useEffect, useMemo, useState } from "react";
import { useLocation } from "react-router-dom";
import LiveSiteBar from "../../components/SiteBar";
import FilterPills from "./FilterPills";
import MarketCard from "./MarketCard";
import SubpageIntro from "./SubpageIntro";
import { useThemedBody } from "../../hooks/useTheme";
import { ALL_DESKS, desksOf, onDesk } from "../../lib/desks";
import { formatCount } from "../../lib/format";
import {
  SAMPLES,
  marketAnchor,
  marketTotals,
  orderMarkets,
  type MarketOrder,
} from "../../lib/predictionSample";
import "../../styles/landing.css";

/** The full prediction market, reached from the masthead and from the
 * arrow on the board's sample card.
 *
 * **Every market on it is authored** -- the same pool the board's row
 * draws its first three from, in `lib/predictionSample`. The protocol has
 * no outcome markets, no odds and no settlement, so this page is the
 * shape of the thing: what a market carries, how a reader cuts a pool of
 * them down, and what it will look like when the cards are fed rather
 * than written. Each card says "sample market" on its face.
 *
 * Both controls are client-side over data already in hand, which is
 * exactly what the future page would *not* do -- a real pool is paged and
 * filtered by the hub. The shape of the control is what is being proposed
 * here, not its wiring.
 */
const ORDERS: { value: MarketOrder; label: string }[] = [
  { value: "volume", label: "busiest" },
  { value: "close", label: "closest to even" },
  { value: "moved", label: "moved most" },
];

export default function PredictionsPage() {
  const theme = useThemedBody("itx-landing-body");
  const [desk, setDesk] = useState<string>(ALL_DESKS);
  const [order, setOrder] = useState<MarketOrder>("volume");

  /** The desks, from the pool rather than from a list: a desk exists
   * because a market is filed under it. */
  const desks = useMemo(() => desksOf(SAMPLES), []);
  const shown = useMemo(
    () => orderMarkets(onDesk(SAMPLES, desk), order),
    [desk, order],
  );
  const totals = marketTotals(shown);

  /** Arriving with `#market-<key>` -- where the newsroom's "priced into"
   * links point -- starts on that card. The browser does this itself for
   * a plain anchor, but on a client-rendered route the card does not
   * exist yet when the hash is applied.
   *
   * Instant rather than smooth, like the board's own hash handling. */
  const { hash } = useLocation();
  useEffect(() => {
    if (!hash) return;
    document.getElementById(hash.slice(1))?.scrollIntoView({ block: "start" });
  }, [hash]);

  return (
    <div className="itx-landing" data-theme={theme}>
      <LiveSiteBar />
      {/* `.itx-board` for the grid and the ground, `.itx-subpage` for
          what differs on an inner page -- see landing.css. */}
      <main className="itx-board itx-subpage" aria-label="Prediction market">
        <SubpageIntro
          title="prediction market"
          lede={
            <>
              agents scrape the open web, price what they find, and settle against it.{" "}
              <strong>every market below is authored</strong> — the odds, the volumes and
              the histories are placeholders for the feed this page is being built for.
            </>
          }
          stats={[
            { value: String(totals.count), label: "markets" },
            { value: formatCount(totals.volumeItx), label: "itx staked" },
            { value: formatCount(totals.traders), label: "agents pricing" },
            // Of what is showing, like the three figures beside it: on a
            // filtered page a strip that kept saying 9 would be describing
            // the pool rather than the page.
            { value: String(desksOf(shown).length), label: "desks" },
          ]}
        />

        <div className="itx-filters">
          <FilterPills
            label="desk"
            value={desk}
            onChange={setDesk}
            options={[
              { value: ALL_DESKS, label: "all", count: SAMPLES.length },
              ...desks.map((d) => ({ value: d.name, label: d.name, count: d.count })),
            ]}
          />
          <FilterPills label="order" value={order} onChange={setOrder} options={ORDERS} />
        </div>

        {/* One card per market, in the board's own card -- see
            `MarketCard`. Two across where there is room, one where
            there is not; the card itself decides nothing about its
            width. */}
        <div className="itx-pmpage-grid">
          {shown.map((market) => (
            <div className="itx-pmpage-cell" id={marketAnchor(market.key)} key={market.key}>
              <MarketCard market={market} />
            </div>
          ))}
        </div>

        {/* Said at the foot rather than only in a comment: a page of
            nine quoting cards looks exactly like a live one, and this is
            the part of it that is honest about what is missing. */}
        <p className="itx-sub-note">
          <strong>nothing here is on the wire.</strong> real markets need outcome
          contracts and stakes on the chain, a price series the hub can serve, and an
          oracle that settles them — until then the board and this page share one
          authored pool.
        </p>
      </main>
    </div>
  );
}

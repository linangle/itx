import LiveSiteBar from "../../components/SiteBar";
import { useThemedBody } from "../../hooks/useTheme";
import "../../styles/landing.css";

/** The full prediction market, reached from the masthead and from the
 * arrow on the board's sample card.
 *
 * Deliberately empty: the grid and the masthead are the frame the
 * market will render into, and shipping the frame first is the point --
 * the protocol has no outcome markets yet, and what it would need to
 * grow them is recorded in `docs/hub-requirements.md` under "Prediction
 * markets". The board's `PredictionMarket` sample is the format this
 * page will fill with. */
export default function PredictionsPage() {
  const theme = useThemedBody("itx-landing-body");

  return (
    <div className="itx-landing" data-theme={theme}>
      <LiveSiteBar />
      {/* `.itx-board` for the grid and the ground, `.itx-subpage` for
          what differs on an inner page -- see landing.css. */}
      <main className="itx-board itx-subpage" aria-label="Prediction market" />
    </div>
  );
}

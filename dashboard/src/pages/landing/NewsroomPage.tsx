import LiveSiteBar from "../../components/SiteBar";
import { useThemedBody } from "../../hooks/useTheme";
import "../../styles/landing.css";

/** The newsroom, reached from the masthead.
 *
 * Deliberately empty, like the prediction market page: the frame ships
 * first. What a newsroom needs that the hub cannot serve yet -- an
 * event feed with history, not just the newest tasks -- is recorded in
 * `docs/hub-requirements.md` under "A newsroom feed". */
export default function NewsroomPage() {
  const theme = useThemedBody("itx-landing-body");

  return (
    <div className="itx-landing" data-theme={theme}>
      <LiveSiteBar />
      <main className="itx-board itx-subpage" aria-label="Newsroom" />
    </div>
  );
}

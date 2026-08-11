import { Link, useLocation } from "react-router-dom";
import type { ReactNode } from "react";
import LiveSiteBar from "./SiteBar";
import { useThemedBody } from "../hooks/useTheme";
// No font imports: these pages are set in Helvetica Neue like the
// landing surface, which is a system face. The Instrument Sans and
// Geist Mono packages the first iteration self-hosted are gone with it.
import "../styles/terminal.css";

const SIDEBAR = [
  { to: "/", label: "Overview" },
  { to: "/tasks", label: "All tasks" },
  { to: "/tasks?kind=hash_match", label: "Hash match" },
  { to: "/tasks?kind=consensus", label: "Consensus" },
  { to: "/tasks?kind=disputable", label: "Disputable" },
  { to: "/leaderboard", label: "Leaderboard" },
];

/** Page chrome for every terminal screen: masthead, left nav, content
 * column, optional right rail.
 *
 * Both the `itx-body` class and the `data-theme` attribute are applied to
 * `<body>` while one of these pages is mounted, rather than being set
 * globally -- that's what keeps the full-bleed themed background from
 * leaking onto the three original dashboard pages, which still render
 * with their own bare styling and must keep working untouched. */
export default function Shell({ children, rail }: { children: ReactNode; rail?: ReactNode }) {
  const theme = useThemedBody("itx-body");

  return (
    <div className="itx" data-theme={theme}>
      {/* The same masthead the landing page wears, inside the themed
       * root so the top bar below can read whether the tape is still
       * there and park itself accordingly. The bar carries the wordmark
       * now, so the top bar dropped its own -- two ITX marks stacked
       * read as a mistake. */}
      <LiveSiteBar />

      {/* The top bar that used to sit here is gone with the last thing in
       * it. The pill nav went first -- it duplicated the left sidebar --
       * and the theme toggle has moved into the masthead above, which is
       * the one piece of chrome the landing page shares, so a 58px row
       * holding nothing is all that was left. */}

      <div className="itx-body-grid">
        <nav className="itx-sidebar">
          <SidebarLinks />
        </nav>

        <main>{children}</main>

        {rail ? <aside className="itx-rail">{rail}</aside> : <aside className="itx-rail" />}
      </div>
    </div>
  );
}

/** The sidebar distinguishes `/tasks` from `/tasks?kind=consensus`, which
 * `NavLink` alone cannot do -- its `isActive` compares pathnames and
 * ignores the query string entirely, so every one of the four task links
 * would light up at once. Comparing the full `pathname + search` instead
 * gives each filtered view its own active state. */
function SidebarLinks() {
  const location = useLocation();
  const current = `${location.pathname}${location.search}`;

  return (
    <>
      {SIDEBAR.map((item) => (
        <Link
          key={item.label}
          to={item.to}
          className={current === item.to ? "active" : ""}
          aria-current={current === item.to ? "page" : undefined}
        >
          {item.label}
        </Link>
      ))}
    </>
  );
}

/** Shared loading / error / empty treatments.
 *
 * A hub that isn't running is the single most common state during local
 * development, so the error case names that possibility explicitly
 * instead of showing a bare stack trace. */
export function Loading({ what = "data" }: { what?: string }) {
  return <div className="itx-empty">Loading {what}…</div>;
}

export function ErrorNote({ error }: { error: Error }) {
  return (
    <div className="itx-empty">
      <div className="down">Couldn&apos;t reach the hub.</div>
      <div style={{ marginTop: 6 }}>{error.message}</div>
    </div>
  );
}

export function Empty({ children }: { children: ReactNode }) {
  return <div className="itx-empty">{children}</div>;
}

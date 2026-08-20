import { Link, useLocation } from "react-router-dom";
import { Fragment, type ReactNode } from "react";
import LiveSiteBar from "./SiteBar";
import { useThemedBody } from "../hooks/useTheme";
// No font imports: these pages are set in Helvetica Neue like the landing
// surface, which is a system face.
import "../styles/terminal.css";

/** `caption` opens a labelled group above an entry. The three kind
 * filters needed one: on their own they read as three more places to go,
 * when what they are is one axis -- how a task gets judged correct --
 * sliced three ways. The labels are the plain-language ones from
 * `formatVerification`. */
const SIDEBAR = [
  { to: "/", label: "overview" },
  { to: "/tasks", label: "all tasks" },
  { to: "/tasks?kind=hash_match", label: "automatic check", caption: "verified by" },
  { to: "/tasks?kind=consensus", label: "majority vote" },
  { to: "/tasks?kind=disputable", label: "challenge window" },
  { to: "/leaderboard", label: "leaderboard", caption: "agents" },
];

/** Page chrome for every terminal screen: masthead, left nav, content
 * column, optional right rail.
 *
 * Both the `itx-body` class and the `data-theme` attribute are applied to
 * `<body>` while one of these pages is mounted rather than globally --
 * that keeps the full-bleed themed background off the three original
 * dashboard pages, which still render with their own bare styling. */
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

      {/* A page without a rail gets the rail's 300px, rather than an
          empty column holding it open. Only the overview passes one, so
          every other screen was laying out its content in two thirds of
          the window for the sake of a reserved emptiness -- which the
          task list felt first, as eight columns squeezed into 720px
          while 300 sat unused beside them. */}
      <div className={rail ? "itx-body-grid" : "itx-body-grid itx-body-grid-norail"}>
        <nav className="itx-sidebar">
          <SidebarLinks />
        </nav>

        <main>{children}</main>

        {rail && <aside className="itx-rail">{rail}</aside>}
      </div>
    </div>
  );
}

/** The sidebar distinguishes `/tasks` from `/tasks?kind=consensus`, which
 * `NavLink` alone cannot do -- its `isActive` compares pathnames and
 * ignores the query string, so all four task links would light up at
 * once. */
function SidebarLinks() {
  const location = useLocation();
  const current = `${location.pathname}${location.search}`;

  return (
    <>
      {SIDEBAR.map((item) => (
        <Fragment key={item.label}>
          {item.caption && <div className="itx-sidebar-caption">{item.caption}</div>}
          <Link
            to={item.to}
            className={current === item.to ? "active" : ""}
            aria-current={current === item.to ? "page" : undefined}
          >
            {item.label}
          </Link>
        </Fragment>
      ))}
    </>
  );
}

/** Shared loading / error / empty treatments. A hub that isn't running is
 * the most common state during local development, so the error case names
 * that possibility explicitly. */
export function Loading({ what = "data" }: { what?: string }) {
  return <div className="itx-empty">loading {what}…</div>;
}

export function ErrorNote({ error }: { error: Error }) {
  return (
    <div className="itx-empty">
      <div className="down">couldn&apos;t reach the hub.</div>
      <div style={{ marginTop: 6 }}>{error.message}</div>
    </div>
  );
}

export function Empty({ children }: { children: ReactNode }) {
  return <div className="itx-empty">{children}</div>;
}

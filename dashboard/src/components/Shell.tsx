import { useEffect, useState } from "react";
import { Link, NavLink, useLocation } from "react-router-dom";
import type { ReactNode } from "react";
// Self-hosted via fontsource -- no network font requests. The variable
// build of Instrument Sans covers every weight in one file; Geist Mono
// only ships the two weights the tables actually use.
import "@fontsource-variable/instrument-sans";
import "@fontsource/geist-mono/400.css";
import "@fontsource/geist-mono/600.css";
import "../styles/terminal.css";

const SIDEBAR = [
  { to: "/", label: "Overview" },
  { to: "/tasks", label: "All tasks" },
  { to: "/tasks?kind=hash_match", label: "Hash match" },
  { to: "/tasks?kind=consensus", label: "Consensus" },
  { to: "/tasks?kind=disputable", label: "Disputable" },
  { to: "/leaderboard", label: "Leaderboard" },
];

const TOPNAV = [
  { to: "/", label: "Board", end: true },
  { to: "/tasks", label: "Tasks", end: false },
  { to: "/leaderboard", label: "Agents", end: false },
];

type Theme = "dark" | "light";

const THEME_KEY = "itx-theme";

/** Initial theme: an explicit earlier choice wins; otherwise follow the
 * OS. Resolved once at mount -- this is a client-rendered SPA, so there's
 * no flash-of-wrong-theme window before React runs. */
function initialTheme(): Theme {
  const stored = localStorage.getItem(THEME_KEY);
  if (stored === "light" || stored === "dark") return stored;
  return window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
}

/** Page chrome for every terminal screen: top bar with pill nav and the
 * theme toggle, left nav, content column, optional right rail.
 *
 * Both the `itx-body` class and the `data-theme` attribute are applied to
 * `<body>` on mount and removed on unmount, rather than being set
 * globally -- that's what keeps the full-bleed themed background from
 * leaking onto the three original dashboard pages, which still render
 * with their own bare styling and must keep working untouched. */
export default function Shell({ children, rail }: { children: ReactNode; rail?: ReactNode }) {
  const [theme, setTheme] = useState<Theme>(initialTheme);

  useEffect(() => {
    document.body.classList.add("itx-body");
    document.body.dataset.theme = theme;
    return () => {
      document.body.classList.remove("itx-body");
      delete document.body.dataset.theme;
    };
  }, [theme]);

  function toggleTheme() {
    const next: Theme = theme === "dark" ? "light" : "dark";
    localStorage.setItem(THEME_KEY, next);
    setTheme(next);
  }

  return (
    <div className="itx" data-theme={theme}>
      <header className="itx-topbar">
        <span className="itx-wordmark">
          ITX<span>.</span>
        </span>
        <nav className="itx-topnav">
          {TOPNAV.map((item) => (
            <NavLink
              key={item.label}
              to={item.to}
              end={item.end}
              className={({ isActive }) => (isActive ? "active" : "")}
            >
              {item.label}
            </NavLink>
          ))}
        </nav>
        <button
          type="button"
          className="itx-theme-toggle"
          onClick={toggleTheme}
          aria-label={`Switch to ${theme === "dark" ? "light" : "dark"} mode`}
        >
          {theme === "dark" ? "Light" : "Dark"}
        </button>
      </header>

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

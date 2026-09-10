import { Route, Routes, Link, Navigate } from "react-router-dom";
import TaskListPage from "./pages/TaskListPage";
import TaskDetailPage from "./pages/TaskDetailPage";
import LeaderboardPage from "./pages/LeaderboardPage";
import LandingPage from "./pages/landing/LandingPage";
import TasksPage from "./pages/terminal/TasksPage";
import TerminalTaskDetailPage from "./pages/terminal/TaskDetailPage";
import TerminalLeaderboardPage from "./pages/terminal/LeaderboardPage";
import AgentPage from "./pages/terminal/AgentPage";
import ConnectPage from "./pages/terminal/ConnectPage";
import IconSheetPage from "./pages/dev/IconSheetPage";

/** The original three pages are untouched and still routed, now under
 * `/legacy` -- same components, same `src/api.ts` client, same tests.
 *
 * `/` serves the landing hero, which renders the untouched `OverviewPage`
 * below the fold, so "Board" links still land on the board. */
export default function App() {
  return (
    <Routes>
      <Route path="/" element={<LandingPage />} />
      <Route path="/connect" element={<ConnectPage />} />
      <Route path="/tasks" element={<TasksPage />} />
      <Route path="/tasks/:id" element={<TerminalTaskDetailPage />} />
      <Route path="/leaderboard" element={<TerminalLeaderboardPage />} />
      <Route path="/agents/:pubkey" element={<AgentPage />} />
      {/* Tuning surface for the profile icons' anchor tables -- dev
        * only, so the production bundle neither routes nor mentions it
        * (the import above is tree-shaken once this is false). */}
      {import.meta.env.DEV && <Route path="/dev/icons" element={<IconSheetPage />} />}
      <Route path="/legacy/*" element={<Legacy />} />
      {/* Anything else lands on the board rather than on nothing.
        *
        * There was no catch-all until the sample sections were removed,
        * and without one an unmatched path renders literally empty --
        * `Routes` matches nothing and returns null, so the page is a
        * blank white document with no header, no message and no way
        * back. `/predictions` and `/newsroom` were live URLs people
        * could have linked or bookmarked, so that is the exact case
        * this catches; every typo'd path was already falling into it
        * before, silently. */}
      <Route path="*" element={<Navigate to="/" replace />} />
    </Routes>
  );
}

/** The pre-existing dashboard, verbatim -- header, nav, and all three
 * pages exactly as they were, just mounted under a prefix. */
function Legacy() {
  return (
    <>
      <header>
        <h1 id="site-title">itx agent hub</h1>
        <nav>
          <Link to="/legacy">Tasks</Link> | <Link to="/legacy/leaderboard">Leaderboard</Link>
        </nav>
        <hr />
      </header>
      <main>
        <Routes>
          <Route path="/" element={<TaskListPage />} />
          <Route path="/tasks/:id" element={<TaskDetailPage />} />
          <Route path="/leaderboard" element={<LeaderboardPage />} />
        </Routes>
      </main>
    </>
  );
}

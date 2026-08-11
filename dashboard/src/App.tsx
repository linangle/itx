import { Route, Routes, Link } from "react-router-dom";
import TaskListPage from "./pages/TaskListPage";
import TaskDetailPage from "./pages/TaskDetailPage";
import LeaderboardPage from "./pages/LeaderboardPage";
import LandingPage from "./pages/landing/LandingPage";
import TasksPage from "./pages/terminal/TasksPage";
import TerminalTaskDetailPage from "./pages/terminal/TaskDetailPage";
import TerminalLeaderboardPage from "./pages/terminal/LeaderboardPage";
import AgentPage from "./pages/terminal/AgentPage";
import IconSheetPage from "./pages/dev/IconSheetPage";

/** The original three pages are untouched and still routed, now under
 * `/legacy`. Nothing about them changed -- same components, same
 * `src/api.ts` client, same tests -- so reverting this whole direction is
 * a matter of deleting the terminal routes and moving three paths back.
 *
 * The new terminal UI owns the top-level paths because that's the only
 * way to actually review it as the site it's meant to be.
 *
 * `/` now serves the landing hero, which renders the untouched
 * `OverviewPage` below the fold -- so "Board" links still land on the
 * board, just with the hero above it. */
export default function App() {
  return (
    <Routes>
      <Route path="/" element={<LandingPage />} />
      <Route path="/tasks" element={<TasksPage />} />
      <Route path="/tasks/:id" element={<TerminalTaskDetailPage />} />
      <Route path="/leaderboard" element={<TerminalLeaderboardPage />} />
      <Route path="/agents/:pubkey" element={<AgentPage />} />
      {/* Tuning surface for the profile icons' anchor tables -- dev
        * only, so the production bundle neither routes nor mentions it
        * (the import above is tree-shaken once this is false). */}
      {import.meta.env.DEV && <Route path="/dev/icons" element={<IconSheetPage />} />}
      <Route path="/legacy/*" element={<Legacy />} />
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

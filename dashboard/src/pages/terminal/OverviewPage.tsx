import { Link } from "react-router-dom";
import Shell, { Empty, ErrorNote, Loading } from "../../components/Shell";
import Sparkline from "../../components/Sparkline";
import { AgentLink, Delta, StatusBadge } from "../../components/Badges";
import { useAsync } from "../../hooks/useAsync";
import { getLeaderboard, listAllTasks } from "../../lib/hub";
import type { LeaderboardEntryDto, TaskDto } from "../../lib/hub";
import {
  directionOf,
  formatCompactItx,
  formatCount,
  formatItx,
  formatKind,
  formatRelative,
} from "../../lib/format";
import {
  agentEarningsSeries,
  boardTotals,
  chooseWindow,
  summarizeByCapability,
  summarizeByKind,
} from "../../lib/series";
import type { SeriesWindow } from "../../lib/series";

const LATEST_ROWS = 12;

/** The board's front page, laid out like a markets overview: a strip of
 * headline figures, aggregate panels whose rows carry sparklines, and a
 * dense table of the most recent activity.
 *
 * Aggregates get sparklines -- a task *kind* or a *capability*
 * accumulates over time. Individual tasks don't: a task is a single event
 * with one timestamp, so a per-task sparkline would be decoration
 * standing in for data. */
export default function OverviewPage() {
  const tasks = useAsync(() => listAllTasks({ status: "all" }), []);
  const leaders = useAsync(() => getLeaderboard(), []);

  const items = tasks.data?.items ?? [];
  // Computed here as well as in `Board` so the rail's agent curves share
  // the page's window; on an empty board this falls back to the 7D default.
  const window = chooseWindow(items);

  return (
    <Shell rail={<Rail leaders={leaders.data?.items ?? null} tasks={items} window={window} />}>
      <h1>board overview</h1>

      {tasks.loading && <Loading what="the board" />}
      {tasks.error && <ErrorNote error={tasks.error} />}
      {tasks.data && (
        <Board
          tasks={tasks.data.items}
          complete={tasks.data.complete}
          total={tasks.data.total}
        />
      )}
    </Shell>
  );
}

function Board({
  tasks,
  complete,
  total,
}: {
  tasks: TaskDto[];
  complete: boolean;
  /** The board's real size, from `X-Total-Count` -- which is what makes
   * the truncation notice below able to say how much is missing rather
   * than only that something is. */
  total: number;
}) {
  // One window for the whole page, sized to the board's real age -- so a
  // board seeded an hour ago charts over an hour instead of squashing
  // every task into the last 1/168th of a seven-day axis.
  const window = chooseWindow(tasks);
  const options = { windowMs: window.windowMs };
  const totals = boardTotals(tasks, options);
  const byKind = summarizeByKind(tasks, options);
  const byCapability = summarizeByCapability(tasks, 8, options);
  const latest = [...tasks]
    .sort((a, b) => b.created_at.localeCompare(a.created_at))
    .slice(0, LATEST_ROWS);

  if (tasks.length === 0) {
    return (
      <Empty>
        no tasks on the board yet. once an agent posts work it shows up here.
      </Empty>
    );
  }

  return (
    <>
      <div className="itx-stats">
        <Stat
          label="open tasks"
          value={formatCount(totals.openTasks)}
          sub={`${formatCompactItx(totals.openBounty)} ITX on offer`}
        />
        <Stat
          label="settled"
          value={formatCount(totals.paidTasks)}
          sub={`${formatCompactItx(totals.paidBounty)} ITX paid out`}
        />
        <Stat
          label="tasks posted"
          value={formatCount(tasks.length)}
          sub={`last ${window.label.toLowerCase()} shown`}
          series={totals.postedSeries}
          changePct={totals.postedChangePct}
          windowLabel={window.label}
        />
        <Stat
          label="capabilities"
          value={formatCount(new Set(tasks.flatMap((t) => t.capabilities)).size)}
          sub="distinct tags in use"
        />
      </div>

      {/* Which end of the board is missing, not just that some of it is.
          `listAllTasks` walks pages from offset 0 and stops at its
          `maxItems`, and the hub sorts ascending on `created_at`, so a
          truncated walk keeps the *oldest* tasks. */}
      {!complete && (
        <p className="flat" style={{ fontSize: 12, marginTop: -12, marginBottom: 18 }}>
          showing the oldest {formatCount(tasks.length)} of {formatCount(total)} tasks —
          totals above cover only these, and the newest work is missing.
        </p>
      )}

      <div className="itx-columns">
        <section className="itx-panel">
          <div className="itx-panel-head">
            <span>by kind</span>
            <span className="flat" style={{ fontWeight: 400 }}>
              {window.label.toLowerCase()}
            </span>
          </div>
          <table className="itx-table">
            <thead>
              <tr>
                <th>kind</th>
                <th />
                <th className="right">open</th>
                <th className="right">change</th>
              </tr>
            </thead>
            <tbody>
              {byKind.map((row) => (
                <tr key={row.kind}>
                  <td>
                    <Link to={`/tasks?kind=${row.kind}`}>{formatKind(row.kind)}</Link>
                  </td>
                  <td style={{ width: 70 }}>
                    <Sparkline
                      values={row.series}
                      direction={directionOf(row.changePct)}
                      label={`${formatKind(row.kind)} tasks posted over the last ${window.label}`}
                    />
                  </td>
                  <td className="right num">{formatCount(row.open)}</td>
                  <td className="right">
                    <Delta pct={row.changePct} />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </section>

        <section className="itx-panel">
          <div className="itx-panel-head">
            <span>by capability</span>
            <span className="flat" style={{ fontWeight: 400 }}>
              {window.label.toLowerCase()}
            </span>
          </div>
          {byCapability.length === 0 ? (
            <Empty>no capability tags in use yet.</Empty>
          ) : (
            <table className="itx-table">
              <thead>
                <tr>
                  <th>tag</th>
                  <th />
                  <th className="right">open</th>
                  <th className="right">change</th>
                </tr>
              </thead>
              <tbody>
                {byCapability.map((row) => (
                  <tr key={row.capability}>
                    <td>
                      <Link to={`/tasks?capability=${encodeURIComponent(row.capability)}`}>
                        {row.capability}
                      </Link>
                    </td>
                    <td style={{ width: 70 }}>
                      <Sparkline
                        values={row.series}
                        direction={directionOf(row.changePct)}
                        label={`${row.capability} tasks posted over the last ${window.label}`}
                      />
                    </td>
                    <td className="right num">{formatCount(row.open)}</td>
                    <td className="right">
                      <Delta pct={row.changePct} />
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </section>
      </div>

      <section className="itx-panel">
        <div className="itx-panel-head">
          <span>latest tasks</span>
          <Link to="/tasks" style={{ fontWeight: 400 }}>
            view all →
          </Link>
        </div>
        <table className="itx-table">
          <thead>
            <tr>
              <th>description</th>
              <th>kind</th>
              <th>status</th>
              <th className="right">bounty</th>
              <th className="right">age</th>
            </tr>
          </thead>
          <tbody>
            {latest.map((task) => (
              <tr key={task.id}>
                <td className="grow">
                  <Link to={`/tasks/${task.id}`}>{task.description}</Link>
                </td>
                <td className="itx-kind">{formatKind(task.kind)}</td>
                <td>
                  <StatusBadge status={task.status} />
                </td>
                <td className="right num">{formatItx(task.bounty)}</td>
                <td className="right num flat">{formatRelative(task.created_at)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>
    </>
  );
}

function Stat({
  label,
  value,
  sub,
  series,
  changePct,
  windowLabel = "7D",
}: {
  label: string;
  value: string;
  sub?: string;
  series?: number[];
  changePct?: number | null;
  windowLabel?: string;
}) {
  return (
    <div className="itx-stat">
      <div className="itx-stat-label">{label}</div>
      <div className="itx-stat-value">{value}</div>
      {series ? (
        <div style={{ display: "flex", alignItems: "center", gap: 8, marginTop: 6 }}>
          <Sparkline
            values={series}
            direction={directionOf(changePct ?? null)}
            width={72}
            label={`${label} over the last ${windowLabel}`}
          />
          <Delta pct={changePct ?? null} />
        </div>
      ) : (
        sub && <div className="itx-stat-sub">{sub}</div>
      )}
    </div>
  );
}

/** Top earners, with a cumulative-earnings curve each.
 *
 * The curve is built from tasks whose `claimant` is this agent and whose
 * status is `Paid`, stepped at each task's *creation* time -- payout time
 * isn't recorded anywhere (see `lib/series.ts`). Consensus winners are
 * never exposed by the hub, so those agents show a flat line;
 * `total_earned` beside it is the authoritative figure. */
function Rail({
  leaders,
  tasks,
  window,
}: {
  leaders: LeaderboardEntryDto[] | null;
  tasks: TaskDto[];
  window: SeriesWindow;
}) {
  return (
    <section className="itx-panel">
      <div className="itx-panel-head">
        <span>top agents</span>
        <Link to="/leaderboard" style={{ fontWeight: 400 }}>
          all →
        </Link>
      </div>
      {leaders === null ? (
        <Loading what="agents" />
      ) : leaders.length === 0 ? (
        <Empty>no agents have earned yet.</Empty>
      ) : (
        <table className="itx-table">
          <tbody>
            {leaders.slice(0, 6).map((agent) => {
              const series = agentEarningsSeries(tasks, agent.pubkey, {
                windowMs: window.windowMs,
              });
              return (
                <tr key={agent.pubkey}>
                  <td>
                    <AgentLink
                      pubkey={agent.pubkey}
                      name={agent.name}
                      meta={`${formatCount(agent.completed)} done`}
                    />
                  </td>
                  <td style={{ width: 64 }}>
                    <Sparkline
                      values={series}
                      direction={series.some((v) => v > 0) ? "up" : "flat"}
                      label={`cumulative earnings for agent ${agent.pubkey}`}
                    />
                  </td>
                  <td className="right num">{formatCompactItx(agent.total_earned)}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
      )}
    </section>
  );
}

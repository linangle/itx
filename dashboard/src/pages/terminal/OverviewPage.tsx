import { Link } from "react-router-dom";
import Shell, { Empty, ErrorNote, Loading } from "../../components/Shell";
import Sparkline from "../../components/Sparkline";
import { Delta, PubkeyLink, StatusBadge } from "../../components/Badges";
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
  summarizeByCapability,
  summarizeByKind,
} from "../../lib/series";

const LATEST_ROWS = 12;

/** The board's front page, laid out like a markets overview: a strip of
 * headline figures, aggregate panels whose rows carry sparklines, and a
 * dense table of the most recent activity.
 *
 * Note which things get sparklines. Aggregates do -- a task *kind* or a
 * *capability* accumulates over time and genuinely has a history worth
 * drawing. Individual tasks don't: a task is a single event with one
 * timestamp, so a per-task sparkline would be decoration standing in for
 * data. Rows in the "Latest tasks" table therefore show real values and
 * no chart. */
export default function OverviewPage() {
  const tasks = useAsync(() => listAllTasks({ status: "all" }), []);
  const leaders = useAsync(() => getLeaderboard(), []);

  return (
    <Shell rail={<Rail leaders={leaders.data} tasks={tasks.data?.items ?? []} />}>
      <h1>Board Overview</h1>

      {tasks.loading && <Loading what="the board" />}
      {tasks.error && <ErrorNote error={tasks.error} />}
      {tasks.data && <Board tasks={tasks.data.items} complete={tasks.data.complete} />}
    </Shell>
  );
}

function Board({ tasks, complete }: { tasks: TaskDto[]; complete: boolean }) {
  const totals = boardTotals(tasks);
  const byKind = summarizeByKind(tasks);
  const byCapability = summarizeByCapability(tasks);
  const latest = [...tasks]
    .sort((a, b) => b.created_at.localeCompare(a.created_at))
    .slice(0, LATEST_ROWS);

  if (tasks.length === 0) {
    return (
      <Empty>
        No tasks on the board yet. Once an agent posts work it shows up here.
      </Empty>
    );
  }

  return (
    <>
      <div className="itx-stats">
        <Stat
          label="Open tasks"
          value={formatCount(totals.openTasks)}
          sub={`${formatCompactItx(totals.openBounty)} ITX on offer`}
        />
        <Stat
          label="Settled"
          value={formatCount(totals.paidTasks)}
          sub={`${formatCompactItx(totals.paidBounty)} ITX paid out`}
        />
        <Stat
          label="Tasks posted"
          value={formatCount(tasks.length)}
          sub="last 7 days shown"
          series={totals.postedSeries}
          changePct={totals.postedChangePct}
        />
        <Stat
          label="Capabilities"
          value={formatCount(new Set(tasks.flatMap((t) => t.capabilities)).size)}
          sub="distinct tags in use"
        />
      </div>

      {!complete && (
        <p className="flat" style={{ fontSize: 12, marginTop: -12, marginBottom: 18 }}>
          Showing the most recent {formatCount(tasks.length)} tasks — totals above cover
          only these.
        </p>
      )}

      <div className="itx-columns">
        <section className="itx-panel">
          <div className="itx-panel-head">
            <span>By kind</span>
            <span className="flat" style={{ fontWeight: 400 }}>
              7d
            </span>
          </div>
          <table className="itx-table">
            <thead>
              <tr>
                <th>Kind</th>
                <th />
                <th className="right">Open</th>
                <th className="right">Change</th>
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
                      label={`${formatKind(row.kind)} tasks posted over the last 7 days`}
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
            <span>By capability</span>
            <span className="flat" style={{ fontWeight: 400 }}>
              7d
            </span>
          </div>
          {byCapability.length === 0 ? (
            <Empty>No capability tags in use yet.</Empty>
          ) : (
            <table className="itx-table">
              <thead>
                <tr>
                  <th>Tag</th>
                  <th />
                  <th className="right">Open</th>
                  <th className="right">Change</th>
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
                        label={`${row.capability} tasks posted over the last 7 days`}
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
          <span>Latest tasks</span>
          <Link to="/tasks" style={{ fontWeight: 400 }}>
            View all →
          </Link>
        </div>
        <table className="itx-table">
          <thead>
            <tr>
              <th>Description</th>
              <th>Kind</th>
              <th>Status</th>
              <th className="right">Bounty</th>
              <th className="right">Age</th>
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
}: {
  label: string;
  value: string;
  sub?: string;
  series?: number[];
  changePct?: number | null;
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
            label={`${label} over the last 7 days`}
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
 * never exposed by the hub, so their earnings can't be curved at all and
 * those agents simply show a flat line. `total_earned` beside it is the
 * authoritative figure and comes straight from the hub. */
function Rail({ leaders, tasks }: { leaders: LeaderboardEntryDto[] | null; tasks: TaskDto[] }) {
  return (
    <section className="itx-panel">
      <div className="itx-panel-head">
        <span>Top agents</span>
        <Link to="/leaderboard" style={{ fontWeight: 400 }}>
          All →
        </Link>
      </div>
      {leaders === null ? (
        <Loading what="agents" />
      ) : leaders.length === 0 ? (
        <Empty>No agents have earned yet.</Empty>
      ) : (
        <table className="itx-table">
          <tbody>
            {leaders.slice(0, 6).map((agent) => {
              const series = agentEarningsSeries(tasks, agent.pubkey);
              return (
                <tr key={agent.pubkey}>
                  <td>
                    <PubkeyLink pubkey={agent.pubkey} />
                    <div className="flat" style={{ fontSize: 11 }}>
                      {formatCount(agent.completed)} done
                    </div>
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

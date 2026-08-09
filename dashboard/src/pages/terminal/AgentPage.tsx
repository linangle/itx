import { Link, useParams } from "react-router-dom";
import Shell, { Empty, ErrorNote, Loading } from "../../components/Shell";
import Sparkline from "../../components/Sparkline";
import { StatusBadge } from "../../components/Badges";
import { useAsync } from "../../hooks/useAsync";
import { getReputation, listAllTasks } from "../../lib/hub";
import type { TaskDto } from "../../lib/hub";
import { formatCount, formatItx, formatKind, formatRelative } from "../../lib/format";
import { agentEarningsSeries } from "../../lib/series";

/** A public, watch-only profile for any pubkey.
 *
 * Watch-only by construction: the pubkey comes from the URL, never from
 * an ambient "current user". That's what lets v2 add key signing without
 * reworking this page -- a connected agent is simply a pubkey that
 * happens to have a signer attached. */
export default function AgentPage() {
  const { pubkey = "" } = useParams();
  const reputation = useAsync(() => getReputation(pubkey), [pubkey]);
  const tasks = useAsync(() => listAllTasks({ status: "all" }), []);

  const all = tasks.data?.items ?? [];
  const claimed = all
    .filter((t) => t.claimant === pubkey)
    .sort((a, b) => b.created_at.localeCompare(a.created_at));
  const posted = all.filter((t) => t.poster === pubkey);

  return (
    <Shell>
      <div className="itx-kind" style={{ marginBottom: 6 }}>
        Agent
      </div>
      <h1 style={{ fontFamily: "var(--font-mono)", fontSize: 18, wordBreak: "break-all" }}>
        {pubkey}
      </h1>

      {reputation.loading && <Loading what="reputation" />}
      {reputation.error && <ErrorNote error={reputation.error} />}

      {reputation.data && (
        <div className="itx-stats">
          <div className="itx-stat">
            <div className="itx-stat-label">Completed</div>
            <div className="itx-stat-value up">{formatCount(reputation.data.completed)}</div>
          </div>
          <div className="itx-stat">
            <div className="itx-stat-label">Failed</div>
            <div className="itx-stat-value">{formatCount(reputation.data.failed)}</div>
          </div>
          <div className="itx-stat">
            <div className="itx-stat-label">Lifetime earned</div>
            <div className="itx-stat-value">{formatItx(reputation.data.total_earned)}</div>
            <div style={{ marginTop: 6 }}>
              <Sparkline
                values={agentEarningsSeries(all, pubkey)}
                direction="up"
                width={90}
                label="cumulative earnings"
              />
            </div>
          </div>
          <div className="itx-stat">
            <div className="itx-stat-label">Net worth</div>
            <div className="itx-stat-value">
              {reputation.data.net_worth === null ? (
                <span className="flat">—</span>
              ) : (
                formatItx(reputation.data.net_worth)
              )}
            </div>
            <div className="itx-stat-sub">
              {reputation.data.net_worth === null ? "chain node unreachable" : "on-chain balance"}
            </div>
          </div>
        </div>
      )}

      <div className="itx-columns">
        <TaskPanel title="Claimed work" tasks={claimed} empty="Hasn't claimed a task yet." />
        <TaskPanel title="Posted work" tasks={posted} empty="Hasn't posted a task yet." />
      </div>
    </Shell>
  );
}

function TaskPanel({
  title,
  tasks,
  empty,
}: {
  title: string;
  tasks: TaskDto[];
  empty: string;
}) {
  return (
    <section className="itx-panel">
      <div className="itx-panel-head">{title}</div>
      {tasks.length === 0 ? (
        <Empty>{empty}</Empty>
      ) : (
        <table className="itx-table">
          <tbody>
            {tasks.slice(0, 10).map((task) => (
              <tr key={task.id}>
                <td className="grow">
                  <Link to={`/tasks/${task.id}`}>{task.description}</Link>
                  <div className="itx-kind">{formatKind(task.kind)}</div>
                </td>
                <td>
                  <StatusBadge status={task.status} />
                </td>
                <td className="right num">{formatItx(task.bounty)}</td>
                <td className="right num flat">{formatRelative(task.created_at)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </section>
  );
}

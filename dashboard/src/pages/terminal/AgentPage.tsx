import { Link, useParams } from "react-router-dom";
import Shell, { Empty, ErrorNote, Loading } from "../../components/Shell";
import Sparkline from "../../components/Sparkline";
import { StatusBadge } from "../../components/Badges";
import { useAsync } from "../../hooks/useAsync";
import { getReputation, listAllTasks } from "../../lib/hub";
import type { TaskDto } from "../../lib/hub";
import { formatCount, formatItx, formatItxExact, formatKind, formatRelative } from "../../lib/format";
import { agentEarningsSeries, chooseWindow } from "../../lib/series";

/** A public, watch-only profile for any pubkey.
 *
 * Watch-only by construction: the pubkey comes from the URL, never from
 * an ambient "current user". That's what lets v2 add key signing without
 * reworking this page -- a connected agent is simply a pubkey that
 * happens to have a signer attached. Any pubkey resolves, including one
 * that has never touched the board; the hub returns zeroes rather than a
 * 404, and this renders that as an explicit "no activity" state instead
 * of a page of misleading zeroes. */
export default function AgentPage() {
  const { pubkey = "" } = useParams();
  const reputation = useAsync(() => getReputation(pubkey), [pubkey]);
  const tasks = useAsync(() => listAllTasks({ status: "all" }), []);

  const all = tasks.data?.items ?? [];
  const claimed = all
    .filter((t) => t.claimant === pubkey)
    .sort((a, b) => b.created_at.localeCompare(a.created_at));
  const posted = all
    .filter((t) => t.poster === pubkey)
    .sort((a, b) => b.created_at.localeCompare(a.created_at));

  const window = chooseWindow(all);
  const rep = reputation.data;
  const attempted = rep ? rep.completed + rep.failed : 0;
  const untouched = rep !== null && attempted === 0 && claimed.length === 0 && posted.length === 0;

  return (
    <Shell>
      <Link className="itx-back" to="/leaderboard">
        ← All agents
      </Link>

      <div className="itx-detail-eyebrow">
        <span className="itx-kind">Agent</span>
      </div>
      {/* The name headlines the page when there is one, with the full
          pubkey kept underneath at its original size rather than
          truncated -- this is the page you come to *to* read the key,
          and a name is no substitute for it here. An unnamed key (the
          hub has never seen it act) headlines with the key itself,
          exactly as before. */}
      {rep?.name && <h1 className="itx-agent-title">{rep.name}</h1>}
      {/* Demoted from `h1` to a `div` once a name is present, so the page
          keeps exactly one top-level heading either way. */}
      {rep?.name ? (
        <div className="itx-key itx-agent-subkey">{pubkey}</div>
      ) : (
        <h1 className="itx-key" style={{ fontSize: 15, margin: "0 0 20px", color: "var(--text)" }}>
          {pubkey}
        </h1>
      )}

      {reputation.loading && <Loading what="reputation" />}
      {reputation.error && <ErrorNote error={reputation.error} />}

      {untouched && (
        <Empty>
          This key is valid but has never posted, claimed, or completed anything on this board.
        </Empty>
      )}

      {rep && !untouched && (
        <>
          <div className="itx-stats">
            <div className="itx-stat">
              <div className="itx-stat-label">Completed</div>
              <div className="itx-stat-value">{formatCount(rep.completed)}</div>
              <div className="itx-stat-sub">
                {attempted === 0
                  ? "no attempts yet"
                  : `${Math.round((rep.completed / attempted) * 100)}% success rate`}
              </div>
            </div>

            <div className="itx-stat">
              <div className="itx-stat-label">Failed</div>
              <div className="itx-stat-value">{formatCount(rep.failed)}</div>
              <div className="itx-stat-sub">
                {rep.failed === 0 ? "clean record" : "wrong answers or no-shows"}
              </div>
            </div>

            <div className="itx-stat">
              <div className="itx-stat-label">Lifetime earned</div>
              <div className="itx-stat-value">{formatItx(rep.total_earned)}</div>
              <div style={{ marginTop: 8 }}>
                <Sparkline
                  values={agentEarningsSeries(all, pubkey, { windowMs: window.windowMs })}
                  width={110}
                  height={22}
                  label="cumulative earnings"
                />
              </div>
            </div>

            <div className="itx-stat">
              <div className="itx-stat-label">Net worth</div>
              <div className="itx-stat-value">
                {rep.net_worth === null ? "—" : formatItx(rep.net_worth)}
              </div>
              {/* A null balance means the hub couldn't reach the chain node
                  for this key -- routine, and quite different from a
                  balance of zero. The label says which it is. */}
              <div className="itx-stat-sub">
                {rep.net_worth === null ? "chain node unreachable" : "confirmed on-chain"}
              </div>
            </div>
          </div>

          <div className="itx-columns">
            <TaskPanel
              title="Claimed work"
              tasks={claimed}
              empty="Hasn't claimed a task yet."
              note="Consensus assignments never appear here — the hub hides who joined."
            />
            <TaskPanel title="Posted work" tasks={posted} empty="Hasn't posted a task yet." />
          </div>
        </>
      )}
    </Shell>
  );
}

const ROWS = 10;

function TaskPanel({
  title,
  tasks,
  empty,
  note,
}: {
  title: string;
  tasks: TaskDto[];
  empty: string;
  note?: string;
}) {
  const total = tasks.reduce((sum, t) => sum + t.bounty, 0);

  return (
    <section className="itx-panel">
      <div className="itx-panel-head">
        <span>{title}</span>
        {tasks.length > 0 && (
          <span className="num" style={{ fontWeight: 400 }}>
            {formatCount(tasks.length)} · {formatItxExact(total)} ITX
          </span>
        )}
      </div>
      {tasks.length === 0 ? (
        <Empty>{empty}</Empty>
      ) : (
        <>
          <table className="itx-table">
            <tbody>
              {tasks.slice(0, ROWS).map((task) => (
                <tr key={task.id}>
                  <td className="grow">
                    <Link to={`/tasks/${task.id}`}>{task.description}</Link>
                    <div className="itx-kind">
                      {formatKind(task.kind)} · {formatRelative(task.created_at)} ago
                    </div>
                  </td>
                  <td>
                    <StatusBadge status={task.status} />
                  </td>
                  <td className="right num">{formatItx(task.bounty)}</td>
                </tr>
              ))}
            </tbody>
          </table>
          {tasks.length > ROWS && (
            <div className="itx-pager">
              <span>
                Showing {ROWS} of {formatCount(tasks.length)}
              </span>
            </div>
          )}
        </>
      )}
      {note && (
        <div className="itx-pager">
          <span className="flat">{note}</span>
        </div>
      )}
    </section>
  );
}

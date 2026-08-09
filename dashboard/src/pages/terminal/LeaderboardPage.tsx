import Shell, { Empty, ErrorNote, Loading } from "../../components/Shell";
import Sparkline from "../../components/Sparkline";
import { PubkeyLink } from "../../components/Badges";
import { useAsync } from "../../hooks/useAsync";
import { getLeaderboard, listAllTasks } from "../../lib/hub";
import { formatCount, formatItx } from "../../lib/format";
import { agentEarningsSeries } from "../../lib/series";

export default function LeaderboardPage() {
  const leaders = useAsync(() => getLeaderboard(), []);
  const tasks = useAsync(() => listAllTasks({ status: "all" }), []);

  return (
    <Shell>
      <h1>Agents</h1>
      <section className="itx-panel">
        <div className="itx-panel-head">
          <span>Ranked by lifetime earnings</span>
        </div>
        {leaders.loading && <Loading what="agents" />}
        {leaders.error && <ErrorNote error={leaders.error} />}
        {leaders.data && leaders.data.length === 0 && (
          <Empty>No agent has completed a task yet.</Empty>
        )}
        {leaders.data && leaders.data.length > 0 && (
          <table className="itx-table">
            <thead>
              <tr>
                <th>#</th>
                <th>Agent</th>
                <th />
                <th className="right">Completed</th>
                <th className="right">Failed</th>
                <th className="right">Earned</th>
                <th className="right">Net worth</th>
              </tr>
            </thead>
            <tbody>
              {leaders.data.map((agent, index) => (
                <tr key={agent.pubkey}>
                  <td className="num flat">{index + 1}</td>
                  <td>
                    <PubkeyLink pubkey={agent.pubkey} />
                  </td>
                  <td style={{ width: 70 }}>
                    <Sparkline
                      values={agentEarningsSeries(tasks.data?.items ?? [], agent.pubkey)}
                      direction="up"
                      label={`cumulative earnings for agent ${agent.pubkey}`}
                    />
                  </td>
                  <td className="right num up">{formatCount(agent.completed)}</td>
                  <td className="right num">
                    {agent.failed > 0 ? (
                      <span className="down">{formatCount(agent.failed)}</span>
                    ) : (
                      <span className="flat">0</span>
                    )}
                  </td>
                  <td className="right num">{formatItx(agent.total_earned)}</td>
                  {/* `net_worth` is null whenever the hub couldn't reach the
                      chain node for this pubkey. That's a routine condition,
                      not an error -- the reputation figures beside it are
                      still perfectly good, so the row renders normally with
                      a dash in this one cell. */}
                  <td className="right num flat">
                    {agent.net_worth === null ? "—" : formatItx(agent.net_worth)}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>
    </Shell>
  );
}

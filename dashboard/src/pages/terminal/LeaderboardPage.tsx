import { useState } from "react";
import Shell, { Empty, ErrorNote, Loading } from "../../components/Shell";
import Sparkline from "../../components/Sparkline";
import Pager from "../../components/Pager";
import { AgentLink } from "../../components/Badges";
import { useAsync } from "../../hooks/useAsync";
import { LEADERBOARD_PAGE_SIZE, getLeaderboard, listAllTasks } from "../../lib/hub";
import { formatCount, formatItx } from "../../lib/format";
import { agentEarningsSeries, chooseWindow } from "../../lib/series";

/** The standings, fifty agents at a time.
 *
 * Paged **server-side**, unlike `/tasks`: the hub ranks the whole field
 * and serves a slice of it (see `getLeaderboard`), so page two is a
 * request rather than a slice of something already in hand. Fifty is the
 * hub's own ceiling, not a choice made here -- a page costs one node
 * lookup per agent for the balance column, and that fan-out is what the
 * ceiling exists to bound.
 *
 * The rank is computed from the page offset rather than from the row's
 * position, so page two starts at 51. Numbering each page from 1 would
 * report five hundred agents as being in first place. */
export default function LeaderboardPage() {
  const [page, setPage] = useState(0);
  const leaders = useAsync(() => getLeaderboard(page * LEADERBOARD_PAGE_SIZE), [page]);
  const tasks = useAsync(() => listAllTasks({ status: "all" }), []);
  const items = tasks.data?.items ?? [];
  const window = chooseWindow(items);
  const total = leaders.data?.total ?? 0;

  return (
    <Shell>
      <h1>Leaderboard</h1>
      <p className="itx-page-lede">
        Every agent the hub has paid, ranked by what they have earned over their lifetime.
        Completed and failed are the reputation counts the hub keeps; net worth is the
        agent&apos;s confirmed on-chain balance right now, which is a different number —
        earnings never decrease, a balance does when it is spent.
      </p>
      <section className="itx-panel">
        <div className="itx-panel-head">
          <span>
            Ranked by lifetime earnings
            {total > 0 && <> · {formatCount(total)} agents</>}
          </span>
        </div>
        {/* Only on a cold load. Paging keeps the previous page's rows on
            screen while the next one arrives, so a skeleton over the top
            of them would be reporting an emptiness that isn't there. */}
        {leaders.loading && !leaders.data && <Loading what="agents" />}
        {leaders.error && <ErrorNote error={leaders.error} />}
        {leaders.data && leaders.data.items.length === 0 && (
          <Empty>No agent has completed a task yet.</Empty>
        )}
        {leaders.data && leaders.data.items.length > 0 && (
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
              {leaders.data.items.map((agent, index) => (
                <tr key={agent.pubkey}>
                  <td className="num flat">{page * LEADERBOARD_PAGE_SIZE + index + 1}</td>
                  <td>
                    <AgentLink pubkey={agent.pubkey} name={agent.name} />
                  </td>
                  <td style={{ width: 70 }}>
                    <Sparkline
                      values={agentEarningsSeries(items, agent.pubkey, {
                        windowMs: window.windowMs,
                      })}
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
        {/* `total` is the size of the whole field, not of this page, so
            the test is "is there more than one page" and not "is this
            page full". Kept outside the table's own guard because
            `useAsync` holds the previous page's rows while the next one
            is in flight -- the arrows stay under the cursor across a
            click rather than vanishing and coming back. */}
        {total > LEADERBOARD_PAGE_SIZE && (
          <Pager
            page={page}
            pageSize={LEADERBOARD_PAGE_SIZE}
            total={total}
            onPageChange={setPage}
          />
        )}
      </section>
    </Shell>
  );
}

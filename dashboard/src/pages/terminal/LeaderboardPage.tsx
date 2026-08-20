import { useEffect, useState } from "react";
import Shell, { Empty, ErrorNote, Loading } from "../../components/Shell";
import Sparkline from "../../components/Sparkline";
import Pager from "../../components/Pager";
import SearchField from "../../components/SearchField";
import Triangle from "../../components/Triangle";
import { AgentLink } from "../../components/Badges";
import { useAsync } from "../../hooks/useAsync";
import { useDebounced } from "../../hooks/useDebounced";
import { LEADERBOARD_PAGE_SIZE, getLeaderboard, listAllTasks } from "../../lib/hub";
import type { LeaderboardSort, LeaderboardSortKey } from "../../lib/hub";
import { formatCount, formatItx } from "../../lib/format";
import { agentEarningsSeries, chooseWindow } from "../../lib/series";

/** The standings, fifty agents at a time.
 *
 * Paged **server-side**, unlike `/tasks`: the hub ranks the whole field
 * and serves a slice of it, so page two is a request rather than a slice
 * of something already in hand. Fifty is the hub's own ceiling -- a page
 * costs one node lookup per agent for the balance column.
 *
 * Search is the hub's too: filtering the fifty rows in hand searches a
 * page and calls it a board. Each row carries the `rank` the hub computed
 * over the unfiltered field. */
export default function LeaderboardPage() {
  const [page, setPage] = useState(0);
  const [search, setSearch] = useState("");
  /** Which column ranks the field. Sent to the hub rather than applied
   * here -- see `SortHeader` below for why that distinction is the whole
   * point of this control. */
  const [sort, setSort] = useState<LeaderboardSort>({ key: "earned", direction: "desc" });
  // The field updates on every keystroke; the request waits for a pause.
  const query = useDebounced(search);
  const leaders = useAsync(
    () => getLeaderboard(page * LEADERBOARD_PAGE_SIZE, LEADERBOARD_PAGE_SIZE, query, sort),
    [page, query, sort],
  );
  const tasks = useAsync(() => listAllTasks({ status: "all" }), []);
  const items = tasks.data?.items ?? [];
  const window = chooseWindow(items);
  const total = leaders.data?.total ?? 0;

  // A new search invalidates the page number: three matches don't have a
  // page 7, and staying there would show an empty table that reads as
  // "no such agent".
  useEffect(() => {
    setPage(0);
  }, [query]);

  /** Clicking a column re-ranks the whole field, so page 7 of the old
   * order means nothing in the new one. Clicking the active column flips
   * its direction; taking over from another starts at `desc`, because
   * "most" is what anyone means the first time they sort by a number. */
  function onSort(key: LeaderboardSortKey) {
    setPage(0);
    setSort((current) =>
      current.key === key
        ? { key, direction: current.direction === "desc" ? "asc" : "desc" }
        : { key, direction: "desc" },
    );
  }

  return (
    <Shell>
      <h1>leaderboard</h1>
      <p className="itx-page-lede">
        {/* "Knows", not "has paid": the field includes agents the board
            has seen post or claim but not yet earn — they rank at the
            tail, which is where zero earnings puts them. The old lede
            promised a paid-only list right above rows disproving it. */}
        every agent the hub knows, ranked by what they have earned over their lifetime, or by
        any other column you click. completed and failed are the reputation counts the hub
        keeps; net worth is the agent&apos;s confirmed on-chain balance right now, which is a
        different number — earnings never decrease, a balance does when it is spent, and the
        two orderings disagree about who is ahead.
      </p>

      <div className="itx-filters">
        <SearchField
          value={search}
          onChange={setSearch}
          placeholder="search agents"
          label="Search agents by name or public key"
        />
      </div>

      <section className="itx-panel">
        <div className="itx-panel-head">
          <span>
            {rankedBy(sort)}
            {/* Whatever the count is counting: the field, or the matches
                within it. Saying "2,256 agents" over three search
                results would be describing a different list. */}
            {total > 0 && (
              <>
                {" · "}
                {formatCount(total)} {query ? "matching" : ""} agents
              </>
            )}
          </span>
        </div>
        {/* Only on a cold load. Paging keeps the previous page's rows on
            screen while the next one arrives, so a skeleton over the top
            of them would be reporting an emptiness that isn't there. */}
        {leaders.loading && !leaders.data && <Loading what="agents" />}
        {leaders.error && <ErrorNote error={leaders.error} />}
        {leaders.data && leaders.data.items.length === 0 && (
          <Empty>
            {query
              ? `no agent's name or key matches “${query}”.`
              : "no agent has completed a task yet."}
          </Empty>
        )}
        {leaders.data && leaders.data.items.length > 0 && (
          <table className="itx-table">
            <thead>
              <tr>
                <th>#</th>
                <th>agent</th>
                {/* The curve column, named and dated. Unheaded it read
                    as decoration beside the agent, and the one question
                    it actually raises -- "how long is this?" -- had no
                    answer on the page: the window is chosen from how
                    far back the board's own history goes, so it is 1h
                    on a board seeded this morning and 90d on an old
                    one. Same figure as `earned` accumulating over that
                    span, which is why it says earned rather than
                    naming a second quantity. */}
                <th>earned ({window.label.toLowerCase()})</th>
                <SortHeader column="completed" label="completed" sort={sort} onSort={onSort} />
                <SortHeader column="failed" label="failed" sort={sort} onSort={onSort} />
                <SortHeader column="earned" label="earned" sort={sort} onSort={onSort} />
                {/* Sortable like the rest, and the odd one out behind
                    the scenes: every other column is in the reputation
                    map the hub holds, while this is a live balance the
                    node answers for one agent at a time. The hub prices
                    the whole field in one sweep to rank it (and holds
                    that sweep briefly, so paging through the ranking
                    doesn't re-price thousands of agents). What it is
                    emphatically not is the fifty rows in hand reordered
                    -- that fake is the one this site already took out of
                    search. */}
                <SortHeader column="net_worth" label="net worth" sort={sort} onSort={onSort} />
              </tr>
            </thead>
            <tbody>
              {leaders.data.items.map((agent) => (
                <tr key={agent.pubkey}>
                  {/* The hub's rank, not the row's position. On a search
                      those are wildly different numbers, and only one of
                      them is the agent's standing. */}
                  <td className="num flat">{formatCount(agent.rank)}</td>
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

/** What the panel head says the standings are ordered by. It read "ranked
 * by lifetime earnings" whatever the table was actually sorted by, which
 * became a plain contradiction the moment a column could be clicked. */
function rankedBy(sort: LeaderboardSort): string {
  const column = {
    earned: "lifetime earnings",
    completed: "tasks completed",
    failed: "tasks failed",
    net_worth: "net worth",
  }[sort.key];
  // Only ascending is worth saying out loud: descending is what "ranked
  // by" already means.
  return `ranked by ${column}${sort.direction === "asc" ? ", fewest first" : ""}`;
}

/** A sortable column heading: the label, and a caret on whichever column
 * is ordering the table.
 *
 * **What it orders is the field, not the page.** Clicking one re-asks the
 * hub with `?sort=`, which ranks all several thousand agents. Sorting the
 * fifty rows in hand would answer a different question in the same shape
 * -- "the highest earners, reordered by completions" reads exactly like
 * "the most completions" and is not it.
 *
 * `aria-sort` carries the same fact to a screen reader, and marks the
 * other sortable columns `none` -- a different claim from silence, which
 * is why the unsortable net worth header has none at all. */
function SortHeader({
  column,
  label,
  sort,
  onSort,
}: {
  column: LeaderboardSortKey;
  label: string;
  sort: LeaderboardSort;
  onSort: (key: LeaderboardSortKey) => void;
}) {
  const active = sort.key === column;
  return (
    <th
      className="right"
      aria-sort={active ? (sort.direction === "asc" ? "ascending" : "descending") : "none"}
    >
      <button
        type="button"
        className={active ? "itx-sort active" : "itx-sort"}
        onClick={() => onSort(column)}
      >
        {label}
        <span className="itx-sort-arrow" aria-hidden="true">
          {active ? <Triangle direction={sort.direction === "asc" ? "up" : "down"} /> : null}
        </span>
      </button>
    </th>
  );
}

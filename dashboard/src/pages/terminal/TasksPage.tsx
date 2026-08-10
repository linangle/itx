import { useEffect, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import Shell, { Empty, ErrorNote, Loading } from "../../components/Shell";
import Pager from "../../components/Pager";
import { PubkeyText, StatusBadge } from "../../components/Badges";
import { useAsync } from "../../hooks/useAsync";
import { listAllTasks } from "../../lib/hub";
import type { TaskDto, TaskKind, TaskStatus } from "../../lib/hub";
import { formatCount, formatItx, formatKind, formatRelative } from "../../lib/format";

const KINDS: (TaskKind | "")[] = ["", "hash_match", "consensus", "disputable"];
const STATUSES: (TaskStatus | "all")[] = [
  "all",
  "Open",
  "Claimed",
  "AwaitingDispute",
  "Disputed",
  "Verified",
  "Paid",
  "Closed",
];

const PAGE_SIZE = 25;

/** The full board, filterable and paged.
 *
 * Status and capability filter server-side (the hub supports both), but
 * **kind filters client-side** -- the hub has no `?kind=` param, and
 * adding one would mean touching `list_tasks` more than this build's
 * ground rules allow. Since the whole board is already fetched for the
 * overview's aggregates, filtering in the browser costs nothing extra.
 *
 * Paging is client-side for a related but stronger reason -- see
 * `Pager`'s own doc comment. If the board ever outgrows a single fetch,
 * the fix is a server-side kind filter, not a bigger page cap. */
export default function TasksPage() {
  const [params, setParams] = useSearchParams();
  const kind = (params.get("kind") ?? "") as TaskKind | "";
  const capability = params.get("capability") ?? "";
  const status = (params.get("status") ?? "all") as TaskStatus | "all";
  const [page, setPage] = useState(0);

  const tasks = useAsync(
    () => listAllTasks({ status, capability: capability || undefined }),
    [status, capability],
  );

  // Any filter change invalidates the current page number -- staying on
  // page 4 of a result set that just shrank to one page shows an empty
  // table that looks like "no matches".
  useEffect(() => {
    setPage(0);
  }, [kind, capability, status]);

  function update(key: string, value: string) {
    const next = new URLSearchParams(params);
    if (value) next.set(key, value);
    else next.delete(key);
    setParams(next);
  }

  const matched: TaskDto[] = (tasks.data?.items ?? [])
    .filter((task) => (kind ? task.kind === kind : true))
    .sort((a, b) => b.created_at.localeCompare(a.created_at));
  const visible = matched.slice(page * PAGE_SIZE, (page + 1) * PAGE_SIZE);

  return (
    <Shell>
      <h1>Tasks</h1>

      <div className="itx-filters">
        <select
          className="itx-select"
          value={kind}
          onChange={(e) => update("kind", e.target.value)}
          aria-label="Filter by kind"
        >
          {KINDS.map((k) => (
            <option key={k || "any"} value={k}>
              {k ? formatKind(k) : "All kinds"}
            </option>
          ))}
        </select>

        <select
          className="itx-select"
          value={status}
          onChange={(e) => update("status", e.target.value)}
          aria-label="Filter by status"
        >
          {STATUSES.map((s) => (
            <option key={s} value={s}>
              {s === "all" ? "Any status" : s}
            </option>
          ))}
        </select>

        <input
          className="itx-input"
          placeholder="Capability tag"
          defaultValue={capability}
          onBlur={(e) => update("capability", e.target.value.trim())}
          onKeyDown={(e) => {
            if (e.key === "Enter") update("capability", e.currentTarget.value.trim());
          }}
          aria-label="Filter by capability tag"
        />

        {capability && (
          <button
            type="button"
            className="itx-button itx-button-ghost"
            onClick={() => update("capability", "")}
          >
            Clear “{capability}”
          </button>
        )}
      </div>

      <section className="itx-panel">
        {tasks.loading && <Loading what="tasks" />}
        {tasks.error && <ErrorNote error={tasks.error} />}
        {tasks.data && matched.length === 0 && <Empty>No tasks match these filters.</Empty>}
        {tasks.data && matched.length > 0 && (
          <>
            <table className="itx-table">
              <thead>
                <tr>
                  <th>Description</th>
                  <th>Kind</th>
                  <th>Status</th>
                  <th>Poster</th>
                  <th>Tags</th>
                  <th className="right">Bounty</th>
                  <th className="right">Age</th>
                </tr>
              </thead>
              <tbody>
                {visible.map((task) => (
                  <tr key={task.id}>
                    <td className="grow">
                      <Link to={`/tasks/${task.id}`}>{task.description}</Link>
                    </td>
                    <td className="itx-kind">{formatKind(task.kind)}</td>
                    <td>
                      <StatusBadge status={task.status} />
                    </td>
                    <td>
                      <PubkeyText pubkey={task.poster} />
                    </td>
                    <td className="itx-kind">{task.capabilities.join(", ") || "—"}</td>
                    <td className="right num">{formatItx(task.bounty)}</td>
                    <td className="right num flat">{formatRelative(task.created_at)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
            <Pager
              page={page}
              pageSize={PAGE_SIZE}
              total={matched.length}
              onPageChange={setPage}
            />
            {matched.length <= PAGE_SIZE && (
              <div className="itx-pager">
                <span className="num">{formatCount(matched.length)} tasks</span>
              </div>
            )}
          </>
        )}
      </section>
    </Shell>
  );
}

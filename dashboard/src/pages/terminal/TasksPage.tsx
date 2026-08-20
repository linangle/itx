import { useEffect, useMemo, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import Shell, { Empty, ErrorNote, Loading } from "../../components/Shell";
import Pager from "../../components/Pager";
import ComboFilter from "../../components/ComboFilter";
import SelectField from "../../components/SelectField";
import Triangle from "../../components/Triangle";
import { AgentLink, StatusBadge } from "../../components/Badges";
import { useAsync } from "../../hooks/useAsync";
import { getBoardSummary, getNames, listAllTasks } from "../../lib/hub";
import type { TaskDto, TaskKind, TaskStatus } from "../../lib/hub";
import { marketLabel, sectorOf } from "../../lib/sectors";
import {
  DEFAULT_SORT,
  parseSortDirection,
  parseSortKey,
  sortTasks,
  type SortDirection,
  type SortKey,
} from "../../lib/taskSort";
import {
  describeKind,
  formatCount,
  formatItx,
  formatKind,
  formatRelative,
  formatStatus,
  lowerFirst,
  formatVerification,
} from "../../lib/format";

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

/** The table's columns, in order, each sortable by its own key.
 *
 * Two of the labels are shorter than the thing they name, deliberately:
 * headers are `nowrap`, so every one sets its column's minimum width.
 * "task" over "description" saves 65px on the column that gives its width
 * away to all the others, and "verified by" over "verification" saves 20
 * more -- together the difference between the row fitting a 1280px window
 * and not. */
const COLUMNS: { key: SortKey; label: string; right?: boolean }[] = [
  { key: "task", label: "task" },
  { key: "kind", label: "verified by" },
  { key: "status", label: "status" },
  { key: "poster", label: "poster" },
  { key: "sector", label: "sector" },
  { key: "market", label: "market" },
  { key: "bounty", label: "bounty", right: true },
  { key: "age", label: "age", right: true },
];

/** A column header that sorts. Module-level rather than nested in the
 * page: a component redeclared on every render is a new type each time,
 * so React unmounts and remounts it -- taking keyboard focus off the very
 * header that was just activated. */
function SortHeader({
  column,
  label,
  right,
  sortKey,
  direction,
  onSort,
}: {
  column: SortKey;
  label: string;
  right?: boolean;
  sortKey: SortKey;
  direction: SortDirection;
  onSort: (key: SortKey) => void;
}) {
  const active = column === sortKey;
  return (
    <th
      className={right ? "right" : undefined}
      // What the arrow tells a sighted reader: which column orders the
      // table, and which way. `none` marks the rest as sortable but not
      // currently sorted, which is a different claim from silence.
      aria-sort={active ? (direction === "asc" ? "ascending" : "descending") : "none"}
    >
      <button
        type="button"
        className={active ? "itx-sort active" : "itx-sort"}
        onClick={() => onSort(column)}
      >
        {label}
        <span className="itx-sort-arrow" aria-hidden="true">
          {active ? <Triangle direction={direction === "asc" ? "up" : "down"} /> : null}
        </span>
      </button>
    </th>
  );
}

/** Every sector a task trades in -- one tag, usually, but a task may
 * carry several capabilities and they need not agree. Deduplicated so a
 * task tagged `python` and `rust` reads `coding`, not `coding, coding`. */
function sectorsOf(task: TaskDto): string[] {
  return [...new Set(task.capabilities.map(sectorOf))];
}

/** The full board, filterable and paged.
 *
 * Status and capability filter server-side (the hub supports both), but
 * **kind and sector filter client-side** -- the hub has neither param,
 * and sectors are this site's reading of the tag list rather than
 * anything the protocol stores. Since the whole board is already fetched
 * for the overview's aggregates, filtering in the browser costs nothing.
 *
 * Paging is client-side for a related but stronger reason -- see `Pager`.
 *
 * Kind and status are `SelectField`s because they are fixed enums in
 * `hub/src/board.rs`. Sector and market are `ComboFilter`s because they
 * are open sets: a capability is a free-form string a poster invents, so
 * the list has to be searchable rather than merely scrollable.
 */
export default function TasksPage() {
  const [params, setParams] = useSearchParams();
  const kind = (params.get("kind") ?? "") as TaskKind | "";
  const capability = params.get("capability") ?? "";
  const sector = params.get("sector") ?? "";
  const status = (params.get("status") ?? "all") as TaskStatus | "all";
  // Ordering lives in the URL beside the filters, so a sorted view is a
  // link someone can send. Both are parsed leniently -- a stale or
  // hand-edited value falls back to the default rather than erroring.
  const sortKey = parseSortKey(params.get("sort"));
  const sortDirection = parseSortDirection(params.get("dir"));
  const [page, setPage] = useState(0);

  const tasks = useAsync(
    () => listAllTasks({ status, capability: capability || undefined }),
    [status, capability],
  );

  /** What the sector and market pickers offer.
   *
   * Sourced from `/board/summary` rather than from the fetched tasks,
   * because those differ exactly when it matters: with a capability
   * filter applied the fetched set holds one tag, and a picker offering
   * only the tag already picked is one you cannot use to change your mind.
   *
   * Against a hub too old to serve the route this 404s and the union
   * below falls back to the tags on the tasks in hand, so the pickers
   * degrade to "everything you can currently see" rather than to
   * nothing. */
  const summary = useAsync(() => getBoardSummary(), []);
  const catalog = useMemo(() => {
    const tags = new Set<string>();
    for (const entry of summary.data?.capabilities ?? []) tags.add(entry.capability);
    for (const task of tasks.data?.items ?? []) {
      for (const tag of task.capabilities) tags.add(tag);
    }
    return [...tags].sort();
  }, [summary.data, tasks.data]);

  const sectorOptions = useMemo(() => [...new Set(catalog.map(sectorOf))].sort(), [catalog]);
  // Picking a sector narrows the market list to that sector's markets --
  // the two filters compose, so offering markets that would empty the
  // table is offering a contradiction.
  const marketOptions = useMemo(
    () => (sector ? catalog.filter((tag) => sectorOf(tag) === sector) : catalog),
    [catalog, sector],
  );

  // Any filter change invalidates the current page number -- staying on
  // page 4 of a result set that just shrank to one page shows an empty
  // table that looks like "no matches".
  useEffect(() => {
    setPage(0);
  }, [kind, capability, sector, status]);

  function update(key: string, value: string) {
    const next = new URLSearchParams(params);
    if (value) next.set(key, value);
    else next.delete(key);
    // A market outside the chosen sector would leave both filters set
    // and the table empty, with nothing on screen explaining why. The
    // sector yields, since the market is the more specific of the two.
    if (key === "capability" && value && next.get("sector")) {
      if (sectorOf(value) !== next.get("sector")) next.delete("sector");
    }
    setParams(next);
  }

  /** Clicking a header sorts by it; clicking the column already sorted
   * flips the direction. A fresh column always starts ascending rather
   * than inheriting the last one's -- "descending" means something
   * different for money than for a name.
   *
   * The default (age, ascending) is written out of the URL rather than
   * into it, so the plain `/tasks` link stays plain. */
  function sortBy(key: SortKey) {
    const direction: SortDirection =
      key === sortKey && sortDirection === "asc" ? "desc" : "asc";
    const next = new URLSearchParams(params);
    if (key === DEFAULT_SORT && direction === "asc") {
      next.delete("sort");
      next.delete("dir");
    } else {
      next.set("sort", key);
      next.set("dir", direction);
    }
    setParams(next);
  }


  const anyFilter = Boolean(kind || sector || capability) || status !== "all";
  const matched: TaskDto[] = useMemo(
    () =>
      sortTasks(
        (tasks.data?.items ?? [])
          .filter((task) => (kind ? task.kind === kind : true))
          .filter((task) =>
            sector ? task.capabilities.some((c) => sectorOf(c) === sector) : true,
          ),
        sortKey,
        sortDirection,
      ),
    [tasks.data, kind, sector, sortKey, sortDirection],
  );
  const visible = matched.slice(page * PAGE_SIZE, (page + 1) * PAGE_SIZE);

  /** Display names for the posters on **this page**, resolved in one
   * request. Not for the whole filtered board: `/names` caps a lookup at
   * 64 keys. A poster whose name hasn't landed yet renders as a truncated
   * key. */
  const posterKeys = visible.map((task) => task.poster).join(",");
  const names = useAsync(
    () => getNames(posterKeys ? posterKeys.split(",") : []),
    [posterKeys],
  );

  const blurb = kind ? describeKind(kind) : null;

  return (
    <Shell>
      <h1>{kind ? `${formatVerification(kind)} tasks` : "tasks"}</h1>
      {blurb && (
        <p className="itx-page-lede">
          {blurb}{" "}
          <span className="flat">
            the protocol calls this kind <code className="itx-key">{kind}</code>.
          </span>
        </p>
      )}

      <div className="itx-filters">
        <SelectField
          value={kind}
          onChange={(value) => update("kind", value)}
          label="Filter by verification"
          options={KINDS.map((k) => ({
            value: k,
            label: k ? `${formatVerification(k)} (${formatKind(k)})` : "any verification",
          }))}
        />

        <SelectField
          value={status}
          onChange={(value) => update("status", value)}
          label="Filter by status"
          options={STATUSES.map((s) => ({
            value: s,
            label: s === "all" ? "any status" : formatStatus(s),
          }))}
        />

        <ComboFilter
          value={sector}
          options={sectorOptions}
          placeholder="sector"
          label="Filter by sector"
          onChange={(value) => update("sector", value)}
        />

        <ComboFilter
          value={capability}
          options={marketOptions}
          placeholder="market (capability tag)"
          label="Filter by market"
          renderOption={marketLabel}
          onChange={(value) => update("capability", value)}
        />

        {anyFilter && (
          <button
            type="button"
            className="itx-button itx-button-ghost"
            // Clears the filters and leaves the ordering alone. Sorting
            // is not a filter -- it hides nothing -- and resetting it
            // here would undo a choice the button doesn't mention.
            onClick={() => {
              const kept = new URLSearchParams();
              const sort = params.get("sort");
              const dir = params.get("dir");
              if (sort) kept.set("sort", sort);
              if (dir) kept.set("dir", dir);
              setParams(kept);
            }}
          >
            clear filters
          </button>
        )}
      </div>

      {/* The one thing the four controls above cannot say about
          themselves. "Is `disputable` a status?" is the obvious reading
          of a board that offers both, and the answer -- no, they are
          different axes and every task has a value on each -- costs one
          line to give and is otherwise only discoverable by experiment. */}
      <p className="itx-filter-note">
        verification is how a task gets judged correct; status is where it has reached in that
        process. every task has one of each. sector groups related markets; a market is the
        capability tag a task is posted under.
      </p>

      <section className="itx-panel">
        {tasks.loading && <Loading what="tasks" />}
        {tasks.error && <ErrorNote error={tasks.error} />}
        {tasks.data && matched.length === 0 && <Empty>no tasks match these filters.</Empty>}
        {tasks.data && matched.length > 0 && (
          <>
            <div className="itx-table-scroll">
              <table className="itx-table">
                <thead>
                  <tr>
                    {COLUMNS.map((column) => (
                      <SortHeader
                        key={column.key}
                        column={column.key}
                        label={column.label}
                        right={column.right}
                        sortKey={sortKey}
                        direction={sortDirection}
                        onSort={sortBy}
                      />
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {visible.map((task) => (
                    <tr key={task.id}>
                      <td className="grow">
                        <Link to={`/tasks/${task.id}`}>{lowerFirst(task.description)}</Link>
                      </td>
                      {/* The protocol name rides along in the tooltip: the
                        column reads in plain language, and anyone
                        checking it against the API can still recover
                        which kind this is without leaving the row. */}
                      <td className="itx-kind" title={formatKind(task.kind)}>
                        {formatVerification(task.kind)}
                      </td>
                      <td>
                        <StatusBadge status={task.status} />
                      </td>
                      {/* Named where the hub has named them. A column of
                        truncated keys is unreadable and unmemorable --
                        `02c545…8a5a` and `02c5a4…8a5a` are the same
                        thing at a glance -- and the name is the hub's
                        own label for the agent, already shown on the
                        leaderboard and the board. The key stays on the
                        row underneath it, because the key is the
                        identity. */}
                      <td>
                        <AgentLink
                          pubkey={task.poster}
                          name={names.data?.get(task.poster) ?? null}
                        />
                      </td>
                      <td className="itx-kind">{sectorsOf(task).join(", ") || "—"}</td>
                      {/* Market labels drop the sector prefix, same as the
                        board does -- `coding/python` reads `python`
                        beside a Sector cell already saying `coding`. The
                        full tag stays in the tooltip, since the full tag
                        is what the hub filters on. */}
                      <td className="itx-kind" title={task.capabilities.join(", ")}>
                        {task.capabilities.map(marketLabel).join(", ") || "—"}
                      </td>
                      <td className="right num">{formatItx(task.bounty)}</td>
                      <td className="right num flat">{formatRelative(task.created_at)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            <Pager page={page} pageSize={PAGE_SIZE} total={matched.length} onPageChange={setPage} />
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

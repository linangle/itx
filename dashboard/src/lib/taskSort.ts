// How the task list orders itself.
//
// Kept out of the page for the usual reason the rest of `src/lib/` is:
// this is a pile of comparators with edge cases worth testing, and
// testing them through a rendered table would be testing React. Nothing
// here imports React.

import type { TaskDto, TaskStatus } from "./hub";
import { formatVerification } from "./format";
import { sectorOf } from "./sectors";

/** Every column the table can sort by. The strings appear in the URL
 * (`/tasks?sort=bounty&dir=desc`), so they are part of a link someone
 * may have kept -- rename one and the old link silently falls back to
 * the default order. */
export type SortKey = "task" | "kind" | "status" | "poster" | "sector" | "market" | "bounty" | "age";

export type SortDirection = "asc" | "desc";

export const DEFAULT_SORT: SortKey = "age";
export const DEFAULT_DIRECTION: SortDirection = "asc";

const SORT_KEYS: SortKey[] = [
  "task",
  "kind",
  "status",
  "poster",
  "sector",
  "market",
  "bounty",
  "age",
];

/** A `?sort=` value the table understands, or the default. A stale or
 * hand-typed link should show the board, not an error. */
export function parseSortKey(raw: string | null): SortKey {
  return SORT_KEYS.includes(raw as SortKey) ? (raw as SortKey) : DEFAULT_SORT;
}

export function parseSortDirection(raw: string | null): SortDirection {
  return raw === "desc" ? "desc" : DEFAULT_DIRECTION;
}

/** Lifecycle order, from `TaskStatus` in `hub/src/board.rs`.
 *
 * Statuses sort by where they sit in a task's life, not by their names:
 * alphabetically `Verified` precedes `Claimed`, which orders the column
 * by an accident of spelling and tells a reader nothing. This way
 * sorting by status groups the board into everything claimable, then
 * everything in flight, then everything finished. */
const STATUS_ORDER: Record<TaskStatus, number> = {
  Open: 0,
  Claimed: 1,
  AwaitingDispute: 2,
  Disputed: 3,
  Verified: 4,
  Paid: 5,
  Closed: 6,
};

/** The first sector a task trades in, for ordering purposes. A task with
 * two tags in different sectors has to sort somewhere, and its first is
 * the one the Sector cell leads with. */
function firstSector(task: TaskDto): string {
  return task.capabilities.length > 0 ? sectorOf(task.capabilities[0]) : "";
}

function firstMarket(task: TaskDto): string {
  return task.capabilities[0] ?? "";
}

/** What each column compares. Strings compare with `localeCompare` so
 * accented tags and mixed case sort the way a reader expects rather than
 * by code point. */
const COMPARATORS: Record<SortKey, (a: TaskDto, b: TaskDto) => number> = {
  task: (a, b) => a.description.localeCompare(b.description),
  kind: (a, b) => formatVerification(a.kind).localeCompare(formatVerification(b.kind)),
  status: (a, b) => STATUS_ORDER[a.status] - STATUS_ORDER[b.status],
  // By key, not by name: names are resolved a page at a time (the hub
  // caps a lookup at 64), so the board has no name for most posters at
  // the moment it sorts. Grouping a poster's tasks together is what this
  // column is for and the key does that exactly.
  poster: (a, b) => a.poster.localeCompare(b.poster),
  sector: (a, b) => firstSector(a).localeCompare(firstSector(b)),
  market: (a, b) => firstMarket(a).localeCompare(firstMarket(b)),
  bounty: (a, b) => a.bounty - b.bounty,
  // Ascending *age* is descending `created_at`: the youngest task is the
  // one created most recently. The column shows an age, so it sorts by
  // one -- a header that says "Age ▲" and lists the oldest first would
  // be sorting by the field behind the column rather than the column.
  age: (a, b) => b.created_at.localeCompare(a.created_at),
};

/** Whether a task has nothing to show in this column. Untagged tasks
 * sort to the bottom in **both** directions rather than flipping to the
 * top on a reverse: a screenful of em dashes is never the answer someone
 * clicking a column header is looking for. */
function isBlank(task: TaskDto, key: SortKey): boolean {
  return (key === "sector" || key === "market") && task.capabilities.length === 0;
}

/** A stable, filtered-then-sorted copy. Never sorts in place -- the
 * array handed in is the fetched board, which other views read. */
export function sortTasks(
  tasks: readonly TaskDto[],
  key: SortKey,
  direction: SortDirection,
): TaskDto[] {
  const compare = COMPARATORS[key];
  const sign = direction === "desc" ? -1 : 1;
  return [...tasks].sort((a, b) => {
    const blankA = isBlank(a, key);
    const blankB = isBlank(b, key);
    if (blankA !== blankB) return blankA ? 1 : -1;
    const primary = compare(a, b) * sign;
    // Ties broken by recency, then by id. Without a tie-break, two tasks
    // with the same bounty can swap places between renders (`Array.sort`
    // is stable, but the list it is given is rebuilt on every poll), and
    // a row that moves while being clicked is a row you click by
    // mistake.
    if (primary !== 0) return primary;
    const byAge = b.created_at.localeCompare(a.created_at);
    return byAge !== 0 ? byAge : a.id.localeCompare(b.id);
  });
}

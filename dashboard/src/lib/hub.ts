// The canonical hub client for the v1 site.
//
// This is a deliberate sibling of `src/api.ts` rather than a replacement
// for it: the three original pages still import `api.ts` and still work
// untouched, so nothing here can break them. The two overlap in their
// type definitions for now; when the owner accepts this direction,
// `api.ts` and the pages using it can be deleted in one commit and this
// becomes the only client.
//
// Nothing in `src/lib/` may import React. It is plain TypeScript against
// `fetch`, which is what lets it be lifted into a package shared with a
// future mobile app without touching a line.
//
// Types mirror `hub/src/handlers.rs`'s DTOs and `hub/src/board.rs`'s
// enums field for field, including exact wire casing -- read from the
// Rust source, not guessed:
//   - `TaskStatus` is plain PascalCase (no `#[serde(rename_all)]`).
//   - `CloseReason` / `DisputeResolution` are `snake_case`.
//   - `TaskKindDto` is `#[serde(tag = "kind", rename_all = "snake_case")]`
//     and flattened into `TaskDto`, so kind-specific fields sit at the
//     top level alongside `kind` itself, not nested under it.

export type TaskStatus =
  | "Open"
  | "Claimed"
  | "AwaitingDispute"
  | "Disputed"
  | "Verified"
  | "Paid"
  | "Closed";

export type TaskKind = "hash_match" | "consensus" | "disputable";

export type CloseReason = "no_majority" | "understaffed" | "cancelled_by_operator";

export type DisputeResolution = "challenger_wins" | "assignee_wins";

export interface DisputeDto {
  challenger: string;
  reason: string;
  bond_amount: number;
  filed_at: string;
  resolution: DisputeResolution | null;
}

interface TaskCommon {
  id: string;
  description: string;
  bounty: number;
  status: TaskStatus;
  poster: string;
  claimant: string | null;
  failed_attempts: number;
  min_reputation: number;
  close_reason: CloseReason | null;
  capabilities: string[];
  /** RFC3339. The only timestamp the hub exposes for a task, and so the
   * basis of every time series on this site. Creation time only -- see
   * `series.ts` for what that does and doesn't let us claim. */
  created_at: string;
}

export type TaskDto = TaskCommon &
  (
    | { kind: "hash_match" }
    | {
        kind: "consensus";
        num_assignees: number;
        assignees_joined: number;
        join_deadline: string;
        submission_deadline: string | null;
      }
    | {
        kind: "disputable";
        answer: string | null;
        dispute_deadline: string | null;
        dispute: DisputeDto | null;
      }
  );

export interface ReputationDto {
  completed: number;
  failed: number;
  total_earned: number;
  /** Current confirmed on-chain balance -- distinct from `total_earned`,
   * which is lifetime cumulative payout and never decreases even after
   * the agent spends it. `null` if the hub couldn't reach the node for
   * this pubkey when the request was made, which is a normal state to
   * render, not an error. */
  net_worth: number | null;
}

export type LeaderboardEntryDto = ReputationDto & { pubkey: string };

const DEFAULT_HUB_URL = "http://127.0.0.1:9100";

export function hubUrl(): string {
  return import.meta.env.VITE_HUB_URL || DEFAULT_HUB_URL;
}

export class HubRequestError extends Error {
  path: string;
  status: number;

  constructor(path: string, status: number) {
    super(`${path} -> HTTP ${status}`);
    this.name = "HubRequestError";
    this.path = path;
    this.status = status;
  }
}

/** A page of results plus the unpaginated total, read from the hub's
 * `X-Total-Count` header. `total` falls back to the page length when the
 * header is missing or unreadable -- which happens against an older hub,
 * or when a misconfigured CORS layer withholds it from JavaScript. Better
 * to render a slightly wrong count than to crash the page over it. */
export interface Page<T> {
  items: T[];
  total: number;
}

async function get<T>(path: string): Promise<{ body: T; response: Response }> {
  const response = await fetch(`${hubUrl()}${path}`);
  if (!response.ok) {
    throw new HubRequestError(path, response.status);
  }
  return { body: (await response.json()) as T, response };
}

async function getJson<T>(path: string): Promise<T> {
  return (await get<T>(path)).body;
}

function query(params: Record<string, string | number | undefined>): string {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined && value !== "") {
      search.set(key, String(value));
    }
  }
  const encoded = search.toString();
  return encoded ? `?${encoded}` : "";
}

export interface ListTasksParams {
  offset?: number;
  limit?: number;
  capability?: string;
  /** Absent means the hub's long-standing default: `Open` tasks only.
   * `"all"` returns every status. See `ListTasksQuery` in
   * `hub/src/handlers.rs`. */
  status?: TaskStatus | "all";
}

export async function listTasks(params: ListTasksParams = {}): Promise<Page<TaskDto>> {
  const path = `/tasks${query({
    offset: params.offset,
    limit: params.limit,
    capability: params.capability,
    status: params.status,
  })}`;
  const { body, response } = await get<TaskDto[]>(path);
  const header = response.headers.get("x-total-count");
  const parsed = header === null ? Number.NaN : Number(header);
  return {
    items: body,
    total: Number.isFinite(parsed) ? parsed : body.length,
  };
}

/** The hub caps one page at `MAX_TASKS_PAGE_SIZE` (200, see
 * `hub/src/handlers.rs`), but the overview's headline figures -- total
 * settled value, open bounty -- are wrong if they only count the most
 * recent page. This walks pages until the board is exhausted.
 *
 * `maxItems` is a safety stop, not a target: it bounds how much a single
 * page load can pull if the board ever grows large, at which point the
 * honest fix is a server-side aggregate endpoint rather than a bigger
 * number here. `complete` reports whether we actually saw everything, so
 * the UI can say so instead of quietly presenting a partial total as a
 * whole one.
 *
 * It was raised to 4000 so a board of a couple of thousand tasks totals
 * honestly rather than silently summing its most recent slice. That is a
 * stopgap and it does not scale: at this size a page load is already
 * pulling a megabyte or so of JSON, and every client recomputes
 * aggregates the hub could have summed once. The server-side endpoint is
 * the actual answer, not the next bump of this number. */
export async function listAllTasks(
  params: Omit<ListTasksParams, "offset" | "limit"> = {},
  maxItems = 7000,
): Promise<Page<TaskDto> & { complete: boolean }> {
  const pageSize = 200;
  const items: TaskDto[] = [];
  let total = 0;

  for (let offset = 0; offset < maxItems; offset += pageSize) {
    const page = await listTasks({ ...params, offset, limit: pageSize });
    items.push(...page.items);
    total = page.total;
    if (page.items.length < pageSize || items.length >= total) break;
  }

  return { items, total, complete: items.length >= total };
}

/** The newest `count` tasks, for the ticker that now rides on every
 * page.
 *
 * Two small requests rather than `listAllTasks`. The tape needs a
 * dozen headlines; walking the whole board for them would pull a
 * megabyte of JSON on every page load and every poll, which is a real
 * cost to pay for a decoration.
 *
 * The hub lists tasks **oldest-first** by `created_at` (see
 * `list_tasks` in `hub/src/handlers.rs`), so the newest are the tail:
 * ask for the total with a one-item request, then take the last page.
 * The count is read from `X-Total-Count`, which is the same header the
 * paginated views already rely on. */
export async function listLatestTasks(
  count = 14,
  params: Omit<ListTasksParams, "offset" | "limit"> = { status: "all" },
): Promise<TaskDto[]> {
  const probe = await listTasks({ ...params, limit: 1 });
  if (probe.total <= count) {
    return (await listTasks({ ...params, limit: count })).items;
  }
  return (await listTasks({ ...params, offset: probe.total - count, limit: count })).items;
}

export function getTask(id: string): Promise<TaskDto> {
  return getJson<TaskDto>(`/tasks/${encodeURIComponent(id)}`);
}

export function getReputation(pubkey: string): Promise<ReputationDto> {
  return getJson<ReputationDto>(`/reputation/${encodeURIComponent(pubkey)}`);
}

export function getLeaderboard(): Promise<LeaderboardEntryDto[]> {
  return getJson<LeaderboardEntryDto[]>("/leaderboard");
}

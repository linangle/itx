// The canonical hub client for the v1 site.
//
// A deliberate sibling of `src/api.ts` rather than a replacement: the
// three original pages still import `api.ts` and still work untouched.
//
// Nothing in `src/lib/` may import React. It is plain TypeScript against
// `fetch`, which is what lets it be lifted into a package shared with a
// future mobile app.
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
  // Every payout is on the wire and the hub is waiting for chain
  // evidence. An ordinary task passes through this on its way to `Paid`,
  // so it is not an edge case -- leaving it out of this union is what let
  // `NewsTicker` render a task the hub routinely serves.
  | "Submitted"
  // Terminal: the payout was proven never to have landed, and the money
  // is still owed. An operator has to resolve it by hand.
  | "PayoutFailed"
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
  /** RFC3339, creation time. Was for a long while the only timestamp the
   * hub exposed, which is why `series.ts` is careful about what a series
   * built on it may claim. */
  created_at: string;
  /** RFC3339, or `null` — when this task's last payout confirmed on
   * chain. `null` while it is unpaid, and `null` forever for a task that
   * closed or failed without paying anyone.
   *
   * The other end of `created_at`, and the reason the board can now say
   * whether posted work is actually being finished rather than only that
   * it was advertised. */
  settled_at: string | null;
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
   * which is lifetime cumulative payout and never decreases. `null` if
   * the hub couldn't reach the node, which is a normal state to render,
   * not an error. */
  net_worth: number | null;
  /** The hub's display name for this agent -- a descriptor and a subject
   * in CamelCase, capped at 15 characters. Assigned by `hub/src/names.rs`,
   * unique, and stable for the agent's lifetime.
   *
   * A **label, not an identity**: the pubkey is the only thing that
   * identifies an agent, and nothing should key off, link by, or compare
   * names. `null` for a pubkey the hub has never seen, so every consumer
   * needs a pubkey fallback -- see `AgentLink`. */
  name: string | null;
}

export type LeaderboardEntryDto = ReputationDto & {
  pubkey: string;
  /** Standing in the whole field, one-based, computed by the hub before
   * it filtered or sliced -- not derivable from the row's position once a
   * search is involved. */
  rank: number;
};

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
 * header is missing or unreadable, which happens against an older hub or
 * a CORS layer that withholds it from JavaScript. */
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

/** How many page requests `listAllTasks` keeps in the air at once. Six is
 * what a browser will actually run in parallel to one HTTP/1.1 origin
 * anyway. */
const MAX_PARALLEL_PAGES = 6;

/** The hub caps one page at `MAX_TASKS_PAGE_SIZE` (200), but the
 * overview's headline figures -- total settled value, open bounty -- are
 * wrong if they only count the most recent page. This walks pages until
 * the board is exhausted.
 *
 * The first page is fetched alone for its `X-Total-Count`; every
 * remaining page is known from that answer and fetched concurrently,
 * `MAX_PARALLEL_PAGES` at a time, so the walk is two sequential rounds
 * rather than one round trip per page. Pages land in `pages[i]` by
 * position, so the assembled list keeps the hub's oldest-first ordering
 * whatever order the responses came back in.
 *
 * `maxItems` is a safety stop, not a target, and `complete` reports
 * whether we actually saw everything so the UI can say so rather than
 * presenting a partial total as a whole one. Every headline figure here
 * is a sum over the whole task list, and the hub sorts ascending on
 * `created_at`, so a truncated walk drops the **newest** tasks -- the
 * half of a live market anyone is looking at. The fix when a real board
 * outgrows this is the aggregate endpoint, not a bigger number.
 *
 * 24000 matches the fixture's backfill (`dashboard/mock/hub.mjs` seeds
 * 20000 and caps growth at 22000); the two have to move together. */
export async function listAllTasks(
  params: Omit<ListTasksParams, "offset" | "limit"> = {},
  maxItems = 24_000,
): Promise<Page<TaskDto> & { complete: boolean }> {
  const pageSize = 200;
  const first = await listTasks({ ...params, limit: pageSize });
  const total = first.total;

  const offsets: number[] = [];
  for (let offset = pageSize; offset < Math.min(total, maxItems); offset += pageSize) {
    offsets.push(offset);
  }

  const pages = new Array<TaskDto[]>(offsets.length);
  let cursor = 0;
  const worker = async () => {
    // Single-threaded, and the increment happens before the first await,
    // so two workers can never take the same index.
    while (cursor < offsets.length) {
      const index = cursor++;
      pages[index] = (await listTasks({ ...params, offset: offsets[index], limit: pageSize })).items;
    }
  };
  await Promise.all(
    Array.from({ length: Math.min(MAX_PARALLEL_PAGES, offsets.length) }, worker),
  );

  const items = first.items.concat(...pages);
  return { items, total, complete: items.length >= total };
}

/** The newest `count` tasks, for the ticker that rides on every page.
 *
 * Two small requests rather than `listAllTasks`: the tape needs a dozen
 * headlines, and walking the whole board for them would pull a megabyte
 * of JSON on every page load and every poll.
 *
 * The hub lists tasks **oldest-first** by `created_at`, so the newest are
 * the tail: ask for the total with a one-item request, then take the last
 * page. */
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

/** How many agents a leaderboard page holds. Matches the hub's own
 * ceiling -- asking for more returns fifty anyway. */
export const LEADERBOARD_PAGE_SIZE = 50;

/** Which column ranks the standings, and which way.
 *
 * Every column the table shows is ranked by the hub. `earned`,
 * `completed` and `failed` are in the reputation map it holds, so ranking
 * by one costs it a sort. `net_worth` is the odd one: a live balance the
 * node answers for, one lookup per agent, which the hub prices in one
 * bounded, briefly cached sweep (`handlers::net_worth_snapshot`) -- which
 * is what lets this be a real ranking rather than a reorder of the fifty
 * rows in hand.
 */
export type LeaderboardSortKey = "earned" | "completed" | "failed" | "net_worth";

export interface LeaderboardSort {
  key: LeaderboardSortKey;
  direction: "asc" | "desc";
}

/** A page of the standings, plus the size of the whole field.
 *
 * Paged because the hub serves it that way: fifty is the whole board on a
 * small hub and the first page of it on a real one, and the difference
 * has to be visible or the ranking quietly stops at fiftieth place.
 *
 * `q` searches by name or pubkey **across the whole field**, server-side.
 * Filtering the fifty rows in hand searches a page and presents it as a
 * board. With the hub doing it, `total` is the number of matches and each
 * entry's `rank` is still its standing among all agents. */
export async function getLeaderboard(
  offset = 0,
  limit = LEADERBOARD_PAGE_SIZE,
  q?: string,
  sort?: LeaderboardSort,
): Promise<Page<LeaderboardEntryDto>> {
  const { body, response } = await get<LeaderboardEntryDto[]>(
    `/leaderboard${query({
      offset: offset || undefined,
      limit,
      q: q?.trim(),
      // Omitted at the default rather than sent, so the common request
      // is the same URL it has always been -- and so a hub that predates
      // the parameter behaves identically.
      sort: sort && sort.key !== "earned" ? sort.key : undefined,
      dir: sort?.direction === "asc" ? "asc" : undefined,
    })}`,
  );
  const header = response.headers.get("x-total-count");
  const parsed = header === null ? Number.NaN : Number(header);
  return {
    items: body,
    total: Number.isFinite(parsed) ? parsed : body.length,
  };
}

// ------------------------------------------------------- board summary
//
// The answer to the page walk above. Mirrors `BoardSummaryDto` in
// `hub/src/handlers.rs`; field names are the wire's snake_case, kept
// as-is rather than camelised so this reads against the Rust it mirrors.

export interface CapabilitySummaryDto {
  capability: string;
  open: number;
  open_bounty: number;
  posted: number;
  /** Tasks posted per bucket, oldest first. */
  posted_series: number[];
  /** Bounty posted per bucket, oldest first. */
  bounty_series: number[];
}

export interface KindSummaryDto {
  kind: TaskKind;
  open: number;
  open_bounty: number;
  posted: number;
  posted_series: number[];
}

export interface BoardSummaryDto {
  /** RFC3339 creation time of the board's oldest task, or `null` on an
   * empty board — the board's *age*, which decides how far back it is
   * meaningful to chart. `window_ms` cannot answer that: it is a preset
   * rounded up from the age. */
  first_task_at: string | null;
  /** How far back every series reaches from the moment of the request. */
  window_ms: number;
  buckets: number;
  /** Tasks the summary covers -- the whole board, by construction. */
  total_tasks: number;
  totals: {
    open_tasks: number;
    open_bounty: number;
    paid_tasks: number;
    paid_bounty: number;
    posted_series: number[];
  };
  kinds: KindSummaryDto[];
  capabilities: CapabilitySummaryDto[];
}

/** Every aggregate the board renders, in one request.
 *
 * The hub does the O(tasks) pass once, over data already in memory, and
 * sends back a few kilobytes of buckets. What it deliberately does not
 * send is percentages or groupings: change is period-over-period with a
 * "too thin to report" rule, and sectors are this site's reading of the
 * tag list -- presentation decisions that live in `series.ts` and
 * `sectors.ts` and would be frozen into the protocol if the hub made
 * them.
 *
 * Against a hub too old to have the route this 404s, which callers should
 * treat as "fall back to `listAllTasks`" -- see `LandingPage`. */
export function getBoardSummary(): Promise<BoardSummaryDto> {
  return getJson<BoardSummaryDto>("/board/summary");
}

/** One market's history, at a window and resolution the caller chooses.
 *
 * Mirrors `MarketSeriesDto` in `hub/src/handlers.rs`. It exists because
 * `/board/summary` structurally cannot serve it: the summary's window is
 * derived from the board's age and its resolution is fixed at 24 buckets,
 * so a chart with range tabs has no way to ask. */
export interface MarketSeriesDto {
  /** Echoed back, so a response arriving after the reader has clicked
   * another market can be recognised as stale. */
  capability: string | null;
  window_ms: number;
  buckets: number;
  /** Epoch millis of the first bucket's left edge and the last one's
   * right edge. The axis is labelled from these rather than from "now
   * minus the window" computed locally — that would use the client's
   * clock, not the clock that bucketed the data. */
  start_ms: number;
  end_ms: number;
  posted_series: number[];
  bounty_series: number[];
  /** Tasks whose last payout confirmed in this bucket, and the bounty
   * that payout moved — bucketed by when the task *settled*, not by when
   * it was posted. The two series deliberately disagree about which
   * bucket a task belongs in: one is demand arriving, the other is work
   * finishing. */
  settled_series: number[];
  paid_bounty_series: number[];
  /** Distinct agents who posted or were paid in each bucket.
   *
   * Distinct *per bucket*, so these do not sum to `agents` — an agent
   * working every day counts once in each bucket and once overall.
   * Summing them would report that agent thirty times. */
  agents_series: number[];
  /** Chain fees the hub paid to settle, one per payout leg. */
  fees_series: number[];
  /** Faucet grants per bucket. **Board-wide**: identical whatever
   * `capability` was asked for, because the faucet issues against a key
   * rather than against a kind of work. */
  faucet_series: number[];
  posted: number;
  bounty: number;
  settled: number;
  paid_bounty: number;
  agents: number;
  fees: number;
  faucet_grants: number;
  /** What those grants issued, in base units. Computed by the hub so a
   * second copy of the grant size cannot go stale here. */
  faucet_itx: number;
  /** Open **right now** — a fact about the present, not about the
   * window. A task posted before the window and still unclaimed is
   * still on offer. */
  open: number;
  open_bounty: number;
  /** When this market first traded, RFC3339, or `null` for a tag that
   * has never appeared on a task. What the chart sizes its range tabs
   * from. */
  first_task_at: string | null;
}

export interface MarketSeriesParams {
  /** Omit to chart the whole board rather than one market. */
  capability?: string;
  /** Omit to let the hub pick from the same preset ladder the summary
   * uses, sized to *this market's* age rather than the board's. */
  windowMs?: number;
  buckets?: number;
}

export function getMarketSeries(params: MarketSeriesParams = {}): Promise<MarketSeriesDto> {
  return getJson<MarketSeriesDto>(
    `/board/series${query({
      capability: params.capability,
      window_ms: params.windowMs,
      buckets: params.buckets,
    })}`,
  );
}

/** How many keys one request asks about. Matches the hub's own ceiling
 * (`MAX_NAMES_LOOKUP`); asking for more would silently drop the tail. */
const MAX_NAMES_PER_REQUEST = 64;

/** Display names for a set of pubkeys, in one request.
 *
 * The leaderboard only carries agents that have *earned*, so anyone who
 * had posted work without being paid yet showed as a bare key. This
 * resolves any well-formed pubkey the registry knows, and answers `null`
 * for one it does not -- a normal state rather than an error.
 *
 * Duplicates are collapsed before the request goes out, and an empty set
 * short-circuits rather than asking the hub about nothing. */
export async function getNames(pubkeys: string[]): Promise<Map<string, string | null>> {
  const unique = [...new Set(pubkeys.filter(Boolean))].slice(0, MAX_NAMES_PER_REQUEST);
  if (unique.length === 0) return new Map();
  const body = await getJson<Record<string, string | null>>(
    `/names${query({ pubkeys: unique.join(",") })}`,
  );
  return new Map(Object.entries(body));
}

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
  /** The hub's display name for this agent -- a descriptor and a subject
   * in CamelCase, capped at 15 characters: `SwiftWarlock`, `AmberOtter`.
   * Assigned by the hub (`hub/src/names.rs`), unique across agents, and
   * stable for the agent's lifetime.
   *
   * A **label, not an identity**: the pubkey is still the only thing
   * that identifies an agent, and nothing here should ever key off, link
   * by, or compare names. Render it beside the pubkey, never instead of
   * it in a context where the distinction matters.
   *
   * `null` for a pubkey the hub has never seen do anything -- the agent
   * page resolves any key, and most keys are strangers. Every consumer
   * needs a pubkey fallback; see `AgentLink`. */
  name: string | null;
}

export type LeaderboardEntryDto = ReputationDto & {
  pubkey: string;
  /** Standing in the whole field, one-based, computed by the hub before
   * it filtered or sliced. Not derivable from the row's position once a
   * search is involved -- see `LeaderboardEntryDto::rank` in
   * `hub/src/handlers.rs`. */
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

/** How many page requests `listAllTasks` keeps in the air at once. Six
 * is what a browser will actually run in parallel to one HTTP/1.1
 * origin anyway; asking for more queues in the browser instead of here,
 * and hitting the hub harder than its own transport allows buys
 * nothing. */
const MAX_PARALLEL_PAGES = 6;

/** The hub caps one page at `MAX_TASKS_PAGE_SIZE` (200, see
 * `hub/src/handlers.rs`), but the overview's headline figures -- total
 * settled value, open bounty -- are wrong if they only count the most
 * recent page. This walks pages until the board is exhausted.
 *
 * The first page is fetched alone for its `X-Total-Count`; every
 * remaining page is known from that answer and fetched concurrently,
 * `MAX_PARALLEL_PAGES` at a time. The previous version walked strictly
 * one page after another, which put 28 sequential round trips in front
 * of a 5600-task board -- off localhost that is 28 x RTT before the
 * page has its data, and it was the single largest latency on the
 * landing page (see Round 36 in `docs/web-v1-log.md`). Now it is two
 * sequential rounds: the probe, then the rest in flights of six.
 *
 * Pages land in `pages[i]` by position rather than arrival order, so
 * the assembled list keeps the hub's oldest-first ordering whatever
 * order the responses came back in.
 *
 * `maxItems` is a safety stop, not a target: it bounds how much a single
 * page load can pull if the board ever grows large, at which point the
 * honest fix is a server-side aggregate endpoint rather than a bigger
 * number here. `complete` reports whether we actually saw everything, so
 * the UI can say so instead of quietly presenting a partial total as a
 * whole one. Parallelism cuts the walk's latency, not its weight: every
 * client still downloads and re-derives what the hub could have summed
 * once, and the server-side endpoint remains the actual answer.
 *
 * **What hitting the stop actually costs.** Every headline figure on the
 * board -- open bounty, settled value, a sector's size, a market's
 * sparkline -- is a sum over the whole task list. Stop the walk early
 * and those sums are computed from the tasks that were fetched but
 * rendered as though they were the market, which is the site
 * misreporting its own size rather than merely showing less.
 *
 * And it truncates at the wrong end. The hub sorts ascending on
 * `created_at` (`tasks.sort_by_key` in `list_tasks`) and offsets slice
 * from the front, so the tasks dropped are the **newest** ones -- the
 * half of a live market anyone is actually looking at. A truncated
 * board would show sparklines flattening toward the right edge and a
 * "latest" feed that has quietly stopped being latest. Which is the
 * other reason this is a stop and not a policy: the fix when a real
 * board outgrows it is the aggregate endpoint, not walking from the
 * other end and inheriting the mirror-image problem.
 *
 * Raised to 24000 with the fixture's backfill (`dashboard/mock/hub.mjs`
 * seeds 20000 and caps its live growth at 22000); the two move
 * together, since a limit under the board's size means every figure on
 * the page is wrong by an unknown amount. */
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

/** How many agents a leaderboard page holds. Matches the hub's own
 * ceiling -- asking for more returns fifty anyway. */
export const LEADERBOARD_PAGE_SIZE = 50;

/** A page of the standings, plus the size of the whole field.
 *
 * Paged rather than a flat list because the hub now serves it that way:
 * fifty is the whole board on a small hub and the first page of it on a
 * real one, and the difference has to be visible or the ranking quietly
 * stops at fiftieth place. `total` comes from `X-Total-Count`, the same
 * header the task list uses.
 *
 * `q` searches by name or pubkey **across the whole field**, server-side.
 * Both leaderboards used to filter the fifty rows they had in hand,
 * which searches a page and presents it as a board -- the agent you are
 * looking for is on page 31 and no amount of typing will find them.
 * With the hub doing it, `total` is the number of matches and each
 * entry's `rank` is still its standing among all agents. */
export async function getLeaderboard(
  offset = 0,
  limit = LEADERBOARD_PAGE_SIZE,
  q?: string,
): Promise<Page<LeaderboardEntryDto>> {
  const { body, response } = await get<LeaderboardEntryDto[]>(
    `/leaderboard${query({ offset: offset || undefined, limit, q: q?.trim() })}`,
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
   * empty board — the board's *age*, which is what decides how far back
   * it is meaningful to chart. `window_ms` cannot answer that: it is a
   * preset rounded up from the age, so a board eight days old and one
   * twenty-nine days old both report 30D. */
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
 * tag list -- both presentation decisions that live in `series.ts` and
 * `sectors.ts` and would be frozen into the protocol if the hub made
 * them. So this returns raw per-bucket arrays and the client finishes
 * the job in O(buckets).
 *
 * Against a hub too old to have the route this 404s, which callers
 * should treat as "fall back to `listAllTasks`" rather than as an error
 * worth showing anyone -- see `LandingPage`. */
export function getBoardSummary(): Promise<BoardSummaryDto> {
  return getJson<BoardSummaryDto>("/board/summary");
}

/** One market's history, at a window and resolution the caller chooses.
 *
 * Mirrors `MarketSeriesDto` in `hub/src/handlers.rs`. This is the
 * endpoint behind the market chart, and it exists because
 * `/board/summary` structurally cannot serve it: the summary's window is
 * derived from the board's age and its resolution is fixed at 24
 * buckets, so a chart with range tabs — the same market at six spans, at
 * more than a sparkline's resolution — has no way to ask. */
export interface MarketSeriesDto {
  /** Echoed back, so a response arriving after the reader has clicked
   * another market can be recognised as stale. */
  capability: string | null;
  window_ms: number;
  buckets: number;
  /** Epoch millis of the first bucket's left edge and the last one's
   * right edge. The axis is labelled from these rather than from "now
   * minus the window" computed locally — that would use the client's
   * clock, which is not the clock that bucketed the data. */
  start_ms: number;
  end_ms: number;
  posted_series: number[];
  bounty_series: number[];
  posted: number;
  bounty: number;
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
 * The alternative for a list of rows was the leaderboard, which only
 * carries agents that have *earned* -- so anyone who had posted work
 * without being paid for it yet showed as a bare key. This resolves any
 * well-formed pubkey the hub's registry knows, and answers `null` for
 * one it does not, which is a normal state rather than an error: a name
 * is a label the hub assigns, and the pubkey is what identifies an
 * agent.
 *
 * Duplicates are collapsed before the request goes out -- a tape of
 * twenty rows is often a handful of distinct posters -- and an empty set
 * short-circuits rather than asking the hub about nothing. */
export async function getNames(pubkeys: string[]): Promise<Map<string, string | null>> {
  const unique = [...new Set(pubkeys.filter(Boolean))].slice(0, MAX_NAMES_PER_REQUEST);
  if (unique.length === 0) return new Map();
  const body = await getJson<Record<string, string | null>>(
    `/names${query({ pubkeys: unique.join(",") })}`,
  );
  return new Map(Object.entries(body));
}

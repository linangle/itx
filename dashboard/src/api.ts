// Thin client for the hub's read-only, unauthenticated endpoints --
// `GET /tasks`, `GET /tasks/:id`, `GET /leaderboard`, `GET
// /reputation/:pubkey`. These types mirror `hub/src/handlers.rs`'s DTOs
// and `hub/src/board.rs`'s enums field for field, including exact wire
// casing -- read from the Rust source, not guessed:
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
   * the hub couldn't reach the node for this pubkey. */
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

async function getJson<T>(path: string): Promise<T> {
  const resp = await fetch(`${hubUrl()}${path}`);
  if (!resp.ok) {
    throw new HubRequestError(path, resp.status);
  }
  return (await resp.json()) as T;
}

export interface ListTasksParams {
  offset?: number;
  limit?: number;
  capability?: string;
}

export function listTasks(params: ListTasksParams = {}): Promise<TaskDto[]> {
  const query = new URLSearchParams();
  if (params.offset) query.set("offset", String(params.offset));
  if (params.limit) query.set("limit", String(params.limit));
  if (params.capability) query.set("capability", params.capability);
  const qs = query.toString();
  return getJson<TaskDto[]>(`/tasks${qs ? `?${qs}` : ""}`);
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

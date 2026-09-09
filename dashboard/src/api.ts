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

/** Where the hub is — the one resolver, re-exported rather than
 * reimplemented.
 *
 * There were two of these. This file's copy read only `VITE_HUB_URL` and
 * never looked at the `itx-hub-url` meta tag, so the three legacy pages
 * under `/legacy/*` ignored the one knob a deployed site actually has:
 * an operator who edited the shipped `index.html` moved the landing page
 * to their hub and left these three pointed at loopback. Nothing said
 * so, because on the machine that built the site loopback answers.
 *
 * A second copy of a rule is a second place for it to be wrong, and this
 * one had been wrong since the tag was introduced. See `lib/hub.ts` for
 * the resolution order and why the build-time override is dev-only. */
export { hubUrl } from "./lib/hub";
import { hubUrl } from "./lib/hub";

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

import { Link } from "react-router-dom";
import type { TaskStatus } from "../lib/hub";
import { directionOf, formatPct, formatStatus, truncatePubkey } from "../lib/format";

/** Status colour follows what the status *means* for a visitor, not the
 * order of the enum:
 *   - green  = claimable right now
 *   - blue   = work in progress
 *   - amber  = contested, needs attention
 *   - grey   = finished, nothing more to do
 * `Paid` is deliberately grey rather than green: it's a settled result,
 * not an opportunity, and a board full of green "Paid" rows would drown
 * out the handful of rows a visitor can actually act on. */
const STATUS_CLASS: Record<TaskStatus, string> = {
  Open: "itx-badge-open",
  Claimed: "itx-badge-active",
  AwaitingDispute: "itx-badge-warn",
  Disputed: "itx-badge-warn",
  Verified: "itx-badge-active",
  Paid: "itx-badge-done",
  Closed: "itx-badge-done",
};

export function StatusBadge({ status }: { status: TaskStatus }) {
  return (
    <span className={`itx-badge ${STATUS_CLASS[status] ?? "itx-badge-done"}`}>
      {formatStatus(status)}
    </span>
  );
}

/** A signed percentage, coloured by direction. `null` renders as a
 * neutral em dash -- "no basis for comparison" is not the same claim as
 * "no change", and colouring it green or red would invent a trend that
 * the data doesn't support. */
export function Delta({ pct }: { pct: number | null }) {
  return <span className={`num ${directionOf(pct)}`}>{formatPct(pct)}</span>;
}

/** Pubkeys are 66 hex characters. Shown truncated, with the full value in
 * the title attribute so it's still copyable and verifiable on hover. */
export function PubkeyLink({ pubkey }: { pubkey: string }) {
  return (
    <Link className="itx-pubkey" to={`/agents/${pubkey}`} title={pubkey}>
      {truncatePubkey(pubkey)}
    </Link>
  );
}

/** An agent, by name where the hub has assigned one.
 *
 * The name is the label; the pubkey is still the identity, so it stays
 * on the row as dimmed secondary text rather than being replaced. Two
 * agents can have near-identical truncated pubkeys (`02a4f1…9c3b` and
 * `03a4e8…9c3b` differ by two characters at opposite ends) and no
 * reader will ever catch that at a glance -- `SwiftWarlock` next to
 * `AmberOtter` they catch instantly. Keeping both means the row is
 * scannable without becoming unverifiable.
 *
 * Falls back to a bare `PubkeyLink` when `name` is null, which is the
 * normal state for a key the hub has no history for. */
export function AgentLink({ pubkey, name }: { pubkey: string; name: string | null }) {
  if (!name) return <PubkeyLink pubkey={pubkey} />;
  return (
    <Link className="itx-agent" to={`/agents/${pubkey}`} title={pubkey}>
      <span className="itx-agent-name">{name}</span>
      <span className="itx-agent-key">{truncatePubkey(pubkey, 4, 4)}</span>
    </Link>
  );
}

export function PubkeyText({ pubkey }: { pubkey: string }) {
  return (
    <span className="itx-pubkey" title={pubkey}>
      {truncatePubkey(pubkey)}
    </span>
  );
}

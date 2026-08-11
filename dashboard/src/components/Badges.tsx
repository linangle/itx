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
 * When `name` is null -- the normal state for a key the hub has no
 * history for -- the key takes the top line instead, at the usual 6/4
 * truncation, which is exactly what a bare `PubkeyLink` used to render.
 *
 * `meta` is an optional extra fact for the second line (the overview's
 * "3 done"). It shares that line with the key rather than taking a third
 * one, because these rows sit in tables whose height is set in CSS and a
 * third line would push every row past it. With no name and no meta
 * there is no second line at all, so a plain leaderboard row stays one
 * line high. */
export function AgentLink({
  pubkey,
  name,
  meta,
}: {
  pubkey: string;
  name: string | null;
  meta?: string;
}) {
  // The key only moves to the second line when a name has displaced it
  // from the first; without one it is already the headline and must not
  // be repeated underneath.
  const sub = [name ? truncatePubkey(pubkey, 4, 4) : null, meta].filter(Boolean).join(" · ");
  return (
    <Link className="itx-agent" to={`/agents/${pubkey}`} title={pubkey}>
      <span className="itx-agent-name">{name ?? truncatePubkey(pubkey)}</span>
      {sub && <span className="itx-agent-key">{sub}</span>}
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

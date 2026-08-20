import { Link } from "react-router-dom";
import type { TaskStatus } from "../lib/hub";
import { directionOf, formatPct, formatStatus, truncatePubkey } from "../lib/format";
import ProfileIcon from "./ProfileIcon";

/** Status colour follows what the status *means* for a visitor, not the
 * order of the enum:
 *   - green  = claimable right now
 *   - blue   = work in progress
 *   - amber  = contested, needs attention
 *   - grey   = finished, nothing more to do
 * `Paid` is deliberately grey rather than green: it is a settled result,
 * not an opportunity. */
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
 * "no change", and colouring it would invent a trend. */
export function Delta({ pct }: { pct: number | null }) {
  return <span className={`num ${directionOf(pct)}`}>{formatPct(pct)}</span>;
}

/** Pubkeys are 66 hex characters. Shown truncated, with the full value in
 * the title attribute so it stays copyable on hover. The icon beside the
 * key is derived from the key itself, so two truncated keys that read
 * near-identically get visibly different faces. */
export function PubkeyLink({ pubkey }: { pubkey: string }) {
  return (
    <Link className="itx-pubkey" to={`/agents/${pubkey}`} title={pubkey}>
      <ProfileIcon pubkey={pubkey} size={20} className="itx-avatar" />
      {truncatePubkey(pubkey)}
    </Link>
  );
}

/** An agent, by name where the hub has assigned one.
 *
 * The name is the label; the pubkey is still the identity, so it stays on
 * the row as dimmed secondary text rather than being replaced -- two
 * agents can have near-identical truncated pubkeys and no reader will
 * catch that at a glance. When `name` is null the key takes the top line
 * instead, at the usual 6/4 truncation.
 *
 * `meta` is an optional extra fact for the second line, which it shares
 * with the key rather than taking a third: these rows sit in tables whose
 * height is set in CSS. With no name and no meta there is no second line
 * at all. */
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
    // Named agents are hovered by their name: the key is already on the
    // row's second line, so repeating it told the reader something they
    // could already see.
    <Link className="itx-agent" to={`/agents/${pubkey}`} title={name ?? pubkey}>
      <ProfileIcon pubkey={pubkey} size={28} className="itx-avatar" />
      <span className="itx-agent-stack">
        <span className="itx-agent-name">{name ?? truncatePubkey(pubkey)}</span>
        {sub && <span className="itx-agent-key">{sub}</span>}
      </span>
    </Link>
  );
}

export function PubkeyText({ pubkey }: { pubkey: string }) {
  return (
    <span className="itx-pubkey" title={pubkey}>
      <ProfileIcon pubkey={pubkey} size={20} className="itx-avatar" />
      {truncatePubkey(pubkey)}
    </span>
  );
}

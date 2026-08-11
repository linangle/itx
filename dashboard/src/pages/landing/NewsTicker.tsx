import { useMemo, useState } from "react";
import type { AsyncState } from "../../hooks/useAsync";
import type { Page, TaskDto } from "../../lib/hub";
import { formatCompactItx, formatKind, truncatePubkey } from "../../lib/format";

/** What scrolls while the hub is loading, unreachable, or empty -- the
 * reference mock's literal "news news news", which doubles as an honest
 * placeholder rather than fabricated market activity. */
const FILLER = Array.from({ length: 18 }, () => "news");

/** One tape headline per task, phrased from its current status. These
 * are the "important transactions / financial events": money on offer,
 * money moving, and money settling. Lowercase throughout, per the
 * reference mock -- `formatKind` and pubkeys are lowercased at the end
 * rather than at each call site. */
function headline(task: TaskDto): string {
  const itx = `${formatCompactItx(task.bounty)} itx`;
  const kind = formatKind(task.kind).toLowerCase();
  switch (task.status) {
    case "Paid":
      return task.claimant
        ? `settled ${itx} → ${truncatePubkey(task.claimant)}`
        : `settled ${itx} → consensus pool`;
    case "Open":
      return `new ${kind} bounty ${itx}`;
    case "Claimed":
      return task.claimant ? `claimed ${itx} by ${truncatePubkey(task.claimant)}` : `claimed ${itx}`;
    case "AwaitingDispute":
      return `answer posted on ${itx} task`;
    case "Disputed":
      return `dispute filed on ${itx} task`;
    case "Verified":
      return `work verified on ${itx} task`;
    case "Closed":
      return `closed ${kind} task, ${itx} released`;
  }
}

/** The sticky tape across the top of the page: newest market events
 * scrolling right to left over the red->blue->green gradient.
 *
 * The track holds two identical copies of the item list and the CSS
 * animation translates it by exactly -50%; copy two lands where copy
 * one started, so the loop has no visible seam. Duration scales with
 * content length so the scroll speed stays roughly constant whether
 * the tape carries 4 headlines or 14.
 *
 * Dismissing unmounts the tape entirely -- the hero grows into the
 * freed space on its own, since its height is `100svh` minus the tape
 * via a CSS variable that the wrapper drops when closed. */
export default function NewsTicker({
  tasks,
}: {
  /** Shared with the board rather than fetched here: one walk of the
   * task list per poll, not two. */
  tasks: AsyncState<Page<TaskDto> & { complete: boolean }>;
}) {
  const [open, setOpen] = useState(true);

  const items = useMemo(() => {
    const list = (tasks.data?.items ?? [])
      .slice()
      .sort((a, b) => b.created_at.localeCompare(a.created_at))
      .slice(0, 14)
      .map(headline);
    return list.length > 0 ? list : FILLER;
  }, [tasks.data]);

  // Quantised to 4s steps. The duration is an inline style, so any change
  // to it restarts the marquee from the left -- and with the tape now
  // polling, an exact character count would nudge it on almost every
  // refresh. Rounding means only a real change in volume moves it.
  const chars = items.reduce((sum, item) => sum + item.length, 0);
  const duration = Math.round(Math.min(96, Math.max(24, chars * 0.14)) / 4) * 4;

  if (!open) return null;

  const half = (hidden: boolean) => (
    <span className="itx-news-half" aria-hidden={hidden || undefined}>
      {items.map((text, i) => (
        <span className="itx-news-item" key={i}>
          {text}
        </span>
      ))}
    </span>
  );

  return (
    <div className="itx-news" aria-label="Latest market events">
      <div className="itx-news-track" style={{ animationDuration: `${duration}s` }}>
        {half(false)}
        {half(true)}
      </div>
      {/* Sits above the fade so the headlines dissolve *behind* the
       * button rather than colliding with it. */}
      <div className="itx-news-mask" aria-hidden="true" />
      <button
        type="button"
        className="itx-news-close"
        onClick={() => setOpen(false)}
        aria-label="Hide the market ticker"
      >
        <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true">
          <path
            d="M3 3 L13 13 M13 3 L3 13"
            stroke="currentColor"
            strokeWidth="2.1"
            strokeLinecap="round"
          />
        </svg>
      </button>
    </div>
  );
}

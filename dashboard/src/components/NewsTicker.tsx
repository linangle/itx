import { useMemo, useState } from "react";
import type { AsyncState } from "../hooks/useAsync";
import type { TaskDto } from "../lib/hub";
import { formatCompactItx, formatKind, truncatePubkey } from "../lib/format";

/** What scrolls while the hub is loading, unreachable, or empty -- the
 * reference mock's literal "news news news", which doubles as an honest
 * placeholder rather than fabricated market activity. */
const FILLER = Array.from({ length: 18 }, () => "news");

/** Remembers that the tape was dismissed.
 *
 * Outside the component because the bar is mounted per page, so a
 * landing-page tape and an agent-page tape are different instances.
 *
 * `sessionStorage`, not `localStorage`: there is no control anywhere to
 * bring the tape back, so a permanent record would be a one-way door out
 * of a feature. Deliberately different from the theme toggle, which
 * persists because it can be undone from the page. */
const DISMISS_KEY = "itx-news-dismissed";

function dismissed(): boolean {
  return sessionStorage.getItem(DISMISS_KEY) === "1";
}

/** One tape headline per task, phrased from its current status: money on
 * offer, money moving, and money settling. Lowercase throughout --
 * `formatKind` and pubkeys are lowercased at the end rather than at each
 * call site.
 *
 * **This function has to be total, and the `default` below is not
 * defensive padding.** It used to switch over `TaskStatus` with no
 * fallback, and `TaskStatus` was missing `Submitted` and `PayoutFailed`
 * -- two statuses the hub had begun serving. A task in either fell
 * through and returned `undefined`, which `chars` then read `.length`
 * from. There is no error boundary in this app and `NewsTicker` sits
 * inside `SiteBar`, which is on every screen, so one settling task in the
 * newest fourteen threw during render and took the entire site down until
 * it aged out of the window.
 *
 * So the fallback earns its place twice over: the union is maintained by
 * hand against `TaskStatus` in `hub/src/board.rs` and will drift again,
 * and the cost of drifting must be a dull headline rather than a blank
 * site. The `never` binding is the other half -- once the union does
 * learn a new status, this stops compiling until it gets a phrasing. */
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
    case "Submitted":
      return task.claimant
        ? `settling ${itx} → ${truncatePubkey(task.claimant)}`
        : `settling ${itx} → consensus pool`;
    case "PayoutFailed":
      return `payout failed on ${itx} task, still owed`;
    case "Closed":
      return `closed ${kind} task, ${itx} released`;
    default: {
      // `never` while the switch covers the union, so adding a status to
      // `TaskStatus` without phrasing it here fails the build.
      const unphrased: never = task.status;
      // And at runtime, where the hub may already be serving a status
      // this build has never heard of: say the true, dull thing.
      return `${itx} ${kind} task (${String(unphrased)})`.toLowerCase();
    }
  }
}

/** The tape across the top of every page: newest market events scrolling
 * right to left over the red->blue->green gradient.
 *
 * The track holds two identical copies of the item list and the CSS
 * animation translates it by exactly -50%, so the loop has no visible
 * seam. Duration scales with content length so the scroll speed stays
 * roughly constant whether the tape carries 4 headlines or 14.
 *
 * Dismissing unmounts the tape entirely -- the hero grows into the freed
 * space on its own, since its height is `100svh` minus the tape via a CSS
 * variable the wrapper drops when closed. */
export default function NewsTicker({
  tasks,
}: {
  /** Supplied rather than fetched here, so the landing page can hand over
   * the list its board already walked. Typed as loosely as the tape needs
   * -- any state carrying a list of tasks will do. */
  tasks: AsyncState<{ items: TaskDto[] }>;
}) {
  const [open, setOpen] = useState(() => !dismissed());

  function close() {
    sessionStorage.setItem(DISMISS_KEY, "1");
    setOpen(false);
  }

  const items = useMemo(() => {
    const list = (tasks.data?.items ?? [])
      .slice()
      .sort((a, b) => b.created_at.localeCompare(a.created_at))
      .slice(0, 14)
      .map(headline);
    return list.length > 0 ? list : FILLER;
  }, [tasks.data]);

  // Quantised to 4s steps. The duration is an inline style, so any change
  // to it restarts the marquee from the left -- and with the tape polling,
  // an exact character count would nudge it on almost every refresh.
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
        onClick={close}
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

// Display formatting. No React, no DOM -- pure string in, string out, so
// it can be unit-tested directly and reused by a future mobile client.

/** Base units per whole ITX.
 *
 * `btclib`'s `INITIAL_REWARD` is 50 and documented as "multiply by 10^8 to
 * get satoshis" (`lib/src/lib.rs`), so the chain's base unit is 1e-8 of a
 * coin, exactly like Bitcoin. Every amount the hub reports -- bounties,
 * `total_earned`, `net_worth`, `HUB_TRANSACTION_FEE` -- is in these base
 * units. For scale: the faucet grants 50_000_000 base units, which is
 * 0.5 ITX. */
export const UNITS_PER_ITX = 100_000_000;

/** Renders a base-unit amount as ITX.
 *
 * Trailing zeros are trimmed but at least two decimals are kept, so a
 * column of amounts stays visually aligned without padding every row out
 * to the full eight decimal places a base unit technically allows. */
function assemble(itx: number, decimalPlaces: number): string {
  const [whole, raw] = itx.toFixed(decimalPlaces).split(".");
  // Strip every trailing zero first, then pad back up to the two-decimal
  // floor. Doing it in that order is what keeps a small amount readable
  // while still letting a round number sit flush in a column.
  const trimmed = (raw ?? "").replace(/0+$/, "").padEnd(2, "0");
  return `${Number(whole).toLocaleString("en-US")}.${trimmed}`;
}

/** Renders a base-unit amount as ITX for display in a column.
 *
 * Capped at four decimals. A base unit is 1e-8 of a coin, so rendering
 * every amount at full precision produces things like `2.50745433` --
 * technically exact, and completely unscannable stacked forty rows deep.
 * Four decimals is the point where a column still reads as a column.
 *
 * The exception is an amount so small that four decimals would round it
 * to zero. The flat network fee is 1_000 base units, and showing that as
 * `0.00` would make every fee in the UI look like nothing at all -- so
 * anything below the four-decimal floor falls back to full precision
 * rather than lying about being zero. Use `formatItxExact` where the
 * precise figure matters more than the alignment. */
export function formatItx(baseUnits: number): string {
  const itx = baseUnits / UNITS_PER_ITX;
  const rounded = assemble(itx, 4);
  if (baseUnits !== 0 && Number(rounded.replace(/,/g, "")) === 0) {
    return formatItxExact(baseUnits);
  }
  return rounded;
}

/** Full-precision ITX, down to the base unit. For detail views, where
 * there's one number on the screen and being exact costs nothing. */
export function formatItxExact(baseUnits: number): string {
  return assemble(baseUnits / UNITS_PER_ITX, 8);
}

/** Compact form for headline stats where precision matters less than
 * scale: 1.2K, 3.4M. Falls back to the exact figure below 1000. */
export function formatCompactItx(baseUnits: number): string {
  const itx = baseUnits / UNITS_PER_ITX;
  if (Math.abs(itx) >= 1000) {
    return new Intl.NumberFormat("en-US", {
      notation: "compact",
      maximumFractionDigits: 1,
    }).format(itx);
  }
  return formatItx(baseUnits);
}

export function formatCount(n: number): string {
  return n.toLocaleString("en-US");
}

/** A signed percentage, terminal style: `+1.30%`, `-1.65%`, `0.00%`.
 *
 * `null` renders as an em dash -- used wherever a change genuinely can't
 * be computed (no prior period to compare against), which is different
 * from a change that happens to be zero. */
export function formatPct(pct: number | null): string {
  if (pct === null || !Number.isFinite(pct)) return "—";
  const sign = pct > 0 ? "+" : "";
  return `${sign}${pct.toFixed(2)}%`;
}

/** Direction of a change, for colour. `flat` covers both "exactly zero"
 * and "unknown", because neither should read as good or bad news. */
export type Direction = "up" | "down" | "flat";

export function directionOf(value: number | null): Direction {
  if (value === null || !Number.isFinite(value) || value === 0) return "flat";
  return value > 0 ? "up" : "down";
}

/** A pubkey is 66 hex characters (33-byte compressed SEC1), far too long
 * to show in a dense table. Shows enough of both ends to compare two at a
 * glance while staying obviously truncated. */
export function truncatePubkey(pubkey: string, lead = 6, tail = 4): string {
  if (pubkey.length <= lead + tail + 1) return pubkey;
  return `${pubkey.slice(0, lead)}…${pubkey.slice(-tail)}`;
}

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/** Short relative time, past or future: `3m`, `4h`, `2d`, `just now`.
 *
 * Unit-less by design -- the column header says whether it's age or time
 * remaining, so repeating "ago" on every row is noise. */
export function formatRelative(iso: string, now: number = Date.now()): string {
  const then = new Date(iso).getTime();
  if (!Number.isFinite(then)) return "—";
  const delta = Math.abs(now - then);
  if (delta < MINUTE) return "just now";
  if (delta < HOUR) return `${Math.floor(delta / MINUTE)}m`;
  if (delta < DAY) return `${Math.floor(delta / HOUR)}h`;
  return `${Math.floor(delta / DAY)}d`;
}

/** A deadline rendered as time remaining, or as how long ago it lapsed.
 *
 * Separate from `formatRelative` because a deadline's *direction* is the
 * whole point -- "in 2h" and "2h ago" mean opposite things for a task
 * you're deciding whether to claim, and a bare "2h" hides which one it
 * is. `expired` is returned rather than baked into the string so callers
 * can colour it without re-parsing. */
export function formatCountdown(
  iso: string,
  now: number = Date.now(),
): { text: string; expired: boolean } {
  const then = new Date(iso).getTime();
  if (!Number.isFinite(then)) return { text: "—", expired: false };
  const expired = then <= now;
  const delta = formatRelative(iso, now);
  if (delta === "just now") {
    return { text: expired ? "just expired" : "any moment", expired };
  }
  return { text: expired ? `${delta} ago` : `in ${delta}`, expired };
}

/** Absolute timestamp for tooltips and detail views, where the exact
 * moment matters more than brevity. */
export function formatTimestamp(iso: string): string {
  const date = new Date(iso);
  if (!Number.isFinite(date.getTime())) return "—";
  return date.toLocaleString("en-US", {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/* Lowercase at the source, like every other label on the site. The
 * micro-labels that read as small caps (table headers, panel titles,
 * fact terms) get there through `text-transform` in CSS, so authoring
 * these in sentence case only meant every caller having to
 * `.toLowerCase()` them back. */
const KIND_LABELS: Record<string, string> = {
  hash_match: "hash match",
  consensus: "consensus",
  disputable: "disputable",
};

export function formatKind(kind: string): string {
  return KIND_LABELS[kind] ?? kind;
}

/** What the three kinds are called on screen, in the reader's terms
 * rather than the protocol's.
 *
 * `hash_match` / `consensus` / `disputable` are accurate names for what
 * they are in `hub/src/board.rs`, and they say nothing at all to someone
 * who has not read it -- a visitor sees three nav entries and cannot
 * guess what any of them lists. Every one of them answers the same
 * question, *how does this task get judged correct*, so the labels name
 * the judging method: a machine check, a vote, or a challenge window.
 *
 * The protocol name is never dropped, only demoted -- it stays beside
 * the plain label in the filter dropdown and on the task detail page, so
 * anyone reading the API alongside the site can still line the two up.
 * `formatKind` is untouched and still returns the protocol name. */
const KIND_VERIFICATION_LABELS: Record<string, string> = {
  hash_match: "automatic check",
  consensus: "majority vote",
  disputable: "challenge window",
};

export function formatVerification(kind: string): string {
  return KIND_VERIFICATION_LABELS[kind] ?? formatKind(kind);
}

/** One sentence on how a kind settles, for the head of its filtered
 * list. Read from `TaskKind` in `hub/src/board.rs` -- the mechanics
 * described here are the ones the hub actually runs, not a gloss. */
const KIND_BLURBS: Record<string, string> = {
  hash_match:
    "One agent claims the task and submits an answer. The hub hashes it and compares that against a target the poster fixed when posting, so the task settles with no human judgement anywhere in the loop. A wrong answer reopens the task and costs the submitter reputation.",
  consensus:
    "Several agents are assigned the same task and answer independently, without seeing each other's work. Whatever answer the majority converges on is treated as correct, and everyone who agreed with it splits the bounty. There is no money at stake for being wrong — reputation is the stake.",
  disputable:
    "One agent claims the task and submits an answer, which then stands unless someone challenges it inside the dispute window. Filing a challenge means posting a bond, and the operator decides who was right; the loser forfeits. It is the kind used for work no machine can check and no vote can settle.",
};

export function describeKind(kind: string): string | null {
  return KIND_BLURBS[kind] ?? null;
}

const STATUS_LABELS: Record<string, string> = {
  Open: "Open",
  Claimed: "Claimed",
  AwaitingDispute: "Awaiting dispute",
  Disputed: "Disputed",
  Verified: "Verified",
  Paid: "Paid",
  Closed: "Closed",
};

export function formatStatus(status: string): string {
  return STATUS_LABELS[status] ?? status;
}

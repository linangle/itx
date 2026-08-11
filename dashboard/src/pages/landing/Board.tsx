import { useEffect, useMemo, useRef, useState } from "react";
import { Link } from "react-router-dom";
import Sparkline from "../../components/Sparkline";
import { sweepColors } from "./marketHue";
import { useAsync } from "../../hooks/useAsync";
import type { AsyncState } from "../../hooks/useAsync";
import { useFitRows } from "../../hooks/useFitRows";
import { getLeaderboard } from "../../lib/hub";
import type { Page, TaskDto } from "../../lib/hub";
import {
  directionOf,
  formatCompactItx,
  formatCount,
  formatKind,
  formatPct,
  formatRelative,
  truncatePubkey,
} from "../../lib/format";
import {
  agentEarningsSeries,
  chooseWindow,
  periodChangePct,
  sumByCreatedAt,
  summarizeByCapability,
  summarizeByKind,
} from "../../lib/series";
import type { KindSummary } from "../../lib/series";

/** Ceilings, not row counts. Every table on the board now renders as
 * many rows as its panel has room for -- see `useFitRows` -- so these
 * only cap how much a panel is *prepared* to show, which is what stops
 * a tall window from asking for a sparkline per agent on the board. */
const MAX_AGENT_ROWS = 24;
const MAX_TRENDING_ROWS = 24;
const MAX_UPDATE_ROWS = 24;
const MAX_LEADER_ROWS = 24;
/** How often the board re-asks the hub. Slow enough to be cheap, quick
 * enough that a settling task shows up while you are still looking. */
const REFRESH_MS = 5000;

/** The supplied search glyph (`assets/search_icon.svg`), inlined.
 *
 * Two changes from the file as exported. It ships as a *white plate
 * with a dark magnifier*, which on these dark panels renders as a light
 * blob -- so the plate is dropped and the glyph takes `currentColor`,
 * letting CSS colour it. And the lens is already a second subpath of
 * the same path, punched in the export by laying a white circle over
 * it; `fill-rule: evenodd` makes it a real hole instead, so the ring is
 * transparent in the middle rather than painted with a background
 * colour that would have to be kept in sync with the panel. */
function SearchIcon() {
  return (
    <svg viewBox="1703 1453 1440 1440" width="13" height="13" aria-hidden="true">
      <path
        fill="currentColor"
        fillRule="evenodd"
        d="M2623.4,2271.99l113.55,157.33c21.1,29.23,18.55,68.17-10.72,90.24s-67.87,14.28-88.75-14.9l-112.75-157.51c-140.53,71.5-311.67,35.57-412.75-82.72-101.65-118.96-108.93-293.97-16.89-420.92,90.78-125.22,256.91-175.28,402.97-116.29,42.46,16.45,81.15,42.65,113.18,74.47,129.02,128.13,134.42,335.62,12.16,470.31ZM2587.59,2043.22c0-119.56-96.92-216.48-216.48-216.48s-216.48,96.92-216.48,216.48,96.92,216.48,216.48,216.48,216.48-96.92,216.48-216.48Z"
      />
    </svg>
  );
}

/** "3m" -> "3m ago"; "just now" stays as is. */
function ago(iso: string): string {
  const rel = formatRelative(iso);
  return rel === "just now" ? rel : `${rel} ago`;
}

interface AgentRow {
  pubkey: string;
  earned: number;
  series: number[];
  changePct: number | null;
}

interface Market {
  /** The capability tag this market trades in. */
  name: string;
  /** Open tasks and the bounty on offer across them -- how the markets
   * are ranked, so the biggest sit at the front of the carousel. */
  open: number;
  openBounty: number;
  agents: AgentRow[];
}

/** Top earners in a set of tasks, as that market's tickers.
 *
 * Written as one grouping pass plus work proportional to the rows
 * actually shown. The previous version filtered the whole task list once
 * per agent, which was fine against a few dozen agents and is not
 * against a thousand -- and this runs on every poll. */
function topAgents(tasks: TaskDto[], windowMs: number): AgentRow[] {
  const earnedBy = new Map<string, number>();
  for (const t of tasks) {
    if (t.status !== "Paid" || !t.claimant) continue;
    earnedBy.set(t.claimant, (earnedBy.get(t.claimant) ?? 0) + t.bounty);
  }

  const top = [...earnedBy.entries()].sort((a, b) => b[1] - a[1]).slice(0, MAX_AGENT_ROWS);
  if (top.length === 0) return [];

  // Only the rows that will be rendered get a curve.
  const wanted = new Set(top.map(([pubkey]) => pubkey));
  const paidByAgent = new Map<string, TaskDto[]>();
  for (const t of tasks) {
    if (t.status !== "Paid" || !t.claimant || !wanted.has(t.claimant)) continue;
    const list = paidByAgent.get(t.claimant);
    if (list) list.push(t);
    else paidByAgent.set(t.claimant, [t]);
  }

  return top.map(([pubkey, earned]) => {
    const paid = paidByAgent.get(pubkey) ?? [];
    const buckets = sumByCreatedAt(paid, (t) => t.bounty, { windowMs });
    // A single bucket of activity is not a trend. Left to itself the
    // period-over-period maths turns one payout into +100% or -100%
    // depending only on which half of the window it landed in, which
    // filled the column with confident-looking noise. Below two active
    // buckets there is nothing to compare, and "—" says so.
    const active = buckets.filter((v) => v > 0).length;
    return {
      pubkey,
      earned,
      // Cumulative curve for shape; change from the per-bucket sums,
      // since a cumulative series only rises and would read every agent
      // as permanently up.
      //
      // Fed the agent's own paid tasks rather than the market's whole
      // list. `agentEarningsSeries` would filter to the same set itself,
      // but doing it here reuses the grouping pass above -- which is
      // what keeps the cost flat now that a tall panel asks for twice
      // as many rows as it used to.
      series: agentEarningsSeries(paid, pubkey, { windowMs }),
      changePct: active >= 2 ? periodChangePct(buckets) : null,
    };
  });
}

/** One market per capability tag currently on the board, biggest first.
 *
 * Ranked by open bounty rather than task count, so a market is "big"
 * when there is real money on offer in it. The order is recomputed from
 * whatever the hub last returned, so markets genuinely move around as
 * work is posted and settled.
 *
 * Consensus tasks count here. They have no visible claimant -- the hub
 * hides who joined -- so they never contribute an agent row, but they do
 * carry bounty, and excluding them would have understated exactly the
 * markets where the biggest pooled work sits. */
function marketsByCapability(tasks: TaskDto[], windowMs: number): Market[] {
  const byCapability = new Map<string, TaskDto[]>();
  for (const t of tasks) {
    for (const capability of t.capabilities) {
      const list = byCapability.get(capability);
      if (list) list.push(t);
      else byCapability.set(capability, [t]);
    }
  }

  return [...byCapability.entries()]
    .map(([name, ofCapability]) => {
      const open = ofCapability.filter((t) => t.status === "Open");
      return {
        name,
        open: open.length,
        openBounty: open.reduce((sum, t) => sum + t.bounty, 0),
        agents: topAgents(ofCapability, windowMs),
      };
    })
    .sort((a, b) => b.openBounty - a.openBounty || b.open - a.open);
}

/** The board below the hero, laid out from the user's mockup: a quote
 * strip in a green-to-grey gradient outline, "market overview" with a
 * clipped carousel of category panels, a leaderboard/trends rail, a
 * latest feed, and a footer placeholder -- all over a faint grid.
 *
 * Panel *labels sit outside their panels* here, above the outline,
 * which is the main structural difference from the previous version. */
export default function Board({
  tasks,
}: {
  /** Fetched and polled by LandingPage and shared with the tape, so the
   * task list is walked once per refresh rather than once per consumer. */
  tasks: AsyncState<Page<TaskDto> & { complete: boolean }>;
}) {
  const leaders = useAsync(() => getLeaderboard(), [], REFRESH_MS);
  const [page, setPage] = useState(0);
  const [query, setQuery] = useState("");

  const items = useMemo(() => tasks.data?.items ?? [], [tasks.data]);
  const window = chooseWindow(items);

  const kinds = useMemo(
    () => summarizeByKind(items, { windowMs: window.windowMs }),
    [items, window.windowMs],
  );
  const markets = useMemo(
    () => marketsByCapability(items, window.windowMs),
    [items, window.windowMs],
  );
  const trending = useMemo(
    () =>
      summarizeByCapability(items, MAX_TRENDING_ROWS, { windowMs: window.windowMs })
        .slice()
        .sort((a, b) => Math.abs(b.changePct ?? -Infinity) - Math.abs(a.changePct ?? -Infinity)),
    [items, window.windowMs],
  );
  const latest = useMemo(
    () =>
      [...items]
        .sort((a, b) => b.created_at.localeCompare(a.created_at))
        .slice(0, MAX_UPDATE_ROWS),
    [items],
  );

  // One of these per table: the panel measures itself and says how many
  // rows it has room for, and the table renders that many.
  const [leaderFit, leaderRows] = useFitRows();
  const [trendFit, trendRows] = useFitRows();
  const [latestFit, latestRows] = useFitRows();

  // Rotated rather than sliced, so there is always one more item past
  // the clip -- the peek from the mockup that says the pager has
  // somewhere to go. Markets re-sort as the board moves, so `page` is an
  // offset into a list whose order is not fixed; that is the point.
  const ordered = markets.length
    ? Array.from({ length: markets.length }, (_, i) => markets[(page + i) % markets.length])
    : [];

  const found = (leaders.data ?? []).filter((l) =>
    l.pubkey.toLowerCase().includes(query.trim().toLowerCase()),
  );

  return (
    <section className="itx-board" aria-label="Market board">
      <div className="itx-board-inner">
        <QuoteStrip kinds={kinds} windowLabel={window.label} />

        <h2 className="itx-board-title">market overview</h2>

        <div className="itx-board-cols">
          <BoardNav markets={markets} page={page} onSelect={setPage} />

          <div className="itx-board-markets" id="itx-board-markets">
            {/* Floated over the track's right end rather than being a row
             * item: it must not take part in the width the panels divide
             * up, or the labels and the panels resolve their percentages
             * against different widths and stop lining up. */}
            <div className="itx-board-pager">
                <button
                  type="button"
                  aria-label="Previous category"
                  onClick={() => setPage((p) => (p + markets.length - 1) % Math.max(1, markets.length))}
                >
                  <svg viewBox="0 0 12 14" width="12" height="14" aria-hidden="true">
                    <path d="M11 1 L2 7 L11 13 Z" fill="currentColor" />
                  </svg>
                </button>
                <button
                  type="button"
                  aria-label="Next category"
                  onClick={() => setPage((p) => (p + 1) % Math.max(1, markets.length))}
                >
                  <svg viewBox="0 0 12 14" width="12" height="14" aria-hidden="true">
                    <path d="M1 1 L10 7 L1 13 Z" fill="currentColor" />
                  </svg>
                </button>
            </div>

            <div className="itx-board-carousel">
              {ordered.map((m) => (
                // Label and panel are one item, so the label cannot drift
                // from the panel it names at any width.
                <div className="itx-board-market" key={m.name}>
                  <span className="itx-board-label">
                    {m.name}
                    <span className="itx-board-label-sub">
                      {formatCount(m.open)} open · {formatCompactItx(m.openBounty)} itx
                    </span>
                  </span>
                  <MarketPanel
                    market={m}
                    windowLabel={window.label}
                    loading={tasks.loading}
                    error={tasks.error}
                  />
                </div>
              ))}
            </div>
          </div>

          <div className="itx-board-rail">
            <span className="itx-board-label">leaderboard</span>
            <div className="itx-board-panel itx-board-panel-leaders" id="itx-board-leaders">
              <div className="itx-board-search">
                <SearchIcon />
                <input
                  type="search"
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                  placeholder="agent search"
                  aria-label="Search agents by public key"
                />
              </div>
              <div className="itx-board-fit" ref={leaderFit}>
                {leaders.data === null ? (
                  <p className="itx-board-note">loading agents…</p>
                ) : found.length === 0 ? (
                  <p className="itx-board-note">
                    {query ? "no agent matches that key." : "no agents have earned yet."}
                  </p>
                ) : (
                  <table className="itx-board-table">
                    <tbody>
                      {found.slice(0, Math.min(leaderRows, MAX_LEADER_ROWS)).map((agent) => (
                        <tr key={agent.pubkey}>
                          <td>
                            <Link to={`/agents/${agent.pubkey}`}>
                              {truncatePubkey(agent.pubkey)}
                            </Link>
                          </td>
                          <td className="right">{formatCompactItx(agent.total_earned)}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                )}
              </div>
            </div>

            <span className="itx-board-label">trends</span>
            <div className="itx-board-panel itx-board-panel-trends" id="itx-board-trends">
              <div className="itx-board-fit" ref={trendFit}>
                {trending.length === 0 ? (
                  <p className="itx-board-note">no capability tags in use yet.</p>
                ) : (
                  <table className="itx-board-table">
                    <tbody>
                      {trending.slice(0, trendRows).map((row) => (
                        <tr key={row.capability}>
                          <td>
                            <Link to={`/tasks?capability=${encodeURIComponent(row.capability)}`}>
                              {row.capability}
                            </Link>
                          </td>
                          <td className="itx-board-cell-spark">
                            <Sparkline
                              values={row.series}
                              direction={directionOf(row.changePct)}
                              label={`${row.capability} tasks posted over the last ${window.label}`}
                            />
                          </td>
                          <td className={`right ${directionOf(row.changePct)}`}>
                            {formatPct(row.changePct)}
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                )}
              </div>
            </div>
          </div>
        </div>

        <div className="itx-board-labels itx-board-labels-latest">
          <span className="itx-board-label">latest</span>
          <span className="itx-board-live-dot" aria-label="live" title="live" />
        </div>
        <div className="itx-board-panel itx-board-panel-latest" id="itx-board-latest">
          <div className="itx-board-fit" ref={latestFit}>
            <ul className="itx-board-updates">
              {latest.length === 0 && !tasks.loading && (
                <li className="itx-board-note">nothing on the tape yet.</li>
              )}
              {latest.slice(0, latestRows).map((t) => (
                <li key={t.id}>
                  <span className="itx-board-dot" aria-hidden="true" />
                  <span className="itx-board-when">{ago(t.created_at)}</span>
                  <Link to={`/tasks/${t.id}`}>{t.description}</Link>
                  <span className="itx-board-amt">{formatCompactItx(t.bounty)} itx</span>
                </li>
              ))}
            </ul>
          </div>
        </div>

        {/* Deliberately empty for now, per the mockup -- the outline is
         * the deliverable at this stage. */}
        <footer className="itx-board-panel itx-board-footer" aria-label="Footer" />
      </div>
    </section>
  );
}

/** How many colour stops the strip's outline samples across its width.
 * The wash's front is soft (it spans most of the width), so the colour
 * changes slowly in space and five stops resolve it without banding. */
const SWEEP_STOPS = 5;

/** The Yahoo-Finance-style quote strip: one cell per task kind, in the
 * gradient outline from the mockup. Structure only for now -- how these
 * figures are chosen and formatted is a later pass.
 *
 * The outline's colour is driven from the same wash as the hero's market
 * line, sampled across the strip's width, so the two sweep together. It
 * writes CSS custom properties rather than a whole gradient string,
 * which keeps the gradient's geometry in the stylesheet and lets this
 * only supply colours. */
function QuoteStrip({ kinds, windowLabel }: { kinds: KindSummary[]; windowLabel: string }) {
  const stripRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const el = stripRef.current;
    if (!el) return;

    const paint = (t: number) => {
      const colors = sweepColors(t, SWEEP_STOPS);
      for (let i = 0; i < colors.length; i++) el.style.setProperty(`--q${i}`, colors[i]);
    };

    // Reading performance.now() is what locks this to the market line:
    // both sample the wash at the same absolute time, so they agree in
    // phase rather than merely sharing a period.
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      paint(2);
      return;
    }

    // Paint once here rather than waiting for the first frame. rAF does
    // not run while the document is hidden, so a tab opened in the
    // background would otherwise sit on the CSS fallback colour until it
    // was focused, then jump.
    paint(performance.now() / 1000);

    let raf = 0;
    const loop = () => {
      paint(performance.now() / 1000);
      raf = requestAnimationFrame(loop);
    };
    raf = requestAnimationFrame(loop);
    return () => cancelAnimationFrame(raf);
  }, []);

  return (
    <div className="itx-board-quotes" ref={stripRef}>
      <div className="itx-board-quotes-inner">
        {kinds.map((k) => (
          <div className="itx-board-quote" key={k.kind}>
            <div className="itx-board-quote-name">{formatKind(k.kind).toLowerCase()}</div>
            <div className="itx-board-quote-row">
              <div>
                <div className="itx-board-quote-value">{formatCompactItx(k.openBounty)}</div>
                <div className={`itx-board-quote-change ${directionOf(k.changePct)}`}>
                  {k.open} open {formatPct(k.changePct)}
                </div>
              </div>
              <Sparkline
                values={k.series}
                direction={directionOf(k.changePct)}
                width={60}
                height={22}
                label={`${formatKind(k.kind)} tasks posted over the last ${windowLabel}`}
              />
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

function MarketPanel({
  market,
  windowLabel,
  loading,
  error,
}: {
  market: Market;
  windowLabel: string;
  loading: boolean;
  error: Error | null;
}) {
  const agents = market.agents;
  // The panel is as tall as the row it sits in, which is set by the rail
  // beside it -- so how many agents belong here is a question only the
  // rendered box can answer. The header row is measured out of the
  // budget rather than assumed, since it is inside the same box.
  const [fitRef, rows] = useFitRows();
  return (
    <section className="itx-board-panel itx-board-panel-market">
      <div className="itx-board-fit" ref={fitRef}>
        {loading ? (
          <p className="itx-board-note">loading the board…</p>
        ) : error ? (
          <p className="itx-board-note">couldn&apos;t reach the hub. {error.message}</p>
        ) : agents.length === 0 ? (
          <p className="itx-board-note">
            nothing settled in {market.name} yet — open work here has not been paid out.
          </p>
        ) : (
          <table className="itx-board-table">
            <thead data-fit-fixed>
              <tr>
                <th>agent</th>
                <th>price</th>
                <th className="right">change</th>
              </tr>
            </thead>
            <tbody>
              {agents.slice(0, rows).map((a) => (
                <tr key={a.pubkey}>
                  <td>
                    <Link to={`/agents/${a.pubkey}`}>{truncatePubkey(a.pubkey)}</Link>
                  </td>
                  <td className="itx-board-cell-spark">
                    <Sparkline
                      values={a.series}
                      direction={directionOf(a.changePct)}
                      label={`earnings for agent ${a.pubkey} over the last ${windowLabel}`}
                    />
                  </td>
                  <td className={`right ${directionOf(a.changePct)}`}>{formatPct(a.changePct)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </section>
  );
}

/** The board's left rail: jump links to the four sections, then the live
 * list of markets, then the pages that carry on past the board.
 *
 * The market entries are the useful part -- with a dozen capability tags
 * and three panels visible at a time, the pager alone means clicking
 * through the carousel to find one. These select it directly, and the
 * one currently at the front of the carousel is marked.
 *
 * The list fills the rail the same way every other panel does, so a tall
 * window shows more markets rather than more empty rail. */
function BoardNav({
  markets,
  page,
  onSelect,
}: {
  markets: Market[];
  page: number;
  onSelect: (index: number) => void;
}) {
  const [fitRef, rows] = useFitRows();
  const current = markets.length ? markets[page % markets.length]?.name : undefined;

  return (
    <nav className="itx-board-nav" aria-label="Board sections">
      <span className="itx-board-label">navigate</span>
      <div className="itx-board-panel itx-board-panel-nav">
        <ul className="itx-board-navlist">
          <li>
            <a href="#itx-board-markets">market overview</a>
          </li>
          <li>
            <a href="#itx-board-leaders">leaderboard</a>
          </li>
          <li>
            <a href="#itx-board-trends">trends</a>
          </li>
          <li>
            <a href="#itx-board-latest">latest</a>
          </li>
        </ul>

        <span className="itx-board-navhead">markets</span>
        <div className="itx-board-fit" ref={fitRef}>
          <ul className="itx-board-navlist">
            {markets.slice(0, rows).map((m, index) => (
              <li key={m.name}>
                <button
                  type="button"
                  className={m.name === current ? "is-active" : undefined}
                  aria-current={m.name === current ? "true" : undefined}
                  onClick={() => onSelect(index)}
                >
                  {m.name}
                </button>
              </li>
            ))}
          </ul>
        </div>

        <ul className="itx-board-navlist itx-board-navlist-pages">
          <li>
            <Link to="/tasks">all tasks</Link>
          </li>
          <li>
            <Link to="/leaderboard">full leaderboard</Link>
          </li>
        </ul>
      </div>
    </nav>
  );
}

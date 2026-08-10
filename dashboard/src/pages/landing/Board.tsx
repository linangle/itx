import { useMemo, useState } from "react";
import { Link } from "react-router-dom";
import Sparkline from "../../components/Sparkline";
import { useAsync } from "../../hooks/useAsync";
import { getLeaderboard, listAllTasks } from "../../lib/hub";
import type { TaskDto } from "../../lib/hub";
import {
  directionOf,
  formatCompactItx,
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

const KIND_COUNT = 3;
const AGENT_ROWS = 10;
const TRENDING_ROWS = 6;
const UPDATE_ROWS = 5;
const LEADER_ROWS = 8;

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

/** Agents paid for work of one kind, as that market's tickers. */
function agentsForKind(tasks: TaskDto[], kind: TaskDto["kind"], windowMs: number): AgentRow[] {
  const ofKind = tasks.filter((t) => t.kind === kind);
  const paid = ofKind.filter((t) => t.status === "Paid" && t.claimant !== null);
  const earnedBy = new Map<string, number>();
  for (const t of paid) earnedBy.set(t.claimant!, (earnedBy.get(t.claimant!) ?? 0) + t.bounty);

  return [...earnedBy.entries()]
    .map(([pubkey, earned]) => {
      const theirs = paid.filter((t) => t.claimant === pubkey);
      return {
        pubkey,
        earned,
        // Cumulative curve for shape; change from the per-bucket sums,
        // since a cumulative series only rises and would read every
        // agent as permanently up.
        series: agentEarningsSeries(ofKind, pubkey, { windowMs }),
        changePct: periodChangePct(sumByCreatedAt(theirs, (t) => t.bounty, { windowMs })),
      };
    })
    .sort((a, b) => b.earned - a.earned)
    .slice(0, AGENT_ROWS);
}

/** The board below the hero, laid out from the user's mockup: a quote
 * strip in a green-to-grey gradient outline, "market overview" with a
 * clipped carousel of category panels, a leaderboard/trends rail, a
 * latest feed, and a footer placeholder -- all over a faint grid.
 *
 * Panel *labels sit outside their panels* here, above the outline,
 * which is the main structural difference from the previous version. */
export default function Board() {
  const tasks = useAsync(() => listAllTasks({ status: "all" }), []);
  const leaders = useAsync(() => getLeaderboard(), []);
  const [page, setPage] = useState(0);
  const [query, setQuery] = useState("");

  const items = useMemo(() => tasks.data?.items ?? [], [tasks.data]);
  const window = chooseWindow(items);

  const kinds = useMemo(
    () => summarizeByKind(items, { windowMs: window.windowMs }),
    [items, window.windowMs],
  );
  const trending = useMemo(
    () =>
      summarizeByCapability(items, 12, { windowMs: window.windowMs })
        .slice()
        .sort((a, b) => Math.abs(b.changePct ?? -Infinity) - Math.abs(a.changePct ?? -Infinity))
        .slice(0, TRENDING_ROWS),
    [items, window.windowMs],
  );
  const latest = useMemo(
    () => [...items].sort((a, b) => b.created_at.localeCompare(a.created_at)).slice(0, UPDATE_ROWS),
    [items],
  );

  // All three kinds stay mounted and the pager rotates the order, so the
  // third is always half-visible past the clip -- the peek in the mockup
  // that signals there is more to page to.
  const ordered = Array.from({ length: KIND_COUNT }, (_, i) => kinds[(page + i) % KIND_COUNT]).filter(
    Boolean,
  );

  const found = (leaders.data ?? []).filter((l) =>
    l.pubkey.toLowerCase().includes(query.trim().toLowerCase()),
  );

  return (
    <section className="itx-board" aria-label="Market board">
      <div className="itx-board-inner">
        <QuoteStrip kinds={kinds} windowLabel={window.label} />

        <h2 className="itx-board-title">market overview</h2>

        <div className="itx-board-cols">
          <div className="itx-board-markets">
            <div className="itx-board-labels">
              {ordered.slice(0, 2).map((k) => (
                <span className="itx-board-label" key={k.kind}>
                  {formatKind(k.kind).toLowerCase()}
                </span>
              ))}
              <div className="itx-board-pager">
                <button
                  type="button"
                  aria-label="Previous category"
                  onClick={() => setPage((p) => (p + KIND_COUNT - 1) % KIND_COUNT)}
                >
                  <svg viewBox="0 0 12 14" width="12" height="14" aria-hidden="true">
                    <path d="M11 1 L2 7 L11 13 Z" fill="currentColor" />
                  </svg>
                </button>
                <button
                  type="button"
                  aria-label="Next category"
                  onClick={() => setPage((p) => (p + 1) % KIND_COUNT)}
                >
                  <svg viewBox="0 0 12 14" width="12" height="14" aria-hidden="true">
                    <path d="M1 1 L10 7 L1 13 Z" fill="currentColor" />
                  </svg>
                </button>
              </div>
            </div>

            <div className="itx-board-carousel">
              {ordered.map((k) => (
                <KindPanel
                  key={k.kind}
                  summary={k}
                  agents={agentsForKind(items, k.kind, window.windowMs)}
                  windowLabel={window.label}
                  loading={tasks.loading}
                  error={tasks.error}
                />
              ))}
            </div>
          </div>

          <div className="itx-board-rail">
            <span className="itx-board-label">leaderboard</span>
            <div className="itx-board-panel itx-board-panel-leaders">
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
              {leaders.data === null ? (
                <p className="itx-board-note">loading agents…</p>
              ) : found.length === 0 ? (
                <p className="itx-board-note">
                  {query ? "no agent matches that key." : "no agents have earned yet."}
                </p>
              ) : (
                <table className="itx-board-table">
                  <tbody>
                    {found.slice(0, LEADER_ROWS).map((agent) => (
                      <tr key={agent.pubkey}>
                        <td>
                          <Link to={`/agents/${agent.pubkey}`}>{truncatePubkey(agent.pubkey)}</Link>
                        </td>
                        <td className="right">{formatCompactItx(agent.total_earned)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </div>

            <span className="itx-board-label">trends</span>
            <div className="itx-board-panel itx-board-panel-trends">
              {trending.length === 0 ? (
                <p className="itx-board-note">no capability tags in use yet.</p>
              ) : (
                <table className="itx-board-table">
                  <tbody>
                    {trending.map((row) => (
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

        <div className="itx-board-labels itx-board-labels-latest">
          <span className="itx-board-label">latest</span>
          <span className="itx-board-live-dot" aria-label="live" title="live" />
        </div>
        <div className="itx-board-panel itx-board-panel-latest">
          <ul className="itx-board-updates">
            {latest.length === 0 && !tasks.loading && (
              <li className="itx-board-note">nothing on the tape yet.</li>
            )}
            {latest.map((t) => (
              <li key={t.id}>
                <span className="itx-board-dot" aria-hidden="true" />
                <span className="itx-board-when">{ago(t.created_at)}</span>
                <Link to={`/tasks/${t.id}`}>{t.description}</Link>
                <span className="itx-board-amt">{formatCompactItx(t.bounty)} itx</span>
              </li>
            ))}
          </ul>
        </div>

        {/* Deliberately empty for now, per the mockup -- the outline is
         * the deliverable at this stage. */}
        <footer className="itx-board-panel itx-board-footer" aria-label="Footer" />
      </div>
    </section>
  );
}

/** The Yahoo-Finance-style quote strip: one cell per task kind, in the
 * green-to-grey gradient outline from the mockup. Structure only for
 * now -- how these figures are chosen and formatted is a later pass. */
function QuoteStrip({ kinds, windowLabel }: { kinds: KindSummary[]; windowLabel: string }) {
  return (
    <div className="itx-board-quotes">
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

function KindPanel({
  summary,
  agents,
  windowLabel,
  loading,
  error,
}: {
  summary: KindSummary;
  agents: AgentRow[];
  windowLabel: string;
  loading: boolean;
  error: Error | null;
}) {
  return (
    <section className="itx-board-panel itx-board-panel-market">
      {loading ? (
        <p className="itx-board-note">loading the board…</p>
      ) : error ? (
        <p className="itx-board-note">couldn&apos;t reach the hub. {error.message}</p>
      ) : agents.length === 0 ? (
        <p className="itx-board-note">
          {summary.kind === "consensus"
            ? // Not a data gap: the hub hides consensus winners by
              // design, so this market can never list agent tickers.
              "consensus winners are never exposed, so no agents can be listed here."
            : "no settled work in this category yet."}
        </p>
      ) : (
        <table className="itx-board-table">
          <thead>
            <tr>
              <th>agent</th>
              <th>price</th>
              <th className="right">change</th>
            </tr>
          </thead>
          <tbody>
            {agents.map((a) => (
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
    </section>
  );
}

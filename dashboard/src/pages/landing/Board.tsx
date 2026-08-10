import { useMemo, useState } from "react";
import { Link } from "react-router-dom";
// The board section is set in Kalam, the marker-style handwriting of the
// user's sketch -- self-hosted via fontsource like every other font here.
import "@fontsource/kalam/400.css";
import "@fontsource/kalam/700.css";
import Sparkline from "../../components/Sparkline";
import { useAsync } from "../../hooks/useAsync";
import { getLeaderboard, listAllTasks } from "../../lib/hub";
import type { TaskDto } from "../../lib/hub";
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

const KIND_COUNT = 3;
const AGENT_ROWS = 8;
const TRENDING_ROWS = 5;
const UPDATE_ROWS = 7;
const LEADER_ROWS = 8;

/** "3m" -> "3m ago"; "just now" stays as is. The sketch writes its
 * update times out as "x min ago", and next to a red "live" pill the
 * direction of the timestamp is worth the extra word. */
function ago(iso: string): string {
  const rel = formatRelative(iso);
  return rel === "just now" ? rel : `${rel} ago`;
}

/** One market row: an agent who has been paid for work of this kind,
 * with their in-kind earnings curve. The agents *are* the tickers on
 * this board -- the sketch's category tables list agents with a price
 * squiggle and a change column, so each kind panel becomes the market
 * for that category of work. */
interface AgentRow {
  pubkey: string;
  earned: number;
  series: number[];
  changePct: number | null;
}

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
        // Cumulative curve for shape; change measured on the per-bucket
        // sums instead, since a cumulative series only ever rises and
        // would put every agent permanently "up".
        series: agentEarningsSeries(ofKind, pubkey, { windowMs }),
        changePct: periodChangePct(sumByCreatedAt(theirs, (t) => t.bounty, { windowMs })),
      };
    })
    .sort((a, b) => b.earned - a.earned)
    .slice(0, AGENT_ROWS);
}

/** The redesigned board below the hero, laid out from the user's
 * hand-drawn mock: a kind-summary strip, "markets overview" with two
 * visible category panels and a pager, a leaderboard-with-search and
 * trending rail, and a live latest-updates feed. Same hub data as the
 * old overview -- only the presentation changed. */
export default function Board() {
  const tasks = useAsync(() => listAllTasks({ status: "all" }), []);
  const leaders = useAsync(() => getLeaderboard(), []);
  // Which kind occupies the left panel slot; the pager walks this
  // cyclically and the right slot shows the next kind along.
  const [page, setPage] = useState(0);
  const [query, setQuery] = useState("");

  const items = useMemo(() => tasks.data?.items ?? [], [tasks.data]);
  const window = chooseWindow(items);

  const kinds = useMemo(() => summarizeByKind(items, { windowMs: window.windowMs }), [items, window.windowMs]);
  const trending = useMemo(
    () =>
      summarizeByCapability(items, 12, { windowMs: window.windowMs })
        .slice()
        .sort(
          (a, b) => Math.abs(b.changePct ?? -Infinity) - Math.abs(a.changePct ?? -Infinity),
        )
        .slice(0, TRENDING_ROWS),
    [items, window.windowMs],
  );
  const latest = useMemo(
    () =>
      [...items]
        .sort((a, b) => b.created_at.localeCompare(a.created_at))
        .slice(0, UPDATE_ROWS),
    [items],
  );

  const visible = [kinds[page % KIND_COUNT], kinds[(page + 1) % KIND_COUNT]].filter(Boolean);

  const found = (leaders.data ?? []).filter((l) =>
    l.pubkey.toLowerCase().includes(query.trim().toLowerCase()),
  );

  return (
    <section className="itx-board" aria-label="Market board">
      <div className="itx-board-strip">
        {kinds.map((k) => (
          <div className="itx-board-strip-cell" key={k.kind}>
            <div>
              <div className="itx-board-strip-label">
                {formatKind(k.kind).toLowerCase()}{" "}
                <span className="itx-board-strip-count">{formatCount(k.open)}</span>
              </div>
              <div className={`itx-board-strip-delta ${directionOf(k.changePct)}`}>
                {formatCompactItx(k.openBounty)} itx {formatPct(k.changePct)}
              </div>
            </div>
            <Sparkline
              values={k.series}
              direction={directionOf(k.changePct)}
              width={92}
              height={26}
              label={`${formatKind(k.kind)} tasks posted over the last ${window.label}`}
            />
          </div>
        ))}
      </div>

      <nav className="itx-board-nav" aria-label="Board sections">
        <Link to="/tasks">all tasks</Link>
        <Link to="/tasks?kind=hash_match">hash match</Link>
        <Link to="/tasks?kind=consensus">consensus</Link>
        <Link to="/tasks?kind=disputable">disputable</Link>
      </nav>

      <div className="itx-board-main">
        <div className="itx-board-head">
          <h2>markets overview</h2>
          <div className="itx-board-pager">
            <button
              type="button"
              aria-label="Previous category"
              onClick={() => setPage((p) => (p + KIND_COUNT - 1) % KIND_COUNT)}
            >
              <svg viewBox="0 0 12 14" width="13" height="15" aria-hidden="true">
                <path d="M11 1 L2 7 L11 13 Z" fill="currentColor" />
              </svg>
            </button>
            <button
              type="button"
              aria-label="Next category"
              onClick={() => setPage((p) => (p + 1) % KIND_COUNT)}
            >
              <svg viewBox="0 0 12 14" width="13" height="15" aria-hidden="true">
                <path d="M1 1 L10 7 L1 13 Z" fill="currentColor" />
              </svg>
            </button>
          </div>
        </div>

        {tasks.loading && <p className="itx-board-note">loading the board…</p>}
        {tasks.error && (
          <p className="itx-board-note">couldn&apos;t reach the hub. {tasks.error.message}</p>
        )}

        <div className="itx-board-panels">
          {visible.map((k) => (
            <KindPanel
              key={k.kind}
              summary={k}
              agents={agentsForKind(items, k.kind, window.windowMs)}
              windowLabel={window.label}
            />
          ))}
        </div>

        <div className="itx-board-head itx-board-head-updates">
          <h2>latest updates</h2>
          <span className="itx-board-live">live</span>
        </div>
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

      <aside className="itx-board-rail">
        <div className="itx-board-panel">
          <h3>leaderboard</h3>
          <input
            className="itx-board-search"
            type="search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="agent search"
            aria-label="Search agents by public key"
          />
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

        <div className="itx-board-panel">
          <h3>trending</h3>
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
      </aside>
    </section>
  );
}

function KindPanel({
  summary,
  agents,
  windowLabel,
}: {
  summary: KindSummary;
  agents: AgentRow[];
  windowLabel: string;
}) {
  return (
    <section className="itx-board-panel">
      <h3>{formatKind(summary.kind).toLowerCase()}</h3>
      {agents.length === 0 ? (
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

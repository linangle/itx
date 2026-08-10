// A seeded stand-in for the hub's read-only endpoints, for frontend work.
//
// Why this exists: getting the real hub to show anything means running a
// chain node, funding the operator, and posting tasks through signed
// envelopes. That's the right thing to test against before shipping, but
// it's far too slow a loop for iterating on a table layout -- and an
// empty board can't exercise any of the states the UI has to handle.
//
// This serves the same shapes as `hub/src/handlers.rs` (including the
// `X-Total-Count` header and CORS), seeded with a deterministic week of
// activity: three task kinds, every status, overlapping capability tags,
// and a handful of agents with different track records.
//
//   node dashboard/mock/hub.mjs
//   VITE_HUB_URL=http://127.0.0.1:9101 npm run dev
//
// It is a fixture, not a simulator: nothing here validates signatures,
// enforces state transitions, or accepts writes.

import { createServer } from "node:http";

const PORT = Number(process.env.PORT ?? 9101);
const UNITS = 100_000_000;
const NOW = Date.now();
const DAY = 86_400_000;

// A fixed 32-bit LCG rather than Math.random, so every reload shows the
// same board and a layout change is the only thing that ever moves.
let seed = 20260809;
function random() {
  seed = (seed * 1_664_525 + 1_013_904_223) % 4_294_967_296;
  return seed / 4_294_967_296;
}

function pick(items) {
  return items[Math.floor(random() * items.length)];
}

function pubkey(index) {
  return `02${String(index).padStart(2, "0")}${"abcdef0123456789".repeat(4)}`.slice(0, 66);
}

const AGENTS = Array.from({ length: 6 }, (_, i) => pubkey(i + 1));
const OPERATOR = pubkey(99);

const CAPABILITIES = ["python", "translation", "ocr", "scraping", "summarization", "rust"];
const DESCRIPTIONS = [
  "Transcribe a 12-minute audio clip to text",
  "Compute SHA256 of the attached dataset manifest",
  "Translate a product listing into German",
  "Summarize a 40-page regulatory filing",
  "Extract tables from a scanned invoice PDF",
  "Rank these 200 search results by relevance",
  "Label sentiment across a customer feedback batch",
  "Deduplicate a 50k-row address list",
  "Write property tests for a parser module",
  "Classify 500 support tickets by topic",
  "Geocode a batch of freeform address strings",
  "Verify a proof-of-work nonce against a target",
];
const STATUSES = ["Open", "Open", "Open", "Claimed", "Paid", "Paid", "Paid", "Verified", "Closed"];

function makeTask(index) {
  const kind = pick(["hash_match", "hash_match", "consensus", "disputable"]);
  const status = pick(STATUSES);
  // Weighted toward recent so the sparklines have visible slope rather
  // than a uniform block.
  const ageDays = Math.pow(random(), 1.7) * 7;
  const created_at = new Date(NOW - ageDays * DAY).toISOString();
  const claimed = status !== "Open";

  const base = {
    id: `00000000-0000-4000-8000-${String(index).padStart(12, "0")}`,
    description: pick(DESCRIPTIONS),
    bounty: Math.round((0.05 + random() * 3) * UNITS),
    status,
    poster: random() < 0.4 ? OPERATOR : pick(AGENTS),
    claimant: claimed && kind !== "consensus" ? pick(AGENTS) : null,
    failed_attempts: random() < 0.25 ? Math.floor(random() * 3) : 0,
    min_reputation: random() < 0.2 ? Math.floor(random() * 4) : 0,
    close_reason: status === "Closed" ? pick(["no_majority", "understaffed"]) : null,
    capabilities: random() < 0.75 ? [pick(CAPABILITIES)] : [],
    created_at,
    kind,
  };

  if (kind === "consensus") {
    const num_assignees = 2 + Math.floor(random() * 3);
    return {
      ...base,
      num_assignees,
      assignees_joined: status === "Open" ? Math.floor(random() * num_assignees) : num_assignees,
      join_deadline: new Date(NOW - ageDays * DAY + DAY).toISOString(),
      submission_deadline: status === "Open" ? null : new Date(NOW - ageDays * DAY + 2 * DAY).toISOString(),
    };
  }

  if (kind === "disputable") {
    const answered = status !== "Open";
    // Deterministic rather than probabilistic: a fixture exists to make
    // every UI state reachable, and with a random gate the seed happened
    // to produce zero disputes across all eight disputable tasks --
    // leaving the dispute callout, bond display, and resolution states
    // permanently unrendered. Every third answered task is challenged,
    // and every other one of those is still awaiting the operator.
    const disputed = answered && index % 3 === 0;
    return {
      ...base,
      answer: answered ? "See attached working; total is 41,208." : null,
      dispute_deadline: answered ? new Date(NOW - ageDays * DAY + DAY).toISOString() : null,
      dispute: disputed
        ? {
            challenger: pick(AGENTS),
            reason: "Figures don't reconcile with the source data.",
            bond_amount: Math.round(0.2 * UNITS),
            filed_at: new Date(NOW - ageDays * DAY + 0.5 * DAY).toISOString(),
            resolution: index % 6 === 0 ? null : pick(["challenger_wins", "assignee_wins"]),
          }
        : null,
    };
  }

  return base;
}

const TASKS = Array.from({ length: 47 }, (_, i) => makeTask(i)).sort((a, b) =>
  a.created_at.localeCompare(b.created_at),
);

const LEADERBOARD = AGENTS.map((pk) => {
  const paid = TASKS.filter((t) => t.claimant === pk && t.status === "Paid");
  return {
    pubkey: pk,
    completed: paid.length,
    failed: Math.floor(random() * 3),
    total_earned: paid.reduce((sum, t) => sum + t.bounty, 0),
    // One agent's node lookup deliberately fails, so the null-net_worth
    // path gets exercised on every page load rather than only in prod.
    net_worth: pk === AGENTS[3] ? null : Math.round(random() * 40 * UNITS),
  };
})
  .filter((a) => a.completed > 0 || a.total_earned > 0)
  .sort((a, b) => b.total_earned - a.total_earned);

function send(res, status, body, extraHeaders = {}) {
  const payload = typeof body === "string" ? body : JSON.stringify(body);
  res.writeHead(status, {
    "content-type": typeof body === "string" ? "text/plain" : "application/json",
    "access-control-allow-origin": "*",
    "access-control-expose-headers": "x-total-count",
    ...extraHeaders,
  });
  res.end(payload);
}

createServer((req, res) => {
  const url = new URL(req.url, `http://${req.headers.host}`);
  const path = url.pathname;

  if (path === "/tasks") {
    const status = (url.searchParams.get("status") ?? "open").toLowerCase();
    const capability = url.searchParams.get("capability")?.trim().toLowerCase();
    const offset = Number(url.searchParams.get("offset") ?? 0);
    const limit = Math.min(Number(url.searchParams.get("limit") ?? 50), 200);

    let matched = TASKS;
    if (status !== "all") matched = matched.filter((t) => t.status.toLowerCase() === status);
    if (capability) matched = matched.filter((t) => t.capabilities.includes(capability));

    return send(res, 200, matched.slice(offset, offset + limit), {
      "x-total-count": String(matched.length),
    });
  }

  const taskMatch = path.match(/^\/tasks\/([^/]+)$/);
  if (taskMatch) {
    const task = TASKS.find((t) => t.id === taskMatch[1]);
    return task ? send(res, 200, task) : send(res, 404, { error: "task not found" });
  }

  if (path === "/leaderboard") return send(res, 200, LEADERBOARD);

  const repMatch = path.match(/^\/reputation\/([^/]+)$/);
  if (repMatch) {
    const entry = LEADERBOARD.find((a) => a.pubkey === repMatch[1]);
    return send(
      res,
      200,
      entry
        ? { completed: entry.completed, failed: entry.failed, total_earned: entry.total_earned, net_worth: entry.net_worth }
        : { completed: 0, failed: 0, total_earned: 0, net_worth: 0 },
    );
  }

  send(res, 404, { error: "not found" });
}).listen(PORT, () => {
  console.log(`mock hub on http://127.0.0.1:${PORT} — ${TASKS.length} tasks, ${LEADERBOARD.length} agents`);
});

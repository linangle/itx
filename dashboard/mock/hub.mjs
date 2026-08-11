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
// activity sized like a real marketplace rather than a smoke test:
// two dozen agents with distinct keys, a few hundred tasks, every
// status, and capability tags that trend differently on purpose --
// some surging, some fading, some steady -- so change columns show
// real ups and downs instead of a wall of identical numbers.
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

function hex(length) {
  let out = "";
  for (let i = 0; i < length; i++) out += "0123456789abcdef"[Math.floor(random() * 16)];
  return out;
}

// Distinct-looking compressed SEC1 keys (02/03 prefix + 64 hex). The old
// fixture built every key from the same repeated string, so a table of
// truncated pubkeys read as one agent cloned -- realistic filler needs
// the first six and last four characters to differ. Generated before
// anything else so the key material is stable in the LCG stream.
const AGENTS = Array.from({ length: 26 }, () => (random() < 0.5 ? "02" : "03") + hex(64));
const OPERATOR = "02" + hex(64);

// Which agents work which kind. Overlapping pools rather than one big
// one, so each market's ticker table has its own cast with familiar
// faces recurring, the way a real marketplace has specialists.
const HASH_WORKERS = AGENTS.slice(0, 12);
const DISPUTE_WORKERS = AGENTS.slice(8, 20);

// Each tag carries an activity profile that shapes *when* its tasks
// happened: "surging" masses them into the recent half of the window,
// "fading" into the early half, "steady" spreads them evenly. That is
// what makes period-over-period change read as a mix of up, down and
// flat instead of null everywhere (the old fixture's single weighting
// left one half of the window nearly empty).
const CAPABILITIES = [
  ["python", "surging"],
  ["rust", "steady"],
  ["translation", "fading"],
  ["ocr", "steady"],
  ["scraping", "surging"],
  ["summarization", "fading"],
  ["geocoding", "steady"],
  ["labeling", "surging"],
  ["transcription", "steady"],
  ["vision", "fading"],
  ["sql", "steady"],
  ["prover", "surging"],
];

const SPAN_DAYS = 6.5;
function ageDaysFor(profile) {
  const r = random();
  if (profile === "surging") return Math.pow(r, 2.4) * SPAN_DAYS;
  if (profile === "fading") return (1 - Math.pow(r, 2.4)) * SPAN_DAYS;
  return r * SPAN_DAYS;
}

const DESCRIPTIONS = [
  "Transcribe a 12-minute audio clip to text",
  "Transcribe a 3-minute voicemail backlog",
  "Compute SHA256 of the attached dataset manifest",
  "Compute checksums for a nightly backup set",
  "Translate a product listing into German",
  "Translate onboarding emails into Japanese",
  "Translate a help-center article into Spanish",
  "Summarize a 40-page regulatory filing",
  "Summarize this week's incident reports",
  "Extract tables from a scanned invoice PDF",
  "Extract line items from 80 receipts",
  "Rank these 200 search results by relevance",
  "Rank candidate headlines for clickthrough",
  "Label sentiment across a customer feedback batch",
  "Label 1,200 images for a detection model",
  "Deduplicate a 50k-row address list",
  "Deduplicate a merged CRM export",
  "Write property tests for a parser module",
  "Write fuzz harnesses for a decoder crate",
  "Classify 500 support tickets by topic",
  "Classify job postings by seniority",
  "Geocode a batch of freeform address strings",
  "Geocode delivery stops for a route planner",
  "Verify a proof-of-work nonce against a target",
  "Verify Merkle proofs for a light client",
  "Scrape a public filings index into JSON",
  "Scrape a public tenders portal daily snapshot",
  "OCR a box of handwritten lab notebooks",
  "OCR historical census sheets, batch 7",
  "Normalize currency fields across ledgers",
  "Draft SQL for a churn cohort report",
  "Backfill missing alt-text for a docs site",
];

const STATUSES = [
  "Open",
  "Open",
  "Open",
  "Claimed",
  "Claimed",
  "Paid",
  "Paid",
  "Paid",
  "Paid",
  "Verified",
  "Closed",
];

function makeTask(index) {
  const kind = pick(["hash_match", "hash_match", "consensus", "disputable", "disputable"]);
  const status = pick(STATUSES);
  const tagged = random() < 0.8 ? pick(CAPABILITIES) : null;
  const ageDays = ageDaysFor(tagged ? tagged[1] : "steady");
  const created_at = new Date(NOW - ageDays * DAY).toISOString();
  const claimed = status !== "Open";
  const workers = kind === "hash_match" ? HASH_WORKERS : DISPUTE_WORKERS;

  const base = {
    id: `00000000-0000-4000-8000-${String(index).padStart(12, "0")}`,
    description: pick(DESCRIPTIONS),
    // Long-tail bounties: mostly under a few ITX, the occasional whale.
    bounty: Math.round((0.05 + Math.pow(random(), 3) * 40) * UNITS),
    status,
    poster: random() < 0.25 ? OPERATOR : pick(AGENTS),
    claimant: claimed && kind !== "consensus" ? pick(workers) : null,
    failed_attempts: random() < 0.25 ? Math.floor(random() * 3) : 0,
    min_reputation: random() < 0.2 ? Math.floor(random() * 4) : 0,
    close_reason: status === "Closed" ? pick(["no_majority", "understaffed"]) : null,
    capabilities: tagged ? [tagged[0]] : [],
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
    // to produce zero disputes -- leaving the dispute callout, bond
    // display, and resolution states permanently unrendered. Every third
    // answered task is challenged, and every other one of those is still
    // awaiting the operator; those pending ones carry the Disputed
    // status the type already declares, so the badge and tape paths for
    // it are exercised too.
    const disputed = answered && index % 3 === 0;
    const resolution = disputed && index % 6 !== 0 ? pick(["challenger_wins", "assignee_wins"]) : null;
    return {
      ...base,
      status: disputed && resolution === null ? "Disputed" : base.status,
      answer: answered ? "See attached working; total is 41,208." : null,
      dispute_deadline: answered ? new Date(NOW - ageDays * DAY + DAY).toISOString() : null,
      dispute: disputed
        ? {
            challenger: pick(DISPUTE_WORKERS),
            reason: "Figures don't reconcile with the source data.",
            bond_amount: Math.round(0.2 * UNITS),
            filed_at: new Date(NOW - ageDays * DAY + 0.5 * DAY).toISOString(),
            resolution,
          }
        : null,
    };
  }

  return base;
}

const TASKS = Array.from({ length: 220 }, (_, i) => makeTask(i)).sort((a, b) =>
  a.created_at.localeCompare(b.created_at),
);

const LEADERBOARD = AGENTS.map((pk) => {
  const paid = TASKS.filter((t) => t.claimant === pk && t.status === "Paid");
  return {
    pubkey: pk,
    completed: paid.length,
    failed: Math.floor(random() * 4),
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

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
// 120 agents with distinct keys, several hundred tasks, every status,
// and capability tags that trend differently on purpose -- some
// surging, some fading, some steady -- so change columns show real ups
// and downs instead of a wall of identical numbers.
//
// It also keeps running. After the backfill it ticks on a timer,
// posting new tasks and advancing existing ones through their
// lifecycles, so the board, the tape and the "live" pill have something
// genuinely moving to show. Set `STATIC=1` to freeze it at the backfill
// instead, which is what you want when comparing two screenshots.
//
//   node dashboard/mock/hub.mjs
//   VITE_HUB_URL=http://127.0.0.1:9101 npm run dev
//
// It is a fixture, not a simulator: nothing here validates signatures,
// enforces state transitions, or accepts writes.

import { createServer } from "node:http";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { gzipSync } from "node:zlib";

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
const AGENTS = Array.from({ length: 1000 }, () => (random() < 0.5 ? "02" : "03") + hex(64));
const OPERATOR = "02" + hex(64);

// Which agents work which kind. Overlapping pools rather than one big
// one, so each market's ticker table has its own cast with familiar
// faces recurring, the way a real marketplace has specialists.
const HASH_WORKERS = AGENTS.slice(0, 620);
const DISPUTE_WORKERS = AGENTS.slice(380, 1000);

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

// Agents specialise. Without this, a thousand agents spread across a
// dozen tags leaves almost everyone with a single paid task per market,
// which is not just thin -- it makes the change column meaningless,
// since one data point in one half of the window is either +100% or
// -100% and never a trend. Rosters are kept wide on purpose -- the point
// of a thousand agents is that a thousand of them are working, so the
// density comes from the volume of tasks rather than from narrowing the
// field. Specialisation stays because it is true of real marketplaces
// (an agent good at OCR keeps drawing OCR work), but at 55 per tag it
// shapes who recurs without shutting anyone out.
const SPECIALISTS = new Map(
  CAPABILITIES.map(([name]) => {
    const roster = [];
    for (let i = 0; i < 55; i++) roster.push(AGENTS[Math.floor(random() * AGENTS.length)]);
    return [name, [...new Set(roster)]];
  }),
);

function workerFor(kind, capability) {
  const regulars = capability ? SPECIALISTS.get(capability) : null;
  if (regulars && regulars.length > 0 && random() < 0.85) return pick(regulars);
  // Untagged work, and the occasional outsider taking a job.
  return pick(kind === "hash_match" ? HASH_WORKERS : DISPUTE_WORKERS);
}

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

  const base = {
    id: `00000000-0000-4000-8000-${String(index).padStart(12, "0")}`,
    description: pick(DESCRIPTIONS),
    // Long-tail bounties: mostly under a few ITX, the occasional whale.
    bounty: Math.round((0.05 + Math.pow(random(), 3) * 40) * UNITS),
    status,
    poster: random() < 0.25 ? OPERATOR : pick(AGENTS),
    claimant: claimed && kind !== "consensus" ? workerFor(kind, tagged?.[0] ?? null) : null,
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

// Sized for density rather than just presence. The change column is
// period-over-period, so an agent needs paid work in *both* halves of
// the window for it to say anything -- and at 2000 tasks, 81% of
// agent-tag pairs had exactly one payout, which is either a misleading
// -100% or an empty dash. Volume is what fixes that without narrowing
// the field to a handful of specialists.
const BACKFILL = 5000;
const TASKS = Array.from({ length: BACKFILL }, (_, i) => makeTask(i)).sort((a, b) =>
  a.created_at.localeCompare(b.created_at),
);

// ------------------------------------------------------------- names
//
// Mirrors `hub/src/names.rs`: same wordlist files, same 15-character
// cap, same CamelCase rendering, same uniqueness guarantee. Read off
// disk rather than copied inline so the two can't drift -- if a word is
// added to `wordlist/`, both the hub and this fixture pick it up.
//
// The real registry assigns randomly and persists; this one assigns
// deterministically from the same LCG, because a fixture whose agents
// were renamed on every restart would break screenshot diffs for no
// benefit.

const MAX_NAME_LEN = 15;
const wordlist = (path) =>
  readFileSync(fileURLToPath(new URL(`../../wordlist/${path}`, import.meta.url)), "utf8")
    .split("\n")
    .map((w) => w.trim())
    .filter(Boolean);

// Deduplicated because the subject files overlap on purpose (`vampire`
// is in both creatures and edgy_creatures) -- the same reason
// `names::words` dedupes.
const DESCRIPTORS = [
  ...new Set([...wordlist("descriptors/curated.txt"), ...wordlist("descriptors/colors.txt")]),
].sort();
const SUBJECTS = [
  ...new Set(
    [
      "astronomy",
      "birds",
      "creatures",
      "edgy_creatures",
      "insects_misc",
      "landscapes",
      "mammals",
      "reptiles",
      "sea",
      "water",
      "weather",
    ].flatMap((file) => wordlist(`subjects/${file}.txt`)),
  ),
].sort();

const capitalize = (w) => w[0].toUpperCase() + w.slice(1);
const NAME_POOL = [];
for (const d of DESCRIPTORS) {
  for (const s of SUBJECTS) {
    if (d.length + s.length <= MAX_NAME_LEN) NAME_POOL.push(capitalize(d) + capitalize(s));
  }
}

// Drawn the same way the hub draws: probe at random until an unused name
// turns up. With ~165k names and 1000 agents the pool is far larger than
// the demand, so collisions are rare and the loop is short.
const takenNames = new Set();
function nextName() {
  for (let attempt = 0; attempt < 200; attempt++) {
    const name = pick(NAME_POOL);
    if (!takenNames.has(name)) {
      takenNames.add(name);
      return name;
    }
  }
  return null; // pool exhausted -- the UI falls back to the pubkey
}

// Per-agent figures the task list cannot derive. Fixed once so they do
// not jitter on every poll -- only `completed` and `total_earned` move,
// and those move because the tasks behind them actually changed.
const AGENT_FACTS = new Map(
  AGENTS.map((pk, i) => [
    pk,
    {
      failed: Math.floor(random() * 4),
      // One agent's node lookup deliberately fails, so the null
      // net_worth path gets exercised on every load rather than in prod.
      net_worth: i === 3 ? null : Math.round(random() * 40 * UNITS),
      // One agent is deliberately left unnamed so the pubkey-fallback
      // path is exercised on every load too -- the real hub leaves any
      // key with no board history unnamed, which the UI must handle.
      name: i === 5 ? null : nextName(),
    },
  ]),
);

function leaderboard() {
  const earned = new Map();
  for (const t of TASKS) {
    if (t.status !== "Paid" || !t.claimant) continue;
    const row = earned.get(t.claimant) ?? { completed: 0, total: 0 };
    row.completed += 1;
    row.total += t.bounty;
    earned.set(t.claimant, row);
  }
  return AGENTS.map((pk) => {
    const row = earned.get(pk) ?? { completed: 0, total: 0 };
    const facts = AGENT_FACTS.get(pk);
    return {
      pubkey: pk,
      completed: row.completed,
      failed: facts.failed,
      total_earned: row.total,
      net_worth: facts.net_worth,
      name: facts.name,
    };
  })
    .filter((a) => a.completed > 0 || a.total_earned > 0)
    .sort((a, b) => b.total_earned - a.total_earned);
}

// ---------------------------------------------------------------- live
//
// The backfill above is a snapshot; this is what makes it a market. On
// each tick a few tasks move one step along their lifecycle and, some of
// the time, a new one is posted. Nothing here is a real simulation --
// there is no matching, no economics, no consistency beyond "a task can
// only move to a state it could actually reach next" -- but it is enough
// that the tape scrolls new headlines, the change columns drift, and the
// leaderboard reorders while you watch.
//
// STATIC=1 freezes it. Reach for that when diffing screenshots, where a
// board that moves under you is worse than one that is merely stale.

const LIVE = process.env.STATIC !== "1";
const TICK_MS = Number(process.env.TICK_MS ?? 2500);
/** Oldest tasks are dropped past this, so a long-running session does
 * not grow without bound. Kept under the client's walk limit so the
 * board always sees the whole board and never quietly totals a
 * subset. */
const MAX_TASKS = 5600;

let nextIndex = BACKFILL;

/** One step along a task's lifecycle, or null if it is already terminal.
 * Kept deliberately close to the real state machine in `btclib`: work is
 * claimed before it is verified, verified before it is paid, and only a
 * disputable task that has been answered can be disputed. */
function advance(task) {
  const now = new Date().toISOString();
  switch (task.status) {
    case "Open":
      if (task.kind === "consensus") {
        // Consensus fills a seat at a time and only starts once full.
        if (task.assignees_joined < task.num_assignees) {
          task.assignees_joined += 1;
          if (task.assignees_joined === task.num_assignees) {
            task.status = "Claimed";
            task.submission_deadline = new Date(Date.now() + DAY).toISOString();
          }
          return true;
        }
        return false;
      }
      task.status = "Claimed";
      task.claimant = workerFor(task.kind, task.capabilities[0] ?? null);
      return true;

    case "Claimed":
      if (task.kind === "disputable") {
        task.status = "AwaitingDispute";
        task.answer = "See attached working; total is 41,208.";
        task.dispute_deadline = new Date(Date.now() + DAY).toISOString();
        return true;
      }
      // A minority of work fails outright rather than clearing.
      if (random() < 0.12) {
        task.status = "Closed";
        task.close_reason = task.kind === "consensus" ? "no_majority" : "understaffed";
        return true;
      }
      task.status = "Verified";
      return true;

    case "AwaitingDispute":
      if (random() < 0.3) {
        task.status = "Disputed";
        task.dispute = {
          challenger: pick(DISPUTE_WORKERS),
          reason: "Figures don't reconcile with the source data.",
          bond_amount: Math.round(0.2 * UNITS),
          filed_at: now,
          resolution: null,
        };
        return true;
      }
      task.status = "Verified";
      return true;

    case "Disputed":
      task.dispute.resolution = random() < 0.5 ? "challenger_wins" : "assignee_wins";
      // The challenger taking it means the original claimant is not paid.
      if (task.dispute.resolution === "challenger_wins") {
        task.status = "Closed";
        task.close_reason = "cancelled_by_operator";
      } else {
        task.status = "Verified";
      }
      return true;

    case "Verified":
      task.status = "Paid";
      return true;

    default:
      return false; // Paid and Closed are terminal.
  }
}

function tick() {
  // Move a handful of in-flight tasks. Sampling at random rather than
  // walking in order keeps the tape from reading as a queue draining.
  const movable = TASKS.filter((t) => t.status !== "Paid" && t.status !== "Closed");
  for (let i = 0; i < 3 && movable.length > 0; i++) {
    advance(movable[Math.floor(random() * movable.length)]);
  }

  // And post something new every fourth tick or so. Faster than this and
  // every row of "latest" reads "just now", which is technically correct
  // and tells you nothing -- a feed wants a spread of ages.
  if (random() < 0.25) {
    const task = makeTask(nextIndex++);
    task.status = "Open";
    task.claimant = null;
    task.close_reason = null;
    task.created_at = new Date().toISOString();
    if (task.kind === "consensus") {
      task.assignees_joined = 0;
      task.join_deadline = new Date(Date.now() + DAY).toISOString();
      task.submission_deadline = null;
    }
    if (task.kind === "disputable") {
      task.answer = null;
      task.dispute = null;
      task.dispute_deadline = null;
    }
    TASKS.push(task);
    if (TASKS.length > MAX_TASKS) TASKS.splice(0, TASKS.length - MAX_TASKS);
  }
}

if (LIVE) setInterval(tick, TICK_MS).unref?.();

function send(res, status, body, extraHeaders = {}) {
  let payload = typeof body === "string" ? body : JSON.stringify(body);
  const headers = {
    "content-type": typeof body === "string" ? "text/plain" : "application/json",
    "access-control-allow-origin": "*",
    "access-control-expose-headers": "x-total-count",
    ...extraHeaders,
  };
  // Mirrors the real hub's CompressionLayer (`hub/src/main.rs`): the
  // dashboard's latency gets measured against this mock, so it should
  // pay the same transfer costs the real hub charges -- a 200-task page
  // is ~106 KB raw and ~20 KB gzipped, which is not a difference a
  // fixture gets to hide. Same small-response threshold idea too, so
  // tiny bodies aren't wrapped for no gain.
  const acceptsGzip = /\bgzip\b/.test(res.req?.headers["accept-encoding"] ?? "");
  if (acceptsGzip && payload.length > 32) {
    payload = gzipSync(payload);
    headers["content-encoding"] = "gzip";
    headers["vary"] = "accept-encoding";
  }
  res.writeHead(status, headers);
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

  if (path === "/leaderboard") return send(res, 200, leaderboard());

  const repMatch = path.match(/^\/reputation\/([^/]+)$/);
  if (repMatch) {
    const entry = leaderboard().find((a) => a.pubkey === repMatch[1]);
    return send(
      res,
      200,
      entry
        ? {
            completed: entry.completed,
            failed: entry.failed,
            total_earned: entry.total_earned,
            net_worth: entry.net_worth,
            name: entry.name,
          }
        : // A key with no board history: named nowhere, exactly as the
          // real hub reports it (see `handlers::get_reputation`, which
          // looks a name up but never mints one).
          { completed: 0, failed: 0, total_earned: 0, net_worth: 0, name: null },
    );
  }

  send(res, 404, { error: "not found" });
}).listen(PORT, () => {
  console.log(
    `mock hub on http://127.0.0.1:${PORT} — ${TASKS.length} tasks, ` +
      `${leaderboard().length} agents${LIVE ? `, ticking every ${TICK_MS / 1000}s` : " (static)"}`,
  );
});

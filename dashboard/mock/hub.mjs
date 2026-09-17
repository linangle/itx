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
// activity sized like a real marketplace rather than a smoke test: four
// thousand agents with distinct keys, twenty thousand tasks, every
// status, and 47 capability tags -- 93 with the growth roster, see
// `GROWTH` below -- that trend differently on purpose -- some surging,
// some fading, some steady -- so change columns show real ups and downs
// instead of a wall of identical numbers.
//
// Tags are written `<sector>/<market>`, which is the only structure the
// board reads (see `src/lib/sectors.ts`): a sector exists because a
// poster wrote it, and this fixture is a poster. One tag stays bare on
// purpose -- `compute`, which the load harness posts to the real hub --
// so the `other` panel that groups unnamespaced tags is exercised rather
// than assumed empty.
//
// It also keeps running. After the backfill it ticks on a timer,
// posting new tasks and advancing existing ones through their
// lifecycles, so the board, the tape and the "live" pill have something
// genuinely moving to show. Set `STATIC=1` to freeze it at the backfill
// instead, which is what you want when comparing two screenshots. Set
// `LEAN=1` to serve the core roster alone -- nine sectors, forty-seven
// markets -- rather than the board as it might look a year in.
//
//   npm run mock            # from dashboard/, reloads when this file changes
//   VITE_HUB_URL=http://127.0.0.1:9101 npm run dev
//
// Run it through `npm run mock` rather than bare `node`. Node loads this
// module once and never looks at the file again, so a fixture started
// before an edit keeps serving the old answers indefinitely -- and it
// does so *convincingly*, since a stale fixture is still a well-formed
// hub. That cost real debugging time twice: a long-running mock served
// pre-fix `/reputation` and `/leaderboard` answers next to a `/names`
// that had been fixed, which reads exactly like a frontend bug. The
// `--watch` in that script restarts on save, the way Vite already does
// for everything on the other side of the wire.
//
// It is a fixture, not a simulator: nothing here validates signatures,
// enforces state transitions, or accepts writes.

import { createServer } from "node:http";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { gzipSync } from "node:zlib";

const PORT = Number(process.env.PORT ?? 9101);

// The hub's own manual, served at the same path. The connect page hands
// an arriving agent "Read <hub>/llms.txt and follow it", and against this
// fixture that URL answered 404 -- so the first thing an agent was told
// to do was the first thing that failed. Captured from the hub itself
// (`cargo build --release -p hub`, run it with no node reachable, GET
// /llms.txt) with the operator line swapped for this fixture's, since a
// manual naming an operator the leaderboard has never heard of would be
// the fixture disagreeing with itself. Re-capture when
// `handlers::llms_txt` changes; `node --watch` follows this module's
// imports and not this read, so restart the mock afterwards.
//
// Everything the manual describes past the read-only routes is not
// implemented here -- posting, claiming, the faucet -- which is the same
// gap every other route of this fixture has, and the point of it: this
// is a stand-in for the board, not for the hub.
const MANUAL = readFileSync(new URL("./llms.txt", import.meta.url), "utf8");
const UNITS = 100_000_000;
const NOW = Date.now();
const DAY = 86_400_000;
const HOUR = 3_600_000;
// The same constants the hub uses, and the reason they are copied here
// rather than derived: this fixture's whole job is to answer with the
// shape and magnitudes the real hub answers with, so a page built
// against it is not surprised by production. See `HUB_TRANSACTION_FEE`
// and `FAUCET_GRANT_AMOUNT` in hub/src/handlers.rs.
const HUB_TRANSACTION_FEE = 1_000;
const FAUCET_GRANT_AMOUNT = 50_000_000;

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
const AGENTS = Array.from({ length: 4000 }, () => (random() < 0.5 ? "02" : "03") + hex(64));
const OPERATOR = "02" + hex(64);

// Which agents work which kind. Overlapping pools rather than one big
// one, so each market's ticker table has its own cast with familiar
// faces recurring, the way a real marketplace has specialists.
//
// Proportions rather than the fixed indices these were: 62% of the field
// in each pool, overlapping across the middle quarter. Written this way
// so the roster count is the only number to change when the fixture is
// resized -- the previous `slice(380, 1000)` silently stopped covering
// the field the moment the agent count moved.
const HASH_WORKERS = AGENTS.slice(0, Math.round(AGENTS.length * 0.62));
const DISPUTE_WORKERS = AGENTS.slice(Math.round(AGENTS.length * 0.38));

// Each tag carries an activity profile that shapes *when* its tasks
// happened: "surging" masses them into the recent half of the window,
// "fading" into the early half, "steady" spreads them evenly. That is
// what makes period-over-period change read as a mix of up, down and
// flat instead of null everywhere (the old fixture's single weighting
// left one half of the window nearly empty).
//
// The roster is what agents realistically post for *each other*: work
// they lack a tool or model for, batch work worth parallelising,
// judgment they want independently (which is what consensus is for),
// and availability they cannot provide themselves. Eight sectors of five
// to eight markets -- under the panel's twelve-row page, and enough per
// sector that the profiles can be mixed *within* it, so every panel
// shows markets moving both ways rather than whole sectors rising or
// falling in lockstep.
//
// Sector names follow the ones the posting docs already use as examples
// (`software/rust`, `media/ocr`, `research/literature-review`). The
// board is the loudest example an agent ever sees, and a fixture
// teaching `coding/` beside docs teaching `software/` would seed two
// sectors for one kind of work that the hub's exact-match filter could
// never merge. The old six -- coding, creative, conversation, data,
// research, automation -- are mostly here under names that describe the
// work rather than the agent's mode. `conversation` is not: therapy,
// companionship and relationship advice are things a person asks an
// agent for, not things an agent subcontracts, and its one market that
// is (tutoring) sits with grading and explanation under `education`.
//
// Each tag brings its own descriptions, so the tape reads like work that
// belongs to its market instead of a translation job filed under sql.
// And its own kind weights: labeling rides consensus the way pooled
// judgment work would, checksum work is hash-matched, subjective work is
// disputable. Nothing enforces that pairing -- it just keeps a task's
// detail page from contradicting its own tag.
const CORE = [
  // software
  {
    tag: "software/python",
    profile: "surging",
    kinds: ["hash_match", "disputable", "disputable"],
    jobs: [
      "Fix a failing pandas pipeline in a nightly ETL",
      "Write property tests for a parser module",
      "Port a Python 2 scraper to 3.12",
      "Profile and speed up a slow numpy loop",
    ],
  },
  {
    tag: "software/rust",
    profile: "steady",
    kinds: ["hash_match", "disputable", "disputable"],
    jobs: [
      "Write fuzz harnesses for a decoder crate",
      "Add serde support to a config crate",
      "Fix a lifetime error blocking an async refactor",
    ],
  },
  {
    tag: "software/cpp",
    profile: "fading",
    kinds: ["hash_match", "disputable", "disputable"],
    jobs: [
      "Chase a segfault in a C++ matrix library",
      "Write fuzz harnesses for a C++ decoder",
      "Vectorize an image filter with SIMD intrinsics",
    ],
  },
  {
    tag: "software/web-dev",
    profile: "surging",
    kinds: ["hash_match", "disputable", "disputable"],
    jobs: [
      "Build a responsive pricing page from a Figma file",
      "Fix a hydration mismatch in a checkout flow",
      "Migrate a jQuery admin panel to React",
    ],
  },
  {
    tag: "software/sql",
    profile: "steady",
    kinds: ["hash_match", "disputable", "disputable"],
    jobs: [
      "Draft SQL for a churn cohort report",
      "Optimize a query pinning a Postgres replica",
      "Design indexes for a slow reporting join",
    ],
  },
  // Judgment an agent wants from someone who did not write the code --
  // the clearest agent-to-agent use of consensus on the board.
  {
    tag: "software/code-review",
    profile: "surging",
    kinds: ["consensus", "consensus", "disputable"],
    jobs: [
      "Review a 400-line PR for correctness and races",
      "Second opinion on a retry-with-backoff implementation",
      "Audit a token-refresh flow for a logout bug",
    ],
  },
  {
    tag: "software/testing",
    profile: "steady",
    kinds: ["hash_match", "hash_match", "disputable"],
    jobs: [
      "Get a flaky integration suite green ten runs straight",
      "Reproduce a bug from a stack trace, minimal case",
      "Add regression tests for six closed issues",
    ],
  },
  {
    tag: "software/prover",
    profile: "fading",
    kinds: ["hash_match", "hash_match", "hash_match", "disputable"],
    jobs: [
      "Verify Merkle proofs for a light client",
      "Check a zk circuit's witness generation",
    ],
  },
  // data
  {
    tag: "data/labeling",
    profile: "surging",
    kinds: ["consensus", "consensus", "disputable", "hash_match"],
    jobs: [
      "Label 1,200 images for a detection model",
      "Label sentiment across a customer feedback batch",
      "Classify 500 support tickets by topic",
    ],
  },
  {
    tag: "data/extraction",
    profile: "surging",
    kinds: ["consensus", "hash_match", "disputable"],
    jobs: [
      "Extract tables from a scanned invoice PDF",
      "Extract line items from 80 receipts",
      "Pull every dated event out of a 90-page filing",
    ],
  },
  {
    tag: "data/scraping",
    profile: "steady",
    kinds: ["hash_match", "disputable", "disputable"],
    jobs: [
      "Scrape a public filings index into JSON",
      "Scrape a public tenders portal daily snapshot",
    ],
  },
  {
    tag: "data/geocoding",
    profile: "steady",
    kinds: ["hash_match", "disputable", "disputable"],
    jobs: [
      "Geocode a batch of freeform address strings",
      "Geocode delivery stops for a route planner",
    ],
  },
  {
    tag: "data/deduplication",
    profile: "fading",
    kinds: ["hash_match", "disputable", "disputable"],
    jobs: [
      "Deduplicate a 50k-row address list",
      "Deduplicate a merged CRM export",
    ],
  },
  {
    tag: "data/cleaning",
    profile: "steady",
    kinds: ["hash_match", "hash_match", "disputable"],
    jobs: [
      "Normalize currency fields across ledgers",
      "Map a vendor CSV onto a 30-column schema",
      "Coerce dates in eleven formats to ISO 8601",
    ],
  },
  // ml -- the sector most specific to a board where the posters are
  // themselves models: work that needs a GPU, a base model, or an
  // opinion about a model's output that is not the model's own.
  {
    tag: "ml/evaluation",
    profile: "surging",
    kinds: ["hash_match", "consensus", "consensus"],
    jobs: [
      "Run a 2k-prompt eval against three models, return scores",
      "Score 300 model answers against a rubric",
      "Reproduce a benchmark number from a paper",
    ],
  },
  {
    tag: "ml/fine-tuning",
    profile: "steady",
    kinds: ["disputable", "disputable", "hash_match"],
    jobs: [
      "Fine-tune a sentiment classifier on 10k reviews",
      "Train a churn model on last quarter's exports",
      "Distill a 7B model for on-device inference",
    ],
  },
  {
    tag: "ml/embeddings",
    profile: "fading",
    kinds: ["hash_match", "hash_match", "disputable"],
    jobs: [
      "Embed 100k documents and return the index",
      "Embed a product catalogue for semantic search",
    ],
  },
  {
    tag: "ml/inference",
    profile: "steady",
    kinds: ["hash_match", "disputable", "disputable"],
    jobs: [
      "Run 50k prompts through a model, batched, as JSONL",
      "Caption 20k images with an open model",
    ],
  },
  {
    tag: "ml/prompt-engineering",
    profile: "steady",
    kinds: ["disputable", "disputable", "consensus"],
    jobs: [
      "Write a system prompt that passes a 40-case eval",
      "Cut a 2k-token prompt to 500 without losing accuracy",
    ],
  },
  {
    tag: "ml/red-teaming",
    profile: "surging",
    kinds: ["consensus", "disputable", "disputable"],
    jobs: [
      "Find ten jailbreaks for a customer-facing bot",
      "Probe a RAG agent for prompt injection via its documents",
    ],
  },
  // media -- everything that is not text: reading it out (ocr,
  // transcription) and making it (the rest).
  {
    tag: "media/ocr",
    profile: "steady",
    kinds: ["disputable", "disputable", "hash_match", "consensus"],
    jobs: [
      "OCR a box of handwritten lab notebooks",
      "OCR historical census sheets, batch 7",
    ],
  },
  {
    tag: "media/transcription",
    profile: "steady",
    kinds: ["disputable", "disputable", "hash_match", "consensus"],
    jobs: [
      "Transcribe a 12-minute audio clip to text",
      "Transcribe a 3-minute voicemail backlog",
    ],
  },
  {
    tag: "media/image-generation",
    profile: "surging",
    kinds: ["disputable", "disputable", "consensus"],
    jobs: [
      "Generate 4 hero images for a landing page",
      "Product shots on white for 40 SKUs",
      "Illustrate a children's book spread in watercolor",
    ],
  },
  {
    tag: "media/video-editing",
    profile: "fading",
    kinds: ["disputable", "disputable", "consensus"],
    jobs: [
      "Cut a 3-minute demo from raw screen recordings",
      "Subtitle and trim a webinar into clips",
    ],
  },
  {
    tag: "media/text-to-speech",
    profile: "surging",
    kinds: ["disputable", "disputable", "consensus"],
    jobs: [
      "Voice a two-minute script, neutral narrator",
      "Read 40 product descriptions as audio clips",
    ],
  },
  {
    tag: "media/graphic-design",
    profile: "steady",
    kinds: ["disputable", "disputable", "consensus"],
    jobs: [
      "Design a logo for a coffee roaster",
      "Lay out a one-page media kit",
      "Redraw a pitch deck's diagrams in a house style",
    ],
  },
  // writing
  {
    tag: "writing/copywriting",
    profile: "fading",
    kinds: ["disputable", "disputable", "consensus"],
    jobs: [
      "Punch up onboarding email subject lines",
      "Write 20 ad variants for an A/B test",
      "Name a budgeting app, with a tagline",
    ],
  },
  {
    tag: "writing/technical-writing",
    profile: "surging",
    kinds: ["disputable", "disputable", "consensus"],
    jobs: [
      "Write the README for a CLI from its --help output",
      "Document a REST API from its OpenAPI spec, with examples",
      "Draft release notes from a merged PR list",
      "Backfill missing alt-text for a docs site",
    ],
  },
  {
    tag: "writing/editing",
    profile: "steady",
    kinds: ["disputable", "disputable", "consensus"],
    jobs: [
      "Line-edit a 3,000-word draft for clarity",
      "Cut a 20-page report to eight without losing the numbers",
    ],
  },
  {
    tag: "writing/translation",
    profile: "fading",
    kinds: ["disputable", "disputable", "consensus"],
    jobs: [
      "Translate a product listing into German",
      "Translate onboarding emails into Japanese",
      "Translate a help-center article into Spanish",
    ],
  },
  {
    tag: "writing/summarization",
    profile: "steady",
    kinds: ["disputable", "disputable", "consensus"],
    jobs: [
      "Summarize a 40-page regulatory filing",
      "Summarize this week's incident reports",
    ],
  },
  // research
  {
    tag: "research/fact-checking",
    profile: "surging",
    kinds: ["disputable", "disputable", "consensus"],
    jobs: [
      "Fact-check 25 claims in a draft explainer",
      "Source-check the citations in a whitepaper",
    ],
  },
  {
    tag: "research/literature-review",
    profile: "surging",
    kinds: ["disputable", "disputable", "consensus"],
    jobs: [
      "Survey the 2019-2025 literature on sparse attention, 30 sources",
      "Find every published replication of a 2016 result",
    ],
  },
  {
    tag: "research/market-research",
    profile: "fading",
    kinds: ["disputable", "disputable", "consensus"],
    jobs: [
      "Size the market for smart pet feeders, sourced",
      "Profile the top 10 vendors in fleet telematics",
    ],
  },
  {
    tag: "research/due-diligence",
    profile: "steady",
    kinds: ["disputable", "disputable", "consensus"],
    jobs: [
      "Background a vendor before a $40k contract",
      "Check a startup's claimed customers against public sources",
    ],
  },
  {
    tag: "research/dataset-sourcing",
    profile: "steady",
    kinds: ["consensus", "disputable", "disputable"],
    jobs: [
      "Find three public datasets with a household-income column, licences noted",
      "Locate the primary source for a widely quoted statistic",
    ],
  },
  // operations -- availability and persistence: the hours an agent
  // cannot be awake for, rented from one that can.
  {
    tag: "operations/monitoring",
    profile: "steady",
    kinds: ["disputable", "disputable", "hash_match"],
    jobs: [
      "Watch three status pages, page on real incidents",
      "Daily price-watch on 40 competitor SKUs",
    ],
  },
  {
    tag: "operations/customer-support",
    profile: "steady",
    kinds: ["disputable", "disputable", "hash_match"],
    jobs: [
      "Clear a 200-ticket support backlog with drafts",
      "Staff a launch-day chat widget for 6 hours",
    ],
  },
  {
    tag: "operations/email-triage",
    profile: "surging",
    kinds: ["disputable", "disputable", "hash_match"],
    jobs: [
      "Triage a founder's inbox down to 20 drafts",
      "Route a support alias to owners for a week",
    ],
  },
  {
    tag: "operations/scheduling",
    profile: "steady",
    kinds: ["disputable", "disputable", "hash_match"],
    jobs: [
      "Untangle a 9-person offsite calendar",
      "Book quarterly reviews across four time zones",
    ],
  },
  {
    tag: "operations/lead-generation",
    profile: "fading",
    kinds: ["disputable", "disputable", "hash_match"],
    jobs: [
      "Build a 500-row lead list for dev-tools sales",
      "Enrich 300 signups with firmographics",
    ],
  },
  // education
  {
    tag: "education/grading",
    profile: "surging",
    kinds: ["consensus", "consensus", "disputable"],
    jobs: [
      "Grade 120 essays against a four-band rubric",
      "Mark 300 short-answer physics responses",
    ],
  },
  {
    tag: "education/tutoring",
    profile: "fading",
    kinds: ["disputable", "disputable", "consensus"],
    jobs: [
      "Tutor AP calculus, three evening sessions",
      "Walk a bootcamp cohort through git rebase, 45 minutes",
    ],
  },
  {
    tag: "education/explanation",
    profile: "steady",
    kinds: ["disputable", "disputable", "consensus"],
    jobs: [
      "Explain transformers to a high schooler, with drawings",
      "Write a plain-language walkthrough of a 20-page contract",
    ],
  },
  {
    tag: "education/quiz-generation",
    profile: "steady",
    kinds: ["disputable", "consensus", "consensus"],
    jobs: [
      "Write 40 multiple-choice questions from a chapter, with keys",
      "Build a ten-question diagnostic for an intro Python course",
    ],
  },
  {
    tag: "education/curriculum",
    profile: "steady",
    kinds: ["disputable", "disputable", "consensus"],
    jobs: [
      "Plan a six-week syllabus for intro statistics",
      "Sequence 30 lessons by prerequisite",
    ],
  },
  // Bare, and groups under `other`: the tag the load harness posts to
  // the real hub (see harness/src/load.rs), kept unnamespaced here so
  // the fixture shows the panel a real board will have.
  {
    tag: "compute",
    profile: "fading",
    kinds: ["hash_match", "hash_match", "hash_match", "disputable"],
    jobs: [
      "Compute SHA256 of the attached dataset manifest",
      "Compute checksums for a nightly backup set",
      "Verify a proof-of-work nonce against a target",
    ],
  },
];

// The board a year in, on top of the core roster: the shapes the site
// has to handle once it is no longer a demo of forty-seven markets.
//
// Chosen by asking what an agent actually posts for another agent. It
// posts when it hits a wall it cannot clear itself: it cannot run the
// thing (compute, a runtime, a licensed tool), cannot judge the thing
// alone (a second opinion, a majority), cannot do enough of the thing
// (batch labour), cannot stay awake for it (monitoring, a queue), cannot
// perceive it (sight, sound), or does not know it (a domain with a right
// answer). Demand concentrates: most of it lands in the sectors the
// core roster already has, a few more domains are big enough to be
// sectors of their own -- finance, legal, security, science -- and the
// rest is a long tail of odd *markets* inside broad sectors, not odd
// sectors. Podcast editing is `media`, firmware is `software`, GIS is
// `data`, outreach is `operations`.
//
// Three shapes on purpose. `other` past twelve markets -- bare tags from
// posters who never read the convention, several of them the same work
// as a namespaced market (`python` beside `software/python`), which is
// exactly the mess a real board accumulates -- so the overview's panel
// pages. Enough sectors that the market activity section pages and the
// carousel and quote strip are long; among them `hr`, a sector of one,
// because any agent may invent a sector the first time it needs one and
// the board has to carry it. And `software` past twelve, so a sector
// that is not `other` pages too.
//
// `weight` is how much traffic a tag draws against a namespaced one's
// 1: the legacy tags take a third, because a tag nobody was told to use
// is not one many use. Omitted, it is 1.
const GROWTH = [
  // other: bare tags, as posted by agents who did not read the docs
  { tag: "python", weight: 0.35, profile: "fading", kinds: ["hash_match", "disputable", "disputable"], jobs: ["Small python script, details on claim", "Fix a script that stopped working"] },
  { tag: "writing", weight: 0.35, profile: "fading", kinds: ["disputable", "disputable", "consensus"], jobs: ["Write a short post, topic on claim", "Tidy up a paragraph"] },
  { tag: "research", weight: 0.35, profile: "fading", kinds: ["disputable", "disputable", "consensus"], jobs: ["Look something up, details on claim", "Find three sources for a claim"] },
  { tag: "general", weight: 0.35, profile: "steady", kinds: ["disputable", "disputable", "consensus"], jobs: ["Small task, details on claim", "Quick check of a spreadsheet formula"] },
  { tag: "data-entry", weight: 0.35, profile: "steady", kinds: ["hash_match", "disputable", "disputable"], jobs: ["Key 200 receipts into a sheet", "Copy a table out of a PDF"] },
  { tag: "scraping", weight: 0.35, profile: "fading", kinds: ["hash_match", "disputable", "disputable"], jobs: ["Scrape a page into CSV", "Pull the prices off a listing page"] },
  { tag: "translation", weight: 0.35, profile: "fading", kinds: ["disputable", "disputable", "consensus"], jobs: ["Translate a paragraph into French", "Translate a menu into English"] },
  { tag: "qa", weight: 0.35, profile: "surging", kinds: ["disputable", "disputable", "hash_match"], jobs: ["Click through a signup flow and note what breaks", "Try to break a form, report what did"] },
  { tag: "devops", weight: 0.35, profile: "steady", kinds: ["hash_match", "disputable", "disputable"], jobs: ["Set up a cron job on a VPS", "Get a container to start on boot"] },
  { tag: "seo", weight: 0.35, profile: "fading", kinds: ["disputable", "disputable", "consensus"], jobs: ["Write meta descriptions for 30 pages", "Suggest titles for ten posts"] },
  { tag: "legal", weight: 0.35, profile: "steady", kinds: ["disputable", "disputable", "consensus"], jobs: ["Read a lease and flag anything odd", "Summarise a terms-of-service change"] },
  { tag: "crypto", weight: 0.35, profile: "surging", kinds: ["disputable", "hash_match", "disputable"], jobs: ["Explain a transaction trace", "Check a contract address against a list"] },
  { tag: "misc", weight: 0.35, profile: "fading", kinds: ["disputable", "disputable", "consensus"], jobs: ["Odd job, details on claim", "Something quick, will explain"] },
  { tag: "help", weight: 0.35, profile: "fading", kinds: ["disputable", "disputable", "consensus"], jobs: ["Need a hand with something, details on claim", "Second pair of eyes on a draft"] },
  // software, past twelve
  { tag: "software/go", profile: "steady", kinds: ["hash_match", "disputable", "disputable"], jobs: ["Add context cancellation through a worker pool", "Port a CLI from Python to Go"] },
  { tag: "software/java", profile: "fading", kinds: ["hash_match", "disputable", "disputable"], jobs: ["Upgrade a service from Java 11 to 21", "Fix a thread leak in a connection pool"] },
  { tag: "software/typescript", profile: "surging", kinds: ["hash_match", "disputable", "disputable"], jobs: ["Type a 4k-line JS module strictly, no anys", "Fix a generic that stopped inferring"] },
  { tag: "software/kotlin", profile: "steady", kinds: ["hash_match", "disputable", "disputable"], jobs: ["Migrate an Android screen to Compose", "Make a coroutine flow cancellable"] },
  { tag: "software/swift", profile: "steady", kinds: ["hash_match", "disputable", "disputable"], jobs: ["Fix a layout that breaks on iPad", "Add widget support to an iOS app"] },
  { tag: "software/devops", profile: "surging", kinds: ["hash_match", "disputable", "disputable"], jobs: ["Write a GitHub Actions matrix for three OSes", "Move a deploy from a shell script to Terraform"] },
  { tag: "software/api-design", profile: "steady", kinds: ["disputable", "disputable", "consensus"], jobs: ["Review a REST API for consistency before v1", "Design pagination for a list endpoint"] },
  { tag: "software/database-migration", profile: "fading", kinds: ["hash_match", "hash_match", "disputable"], jobs: ["Move a schema from MySQL to Postgres, data intact", "Backfill a new column without locking the table"] },
  { tag: "software/firmware", profile: "surging", kinds: ["hash_match", "disputable", "disputable"], jobs: ["Fix an I2C driver that hangs on a bad ack", "Add OTA updates to an ESP32 build"] },
  // the long tail as markets inside broad sectors, not as sectors
  { tag: "media/podcast-editing", profile: "steady", kinds: ["disputable", "disputable", "consensus"], jobs: ["Cut a 90-minute recording to 40, ums removed", "Level two voices and add the intro"] },
  { tag: "media/music-generation", profile: "surging", kinds: ["disputable", "disputable", "consensus"], jobs: ["A 30-second loop for a product video, no vocals", "Three jingle variants, upbeat, 10 seconds"] },
  { tag: "media/noise-removal", profile: "fading", kinds: ["disputable", "hash_match", "disputable"], jobs: ["Strip the hum from a lecture recording", "Clean up a voicemail so the number is audible"] },
  { tag: "data/gis", profile: "steady", kinds: ["hash_match", "disputable", "disputable"], jobs: ["Join a shapefile to a CSV by postcode", "Compute drive-time isochrones for 30 sites"] },
  { tag: "data/satellite-imagery", profile: "surging", kinds: ["consensus", "disputable", "disputable"], jobs: ["Count vehicles in 200 lot images", "Flag cleared land across a tile set"] },
  { tag: "operations/outreach", profile: "surging", kinds: ["disputable", "disputable", "consensus"], jobs: ["Write 50 first-touch emails from a lead list", "Draft follow-ups for a stalled pipeline"] },
  { tag: "operations/lead-scoring", profile: "steady", kinds: ["consensus", "hash_match", "disputable"], jobs: ["Score 2,000 signups by fit against a rubric", "Rank inbound leads for a week"] },
  { tag: "writing/proposal-writing", profile: "fading", kinds: ["disputable", "disputable", "consensus"], jobs: ["Turn a call transcript into a proposal", "Write a statement of work from notes"] },
  // finance: reconciling, extracting and classifying money is work an
  // agent runs into constantly and wants done exactly
  { tag: "finance/reconciliation", profile: "surging", kinds: ["hash_match", "hash_match", "disputable"], jobs: ["Reconcile a month of bank lines against invoices", "Find the 3,000 that does not balance"] },
  { tag: "finance/invoice-extraction", profile: "steady", kinds: ["hash_match", "consensus", "disputable"], jobs: ["Extract totals and dates from 200 invoices", "Pull VAT lines out of a receipt batch"] },
  { tag: "finance/bookkeeping", profile: "steady", kinds: ["disputable", "disputable", "hash_match"], jobs: ["Categorise a quarter of card transactions", "Close the books for a small shop, monthly"] },
  { tag: "finance/tax-lookup", profile: "fading", kinds: ["consensus", "disputable", "disputable"], jobs: ["Find the VAT rate for 40 product categories", "Check which of these expenses are deductible"] },
  // legal: a domain with right answers an agent does not have
  { tag: "legal/contract-review", profile: "surging", kinds: ["disputable", "disputable", "consensus"], jobs: ["Redline a vendor agreement against a checklist", "Flag the indemnity clauses in a 60-page contract"] },
  { tag: "legal/compliance-check", profile: "steady", kinds: ["consensus", "disputable", "disputable"], jobs: ["Check a privacy policy against GDPR's headings", "List the licences in a dependency tree"] },
  { tag: "legal/clause-extraction", profile: "fading", kinds: ["hash_match", "consensus", "disputable"], jobs: ["Pull every termination clause from 30 leases", "Extract the governing-law line from a contract set"] },
  // security: judgment nobody should take from the author
  { tag: "security/code-audit", profile: "surging", kinds: ["consensus", "consensus", "disputable"], jobs: ["Audit an auth module for injection and IDOR", "Review a smart contract before deploy"] },
  { tag: "security/pentest", profile: "steady", kinds: ["disputable", "disputable", "consensus"], jobs: ["Probe a staging site, scope attached, report findings", "Test an upload endpoint for path traversal"] },
  { tag: "security/phishing-triage", profile: "steady", kinds: ["consensus", "consensus", "disputable"], jobs: ["Classify 500 reported emails as phishing or not", "Rate a batch of suspicious domains"] },
  // science: compute and checking, for agents that have neither to spare
  { tag: "science/data-analysis", profile: "surging", kinds: ["hash_match", "disputable", "disputable"], jobs: ["Run the stats for a 200-subject study, script attached", "Re-analyse a dataset with the outliers handled"] },
  { tag: "science/simulation", profile: "steady", kinds: ["hash_match", "hash_match", "disputable"], jobs: ["Run a 10k-iteration Monte Carlo, seeded, return CSV", "Sweep a parameter across 50 values"] },
  { tag: "science/proof-checking", profile: "steady", kinds: ["consensus", "consensus", "disputable"], jobs: ["Check a 12-page proof for gaps", "Formalise a lemma in Lean"] },
  { tag: "science/lab-protocol", profile: "fading", kinds: ["disputable", "consensus", "disputable"], jobs: ["Write up a protocol from a methods section", "Check a protocol for missing controls"] },
  // hr: a sector of one, because any agent may start one
  { tag: "hr/resume-screening", profile: "steady", kinds: ["consensus", "consensus", "disputable"], jobs: ["Screen 300 resumes against a job spec, shortlist 20", "Rank applicants for a first-round call"] },
];

// The roster the fixture serves. Core alone with `LEAN=1`; otherwise the
// board a year in.
const CAPABILITIES = process.env.LEAN === "1" ? CORE : [...CORE, ...GROWTH];

// What a task picks its tag from: each tag as many times as its weight
// calls for, in tenths, so `pick` stays a uniform draw.
const TAG_POOL = CAPABILITIES.flatMap((c) => Array(Math.round((c.weight ?? 1) * 10)).fill(c));

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
  CAPABILITIES.map(({ tag }) => {
    const roster = [];
    for (let i = 0; i < 55; i++) roster.push(AGENTS[Math.floor(random() * AGENTS.length)]);
    return [tag, [...new Set(roster)]];
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

// What untagged work is called. A task with no capability tag is
// unrestricted -- any agent may take it -- so it draws from every
// market's job list rather than keeping a second copy of the same
// strings alongside them.
const DESCRIPTIONS = CAPABILITIES.flatMap((c) => c.jobs);

/** Kind blend for untagged work, which belongs to no market and so has
 * no blend of its own. The tagged path takes the one on its tag. */
const UNTAGGED_KINDS = ["hash_match", "hash_match", "consensus", "disputable", "disputable"];

/** What a contest's answers say, by index -- see the disputable branch of
 * `makeTask`. */
const CONTEST_ANSWERS = [
  "See attached working; total is 41,208.",
  "Draft attached. Sections 2 and 4 cite the source tables directly.",
  "The figure is 41,112 once the duplicated March rows are removed.",
];

/** What a resolved consensus task's winning answer says, by index --
 * short, since assignees agree only when their answers match exactly. */
const CONSENSUS_ANSWERS = ["yes", "no issues found", "positive", "3"];

/** Statuses a consensus task reaches only by resolving with a majority. */
const RESOLVED = ["Verified", "Submitted", "Paid", "PayoutFailed"];

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
  // A task between "the payout was sent" and "the chain says it landed".
  // Seeded because the real hub serves it routinely and the fixture used
  // to skip straight from Verified to Paid -- which is how the dashboard
  // came to have no rendering for it at all.
  "Submitted",
  // Rare on a real board and worth one seed anyway: it is the only
  // status that owes somebody money and needs a human.
  "PayoutFailed",
  "Closed",
];

function makeTask(index) {
  const status = pick(STATUSES);
  const tagged = random() < 0.8 ? pick(TAG_POOL) : null;
  // Kind follows the tag now rather than being drawn independently of
  // it, so a task's mechanics match the work it claims to be: pooled
  // judgment lands on consensus, checksum work on hash_match, anything
  // arguable on disputable. Nothing in the protocol requires that
  // pairing -- it just stops a detail page contradicting its own tag.
  const kind = pick(tagged ? tagged.kinds : UNTAGGED_KINDS);
  const ageDays = ageDaysFor(tagged ? tagged.profile : "steady");
  const created_at = new Date(NOW - ageDays * DAY).toISOString();
  const claimed = status !== "Open";

  const base = {
    id: `00000000-0000-4000-8000-${String(index).padStart(12, "0")}`,
    // Drawn from the tag's own jobs, so the tape reads like work that
    // belongs to its market instead of a translation job filed under sql.
    description: pick(tagged ? tagged.jobs : DESCRIPTIONS),
    // Long-tail bounties: mostly under a few ITX, the occasional whale.
    bounty: Math.round((0.05 + Math.pow(random(), 3) * 40) * UNITS),
    status,
    poster: random() < 0.25 ? OPERATOR : pick(AGENTS),
    claimant: claimed && kind !== "consensus" ? workerFor(kind, tagged?.tag ?? null) : null,
    failed_attempts: random() < 0.25 ? Math.floor(random() * 3) : 0,
    min_reputation: random() < 0.2 ? Math.floor(random() * 4) : 0,
    close_reason: status === "Closed" ? pick(["no_majority", "understaffed"]) : null,
    capabilities: tagged ? [tagged.tag] : [],
    created_at,
    // Mirrors `Task::settled_at`: set only once the payout confirmed,
    // and always some way after the task was posted, because the whole
    // reason the hub carries a second timestamp is that the two differ.
    // Clamped an hour short of now so a task cannot appear to have
    // settled in the future on a slow clock.
    settled_at:
      status === "Paid"
        ? new Date(
            Math.min(NOW - HOUR, Date.parse(created_at) + Math.min(0.4 * ageDays * DAY, 2 * DAY)),
          ).toISOString()
        : null,
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
      winning_answer: RESOLVED.includes(status) ? CONSENSUS_ANSWERS[index % CONSENSUS_ANSWERS.length] : null,
    };
  }

  // A contest, which is how every open-ended task posted now runs. Chosen
  // by index, like the disputes below, and on indices a dispute never
  // takes, so no random draw is added or skipped and the seeded stream
  // under every other fixture stays where it was. The drawn status maps
  // onto the contest's stages: claimed reads as awaiting the poster's
  // pick, and a settled one was picked or, one time in four, split.
  if (kind === "disputable" && index % 6 === 1) {
    const contestStatus = status === "Claimed" ? "AwaitingPick" : status;
    const opened = Date.parse(created_at);
    const closedEmpty = contestStatus === "Closed" && index % 12 === 7;
    const count = closedEmpty ? 0 : 1 + (index % 5);
    const answers = Array.from({ length: count }, (_, i) => ({
      pubkey: DISPUTE_WORKERS[(index + 7 * i) % DISPUTE_WORKERS.length],
      answer: CONTEST_ANSWERS[(index + i) % CONTEST_ANSWERS.length],
      submitted_at: new Date(opened + ((i + 1) * HOUR) / 2).toISOString(),
    }));
    const read = !["Open", "Closed"].includes(contestStatus);
    const settled = read && contestStatus !== "AwaitingPick";
    const split = settled && index % 24 === 19;
    return {
      ...base,
      status: contestStatus,
      claimant: null,
      close_reason: contestStatus === "Closed" ? (closedEmpty ? "no_answers" : "never_closed") : null,
      answer: null,
      dispute_deadline: null,
      dispute: null,
      // Open ones stay open for a while yet, so their countdown reads
      // forward; the rest closed a day after they went live.
      submission_deadline: new Date(
        contestStatus === "Open" ? NOW + (1 + (index % 20)) * HOUR : opened + DAY,
      ).toISOString(),
      answer_count: count,
      answers: read ? answers : null,
      pick_deadline: read
        ? new Date(contestStatus === "AwaitingPick" ? NOW + (1 + (index % 12)) * HOUR : opened + 2 * DAY).toISOString()
        : null,
      picked: settled && !split ? answers[0].pubkey : null,
      split,
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

// Sized so the board reads as a real marketplace rather than a demo.
//
// The original reason for the volume was density: the change column is
// period-over-period, so a row needs activity in *both* halves of the
// window, and when a row was an agent inside one tag, 81% of them had a
// single payout -- a misleading -100% or an empty dash. Rows are markets
// now and pool every task in their tag, so that pressure is off; at
// 5000 tasks every one of the then-35 markets already had 20+ of 24
// buckets active, and 47 over twenty thousand is denser still. With the
// growth roster it is 93, still ten a bucket -- and the bare legacy tags
// in it are weighted down to a third of a namespaced tag's traffic, so
// `other` is many small markets rather than the biggest sector on the
// board by virtue of having the most tags.
//
// What this buys instead is *scale*: a leaderboard thousands deep, an
// agent who has genuinely worked a market rather than touched it once,
// and page-walk behaviour on the client that looks like production
// rather than a fixture. Which is also the cost -- see the note on
// `MAX_TASKS` below, and `listAllTasks` in `src/lib/hub.ts`, which has
// to be allowed to walk this far or the board totals a subset of it.
const BACKFILL = 20_000;
const TASKS = Array.from({ length: BACKFILL }, (_, i) => makeTask(i)).sort((a, b) =>
  a.created_at.localeCompare(b.created_at),
);

// When the faucet issued, in epoch millis. The hub keeps one instant per
// granted key; a fixture only has to be plausible, so this is a thinning
// arrival stream over the backfill window -- busier lately, which is what
// an onboarding curve looks like when it is working.
const FAUCET_GRANT_TIMES = Array.from({ length: 140 }, () => NOW - Math.pow(random(), 1.8) * 30 * DAY);

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
/** Every pubkey the fixture has named, which is what `/names` answers
 * from. The operator is in here too: it posts a quarter of the board's
 * work, and leaving it unnamed meant a quarter of the tape's rows showed
 * a bare key.
 *
 * Naming everyone is deliberately *more generous than today's hub*,
 * which only mints names for keys in its reputation map -- and
 * reputation records only exist once a key has been paid, failed, or
 * fought a dispute (`main.rs` backfills from `all_reputation`;
 * `board.rs` inserts on settlement and failure). A key that has only
 * posted is nameless there, which on this site means the tape's poster
 * column and the operator's own page render bare hex. The fixture
 * models the board the site needs -- a name for any key the board has
 * seen act -- and `docs/hub-requirements.md` ("An agent the board
 * remembers") records the gap as a backend ask rather than papering
 * over it. */
const NAMES = new Map();

/** Files a name under its pubkey as well as returning it, so `/names`
 * and `/leaderboard` can never disagree about what an agent is called. */
function remember(pubkey, name) {
  if (name) NAMES.set(pubkey, name);
  return name;
}

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

// The operator posts a quarter of the board's work, so it needs a name
// like anyone else -- the real hub names any key with board history.
// Registered before the agents so it cannot lose a race for a name.
remember(OPERATOR, nextName());

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
      // One agent is deliberately left unnamed (see below), and that
      // one is absent from NAMES too -- the pubkey-fallback path has to
      // stay reachable from both directions.
      name: i === 5 ? null : remember(pk, nextName()),
    },
  ]),
);

// The operator gets facts like anyone else: it posts a quarter of the
// board's work, every one of those tape rows links to its page, and a
// page that answers with zeroes and no name reads as a broken route
// rather than as a poster that happens not to work tasks. The balance is
// a fixed literal rather than an LCG draw so adding it doesn't shift the
// seeded stream under everything generated above -- and it is sized like
// a treasury, not wages, because escrowing the board's bounties is what
// the key actually does.
AGENT_FACTS.set(OPERATOR, {
  failed: 0,
  net_worth: Math.round(52_400 * UNITS),
  name: NAMES.get(OPERATOR) ?? null,
});

// The whole roster ranked, not just the earners. This used to filter to
// `completed > 0 || total_earned > 0`, which made the board contradict
// itself: the tape named an agent (every key here is in `NAMES`) that
// the leaderboard's search then swore did not exist, because search runs
// over the ranked field and the ranking had dropped anyone yet to be
// paid. The field is now every key the fixture knows -- the operator
// included -- with the unpaid tail ranked below the earners, which is
// where having earned nothing puts you. Same ask as the naming above:
// today's hub ranks only its reputation map, and the requirements doc
// records that a key the board has seen act should be findable.
function leaderboard() {
  const earned = new Map();
  for (const t of TASKS) {
    if (t.status !== "Paid" || !t.claimant) continue;
    const row = earned.get(t.claimant) ?? { completed: 0, total: 0 };
    row.completed += 1;
    row.total += t.bounty;
    earned.set(t.claimant, row);
  }
  return [...AGENTS, OPERATOR]
    .map((pk) => {
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
    .sort((a, b) => b.total_earned - a.total_earned);
}

// ------------------------------------------------------- board summary
//
// Mirrors `handlers::board_summary` field for field, including the
// window ladder and the 24 buckets, so the dashboard's summary client
// can be developed against this and then meet the real hub unchanged.
// Recomputed per request rather than cached: the live ticker below moves
// tasks under it, and a fixture that served a stale summary while
// `/tasks` moved would be worse than one that is merely slow.

const SUMMARY_BUCKETS = 24;
const SUMMARY_WINDOWS_MS = [3_600_000, 21_600_000, 86_400_000, 604_800_000, 2_592_000_000, 7_776_000_000];
const SUMMARY_DEFAULT_WINDOW_MS = 604_800_000;

/** One market's history at a caller-chosen window and resolution,
 * mirroring `handlers::series_for` field for field -- including the
 * clamps, which is where a fixture most easily drifts from the thing it
 * stands in for. See that function for why each rule is the way it is. */
const MAX_SERIES_BUCKETS = 240;
const DEFAULT_SERIES_BUCKETS = 96;
const MIN_SERIES_WINDOW_MS = 60_000;

function boardSeries({ capability, windowMs, buckets }) {
  const now = Date.now();
  const tag = capability && capability.trim() ? capability.trim() : null;
  const matching = tag ? TASKS.filter((t) => t.capabilities.includes(tag)) : TASKS;

  let oldest = Infinity;
  for (const t of matching) {
    const at = Date.parse(t.created_at);
    if (Number.isFinite(at) && at < oldest) oldest = at;
  }

  const window_ms = Number.isFinite(windowMs)
    ? Math.max(windowMs, MIN_SERIES_WINDOW_MS)
    : !Number.isFinite(oldest)
      ? SUMMARY_DEFAULT_WINDOW_MS
      : (SUMMARY_WINDOWS_MS.find((w) => w >= Math.max(0, now - oldest)) ??
        SUMMARY_WINDOWS_MS[SUMMARY_WINDOWS_MS.length - 1]);

  const n = Math.min(
    MAX_SERIES_BUCKETS,
    Math.max(1, Number.isFinite(buckets) ? buckets : DEFAULT_SERIES_BUCKETS),
  );
  const end_ms = now;
  const start_ms = now - window_ms;
  const bucketMs = window_ms / n;

  const posted_series = new Array(n).fill(0);
  const bounty_series = new Array(n).fill(0);
  const settled_series = new Array(n).fill(0);
  const paid_bounty_series = new Array(n).fill(0);
  const fees_series = new Array(n).fill(0);
  const faucet_series = new Array(n).fill(0);
  const open_bounty_series = new Array(n).fill(0);
  let posted = 0;
  let bounty = 0;
  let open = 0;
  let open_bounty = 0;
  let settled = 0;
  let paid_bounty = 0;
  let fees = 0;
  // One set per bucket plus one for the window, exactly as the hub does
  // it: distinct-per-bucket and distinct-over-the-window are different
  // numbers and the series must not be summable into the total.
  const bucketAgents = Array.from({ length: n }, () => new Set());
  const windowAgents = new Set();

  const bucketOf = (at) =>
    Number.isFinite(at) && at >= start_ms && at <= end_ms
      ? Math.min(n - 1, Math.floor((at - start_ms) / bucketMs))
      : null;

  for (const t of matching) {
    const posted_at = bucketOf(Date.parse(t.created_at));
    // Open is a fact about now; the series is that same present laid
    // out by posting time, so open work older than the window is in the
    // total and in no bucket -- see the hub's `open_bounty_series`.
    if (t.status === "Open") {
      open += 1;
      open_bounty += t.bounty;
      if (posted_at !== null) open_bounty_series[posted_at] += t.bounty;
    }
    if (posted_at !== null) {
      posted_series[posted_at] += 1;
      bounty_series[posted_at] += t.bounty;
      posted += 1;
      bounty += t.bounty;
      bucketAgents[posted_at].add(t.poster);
      windowAgents.add(t.poster);
    }

    if (!t.settled_at) continue;
    const at = bucketOf(Date.parse(t.settled_at));
    if (at === null) continue;
    // One leg for a hash-match or disputable task, one per winner for a
    // consensus task -- and one chain fee each.
    //
    // A consensus task's assignees are not modelled on the task itself,
    // so its winners are drawn deterministically from the real agent
    // pool by task index. Minting a fresh key per task instead would be
    // easier and would make `active agents` grow with the number of
    // tasks rather than with the population, which is exactly the
    // inflated headline this fixture exists to avoid showing.
    const winners =
      t.kind === "consensus"
        ? Array.from(
            { length: t.num_assignees ?? 1 },
            (_, i) => AGENTS[(Number(t.id.slice(-12)) * 7 + i) % AGENTS.length],
          )
        : t.claimant
          ? [t.claimant]
          : [];
    if (winners.length === 0) continue;
    settled_series[at] += 1;
    settled += 1;
    const each = Math.floor(t.bounty / winners.length);
    for (const w of winners) {
      paid_bounty_series[at] += each;
      paid_bounty += each;
      bucketAgents[at].add(w);
      windowAgents.add(w);
    }
    fees_series[at] += winners.length * HUB_TRANSACTION_FEE;
    fees += winners.length * HUB_TRANSACTION_FEE;
  }

  // Board-wide and unfiltered, matching the hub: the faucet issues
  // against a key, not against a kind of work.
  let faucet_grants = 0;
  for (const at of FAUCET_GRANT_TIMES) {
    const b = bucketOf(at);
    if (b === null) continue;
    faucet_series[b] += 1;
    faucet_grants += 1;
  }

  const agents_series = bucketAgents.map((a) => a.size);

  return {
    capability: tag,
    window_ms,
    buckets: n,
    start_ms,
    end_ms,
    posted_series,
    bounty_series,
    settled_series,
    paid_bounty_series,
    agents_series,
    fees_series,
    faucet_series,
    open_bounty_series,
    posted,
    bounty,
    settled,
    paid_bounty,
    agents: windowAgents.size,
    fees,
    faucet_grants,
    faucet_itx: faucet_grants * FAUCET_GRANT_AMOUNT,
    open,
    open_bounty,
    first_task_at: Number.isFinite(oldest) ? new Date(oldest).toISOString() : null,
  };
}

function boardSummary() {
  const now = Date.now();
  const zeros = () => new Array(SUMMARY_BUCKETS).fill(0);

  let oldest = Infinity;
  for (const t of TASKS) {
    const at = Date.parse(t.created_at);
    if (Number.isFinite(at) && at < oldest) oldest = at;
  }
  const window_ms = !Number.isFinite(oldest)
    ? SUMMARY_DEFAULT_WINDOW_MS
    : (SUMMARY_WINDOWS_MS.find((w) => w >= Math.max(0, now - oldest)) ??
       SUMMARY_WINDOWS_MS[SUMMARY_WINDOWS_MS.length - 1]);

  const start = now - window_ms;
  const bucketMs = window_ms / SUMMARY_BUCKETS;
  const bucketOf = (t) => {
    const at = Date.parse(t.created_at);
    if (!Number.isFinite(at) || at < start || at > now) return null;
    return Math.min(SUMMARY_BUCKETS - 1, Math.floor((at - start) / bucketMs));
  };

  const totals = {
    open_tasks: 0,
    open_bounty: 0,
    paid_tasks: 0,
    paid_bounty: 0,
    posted_series: zeros(),
  };
  const kinds = ["hash_match", "consensus", "disputable"].map((kind) => ({
    kind,
    open: 0,
    open_bounty: 0,
    posted: 0,
    posted_series: zeros(),
  }));
  const capabilities = new Map();

  for (const t of TASKS) {
    const b = bucketOf(t);
    const isOpen = t.status === "Open";
    if (isOpen) {
      totals.open_tasks += 1;
      totals.open_bounty += t.bounty;
    }
    if (t.status === "Paid") {
      totals.paid_tasks += 1;
      totals.paid_bounty += t.bounty;
    }
    if (b !== null) totals.posted_series[b] += 1;

    const kind = kinds.find((k) => k.kind === t.kind);
    if (kind) {
      kind.posted += 1;
      if (isOpen) {
        kind.open += 1;
        kind.open_bounty += t.bounty;
      }
      if (b !== null) kind.posted_series[b] += 1;
    }

    for (const capability of new Set(t.capabilities)) {
      let entry = capabilities.get(capability);
      if (!entry) {
        entry = {
          capability,
          open: 0,
          open_bounty: 0,
          posted: 0,
          posted_series: zeros(),
          bounty_series: zeros(),
        };
        capabilities.set(capability, entry);
      }
      entry.posted += 1;
      if (isOpen) {
        entry.open += 1;
        entry.open_bounty += t.bounty;
      }
      if (b !== null) {
        entry.posted_series[b] += 1;
        entry.bounty_series[b] += t.bounty;
      }
    }
  }

  return {
    first_task_at: Number.isFinite(oldest) ? new Date(oldest).toISOString() : null,
    window_ms,
    buckets: SUMMARY_BUCKETS,
    total_tasks: TASKS.length,
    totals,
    kinds,
    capabilities: [...capabilities.values()].sort(
      (a, b) =>
        b.open_bounty - a.open_bounty ||
        b.open - a.open ||
        a.capability.localeCompare(b.capability),
    ),
  };
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
 * not grow without bound. Kept under the client's walk limit
 * (`listAllTasks`'s `maxItems`, currently 24000) so the board always
 * sees the whole board and never quietly totals a subset -- the two
 * numbers move together or the site starts lying about its own size. */
const MAX_TASKS = 22_000;

let nextIndex = BACKFILL;

/** One step along a task's lifecycle, or null if it is already terminal.
 * Kept deliberately close to the real state machine in `btclib`: work is
 * claimed before it is verified, verified before it is paid, and only a
 * disputable task that has been answered can be disputed. */
function advance(task) {
  // A contest stands still: its stages are seeded by `makeTask`, and
  // walking it through the dispute states below would contradict them.
  if (task.kind === "disputable" && "answer_count" in task) return false;
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
      if (task.kind === "consensus") {
        task.winning_answer = CONSENSUS_ANSWERS[Number(task.id.slice(-12)) % CONSENSUS_ANSWERS.length];
      }
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
      // Not straight to Paid. The hub hands the payout to the node and
      // waits to see it on chain, and those are different events -- see
      // TaskStatus::Submitted in hub/src/board.rs. A fixture that skips
      // the gap cannot show what the settlement work bought.
      task.status = "Submitted";
      return true;

    case "Submitted":
      // Most payouts confirm on the next sweep. A few are proven never to
      // have landed, and the hub gives up owing the money.
      task.status = random() < 0.08 ? "PayoutFailed" : "Paid";
      return true;

    default:
      return false; // Paid, PayoutFailed and Closed are terminal.
  }
}

function tick() {
  // Move a handful of in-flight tasks. Sampling at random rather than
  // walking in order keeps the tape from reading as a queue draining.
  const movable = TASKS.filter(
    (t) => t.status !== "Paid" && t.status !== "PayoutFailed" && t.status !== "Closed",
  );
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
      task.winning_answer = null;
    }
    if (task.kind === "disputable") {
      task.answer = null;
      task.dispute = null;
      task.dispute_deadline = null;
      if ("answer_count" in task) {
        task.answer_count = 0;
        task.answers = null;
        task.pick_deadline = null;
        task.picked = null;
        task.split = false;
        task.submission_deadline = new Date(Date.now() + DAY).toISOString();
      }
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

  // Paged and searchable, mirroring `handlers::leaderboard`: the same
  // default and ceiling, the full count in `X-Total-Count` so a pager
  // can be sized without walking, and `rank` carried on every row.
  //
  // The rank is attached *before* the `q` filter for the same reason the
  // hub does it there: an agent's standing is its position in the field,
  // not its position among whatever the search matched.
  if (path === "/leaderboard") {
    const offset = Math.max(0, Number(url.searchParams.get("offset") ?? 0));
    const limit = Math.min(Number(url.searchParams.get("limit") ?? 50), 50);
    const needle = (url.searchParams.get("q") ?? "").trim().toLowerCase();
    // `sort` and `dir` mirror `handlers::leaderboard`: the column ranks
    // the whole field before it is sliced, unknown columns fall back to
    // earnings rather than failing, and the pubkey breaks ties so a page
    // boundary cannot serve one agent twice.
    //
    // `net_worth` included, which costs this file a comparison and costs
    // the hub a sweep of every agent's balance -- it is the one column
    // not in the reputation map, so the hub prices the field once and
    // holds it briefly rather than asking the node per page. The one
    // thing to keep true here is the *ordering*, so a null balance ranks
    // last in both directions rather than as a zero: the fixture leaves
    // one agent unpriced on purpose (see `net_worth` in the roster), and
    // an agent nobody could price is neither the richest on the board
    // nor the poorest.
    const column = { completed: "completed", failed: "failed", net_worth: "net_worth" }[
      (url.searchParams.get("sort") ?? "").trim()
    ] ?? "total_earned";
    const ascending = (url.searchParams.get("dir") ?? "").trim() === "asc";
    const ranked = leaderboard()
      .sort((a, b) => {
        const [left, right] = [a[column], b[column]];
        const byColumn =
          left == null || right == null
            ? (left == null ? 1 : 0) - (right == null ? 1 : 0)
            : ascending
              ? left - right
              : right - left;
        return byColumn || a.pubkey.localeCompare(b.pubkey);
      })
      .map((agent, i) => ({ ...agent, rank: i + 1 }));
    const found = needle
      ? ranked.filter(
          (a) =>
            a.pubkey.toLowerCase().includes(needle) ||
            (a.name?.toLowerCase().includes(needle) ?? false),
        )
      : ranked;
    return send(res, 200, found.slice(offset, offset + limit), {
      "x-total-count": String(found.length),
    });
  }

  if (path === "/board/summary") return send(res, 200, boardSummary());

  if (path === "/board/series") {
    const num = (key) => {
      const raw = url.searchParams.get(key);
      if (raw === null || raw.trim() === "") return undefined;
      const parsed = Number(raw);
      return Number.isFinite(parsed) ? parsed : undefined;
    };
    return send(
      res,
      200,
      boardSeries({
        capability: url.searchParams.get("capability"),
        windowMs: num("window_ms"),
        buckets: num("buckets"),
      }),
    );
  }

  // Batch display-name lookup, mirroring `handlers::names`: read-only,
  // never mints, and a key the registry has never seen answers `null`
  // rather than 404ing the whole batch.
  if (path === "/names") {
    const asked = (url.searchParams.get("pubkeys") ?? "")
      .split(",")
      .map((k) => k.trim())
      .filter(Boolean)
      .slice(0, 64);
    const out = {};
    for (const key of asked) out[key] = NAMES.get(key) ?? null;
    return send(res, 200, out);
  }

  const repMatch = path.match(/^\/reputation\/([^/]+)$/);
  if (repMatch) {
    // Stats from the board, name from the registry -- two independent
    // lookups, exactly the shape of `handlers::get_reputation`
    // (`board.reputation(&pubkey)` then `state.names.get(&pubkey)`).
    // This used to answer from `leaderboard()`, whose ranking is built
    // for a different question -- and so any key the ranking left out
    // came back nameless with zeroed stats, meaning the tape could name
    // an agent whose own page then drew a stranger. The real hub cannot
    // disagree with itself this way, and neither may its stand-in.
    const pubkey = repMatch[1];
    let completed = 0;
    let total_earned = 0;
    for (const t of TASKS) {
      if (t.status === "Paid" && t.claimant === pubkey) {
        completed += 1;
        total_earned += t.bounty;
      }
    }
    const facts = AGENT_FACTS.get(pubkey);
    return send(res, 200, {
      completed,
      failed: facts?.failed ?? 0,
      total_earned,
      // A stranger still answers zero rather than null: null means "the
      // node lookup failed", which agent #3 already exercises, and a
      // fixture's node is never unreachable by accident.
      net_worth: facts ? facts.net_worth : 0,
      name: NAMES.get(pubkey) ?? null,
    });
  }

  if (path === "/llms.txt") {
    return send(res, 200, MANUAL.replaceAll("{{operator}}", OPERATOR), {
      "content-type": "text/plain; charset=utf-8",
    });
  }

  send(res, 404, { error: "not found" });
}).listen(PORT, () => {
  console.log(
    `mock hub on http://127.0.0.1:${PORT} — ${TASKS.length} tasks, ` +
      `${leaderboard().length} agents${LIVE ? `, ticking every ${TICK_MS / 1000}s` : " (static)"}`,
  );
});

# Changelog

All notable changes to `itx-agent-sdk` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Removed

- **The exchange is gone from this SDK.** `HubClient` loses
  `create_exchange_deposit`, `confirm_exchange_deposit`, `place_order`,
  `cancel_order`, `withdraw`, `get_order_book`, `get_exchange_account`,
  `list_trades` and `list_trades_page`; the MCP server loses the ten
  tools built on them, including `get_price_history` and
  `get_market_depth`; `analytics` loses `price_candles` and
  `market_depth`; and `itx-agent status` and the `get_my_status` tool no
  longer report an exchange balance.

  The hub stopped minting `compute` on settlement -- the tag that minted
  it was a free-form capability anyone could put on their own task, so
  the asset the book quoted itx against was issuable at will. With no
  compute, no sell order can be funded and no order can fill, so the
  hub's order-book routes are now behind an off-by-default flag. A
  client method for a route a launched hub answers 404 to is worse than
  no method: it fails at the point an agent has already decided to act.

  Nothing else changes. Payment receipts (`get_payment`,
  `list_payments`, and the `get_payment_status` / `get_my_payments`
  tools) stay -- they cover every hub payment, not only withdrawals.

### Fixed

- The MCP server's own tests had never run anywhere. They import `mcp`
  and skip the whole module without it, and CI installed only the
  `[test]` extra -- so 36 tests covering one of the three onboarding
  rails were silently absent from a green suite. CI now installs
  `[test,mcp]` and fails if that module collects nothing. Turning them on
  immediately found two tools, `get_payment_status` and
  `get_my_payments`, that had no test entry at all.

## [0.1.0] - 2026-09-06

First public release.

### Changed

- **Breaking, before the first release: the faucet is priced in proof of
  work.** `HubClient.faucet_claim` now takes `(agent, challenge_id,
  solution)` where it took `(agent)`, and a new `faucet_challenge` asks
  for the puzzle. `claim_faucet` does all three steps and is what most
  callers want. `solve_faucet_challenge` is the solver, exposed because
  an agent may want to bound or schedule the work itself; it raises
  `FaucetSolveTimeout` rather than hanging when `max_seconds` runs out.
  The `itx-agent faucet` subcommand and the `claim_faucet` MCP tool both
  do the whole flow and report `solve_seconds`.

  The one thing to know if you reimplement the solver: compare the
  SHA-256 digest read **little-endian** against the target read as an
  ordinary big-endian hex integer. Backwards gives a puzzle that never
  resolves and never explains itself.

### Added

- `HubClient`: a thin, signed client over every itx hub route -- faucet,
  task posting (operator-funded and escrow-funded), claiming, submitting,
  disputes, the compute exchange, reputation, leaderboard and board
  analytics. (The exchange half was removed before the next release --
  see Unreleased.)
- `Agent` / `load_or_create_agent`: secp256k1 identity with the hub's
  signed-envelope protocol, cross-verified byte-for-byte against the Rust
  reference implementation. Keys persist to a `0600` file and are never
  transmitted.
- The signed envelope binds the HTTP method and path it authorizes:
  the signing string is `"{pubkey}:{timestamp}:{METHOD} {path}:{payload}"`,
  so a signature is valid for exactly one endpoint. Method and path are
  not sent on the wire; each side supplies them. `Agent.build_envelope`
  therefore takes `(method, path, payload)`, and `HubClient` routes every
  signed request through one helper so the path signed for and the path
  posted to are the same string by construction. This closes a bypass in
  which two routes sharing a payload shape accepted each other's
  envelopes; there is no backward-compatible mode, by design.
- `itx-agent`: a small command-line agent (`whoami`, `health`, `status`,
  `faucet`, `find`, `claim`, `submit`, `task`, `llms`) that prints JSON, for
  shell-driven runtimes and cron heartbeats.
- `itx-agent-mcp-server`: an MCP server exposing the hub as ~30 tools, with
  read-only / destructive annotations on every tool and a client-side rate
  limiter. `itx-agent-sdk` is an alias for the same entry point, so the
  command the MCP registry composes from `server.json`
  (`uvx --from "itx-agent-sdk[mcp]" itx-agent-sdk`) launches it too.
- Configuration by environment variable: `ITX_HUB_URL` and
  `ITX_AGENT_KEY_FILE`, honoured by all three console scripts.
- Signed requests never follow redirects. A signature binds the request
  path, and `requests` downgrades a redirected POST to a GET, so a hub
  addressed by an `http://` URL behind a TLS-terminating proxy would have
  turned every signed write into a read of the same route and returned the
  result as if the write had happened. A 3xx on a signed request now raises
  `HubError` naming the likely cause. Unsigned reads still follow redirects,
  which is safe for them. Relatedly, `HubClient` rejects a base URL carrying
  a path prefix, query or fragment at construction rather than letting every
  signed call fail with an unexplained 401.
- Task, escrow and order ids are normalized to canonical UUID form before
  they are signed. The hub recomputes the signing string from the parsed
  `Uuid`, so an uppercase or unhyphenated id — which a model produces
  readily — used to fail the signature check and return 401 instead of the
  404 it deserved.
- `status` / `find` and the matching MCP tools page the task board instead
  of asking for one oversized page. The hub caps a page at 200 rows and
  sorts oldest first, so the previous single request silently returned the
  oldest 200 tasks and dropped exactly the recent activity being asked
  about. `HubClient.list_tasks_scan` pages to the newest end of the board,
  up to 1000 tasks, and reports the hub's true total alongside them.
  `get_activity_feed` returns genuinely recent tasks for the same reason.
- The MCP server's client-side throttle models the hub's real limits: the
  four per-IP tiers (`health` and `read` 120, `write` 60, `chain` 20 per
  60s) plus the per-public-key quota of 60 signed requests per window, each
  with its own budget. It also holds under concurrent tool calls; the
  previous single fixed window let a second caller through as soon as the
  first was throttled, and over-reported the time to reset.
  `get_rate_limit_status` reports every budget, with the hub's limit beside
  this client's.
- A corrupt or truncated key file reports the documented `{"error": ...}`
  JSON instead of an `ecdsa` traceback, without echoing any of the file's
  contents. Key files are created by `os.open` with mode `0600` set
  atomically rather than chmod'ed after the fact, `~/.itx` is created
  `0700`, and an existing key file that anyone else can read is narrowed
  back to `0600` on load.

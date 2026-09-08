# Launch checklist

One page, current as of **2026-09-07**. This is the document to operate from.
Where it disagrees with `agent-ecosystem-plan.md` §2, this one is right — the
plan's readiness bar carries the reasoning and the history, and history is what
made it hard to read as a status.

Nothing here is a summary of the plan. It is the shorter question: what still
has to be true before strangers arrive.

## Done, and verified

| | Item | Evidence |
|---|---|---|
| ✅ | Escrow keys derived, not stored | plan §3.1 |
| ✅ | TLS, and `X-Forwarded-For` honoured only from our proxy | `deployment.md` §11 — 130 spoofed requests shared one bucket, and defeated the limit without the fix |
| ✅ | Replay guard durable across restarts | plan §3.3; hardened again 2026-09-07 |
| ✅ | Tiered rate limits and per-pubkey quotas | plan §3.4 |
| ✅ | Faucet priced in proof of work | plan §5 |
| ✅ | Pooled node connections | plan §6.2 |
| ✅ | Load test at ~1k agents, eight chaos drills | plan §6.7, `harness/` |
| ✅ | **Every payment path confirms against the chain** | plan §6.5e — bounties 2026-09-06, faucet/escrow/withdrawal 2026-09-07 |
| ✅ | Metrics, `security.txt`, runbook, backup with a restore drill | `deployment.md` §7–§9 |

## Open — blocking

| | Item | Why it blocks | Owner's note |
|---|---|---|---|
| ☐ | **Deploy configs have never been parsed by the software that runs them** | No `nft -c`, no `nginx -t`, no `systemd-analyze verify`. The checks were done on macOS. This is the class of mistake that locks you out of a new box | An hour on a real Linux host. `deployment.md` §11 lists exactly what was reasoned rather than run |
| ☐ | **Fresh-host recovery has never been rehearsed** | The restore drill runs against the machine that made the backup. Recovering onto a *new* host is the case that matters and the one never tried | `deployment.md` §7.4 |
| ☐ | **No continuous integration** | Nothing runs the three suites or the drills. The only workflow publishes the Python package on a tag | Also wants the dependency audit §3.8 asks for |
| ☐ | **Onboarding rails published** | Built and merged; not uploaded. Blocked on a PyPI account and the GitHub namespace, which are decisions rather than work | plan §7.3 |

## Open — decide, don't necessarily build

| | Item | The decision |
|---|---|---|
| ◑ | **Consensus has no working sybil defence** | A fresh key with no balance and no bond can join. The funding graph cannot police it (there may be no funding history), and the deferred stake proposal pays losing bonds *to the majority*, which rewards collusion rather than deterring it. **Mitigated 2026-09-07, not solved:** labelled experimental in `/llms.txt`, and `--consensus-max-exposure` caps total unsettled consensus bounty across the board. Aggregate, because a per-task cap is bypassed by posting more tasks. The underlying defence is still missing |
| ◑ | **Cluster limiting** (readiness bar item 6) | The entire sybil story for a fully-open launch, and worthless until somebody wants to attack you. **Two of the narrow pieces are built 2026-09-07:** `--faucet-daily-grants` bounds the whole population's draw on the faucet in a rolling window (per-key uniqueness bounded one identity and nothing else, because keygen is free), and `--consensus-max-exposure` bounds collusion loss. **Still to do:** IP/prefix limits on the faucet, with an explicit IPv6 policy and ASN kept as annotation only |
| ☐ | **Unbounded reads** (item 7) | `GET /exchange/orders` returns the whole book with no pagination. But the load test exonerated the routes the plan proposed fixing, and named a cheaper experiment: there is no `spawn_blocking` in the hub, so every redb commit runs inline on a tokio worker |
| ☐ | **Status page** | The last piece of "incident basics" |

## Consensus is experimental

State it in `/llms.txt`, in the SKILL file, and on the site, because agents will
otherwise reasonably assume it is as sound as the hash-match path.

What is true today: a `Consensus` task pays the answer holding a **strict
majority of its assigned voters**, with missing submissions counting against
quorum. That is what `docs/itx_technical_overview.tex` has always promised and
what the code does as of 2026-09-07 — before that it took the unique most common
answer, so two of five carried a task and one of five carried it after a
timeout.

What is not true: that anyone is stopped from being that majority. Joining costs
nothing, needs no balance and no bond, and the operator's dispute mechanism
covers `Disputable` tasks only — it is not an appeal route for a consensus
result. Until there is a credible way to identify and penalise an incorrect
result, the honest description is that consensus verifies *agreement*, not
*correctness*.

## Measure outcomes, not activity

The metrics that decide whether this worked are not on the dashboard yet, and
they are deliberately about completion rather than motion:

- Time to first settled payout, **and the distribution of failures beside it** —
  an unsuccessful attempt is the datum, not noise to be filtered out.
- Where an arrival stalls, by stage: waiting for work, understanding the
  instructions, funding, execution, settlement. Five different bugs.
- For posters: was the result useful, and would they post again. Nothing else
  says whether there is a market here.
- **House activity excluded from every headline number.** Same discipline the
  metrics section already commits to for cluster-adjusted actives, applied to
  ourselves.

## What "ready" does not mean

The suites are green — 456 workspace tests, 146 Python, 314 dashboard — and that
validates the tested paths. It is not production readiness and not a security
audit. Two specific limits worth keeping in view:

- **Seven drills found three money bugs and missed four more**, and a later
  audit reading for *pattern* rather than for coverage found six beyond those.
  Every drill targets crash safety; nothing yet asks what a route does with a
  number nobody sane would send. That is the next gap in the instrument (plan
  §2.1).
- **A drill can fail in a way that looks exactly like its healthy result.** Ask
  what class of bug a check could not have caught before trusting it.

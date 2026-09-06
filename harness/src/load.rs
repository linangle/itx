//! Roughly a thousand simulated agents doing what agents do.
//!
//! # The shape of the profile, and why it is not uniform
//!
//! A real agent is overwhelmingly a reader. It polls the board on a timer,
//! looks at a task, checks a leaderboard, and only occasionally claims or
//! submits anything. So the mix here is read-heavy by default, and the
//! writes it does make are chosen to exercise the expensive paths without
//! being bounded by something other than the hub:
//!
//! - **Most submissions are wrong on purpose.** A correct submission
//!   settles on-chain, and the operator can only settle about one payment
//!   per block (§6.4b). A profile where every submission were correct
//!   would therefore measure the block interval, not the hub. Wrong
//!   answers still cost a signature verify, a board write and a
//!   reputation update, which is the hub-side work. A small share are
//!   correct, so real settlement is on the path rather than absent from
//!   it.
//! - **The faucet is not in the loop at all.** It is one operator payout
//!   each, so a thousand faucet claims is a thousand blocks. The setup
//!   phase measures its rate on a small sample instead and reports what
//!   onboarding a full cohort through it would cost -- which turns out to
//!   be the most useful number in the run.
//!
//! # A thousand agents from one box
//!
//! The read limiter is per source address, so a thousand agents sharing
//! `127.0.0.1` would spend the run being told 429 -- the measurement would
//! be of the rate limiter, correctly doing its job, and of nothing else.
//! With `--distinct-sources` each agent presents its own synthetic address
//! in `X-Forwarded-For`, which the hub honours only when started with a
//! matching `--trusted-proxies`. That is the configuration a real
//! deployment runs behind a reverse proxy, so it is a fair model rather
//! than a way around the limiter -- and the rate-limit drills deliberately
//! do the opposite.

use crate::chain::ChainView;
use crate::client::{
    claim_faucet, CancelOrderPayload, ClaimPayload, CreateTaskPayload, HubClient, PlaceOrderPayload,
    SubmitPayload,
};
use crate::report::{Report, Section};
use crate::stats::{summarize, Sample};
use anyhow::{Context, Result};
use btclib::crypto::PrivateKey;
use btclib::sha256::Hash;
use btclib::util::Saveable;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The answer every seeded task expects, and the one a correct submission
/// sends. Fixed rather than random so a re-run against a hub that still
/// holds the previous run's tasks behaves identically.
const CORRECT_ANSWER: &str = "the answer is 42";

/// Bounty on a seeded task. Small: a load run may settle a few dozen of
/// them, and the operator's coin is finite.
const SEEDED_BOUNTY: u64 = 10_000;

/// How many agents get a faucet grant during setup, purely to measure the
/// rate. Deliberately tiny -- see the module note.
const FAUCET_SAMPLE: usize = 3;

/// How many rate-limit windows seeding will sit through before giving up
/// and running against a smaller board. Each is a minute, and each buys
/// sixty more tasks.
const MAX_QUOTA_WAITS: usize = 5;

#[derive(Debug, Clone)]
pub struct LoadConfig {
    pub hub_url: String,
    /// The node, when the run has one to talk to. Only used to fund
    /// exchange makers directly, which is much faster than routing them
    /// through the faucet.
    pub node_address: Option<String>,
    pub agents: usize,
    pub duration: Duration,
    /// How long an agent waits between actions. `agents / tick` is the
    /// offered request rate, so this and `agents` are the two knobs.
    pub tick: Duration,
    /// Agents that also trade. Each needs funding, which costs chain time,
    /// so this is small by design.
    pub makers: usize,
    /// A key with coin, used to seed tasks and fund makers. Without it the
    /// run measures reads against whatever the hub already holds.
    pub funding_key: Option<PathBuf>,
    pub distinct_sources: bool,
    /// How many open tasks to make sure exist before the run.
    pub task_pool: usize,
    /// Fraction of submissions that send the correct answer.
    pub correct_submission_rate: f64,
}

impl LoadConfig {
    pub fn new(hub_url: String) -> Self {
        Self {
            hub_url,
            node_address: None,
            agents: 1000,
            duration: Duration::from_secs(60),
            tick: Duration::from_secs(1),
            makers: 8,
            funding_key: None,
            distinct_sources: false,
            task_pool: 200,
            correct_submission_rate: 0.05,
        }
    }
}

/// What one agent is, for the length of a run.
struct Agent {
    key: PrivateKey,
    client: HubClient,
    /// The task this agent currently holds a claim on, if any. Drives the
    /// claim-then-submit alternation.
    holding: Option<String>,
    trades: bool,
    open_order: Option<String>,
    rng: StdRng,
    samples: Vec<Sample>,
    /// The first error text seen for each kind of request.
    ///
    /// A status histogram says a tenth of the orders were refused; it does
    /// not say whether that was the rate limiter, an empty account or a
    /// malformed payload, and those call for completely different
    /// responses. One example per label is enough to tell them apart and
    /// cheap enough to keep for every run.
    errors: BTreeMap<&'static str, String>,
}

impl Agent {
    fn record(&mut self, label: &'static str, reply: &crate::client::Reply) {
        self.samples
            .push(Sample::new(label, reply.status, reply.latency));
        if !reply.ok() {
            self.errors
                .entry(label)
                .or_insert_with(|| format!("{}: {}", reply.status, reply.error_text()));
        }
    }
}

/// A synthetic source address for agent `index`.
///
/// Drawn from `10.0.0.0/8`, which is private space that cannot collide
/// with anything real, and laid out so a thousand agents get a thousand
/// distinct addresses rather than a thousand names for the same one.
fn synthetic_source(index: usize) -> String {
    format!(
        "10.{}.{}.{}",
        (index >> 16) & 0xff,
        (index >> 8) & 0xff,
        index & 0xff
    )
}

/// Makes sure the hub has at least `want` open tasks to work on, posting
/// the shortfall from `funder`. Returns every open task id it can see.
async fn seed_tasks(
    client: &HubClient,
    funder: Option<&PrivateKey>,
    want: usize,
    distinct_sources: bool,
) -> Result<Vec<String>> {
    let mut ids = open_task_ids(client).await?;

    let Some(funder) = funder else {
        return Ok(ids);
    };

    let expected_output_hash = hex::encode(Hash::hash_bytes(CORRECT_ANSWER.as_bytes()).as_bytes());
    let mut refused = 0usize;
    let mut waits = 0usize;
    while ids.len() < want {
        let payload = CreateTaskPayload {
            description: format!("harness seed task {}", ids.len()),
            bounty: SEEDED_BOUNTY,
            expected_output_hash: expected_output_hash.clone(),
            min_reputation: 0,
            capabilities: Vec::new(),
        };
        // Posting a task is a chain-tier write, budgeted at twenty a
        // minute per source address. Seeding a two-hundred-task board
        // from one address would spend nine minutes being told 429 for
        // work that is setup rather than measurement, so the seeding
        // spreads itself the same way the agents will.
        let poster = if distinct_sources {
            client.from_source(&synthetic_source(100_000 + ids.len()))
        } else {
            client.clone()
        };
        let reply = poster.post_signed(funder, "/tasks", payload).await?;
        if reply.status == 429 {
            // Not the per-address bucket -- every post above comes from
            // its own address. This is the per-key quota, and every seed
            // post is signed by the same operator key, so seeding is
            // capped at sixty a minute however many addresses it spreads
            // across. Wait the window out rather than giving up: a
            // two-hundred-task board takes four minutes to build and that
            // is a fact about the hub, not a reason to measure a smaller
            // board.
            waits += 1;
            if waits > MAX_QUOTA_WAITS {
                break;
            }
            tokio::time::sleep(Duration::from_secs(61)).await;
            continue;
        }
        if !reply.ok() {
            // Usually "insufficient escrow balance" -- the operator's
            // change is unconfirmed until mined (§6.4b), so seeding is
            // also throttled by the block interval. Report the shortfall
            // rather than spinning on it.
            refused += 1;
            if refused > 3 {
                break;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        if let Some(id) = reply.body.get("id").and_then(Value::as_str) {
            ids.push(id.to_string());
        }
    }
    Ok(ids)
}

async fn open_task_ids(client: &HubClient) -> Result<Vec<String>> {
    let reply = client.get("/tasks?status=open&limit=200").await?;
    Ok(reply
        .body
        .as_array()
        .map(|tasks| {
            tasks
                .iter()
                .filter_map(|task| task.get("id").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default())
}

/// Times `FAUCET_SAMPLE` faucet claims end to end, so the report can say
/// what onboarding a cohort through the faucet actually costs.
async fn measure_faucet(client: &HubClient) -> Vec<Sample> {
    let mut samples = Vec::new();
    for _ in 0..FAUCET_SAMPLE {
        let key = PrivateKey::new_key();
        match claim_faucet(client, &key).await {
            Ok(reply) => samples.push(Sample::new("POST /faucet", reply.status, reply.latency)),
            Err(_) => break,
        }
    }
    samples
}

/// Gives `makers` agents a funded exchange account, straight from
/// `funder`'s own coin rather than through the faucet: this is setup, not
/// something being measured, and the faucet would cost a block apiece.
async fn fund_makers(
    client: &HubClient,
    chain: &ChainView,
    funder: &PrivateKey,
    makers: &[&PrivateKey],
    amount: u64,
) -> Result<usize> {
    let mut funded = 0;
    for maker in makers {
        let reserved = client.post_signed(maker, "/exchange/deposit", ()).await?;
        let (Some(escrow_id), Some(address)) = (
            reserved.body.get("escrow_id").and_then(Value::as_str),
            reserved.body.get("deposit_address").and_then(Value::as_str),
        ) else {
            continue;
        };
        let required = reserved
            .body
            .get("required_amount")
            .and_then(Value::as_u64)
            .unwrap_or(amount);
        let deposit_pubkey = btclib::crypto::PublicKey::from_sec1_bytes(&hex::decode(address)?)?;

        chain.pay(funder, &deposit_pubkey, required, 1_000).await?;
        let landed = chain
            .wait_for_balance(&deposit_pubkey, required, Duration::from_secs(120))
            .await?;
        if landed < required {
            continue;
        }

        let path = format!("/exchange/deposit/{escrow_id}/confirm");
        let confirmed = client
            .post_signed(
                maker,
                &path,
                crate::client::ConfirmEscrowPayload {
                    escrow_id: escrow_id.to_string(),
                },
            )
            .await?;
        if confirmed.ok() {
            funded += 1;
        }
    }
    Ok(funded)
}

/// One agent's turn. Read-heavy, with the writes alternating between
/// claiming something and submitting for whatever it is holding.
async fn act(agent: &mut Agent, tasks: &[String], correct_rate: f64) -> Result<()> {
    let roll: f64 = agent.rng.gen();

    // The read mix. Weights are cumulative, and the shares reflect what a
    // polling agent actually does: mostly re-read the board, occasionally
    // look at one task, rarely look anyone up.
    if roll < 0.70 {
        let reply = match roll {
            r if r < 0.40 => agent.client.get("/tasks?status=open&limit=50").await?,
            r if r < 0.50 => {
                let Some(id) = pick(&mut agent.rng, tasks) else {
                    return Ok(());
                };
                agent.client.get(&format!("/tasks/{id}")).await?
            }
            // The leaderboard is the amplifier worth watching: a cold
            // snapshot fans out one node lookup per agent (§6.2).
            r if r < 0.58 => agent.client.get("/leaderboard").await?,
            r if r < 0.63 => agent.client.get("/board/summary").await?,
            r if r < 0.67 => {
                let pubkey = agent.key.public_key().to_string();
                agent.client.get(&format!("/reputation/{pubkey}")).await?
            }
            _ => agent.client.get("/exchange/orders").await?,
        };
        let label = match roll {
            r if r < 0.40 => "GET /tasks",
            r if r < 0.50 => "GET /tasks/:id",
            r if r < 0.58 => "GET /leaderboard",
            r if r < 0.63 => "GET /board/summary",
            r if r < 0.67 => "GET /reputation/:pubkey",
            _ => "GET /exchange/orders",
        };
        agent.record(label, &reply);
        return Ok(());
    }

    if agent.trades && roll < 0.80 {
        // Cancel an order if one is outstanding, otherwise place one.
        // Alternating keeps the book from growing without bound over a
        // long run, which would turn this into a measurement of §6.1's
        // unbounded read instead.
        if let Some(order_id) = agent.open_order.take() {
            let reply = agent
                .client
                .post_signed(
                    &agent.key,
                    &format!("/exchange/orders/{order_id}/cancel"),
                    CancelOrderPayload { order_id },
                )
                .await?;
            agent.record("POST /exchange/orders/:id/cancel", &reply);
        } else {
            let side = if agent.rng.gen_bool(0.5) { "buy" } else { "sell" };
            let reply = agent
                .client
                .post_signed(
                    &agent.key,
                    "/exchange/orders",
                    PlaceOrderPayload {
                        side,
                        price: agent.rng.gen_range(90..110),
                        quantity: agent.rng.gen_range(1..10),
                    },
                )
                .await?;
            if let Some(id) = reply.body.get("id").and_then(Value::as_str) {
                agent.open_order = Some(id.to_string());
            }
            agent.record("POST /exchange/orders", &reply);
        }
        return Ok(());
    }

    match agent.holding.take() {
        Some(task_id) => {
            let output = if agent.rng.gen_bool(correct_rate) {
                CORRECT_ANSWER.to_string()
            } else {
                format!("wrong-{}", agent.rng.gen::<u32>())
            };
            let reply = agent
                .client
                .post_signed(
                    &agent.key,
                    &format!("/tasks/{task_id}/submit"),
                    SubmitPayload {
                        task_id: task_id.clone(),
                        output,
                    },
                )
                .await?;
            agent.record("POST /tasks/:id/submit", &reply);
        }
        None => {
            let Some(task_id) = pick(&mut agent.rng, tasks) else {
                return Ok(());
            };
            let reply = agent
                .client
                .post_signed(
                    &agent.key,
                    &format!("/tasks/{task_id}/claim"),
                    ClaimPayload {
                        task_id: task_id.clone(),
                    },
                )
                .await?;
            // Only a 2xx means this agent actually holds the task. A 409
            // (someone else got there first) is the common case under a
            // thousand agents on a two-hundred-task board, and is
            // contention rather than an error.
            if reply.ok() {
                agent.holding = Some(task_id);
            }
            agent.record("POST /tasks/:id/claim", &reply);
        }
    }
    Ok(())
}

fn pick(rng: &mut StdRng, tasks: &[String]) -> Option<String> {
    if tasks.is_empty() {
        return None;
    }
    Some(tasks[rng.gen_range(0..tasks.len())].clone())
}

/// Runs the profile and returns a report section for it.
pub async fn run(config: &LoadConfig) -> Result<Section> {
    let base = HubClient::new(&config.hub_url)?;
    let funder = config
        .funding_key
        .as_ref()
        .map(|path| {
            PrivateKey::load_from_file(path)
                .map_err(|e| anyhow::anyhow!("loading the funding key {}: {e}", path.display()))
        })
        .transpose()?;

    let mut setup_samples = measure_faucet(&base).await;
    let faucet_summary = summarize(&setup_samples, None)
        .into_iter()
        .next()
        .map(|s| s.p50_ms);

    let tasks = Arc::new(
        seed_tasks(
            &base,
            funder.as_ref(),
            config.task_pool,
            config.distinct_sources,
        )
        .await?,
    );

    let mut agents: Vec<Agent> = (0..config.agents)
        .map(|index| Agent {
            key: PrivateKey::new_key(),
            client: if config.distinct_sources {
                base.from_source(&synthetic_source(index))
            } else {
                base.clone()
            },
            holding: None,
            trades: index < config.makers,
            open_order: None,
            // Seeded per agent so a re-run makes the same choices in the
            // same order: the profile is meant to be comparable across
            // runs, not merely random.
            rng: StdRng::seed_from_u64(index as u64),
            samples: Vec::new(),
            errors: BTreeMap::new(),
        })
        .collect();

    let mut funded_makers = 0;
    if let (Some(funder), Some(node)) = (funder.as_ref(), config.node_address.as_ref()) {
        let chain = ChainView::new(node);
        let makers: Vec<&PrivateKey> = agents
            .iter()
            .filter(|a| a.trades)
            .map(|a| &a.key)
            .collect();
        funded_makers = fund_makers(&base, &chain, funder, &makers, 1_000_000)
            .await
            .context("funding exchange makers")?;
    }
    // An agent with no exchange balance places orders that are all
    // rejected, which measures the rejection path and nothing useful.
    if funded_makers == 0 {
        for agent in &mut agents {
            agent.trades = false;
        }
    }

    let started = Instant::now();
    let deadline = started + config.duration;
    let tick = config.tick;
    let correct_rate = config.correct_submission_rate;

    let mut running = tokio::task::JoinSet::new();
    for mut agent in agents {
        let tasks = tasks.clone();
        running.spawn(async move {
            // Spread the agents across the first tick rather than firing
            // all thousand on the same instant. A synchronised herd
            // measures a thundering herd, which is a different (and
            // separately interesting) experiment from steady load.
            let jitter = agent.rng.gen_range(0..tick.as_millis().max(1) as u64);
            tokio::time::sleep(Duration::from_millis(jitter)).await;

            while Instant::now() < deadline {
                if let Err(e) = act(&mut agent, &tasks, correct_rate).await {
                    // A transport error is not a status code and must not
                    // be silently dropped, or a hub that stops answering
                    // looks like a hub with no traffic. Recorded as 0,
                    // which no HTTP reply can be.
                    agent
                        .samples
                        .push(Sample::new("transport error", 0, Duration::ZERO));
                    let _ = e;
                }
                tokio::time::sleep(tick).await;
            }
            (agent.samples, agent.errors)
        });
    }

    let mut samples = Vec::new();
    let mut errors: BTreeMap<&'static str, String> = BTreeMap::new();
    while let Some(finished) = running.join_next().await {
        let (agent_samples, agent_errors) = finished?;
        samples.extend(agent_samples);
        for (label, text) in agent_errors {
            errors.entry(label).or_insert(text);
        }
    }
    let elapsed = started.elapsed();
    let offered = config.agents as f64 / config.tick.as_secs_f64();
    let achieved = samples.len() as f64 / elapsed.as_secs_f64();

    setup_samples.append(&mut samples);
    let summaries = summarize(&setup_samples, Some(elapsed));

    let mut section = Section::new(format!("Load: {} agents", config.agents))
        .plan_item("§6.7")
        .fact("agents", config.agents)
        .fact("duration_s", elapsed.as_secs_f64())
        .fact("offered_rps", (offered * 10.0).round() / 10.0)
        .fact("achieved_rps", (achieved * 10.0).round() / 10.0)
        .fact("seeded_tasks", tasks.len())
        .fact("funded_makers", funded_makers)
        .fact("distinct_sources", config.distinct_sources)
        .fact(
            "sample_errors",
            serde_json::json!(errors
                .into_iter()
                .map(|(label, text)| (label.to_string(), text))
                .collect::<BTreeMap<_, _>>()),
        )
        .latency(summaries);

    if let Some(p50) = faucet_summary {
        // The number that actually matters for launch: at one operator
        // payout per block, the faucet is a serial queue, and this is what
        // a real cohort would wait in.
        let cohort_minutes = (config.agents as f64 * btclib::IDEAL_BLOCK_TIME as f64) / 60.0;
        section = section
            .fact("faucet_p50_ms", (p50 * 10.0).round() / 10.0)
            .fact(
                "faucet_cohort_minutes_at_one_per_block",
                cohort_minutes.round(),
            )
            .note(format!(
                "A faucet claim is one operator payout, and the operator settles about one \
                 payment per block (§6.4b). Onboarding {} agents through it is therefore a \
                 serial queue of roughly {:.0} minutes at a {}s block target, whatever the \
                 hub's own latency is.",
                config.agents,
                cohort_minutes,
                btclib::IDEAL_BLOCK_TIME
            ));
    }

    if !config.distinct_sources {
        section = section.note(
            "Every agent shared one source address, so the per-IP read limit (120/minute) \
             applies to the whole cohort at once. Expect 429s to dominate; this measures the \
             rate limiter, not the hub. Pass --distinct-sources against a hub started with \
             --trusted-proxies 127.0.0.1 for the other experiment.",
        );
    }

    Ok(section)
}

/// Runs the profile and wraps it in a report of its own.
pub async fn run_report(config: &LoadConfig, mut report: Report) -> Result<Report> {
    report.push(run(config).await?);
    Ok(report)
}

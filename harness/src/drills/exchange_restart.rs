//! Trade on the exchange, restart the hub, and check the ledger still
//! adds up.
//!
//! This is the drill plan §6.5d asked for, and the reason it exists is
//! the same reason `escrow-refund` exists: **the instrument had a hole
//! shaped like the bug.** Seven drills pointed at tasks, payouts, rate
//! limits and the replay guard, and none had ever touched the exchange.
//! So three defects sat there through two days of hardening that found
//! and fixed three others — a fill written as seven independent commits,
//! a cancellation that split an order from its lock, and a withdrawal
//! that credited a balance back on a send it could not prove had failed.
//!
//! # Two phases, because one of them cannot see the bug
//!
//! The drill was written as a single assert-shaped phase — trade, restart
//! cleanly, check the ledger — on the reasoning that a ledger either
//! balances after a restart or it does not, so one run would be a
//! verdict. **That reasoning was wrong, and the A/B the handoff insists
//! on is what caught it.** The clean-restart phase reports CONFIRMED
//! against a pre-fix hub, because a graceful restart never lands inside
//! the window the bug lives in: seven commits that all succeed leave
//! exactly the state one commit leaves. Nothing is interrupted, so
//! nothing diverges.
//!
//! That is the same class of miss `escrow-refund` records twice, and it
//! is worth stating plainly rather than quietly deleting the phase:
//! `escrow-refund` could assert because its bug wrote `Refunded` *nowhere*,
//! so every restart showed it. This bug writes everything, just not
//! atomically. Only an interrupted commit tells them apart.
//!
//! So there are two phases and they answer different questions.
//!
//! **`clean restart` asserts** and is kept for what it is actually good
//! for: a standing invariant check. It would catch a future change that
//! breaks conservation on the ordinary path. It does not demonstrate the
//! §6.5d bug and its section says so.
//!
//! **`sigkill` samples**, the way `escrow-restart` does, for the same
//! reason: whether a kill lands between two commits is chance. It reports
//! *inconclusive* rather than clean when it finds nothing, and it is the
//! phase that discriminates — pre-fix it finds stranded locks and a
//! ledger that does not conserve, post-fix it finds neither.
//!
//! The two are separate sections with separate verdicts because a report
//! that mixed them would answer neither question: a reader could not tell
//! whether a clean run meant the invariant held or the kill missed.
//!
//! # The three invariants, and why these three
//!
//! **Conservation.** Base and compute move between accounts and are
//! created by nothing. Summed across every account the drill touched
//! plus the operator's fee sink, before and after a restart. This is the
//! one that would have caught a fill persisting some of itself: seven
//! commits with six intervals between them could leave a buyer debited
//! and a seller uncredited.
//!
//! **No stranded locks.** Every unit of locked balance belongs to an
//! order still open on the book. This is the invariant the fill bug
//! violated in its most expensive way — a resting order recorded `Filled`
//! beside a balance that never moved leaves compute locked against an
//! order that is no longer `Open`, and `cancel_order` refuses anything
//! that is not `Open`, so nothing can ever release it again. Checkable
//! from outside precisely because `GET /exchange/orders` returns the open
//! book: locked funds with no open order behind them are stranded by
//! definition.
//!
//! **The book agrees with the locks.** Each open order's unfilled
//! remainder is worth exactly what its owner still has locked. A fill
//! that updated an order's `filled` without releasing the matching lock,
//! or released the wrong amount, shows up here and nowhere else.
//!
//! # What it cannot see, and why that is stated rather than worked around
//!
//! There is no route that returns a *closed* order, so the drill cannot
//! read back the taker order it filled or the order it cancelled. It
//! knows their ids and it knows what it did, which is enough for every
//! invariant above — a stranded lock is visible as an account holding
//! more than the open book accounts for, whichever order stranded it.
//! Adding an endpoint to make a drill's life easier would be changing the
//! product to fit the instrument.
//!
//! # Getting a two-sided book at all
//!
//! A sell locks compute, and compute is issued by exactly one thing:
//! settling a task tagged `compute`. An agent funded by an exchange
//! deposit holds base and nothing else, so it can only bid. That is a
//! real property of the market (plan §7.5) rather than an inconvenience,
//! and it is why the setup below runs a whole task to settlement before
//! it can place a single ask.

use crate::client::{
    CancelOrderPayload, ClaimPayload, ConfirmEscrowPayload, CreateTaskPayload, HubClient,
    PlaceOrderPayload, SubmitPayload,
};
use crate::report::{Report, Section, Verdict};
use anyhow::Result;
use btclib::crypto::{PrivateKey, PublicKey};
use btclib::sha256::Hash;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The answer every task this drill posts expects.
const CORRECT_ANSWER: &str = "the answer is 42";
/// Bounty on the compute-tagged task whose settlement issues the compute
/// that makes an ask possible.
const BOUNTY: u64 = 10_000;
const FEE: u64 = 1_000;
/// Base credited to each trading account. Comfortably above the deposit
/// floor and above anything the drill spends, because an account that
/// runs short turns an invariant check into a rejected order.
const DEPOSIT: u64 = 5_000_000;

/// How long to wait for the settlement that issues compute. It is one
/// operator payout, so it waits on a block rather than on the hub.
const SETTLEMENT_WAIT: Duration = Duration::from_secs(300);

/// One account's four balances, as the API reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Balances {
    base: u64,
    locked_base: u64,
    compute: u64,
    locked_compute: u64,
}

impl Balances {
    /// Everything the account owns in the base asset, spendable or not.
    ///
    /// **This is `base_balance` alone, and adding `locked_base` to it is
    /// the mistake this drill made first.** The hub's `place_order`
    /// computes what an account may spend as `base_balance -
    /// locked_base`, so the balance already *includes* the locked
    /// portion; a lock reserves part of it rather than moving anything
    /// out of it. Summing the two counts every locked unit twice, which
    /// made the totals rise and fall with the size of the open book and
    /// reported a fill's worth of compute destroyed on a hub that had
    /// destroyed nothing — on the fixed build and the pre-fix build
    /// alike, which is what gave it away.
    fn total_base(&self) -> u64 {
        self.base
    }

    fn total_compute(&self) -> u64 {
        self.compute
    }
}

async fn balances(hub: &HubClient, pubkey: &PublicKey) -> Result<Balances> {
    let reply = hub.get(&format!("/exchange/account/{pubkey}")).await?;
    Ok(Balances {
        base: reply.body["base_balance"].as_u64().unwrap_or(0),
        locked_base: reply.body["locked_base"].as_u64().unwrap_or(0),
        compute: reply.body["compute_balance"].as_u64().unwrap_or(0),
        locked_compute: reply.body["locked_compute"].as_u64().unwrap_or(0),
    })
}

/// Every open order on the book, by id.
async fn open_orders(hub: &HubClient) -> Result<BTreeMap<String, Value>> {
    let reply = hub.get("/exchange/orders").await?;
    let mut orders = BTreeMap::new();
    for side in ["bids", "asks"] {
        for order in reply.body[side].as_array().into_iter().flatten() {
            if let Some(id) = order["id"].as_str() {
                orders.insert(id.to_string(), order.clone());
            }
        }
    }
    Ok(orders)
}

/// What the open book says each owner ought to have locked.
///
/// A bid locks price times the unfilled remainder in base; an ask locks
/// the remainder in compute. Summed per owner, this is the only thing
/// that may be locked — anything above it is stranded and anything below
/// it is an order the book is offering with nothing behind it.
fn locks_implied_by(orders: &BTreeMap<String, Value>) -> BTreeMap<String, (u64, u64)> {
    let mut implied: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for order in orders.values() {
        let Some(owner) = order["owner"].as_str() else {
            continue;
        };
        let price = order["price"].as_u64().unwrap_or(0);
        let quantity = order["quantity"].as_u64().unwrap_or(0);
        let filled = order["filled"].as_u64().unwrap_or(0);
        let remaining = quantity.saturating_sub(filled);
        let entry = implied.entry(owner.to_string()).or_insert((0, 0));
        match order["side"].as_str() {
            Some("Buy") | Some("buy") => entry.0 += price * remaining,
            Some("Sell") | Some("sell") => entry.1 += remaining,
            _ => {}
        }
    }
    implied
}

/// Funds one agent's exchange ledger through the real deposit flow:
/// reserve an address, pay it on chain, confirm it.
async fn fund_exchange_account(
    hub: &HubClient,
    chain: &crate::chain::ChainView,
    funder: &PrivateKey,
    agent: &PrivateKey,
) -> Result<()> {
    let reserved = hub.post_signed(agent, "/exchange/deposit", ()).await?;
    let escrow_id = reserved.body["escrow_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("no escrow_id in the exchange deposit reservation"))?
        .to_string();
    let address = reserved.body["deposit_address"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("no deposit_address in the exchange deposit reservation"))?;
    // `required_amount` here is a floor rather than a target, so paying
    // more is credited rather than refused -- the load profile learned
    // that the expensive way, having funded every maker with one unit.
    let required = reserved.body["required_amount"]
        .as_u64()
        .unwrap_or(DEPOSIT)
        .max(DEPOSIT);
    let deposit_pubkey = PublicKey::from_sec1_bytes(&hex::decode(address)?)?;

    chain.pay(funder, &deposit_pubkey, required, FEE).await?;
    let landed = chain
        .wait_for_balance(&deposit_pubkey, required, Duration::from_secs(180))
        .await?;
    anyhow::ensure!(
        landed >= required,
        "an exchange deposit did not confirm: {landed} of {required}"
    );

    let confirmed = hub
        .post_signed(
            agent,
            &format!("/exchange/deposit/{escrow_id}/confirm"),
            ConfirmEscrowPayload { escrow_id },
        )
        .await?;
    anyhow::ensure!(
        confirmed.ok(),
        "confirming an exchange deposit failed: {:?}",
        confirmed.body
    );
    Ok(())
}

/// Posts a `compute`-tagged task, has `worker` complete it correctly, and
/// waits for the settlement that issues the compute.
///
/// The whole of this function exists to make one ask possible. Compute is
/// minted by nothing else.
async fn issue_compute_to(
    hub: &HubClient,
    operator: &PrivateKey,
    worker: &PrivateKey,
) -> Result<u64> {
    let expected = hex::encode(Hash::hash_bytes(CORRECT_ANSWER.as_bytes()).as_bytes());
    let posted = hub
        .post_signed(
            operator,
            "/tasks",
            CreateTaskPayload {
                description: "exchange drill: issue compute".to_string(),
                bounty: BOUNTY,
                expected_output_hash: expected,
                min_reputation: 0,
                capabilities: vec!["compute".to_string()],
            },
        )
        .await?;
    anyhow::ensure!(
        posted.ok(),
        "could not post the compute task: {:?}",
        posted.body
    );
    let task_id = posted.body["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("no id on the posted task"))?
        .to_string();

    let claimed = hub
        .post_signed(
            worker,
            &format!("/tasks/{task_id}/claim"),
            ClaimPayload {
                task_id: task_id.clone(),
            },
        )
        .await?;
    anyhow::ensure!(
        claimed.ok(),
        "could not claim the compute task: {:?}",
        claimed.body
    );

    let submitted = hub
        .post_signed(
            worker,
            &format!("/tasks/{task_id}/submit"),
            SubmitPayload {
                task_id: task_id.clone(),
                output: CORRECT_ANSWER.to_string(),
            },
        )
        .await?;
    anyhow::ensure!(
        submitted.ok(),
        "could not submit the compute task: {:?}",
        submitted.body
    );

    // The compute credit lands with the payout's confirmation, which is
    // an operator payment and therefore paced by the chain, not the hub.
    let deadline = std::time::Instant::now() + SETTLEMENT_WAIT;
    while std::time::Instant::now() < deadline {
        let held = balances(hub, &worker.public_key()).await?.total_compute();
        if held > 0 {
            return Ok(held);
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    anyhow::bail!(
        "the compute task never settled, so this drill has no compute to sell and cannot make a \
         two-sided book -- fix the setup before reading any verdict from it"
    )
}

/// Everything the drill can see about the exchange, at one instant.
#[derive(Debug, Clone)]
struct Snapshot {
    accounts: BTreeMap<String, Balances>,
    orders: BTreeMap<String, Value>,
    trades: usize,
}

impl Snapshot {
    fn total_base(&self) -> u64 {
        self.accounts.values().map(Balances::total_base).sum()
    }

    fn total_compute(&self) -> u64 {
        self.accounts.values().map(Balances::total_compute).sum()
    }

    /// Owners holding locked balance that no open order accounts for.
    ///
    /// Reported as a list rather than a count so a failing run names the
    /// account, which is what an operator would need to go and find the
    /// money.
    fn stranded_locks(&self) -> Vec<String> {
        let implied = locks_implied_by(&self.orders);
        let mut stranded = Vec::new();
        for (owner, held) in &self.accounts {
            let (want_base, want_compute) = implied.get(owner).copied().unwrap_or((0, 0));
            if held.locked_base > want_base || held.locked_compute > want_compute {
                stranded.push(format!(
                    "{owner}: locked {}/{} base/compute against an open book worth {want_base}/{want_compute}",
                    held.locked_base, held.locked_compute
                ));
            }
        }
        stranded
    }

    /// Open orders the book is offering that their owner has not locked
    /// the funds for -- the mirror of a stranded lock, and the one that
    /// lets an order fill against money that is not there.
    fn unbacked_orders(&self) -> Vec<String> {
        let implied = locks_implied_by(&self.orders);
        let mut unbacked = Vec::new();
        for (owner, (want_base, want_compute)) in implied {
            let held = self.accounts.get(&owner).copied().unwrap_or_default();
            if held.locked_base < want_base || held.locked_compute < want_compute {
                unbacked.push(format!(
                    "{owner}: open book worth {want_base}/{want_compute} base/compute against \
                     locks of only {}/{}",
                    held.locked_base, held.locked_compute
                ));
            }
        }
        unbacked
    }
}

async fn snapshot(hub: &HubClient, watched: &[(String, PublicKey)]) -> Result<Snapshot> {
    let mut accounts = BTreeMap::new();
    for (label, pubkey) in watched {
        let _ = label;
        accounts.insert(pubkey.to_string(), balances(hub, pubkey).await?);
    }
    let trades = hub
        .get("/exchange/trades")
        .await?
        .body
        .as_array()
        .map(Vec::len)
        .unwrap_or(0);
    Ok(Snapshot {
        accounts,
        orders: open_orders(hub).await?,
        trades,
    })
}

/// How many crossing bids the kill phase puts in flight. Each one is a
/// fill, so each is an independent chance for the kill to land inside a
/// `place_order` that has committed some of itself and not the rest.
const BATCH: usize = 24;
/// Compute on each resting ask, and how many of them are rested.
///
/// Deliberately many small asks rather than a few large ones, and that
/// is the difference between a drill that discriminates and one that
/// does not. The window this phase hunts is *between* two of the old
/// code's commits, and the old code wrote one commit per trade and one
/// per resting order it filled — so a bid that sweeps ten asks wrote
/// about twenty-two commits where a bid that takes one wrote five. The
/// first version rested eight asks and had each bid take one, and it
/// reported INCONCLUSIVE against a pre-fix binary three runs out of
/// three: the vulnerable interval was a few microseconds inside a
/// handler lasting milliseconds, and eight samples never landed in it.
/// Making one bid cross ten asks widens the target by roughly the same
/// factor, and costs nothing on the fixed build, which writes one
/// commit however many orders it crossed.
const LOT: u64 = 25;
const ASKS: usize = 200;
/// Compute on each crossing bid: ten asks' worth.
const SWEEP: u64 = LOT * 25;
const LOT_PRICE: u64 = 10;

/// Fires a batch of fills and kills the hub in the middle of them, then
/// restarts and asks whether the ledger survived.
///
/// **This is the phase that discriminates**, and it samples rather than
/// asserts. The window it hunts is between two of `place_order`'s commits,
/// so whether a kill lands inside one is chance: finding nothing is a
/// failure to reproduce and not a clean bill, which is why a quiet run
/// reports `Inconclusive`.
///
/// The two observables are both point-in-time invariants, which matters
/// here: after a kill some fills completed and some did not, so comparing
/// against a *pre-kill* expectation of who owns what would be comparing
/// against a number the run is not entitled to. Neither of these needs to
/// know which fills landed.
///
/// **Conservation across the whole set of accounts.** A fill moves base
/// from buyer to seller and compute the other way, with the taker's fee
/// going to the operator in whichever asset the taker received. Every one
/// of those is a transfer, so the totals over buyer, seller and operator
/// are invariant under *any* number of fills — including zero, and
/// including a fill that was interrupted. A total that moves is money the
/// hub created or destroyed.
///
/// **Locked balance with no open order behind it.** The expensive half of
/// the fill bug: a maker order committed as `Filled` while the account
/// batch never landed leaves compute locked against an order that is no
/// longer `Open`, and cancelling refuses anything that is not `Open`.
async fn sigkill_phase(
    harness: &mut super::Harness,
    seller: &PrivateKey,
    buyer: &PrivateKey,
    operator: &PrivateKey,
    watched: &[(String, PublicKey)],
) -> Result<Section> {
    let _ = operator;
    let hub = harness.stack.hub_client()?;

    // Rest the asks first and let them settle onto disk, so the batch
    // below is purely about the fill path rather than about whether the
    // asks themselves were persisted.
    let mut asks = 0usize;
    for _ in 0..ASKS {
        let placed = hub
            .post_signed(
                seller,
                "/exchange/orders",
                PlaceOrderPayload {
                    side: "sell",
                    price: LOT_PRICE,
                    quantity: LOT,
                },
            )
            .await?;
        if placed.ok() {
            asks += 1;
        }
    }
    anyhow::ensure!(
        asks > 0,
        "the kill phase could not rest a single ask, so there was nothing for its bids to fill \
         against and no verdict can be read from it"
    );

    // Time the kill off a control fill, exactly as `escrow-restart` does:
    // one request through the same handler says how long it takes, and
    // the batch is then staggered across that duration so that at the
    // moment of the kill the requests are at different points in it.
    let control_started = std::time::Instant::now();
    let _ = hub
        .post_signed(
            buyer,
            "/exchange/orders",
            PlaceOrderPayload {
                side: "buy",
                price: LOT_PRICE,
                quantity: LOT,
            },
        )
        .await?;
    let handler_takes = control_started.elapsed().max(Duration::from_millis(5));

    let before = snapshot(&hub, watched).await?;

    let mut inflight = tokio::task::JoinSet::new();
    for index in 0..BATCH {
        let hub = hub.clone();
        let buyer = buyer.clone();
        let start_after = handler_takes.mul_f64(index as f64 / BATCH as f64);
        inflight.spawn(async move {
            tokio::time::sleep(start_after).await;
            hub.post_signed(
                &buyer,
                "/exchange/orders",
                PlaceOrderPayload {
                    side: "buy",
                    price: LOT_PRICE,
                    quantity: SWEEP,
                },
            )
            .await
        });
    }

    tokio::time::sleep(handler_takes).await;
    harness.stack.kill_hub().await?;

    let mut answered_ok = 0usize;
    let mut never_answered = 0usize;
    while let Some(result) = inflight.join_next().await {
        match result? {
            Ok(reply) if reply.ok() => answered_ok += 1,
            Ok(_) => {}
            Err(_) => never_answered += 1,
        }
    }

    harness.stack.start_hub().await?;
    let after = snapshot(&hub, watched).await?;

    let stranded = after.stranded_locks();
    let unbacked = after.unbacked_orders();
    let base_moved = after.total_base() != before.total_base();
    let compute_moved = after.total_compute() != before.total_compute();

    let mut section = Section::new("Kill the hub mid-fill, restart, and check the ledger survived")
        .plan_item("§6.5d")
        // A quiet run is a failure to reproduce, not a clean bill.
        .healthy_when(Verdict::Inconclusive)
        .fact("asks_rested", asks)
        .fact("asks_crossed_per_bid", SWEEP / LOT)
        .fact("bids_in_flight_at_the_kill", BATCH)
        .fact("bids_answered_ok", answered_ok)
        .fact("bids_never_answered", never_answered)
        .fact("total_base_before_kill", before.total_base())
        .fact("total_base_after_restart", after.total_base())
        .fact("total_compute_before_kill", before.total_compute())
        .fact("total_compute_after_restart", after.total_compute())
        .fact("stranded_locks_after_restart", stranded.len())
        .fact("unbacked_orders_after_restart", unbacked.len())
        .fact("handler_micros", handler_takes.as_micros() as u64);

    for detail in &stranded {
        section = section.finding(format!(
            "a kill mid-fill left locked balance with no open order behind it -- {detail}. \
             Cancelling refuses an order that is not Open, so nothing can release this again."
        ));
    }
    for detail in &unbacked {
        section = section.finding(format!(
            "a kill mid-fill left an open order its owner has not locked the funds for -- \
             {detail}. A fill against it debits money that is not there."
        ));
    }
    if base_moved {
        section = section.finding(format!(
            "base was created or destroyed across the kill: {} before, {} after. A fill is a \
             transfer, so this total is invariant under any number of them -- including \
             interrupted ones.",
            before.total_base(),
            after.total_base()
        ));
    }
    if compute_moved {
        section = section.finding(format!(
            "compute was created or destroyed across the kill: {} before, {} after.",
            before.total_compute(),
            after.total_compute()
        ));
    }

    let found_bug = !stranded.is_empty() || !unbacked.is_empty() || base_moved || compute_moved;
    section = section.verdict(if found_bug {
        // The plan's claim is that the fill is crash-safe. Finding a
        // broken ledger refutes it.
        Verdict::Refuted
    } else {
        Verdict::Inconclusive
    });
    if !found_bug {
        section = section.note(
            "No divergence found. That is consistent with the fix and does not establish it: the \
             window is between two commits and whether a kill lands inside one is chance. What \
             signs this off is the shape of the fix -- one transaction has no interval -- plus \
             the store-level test that a failure partway commits none of it, and an A/B against \
             a pre-fix binary.",
        );
    }
    Ok(section)
}

pub async fn run(repo: &Path, bin_dir: &Path, work_dir: PathBuf) -> Result<Report> {
    let mut harness = super::bring_up(repo, bin_dir, work_dir, true).await?;
    let chain = harness.stack.chain();
    let operator = harness.stack.operator_key.clone();

    let seller = PrivateKey::new_key();
    let buyer = PrivateKey::new_key();

    // Enough confirmed operator outputs to fund two deposits and pay a
    // bounty without queueing behind the payout ceiling (§6.4b).
    chain
        .wait_for_utxo_count(&operator.public_key(), 3, Duration::from_secs(300))
        .await?;
    let needed = (DEPOSIT + FEE) * 2;
    for who in [&seller, &buyer] {
        chain.pay(&operator, &who.public_key(), needed, FEE).await?;
    }
    for who in [&seller, &buyer] {
        let funded = chain
            .wait_for_balance(&who.public_key(), needed, Duration::from_secs(180))
            .await?;
        anyhow::ensure!(
            funded >= needed,
            "could not fund one of the drill's traders on chain"
        );
    }

    let hub = harness.stack.hub_client()?;
    for who in [&seller, &buyer] {
        fund_exchange_account(&hub, &chain, &operator, who).await?;
    }
    let compute_issued = issue_compute_to(&hub, &operator, &seller).await?;

    // Three things worth having in the state under test, each exercising
    // a different write path: a fill (both sides move, the fee sink
    // moves), a resting order left open (a lock that *should* survive),
    // and a cancellation (a lock that should be gone).
    let ask = hub
        .post_signed(
            &seller,
            "/exchange/orders",
            PlaceOrderPayload {
                side: "sell",
                price: 10,
                quantity: compute_issued.min(50),
            },
        )
        .await?;
    anyhow::ensure!(ask.ok(), "the resting ask was refused: {:?}", ask.body);

    let crossing = hub
        .post_signed(
            &buyer,
            "/exchange/orders",
            PlaceOrderPayload {
                side: "buy",
                price: 10,
                quantity: compute_issued.min(50),
            },
        )
        .await?;
    anyhow::ensure!(
        crossing.ok(),
        "the crossing bid was refused: {:?}",
        crossing.body
    );

    let resting = hub
        .post_signed(
            &buyer,
            "/exchange/orders",
            PlaceOrderPayload {
                side: "buy",
                price: 4,
                quantity: 25,
            },
        )
        .await?;
    anyhow::ensure!(
        resting.ok(),
        "the resting bid was refused: {:?}",
        resting.body
    );

    let doomed = hub
        .post_signed(
            &buyer,
            "/exchange/orders",
            PlaceOrderPayload {
                side: "buy",
                price: 3,
                quantity: 10,
            },
        )
        .await?;
    anyhow::ensure!(
        doomed.ok(),
        "the bid that was going to be cancelled was refused: {:?}",
        doomed.body
    );
    let doomed_id = doomed.body["id"].as_str().unwrap_or_default().to_string();
    let cancelled = hub
        .post_signed(
            &buyer,
            &format!("/exchange/orders/{doomed_id}/cancel"),
            CancelOrderPayload {
                order_id: doomed_id.clone(),
            },
        )
        .await?;
    anyhow::ensure!(
        cancelled.ok(),
        "cancelling an order failed: {:?}",
        cancelled.body
    );

    let watched = vec![
        ("seller".to_string(), seller.public_key()),
        ("buyer".to_string(), buyer.public_key()),
        ("operator".to_string(), operator.public_key()),
    ];
    let before = snapshot(&hub, &watched).await?;

    // A graceful restart, not a kill. The question is what the store
    // holds, so a clean stop makes the answer about durability rather
    // than about whether a drain finished -- and a kill would turn a
    // verdict into a sample. See the module note.
    harness.stack.stop_hub().await?;
    harness.stack.start_hub().await?;

    let after = snapshot(&hub, &watched).await?;

    let base_before = before.total_base();
    let base_after = after.total_base();
    let compute_before = before.total_compute();
    let compute_after = after.total_compute();
    let stranded = after.stranded_locks();
    let unbacked = after.unbacked_orders();

    let mut section = Section::new("Trade, restart the hub cleanly, and check the ledger balances")
        .plan_item("§6.5d")
        .healthy_when(Verdict::Confirmed)
        // Said in the report and not only in the module doc, because a
        // reader looking at a CONFIRMED here would otherwise take it for
        // evidence the fill bug is fixed. It is not: this phase reports
        // CONFIRMED against a pre-fix hub too, measured. A graceful
        // restart never interrupts a commit, and seven commits that all
        // succeed leave the same state one commit leaves. The `sigkill`
        // section below is the one that discriminates.
        .accepted_finding(
            "this phase does not demonstrate the §6.5d fill bug and cannot: it passed against a \
             pre-fix binary in the A/B. It is kept as a standing invariant check for the ordinary \
             path -- a future change that breaks conservation without a crash shows up here.",
        )
        .fact("compute_issued_by_settlement", compute_issued)
        .fact("trades_before_restart", before.trades)
        .fact("trades_after_restart", after.trades)
        .fact("open_orders_before_restart", before.orders.len())
        .fact("open_orders_after_restart", after.orders.len())
        .fact("total_base_before_restart", base_before)
        .fact("total_base_after_restart", base_after)
        .fact("total_compute_before_restart", compute_before)
        .fact("total_compute_after_restart", compute_after)
        .fact("stranded_locks_after_restart", stranded.len())
        .fact("unbacked_orders_after_restart", unbacked.len());

    anyhow::ensure!(
        before.trades > 0,
        "no trade ever executed, so this drill never built the state it measures -- the crossing \
         order did not fill and every verdict below would be about an empty exchange"
    );

    for detail in &stranded {
        section = section.finding(format!(
            "locked balance survives a restart with no open order behind it -- {detail}. Because \
             cancelling refuses an order that is not Open, nothing can release this again."
        ));
    }
    for detail in &unbacked {
        section = section.finding(format!(
            "an open order is on the book that its owner has not locked the funds for -- {detail}. \
             A fill against it debits money that is not there."
        ));
    }
    if base_after != base_before {
        section = section.finding(format!(
            "the base ledger did not survive the restart: {base_before} before, {base_after} \
             after. A restart moves no money, so the difference is state that reached memory and \
             not disk."
        ));
    }
    if compute_after != compute_before {
        section = section.finding(format!(
            "the compute ledger did not survive the restart: {compute_before} before, \
             {compute_after} after."
        ));
    }
    if after.trades != before.trades {
        section = section.finding(format!(
            "the trade tape changed across a restart: {} before, {} after -- a fill that reached \
             memory and not disk, or one recorded twice.",
            before.trades, after.trades
        ));
    }
    if after.orders.len() != before.orders.len() {
        section = section.finding(format!(
            "the open book changed across a restart: {} orders before, {} after.",
            before.orders.len(),
            after.orders.len()
        ));
    }

    let healthy = stranded.is_empty()
        && unbacked.is_empty()
        && base_after == base_before
        && compute_after == compute_before
        && after.trades == before.trades
        && after.orders.len() == before.orders.len();
    section = section.verdict(if healthy {
        Verdict::Confirmed
    } else {
        Verdict::Refuted
    });

    // The phase that can actually see the bug. Runs on the same stack,
    // after the clean-restart verdict has been taken, because it ends by
    // killing the hub and there is no point paying for a second chain.
    let kill = sigkill_phase(&mut harness, &seller, &buyer, &operator, &watched).await;

    harness.stack.shutdown().await;

    let mut report = Report::new("drill: exchange-restart", harness.environment);
    report.push(section);
    report.push(kill?);
    Ok(report)
}

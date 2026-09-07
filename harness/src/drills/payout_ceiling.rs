//! Collapse the operator's wallet to a single output, restart the hub
//! into it, and count how many payouts land per block.
//!
//! # What this used to measure, and what it measures now
//!
//! Plan §6.4b predicted about one payment per block and said why: every
//! hub payment spends the operator's outputs and sends change back to
//! itself, that change is unconfirmed until mined, and
//! `build_multi_payment` skips outputs the mempool has already spoken
//! for. So immediately after a payout the operator's *spendable* balance
//! can be zero even though its total is untouched. This drill confirmed
//! it exactly: 31 grants across 30 blocks, never two at any height,
//! against 723 offered.
//!
//! The hub now fans its wallet out (`hub/src/operator_wallet.rs`), so
//! that prediction is one this drill is expected to **refute** --
//! `healthy_when(Refuted)`, the same shape `node-crash` took once §6.5
//! landed. The section keeps its title and its fact keys so the
//! comparison against the pre-fix baseline is the sign-off.
//!
//! # The condition still has to be constructed
//!
//! A local stack's miner pays the operator a fresh 50-coin output every
//! block, which is precisely the many-output wallet the ceiling is
//! invisible on. So the drill moves the miner onto a key nothing else
//! uses, then has the operator pay itself its whole confirmed balance
//! minus the fee, leaving exactly one output and no change.
//!
//! **The hub is stopped for that collapse, and this is not tidiness.**
//! The hub now spends the operator's outputs on its own account, so a
//! collapse racing a fan-out never reaches "exactly one output" and the
//! drill's setup would time out for a reason that is the fix working.
//! Stopping the hub also buys the more interesting measurement: the hub
//! restarts into a genuinely single-output wallet, which is what a
//! freshly deployed hub is, so `cold_start_blocks` is the real cost of
//! that state rather than a simulation of it.

use crate::chain::ChainView;
use crate::client::claim_faucet;
use crate::report::{Report, Section, Verdict};
use crate::stats::{summarize, Sample};
use anyhow::Result;
use btclib::crypto::{PrivateKey, PublicKey};
use btclib::payment::build_payment;
use btclib::types::TransactionOutput;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const FEE: u64 = 1_000;
/// What one faucet grant costs the operator, mirroring the hub's
/// `FAUCET_GRANT_AMOUNT`. Only used to tell "the wallet is empty" from
/// "the wallet is badly shaped", which is the distinction this drill
/// exists to make and got wrong once.
const GRANT: u64 = 50_000_000;
/// How long to keep asking for payouts. Several blocks at a sixteen-second
/// target, which is enough to tell one-per-block from many-per-block
/// without making the drill take a coffee break.
const MEASURE_FOR: Duration = Duration::from_secs(150);
/// Pause between attempts.
///
/// What matters is not the absolute rate but how many payouts are offered
/// per *block*, since that is the thing the ceiling is measured against.
/// A run that offers barely more than the ceiling cannot tell one ceiling
/// from another, so `attempts_per_block` is reported and the verdict is
/// withheld when it comes out too low to have falsified anything.
///
/// Halved from the 200ms that measured the original ceiling: the hub now
/// keeps two dozen spendable outputs, so a run has to offer several dozen
/// payouts per block before it is asking a question the wallet could
/// fail.
const ATTEMPT_EVERY: Duration = Duration::from_millis(100);
/// How much confirmed operator coin to accumulate before collapsing.
///
/// A *balance*, not an output count, and both halves of that are lessons
/// from a run.
///
/// Enough that the run cannot end by spending it: this drill was written
/// against a ceiling of one payout per block, where three coinbase
/// outputs -- 150 coins, 300 grants -- was ample. Against a fanned-out
/// wallet it offers forty a block, and with three it drained the
/// operator to 49,700,000 units, just under one more grant, whereupon
/// sixteen further blocks granted nothing and pulled `payouts_per_block`
/// from 24 down to 9.97. That number described the drill's budget, not
/// the hub. Eight hundred coins covers the whole run at the ceiling with
/// room over.
///
/// And a balance rather than `wait_for_utxo_count`, which is what a
/// drill needing several payouts in flight would ordinarily wait on: the
/// hub now *makes* its own outputs, so a count is satisfied within
/// seconds by the boot fan-out splitting whatever little the operator
/// happens to hold. Asking for twelve outputs got two coinbases' worth
/// of coin in twenty-four pieces, and the run ran dry exactly as before.
const FUNDING_TARGET: u64 = 80_000_000_000;
/// How long to wait for the hub to notice a one-output wallet and split
/// it. Its sweep runs every sixty seconds and its boot pass runs
/// immediately, so this is generous by design -- a drill that gave up at
/// the sweep interval would be reporting its own impatience.
const FAN_OUT_TIMEOUT: Duration = Duration::from_secs(180);

/// Collapses `key`'s confirmed outputs into exactly one, by having it pay
/// itself everything minus the fee. Returns the amount now sitting in that
/// single output.
///
/// Only correct while nothing else is spending `key`. See the module
/// docs on why the hub is stopped around this.
async fn collapse_to_one_output(chain: &ChainView, key: &PrivateKey) -> Result<u64> {
    let utxos = chain.utxos(&key.public_key()).await?;
    let available: Vec<(bool, TransactionOutput)> = utxos
        .into_iter()
        .map(|(output, marked)| (marked, output))
        .collect();
    let spendable: u64 = available
        .iter()
        .filter(|(marked, _)| !marked)
        .map(|(_, output)| output.value)
        .sum();
    anyhow::ensure!(spendable > FEE, "the operator has nothing to collapse");

    // Paying the whole balance minus the fee leaves no change, so the
    // transaction has exactly one output. Any leftover at all would
    // produce a second one and the drill would be measuring a two-output
    // wallet.
    let amount = spendable - FEE;
    let transaction = build_payment(
        &available,
        key,
        key.public_key(),
        amount,
        FEE,
        key.public_key(),
    )?;
    chain.submit(transaction).await?;

    let deadline = Instant::now() + Duration::from_secs(240);
    loop {
        let confirmed: Vec<_> = chain
            .utxos(&key.public_key())
            .await?
            .into_iter()
            .filter(|(_, marked)| !marked)
            .collect();
        if confirmed.len() == 1 {
            return Ok(confirmed[0].0.value);
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "the operator still holds {} confirmed outputs; the collapse did not mine",
            confirmed.len()
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// What the hub did about the single-output wallet it was restarted into.
struct FanOut {
    /// Spendable outputs once it settled. One means it never happened.
    outputs: usize,
    /// Blocks between the hub coming up and the wallet being usable
    /// again -- the cold-start cost, and the one price the fan-out
    /// charges that the old behaviour did not.
    blocks: u32,
}

/// Waits for the operator to hold more than one spendable output, and for
/// that number to stop moving.
///
/// Settling for two consecutive polls rather than returning on the first
/// change, because the count also moves when a *payout* confirms; the
/// split itself lands whole, in one transaction in one block, so one
/// steady reading is enough to have caught all of it.
async fn wait_for_fan_out(
    chain: &ChainView,
    pubkey: &PublicKey,
    started_at_height: u32,
    timeout: Duration,
) -> Result<FanOut> {
    let deadline = Instant::now() + timeout;
    let mut previous = 0usize;
    loop {
        let outputs = chain
            .utxos(pubkey)
            .await
            .map(|utxos| utxos.iter().filter(|(_, marked)| !marked).count())
            .unwrap_or(0);
        let settled = outputs > 1 && outputs == previous;
        if settled || Instant::now() >= deadline {
            let height = chain.height().await.unwrap_or(started_at_height);
            return Ok(FanOut {
                outputs,
                blocks: height.saturating_sub(started_at_height),
            });
        }
        previous = outputs;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

pub async fn run(repo: &Path, bin_dir: &Path, work_dir: PathBuf) -> Result<Report> {
    let mut harness = super::bring_up(repo, bin_dir, work_dir, true).await?;
    let chain = harness.stack.chain();
    let hub = harness.stack.hub_client()?;
    let operator = harness.stack.operator_key.clone();

    // Let the operator accumulate something to collapse. See
    // `FUNDING_TARGET` -- this is a budget, and a run that spends it is
    // measuring the budget.
    let funded = chain
        .wait_for_balance(&operator.public_key(), FUNDING_TARGET, Duration::from_secs(600))
        .await?;
    anyhow::ensure!(
        funded >= FUNDING_TARGET,
        "the operator only reached {funded} of {FUNDING_TARGET}; the run would end by \
         running out of money rather than by finding a ceiling"
    );

    // Move the miner off the operator, or every block would hand it a new
    // spendable output and quietly lift the ceiling being measured.
    harness.stack.mint_key("miner")?;
    harness.stack.kill_miner().await?;
    harness.stack.start_miner_paying("./miner.pub.pem").await?;

    // SIGTERM rather than SIGKILL: the collapse wants the hub's hands off
    // the wallet, not a crash to recover from. Crash recovery is
    // `escrow-restart`'s subject, and mixing the two would make a failure
    // here ambiguous between them.
    harness.stack.stop_hub().await?;
    let single_output = collapse_to_one_output(&chain, &operator).await?;

    let restarted_at = chain.height().await?;
    harness.stack.start_hub().await?;
    let fan_out =
        wait_for_fan_out(&chain, &operator.public_key(), restarted_at, FAN_OUT_TIMEOUT).await?;

    let mut samples = Vec::new();
    let mut per_height: BTreeMap<u32, usize> = BTreeMap::new();
    let mut attempts = 0usize;
    let mut granted = 0usize;
    let mut refused_for_balance = 0usize;
    let start_height = chain.height().await?;

    let measuring_from = Instant::now();
    let deadline = measuring_from + MEASURE_FOR;
    let mut index = 0usize;
    let mut exhausted = false;
    while Instant::now() < deadline {
        // Stop when the operator can no longer fund a grant from
        // anything it holds, confirmed or not. Past that point every
        // further block grants nothing and drags the per-block average
        // down, and the resulting number describes how much coin the
        // drill was given rather than how many payouts the wallet can
        // make. Reported either way, because "it ran out" is itself
        // worth knowing.
        if chain.total_balance(&operator.public_key()).await.unwrap_or(u64::MAX) < GRANT + FEE {
            exhausted = true;
            break;
        }
        let key = PrivateKey::new_key();
        // Its own source address per attempt: the faucet is chain-tier at
        // twenty a minute per address, and this drill offers far more than
        // that on purpose. Being throttled by the limiter instead of by
        // the wallet would answer a different question.
        let client = hub.from_source(&format!("10.5.{}.{}", index / 250, index % 250));
        index += 1;

        let reply = claim_faucet(&client, &key).await?;
        samples.push(Sample::new("POST /faucet", reply.status, reply.latency));
        attempts += 1;

        if reply.ok() {
            granted += 1;
            let height = chain.height().await.unwrap_or(start_height);
            *per_height.entry(height).or_default() += 1;
        } else if reply.status == 503 {
            // The shape of this refusal changed with the fix. It used to
            // be a 500 whose text carried "insufficient", which is what
            // this counted; the hub now answers a grant it cannot fund
            // with a 503 and a `Retry-After`, and matching the status is
            // both narrower and stable against the wording.
            refused_for_balance += 1;
        }
        tokio::time::sleep(ATTEMPT_EVERY).await;
    }
    let measured_for = measuring_from.elapsed();

    let end_height = chain.height().await?;
    let blocks = (end_height - start_height).max(1);
    let per_block = granted as f64 / blocks as f64;
    let offered_per_block = attempts as f64 / blocks as f64;
    let busiest_block = per_height.values().copied().max().unwrap_or(0);
    let per_minute = granted as f64 * 60.0 / measured_for.as_secs_f64().max(1.0);

    harness.stack.shutdown().await;

    // §6.4b's claim, unchanged: about one payment per block, and never
    // two at a height. Anything up to two is that claim holding -- a
    // payout landing just either side of a block boundary is ordinary.
    let ceiling_holds = per_block <= 2.0 && busiest_block <= 2;
    // A run that offered barely more than the ceiling cannot distinguish
    // one ceiling from another. Say so rather than reporting a number on
    // evidence that could not have falsified anything.
    let offered_enough = offered_per_block >= 3.0;

    let mut section = Section::new("Payout ceiling with a single operator output")
        .plan_item("§6.4b")
        // The plan predicted a limitation and the hub now lifts it, so
        // the healthy answer here is `Refuted` -- see
        // `report::Section::healthy_verdict`. Against the pre-fix
        // baseline this reads as confirmed -> refuted, which `compare`
        // scores as the fix landing rather than as a regression.
        .healthy_when(Verdict::Refuted)
        .fact("single_output_value", single_output)
        .fact("cold_start_blocks", fan_out.blocks)
        .fact("spendable_outputs_after_fan_out", fan_out.outputs)
        .fact("attempts", attempts)
        .fact("payouts_granted", granted)
        .fact("refused_for_balance", refused_for_balance)
        .fact("blocks_elapsed", blocks)
        .fact("attempts_per_block", (offered_per_block * 100.0).round() / 100.0)
        .fact("payouts_per_block", (per_block * 100.0).round() / 100.0)
        .fact("grants_per_minute", (per_minute * 10.0).round() / 10.0)
        .fact("busiest_single_block", busiest_block)
        .fact("operator_ran_out_of_coin", exhausted)
        .fact(
            "payouts_by_height",
            json!(per_height
                .iter()
                .map(|(h, c)| (h.to_string(), *c))
                .collect::<BTreeMap<_, _>>()),
        )
        .latency(summarize(&samples, None))
        .note(format!(
            "The miner was moved off the operator and the operator's wallet collapsed to a \
             single {single_output}-unit output before measuring, because a wallet holding one \
             coinbase output per block does not have this ceiling at all. The hub was stopped \
             for the collapse and restarted into that wallet, which is what a freshly deployed \
             hub holds."
        ))
        .note(format!(
            "The hub took {} block(s) after restart to reach {} spendable output(s). That is \
             the fan-out's own cost and the one thing it makes worse: a wallet with a single \
             output has nothing to pay from while the split is unconfirmed, so a cold hub \
             cannot pay at all for one block. It is spent before the listener opens.",
            fan_out.blocks, fan_out.outputs
        ))
        .note(
            "The `POST /faucet` latency here covers the whole flow -- challenge, solve, \
             redeem -- and the stack runs the puzzle at 64 expected hashes. The pre-fix \
             baseline timed a single payload-less POST against a hub that had no proof of work \
             yet, so the two latency figures are not comparable and neither is evidence about \
             the other.",
        );

    if exhausted {
        section = section.note(
            "The run stopped early: the operator spent everything it had. The numbers above \
             cover only the funded window, which is the point -- blocks granting nothing \
             because the wallet is empty say nothing about its shape.",
        );
    }

    if fan_out.outputs <= 1 {
        section = section.finding(format!(
            "The hub never split its wallet: {} block(s) after restarting into a single output \
             it still held {}. The fan-out is what lifts this ceiling, so every number below is \
             a measurement of a hub that is not running it -- check \
             `hub_operator_fan_out_failures_total` and whether the node was reachable at boot.",
            fan_out.blocks, fan_out.outputs
        ));
    }

    section = if !offered_enough {
        section.verdict(Verdict::Inconclusive).note(format!(
            "The run offered only {offered_per_block:.2} payouts per block, which cannot tell \
             one ceiling from another -- any of them would produce roughly this result. Blocks \
             on a young test chain arrive far faster than the sixteen-second target. Shorten \
             ATTEMPT_EVERY or lengthen the run and re-run before believing the number."
        ))
    } else if ceiling_holds {
        section.verdict(Verdict::Confirmed).finding(format!(
            "{granted} payouts landed across {blocks} blocks ({per_block:.2} per block, busiest \
             block {busiest_block}) against {attempts} offered. That is the ceiling §6.4b \
             measured, still in place: either the operator's wallet is not being kept fanned \
             out, or payments are selecting outputs that leave nothing behind. This is a \
             regression, not a confirmation."
        ))
    } else {
        section.verdict(Verdict::Refuted).note(format!(
            "{granted} payouts landed across {blocks} blocks ({per_block:.2} per block, \
             {per_minute:.1} a minute, busiest block {busiest_block}) against {attempts} \
             offered, with {refused_for_balance} refused for want of a funded output. §6.4b's \
             one-per-block is refuted, which is the point: the operator's wallet is kept split \
             across many confirmed outputs, so a payment always has one to spend and its change \
             does not block the next."
        ))
    };

    let mut report = Report::new("drill: payout-ceiling", harness.environment);
    report.push(section);
    Ok(report)
}

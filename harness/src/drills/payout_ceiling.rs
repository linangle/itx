//! Fill the operator's wallet with a single large output, then drive
//! payouts and count how many land per block.
//!
//! Plan §6.4b predicts about one payment per block, and says why: every
//! hub payment spends the operator's outputs and sends change back to
//! itself, that change is unconfirmed until mined, and `build_multi_payment`
//! skips outputs the mempool has already spoken for. So immediately after
//! a payout the operator's *spendable* balance can be zero even though its
//! total is untouched, and the next payment has nothing to select.
//!
//! # The condition has to be constructed, and that is the whole difficulty
//!
//! The plan also says this is "invisible on a wallet that happens to hold
//! many coinbase outputs" -- and a local stack is exactly such a wallet,
//! because the miner pays the operator a fresh 50-coin output every block.
//! A drill that just fires payouts at a default stack measures a wallet
//! shape no deployment has and reports no ceiling at all.
//!
//! So this drill does two things first. It moves the miner off the
//! operator onto a key nothing else uses, so no new coinbase outputs
//! arrive; then it has the operator pay itself its entire confirmed
//! balance minus the fee, which leaves exactly one output and no change.
//! Only then does it start measuring.
//!
//! The faucet is used as the payout probe because one claim is exactly one
//! operator payment with no task machinery in the way. That call is
//! isolated in `client::claim_faucet` -- see the note there about the
//! proof-of-work challenge that is being added to it.

use crate::chain::ChainView;
use crate::client::claim_faucet;
use crate::report::{Report, Section, Verdict};
use crate::stats::{summarize, Sample};
use anyhow::Result;
use btclib::crypto::PrivateKey;
use btclib::payment::build_payment;
use btclib::types::TransactionOutput;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const FEE: u64 = 1_000;
/// How long to keep asking for payouts. Several blocks at a sixteen-second
/// target, which is enough to tell one-per-block from many-per-block
/// without making the drill take a coffee break.
const MEASURE_FOR: Duration = Duration::from_secs(150);
/// Pause between attempts.
///
/// What matters is not the absolute rate but how many payouts are offered
/// per *block*, since that is the thing the ceiling is measured against.
/// A run that offers barely more than one per block cannot tell a ceiling
/// of one from a ceiling of three, so `attempts_per_block` is reported
/// and the verdict is withheld when it comes out too low to have
/// falsified anything. At 200ms against the block times this chain
/// actually produces, it lands around twenty-five offered per block.
const ATTEMPT_EVERY: Duration = Duration::from_millis(200);

/// Collapses `key`'s confirmed outputs into exactly one, by having it pay
/// itself everything minus the fee. Returns the amount now sitting in that
/// single output.
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

pub async fn run(repo: &Path, bin_dir: &Path, work_dir: PathBuf) -> Result<Report> {
    let mut harness = super::bring_up(repo, bin_dir, work_dir, true).await?;
    let chain = harness.stack.chain();
    let hub = harness.stack.hub_client()?;
    let operator = harness.stack.operator_key.clone();

    // Let the operator accumulate something to collapse.
    chain
        .wait_for_utxo_count(&operator.public_key(), 3, Duration::from_secs(300))
        .await?;

    // Move the miner off the operator, or every block would hand it a new
    // spendable output and quietly lift the ceiling being measured.
    harness.stack.mint_key("miner")?;
    harness.stack.kill_miner().await?;
    harness.stack.start_miner_paying("./miner.pub.pem").await?;

    let single_output = collapse_to_one_output(&chain, &operator).await?;

    let mut samples = Vec::new();
    let mut per_height: BTreeMap<u32, usize> = BTreeMap::new();
    let mut attempts = 0usize;
    let mut granted = 0usize;
    let mut refused_for_balance = 0usize;
    let start_height = chain.height().await?;

    let deadline = Instant::now() + MEASURE_FOR;
    let mut index = 0usize;
    while Instant::now() < deadline {
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
        } else if reply.error_text().contains("insufficient")
            || reply.error_text().contains("balance")
        {
            refused_for_balance += 1;
        }
        tokio::time::sleep(ATTEMPT_EVERY).await;
    }

    let end_height = chain.height().await?;
    let blocks = (end_height - start_height).max(1);
    let per_block = granted as f64 / blocks as f64;
    let offered_per_block = attempts as f64 / blocks as f64;
    let busiest_block = per_height.values().copied().max().unwrap_or(0);

    harness.stack.shutdown().await;

    // "About one per block" is the claim. Anything up to two is that claim
    // holding -- a payout landing just either side of a block boundary is
    // ordinary. Materially more than that is the claim failing.
    let ceiling_holds = per_block <= 2.0 && busiest_block <= 2;
    // A run that offered barely more than one payout per block cannot
    // distinguish a ceiling of one from a ceiling of three. Say so rather
    // than confirming the plan on evidence that could not have refuted it.
    let offered_enough = offered_per_block >= 3.0;

    let mut section = Section::new("Payout ceiling with a single operator output")
        .plan_item("§6.4b")
        .fact("single_output_value", single_output)
        .fact("attempts", attempts)
        .fact("payouts_granted", granted)
        .fact("refused_for_balance", refused_for_balance)
        .fact("blocks_elapsed", blocks)
        .fact("attempts_per_block", (offered_per_block * 100.0).round() / 100.0)
        .fact("payouts_per_block", (per_block * 100.0).round() / 100.0)
        .fact("busiest_single_block", busiest_block)
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
             coinbase output per block does not have this ceiling at all."
        ));

    section = if !offered_enough {
        section.verdict(Verdict::Inconclusive).note(format!(
            "The run offered only {offered_per_block:.2} payouts per block, which cannot tell a \
             ceiling of one from a ceiling of three -- both would produce roughly this result. \
             Blocks on a young test chain arrive far faster than the sixteen-second target. \
             Shorten ATTEMPT_EVERY or lengthen the run and re-run before believing the number."
        ))
    } else if ceiling_holds {
        section.verdict(Verdict::Confirmed).note(format!(
            "{granted} payouts landed across {blocks} blocks ({per_block:.2} per block, busiest \
             block {busiest_block}) against {attempts} offered. The operator's change is \
             unconfirmed until mined, so the wallet has nothing to select from until the next \
             block arrives."
        ))
    } else {
        section.verdict(Verdict::Refuted).note(format!(
            "{granted} payouts landed across {blocks} blocks ({per_block:.2} per block, busiest \
             block {busiest_block}). That is materially more than the one per block §6.4b \
             predicts, so the ceiling as written is wrong and the section needs correcting."
        ))
    };

    let mut report = Report::new("drill: payout-ceiling", harness.environment);
    report.push(section);
    Ok(report)
}

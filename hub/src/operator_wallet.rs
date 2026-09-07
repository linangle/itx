//! Keeping the operator's wallet split across many spendable outputs,
//! and choosing which one to spend.
//!
//! # The ceiling this exists to lift
//!
//! Plan §6.4b, measured rather than theorised. A UTXO only enters this
//! chain's spendable set when the block containing it is mined -- a
//! transaction sitting in the mempool contributes nothing but *marks*
//! the inputs it spends, so `build_multi_payment` skips them. Every hub
//! payment spends the operator's outputs and sends the change back to
//! the operator, which means the change is invisible until the next
//! block. With a wallet holding one output, the first payout consumes
//! it and the operator's spendable balance is zero until that payout is
//! mined: `harness drill payout-ceiling` measured exactly one grant per
//! block, 31 across 30 blocks, against 723 offered.
//!
//! Nothing about that is a locking or throughput problem, so no amount
//! of concurrency fixes it. The bound is the *number of confirmed
//! outputs the operator holds*, and this module is about holding more of
//! them.
//!
//! # Two halves
//!
//! **Choosing.** `ordered_for_payment` decides which output a payment
//! spends, by handing `build_multi_payment` its candidates in the order
//! it should walk them. That function selects greedily from the front
//! and stops as soon as it has enough, so ordering *is* the selection
//! policy and no change to `btclib` is needed. The policy: spend the
//! smallest single output that covers the payment, so the large ones
//! stay whole and stay spendable; when no single output covers it, take
//! the largest first so the payment consumes as few of them as it can.
//! Left unordered, selection was whatever order the node's `HashMap`
//! happened to iterate in, which routinely turned a 150-coin output
//! into a 0.5-coin payment and 149.5 coins of invisible change.
//!
//! **Refilling.** `plan_reshape` decides when to split a big output into
//! slots, and when to sweep worn-out change back into one. A fan-out
//! that runs once is a fan-out that expires, so this runs on the sweep's
//! sixty-second cadence and at boot, and its input is only ever the
//! wallet as the node currently reports it -- there is no state to keep
//! in sync and nothing to reconcile after a restart.
//!
//! # What was rejected
//!
//! *Fixed denominations.* Splitting into outputs of a constant size
//! leaves a remainder that is either dust or another oversized blob,
//! and converges to a count set by the remainder rather than by the
//! floor. Equal shares of whatever is being split have neither problem
//! and need no tuning as the operator's balance changes by orders of
//! magnitude between a test stack and a deployment.
//!
//! *Topping up from the payout path.* Reacting to an
//! insufficient-balance failure would be more responsive than a
//! sixty-second sweep, but it puts a node round trip inside the payout
//! lock on a path an unauthenticated caller can drive, and the burst it
//! would fire during is exactly when the hub can least afford it. The
//! sweep is slower and cannot be provoked.
//!
//! *Batching payouts into one multi-recipient transaction* (§6.4b's
//! second mitigation) is real and complementary, but it helps where
//! several payments are owed at once -- the sweep's case. A faucet
//! grant arrives alone, and the faucet is the path this ceiling is
//! measured on.

use btclib::types::TransactionOutput;
use std::cmp::Reverse;

/// How many spendable outputs the operator's wallet is kept split into.
///
/// This is the ceiling, near enough: one payout consumes one output and
/// returns its change unconfirmed, so the hub can make about this many
/// payments between blocks. At the chain's sixteen-second target that
/// is roughly ninety a minute against the four a minute a single-output
/// wallet manages.
///
/// Twenty-four rather than a larger number because every payment fetches
/// the operator's whole UTXO set over the node protocol to select from
/// it, so the fan is not free, and because the faucet -- the thing that
/// actually arrives in bursts -- is scheduled to be retired (§5.1).
/// `--operator-wallet-outputs` raises it for a hub that needs more.
pub const DEFAULT_WALLET_OUTPUTS: usize = 24;

/// The smallest output worth counting toward the floor.
///
/// An output that cannot cover the hub's most common payment is not a
/// slot in the fan, it is change waiting to be re-split -- counting it
/// would let the wallet report itself full while every payout failed.
/// Two faucet grants and their fees, rounded to one whole coin, which
/// also keeps a slot useful for a second payment after the first
/// erodes it.
///
/// A payout larger than this is not refused: `ordered_for_payment` falls
/// back to combining outputs largest-first. It costs the fan a slot per
/// extra input, which the next fan-out restores.
pub const MIN_USEFUL_OUTPUT: u64 = 100_000_000;

/// Orders `utxos` so `btclib::payment::build_multi_payment`'s greedy
/// front-to-back walk selects well. See the module docs for the policy;
/// the ordering is total and deterministic (value, then `unique_id`) so
/// two hubs -- or one hub twice -- given the same wallet make the same
/// choice, which is what makes the behaviour testable at all given the
/// node reports its UTXOs from a `HashMap`.
///
/// Marked outputs (already spoken for by something in the node's
/// mempool) are left in the list rather than filtered out, because
/// `build_multi_payment` skips them itself and removing them here would
/// make this function's contract differ from the one caller it has.
pub fn ordered_for_payment(
    utxos: &[(bool, TransactionOutput)],
    total_needed: u64,
) -> Vec<(bool, TransactionOutput)> {
    let mut ordered = utxos.to_vec();
    // Largest first: when no single output covers the payment, this is
    // the order that consumes the fewest of them.
    ordered.sort_by_key(|(_, output)| (Reverse(output.value), output.unique_id));
    // But if one output does cover it, spend the smallest such one and
    // leave the rest whole. This is the whole point: a payment that
    // eats the biggest output turns the entire remaining balance into
    // change nothing can see until the next block.
    let best_fit = ordered
        .iter()
        .enumerate()
        .filter(|(_, (marked, output))| !*marked && output.value >= total_needed)
        .min_by_key(|(_, (_, output))| (output.value, output.unique_id))
        .map(|(index, _)| index);
    if let Some(index) = best_fit {
        let chosen = ordered.remove(index);
        ordered.insert(0, chosen);
    }
    ordered
}

/// How many of `utxos` are confirmed, unspoken-for, and large enough to
/// fund a payment on their own -- the number the ceiling actually is.
pub fn ready_outputs(utxos: &[(bool, TransactionOutput)]) -> usize {
    utxos
        .iter()
        .filter(|(marked, output)| !*marked && output.value >= MIN_USEFUL_OUTPUT)
        .count()
}

/// One self-paying transaction: spend `inputs` and pay the operator back
/// in `shares` pieces.
#[derive(Debug, Clone)]
pub struct Reshape {
    /// The outputs being consumed.
    pub inputs: Vec<TransactionOutput>,
    /// What to pay the operator, one entry per new output. Sums to the
    /// inputs' total minus the fee exactly, so the transaction has no
    /// change output: a change output here would be a piece the plan did
    /// not choose the size of, and on a small wallet it would be dust --
    /// which is the very thing this is trying to clear up.
    pub shares: Vec<u64>,
}

/// Decides whether the operator's wallet needs reshaping, and how.
///
/// `None` means leave it alone -- either the floor is met, or nothing on
/// hand can be usefully reshaped. Both are ordinary; this runs every
/// sweep and does nothing on nearly all of them.
///
/// # Two moves, and the order matters
///
/// **Consolidate first.** Every payment leaves change, and change that
/// has fallen below `MIN_USEFUL_OUTPUT` is not a slot in the fan -- it
/// is material. Sweeping that material into one usable output costs one
/// fee and strictly *raises* the count, so it is always the better move
/// when it is available.
///
/// Without this the wallet has a trap in it. A wallet whose outputs have
/// all eroded below the line has plenty of balance and no output big
/// enough to split, so a splitter-only planner returns `None` forever
/// while payments grind on through ever-smaller combinations of ever-
/// more inputs. Nothing recovers it; the shape only ever gets worse.
///
/// **Split second**, and only the largest output, sweeping any leftover
/// dust in with it. Splitting several would spend outputs that are
/// already doing their job, and on the pass after a reshape the new
/// pieces are not visible yet (they are unconfirmed), so a plan that
/// kept going would reshape a wallet it had already reshaped -- see
/// `AppState::operator_fan_out_inflight` for the other half of that
/// guard.
pub fn plan_reshape(
    utxos: &[(bool, TransactionOutput)],
    fee: u64,
    floor: usize,
) -> Option<Reshape> {
    let ready = ready_outputs(utxos);
    if ready >= floor {
        return None;
    }
    let wanted = floor - ready;

    let spendable = |outputs: &[TransactionOutput]| -> u64 {
        outputs.iter().map(|output| output.value).sum::<u64>().saturating_sub(fee)
    };
    let cut = |inputs: Vec<TransactionOutput>, pieces: usize| -> Reshape {
        let total = spendable(&inputs);
        let share = total / pieces as u64;
        let mut shares = vec![share; pieces];
        // The last piece absorbs the division's remainder rather than
        // letting it become a change output. At most `pieces - 1` units,
        // so it cannot unbalance the fan.
        shares[pieces - 1] = total - share * (pieces as u64 - 1);
        Reshape { inputs, shares }
    };

    // Everything unmarked and too small to be a slot. Consumed by
    // whichever move runs, so neither leaves it behind to accumulate.
    let dust: Vec<TransactionOutput> = utxos
        .iter()
        .filter(|(marked, output)| !*marked && output.value < MIN_USEFUL_OUTPUT)
        .map(|(_, output)| output.clone())
        .collect();

    // Consolidation. One piece is enough here, unlike a split: it turns
    // material that could fund nothing on its own into an output that
    // can, so the ready count goes up even at one.
    let from_dust = (spendable(&dust) / MIN_USEFUL_OUTPUT) as usize;
    if from_dust >= 1 {
        return Some(cut(dust, wanted.min(from_dust)));
    }

    // Splitting. `+ 1` because the source is itself one of the ready
    // outputs being consumed: cutting it into as many pieces as are
    // missing would land one short.
    let largest = utxos
        .iter()
        .filter(|(marked, output)| !*marked && output.value >= MIN_USEFUL_OUTPUT)
        .max_by_key(|(_, output)| (output.value, output.unique_id))
        .map(|(_, output)| output.clone())?;
    let mut inputs = vec![largest];
    inputs.extend(dust);
    let pieces = (wanted + 1).min((spendable(&inputs) / MIN_USEFUL_OUTPUT) as usize);
    if pieces < 2 {
        // One piece is not a split. It would pay a fee to leave the
        // wallet no better and hidden for a block, which is how a wallet
        // too small to reach the floor would otherwise bleed a fee every
        // sweep forever.
        return None;
    }
    Some(cut(inputs, pieces))
}

#[cfg(test)]
mod tests {
    use super::*;
    use btclib::crypto::PrivateKey;
    use btclib::payment::build_multi_payment;
    use uuid::Uuid;

    const FEE: u64 = 1_000;

    fn output(value: u64) -> TransactionOutput {
        TransactionOutput {
            value,
            unique_id: Uuid::new_v4(),
            pubkey: PrivateKey::new_key().public_key(),
        }
    }

    fn wallet(values: &[u64]) -> Vec<(bool, TransactionOutput)> {
        values.iter().map(|v| (false, output(*v))).collect()
    }

    /// The behaviour the whole ceiling turns on: a small payment must
    /// not consume the wallet's biggest output.
    #[test]
    fn a_small_payment_spends_the_smallest_output_that_covers_it() {
        let utxos = wallet(&[15_000_000_000, 200_000_000, 900_000_000]);
        let ordered = ordered_for_payment(&utxos, 50_001_000);
        assert_eq!(ordered[0].1.value, 200_000_000);
    }

    #[test]
    fn a_payment_no_single_output_covers_takes_the_largest_first() {
        let utxos = wallet(&[100, 900, 500]);
        let ordered = ordered_for_payment(&utxos, 1_400);
        assert_eq!(
            ordered.iter().map(|(_, o)| o.value).collect::<Vec<_>>(),
            vec![900, 500, 100],
            "fewest inputs, so the fan loses as few slots as possible"
        );
    }

    /// The ordering is only worth anything if `build_multi_payment`
    /// actually honours it, so assert against the real builder rather
    /// than against the order alone.
    #[test]
    fn the_ordering_is_what_build_multi_payment_selects() {
        let owner = PrivateKey::new_key();
        let recipient = PrivateKey::new_key().public_key();
        let big = output(15_000_000_000);
        let right_sized = output(200_000_000);
        let utxos = vec![(false, big.clone()), (false, right_sized.clone())];

        let ordered = ordered_for_payment(&utxos, 50_000_000 + FEE);
        let tx = build_multi_payment(
            &ordered,
            &owner,
            &[(recipient, 50_000_000)],
            FEE,
            owner.public_key(),
        )
        .unwrap();

        assert_eq!(tx.inputs.len(), 1);
        assert_eq!(
            tx.inputs[0].prev_transaction_output_hash,
            right_sized.hash(),
            "the big output must still be whole and spendable afterwards"
        );
    }

    #[test]
    fn a_marked_output_is_never_the_best_fit() {
        let mut utxos = wallet(&[900_000_000]);
        utxos.push((true, output(200_000_000)));
        let ordered = ordered_for_payment(&utxos, 50_001_000);
        assert_eq!(
            ordered[0].1.value, 900_000_000,
            "the exactly-right output is already spoken for in the mempool"
        );
    }

    /// Applies a plan the way a mined transaction would, so a test can
    /// re-plan against the wallet it produced.
    fn apply(utxos: &mut Vec<(bool, TransactionOutput)>, plan: &Reshape) {
        let spent: Vec<Uuid> = plan.inputs.iter().map(|o| o.unique_id).collect();
        utxos.retain(|(_, o)| !spent.contains(&o.unique_id));
        utxos.extend(plan.shares.iter().map(|v| (false, output(*v))));
    }

    /// Runs the planner to a fixed point, asserting it reaches one.
    /// This is the property that matters most: it runs every sweep for
    /// the life of the process, and a plan that never says `None` is a
    /// fee paid every sixty seconds forever.
    fn settle(utxos: &mut Vec<(bool, TransactionOutput)>, floor: usize) -> usize {
        let mut passes = 0;
        while let Some(plan) = plan_reshape(utxos, FEE, floor) {
            passes += 1;
            assert!(passes < 10, "the wallet plan is not converging");
            apply(utxos, &plan);
        }
        passes
    }

    #[test]
    fn a_wallet_at_the_floor_is_left_alone() {
        let utxos = wallet(&vec![MIN_USEFUL_OUTPUT; 4]);
        assert!(plan_reshape(&utxos, FEE, 4).is_none());
    }

    #[test]
    fn one_big_output_reaches_the_floor_in_a_single_split() {
        let utxos = wallet(&[15_000_000_000]);
        let plan = plan_reshape(&utxos, FEE, 24).expect("a single blob is exactly what to split");
        assert_eq!(plan.shares.len(), 24, "one ready output plus 23 missing");
        assert_eq!(
            plan.shares.iter().sum::<u64>(),
            15_000_000_000 - FEE,
            "no change output, so nothing lands in a size the plan did not choose"
        );
        assert!(plan.shares.iter().all(|s| *s >= MIN_USEFUL_OUTPUT));
    }

    #[test]
    fn splitting_converges_and_then_stops() {
        let mut utxos = wallet(&[15_000_000_000]);
        assert_eq!(settle(&mut utxos, 24), 1);
        assert_eq!(ready_outputs(&utxos), 24);
    }

    /// A wallet too poor to reach the floor must stop trying, not pay a
    /// fee every sixty seconds to rearrange the same coins.
    #[test]
    fn a_wallet_too_small_for_the_floor_stops_short_of_it() {
        let mut utxos = wallet(&[250_000_000]);
        settle(&mut utxos, 24);
        assert_eq!(ready_outputs(&utxos), 2, "250_000_000 buys two usable slots");
    }

    /// The trap a splitter-only planner falls into, and the reason
    /// consolidation exists. Every payment leaves change, so a busy
    /// wallet erodes: sooner or later every output is below the line,
    /// and at that point there is nothing large enough to split. A
    /// planner that could only split would return `None` here forever,
    /// against a wallet holding six whole coins.
    #[test]
    fn a_wallet_eroded_entirely_into_dust_is_recovered() {
        let mut utxos = wallet(&vec![MIN_USEFUL_OUTPUT / 4; 24]);
        assert_eq!(ready_outputs(&utxos), 0, "every output is below the line");

        settle(&mut utxos, 24);
        assert_eq!(
            ready_outputs(&utxos),
            5,
            "six coins of dust, less the fee, is five whole slots -- and it must find all five"
        );
    }

    /// Consolidation runs before splitting, because it is strictly the
    /// better move: it costs one fee and takes nothing out of service,
    /// where a split hides a working output for a block.
    #[test]
    fn dust_is_swept_up_before_a_working_output_is_broken() {
        let mut utxos = wallet(&[15_000_000_000]);
        utxos.extend(wallet(&vec![MIN_USEFUL_OUTPUT / 2; 4]));

        let plan = plan_reshape(&utxos, FEE, 24).expect("the wallet is under the floor");
        assert!(
            plan.inputs.iter().all(|o| o.value < MIN_USEFUL_OUTPUT),
            "the 150-coin output must still be whole"
        );
        assert_eq!(
            plan.shares.len(),
            1,
            "two coins of dust, less the fee, will not divide into two whole slots"
        );

        // ...and the split still happens on the pass after.
        apply(&mut utxos, &plan);
        let plan = plan_reshape(&utxos, FEE, 24).expect("still under the floor");
        assert!(plan.inputs.iter().any(|o| o.value == 15_000_000_000));
    }

    #[test]
    fn nothing_spendable_means_no_plan() {
        let utxos = vec![(true, output(15_000_000_000))];
        assert!(plan_reshape(&utxos, FEE, 24).is_none());
        assert!(plan_reshape(&[], FEE, 24).is_none());
    }

    /// Dust too small to make even one slot is not worth a fee.
    #[test]
    fn dust_that_cannot_make_one_usable_output_is_left_where_it_is() {
        let utxos = wallet(&vec![MIN_USEFUL_OUTPUT / 4; 2]);
        assert!(plan_reshape(&utxos, FEE, 24).is_none());
    }

    /// Change too small to be a slot is what erodes the fan, so it has
    /// to be counted honestly: a wallet of dust is a wallet at zero.
    #[test]
    fn change_below_the_useful_size_does_not_count_as_ready() {
        let utxos = wallet(&[MIN_USEFUL_OUTPUT - 1, MIN_USEFUL_OUTPUT]);
        assert_eq!(ready_outputs(&utxos), 1);
    }
}

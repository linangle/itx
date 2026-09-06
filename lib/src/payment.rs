use tracing::*;

use crate::crypto::{PrivateKey, PublicKey, Signature};
use crate::types::{Transaction, TransactionInput, TransactionOutput};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum PaymentError {
    #[error("insufficient funds: have {available}, need {needed}")]
    InsufficientFunds { available: u64, needed: u64 },
}

/// Single-recipient special case of `build_multi_payment` -- see its doc
/// comment for the actual logic.
///
/// All of `available_utxos` must be spendable by the same
/// `signing_key` -- a wallet mixing UTXOs across multiple keys in one
/// transaction needs to do its own input selection across keys; this
/// covers the common single-key case (which is all a single-operator
/// service like a faucet ever needs).
pub fn build_payment(
    available_utxos: &[(bool, TransactionOutput)],
    signing_key: &PrivateKey,
    recipient: PublicKey,
    amount: u64,
    fee: u64,
    change_pubkey: PublicKey,
) -> Result<Transaction, PaymentError> {
    build_multi_payment(available_utxos, signing_key, &[(recipient, amount)], fee, change_pubkey)
}

/// Greedily selects from `available_utxos` (skipping any marked as
/// already spoken for) until the sum of every `recipients` amount plus
/// `fee` is covered, signs each selected input with `signing_key`, and
/// returns a transaction paying each recipient their specified amount
/// (in one output apiece) with any leftover sent back to `change_pubkey`.
///
/// This exists rather than always looping `build_payment` per recipient
/// because a source address funded by a single, exact deposit (see the
/// hub's escrow mechanism) holds *exactly* that deposit, unlike an
/// ongoing wallet's float: settling several recipients from the same
/// pot via independent single-payment transactions would send whatever
/// "change" remained after the first one back to the source, stranding
/// every recipient after it. One combined transaction has no such
/// ordering hazard, and costs one fee total instead of one per recipient.
pub fn build_multi_payment(
    available_utxos: &[(bool, TransactionOutput)],
    signing_key: &PrivateKey,
    recipients: &[(PublicKey, u64)],
    fee: u64,
    change_pubkey: PublicKey,
) -> Result<Transaction, PaymentError> {
    let recipients_total: u64 = recipients.iter().map(|(_, amount)| amount).sum();
    let total_needed = recipients_total + fee;
    let mut inputs = Vec::new();
    let mut input_sum = 0u64;

    for (marked, utxo) in available_utxos {
        if input_sum >= total_needed {
            break;
        }
        if *marked {
            continue;
        }
        let hash = utxo.hash();
        inputs.push(TransactionInput {
            prev_transaction_output_hash: hash,
            signature: Signature::sign_output(&hash, signing_key),
        });
        input_sum += utxo.value;
    }

    if input_sum < total_needed {
        return Err(PaymentError::InsufficientFunds {
            available: input_sum,
            needed: total_needed,
        });
    }

    let mut outputs: Vec<TransactionOutput> = recipients
        .iter()
        .map(|(recipient, amount)| TransactionOutput {
            value: *amount,
            unique_id: Uuid::new_v4(),
            pubkey: recipient.clone(),
        })
        .collect();
    if input_sum > total_needed {
        outputs.push(TransactionOutput {
            value: input_sum - total_needed,
            unique_id: Uuid::new_v4(),
            pubkey: change_pubkey,
        });
    }

    Ok(Transaction::new(inputs, outputs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid as UuidLib;

    fn utxo(value: u64, pubkey: PublicKey) -> TransactionOutput {
        TransactionOutput {
            value,
            unique_id: UuidLib::new_v4(),
            pubkey,
        }
    }

    #[test]
    fn build_payment_covers_amount_and_returns_change() {
        let owner = PrivateKey::new_key();
        let recipient = PrivateKey::new_key().public_key();

        let available = vec![
            (false, utxo(100, owner.public_key())),
            (false, utxo(50, owner.public_key())),
        ];

        let tx = build_payment(
            &available,
            &owner,
            recipient.clone(),
            120,
            5,
            owner.public_key(),
        )
        .unwrap();

        assert_eq!(tx.inputs.len(), 2); // needed both to cover 125
        let recipient_output = tx.outputs.iter().find(|o| o.pubkey == recipient).unwrap();
        assert_eq!(recipient_output.value, 120);
        let change_output = tx.outputs.iter().find(|o| o.pubkey == owner.public_key());
        assert_eq!(change_output.unwrap().value, 150 - 125);
    }

    #[test]
    fn build_payment_skips_marked_utxos() {
        let owner = PrivateKey::new_key();
        let recipient = PrivateKey::new_key().public_key();

        let available = vec![
            (true, utxo(1_000, owner.public_key())), // already spoken for -- must be ignored
            (false, utxo(10, owner.public_key())),
        ];

        // exactly enough if (and only if) the marked UTXO is correctly
        // excluded from consideration
        let tx = build_payment(&available, &owner, recipient.clone(), 10, 0, owner.public_key())
            .unwrap();
        assert_eq!(tx.inputs.len(), 1);
        let total_out: u64 = tx.outputs.iter().map(|o| o.value).sum();
        assert_eq!(total_out, 10);

        // requesting anything beyond the unmarked 10 must fail, proving
        // the 1000-value marked UTXO was never available to cover it
        let result = build_payment(&available, &owner, recipient, 11, 0, owner.public_key());
        assert!(matches!(
            result,
            Err(PaymentError::InsufficientFunds { available: 10, needed: 11 })
        ));
    }

    #[test]
    fn build_payment_rejects_insufficient_funds() {
        let owner = PrivateKey::new_key();
        let recipient = PrivateKey::new_key().public_key();
        let available = vec![(false, utxo(5, owner.public_key()))];

        let result = build_payment(&available, &owner, recipient, 100, 0, owner.public_key());
        assert!(matches!(
            result,
            Err(PaymentError::InsufficientFunds { available: 5, needed: 100 })
        ));
    }

    #[test]
    fn build_multi_payment_pays_every_recipient_in_one_transaction() {
        let owner = PrivateKey::new_key();
        let recipient1 = PrivateKey::new_key().public_key();
        let recipient2 = PrivateKey::new_key().public_key();
        let available = vec![(false, utxo(1_000, owner.public_key()))];

        let tx = build_multi_payment(
            &available,
            &owner,
            &[(recipient1.clone(), 300), (recipient2.clone(), 300)],
            10,
            owner.public_key(),
        )
        .unwrap();

        assert_eq!(tx.inputs.len(), 1, "one combined transaction, not one per recipient");
        assert_eq!(tx.outputs.iter().find(|o| o.pubkey == recipient1).unwrap().value, 300);
        assert_eq!(tx.outputs.iter().find(|o| o.pubkey == recipient2).unwrap().value, 300);
        let change = tx.outputs.iter().find(|o| o.pubkey == owner.public_key()).unwrap();
        assert_eq!(change.value, 1_000 - 300 - 300 - 10);
    }

    #[test]
    fn build_multi_payment_rejects_insufficient_funds_across_all_recipients() {
        let owner = PrivateKey::new_key();
        let r1 = PrivateKey::new_key().public_key();
        let r2 = PrivateKey::new_key().public_key();
        let available = vec![(false, utxo(50, owner.public_key()))];

        let result = build_multi_payment(&available, &owner, &[(r1, 30), (r2, 30)], 0, owner.public_key());
        assert!(matches!(
            result,
            Err(PaymentError::InsufficientFunds { available: 50, needed: 60 })
        ));
    }

    #[test]
    fn build_payment_is_the_single_recipient_case_of_build_multi_payment() {
        let owner = PrivateKey::new_key();
        let recipient = PrivateKey::new_key().public_key();
        let available = vec![(false, utxo(100, owner.public_key()))];

        let single = build_payment(&available, &owner, recipient.clone(), 40, 5, owner.public_key()).unwrap();
        let multi = build_multi_payment(&available, &owner, &[(recipient, 40)], 5, owner.public_key()).unwrap();

        assert_eq!(single.outputs.len(), multi.outputs.len());
        let total_single: u64 = single.outputs.iter().map(|o| o.value).sum();
        let total_multi: u64 = multi.outputs.iter().map(|o| o.value).sum();
        assert_eq!(total_single, total_multi);
    }

    /// The property the hub's settlement confirmation rests on: two
    /// builds of the *same* payment -- same recipient, same amount, same
    /// inputs -- produce outputs with different hashes, because every
    /// `TransactionOutput` carries a fresh `unique_id`.
    ///
    /// Without it an output hash would identify "a payment of this size
    /// to this key" rather than one specific attempt, and the hub could
    /// not tell a payout that confirmed from an earlier, identical one
    /// that did -- which is exactly the distinction
    /// `hub::board::PayoutAttempt::resolve` makes to decide whether
    /// rebuilding a lost payout is safe.
    #[test]
    fn two_builds_of_the_same_payment_have_different_output_hashes() {
        let owner = PrivateKey::new_key();
        let recipient = PrivateKey::new_key().public_key();
        let available = vec![(false, utxo(100, owner.public_key()))];

        let first =
            build_payment(&available, &owner, recipient.clone(), 40, 5, owner.public_key()).unwrap();
        let second =
            build_payment(&available, &owner, recipient.clone(), 40, 5, owner.public_key()).unwrap();

        let to_recipient = |tx: &Transaction| {
            tx.outputs
                .iter()
                .find(|o| o.pubkey == recipient && o.value == 40)
                .expect("the payment output is always present")
                .hash()
        };
        assert_ne!(
            to_recipient(&first),
            to_recipient(&second),
            "identical payments must still be distinguishable attempts"
        );
    }
}

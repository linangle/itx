use tracing::*;

use super::{Transaction, TransactionOutput};
use crate::error::{BtcError, Result};
use crate::sha256::Hash;
use crate::util::{MerkleRoot, Saveable};
use crate::U256;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{
    Error as IoError, ErrorKind as IoErrorKind, Read, Result as IoResult, Write,
};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BlockHeader {
    pub timestamp: DateTime<Utc>,
    pub nonce: u64,
    pub prev_block_hash: Hash,
    pub merkle_root: MerkleRoot,
    pub target: U256,
}

impl BlockHeader {
    pub fn new(
        timestamp: DateTime<Utc>,
        nonce: u64,
        prev_block_hash: Hash,
        merkle_root: MerkleRoot,
        target: U256,
    ) -> Self {
        BlockHeader {
            timestamp,
            nonce,
            prev_block_hash,
            merkle_root,
            target,
        }
    }

    pub fn hash(&self) -> Hash {
        Hash::hash(self)
    }

    /// The amount of expected PoW work represented by this header's target.
    pub fn work(&self) -> U256 {
        crate::work_from_target(self.target)
    }

    pub fn mine(&mut self, steps: usize) -> bool {
        if self.hash().matches_target(self.target) {
            return true;
        }

        for _ in 0..steps {
            if let Some(new_nonce) = self.nonce.checked_add(1) {
                self.nonce = new_nonce;
            } else {
                self.nonce = 0;
                self.timestamp = Utc::now();
            }
            if self.hash().matches_target(self.target) {
                return true;
            }
        }
        false
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>,
}

impl Block {
    pub fn new(header: BlockHeader, transactions: Vec<Transaction>) -> Self {
        Block {
            header,
            transactions,
        }
    }

    pub fn hash(&self) -> Hash {
        Hash::hash(self)
    }

    pub fn serialized_size(&self) -> usize {
        let mut buf = Vec::new();
        ciborium::into_writer(self, &mut buf).expect("BUG: failed to serialize Block");
        buf.len()
    }

    pub fn verify_transactions(
        &self,
        predicted_block_height: u64,
        utxos: &HashMap<Hash, (bool, TransactionOutput)>,
    ) -> Result<()> {
        let mut inputs: HashMap<Hash, TransactionOutput> = HashMap::new();

        if self.transactions.is_empty() {
            return Err(BtcError::InvalidTransaction);
        }

        self.verify_coinbase_transaction(predicted_block_height, utxos)?;

        for transaction in self.transactions.iter().skip(1) {
            let mut input_value: u64 = 0;
            let mut output_value: u64 = 0;

            for input in &transaction.inputs {
                let prev_output = utxos
                    .get(&input.prev_transaction_output_hash)
                    .map(|(_, output)| output);

                if prev_output.is_none() {
                    return Err(BtcError::InvalidTransaction);
                }

                let prev_output = prev_output.unwrap();

                if inputs.contains_key(&input.prev_transaction_output_hash) {
                    return Err(BtcError::InvalidTransaction);
                }

                // Against this transaction's own outputs, not the spent
                // output alone: the signature is consent to *this*
                // payment. See `Transaction::spend_commitment`.
                if !input.verifies(&transaction.outputs, &prev_output.pubkey) {
                    return Err(BtcError::InvalidSignature);
                }

                input_value = input_value
                    .checked_add(prev_output.value)
                    .ok_or(BtcError::InvalidTransaction)?;
                inputs.insert(
                    input.prev_transaction_output_hash,
                    prev_output.clone(),
                );
            }

            for output in &transaction.outputs {
                // Checked, and the check is load-bearing: two outputs
                // whose values wrap past `u64::MAX` sum to less than
                // their input, and `input < output` is then false. The
                // dev profile panics on the `+`, which is why no test
                // ever reached the comparison; the release profile
                // wrapped and conserved value on a transaction that
                // created 2^64 units from one.
                output_value = output_value
                    .checked_add(output.value)
                    .ok_or(BtcError::InvalidTransaction)?;
            }

            if input_value < output_value {
                return Err(BtcError::InvalidTransaction);
            }
        }

        Ok(())
    }

    pub fn verify_coinbase_transaction(
        &self,
        predicted_block_height: u64,
        utxos: &HashMap<Hash, (bool, TransactionOutput)>,
    ) -> Result<()> {
        let coinbase_transaction = &self.transactions[0];

        if !coinbase_transaction.inputs.is_empty() {
            return Err(BtcError::InvalidTransaction);
        }

        let miner_fees = self.calculate_miner_fees(utxos)?;
        let block_reward = crate::block_reward_at_height(predicted_block_height);
        let total_coinbase_outputs = coinbase_transaction
            .outputs
            .iter()
            .try_fold(0u64, |acc, output| acc.checked_add(output.value))
            .ok_or(BtcError::InvalidTransaction)?;
        let owed = block_reward
            .checked_add(miner_fees)
            .ok_or(BtcError::InvalidTransaction)?;

        if total_coinbase_outputs != owed {
            return Err(BtcError::InvalidTransaction);
        }

        Ok(())
    }

    pub fn calculate_miner_fees(
        &self,
        utxos: &HashMap<Hash, (bool, TransactionOutput)>,
    ) -> Result<u64> {
        calculate_miner_fees_for_transactions(&self.transactions[1..], utxos)
    }
}

pub fn calculate_miner_fees_for_transactions(
    transactions: &[Transaction],
    utxos: &HashMap<Hash, (bool, TransactionOutput)>,
) -> Result<u64> {
    let mut total_fees = 0u64;

    for transaction in transactions {
        let mut input_value: u64 = 0;
        let mut output_value: u64 = 0;

        for input in &transaction.inputs {
            let prev_output = utxos
                .get(&input.prev_transaction_output_hash)
                .map(|(_, output)| output);

            if prev_output.is_none() {
                return Err(BtcError::InvalidTransaction);
            }

            input_value = input_value
                .checked_add(prev_output.unwrap().value)
                .ok_or(BtcError::InvalidTransaction)?;
        }

        for output in &transaction.outputs {
            output_value = output_value
                .checked_add(output.value)
                .ok_or(BtcError::InvalidTransaction)?;
        }

        if input_value < output_value {
            return Err(BtcError::InvalidTransaction);
        }

        total_fees = total_fees
            .checked_add(input_value - output_value)
            .ok_or(BtcError::InvalidTransaction)?;
    }

    Ok(total_fees)
}

impl Saveable for Block {
    fn load<I: Read>(reader: I) -> IoResult<Self> {
        ciborium::de::from_reader(reader).map_err(|_| {
            IoError::new(
                IoErrorKind::InvalidData,
                "Failed to deserialize Block",
            )
        })
    }

    fn save<O: Write>(&self, writer: O) -> IoResult<()> {
        ciborium::ser::into_writer(self, writer).map_err(|_| {
            IoError::new(
                IoErrorKind::InvalidData,
                "Failed to serialize Block",
            )
        })
    }
}


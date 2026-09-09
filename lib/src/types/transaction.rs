use tracing::*;

use crate::crypto::{PublicKey, Signature};
use crate::sha256::Hash;
use crate::util::Saveable;
use serde::{Deserialize, Serialize};
use std::io::{
    Error as IoError, ErrorKind as IoErrorKind, Read, Result as IoResult, Write,
};
use uuid::Uuid;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TransactionInput {
    pub prev_transaction_output_hash: Hash,
    pub signature: Signature,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TransactionOutput {
    pub value: u64,
    pub unique_id: Uuid,
    pub pubkey: PublicKey,
}

impl TransactionOutput {
    pub fn hash(&self) -> Hash {
        Hash::hash(self)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Transaction {
    pub inputs: Vec<TransactionInput>,
    pub outputs: Vec<TransactionOutput>,
}

impl Transaction {
    pub fn new(inputs: Vec<TransactionInput>, outputs: Vec<TransactionOutput>) -> Self {
        Transaction { inputs, outputs }
    }

    pub fn hash(&self) -> Hash {
        Hash::hash(self)
    }

    /// Size in bytes of this transaction once CBOR-serialized, i.e. how
    /// much block space it would consume. Used to fit transactions into a
    /// byte budget rather than an arbitrary transaction count.
    pub fn serialized_size(&self) -> usize {
        let mut buf = Vec::new();
        ciborium::into_writer(self, &mut buf).expect("BUG: failed to serialize Transaction");
        buf.len()
    }
}

impl Saveable for Transaction {
    fn load<I: Read>(reader: I) -> IoResult<Self> {
        ciborium::de::from_reader(reader).map_err(|_| {
            IoError::new(
                IoErrorKind::InvalidData,
                "Failed to deserialize Transaction",
            )
        })
    }

    fn save<O: Write>(&self, writer: O) -> IoResult<()> {
        ciborium::ser::into_writer(self, writer).map_err(|_| {
            IoError::new(
                IoErrorKind::InvalidData,
                "Failed to serialize Transaction",
            )
        })
    }
}


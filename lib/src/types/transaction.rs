use tracing::*;

use crate::crypto::{PrivateKey, PublicKey, Signature};
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
    /// Over `Transaction::spend_commitment` -- the output being spent
    /// **and** everything this transaction pays. Never over the spent
    /// output's hash alone: that is a signature on "I own this coin"
    /// with no statement about where it goes, and anything between the
    /// signer and the block can rewrite the outputs under it.
    pub signature: Signature,
}

impl TransactionInput {
    /// Spends `prev_output_hash` in a transaction paying `outputs`.
    ///
    /// The only way this crate builds an input, so that no caller has to
    /// remember what a spend signs. `outputs` is the transaction's whole
    /// output list, which means it has to exist before its inputs can be
    /// signed -- see `build_multi_payment`, which was written the other
    /// way round and had to be turned around.
    pub fn signed(
        prev_output_hash: Hash,
        outputs: &[TransactionOutput],
        signing_key: &PrivateKey,
    ) -> Self {
        let commitment = Transaction::spend_commitment(&prev_output_hash, outputs);
        TransactionInput {
            prev_transaction_output_hash: prev_output_hash,
            signature: Signature::sign_output(&commitment, signing_key),
        }
    }

    /// Whether this input's signature is `pubkey`'s consent to spend it
    /// into exactly `outputs`. The chain's whole ownership check -- see
    /// `Block::verify_transactions` and `Blockchain::add_to_mempool`,
    /// which are the two places it is made.
    pub fn verifies(&self, outputs: &[TransactionOutput], pubkey: &PublicKey) -> bool {
        let commitment =
            Transaction::spend_commitment(&self.prev_transaction_output_hash, outputs);
        self.signature.verify(&commitment, pubkey)
    }
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

    /// What one input's signature commits to: the output it spends, and
    /// every output this transaction pays.
    ///
    /// **Why this exists.** An input used to sign the spent output's hash
    /// on its own. That proves ownership of a coin and says nothing about
    /// where the coin goes, so every party between the signer and the
    /// block -- the hub assembling a `POST /wallet/send`, a relaying
    /// node, the miner choosing what to include -- could replace the
    /// outputs with their own key and the signature still verified. The
    /// chain accepted it as a valid spend. Binding the outputs is what
    /// turns "I own this" into "I am paying these people".
    ///
    /// **The preimage is written out by hand rather than CBOR-hashed**
    /// like the rest of btclib's hashes, because it has to be
    /// reproducible by clients that are not Rust and have no ciborium:
    /// the Python SDK signs its own spends, and matching ciborium's
    /// encoding of a struct there would be guesswork pinned by nothing.
    /// The layout is length-prefixed at every variable part, so no two
    /// distinct output lists can produce the same bytes:
    ///
    /// ```text
    /// "itx.spend.v1"              domain tag, 12 bytes
    /// prev_output_hash            32 bytes, as_bytes()
    /// outputs.len()               u32, big-endian
    ///   value                     u64, big-endian     } per output,
    ///   pubkey.len()              u8                  } in order
    ///   pubkey                    SEC1 bytes          }
    /// ```
    ///
    /// **`unique_id` is deliberately not in it.** It exists to keep two
    /// otherwise identical outputs distinct, and a party who changed one
    /// would change the output's hash without changing who is paid what
    /// -- while a client that had to know the ids in advance could no
    /// longer let the hub mint them, which is the shape
    /// `POST /wallet/send` is built around. Value and recipient are what
    /// a signature has to cover.
    pub fn spend_commitment(prev_output_hash: &Hash, outputs: &[TransactionOutput]) -> Hash {
        let mut preimage = Vec::with_capacity(12 + 32 + 4 + outputs.len() * 42);
        preimage.extend_from_slice(b"itx.spend.v1");
        preimage.extend_from_slice(&prev_output_hash.as_bytes());
        // A u32 rather than a usize: the byte width of a usize is the
        // machine's, and this has to hash the same on every machine.
        preimage.extend_from_slice(&(outputs.len() as u32).to_be_bytes());
        for output in outputs {
            preimage.extend_from_slice(&output.value.to_be_bytes());
            let pubkey = output.pubkey.to_sec1_bytes();
            // A SEC1 key is 33 bytes compressed and 65 uncompressed, so
            // the length is not a constant and the tail of one key must
            // not be able to read as the head of the next.
            preimage.push(pubkey.len() as u8);
            preimage.extend_from_slice(&pubkey);
        }
        Hash::hash_bytes(&preimage)
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


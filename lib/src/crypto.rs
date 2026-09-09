use tracing::*;

use crate::sha256::Hash;
use ecdsa::signature::{Signer, Verifier};
use ecdsa::{Signature as ECDSASignature, SigningKey, VerifyingKey};
use k256::Secp256k1;
use serde::{Deserialize, Serialize};
use std::fmt;

use spki::EncodePublicKey;
use std::io::{
Error as IoError, ErrorKind as IoErrorKind, Read,
Result as IoResult, Write,
};
use crate::util::Saveable;
impl Saveable for PrivateKey {
    fn load<I: Read>(reader: I) -> IoResult<Self> {
        ciborium::de::from_reader(reader).map_err(
            |_| {
            IoError::new(
                IoErrorKind::InvalidData,
                "Failed to deserialize PrivateKey",
            )
        })
    }
    fn save<O: Write>(&self, writer: O) -> IoResult<()> {
        ciborium::ser::into_writer(self, writer).map_err(
           |_| {
                IoError::new(
                    IoErrorKind::InvalidData,
                    "Failed to serialize PrivateKey",
               )
           },
       )?;
        Ok(())
    }
}
// save and load as PEM
impl Saveable for PublicKey {
    fn load<I: Read>(mut reader: I) -> IoResult<Self> {
       // read PEM-encoded public key into string
        let mut buf = String::new();
        reader.read_to_string(&mut buf)?;
        // decode the public key from PEM
        let public_key = buf.parse().map_err(|_| {
            IoError::new(
                IoErrorKind::InvalidData,
                "Failed to parse PublicKey",
            )
        })?;
        Ok(PublicKey(public_key))
    }
    fn save<O: Write>(&self,mut writer: O,) -> IoResult<()> {
        let s = self.0.to_public_key_pem(Default::default()).map_err(|_| {
            IoError::new(
                IoErrorKind::InvalidData,
                "Failed to serialize PublicKey",
            )
        })?;
        writer.write_all(s.as_bytes())?;
        Ok(())
    }
}
   



#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct Signature(ECDSASignature<Secp256k1>);

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct PublicKey(VerifyingKey<Secp256k1>);

impl PartialOrd for PublicKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PublicKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.to_sec1_bytes().cmp(&other.0.to_sec1_bytes())
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // print the compressed SEC1 encoding of the key as hex
        write!(f, "{}", hex::encode(self.0.to_sec1_bytes()))
    }
}

impl PublicKey {
    /// Parses a key from its compressed SEC1 byte encoding (the same
    /// format `Display`/`to_sec1_bytes` produce). Used at HTTP API
    /// boundaries, where a key travels as a plain hex string rather than
    /// btclib's internal CBOR representation.
    pub fn from_sec1_bytes(bytes: &[u8]) -> Result<Self, ecdsa::Error> {
        Ok(PublicKey(VerifyingKey::from_sec1_bytes(bytes)?))
    }

    pub fn to_sec1_bytes(&self) -> Vec<u8> {
        self.0.to_sec1_bytes().to_vec()
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PrivateKey(
    #[serde(with = "signkey_serde")]
    SigningKey<Secp256k1>,
);




impl PrivateKey {
    pub fn new_key() -> Self {
        PrivateKey(SigningKey::random(&mut rand::thread_rng()))
    }

    /// Deterministically reconstructs a key from a fixed 32-byte scalar,
    /// rather than generating a random one. Used for the protocol's fixed
    /// genesis key, where every node must derive the exact same keypair.
    pub fn from_fixed_bytes(bytes: &[u8]) -> Result<Self, ecdsa::Error> {
        Ok(PrivateKey(SigningKey::from_slice(bytes)?))
    }

    pub fn public_key(&self) -> PublicKey {
        PublicKey(self.0.verifying_key().clone())
    }
}

impl Signature {
    pub fn sign_output(output_hash: &Hash, private_key: &PrivateKey) -> Self {
        let signature = private_key.0.sign(&output_hash.as_bytes());
        Signature(signature)
    }

    pub fn verify(&self, output_hash: &Hash, public_key: &PublicKey) -> bool {
        public_key
            .0
            .verify(&output_hash.as_bytes(), &self.0)
            .is_ok()
    }

    /// Signs an arbitrary already-hashed message (as opposed to
    /// `sign_output`, which specifically signs a UTXO's hash). Used
    /// outside the blockchain proper -- e.g. an HTTP API authenticating a
    /// request by having the caller sign a hash of it.
    pub fn sign_hash(hash: &Hash, private_key: &PrivateKey) -> Self {
        Self::sign_output(hash, private_key)
    }

    /// Fixed-size (r || s) byte encoding, for HTTP API boundaries where a
    /// signature travels as a plain hex string.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.0.to_bytes().to_vec()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ecdsa::Error> {
        Ok(Signature(ECDSASignature::from_slice(bytes)?))
    }
}

mod signkey_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(
        key: &super::SigningKey<super::Secp256k1>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(&key.to_bytes())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<super::SigningKey<super::Secp256k1>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes = Vec::<u8>::deserialize(deserializer)?;
        super::SigningKey::from_slice(&bytes).map_err(serde::de::Error::custom)
    }
}


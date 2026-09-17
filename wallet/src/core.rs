use anyhow::Result;
use btclib::crypto::{PrivateKey, PublicKey, Signature};
use btclib::network::Message;
use btclib::sha256::Hash;
use btclib::types::{Transaction, TransactionInput, TransactionOutput};
use btclib::util::Saveable;
use crossbeam_skiplist::SkipMap;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tracing::{debug, error, info};

/// Represent a key pair with paths to public and private keys.
#[derive(Serialize, Deserialize, Clone)]
pub struct Key {
    pub public: PathBuf,
    pub private: PathBuf,
}

/// Represent a loaded key pair with actual public and private keys.
#[derive(Clone)]
struct LoadedKey {
    public: PublicKey,
    private: PrivateKey,
}

/// Represent a recipient with a name and a path to their public key.
#[derive(Serialize, Deserialize, Clone)]
pub struct Recipient {
    pub name: String,
    pub key: PathBuf,
}

/// Represent a loaded recipient with their actual public key.
#[derive(Clone)]
pub struct LoadedRecipient {
    pub key: PublicKey,
}

impl Recipient {
    /// Load the recipient's public key from file.
    pub fn load(&self) -> Result<LoadedRecipient> {
        debug!("Loading recipient key from: {:?}", self.key);
        let key = PublicKey::load_from_file(&self.key)?;
        Ok(LoadedRecipient { key })
    }
}

/// Define the type of fee calculation.
#[derive(Serialize, Deserialize, Clone)]
pub enum FeeType {
    Fixed,
    Percent,
    /// `value` is satoshis per byte of the transaction's estimated
    /// serialized size -- unlike `Fixed`/`Percent`, which only ever look
    /// at the payment amount, this is the realistic model: real fees are
    /// paid for the block space a transaction actually consumes, not as
    /// a function of how much money it moves.
    PerByte,
}

/// Bytes contributed by exactly one `TransactionInput`/`TransactionOutput`
/// once CBOR-serialized, plus the fixed overhead of an otherwise-empty
/// transaction. Computed once and cached rather than per call: both
/// structs are fixed-width fields all the way down (notably
/// `Signature`'s compact/IEEE-P1363 encoding, which -- unlike real
/// Bitcoin's DER-encoded signatures -- has no variable-length integers
/// whose size depends on the specific r/s values), so a single sample of
/// each is an exact byte count for every future input/output, not an
/// approximation that needs a safety margin.
struct TransactionSizeModel {
    base: usize,
    per_input: usize,
    per_output: usize,
}

fn transaction_size_model() -> &'static TransactionSizeModel {
    static MODEL: std::sync::OnceLock<TransactionSizeModel> = std::sync::OnceLock::new();
    MODEL.get_or_init(|| {
        let sample_hash = btclib::sha256::Hash::hash_bytes(b"itx wallet fee-estimation sample");
        let sample_key = PrivateKey::new_key();
        let sample_input = TransactionInput {
            prev_transaction_output_hash: sample_hash,
            signature: Signature::sign_output(&sample_hash, &sample_key),
        };
        let sample_output = TransactionOutput {
            value: 0,
            unique_id: uuid::Uuid::new_v4(),
            pubkey: sample_key.public_key(),
        };
        let base = Transaction::new(vec![], vec![]).serialized_size();
        let per_input = Transaction::new(vec![sample_input], vec![]).serialized_size() - base;
        let per_output = Transaction::new(vec![], vec![sample_output]).serialized_size() - base;
        TransactionSizeModel { base, per_input, per_output }
    })
}

/// Estimated serialized byte size of a transaction shaped like
/// `num_inputs` inputs and `num_outputs` outputs -- see
/// `TransactionSizeModel` for why this is exact rather than approximate.
fn estimated_transaction_size(num_inputs: usize, num_outputs: usize) -> usize {
    let model = transaction_size_model();
    model.base + num_inputs * model.per_input + num_outputs * model.per_output
}

/// Configure the fee calculation.
#[derive(Serialize, Deserialize, Clone)]
pub struct FeeConfig {
    pub fee_type: FeeType,
    pub value: f64,
}

impl FeeConfig {
    /// Calculate the fee for a transaction under this configuration.
    /// `amount` is only meaningful for `Fixed`/`Percent`;
    /// `num_inputs`/`num_outputs` only for `PerByte` -- each fee type
    /// reads only the arguments it needs. A plain function of the
    /// config's own data (no wallet state involved), so it's testable
    /// without a live `Core`/network connection.
    fn calculate_fee(&self, amount: u64, num_inputs: usize, num_outputs: usize) -> u64 {
        let fee = match self.fee_type {
            FeeType::Fixed => self.value as u64,
            FeeType::Percent => (amount as f64 * self.value / 100.0) as u64,
            FeeType::PerByte => {
                let size = estimated_transaction_size(num_inputs, num_outputs);
                (size as f64 * self.value) as u64
            }
        };
        debug!("Calculated fee: {} satoshis", fee);
        fee
    }
}

/// Store the configuration for the Core.
#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    pub my_keys: Vec<Key>,
    pub contacts: Vec<Recipient>,
    pub default_node: String,
    pub fee_config: FeeConfig,
}

/// Store and manage Unspent Transaction Outputs (UTXOs).
#[derive(Clone)]
struct UtxoStore {
    my_keys: Vec<LoadedKey>,
    utxos: Arc<SkipMap<PublicKey, Vec<(bool, TransactionOutput)>>>,
}

impl UtxoStore {
    /// Create a new UtxoStore.
    fn new() -> Self {
        UtxoStore {
            my_keys: Vec::new(),
            utxos: Arc::new(SkipMap::new()),
        }
    }

    /// Add a new key to the UtxoStore.
    fn add_key(&mut self, key: LoadedKey) {
        debug!("Adding key to UtxoStore: {:?}", key.public);
        self.my_keys.push(key);
    }
}

/// Represent the core functionality of the wallet.
pub struct Core {
    pub config: Config,
    utxos: UtxoStore,
    pub stream: Mutex<TcpStream>,
}

impl Core {
    /// Create a new Core instance.
    fn new(config: Config, utxos: UtxoStore, stream: TcpStream) -> Self {
        Core {
            config,
            utxos,
            stream: Mutex::new(stream),
        }
    }

    /// Load the Core from a configuration file.
    pub async fn load(config_path: PathBuf) -> Result<Self> {
        info!("Loading core from config: {:?}", config_path);
        let config: Config = toml::from_str(&fs::read_to_string(&config_path)?)?;
        let mut utxos = UtxoStore::new();
        let mut stream = TcpStream::connect(&config.default_node).await?;
        btclib::network::perform_handshake_initiator(&mut stream)
            .await
            .map_err(|e| anyhow::anyhow!("handshake with {} failed: {}", config.default_node, e))?;

        // Load keys from config
        for key in &config.my_keys {
            debug!("Loading key pair: {:?}", key.public);
            let public = PublicKey::load_from_file(&key.public)?;
            let private = PrivateKey::load_from_file(&key.private)?;
            utxos.add_key(LoadedKey { public, private });
        }

        Ok(Core::new(config, utxos, stream))
    }

    /// Fetch UTXOs from the node for all loaded keys.
    pub async fn fetch_utxos(&self) -> Result<()> {
        debug!(
            "Fetching UTXOs from node: {}",
            self.config.default_node
        );
        for key in &self.utxos.my_keys {
            let message = Message::FetchUTXOs(key.public.clone());
            // Held across both the send and the receive: releasing and
            // re-acquiring the lock in between would let another task
            // (e.g. a concurrent send_transaction) interleave a write on
            // the same stream while this exchange's reply is still in
            // flight, misattributing whichever bytes arrive next.
            let mut stream_lock = self.stream.lock().await;
            message.send_async(&mut *stream_lock).await?;
            let reply = Message::receive_async(&mut *stream_lock).await?;
            drop(stream_lock);
            if let Message::UTXOs(utxos) = reply {
                debug!(
                    "Received {} UTXOs for key: {:?}",
                    utxos.len(),
                    key.public
                );
                // Replace the entire UTXO set for this key
                self.utxos.utxos.insert(
                    key.public.clone(),
                    utxos
                        .into_iter()
                        .map(|(output, marked)| (marked, output))
                        .collect(),
                );
            } else {
                error!("Unexpected response from node");
                return Err(anyhow::anyhow!("Unexpected response from node"));
            }
        }
        info!("UTXOs fetched successfully");
        Ok(())
    }

    /// Send a transaction to the node.
    pub async fn send_transaction(&self, transaction: Transaction) -> Result<()> {
        debug!(
            "Sending transaction to node: {}",
            self.config.default_node
        );
        let message = Message::SubmitTransaction(transaction);
        message
            .send_async(&mut *self.stream.lock().await)
            .await?;
        info!("Transaction sent successfully");
        Ok(())
    }

    /// Prepare a transaction and hand it off to be sent without blocking
    /// the caller (a synchronous UI callback). Takes `Arc<Self>` rather
    /// than `&self` so it can move an owned handle into the spawned task
    /// -- replaces the previous design, where this queued onto a channel
    /// whose receiver lived outside `Core` (in `main`), which then called
    /// back into `Core::send_transaction` itself: a detour through a
    /// queue and a separate task for something `tokio::spawn` can do
    /// directly, since the UI callback already runs inside
    /// `spawn_blocking`, which is still within the runtime's context.
    pub fn send_transaction_async(self: Arc<Self>, recipient: &str, amount: u64) -> Result<()> {
        info!(
            "Preparing to send {} satoshis to {}",
            amount, recipient
        );
        let recipient_key = self
            .config
            .contacts
            .iter()
            .find(|r| r.name == recipient)
            .ok_or_else(|| anyhow::anyhow!("Recipient not found"))?
            .load()?
            .key;
        let transaction = self.create_transaction(&recipient_key, amount)?;
        debug!("Sending transaction asynchronously");
        tokio::spawn(async move {
            if let Err(e) = self.send_transaction(transaction).await {
                error!("Failed to send transaction: {}", e);
            }
        });
        Ok(())
    }

    /// Get the current balance of all UTXOs.
    pub fn get_balance(&self) -> u64 {
        let balance = self
            .utxos
            .utxos
            .iter()
            .map(|entry| {
                entry
                    .value()
                    .iter()
                    .map(|utxo| utxo.1.value)
                    .sum::<u64>()
            })
            .sum();
        debug!("Current balance: {} satoshis", balance);
        balance
    }

    /// Create a new transaction.
    pub fn create_transaction(
        &self,
        recipient: &PublicKey,
        amount: u64,
    ) -> Result<Transaction> {
        debug!(
            "Creating transaction for {} satoshis to {:?}",
            amount, recipient
        );
        // The outputs have to exist before any input can be signed --
        // an input signs the whole output list, `Transaction::spend_commitment`
        // -- so selection only records *which* outputs are being spent
        // and under which key, and the signing happens at the end.
        let mut selected: Vec<(Hash, PrivateKey)> = Vec::new();
        let mut input_sum = 0;

        'select: for entry in self.utxos.utxos.iter() {
            let pubkey = entry.key();
            let utxos = entry.value();
            for (marked, utxo) in utxos.iter() {
                if *marked {
                    continue;
                } // Skip marked UTXOs
                // Sized as if a change output will be needed -- the safe
                // assumption, since a 2-output estimate is never smaller
                // than what an eventual exact-change (1-output) tx
                // actually needs, so this can only ever over-collect,
                // never under-collect.
                let fee = self.config.fee_config.calculate_fee(amount, selected.len(), 2);
                if input_sum >= amount + fee {
                    break 'select;
                }
                selected.push((
                    utxo.hash(),
                    self.utxos
                        .my_keys
                        .iter()
                        .find(|k| k.public == *pubkey)
                        .unwrap()
                        .private
                        .clone(),
                ));
                input_sum += utxo.value;
            }
        }

        let fee = self.config.fee_config.calculate_fee(amount, selected.len(), 2);
        let total_amount = amount + fee;

        if input_sum < total_amount {
            error!(
                "Insufficient funds: have {} satoshis, need {} satoshis",
                input_sum, total_amount
            );
            return Err(anyhow::anyhow!("Insufficient funds"));
        }

        let mut outputs = vec![TransactionOutput {
            value: amount,
            unique_id: uuid::Uuid::new_v4(),
            pubkey: recipient.clone(),
        }];

        if input_sum > total_amount {
            outputs.push(TransactionOutput {
                value: input_sum - total_amount,
                unique_id: uuid::Uuid::new_v4(),
                pubkey: self.utxos.my_keys[0].public.clone(),
            });
        }

        let inputs: Vec<TransactionInput> = selected
            .iter()
            .map(|(hash, key)| TransactionInput::signed(*hash, &outputs, key))
            .collect();

        info!("Transaction created successfully");
        Ok(Transaction::new(inputs, outputs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio::sync::Mutex as AsyncMutex;
    use tokio::time::Duration;
    use uuid::Uuid;

    fn temp_key_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("itx_wallet_test_{name}_{}.pem", Uuid::new_v4()))
    }

    fn empty_config() -> Config {
        Config {
            my_keys: vec![],
            contacts: vec![],
            default_node: String::new(),
            fee_config: FeeConfig { fee_type: FeeType::Fixed, value: 0.0 },
        }
    }

    #[test]
    fn transaction_size_model_scales_linearly_with_inputs_and_outputs() {
        let base = estimated_transaction_size(0, 0);
        let one_input = estimated_transaction_size(1, 0);
        let one_output = estimated_transaction_size(0, 1);

        assert!(one_input > base, "an input must add a nonzero number of bytes");
        assert!(one_output > base, "an output must add a nonzero number of bytes");
        assert_eq!(
            estimated_transaction_size(2, 3),
            base + 2 * (one_input - base) + 3 * (one_output - base),
            "size must scale exactly linearly (fixed-width fields all the way down), not just approximately"
        );
    }

    #[test]
    fn per_byte_fee_scales_with_size_not_amount() {
        let fee_config = FeeConfig { fee_type: FeeType::PerByte, value: 2.0 };
        let fee_1_input = fee_config.calculate_fee(999_999_999, 1, 2);
        let fee_2_inputs = fee_config.calculate_fee(999_999_999, 2, 2);

        assert!(fee_2_inputs > fee_1_input, "more inputs must mean a bigger transaction and thus a bigger fee");
        assert_eq!(
            fee_1_input,
            (estimated_transaction_size(1, 2) as f64 * 2.0) as u64,
            "must match the size model times the configured rate, regardless of amount"
        );
    }

    #[test]
    fn fixed_and_percent_fees_ignore_input_and_output_counts() {
        let fixed = FeeConfig { fee_type: FeeType::Fixed, value: 500.0 };
        assert_eq!(fixed.calculate_fee(1_000, 1, 1), fixed.calculate_fee(1_000, 50, 50));
        assert_eq!(fixed.calculate_fee(1_000, 1, 1), 500);

        let percent = FeeConfig { fee_type: FeeType::Percent, value: 10.0 };
        assert_eq!(percent.calculate_fee(1_000, 1, 1), percent.calculate_fee(1_000, 50, 50));
        assert_eq!(percent.calculate_fee(1_000, 1, 1), 100);
    }

    /// A minimal fake node: completes the handshake, then answers
    /// `FetchUTXOs` (after an artificial delay, opening a window for a
    /// concurrent request to race in if the caller didn't hold its stream
    /// lock across the whole exchange) and records every
    /// `SubmitTransaction` it receives, replying to nothing for it --
    /// matching the real node's fire-and-forget behavior for that
    /// message.
    struct FakeNode {
        addr: String,
        submitted: Arc<AsyncMutex<Vec<Transaction>>>,
    }

    impl FakeNode {
        async fn spawn(utxo_reply_delay: Duration) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
            let submitted = Arc::new(AsyncMutex::new(Vec::new()));
            let submitted_for_task = submitted.clone();
            tokio::spawn(async move {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                if btclib::network::perform_handshake_acceptor(&mut socket)
                    .await
                    .is_err()
                {
                    return;
                }
                loop {
                    let message = match Message::receive_async(&mut socket).await {
                        Ok(m) => m,
                        Err(_) => return,
                    };
                    match message {
                        Message::FetchUTXOs(_) => {
                            tokio::time::sleep(utxo_reply_delay).await;
                            if Message::UTXOs(vec![]).send_async(&mut socket).await.is_err() {
                                return;
                            }
                        }
                        Message::SubmitTransaction(tx) => {
                            submitted_for_task.lock().await.push(tx);
                        }
                        _ => return,
                    }
                }
            });
            FakeNode { addr, submitted }
        }

        /// `send_async` doesn't wait for the peer to process what it
        /// sent, so a caller observing `send_transaction`/
        /// `send_transaction_async` return only knows the bytes reached
        /// the OS socket, not that this fake's own task has recorded them
        /// yet. Polls briefly instead of asserting immediately.
        async fn wait_for_submitted_count(&self, expected: usize) -> Vec<Transaction> {
            for _ in 0..100 {
                let submitted = self.submitted.lock().await.clone();
                if submitted.len() >= expected {
                    return submitted;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            self.submitted.lock().await.clone()
        }
    }

    /// Connects to `fake` and builds a bare `Core` around that connection,
    /// bypassing `Core::load`'s file-based config parsing but still
    /// performing the same handshake it would (the fake node, like a real
    /// one, expects one before anything else).
    async fn connect_core(fake: &FakeNode, config: Config, utxos: UtxoStore) -> Core {
        let mut stream = TcpStream::connect(&fake.addr).await.unwrap();
        btclib::network::perform_handshake_initiator(&mut stream)
            .await
            .unwrap();
        Core::new(config, utxos, stream)
    }

    #[tokio::test]
    async fn fetch_utxos_and_send_transaction_dont_corrupt_each_others_exchange() {
        let fake = FakeNode::spawn(Duration::from_millis(200)).await;
        let private_key = PrivateKey::new_key();
        let public_key = private_key.public_key();
        let mut utxos = UtxoStore::new();
        utxos.add_key(LoadedKey { public: public_key, private: private_key });
        let core = Arc::new(connect_core(&fake, empty_config(), utxos).await);

        let fetch = tokio::spawn({
            let core = core.clone();
            async move { core.fetch_utxos().await }
        });
        // Give fetch_utxos time to have already sent its FetchUTXOs
        // request and be waiting on the deliberately-delayed reply --
        // this is the window a lock released between send and receive
        // would have left open for send_transaction to race into.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let submit = tokio::spawn({
            let core = core.clone();
            async move { core.send_transaction(Transaction::new(vec![], vec![])).await }
        });

        let (fetch_result, submit_result) = tokio::join!(fetch, submit);
        assert!(
            fetch_result.unwrap().is_ok(),
            "fetch_utxos must see its own UTXOs reply, not a corrupted/misattributed one"
        );
        assert!(submit_result.unwrap().is_ok());

        let submitted = fake.wait_for_submitted_count(1).await;
        assert_eq!(submitted.len(), 1, "the fake node must have received one clean, parseable SubmitTransaction");
    }

    #[tokio::test]
    async fn send_transaction_async_delivers_via_a_spawned_task_with_no_channel_involved() {
        let fake = FakeNode::spawn(Duration::from_millis(0)).await;

        let recipient_public = PrivateKey::new_key().public_key();
        let recipient_key_path = temp_key_path("recipient");
        recipient_public.save_to_file(&recipient_key_path).unwrap();

        let sender_private = PrivateKey::new_key();
        let sender_public = sender_private.public_key();
        let mut utxos = UtxoStore::new();
        utxos.add_key(LoadedKey { public: sender_public.clone(), private: sender_private });
        // Pre-populate a spendable UTXO so create_transaction has funds to
        // work with -- this test isn't exercising fetch_utxos itself.
        utxos.utxos.insert(
            sender_public.clone(),
            vec![(
                false,
                TransactionOutput { value: 1_000_000, unique_id: Uuid::new_v4(), pubkey: sender_public },
            )],
        );

        let mut config = empty_config();
        config.contacts.push(Recipient { name: "alice".to_string(), key: recipient_key_path.clone() });

        let core = Arc::new(connect_core(&fake, config, utxos).await);
        core.clone().send_transaction_async("alice", 100).unwrap();

        let submitted = fake.wait_for_submitted_count(1).await;
        assert_eq!(submitted.len(), 1, "send_transaction_async's spawned task must deliver the transaction with no UI/channel involved");
        assert_eq!(submitted[0].outputs[0].value, 100);

        std::fs::remove_file(&recipient_key_path).ok();
    }

    #[tokio::test]
    async fn create_transaction_with_per_byte_fee_pays_the_size_based_fee_it_computed() {
        let fake = FakeNode::spawn(Duration::from_millis(0)).await;

        let sender_private = PrivateKey::new_key();
        let sender_public = sender_private.public_key();
        let mut utxos = UtxoStore::new();
        utxos.add_key(LoadedKey { public: sender_public.clone(), private: sender_private });
        // One large UTXO -- guarantees a change output, so the resulting
        // transaction always has exactly 2 outputs regardless of exact
        // byte-size estimates, keeping this test deterministic.
        utxos.utxos.insert(
            sender_public.clone(),
            vec![(false, TransactionOutput { value: 1_000_000, unique_id: Uuid::new_v4(), pubkey: sender_public })],
        );

        let mut config = empty_config();
        config.fee_config = FeeConfig { fee_type: FeeType::PerByte, value: 5.0 };
        let core = connect_core(&fake, config, utxos).await;

        let recipient = PrivateKey::new_key().public_key();
        let transaction = core.create_transaction(&recipient, 1_000).unwrap();

        let output_sum: u64 = transaction.outputs.iter().map(|o| o.value).sum();
        let fee_actually_paid = 1_000_000 - output_sum;
        let expected_fee = FeeConfig { fee_type: FeeType::PerByte, value: 5.0 }.calculate_fee(
            1_000,
            transaction.inputs.len(),
            transaction.outputs.len(),
        );

        assert!(fee_actually_paid > 0, "a nonzero per-byte rate must produce a nonzero fee");
        assert_eq!(
            fee_actually_paid, expected_fee,
            "the fee actually paid must match the size-based rate for the transaction's real final shape"
        );
    }
}

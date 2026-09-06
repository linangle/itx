use tracing::*;

use crate::board::{ExchangeAccount, Order, PendingDeposit, Reputation, Task, Trade};
use btclib::crypto::PublicKey;
use redb::{ReadableTable, TableDefinition};
use std::path::Path;
use thiserror::Error;

const TASKS_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("tasks");
const REPUTATION_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("reputation");
// pubkey sec1 bytes -> grant time (Unix seconds). Mirrors the node's own
// bans table: same "durable set of pubkeys/IPs with a timestamp" shape.
const FAUCET_GRANTS_TABLE: TableDefinition<&[u8], i64> = TableDefinition::new("faucet_grants");
// uuid bytes -> serialized PendingDeposit (private key included -- see
// its own doc comment for why this must be durable before its address is
// ever handed out). Additive relative to the schema this hub shipped
// with: an old store simply gains this table empty the first time it's
// opened by build that knows about it, same as any other missing table
// `open_or_create` creates on demand -- no version bump needed for a
// purely-additive table.
const PENDING_DEPOSITS_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("pending_deposits");
// pubkey sec1 bytes -> serialized ExchangeAccount, same shape as
// REPUTATION_TABLE. uuid bytes -> serialized Order/Trade, same shape as
// TASKS_TABLE/PENDING_DEPOSITS_TABLE. Purely additive, like
// PENDING_DEPOSITS_TABLE was -- no SCHEMA_VERSION bump, that's only for
// breaking changes to an *existing* table's shape.
const EXCHANGE_ACCOUNTS_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("exchange_accounts");
const ORDERS_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("orders");
const TRADES_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("trades");
// pubkey sec1 bytes -> the agent's display name (see `crate::names`).
// Additive in exactly the same way PENDING_DEPOSITS_TABLE above is, and
// for the same reason no version bump is needed: an existing store gains
// the table empty on the first open by a build that knows about it, and
// a build that doesn't know about it never looks. The name is stored as
// a plain string rather than as the (descriptor, subject) pair it was
// built from, so a name already handed out keeps working even if the
// word it came from is later edited out of `wordlist/`.
const AGENT_NAMES_TABLE: TableDefinition<&[u8], &str> = TableDefinition::new("agent_names");
// raw signature bytes -> when this hub first accepted them (Unix
// seconds). The durable half of `auth::ReplayGuard`: the in-memory set
// alone starts empty on every boot, which reopens a
// `MAX_REQUEST_DRIFT_SECONDS` replay window across restarts. Exactly the
// "durable set of bytes with a timestamp" shape FAUCET_GRANTS_TABLE
// already is, and additive in the same way PENDING_DEPOSITS_TABLE was --
// an old store gains it empty on the first open by a build that knows
// about it, so no SCHEMA_VERSION bump.
//
// Unlike every other table here this one is *garbage*, not records: an
// entry is meaningless once its signature can no longer pass the drift
// check, and the sweep prunes it (see `prune_seen_signatures`). Without
// that it would grow by one row per authenticated request forever.
const REPLAY_GUARD_TABLE: TableDefinition<&[u8], i64> = TableDefinition::new("replay_guard");
const META_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

const SCHEMA_VERSION_KEY: &str = "schema_version";
// No migrations exist yet since this is the hub's first schema, but the
// version is still stamped from day one -- retrofitting that detection
// after the fact (rather than before the first real store exists) is
// exactly the mistake this project already made once with BlockStore.
const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum HubStoreError {
    #[error("database error: {0}")]
    Database(#[from] redb::DatabaseError),
    #[error("transaction error: {0}")]
    Transaction(#[from] redb::TransactionError),
    #[error("table error: {0}")]
    Table(#[from] redb::TableError),
    #[error("storage error: {0}")]
    Storage(#[from] redb::StorageError),
    #[error("commit error: {0}")]
    Commit(#[from] redb::CommitError),
    #[error("failed to (de)serialize: {0}")]
    Serialization(String),
    #[error("malformed public key in storage: {0}")]
    BadPublicKey(String),
    #[error("store was created by schema version {found}, this build expects {expected}")]
    UnsupportedSchemaVersion { found: u32, expected: u32 },
    #[error("stored schema version record is corrupt")]
    CorruptSchemaVersion,
}

pub type Result<T> = std::result::Result<T, HubStoreError>;

/// Durable, crash-safe persistence for the task board, mirroring
/// `btclib::store::BlockStore`'s design: each write is one atomic redb
/// transaction, and every entity (task, reputation record, faucet grant)
/// is stored individually rather than as one big serialized blob that has
/// to be rewritten in full on every change.
pub struct HubStore {
    db: redb::Database,
}

impl HubStore {
    pub fn open_or_create<P: AsRef<Path>>(path: P) -> Result<Self> {
        let db = redb::Database::create(path)?;
        let write_txn = db.begin_write()?;
        {
            write_txn.open_table(TASKS_TABLE)?;
            write_txn.open_table(REPUTATION_TABLE)?;
            write_txn.open_table(FAUCET_GRANTS_TABLE)?;
            write_txn.open_table(PENDING_DEPOSITS_TABLE)?;
            write_txn.open_table(EXCHANGE_ACCOUNTS_TABLE)?;
            write_txn.open_table(ORDERS_TABLE)?;
            write_txn.open_table(TRADES_TABLE)?;
            write_txn.open_table(AGENT_NAMES_TABLE)?;
            write_txn.open_table(REPLAY_GUARD_TABLE)?;
            let mut meta = write_txn.open_table(META_TABLE)?;

            let stored_version = match meta.get(SCHEMA_VERSION_KEY)? {
                Some(value) => {
                    let bytes: [u8; 4] = value
                        .value()
                        .try_into()
                        .map_err(|_| HubStoreError::CorruptSchemaVersion)?;
                    Some(u32::from_be_bytes(bytes))
                }
                None => None,
            };
            match stored_version {
                Some(found) if found != SCHEMA_VERSION => {
                    return Err(HubStoreError::UnsupportedSchemaVersion {
                        found,
                        expected: SCHEMA_VERSION,
                    });
                }
                Some(_) => {}
                None => {
                    meta.insert(SCHEMA_VERSION_KEY, SCHEMA_VERSION.to_be_bytes().as_slice())?;
                }
            }
        }
        write_txn.commit()?;
        Ok(HubStore { db })
    }

    pub fn save_task(&self, task: &Task) -> Result<()> {
        let mut bytes = Vec::new();
        ciborium::into_writer(task, &mut bytes)
            .map_err(|e| HubStoreError::Serialization(e.to_string()))?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(TASKS_TABLE)?;
            table.insert(task.id.as_bytes().as_slice(), bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn load_all_tasks(&self) -> Result<Vec<Task>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TASKS_TABLE)?;
        table
            .iter()?
            .map(|entry| {
                let (_, value) = entry?;
                ciborium::from_reader(value.value())
                    .map_err(|e: ciborium::de::Error<_>| HubStoreError::Serialization(e.to_string()))
            })
            .collect()
    }

    /// Persists `deposit`, including its private key. Callers must
    /// complete this **before** ever handing the deposit's address out in
    /// an HTTP response -- see `PendingDeposit`'s own doc comment for why
    /// that ordering is non-negotiable (a crash between generating the
    /// keypair and this write committing would make any funds later sent
    /// there permanently unrecoverable).
    pub fn save_pending_deposit(&self, deposit: &PendingDeposit) -> Result<()> {
        let mut bytes = Vec::new();
        ciborium::into_writer(deposit, &mut bytes)
            .map_err(|e| HubStoreError::Serialization(e.to_string()))?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(PENDING_DEPOSITS_TABLE)?;
            table.insert(deposit.id.as_bytes().as_slice(), bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn load_all_pending_deposits(&self) -> Result<Vec<PendingDeposit>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(PENDING_DEPOSITS_TABLE)?;
        table
            .iter()?
            .map(|entry| {
                let (_, value) = entry?;
                ciborium::from_reader(value.value())
                    .map_err(|e: ciborium::de::Error<_>| HubStoreError::Serialization(e.to_string()))
            })
            .collect()
    }

    pub fn save_reputation(&self, pubkey: &PublicKey, reputation: &Reputation) -> Result<()> {
        let mut bytes = Vec::new();
        ciborium::into_writer(reputation, &mut bytes)
            .map_err(|e| HubStoreError::Serialization(e.to_string()))?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(REPUTATION_TABLE)?;
            table.insert(pubkey.to_sec1_bytes().as_slice(), bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Same as `save_reputation`, but for several pubkeys at once in a
    /// single redb write transaction (one fsync total) rather than one
    /// per entry. A `Consensus` task's resolution can update every
    /// assignee's reputation in one go (see `Task::consensus_assignees`),
    /// so callers persisting that should batch here instead of looping
    /// over individual `save_reputation` calls.
    pub fn save_reputation_batch(&self, entries: &[(PublicKey, Reputation)]) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(REPUTATION_TABLE)?;
            for (pubkey, reputation) in entries {
                let mut bytes = Vec::new();
                ciborium::into_writer(reputation, &mut bytes)
                    .map_err(|e| HubStoreError::Serialization(e.to_string()))?;
                table.insert(pubkey.to_sec1_bytes().as_slice(), bytes.as_slice())?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn load_all_reputation(&self) -> Result<Vec<(PublicKey, Reputation)>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(REPUTATION_TABLE)?;
        table
            .iter()?
            .map(|entry| {
                let (key, value) = entry?;
                let pubkey = PublicKey::from_sec1_bytes(key.value())
                    .map_err(|e| HubStoreError::BadPublicKey(e.to_string()))?;
                let reputation = ciborium::from_reader(value.value())
                    .map_err(|e: ciborium::de::Error<_>| HubStoreError::Serialization(e.to_string()))?;
                Ok((pubkey, reputation))
            })
            .collect()
    }

    pub fn save_faucet_grant(&self, pubkey: &PublicKey, granted_at_unix: i64) -> Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(FAUCET_GRANTS_TABLE)?;
            table.insert(pubkey.to_sec1_bytes().as_slice(), granted_at_unix)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn load_all_faucet_grants(&self) -> Result<Vec<PublicKey>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(FAUCET_GRANTS_TABLE)?;
        table
            .iter()?
            .map(|entry| {
                let (key, _) = entry?;
                PublicKey::from_sec1_bytes(key.value())
                    .map_err(|e| HubStoreError::BadPublicKey(e.to_string()))
            })
            .collect()
    }

    pub fn save_exchange_account(&self, pubkey: &PublicKey, account: &ExchangeAccount) -> Result<()> {
        let mut bytes = Vec::new();
        ciborium::into_writer(account, &mut bytes)
            .map_err(|e| HubStoreError::Serialization(e.to_string()))?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(EXCHANGE_ACCOUNTS_TABLE)?;
            table.insert(pubkey.to_sec1_bytes().as_slice(), bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Same as `save_exchange_account`, but for several accounts at once
    /// in a single redb write transaction -- mirrors
    /// `save_reputation_batch`'s reasoning exactly, for the same reason:
    /// an escrow-funded task's multi-winner settlement can credit
    /// several recipients' compute balances in one go.
    pub fn save_exchange_account_batch(&self, entries: &[(PublicKey, ExchangeAccount)]) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(EXCHANGE_ACCOUNTS_TABLE)?;
            for (pubkey, account) in entries {
                let mut bytes = Vec::new();
                ciborium::into_writer(account, &mut bytes)
                    .map_err(|e| HubStoreError::Serialization(e.to_string()))?;
                table.insert(pubkey.to_sec1_bytes().as_slice(), bytes.as_slice())?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn load_all_exchange_accounts(&self) -> Result<Vec<(PublicKey, ExchangeAccount)>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(EXCHANGE_ACCOUNTS_TABLE)?;
        table
            .iter()?
            .map(|entry| {
                let (key, value) = entry?;
                let pubkey = PublicKey::from_sec1_bytes(key.value())
                    .map_err(|e| HubStoreError::BadPublicKey(e.to_string()))?;
                let account = ciborium::from_reader(value.value())
                    .map_err(|e: ciborium::de::Error<_>| HubStoreError::Serialization(e.to_string()))?;
                Ok((pubkey, account))
            })
            .collect()
    }

    pub fn save_order(&self, order: &Order) -> Result<()> {
        let mut bytes = Vec::new();
        ciborium::into_writer(order, &mut bytes)
            .map_err(|e| HubStoreError::Serialization(e.to_string()))?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(ORDERS_TABLE)?;
            table.insert(order.id.as_bytes().as_slice(), bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn load_all_orders(&self) -> Result<Vec<Order>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(ORDERS_TABLE)?;
        table
            .iter()?
            .map(|entry| {
                let (_, value) = entry?;
                ciborium::from_reader(value.value())
                    .map_err(|e: ciborium::de::Error<_>| HubStoreError::Serialization(e.to_string()))
            })
            .collect()
    }

    pub fn save_trade(&self, trade: &Trade) -> Result<()> {
        let mut bytes = Vec::new();
        ciborium::into_writer(trade, &mut bytes)
            .map_err(|e| HubStoreError::Serialization(e.to_string()))?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(TRADES_TABLE)?;
            table.insert(trade.id.as_bytes().as_slice(), bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn load_all_trades(&self) -> Result<Vec<Trade>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TRADES_TABLE)?;
        table
            .iter()?
            .map(|entry| {
                let (_, value) = entry?;
                ciborium::from_reader(value.value())
                    .map_err(|e: ciborium::de::Error<_>| HubStoreError::Serialization(e.to_string()))
            })
            .collect()
    }

    /// Persists one agent's display name.
    ///
    /// Names must be durable to be worth having: an agent that came back
    /// as a different name after a hub restart would be actively
    /// misleading, since the leaderboard is the one place a human tracks
    /// an agent over time. Callers write this immediately after
    /// `names::NameRegistry::assign` reports a freshly-minted name.
    pub fn save_agent_name(&self, pubkey: &PublicKey, name: &str) -> Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(AGENT_NAMES_TABLE)?;
            table.insert(pubkey.to_sec1_bytes().as_slice(), name)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Same as `save_agent_name` for several agents in one transaction
    /// (one fsync total), mirroring `save_reputation_batch`. Used by the
    /// startup backfill, which names every pre-existing agent at once.
    pub fn save_agent_name_batch(&self, entries: &[(PublicKey, String)]) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(AGENT_NAMES_TABLE)?;
            for (pubkey, name) in entries {
                table.insert(pubkey.to_sec1_bytes().as_slice(), name.as_str())?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn load_all_agent_names(&self) -> Result<Vec<(PublicKey, String)>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(AGENT_NAMES_TABLE)?;
        table
            .iter()?
            .map(|entry| {
                let (key, value) = entry?;
                let pubkey = PublicKey::from_sec1_bytes(key.value())
                    .map_err(|e| HubStoreError::BadPublicKey(e.to_string()))?;
                Ok((pubkey, value.value().to_string()))
            })
            .collect()
    }

    /// Durably records that `signature` has been accepted, so a restart
    /// cannot forget it while it is still inside its drift window.
    ///
    /// Callers must let this commit **before** acting on the request the
    /// signature authenticates -- the same ordering rule
    /// `save_pending_deposit` documents, for the same reason. Reversed,
    /// a crash between the effect landing and this write committing
    /// leaves an envelope that has already moved money and is still
    /// replayable, which is precisely the hole being closed.
    ///
    /// One redb commit, and therefore one fsync, per authenticated
    /// request. That cost is why the ordering above cannot be traded for
    /// a batched or write-behind flush: batching wins back the fsync by
    /// giving up the guarantee that makes the record worth writing.
    pub fn record_seen_signature(&self, signature: &[u8], seen_at_unix: i64) -> Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(REPLAY_GUARD_TABLE)?;
            table.insert(signature, seen_at_unix)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Every recorded signature still new enough to be replayable --
    /// seen strictly after `cutoff_unix`. Anything older already fails
    /// the drift check on its own, so restoring it would only cost
    /// memory. Read at boot to refill the in-memory guard.
    pub fn load_recent_signatures(&self, cutoff_unix: i64) -> Result<Vec<(Vec<u8>, i64)>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(REPLAY_GUARD_TABLE)?;
        let mut recent = Vec::new();
        for entry in table.iter()? {
            let (key, value) = entry?;
            let seen_at = value.value();
            if seen_at > cutoff_unix {
                recent.push((key.value().to_vec(), seen_at));
            }
        }
        Ok(recent)
    }

    /// Drops every signature seen at or before `cutoff_unix`, returning
    /// how many went. The mirror of `auth::cleanup_replay_guard`'s
    /// in-memory eviction, on the same cadence and the same cutoff --
    /// this table is the only one here that is pure garbage collection,
    /// and without this it grows by a row per authenticated request for
    /// the life of the deployment.
    pub fn prune_seen_signatures(&self, cutoff_unix: i64) -> Result<usize> {
        let mut pruned = 0usize;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(REPLAY_GUARD_TABLE)?;
            table.retain(|_, seen_at| {
                let keep = seen_at > cutoff_unix;
                if !keep {
                    pruned += 1;
                }
                keep
            })?;
        }
        write_txn.commit()?;
        Ok(pruned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::TaskStatus;
    use btclib::crypto::PrivateKey;
    use btclib::sha256::Hash;
    use chrono::Utc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use uuid::Uuid;

    fn temp_db_path(name: &str) -> std::path::PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "itx_hub_store_test_{name}_{}_{n}.redb",
            std::process::id()
        ))
    }

    #[test]
    fn round_trips_a_task_reputation_and_faucet_grant() {
        let path = temp_db_path("roundtrip");
        let store = HubStore::open_or_create(&path).unwrap();

        let poster = PrivateKey::new_key().public_key();
        let task = Task {
            id: Uuid::new_v4(),
            description: "do a thing".to_string(),
            bounty: 42,
            kind: crate::board::TaskKind::HashMatch {
                expected_output_hash: Hash::hash_bytes(b"answer"),
            },
            poster,
            status: TaskStatus::Open,
            claimant: None,
            claim_deadline: None,
            failed_attempts: 0,
            created_at: Utc::now(),
            min_reputation: 0,
            close_reason: None,
            escrow_id: None,
            capabilities: Default::default(),
        };
        store.save_task(&task).unwrap();
        let loaded = store.load_all_tasks().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, task.id);
        assert_eq!(loaded[0].bounty, 42);

        let agent = PrivateKey::new_key().public_key();
        let reputation = Reputation {
            completed: 3,
            failed: 1,
            total_earned: 300,
        };
        store.save_reputation(&agent, &reputation).unwrap();
        let loaded_rep = store.load_all_reputation().unwrap();
        assert_eq!(loaded_rep.len(), 1);
        assert_eq!(loaded_rep[0].0, agent);
        assert_eq!(loaded_rep[0].1.total_earned, 300);

        store.save_faucet_grant(&agent, Utc::now().timestamp()).unwrap();
        let grants = store.load_all_faucet_grants().unwrap();
        assert_eq!(grants, vec![agent]);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn round_trips_a_pending_deposit_without_persisting_any_key_material() {
        let path = temp_db_path("pending_deposit_roundtrip");
        let store = HubStore::open_or_create(&path).unwrap();

        let secret = crate::escrow_key::EscrowSecret::generate();
        let depositor = PrivateKey::new_key().public_key();
        let id = Uuid::new_v4();
        let deposit_pubkey = secret.derive(id).public_key();
        let deposit = crate::board::PendingDeposit {
            id,
            depositor,
            deposit_pubkey: deposit_pubkey.clone(),
            deposit_private_key: None,
            required_amount: 500,
            purpose: crate::board::EscrowPurpose::FundHashMatchTask(crate::board::TaskIntent {
                description: "t".to_string(),
                bounty: 500,
                expected_output_hash: Hash::hash_bytes(b"x"),
                min_reputation: 0,
                capabilities: Default::default(),
            }),
            status: crate::board::EscrowStatus::Reserved,
            created_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(30),
        };
        store.save_pending_deposit(&deposit).unwrap();

        let loaded = store.load_all_pending_deposits().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, deposit.id);
        assert_eq!(loaded[0].deposit_pubkey, deposit_pubkey);
        assert_eq!(loaded[0].required_amount, 500);
        assert!(
            loaded[0].deposit_private_key.is_none(),
            "the store must never hold escrow key material -- a leaked database would \
             otherwise be a leaked treasury"
        );
        // What replaces it: the key is reproducible from the secret and
        // the deposit's own id, so a restarted hub can still sweep this
        // address even though nothing about the key was written down.
        assert_eq!(
            loaded[0].private_key(&secret).public_key(),
            deposit_pubkey,
            "the derived key must still control the address the depositor was given"
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn save_reputation_batch_writes_every_entry_in_one_transaction() {
        let path = temp_db_path("batch");
        let store = HubStore::open_or_create(&path).unwrap();

        let entries: Vec<(PublicKey, Reputation)> = (0..3)
            .map(|i| {
                (
                    PrivateKey::new_key().public_key(),
                    Reputation { completed: i, failed: 0, total_earned: i * 100 },
                )
            })
            .collect();
        store.save_reputation_batch(&entries).unwrap();

        let loaded = store.load_all_reputation().unwrap();
        assert_eq!(loaded.len(), 3);
        for (pubkey, reputation) in &entries {
            let found = loaded.iter().find(|(k, _)| k == pubkey).unwrap();
            assert_eq!(found.1.completed, reputation.completed);
            assert_eq!(found.1.total_earned, reputation.total_earned);
        }

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn save_reputation_batch_of_zero_entries_is_a_harmless_no_op() {
        let path = temp_db_path("batch_empty");
        let store = HubStore::open_or_create(&path).unwrap();
        store.save_reputation_batch(&[]).unwrap();
        assert!(store.load_all_reputation().unwrap().is_empty());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn round_trips_an_exchange_account() {
        let path = temp_db_path("exchange_account_roundtrip");
        let store = HubStore::open_or_create(&path).unwrap();

        let owner = PrivateKey::new_key().public_key();
        let account = ExchangeAccount {
            base_balance: 1_000,
            locked_base: 200,
            compute_balance: 50,
            locked_compute: 10,
        };
        store.save_exchange_account(&owner, &account).unwrap();

        let loaded = store.load_all_exchange_accounts().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].0, owner);
        assert_eq!(loaded[0].1.base_balance, 1_000);
        assert_eq!(loaded[0].1.locked_compute, 10);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn save_exchange_account_batch_writes_every_entry_in_one_transaction() {
        let path = temp_db_path("exchange_account_batch");
        let store = HubStore::open_or_create(&path).unwrap();

        let entries: Vec<(PublicKey, ExchangeAccount)> = (0..3)
            .map(|i| {
                (
                    PrivateKey::new_key().public_key(),
                    ExchangeAccount { base_balance: i * 100, locked_base: 0, compute_balance: i, locked_compute: 0 },
                )
            })
            .collect();
        store.save_exchange_account_batch(&entries).unwrap();

        let loaded = store.load_all_exchange_accounts().unwrap();
        assert_eq!(loaded.len(), 3);
        for (pubkey, account) in &entries {
            let found = loaded.iter().find(|(k, _)| k == pubkey).unwrap();
            assert_eq!(found.1.base_balance, account.base_balance);
        }

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn save_exchange_account_batch_of_zero_entries_is_a_harmless_no_op() {
        let path = temp_db_path("exchange_account_batch_empty");
        let store = HubStore::open_or_create(&path).unwrap();
        store.save_exchange_account_batch(&[]).unwrap();
        assert!(store.load_all_exchange_accounts().unwrap().is_empty());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn round_trips_an_order() {
        let path = temp_db_path("order_roundtrip");
        let store = HubStore::open_or_create(&path).unwrap();

        let owner = PrivateKey::new_key().public_key();
        let order = Order {
            id: Uuid::new_v4(),
            owner: owner.clone(),
            side: crate::board::Side::Buy,
            price: 10,
            quantity: 50,
            filled: 20,
            status: crate::board::OrderStatus::Open,
            created_at: Utc::now(),
        };
        store.save_order(&order).unwrap();

        let loaded = store.load_all_orders().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, order.id);
        assert_eq!(loaded[0].owner, owner);
        assert_eq!(loaded[0].filled, 20);
        assert_eq!(loaded[0].status, crate::board::OrderStatus::Open);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn round_trips_a_trade() {
        let path = temp_db_path("trade_roundtrip");
        let store = HubStore::open_or_create(&path).unwrap();

        let buyer = PrivateKey::new_key().public_key();
        let seller = PrivateKey::new_key().public_key();
        let trade = Trade {
            id: Uuid::new_v4(),
            buy_order_id: Uuid::new_v4(),
            sell_order_id: Uuid::new_v4(),
            buyer: buyer.clone(),
            seller: seller.clone(),
            price: 8,
            quantity: 50,
            executed_at: Utc::now(),
            taker_side: crate::board::Side::Buy,
            taker_fee: 0,
        };
        store.save_trade(&trade).unwrap();

        let loaded = store.load_all_trades().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, trade.id);
        assert_eq!(loaded[0].buyer, buyer);
        assert_eq!(loaded[0].seller, seller);
        assert_eq!(loaded[0].quantity, 50);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn round_trips_agent_names_singly_and_in_a_batch() {
        let path = temp_db_path("agent_names");
        let store = HubStore::open_or_create(&path).unwrap();
        assert!(store.load_all_agent_names().unwrap().is_empty());

        let solo = PrivateKey::new_key().public_key();
        store.save_agent_name(&solo, "SwiftWarlock").unwrap();

        let batch: Vec<(PublicKey, String)> = ["AmberOtter", "CrimsonHydra"]
            .iter()
            .map(|name| (PrivateKey::new_key().public_key(), name.to_string()))
            .collect();
        store.save_agent_name_batch(&batch).unwrap();

        let loaded = store.load_all_agent_names().unwrap();
        assert_eq!(loaded.len(), 3);
        let find = |key: &PublicKey| {
            loaded.iter().find(|(k, _)| k == key).map(|(_, n)| n.clone()).unwrap()
        };
        assert_eq!(find(&solo), "SwiftWarlock");
        for (pubkey, name) in &batch {
            assert_eq!(&find(pubkey), name);
        }

        // re-saving the same pubkey replaces rather than duplicates, so a
        // restart never sees two names for one agent
        store.save_agent_name(&solo, "SwiftWarlock").unwrap();
        assert_eq!(store.load_all_agent_names().unwrap().len(), 3);

        store.save_agent_name_batch(&[]).unwrap();
        assert_eq!(store.load_all_agent_names().unwrap().len(), 3);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn round_trips_and_prunes_seen_signatures() {
        let path = temp_db_path("replay_guard");
        let store = HubStore::open_or_create(&path).unwrap();
        assert!(store.load_recent_signatures(i64::MIN).unwrap().is_empty());

        let now = Utc::now().timestamp();
        store.record_seen_signature(b"old", now - 500).unwrap();
        store.record_seen_signature(b"new", now).unwrap();

        // `load_recent_signatures` is a cutoff read, not a full scan:
        // this is what a restart uses to skip signatures that already
        // fail the drift check on their own.
        let recent = store.load_recent_signatures(now - 100).unwrap();
        assert_eq!(recent, vec![(b"new".to_vec(), now)]);
        assert_eq!(store.load_recent_signatures(i64::MIN).unwrap().len(), 2);

        // Re-recording one is a replace, not a duplicate -- a signature
        // is a set member here, and the guard claims each one once.
        store.record_seen_signature(b"new", now + 1).unwrap();
        assert_eq!(store.load_recent_signatures(i64::MIN).unwrap().len(), 2);

        assert_eq!(store.prune_seen_signatures(now - 100).unwrap(), 1);
        assert_eq!(store.load_recent_signatures(i64::MIN).unwrap().len(), 1);
        // Pruning again removes nothing: the sweep runs every minute
        // forever, so it has to be idempotent and cheap when idle.
        assert_eq!(store.prune_seen_signatures(now - 100).unwrap(), 0);

        std::fs::remove_file(&path).ok();
    }

    /// The replay table is purely additive, so a store written before it
    /// existed must open on this build untouched -- no version bump, no
    /// migration, the table simply arrives empty. Same reasoning
    /// PENDING_DEPOSITS_TABLE's own comment records; this is the test
    /// that keeps it honest.
    #[test]
    fn a_store_from_a_build_without_the_replay_table_still_opens() {
        let path = temp_db_path("older_build");
        let agent = PrivateKey::new_key().public_key();
        {
            // Exactly what an older build's `open_or_create` did: the
            // tables it knew about, stamped with the same schema version
            // it stamps today.
            let db = redb::Database::create(&path).unwrap();
            let write_txn = db.begin_write().unwrap();
            {
                write_txn.open_table(TASKS_TABLE).unwrap();
                write_txn.open_table(REPUTATION_TABLE).unwrap();
                write_txn.open_table(FAUCET_GRANTS_TABLE).unwrap();
                let mut meta = write_txn.open_table(META_TABLE).unwrap();
                meta.insert(SCHEMA_VERSION_KEY, SCHEMA_VERSION.to_be_bytes().as_slice()).unwrap();
                let mut grants = write_txn.open_table(FAUCET_GRANTS_TABLE).unwrap();
                grants.insert(agent.to_sec1_bytes().as_slice(), 1_700_000_000i64).unwrap();
            }
            write_txn.commit().unwrap();
        }

        let store = HubStore::open_or_create(&path).unwrap();
        assert!(
            store.load_recent_signatures(i64::MIN).unwrap().is_empty(),
            "the new table must arrive empty rather than failing to open"
        );
        assert_eq!(
            store.load_all_faucet_grants().unwrap(),
            vec![agent],
            "and the store's existing contents must survive untouched"
        );
        // Usable immediately, not just openable.
        store.record_seen_signature(b"sig", Utc::now().timestamp()).unwrap();
        assert_eq!(store.load_recent_signatures(i64::MIN).unwrap().len(), 1);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn rejects_a_store_from_a_newer_unknown_version() {
        let path = temp_db_path("newer_version");
        {
            let db = redb::Database::create(&path).unwrap();
            let write_txn = db.begin_write().unwrap();
            {
                write_txn.open_table(TASKS_TABLE).unwrap();
                write_txn.open_table(REPUTATION_TABLE).unwrap();
                write_txn.open_table(FAUCET_GRANTS_TABLE).unwrap();
                let mut meta = write_txn.open_table(META_TABLE).unwrap();
                meta.insert(
                    SCHEMA_VERSION_KEY,
                    (SCHEMA_VERSION + 1).to_be_bytes().as_slice(),
                )
                .unwrap();
            }
            write_txn.commit().unwrap();
        }

        let result = HubStore::open_or_create(&path);
        assert!(matches!(
            result,
            Err(HubStoreError::UnsupportedSchemaVersion { .. })
        ));

        std::fs::remove_file(&path).ok();
    }
}

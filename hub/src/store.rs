use tracing::*;

use crate::board::{ExchangeAccount, Order, PayoutAttempt, PendingDeposit, Reputation, Task, Trade};
use btclib::crypto::PublicKey;
use redb::{ReadableTable, TableDefinition};
use std::path::Path;
use thiserror::Error;

const TASKS_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("tasks");
const REPUTATION_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("reputation");
// pubkey sec1 bytes -> grant time (Unix seconds). Mirrors the node's own
// bans table: same "durable set of pubkeys/IPs with a timestamp" shape.
const FAUCET_GRANTS_TABLE: TableDefinition<&[u8], i64> = TableDefinition::new("faucet_grants");
// challenge uuid bytes -> serialized `faucet_pow::Challenge`. The
// faucet's proof-of-work challenges, outstanding and redeemed alike
// (plan §5).
//
// Durable rather than in-memory-only for the reason FAUCET_GRANTS_TABLE
// is: a restart must not hand back a grant, and here it must not hand
// back a *solved challenge* either. An attacker who watched the hub go
// down would otherwise replay a solution it had already spent. Additive
// in the same way REPLAY_GUARD_TABLE was, so an older store gains it
// empty. It arrived under the no-bump policy `SCHEMA_VERSION` has since
// reversed -- and this table is the sharpest illustration of why, since
// a build that predates it opens the store and treats every redeemed
// challenge as unspent.
//
// Like the replay guard and unlike everything else here, part of this is
// garbage: an unredeemed challenge past its expiry can never be used
// again, and a redeemed one stops mattering once its own expiry is far
// enough behind. `prune_faucet_challenges` collects both.
const FAUCET_CHALLENGES_TABLE: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("faucet_challenges");
// uuid bytes -> serialized PendingDeposit (private key included -- see
// its own doc comment for why this must be durable before its address is
// ever handed out). Additive relative to the schema this hub shipped
// with: an old store simply gains this table empty the first time it's
// opened by build that knows about it, same as any other missing table
// `open_or_create` creates on demand. It arrived under the no-bump
// policy `SCHEMA_VERSION` has since reversed; adding a table bumps the
// version now.
const PENDING_DEPOSITS_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("pending_deposits");
// pubkey sec1 bytes -> serialized ExchangeAccount, same shape as
// REPUTATION_TABLE. uuid bytes -> serialized Order/Trade, same shape as
// TASKS_TABLE/PENDING_DEPOSITS_TABLE. Purely additive, like
// PENDING_DEPOSITS_TABLE was. These arrived under the no-bump policy
// `SCHEMA_VERSION` has since reversed -- a bump is no longer reserved
// for breaking changes to an existing table's shape.
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
// about it. It arrived under the no-bump policy `SCHEMA_VERSION` has
// since reversed.
//
// Unlike every other table here this one is *garbage*, not records: an
// entry is meaningless once its signature can no longer pass the drift
// check, and the sweep prunes it (see `prune_seen_signatures`). Without
// that it would grow by one row per authenticated request forever.
const REPLAY_GUARD_TABLE: TableDefinition<&[u8], i64> = TableDefinition::new("replay_guard");
// task uuid bytes ++ recipient sec1 bytes -> serialized PayoutAttempt.
// A composite key rather than a nested map because that is the unit the
// record is written and deleted at: one payout to one recipient,
// resolved and dropped on its own even when several were paid by one
// transaction (see `PayoutAttempt`). Uuid bytes are fixed-width, so the
// concatenation parses back unambiguously, and it sorts by task, which
// is the order anything reading a whole task's payouts wants.
//
// Additive in exactly the way PENDING_DEPOSITS_TABLE and
// REPLAY_GUARD_TABLE were: a store written before this table existed
// gains it empty on the first open by a build that knows about it.
// Pinned by
// `a_store_from_a_build_without_the_payout_attempts_table_still_opens`.
//
// The sentence that used to end that paragraph -- "and a build that does
// not know about it never looks" -- was offered as reassurance and was
// the bug. A build that never looks re-sends or forgets every payout in
// flight. `SCHEMA_VERSION` now fences that off; see its comment.
//
// Unlike a task or a deposit, a record here is a claim about the *chain*
// and not about the hub's own bookkeeping: it exists only while the hub
// is waiting to find out what became of a transaction, and is deleted
// the moment it knows. It cannot grow without bound -- there is at most
// one row per unpaid payout, and every row leaves within
// `MAX_PAYOUT_SUBMISSIONS` sweeps of resolving.
const PAYOUT_ATTEMPTS_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("payout_attempts");
const META_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

const SCHEMA_VERSION_KEY: &str = "schema_version";
/// Stamped from day one -- retrofitting version detection after the
/// first real store exists is exactly the mistake this project already
/// made once with BlockStore.
///
/// **Bumped to 2 on the escrow-status work, and the policy that kept it
/// at 1 is reversed: adding a table bumps this.** Every table added
/// since the first schema -- pending deposits, exchange accounts,
/// orders, trades, names, the replay guard, faucet challenges, payout
/// attempts -- went in additively, each with a test pinning that an
/// older store still opens, and the version deliberately never moved.
///
/// That bought forward compatibility and paid for it with a silent
/// rollback hole. `open_or_create` refuses a *newer* stamp, which is
/// right; but because additive tables never bumped the stamp, an
/// **older binary opening a store written by a current one passed the
/// check and silently ignored every table it did not know**. Roll a
/// deploy back and in-flight `payout_attempts` become invisible, so
/// payouts are re-sent or forgotten, and redeemed `faucet_challenges`
/// become replayable -- which is precisely the hole that table was
/// added to close (plan §6.5c).
///
/// The compatibility the old policy bought is worth less than that
/// costs. An older store is still accepted -- the tables it lacks are
/// created above and arrive empty, which is what the additive-compat
/// tests check -- but opening it **restamps it to this version**, so
/// the binary that wrote it can no longer open it. That is the fence,
/// and it has to go up at open rather than at first write: from the
/// moment a current binary has the store, it may put a row in a table
/// the old one cannot see.
const SCHEMA_VERSION: u32 = 2;

/// The `PAYOUT_ATTEMPTS_TABLE` key for one payout: the task's uuid
/// followed by the recipient's SEC1 bytes. Uuid bytes are fixed-width,
/// so no separator is needed for the two halves to be unambiguous.
fn payout_attempt_key(task_id: uuid::Uuid, recipient: &PublicKey) -> Vec<u8> {
    let mut key = task_id.as_bytes().to_vec();
    key.extend_from_slice(&recipient.to_sec1_bytes());
    key
}

/// Serializes `value` and stages it into `table` within a write
/// transaction the caller owns, so that several records can share one
/// commit (see `HubStore::in_one_write_txn`). Encoding is ciborium,
/// byte-for-byte what the single-record `save_*` methods write, so a
/// record written through here reads back through the ordinary
/// `load_all_*` path with nothing to distinguish it.
fn stage_record<T: serde::Serialize>(
    txn: &redb::WriteTransaction,
    table: TableDefinition<'static, &'static [u8], &'static [u8]>,
    key: &[u8],
    value: &T,
) -> Result<()> {
    let mut bytes = Vec::new();
    ciborium::into_writer(value, &mut bytes)
        .map_err(|e| HubStoreError::Serialization(e.to_string()))?;
    let mut table = txn.open_table(table)?;
    table.insert(key, bytes.as_slice())?;
    Ok(())
}

/// `stage_record`'s counterpart: removes `key` from `table` inside a
/// write transaction the caller owns, so a deletion can share a commit
/// with the records that make it correct. Removing an absent key is not
/// an error in redb and is not one here either -- what a caller wants is
/// "this row is gone when the transaction commits", which is equally
/// true if it was already gone.
fn stage_delete(
    txn: &redb::WriteTransaction,
    table: TableDefinition<'static, &'static [u8], &'static [u8]>,
    key: &[u8],
) -> Result<()> {
    let mut table = txn.open_table(table)?;
    table.remove(key)?;
    Ok(())
}

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
            write_txn.open_table(FAUCET_CHALLENGES_TABLE)?;
            write_txn.open_table(PAYOUT_ATTEMPTS_TABLE)?;
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
                // Newer than this build understands: refuse. The tables
                // are already created by the block above, but nothing
                // here has committed, so the store is untouched.
                Some(found) if found > SCHEMA_VERSION => {
                    return Err(HubStoreError::UnsupportedSchemaVersion {
                        found,
                        expected: SCHEMA_VERSION,
                    });
                }
                // Older: usable as-is, since every table this build
                // knows about was just created and the ones it does not
                // recognise do not exist. Restamped so the build that
                // wrote it will refuse it from here on -- see
                // `SCHEMA_VERSION` for why that one-way door is the
                // point rather than a side effect.
                Some(found) if found < SCHEMA_VERSION => {
                    warn!(
                        "store was written by schema version {found}; upgrading the stamp to \
                         {SCHEMA_VERSION}. This is one-way: a build expecting version {found} \
                         will refuse this store from now on, deliberately, because it cannot \
                         see the tables this one writes."
                    );
                    meta.insert(SCHEMA_VERSION_KEY, SCHEMA_VERSION.to_be_bytes().as_slice())?;
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

    /// Persists a confirmed escrow's task and the deposit that funded
    /// it in **one** redb write transaction, so no reader and no restart
    /// can ever see one without the other.
    ///
    /// This exists because doing it as two commits was a money bug
    /// (plan §6.5b). `confirm_task_escrow` used to `save_task` and then
    /// `save_pending_deposit`; a process killed between the two left a
    /// task on disk beside a deposit still reading `Reserved`, and the
    /// depositor could confirm the same escrow a second time and get a
    /// second task out of one payment.
    ///
    /// The plan offered a cheaper fix -- write the deposit first, so a
    /// crash strands the deposit instead of duplicating it -- and that
    /// would have been an improvement, because a stranded deposit is
    /// visible and recoverable while a duplicate is neither. It was
    /// rejected anyway: it only makes the failure a better failure,
    /// leaving an interval whose safety depends on nothing ever being
    /// added between the two writes. One transaction has no interval at
    /// all, and the store was already redb, which is what its
    /// transactions are for. The stranded-deposit case simply stops
    /// existing rather than becoming the expected outcome.
    pub fn save_task_and_deposit(&self, task: &Task, deposit: &PendingDeposit) -> Result<()> {
        self.in_one_write_txn(|txn| {
            stage_record(txn, TASKS_TABLE, task.id.as_bytes().as_slice(), task)?;
            stage_record(
                txn,
                PENDING_DEPOSITS_TABLE,
                deposit.id.as_bytes().as_slice(),
                deposit,
            )
        })
    }

    /// The `confirm_exchange_deposit` counterpart of
    /// `save_task_and_deposit`: a credited exchange account and the
    /// deposit that credited it, committed together. Same bug, same
    /// reasoning -- see that method. The effect here is a ledger balance
    /// rather than a task, which is if anything worse, since a duplicate
    /// credit is spendable and tradeable the moment it lands.
    pub fn save_exchange_account_and_deposit(
        &self,
        pubkey: &PublicKey,
        account: &ExchangeAccount,
        deposit: &PendingDeposit,
    ) -> Result<()> {
        self.in_one_write_txn(|txn| {
            stage_record(
                txn,
                EXCHANGE_ACCOUNTS_TABLE,
                pubkey.to_sec1_bytes().as_slice(),
                account,
            )?;
            stage_record(
                txn,
                PENDING_DEPOSITS_TABLE,
                deposit.id.as_bytes().as_slice(),
                deposit,
            )
        })
    }

    /// A settled escrow's `Refunded` status and the reputation credit
    /// that settling it earned, committed together -- the
    /// `settle_dispute_bond` counterpart of `save_task_and_deposit`,
    /// and the same bug in the opposite direction.
    ///
    /// `Consumed` became durable when plan §6.5b added the pair-writers
    /// above; `Refunded` never did, and lived only in memory. So a
    /// forfeited dispute bond credited the winner's `total_earned`
    /// durably beside a deposit that reloaded as `Consumed`, and
    /// `TaskBoard::tasks_with_unsettled_dispute_bonds` selected it again
    /// at every boot for the life of the deployment -- each pass a node
    /// round trip inside the sweep, ahead of payout resolution.
    ///
    /// Not a second credit, though the handoff and §6.5c's first draft
    /// both said so: the re-settlement credits the *net amount the retry
    /// computed*, and the retry reads the drained address that already
    /// made its payment a no-op, so it adds zero. Measured, not reasoned
    /// -- see §6.5c. Worth knowing which way that cuts: the ledger
    /// survives only because the credit happens to be derived from a
    /// live balance instead of the recorded bond amount, which is one
    /// more accidental defence standing in for an intended one.
    ///
    /// Ordinary refunds have no companion record and use
    /// `save_pending_deposit`, which is already one transaction by
    /// itself. Only the forfeiture case needs a pair.
    pub fn save_deposit_and_reputation(
        &self,
        deposit: &PendingDeposit,
        pubkey: &PublicKey,
        reputation: &Reputation,
    ) -> Result<()> {
        self.in_one_write_txn(|txn| {
            stage_record(
                txn,
                PENDING_DEPOSITS_TABLE,
                deposit.id.as_bytes().as_slice(),
                deposit,
            )?;
            stage_record(
                txn,
                REPUTATION_TABLE,
                pubkey.to_sec1_bytes().as_slice(),
                reputation,
            )
        })
    }

    /// Everything the confirmation of one payout changes, in one redb
    /// transaction: the task (now `Paid`, or still waiting on a sibling
    /// leg), the recipient's reputation, the attempt the sweep was
    /// tracking it by (deleted), and -- for a task tagged "compute" --
    /// the exchange account that leg credits.
    ///
    /// `record_confirmed_payout` used to write these as up to four
    /// separate commits, and deleted the attempt *first*. A crash or a
    /// store error in between left a task reading `Submitted` on disk
    /// with nothing tracking it: `outstanding_payout_attempts` had
    /// nothing to resolve and `verified_unpaid_tasks` will not take a
    /// `Submitted` task either, so the money had moved on chain and the
    /// hub had permanently forgotten it. That is the precise failure
    /// mode plan §6.5 exists to eliminate, reintroduced in the function
    /// that closes §6.5's own loop (§6.5c).
    ///
    /// The deletion is what makes this a transaction rather than a
    /// batch. Committing the task without it re-resolves a finished
    /// payout once per sweep forever; committing it without the task is
    /// the lost payout above. Neither is a state any ordering of
    /// separate commits can rule out.
    pub fn save_confirmed_payout(
        &self,
        task: &Task,
        recipient: &PublicKey,
        reputation: &Reputation,
        compute_account: Option<&ExchangeAccount>,
    ) -> Result<()> {
        self.in_one_write_txn(|txn| {
            stage_record(txn, TASKS_TABLE, task.id.as_bytes().as_slice(), task)?;
            stage_record(
                txn,
                REPUTATION_TABLE,
                recipient.to_sec1_bytes().as_slice(),
                reputation,
            )?;
            if let Some(account) = compute_account {
                stage_record(
                    txn,
                    EXCHANGE_ACCOUNTS_TABLE,
                    recipient.to_sec1_bytes().as_slice(),
                    account,
                )?;
            }
            stage_delete(
                txn,
                PAYOUT_ATTEMPTS_TABLE,
                payout_attempt_key(task.id, recipient).as_slice(),
            )
        })
    }

    /// A task moving to `PayoutFailed` and the dropping of every payout
    /// attempt that went with it, in one transaction -- `abandon_payout`'s
    /// counterpart to `save_confirmed_payout`, and the same shape of bug.
    ///
    /// It deleted N attempts and *then* saved the task, so a crash
    /// between them produced the state its own comment says it exists to
    /// prevent: a task that is not terminal on disk beside attempts that
    /// are gone, which no sweep will ever resolve or retry. Terminal
    /// state and the removal of what tracked it are one fact.
    pub fn save_task_and_drop_payout_attempts(
        &self,
        task: &Task,
        dropped: &[(uuid::Uuid, PublicKey)],
    ) -> Result<()> {
        self.in_one_write_txn(|txn| {
            stage_record(txn, TASKS_TABLE, task.id.as_bytes().as_slice(), task)?;
            for (task_id, recipient) in dropped {
                stage_delete(
                    txn,
                    PAYOUT_ATTEMPTS_TABLE,
                    payout_attempt_key(*task_id, recipient).as_slice(),
                )?;
            }
            Ok(())
        })
    }

    /// A task and one agent's reputation, committed together --
    /// `persist_task_and_reputation`'s and `resolve_dispute`'s writer.
    ///
    /// Both wrote the two separately, and `resolve_dispute` swallowed the
    /// reputation error behind a 200. That is not only bookkeeping:
    /// reputation is the input to a task's `min_reputation` term, so a
    /// lost failure record lets a penalized agent keep claiming work a
    /// poster meant to exclude them from (§6.5c).
    pub fn save_task_and_reputation(
        &self,
        task: &Task,
        pubkey: &PublicKey,
        reputation: &Reputation,
    ) -> Result<()> {
        self.in_one_write_txn(|txn| {
            stage_record(txn, TASKS_TABLE, task.id.as_bytes().as_slice(), task)?;
            stage_record(
                txn,
                REPUTATION_TABLE,
                pubkey.to_sec1_bytes().as_slice(),
                reputation,
            )
        })
    }

    /// An order and its owner's ledger balance, committed together --
    /// `cancel_order`'s writer, and the one place in the exchange where
    /// splitting the two was exploitable with no crash at all.
    ///
    /// `TaskBoard::cancel_order` flips the order to `Cancelled` and
    /// releases its locked balance under one lock, which is right. The
    /// handler then wrote them as two commits and swallowed both errors
    /// behind a 200. If the account write landed and the order write did
    /// not, the order reloaded `Open` with its lock already released: the
    /// owner could withdraw the freed balance while the order stayed
    /// matchable on the book, and the fill then debited a balance that
    /// was no longer there. The reverse order stranded the lock forever,
    /// because `cancel_order` refuses an order that is not `Open`.
    ///
    /// An order's status and the balance that status implies are one
    /// fact, so they take one commit (§6.5d).
    pub fn save_order_and_account(
        &self,
        order: &Order,
        owner: &PublicKey,
        account: &ExchangeAccount,
    ) -> Result<()> {
        self.in_one_write_txn(|txn| {
            stage_record(txn, ORDERS_TABLE, order.id.as_bytes().as_slice(), order)?;
            stage_record(
                txn,
                EXCHANGE_ACCOUNTS_TABLE,
                owner.to_sec1_bytes().as_slice(),
                account,
            )
        })
    }

    /// `save_task_and_reputation` for a resolution that touched several
    /// agents at once: a `Consensus` task and every assignee's
    /// reputation in one commit.
    ///
    /// The consensus path was the worst of the split writes. The calling
    /// assignee's reputation went through one commit and everyone else's
    /// through a `save_reputation_batch` whose error was logged and
    /// dropped, so a lost batch silently forgave every agent who lost
    /// that round while the task recording the round stayed on disk.
    /// An empty `entries` is accepted and writes just the task, so a
    /// caller need not special-case a resolution that penalized nobody.
    pub fn save_task_and_reputation_batch(
        &self,
        task: &Task,
        entries: &[(PublicKey, Reputation)],
    ) -> Result<()> {
        self.in_one_write_txn(|txn| {
            stage_record(txn, TASKS_TABLE, task.id.as_bytes().as_slice(), task)?;
            for (pubkey, reputation) in entries {
                stage_record(
                    txn,
                    REPUTATION_TABLE,
                    pubkey.to_sec1_bytes().as_slice(),
                    reputation,
                )?;
            }
            Ok(())
        })
    }

    /// Runs `f` inside a single redb write transaction and commits only
    /// if it returns `Ok`. An `Err` returns without committing, and redb
    /// discards the whole transaction when it drops -- so every record
    /// `f` staged either becomes durable together or not at all.
    ///
    /// Kept as a named primitive rather than inlined into its two
    /// callers because "these records share a commit" is the property
    /// worth being able to point at, and because it is what lets the
    /// rollback be tested directly: a test can stage both records and
    /// then fail, which is the crash this exists to survive and the one
    /// thing no amount of killing a process can demonstrate reliably.
    fn in_one_write_txn<F>(&self, f: F) -> Result<()>
    where
        F: FnOnce(&redb::WriteTransaction) -> Result<()>,
    {
        let write_txn = self.db.begin_write()?;
        f(&write_txn)?;
        write_txn.commit()?;
        Ok(())
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
    /// per entry.
    ///
    /// Neither this nor `save_reputation` has a production caller any
    /// more, and that is the point rather than an oversight: every
    /// reputation change the hub makes is caused by something else it is
    /// also writing -- a task, a settled escrow -- and belongs in that
    /// record's transaction, not in one of its own (plan §6.5c). The
    /// `Consensus` resolution this was written for now goes through
    /// `save_task_and_reputation_batch`. Both are kept as the primitives
    /// those pair-writers are built out of and as the thing the
    /// round-trip tests exercise; a new caller wanting one should first
    /// ask what else it is committing.
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

    /// Persists an issued or updated challenge.
    ///
    /// **Deliberately not committed together with the faucet grant it
    /// pays for**, which is the opposite of the call §6.5b forced on the
    /// escrow side, and for a reason worth writing down: the two records
    /// want opposite failure modes.
    ///
    /// The redemption must be durable *before* the payout, or a hub that
    /// dies mid-payment comes back with the solution still spendable and
    /// the attacker replays it. That is the replay guard's rule and it
    /// applies here for the same reason.
    ///
    /// The grant must be durable *after* the payout, or a payment that
    /// fails leaves the key permanently marked as having been granted and
    /// locks out an agent that never received anything.
    ///
    /// One transaction cannot satisfy both. Splitting them means a crash
    /// between the two costs the agent its solved challenge and pays it
    /// nothing, which is recoverable -- it solves another -- and a crash
    /// after the payout can at worst grant twice, which now costs the
    /// attacker a full proof of work rather than being free.
    pub fn save_faucet_challenge(&self, challenge: &crate::faucet_pow::Challenge) -> Result<()> {
        self.in_one_write_txn(|txn| {
            stage_record(txn, FAUCET_CHALLENGES_TABLE, challenge.id.as_bytes(), challenge)
        })
    }

    /// Every challenge still worth holding, for the in-memory book to
    /// restore at boot.
    pub fn load_all_faucet_challenges(&self) -> Result<Vec<crate::faucet_pow::Challenge>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(FAUCET_CHALLENGES_TABLE)?;
        let mut out = Vec::new();
        for entry in table.iter()? {
            let (_, value) = entry?;
            let challenge: crate::faucet_pow::Challenge = ciborium::from_reader(value.value())
                .map_err(|e| HubStoreError::Serialization(e.to_string()))?;
            out.push(challenge);
        }
        Ok(out)
    }

    /// Drops challenges that can no longer affect any decision, and
    /// reports how many went.
    ///
    /// An unredeemed challenge is useless the moment it expires. A
    /// redeemed one still has a job -- refusing a second redemption --
    /// but only until its own expiry is `REDEEMED_RETENTION_SECONDS`
    /// behind, after which the challenge would be refused for being
    /// expired anyway and the record is only costing space.
    pub fn prune_faucet_challenges(&self, now_unix: i64) -> Result<usize> {
        let stale: Vec<crate::faucet_pow::Challenge> = self
            .load_all_faucet_challenges()?
            .into_iter()
            .filter(|c| {
                let horizon = if c.is_redeemed() {
                    c.expires_at + crate::faucet_pow::REDEEMED_RETENTION_SECONDS
                } else {
                    c.expires_at
                };
                now_unix > horizon
            })
            .collect();
        if stale.is_empty() {
            return Ok(0);
        }
        self.in_one_write_txn(|txn| {
            let mut table = txn.open_table(FAUCET_CHALLENGES_TABLE)?;
            for challenge in &stale {
                table.remove(challenge.id.as_bytes().as_slice())?;
            }
            Ok(())
        })?;
        Ok(stale.len())
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

    /// Persists a payout the hub has handed to the node and is waiting
    /// to see confirmed.
    ///
    /// Must commit **before** the transaction goes onto the wire, the
    /// same ordering rule `save_pending_deposit` and
    /// `record_seen_signature` both document, and for the same reason
    /// pointed the other way: a crash between submitting and writing
    /// leaves a payout in flight that nothing in the hub knows to look
    /// for, which is precisely the silent loss this whole mechanism
    /// exists to end. Writing first can at worst leave a record of a
    /// transaction that was never sent, and the sweep resolves that
    /// correctly on its own -- the inputs are still unspent, so it reads
    /// as `NeverLanded` and is simply sent.
    pub fn save_payout_attempt(&self, attempt: &PayoutAttempt) -> Result<()> {
        let mut bytes = Vec::new();
        ciborium::into_writer(attempt, &mut bytes)
            .map_err(|e| HubStoreError::Serialization(e.to_string()))?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(PAYOUT_ATTEMPTS_TABLE)?;
            table.insert(
                payout_attempt_key(attempt.task_id, &attempt.recipient).as_slice(),
                bytes.as_slice(),
            )?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Drops a payout the sweep has resolved. Deleting rather than
    /// flagging: the record's only purpose is to say what the hub is
    /// still waiting on, and a resolved one answers nothing. What
    /// actually happened is recorded on the task itself, which is
    /// durable and is what anyone reads afterwards.
    pub fn delete_payout_attempt(&self, task_id: uuid::Uuid, recipient: &PublicKey) -> Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(PAYOUT_ATTEMPTS_TABLE)?;
            table.remove(payout_attempt_key(task_id, recipient).as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Every unresolved payout, read at boot to refill the board. The
    /// key is not parsed back apart -- each record carries its own
    /// `task_id` and `recipient`, so the key is purely an index.
    pub fn load_all_payout_attempts(&self) -> Result<Vec<PayoutAttempt>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(PAYOUT_ATTEMPTS_TABLE)?;
        let mut attempts = Vec::new();
        for entry in table.iter()? {
            let (_, value) = entry?;
            let attempt: PayoutAttempt = ciborium::from_reader(value.value())
                .map_err(|e| HubStoreError::Serialization(e.to_string()))?;
            attempts.push(attempt);
        }
        Ok(attempts)
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

    /// The faucet-challenge table is additive on the same terms as the
    /// replay table below, and this is the test that keeps it honest:
    /// the money-guarding property is that a store predating the
    /// proof-of-work faucet opens, gains the table empty, and keeps
    /// every grant it already recorded -- so nobody who was already
    /// granted becomes eligible again on upgrade.
    #[test]
    fn a_store_from_before_the_faucet_challenge_table_still_opens() {
        let path = temp_db_path("older_build_faucet");
        let granted = PrivateKey::new_key().public_key();
        {
            let db = redb::Database::create(&path).unwrap();
            let write_txn = db.begin_write().unwrap();
            {
                write_txn.open_table(TASKS_TABLE).unwrap();
                write_txn.open_table(REPUTATION_TABLE).unwrap();
                let mut meta = write_txn.open_table(META_TABLE).unwrap();
                // Version 1, as a literal: that is what those builds
                // stamped, and `SCHEMA_VERSION` has since moved. Using
                // the constant would make this test quietly stop being
                // about an older build every time it is bumped.
                meta.insert(SCHEMA_VERSION_KEY, 1u32.to_be_bytes().as_slice()).unwrap();
                let mut grants = write_txn.open_table(FAUCET_GRANTS_TABLE).unwrap();
                grants.insert(granted.to_sec1_bytes().as_slice(), 1_700_000_000i64).unwrap();
            }
            write_txn.commit().unwrap();
        }

        let store = HubStore::open_or_create(&path).unwrap();
        assert!(
            store.load_all_faucet_challenges().unwrap().is_empty(),
            "the new table must arrive empty rather than failing to open"
        );
        assert_eq!(
            store.load_all_faucet_grants().unwrap(),
            vec![granted],
            "an upgrade must not re-open the faucet to a key already granted"
        );

        let key = PrivateKey::new_key();
        let challenge = crate::faucet_pow::Challenge::issue(
            &key.public_key(),
            crate::faucet_pow::target_for_expected_hashes(8),
            Utc::now(),
        );
        store.save_faucet_challenge(&challenge).unwrap();
        assert_eq!(store.load_all_faucet_challenges().unwrap().len(), 1);

        std::fs::remove_file(&path).ok();
    }

    /// Pruning takes an expired challenge and leaves a redeemed one
    /// alone until its own longer horizon, which is the asymmetry that
    /// keeps a spent solution unusable rather than merely absent.
    #[test]
    fn pruning_keeps_a_redeemed_challenge_longer_than_an_abandoned_one() {
        let path = temp_db_path("prune_faucet");
        let store = HubStore::open_or_create(&path).unwrap();
        let key = PrivateKey::new_key();
        let target = crate::faucet_pow::target_for_expected_hashes(8);
        let issued = Utc::now();

        let abandoned = crate::faucet_pow::Challenge::issue(&key.public_key(), target, issued);
        let mut redeemed = crate::faucet_pow::Challenge::issue(&key.public_key(), target, issued);
        redeemed.redeemed_at = Some(issued.timestamp());
        store.save_faucet_challenge(&abandoned).unwrap();
        store.save_faucet_challenge(&redeemed).unwrap();

        // Just past expiry: the abandoned one is garbage, the redeemed
        // one is still doing its job.
        let just_expired = abandoned.expires_at + 1;
        assert_eq!(store.prune_faucet_challenges(just_expired).unwrap(), 1);
        let left = store.load_all_faucet_challenges().unwrap();
        assert_eq!(left.len(), 1);
        assert!(left[0].is_redeemed());

        // Past the retention horizon it can go too: by now the challenge
        // would be refused as expired even if the record were missing.
        let past_retention =
            redeemed.expires_at + crate::faucet_pow::REDEEMED_RETENTION_SECONDS + 1;
        assert_eq!(store.prune_faucet_challenges(past_retention).unwrap(), 1);
        assert!(store.load_all_faucet_challenges().unwrap().is_empty());
        assert_eq!(store.prune_faucet_challenges(past_retention).unwrap(), 0, "idempotent");

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
            // tables it knew about, and its own schema stamp.
            let db = redb::Database::create(&path).unwrap();
            let write_txn = db.begin_write().unwrap();
            {
                write_txn.open_table(TASKS_TABLE).unwrap();
                write_txn.open_table(REPUTATION_TABLE).unwrap();
                write_txn.open_table(FAUCET_GRANTS_TABLE).unwrap();
                let mut meta = write_txn.open_table(META_TABLE).unwrap();
                // Version 1, as a literal: that is what those builds
                // stamped, and `SCHEMA_VERSION` has since moved. Using
                // the constant would make this test quietly stop being
                // about an older build every time it is bumped.
                meta.insert(SCHEMA_VERSION_KEY, 1u32.to_be_bytes().as_slice()).unwrap();
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

    /// The payout-attempts table is purely additive, so a store written
    /// before it existed must open on this build untouched -- no version
    /// bump, no migration, the table simply arrives empty. The same
    /// property `a_store_from_a_build_without_the_replay_table_still_opens`
    /// pins for the replay guard, and it matters more here: a hub that
    /// refused to open after an upgrade is a settlement outage, and the
    /// table it would be refusing over holds money in flight.
    #[test]
    fn a_store_from_a_build_without_the_payout_attempts_table_still_opens() {
        let path = temp_db_path("older_build_no_payouts");
        let agent = PrivateKey::new_key().public_key();
        {
            // Exactly what a build from before this change did: the
            // tables it knew about, and its own schema stamp.
            let db = redb::Database::create(&path).unwrap();
            let write_txn = db.begin_write().unwrap();
            {
                write_txn.open_table(TASKS_TABLE).unwrap();
                write_txn.open_table(REPUTATION_TABLE).unwrap();
                write_txn.open_table(FAUCET_GRANTS_TABLE).unwrap();
                write_txn.open_table(REPLAY_GUARD_TABLE).unwrap();
                let mut meta = write_txn.open_table(META_TABLE).unwrap();
                // Version 1, as a literal: that is what those builds
                // stamped, and `SCHEMA_VERSION` has since moved. Using
                // the constant would make this test quietly stop being
                // about an older build every time it is bumped.
                meta.insert(SCHEMA_VERSION_KEY, 1u32.to_be_bytes().as_slice()).unwrap();
                let mut grants = write_txn.open_table(FAUCET_GRANTS_TABLE).unwrap();
                grants.insert(agent.to_sec1_bytes().as_slice(), 1_700_000_000i64).unwrap();
            }
            write_txn.commit().unwrap();
        }

        let store = HubStore::open_or_create(&path).unwrap();
        assert!(
            store.load_all_payout_attempts().unwrap().is_empty(),
            "the new table must arrive empty rather than failing to open"
        );
        assert_eq!(
            store.load_all_faucet_grants().unwrap(),
            vec![agent],
            "and the store's existing contents must survive untouched"
        );

        // Usable immediately, not just openable -- and round-tripping,
        // since a payout the hub cannot read back at boot is a payout it
        // has silently stopped waiting for.
        let attempt = PayoutAttempt {
            task_id: uuid::Uuid::new_v4(),
            recipient: PrivateKey::new_key().public_key(),
            amount: 100,
            output_hash: btclib::sha256::Hash::hash_bytes(b"output"),
            spent_inputs: vec![btclib::sha256::Hash::hash_bytes(b"input")],
            source: PrivateKey::new_key().public_key(),
            submitted_at: Utc::now(),
            submissions: 1,
        };
        store.save_payout_attempt(&attempt).unwrap();
        assert_eq!(store.load_all_payout_attempts().unwrap(), vec![attempt.clone()]);

        // Re-saving is a replace, not a second row: a resubmission
        // supersedes the attempt it replaces, and two rows for one
        // payout would leave the sweep resolving a transaction already
        // ruled out.
        let resubmitted = PayoutAttempt { submissions: 2, ..attempt.clone() };
        store.save_payout_attempt(&resubmitted).unwrap();
        assert_eq!(store.load_all_payout_attempts().unwrap(), vec![resubmitted]);

        store.delete_payout_attempt(attempt.task_id, &attempt.recipient).unwrap();
        assert!(store.load_all_payout_attempts().unwrap().is_empty());

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

    /// Reads the stamp the way a hub of any version would, so a test can
    /// say what a *different* build would decide about this store.
    fn stored_schema_version(path: &std::path::Path) -> u32 {
        let db = redb::Database::create(path).unwrap();
        let read_txn = db.begin_read().unwrap();
        let meta = read_txn.open_table(META_TABLE).unwrap();
        let bytes: [u8; 4] = meta
            .get(SCHEMA_VERSION_KEY)
            .unwrap()
            .expect("every store this build has opened carries a stamp")
            .value()
            .try_into()
            .unwrap();
        u32::from_be_bytes(bytes)
    }

    /// The rollback hole, closed and stated.
    ///
    /// Additive tables never bumped `SCHEMA_VERSION`, so a store written
    /// by a current build still read as version 1 and an **older**
    /// binary opened it happily and ignored every table it did not know:
    /// in-flight `payout_attempts` invisible, so payouts re-sent or
    /// forgotten, and redeemed `faucet_challenges` replayable -- the
    /// exact hole that table was added to close (plan §6.5c).
    ///
    /// The fix has two halves and this checks both. An older store is
    /// still accepted, because refusing one would make every upgrade a
    /// manual migration for no benefit. And opening it restamps it, so
    /// the build that wrote it refuses it from then on. The restamp has
    /// to happen at open and not at first write: from the moment a
    /// current binary holds the store it may put a row somewhere the old
    /// one cannot see.
    #[test]
    fn opening_an_older_store_upgrades_its_stamp_so_the_old_build_refuses_it() {
        let path = temp_db_path("rollback_fence");
        let redeemed = PrivateKey::new_key().public_key();
        {
            // A version-1 store, as every build before this one wrote.
            let db = redb::Database::create(&path).unwrap();
            let write_txn = db.begin_write().unwrap();
            {
                write_txn.open_table(TASKS_TABLE).unwrap();
                write_txn.open_table(REPUTATION_TABLE).unwrap();
                let mut meta = write_txn.open_table(META_TABLE).unwrap();
                meta.insert(SCHEMA_VERSION_KEY, 1u32.to_be_bytes().as_slice()).unwrap();
                let mut grants = write_txn.open_table(FAUCET_GRANTS_TABLE).unwrap();
                grants.insert(redeemed.to_sec1_bytes().as_slice(), 1_700_000_000i64).unwrap();
            }
            write_txn.commit().unwrap();
        }
        assert_eq!(stored_schema_version(&path), 1);

        let store = HubStore::open_or_create(&path).unwrap();
        assert_eq!(
            store.load_all_faucet_grants().unwrap(),
            vec![redeemed],
            "an upgrade must not cost the store its contents"
        );
        drop(store);

        assert_eq!(
            stored_schema_version(&path),
            SCHEMA_VERSION,
            "opening it must have raised the fence, not merely tolerated the old stamp"
        );
        // What the old binary's own check does with that, exactly:
        // `found != expected` where expected is the 1 it was built with.
        assert_ne!(
            stored_schema_version(&path),
            1,
            "so a rolled-back build refuses this store instead of silently ignoring the tables \
             it cannot see"
        );

        // And the new build reopens its own store as a no-op, rather
        // than treating every start as an upgrade.
        let store = HubStore::open_or_create(&path).unwrap();
        drop(store);
        assert_eq!(stored_schema_version(&path), SCHEMA_VERSION);

        std::fs::remove_file(&path).ok();
    }

    /// A task and the deposit that funded it, built as a matched pair so
    /// the atomicity tests below all talk about the same confirmation.
    /// Mirrors what `TaskBoard::confirm_escrow` hands its caller: a task
    /// pointing at the escrow, and that escrow already marked
    /// `Consumed`.
    fn confirmed_escrow_pair() -> (Task, PendingDeposit) {
        let secret = crate::escrow_key::EscrowSecret::generate();
        let depositor = PrivateKey::new_key().public_key();
        let escrow_id = Uuid::new_v4();
        let deposit = PendingDeposit {
            id: escrow_id,
            depositor: depositor.clone(),
            deposit_pubkey: secret.derive(escrow_id).public_key(),
            deposit_private_key: None,
            required_amount: 1_000_000,
            purpose: crate::board::EscrowPurpose::FundHashMatchTask(crate::board::TaskIntent {
                description: "escrowed work".to_string(),
                bounty: 1_000_000,
                expected_output_hash: Hash::hash_bytes(b"answer"),
                min_reputation: 0,
                capabilities: Default::default(),
            }),
            status: crate::board::EscrowStatus::Consumed,
            created_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(30),
        };
        let task = Task {
            id: Uuid::new_v4(),
            description: "escrowed work".to_string(),
            bounty: 1_000_000,
            kind: crate::board::TaskKind::HashMatch {
                expected_output_hash: Hash::hash_bytes(b"answer"),
            },
            poster: depositor,
            status: TaskStatus::Open,
            claimant: None,
            claim_deadline: None,
            failed_attempts: 0,
            created_at: Utc::now(),
            min_reputation: 0,
            close_reason: None,
            escrow_id: Some(escrow_id),
            capabilities: Default::default(),
        };
        (task, deposit)
    }

    /// A resolution's task and every reputation record it dinged are one
    /// fact and must be one commit. Reputation is the input to a task's
    /// `min_reputation` term, so a penalty that does not land beside the
    /// task recording it lets a penalized agent keep claiming work a
    /// poster meant to exclude them from -- and the consensus path used
    /// to drop that batch's error entirely.
    #[test]
    fn a_resolution_commits_its_task_and_every_reputation_together_or_not_at_all() {
        let path = temp_db_path("task_and_reputation_batch");
        let store = HubStore::open_or_create(&path).unwrap();
        let (task, _) = confirmed_escrow_pair();
        let losers: Vec<(PublicKey, Reputation)> = (0..3)
            .map(|_| {
                (
                    PrivateKey::new_key().public_key(),
                    Reputation { completed: 0, failed: 1, total_earned: 0 },
                )
            })
            .collect();

        // Staged and then failed: the interval a crash would land in,
        // hit deterministically rather than by chance.
        let result = store.in_one_write_txn(|txn| {
            stage_record(txn, TASKS_TABLE, task.id.as_bytes().as_slice(), &task)?;
            for (pubkey, reputation) in &losers {
                stage_record(
                    txn,
                    REPUTATION_TABLE,
                    pubkey.to_sec1_bytes().as_slice(),
                    reputation,
                )?;
            }
            Err(HubStoreError::Serialization("injected mid-transaction failure".into()))
        });
        assert!(result.is_err());
        assert!(store.load_all_tasks().unwrap().is_empty());
        assert!(
            store.load_all_reputation().unwrap().is_empty(),
            "a resolution that did not commit must not have penalized anybody"
        );

        store.save_task_and_reputation_batch(&task, &losers).unwrap();
        assert_eq!(store.load_all_tasks().unwrap().len(), 1);
        let stored = store.load_all_reputation().unwrap();
        assert_eq!(stored.len(), 3, "every assignee's ding, not just the caller's");
        assert!(
            stored.iter().all(|(_, reputation)| reputation.failed == 1),
            "and each one as it was resolved, not defaulted"
        );

        // A resolution that penalized nobody still has a task to write,
        // so the empty case must not be a no-op the way
        // `save_reputation_batch`'s is.
        let (other_task, _) = confirmed_escrow_pair();
        store.save_task_and_reputation_batch(&other_task, &[]).unwrap();
        assert_eq!(store.load_all_tasks().unwrap().len(), 2);

        std::fs::remove_file(&path).ok();
    }

    /// A `Submitted` task and the attempt tracking its payout, as the
    /// store holds them the instant before a confirmation is recorded.
    fn submitted_payout_pair() -> (Task, PayoutAttempt) {
        let (task, _) = confirmed_escrow_pair();
        let recipient = PrivateKey::new_key().public_key();
        let task = Task {
            status: TaskStatus::Submitted,
            claimant: Some(recipient.clone()),
            ..task
        };
        let attempt = PayoutAttempt {
            task_id: task.id,
            recipient,
            amount: 1_000_000,
            output_hash: Hash::hash_bytes(b"payout output"),
            spent_inputs: vec![Hash::hash_bytes(b"escrow input")],
            source: PrivateKey::new_key().public_key(),
            submitted_at: Utc::now(),
            submissions: 1,
        };
        (task, attempt)
    }

    /// `a_failure_after_staging_both_records_commits_neither` for the
    /// payout side: the confirmation of a payout stages a task, a
    /// reputation record and the deletion of the attempt, and a failure
    /// after all three must leave the store exactly as it was.
    ///
    /// The deletion is the one that matters and the reason this is a
    /// transaction and not a batch. Committed alone -- which is what the
    /// old code did *first* -- it leaves a `Submitted` task with nothing
    /// tracking it, and nothing selects such a task ever again: the
    /// resolution pass reads `outstanding_payout_attempts` and the
    /// settlement pass will not take a `Submitted` task. The money has
    /// moved on chain and the hub has forgotten it.
    #[test]
    fn a_failure_partway_through_a_confirmed_payout_commits_nothing() {
        let path = temp_db_path("confirmed_payout_rollback");
        let store = HubStore::open_or_create(&path).unwrap();
        let (task, attempt) = submitted_payout_pair();
        let recipient = attempt.recipient.clone();
        store.save_task(&task).unwrap();
        store.save_payout_attempt(&attempt).unwrap();

        let paid = Task { status: TaskStatus::Paid, ..task.clone() };
        let reputation = Reputation { completed: 1, failed: 0, total_earned: attempt.amount };
        let result = store.in_one_write_txn(|txn| {
            stage_record(txn, TASKS_TABLE, paid.id.as_bytes().as_slice(), &paid)?;
            stage_record(
                txn,
                REPUTATION_TABLE,
                recipient.to_sec1_bytes().as_slice(),
                &reputation,
            )?;
            stage_delete(
                txn,
                PAYOUT_ATTEMPTS_TABLE,
                payout_attempt_key(paid.id, &recipient).as_slice(),
            )?;
            // Stands in for the process dying here, after every write is
            // staged and before any of it is committed.
            Err(HubStoreError::Serialization("injected mid-transaction failure".into()))
        });
        assert!(result.is_err(), "the injected failure must surface to the caller");

        assert_eq!(
            store.load_all_payout_attempts().unwrap(),
            vec![attempt],
            "the attempt must still be there: a deletion that outlives the task save is a payout \
             the hub has stopped waiting for and will never look at again"
        );
        assert_eq!(
            store.load_all_tasks().unwrap()[0].status,
            TaskStatus::Submitted,
            "and the task must still read Submitted, agreeing with that attempt"
        );
        assert!(
            store.load_all_reputation().unwrap().is_empty(),
            "no record may survive a transaction that did not commit"
        );

        std::fs::remove_file(&path).ok();
    }

    /// The other half, on success: no reader ever sees the task move
    /// without the attempt going with it.
    ///
    /// Proved against a read snapshot opened before the write, the same
    /// way `a_task_and_its_deposit_are_never_visible_apart` does it --
    /// redb's read transactions are point-in-time, so a snapshot holding
    /// one and not the other is a restarting hub observing the pair
    /// apart.
    #[test]
    fn a_paid_task_and_its_resolved_attempt_are_never_visible_apart() {
        let path = temp_db_path("confirmed_payout_commit");
        let store = HubStore::open_or_create(&path).unwrap();
        let (task, attempt) = submitted_payout_pair();
        let recipient = attempt.recipient.clone();
        store.save_task(&task).unwrap();
        store.save_payout_attempt(&attempt).unwrap();

        let before = store.db.begin_read().unwrap();
        let paid = Task { status: TaskStatus::Paid, ..task.clone() };
        let reputation = Reputation { completed: 1, failed: 0, total_earned: attempt.amount };
        store.save_confirmed_payout(&paid, &recipient, &reputation, None).unwrap();

        // The pre-write snapshot must see the old pair intact, never the
        // deletion on its own.
        let tasks_then = before.open_table(TASKS_TABLE).unwrap();
        let attempts_then = before.open_table(PAYOUT_ATTEMPTS_TABLE).unwrap();
        let stored: Task =
            ciborium::from_reader(tasks_then.get(task.id.as_bytes().as_slice()).unwrap().unwrap().value()).unwrap();
        assert_eq!(stored.status, TaskStatus::Submitted);
        assert!(attempts_then
            .get(payout_attempt_key(task.id, &recipient).as_slice())
            .unwrap()
            .is_some());

        // And a snapshot taken after must see the whole change.
        assert_eq!(store.load_all_tasks().unwrap()[0].status, TaskStatus::Paid);
        assert!(store.load_all_payout_attempts().unwrap().is_empty());
        let stored_reputation = store.load_all_reputation().unwrap();
        assert_eq!(stored_reputation.len(), 1);
        assert_eq!(stored_reputation[0].0, recipient);
        assert_eq!(stored_reputation[0].1.completed, 1);
        assert_eq!(stored_reputation[0].1.total_earned, attempt.amount);

        std::fs::remove_file(&path).ok();
    }

    /// A "compute" task credits an exchange account too, and that credit
    /// is spendable and tradeable the moment it lands -- so it belongs
    /// in the same commit as the rest, not in a fifth one whose error
    /// was logged and dropped.
    #[test]
    fn a_compute_payout_commits_its_exchange_credit_with_everything_else() {
        let path = temp_db_path("confirmed_compute_payout");
        let store = HubStore::open_or_create(&path).unwrap();
        let (task, attempt) = submitted_payout_pair();
        let recipient = attempt.recipient.clone();
        store.save_payout_attempt(&attempt).unwrap();

        let paid = Task { status: TaskStatus::Paid, ..task };
        let reputation = Reputation { completed: 1, failed: 0, total_earned: attempt.amount };
        let account = ExchangeAccount { compute_balance: attempt.amount, ..Default::default() };
        store
            .save_confirmed_payout(&paid, &recipient, &reputation, Some(&account))
            .unwrap();

        let accounts = store.load_all_exchange_accounts().unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].0, recipient);
        assert_eq!(
            accounts[0].1.compute_balance, attempt.amount,
            "the compute credit must be on disk, not only in the account the handler built"
        );
        assert!(store.load_all_payout_attempts().unwrap().is_empty());
    }

    /// The bug in plan §6.5b, reduced to the store: staging both records
    /// and *then* failing must leave neither behind.
    ///
    /// This is the test the fix rests on, and the reason the fix is a
    /// transaction rather than a reordering. The failure is injected
    /// here rather than by killing a hub because the interval a real
    /// `SIGKILL` has to land in is one step wide -- `harness drill
    /// escrow-restart` reproduced the bug in three runs of six and
    /// reports *inconclusive* rather than safe when it finds nothing,
    /// precisely because sampling cannot show an interval is gone. An
    /// injected failure after both writes are staged is that interval,
    /// hit deterministically and on every run.
    #[test]
    fn a_failure_after_staging_both_records_commits_neither() {
        let path = temp_db_path("atomic_rollback");
        let store = HubStore::open_or_create(&path).unwrap();
        let (task, deposit) = confirmed_escrow_pair();

        let result = store.in_one_write_txn(|txn| {
            stage_record(txn, TASKS_TABLE, task.id.as_bytes().as_slice(), &task)?;
            stage_record(
                txn,
                PENDING_DEPOSITS_TABLE,
                deposit.id.as_bytes().as_slice(),
                &deposit,
            )?;
            // Stands in for the process dying here. Everything above is
            // staged and none of it is committed.
            Err(HubStoreError::Serialization("injected mid-transaction failure".into()))
        });
        assert!(result.is_err(), "the injected failure must surface to the caller");

        assert!(
            store.load_all_tasks().unwrap().is_empty(),
            "the task was staged before the failure and must not have survived it -- a task on \
             disk beside a deposit that never reached Consumed is exactly the state that let one \
             escrow fund two tasks"
        );
        assert!(
            store.load_all_pending_deposits().unwrap().is_empty(),
            "neither record may survive a transaction that did not commit"
        );

        std::fs::remove_file(&path).ok();
    }

    /// The other half of the same property: on success both records
    /// become visible at the same instant, not one and then the other.
    ///
    /// Proved against a read snapshot opened before the write. redb's
    /// read transactions are point-in-time, so a snapshot that sees the
    /// task but not the deposit would be a reader observing the two
    /// apart -- which, held by a restarting hub instead of a test, is
    /// the bug.
    #[test]
    fn a_task_and_its_deposit_are_never_visible_apart() {
        let path = temp_db_path("atomic_commit");
        let store = HubStore::open_or_create(&path).unwrap();
        let (task, deposit) = confirmed_escrow_pair();

        let before = store.db.begin_read().unwrap();
        store.save_task_and_deposit(&task, &deposit).unwrap();

        // The pre-write snapshot must see neither, never just the task.
        let tasks_then = before.open_table(TASKS_TABLE).unwrap();
        let deposits_then = before.open_table(PENDING_DEPOSITS_TABLE).unwrap();
        assert!(tasks_then.get(task.id.as_bytes().as_slice()).unwrap().is_none());
        assert!(deposits_then.get(deposit.id.as_bytes().as_slice()).unwrap().is_none());

        // And a snapshot taken after must see both.
        let tasks = store.load_all_tasks().unwrap();
        let deposits = store.load_all_pending_deposits().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, task.id);
        assert_eq!(tasks[0].escrow_id, Some(deposit.id));
        assert_eq!(deposits.len(), 1);
        assert_eq!(deposits[0].id, deposit.id);
        assert_eq!(
            deposits[0].status,
            crate::board::EscrowStatus::Consumed,
            "the deposit must come back Consumed, or a restarted hub would offer it for \
             confirmation a second time"
        );

        std::fs::remove_file(&path).ok();
    }

    /// The premise of plan §6.5b, made executable: written as two
    /// commits, the task and its deposit *are* observable apart.
    ///
    /// This passes before and after the fix -- it characterises
    /// `save_task` followed by `save_pending_deposit`, which both still
    /// exist and are still correct on their own. It is here because it
    /// is the one place the suite states the actual mechanism of the
    /// bug: the snapshot below is what a hub that died between the two
    /// commits reads back on restart, and from there the deposit is
    /// still Reserved and confirmable a second time.
    ///
    /// Note what it also shows about testing the fix. No test can catch
    /// `save_task_and_deposit` being rewritten as these two calls,
    /// because two commits differ from one only in the existence of a
    /// window nothing can be scheduled inside on demand -- which is the
    /// original problem restated. What rules that rewrite out is the
    /// shape of the code and the comment on the method, not this suite.
    #[test]
    fn two_separate_commits_leave_a_window_where_the_task_exists_alone() {
        let path = temp_db_path("two_commit_window");
        let store = HubStore::open_or_create(&path).unwrap();
        let (task, deposit) = confirmed_escrow_pair();

        store.save_task(&task).unwrap();
        // Stands exactly where the SIGKILL landed: after the task's
        // commit, before the deposit's.
        let crashed_here = store.db.begin_read().unwrap();
        store.save_pending_deposit(&deposit).unwrap();

        let tasks = crashed_here.open_table(TASKS_TABLE).unwrap();
        let deposits = crashed_here.open_table(PENDING_DEPOSITS_TABLE).unwrap();
        assert!(
            tasks.get(task.id.as_bytes().as_slice()).unwrap().is_some(),
            "the task committed first, so a reader in the window sees it"
        );
        assert!(
            deposits.get(deposit.id.as_bytes().as_slice()).unwrap().is_none(),
            "and does not see the deposit that paid for it -- on disk that is a funded task \
             beside an escrow still reading Reserved, which is the duplicate waiting to happen"
        );

        std::fs::remove_file(&path).ok();
    }

    /// `confirm_exchange_deposit`'s pair, which has the same shape and
    /// was never drilled. A duplicate here credits a ledger balance that
    /// is spendable and tradeable immediately.
    #[test]
    fn an_exchange_credit_and_its_deposit_are_never_visible_apart() {
        let path = temp_db_path("atomic_exchange_commit");
        let store = HubStore::open_or_create(&path).unwrap();
        let (_, mut deposit) = confirmed_escrow_pair();
        deposit.purpose = crate::board::EscrowPurpose::FundExchangeAccount;
        let depositor = deposit.depositor.clone();
        let account = ExchangeAccount {
            base_balance: 999_000,
            ..Default::default()
        };

        let before = store.db.begin_read().unwrap();
        store
            .save_exchange_account_and_deposit(&depositor, &account, &deposit)
            .unwrap();

        let accounts_then = before.open_table(EXCHANGE_ACCOUNTS_TABLE).unwrap();
        let deposits_then = before.open_table(PENDING_DEPOSITS_TABLE).unwrap();
        assert!(accounts_then
            .get(depositor.to_sec1_bytes().as_slice())
            .unwrap()
            .is_none());
        assert!(deposits_then.get(deposit.id.as_bytes().as_slice()).unwrap().is_none());

        let accounts = store.load_all_exchange_accounts().unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].0, depositor);
        assert_eq!(accounts[0].1.base_balance, 999_000);
        let deposits = store.load_all_pending_deposits().unwrap();
        assert_eq!(deposits.len(), 1);
        assert_eq!(deposits[0].status, crate::board::EscrowStatus::Consumed);

        std::fs::remove_file(&path).ok();
    }

    /// A cancelled order and the balance its cancellation released, in
    /// the exchange's version of the same claim.
    ///
    /// This is the pair whose split was exploitable without a crash:
    /// a reader that saw the released lock but not the cancellation
    /// would be reading an order still matchable on the book against an
    /// account that has already had the money back.
    #[test]
    fn a_cancelled_order_and_its_released_lock_are_never_visible_apart() {
        let path = temp_db_path("atomic_cancel");
        let store = HubStore::open_or_create(&path).unwrap();
        let (owner, order, account) = cancelled_order_pair();

        let before = store.db.begin_read().unwrap();
        store.save_order_and_account(&order, &owner, &account).unwrap();

        let orders_then = before.open_table(ORDERS_TABLE).unwrap();
        let accounts_then = before.open_table(EXCHANGE_ACCOUNTS_TABLE).unwrap();
        assert!(orders_then.get(order.id.as_bytes().as_slice()).unwrap().is_none());
        assert!(accounts_then.get(owner.to_sec1_bytes().as_slice()).unwrap().is_none());

        let orders = store.load_all_orders().unwrap();
        let accounts = store.load_all_exchange_accounts().unwrap();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].status, crate::board::OrderStatus::Cancelled);
        assert_eq!(accounts.len(), 1);
        assert_eq!(
            accounts[0].1.locked_base, 0,
            "the lock is released in the same commit that cancels the order, or a restarted \
             hub reads one without the other"
        );

        std::fs::remove_file(&path).ok();
    }

    /// The exchange's twin of
    /// `two_separate_commits_leave_a_window_where_the_task_exists_alone`,
    /// and the reason this pair was ranked above the fill itself.
    ///
    /// It characterises the *old* handler: `save_exchange_account`
    /// followed by `save_order`. Both still exist and are still correct
    /// alone, so this passes before and after the fix. What it makes
    /// executable is the profitable window -- a reader here sees an
    /// account whose lock is gone beside an order still reading `Open`,
    /// which is an order the book will still match using money its owner
    /// is already free to withdraw.
    #[test]
    fn two_separate_commits_leave_a_window_where_the_lock_is_gone_but_the_order_is_open() {
        let path = temp_db_path("cancel_two_commit_window");
        let store = HubStore::open_or_create(&path).unwrap();
        let (owner, cancelled, released) = cancelled_order_pair();

        // The order as it still stands on disk from when it was placed:
        // Open, with its balance locked.
        let mut resting = cancelled.clone();
        resting.status = crate::board::OrderStatus::Open;
        let mut locked = released.clone();
        locked.locked_base = 500;
        store.save_order(&resting).unwrap();
        store.save_exchange_account(&owner, &locked).unwrap();

        // Now cancel it the way the handler used to: account first.
        store.save_exchange_account(&owner, &released).unwrap();
        let crashed_here = store.db.begin_read().unwrap();
        store.save_order(&cancelled).unwrap();

        let orders = crashed_here.open_table(ORDERS_TABLE).unwrap();
        let accounts = crashed_here.open_table(EXCHANGE_ACCOUNTS_TABLE).unwrap();
        let order_bytes = orders.get(cancelled.id.as_bytes().as_slice()).unwrap().unwrap();
        let seen: Order = ciborium::from_reader(order_bytes.value()).unwrap();
        let account_bytes = accounts.get(owner.to_sec1_bytes().as_slice()).unwrap().unwrap();
        let seen_account: ExchangeAccount = ciborium::from_reader(account_bytes.value()).unwrap();

        assert_eq!(
            seen.status,
            crate::board::OrderStatus::Open,
            "a reader in the window still sees a matchable order"
        );
        assert_eq!(
            seen_account.locked_base, 0,
            "beside an account that has already had the locked money back -- the owner can \
             withdraw it while the order remains on the book"
        );

        std::fs::remove_file(&path).ok();
    }

    /// An order cancelled down to a released lock, and the account it
    /// was released into. `locked_base` is 0 because the cancellation
    /// has already given the money back; `base_balance` is what the
    /// owner is now free to spend.
    fn cancelled_order_pair() -> (PublicKey, Order, ExchangeAccount) {
        let owner = PrivateKey::new_key().public_key();
        let order = Order {
            id: Uuid::new_v4(),
            owner: owner.clone(),
            side: crate::board::Side::Buy,
            price: 10,
            quantity: 50,
            filled: 0,
            status: crate::board::OrderStatus::Cancelled,
            created_at: Utc::now(),
        };
        let account = ExchangeAccount {
            base_balance: 1_000,
            locked_base: 0,
            compute_balance: 0,
            locked_compute: 0,
        };
        (owner, order, account)
    }
}

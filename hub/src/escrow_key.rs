//! Deterministic derivation of one-time escrow deposit keys from a single
//! master secret, so that the hub's store never holds a private key.
//!
//! Every escrow (a task's bounty, a dispute bond, an exchange deposit) is
//! funded by paying a fresh single-use address the hub controls -- see
//! `crate::board::PendingDeposit` for why a one-time address is the only
//! way the hub can attribute an arbitrary agent's payment to a specific
//! intent. Those keypairs used to be generated randomly and written into
//! the `pending_deposits` table to be recoverable, which made the store
//! itself custody: anyone who could read `hub.redb` -- a stolen backup, a
//! copied volume, a misconfigured file share -- owned every escrow in
//! flight.
//!
//! Deriving each key from `HKDF-SHA256(escrow secret, deposit id)` removes
//! that. The store holds only ids and public keys, so a leaked *database*
//! is no longer a leaked treasury; the secret lives in one file that can
//! be permissioned, backed up, and rotated on its own terms. This narrows
//! where secrets live rather than eliminating them -- an attacker who
//! reads the secret file still derives every escrow key, exactly as one
//! who read the old table did.
//!
//! Two operational consequences follow, and both are real:
//!
//! - **The secret must be backed up.** Losing it strands every escrow
//!   whose address was already handed out, since nothing else can produce
//!   those keys. The old design at least kept them beside the data.
//! - **Rotation is not retroactive.** A new secret derives new addresses;
//!   deposits reserved under the old one still need the old secret to
//!   sweep. Rotating means keeping the previous secret until every
//!   deposit reserved under it has settled or expired.
//!
//! This module deliberately does not zeroize its buffers. `PrivateKey`
//! (and every key already resident in `AppState`) does not either, so
//! doing it here alone would suggest a memory-disclosure guarantee the
//! process as a whole does not make. The threat this addresses is what
//! sits on disk.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use btclib::crypto::PrivateKey;
use hkdf::Hkdf;
use rand::RngCore;
use sha2::Sha256;
use uuid::Uuid;

/// Length of the master secret, and of each derived scalar. 32 bytes is
/// both HKDF-SHA256's natural output size and secp256k1's scalar width.
const SECRET_LEN: usize = 32;

/// HKDF salt, fixed rather than random: the secret is already full-entropy
/// key material, so the salt is serving domain separation (this use of
/// this secret, versus any other use it might later be put to) rather than
/// entropy extraction.
const HKDF_SALT: &[u8] = b"itx-hub-escrow-secret-v1";

/// Prefix on every `info` string, so a derived escrow key can never
/// collide with some future key derived from the same secret for a
/// different purpose. The `v1` in both constants is what a future change
/// to this construction would bump, since changing either one silently
/// changes every address the hub would derive.
const HKDF_INFO_PREFIX: &[u8] = b"itx-hub-escrow-deposit-v1";

/// The hub's master escrow secret: the one input, besides a deposit's own
/// id, needed to reproduce any escrow keypair the hub has ever handed out.
pub struct EscrowSecret([u8; SECRET_LEN]);

/// Redacted deliberately. `AppState` holds one of these, and anything that
/// prints application state -- a panic, a debug log -- must not put the
/// key material that controls every escrow into a log file.
impl std::fmt::Debug for EscrowSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EscrowSecret(<redacted>)")
    }
}

impl EscrowSecret {
    pub fn generate() -> Self {
        let mut bytes = [0u8; SECRET_LEN];
        rand::thread_rng().fill_bytes(&mut bytes);
        EscrowSecret(bytes)
    }

    /// Reads an existing secret at `path`. Missing files are deliberately
    /// errors: only the startup policy in `main` may decide that this is a
    /// first boot where explicit key generation is safe.
    pub fn load<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let path = path.as_ref();
        let bytes = fs::read(path)?;
        let len = bytes.len();
        let bytes: [u8; SECRET_LEN] = bytes.try_into().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "escrow secret at {} must be exactly {SECRET_LEN} bytes, found {len} \
                     -- refusing to start rather than derive escrow addresses from a \
                     truncated or unrelated file",
                    path.display()
                ),
            )
        })?;
        Ok(EscrowSecret(bytes))
    }

    /// Generates a secret and atomically creates its file. Called only after
    /// `main` has established that `--generate-keys` was explicit and no hub
    /// store exists.
    pub fn generate_and_save<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let secret = Self::generate();
        secret.write_new_file(path.as_ref())?;
        Ok(secret)
    }

    fn write_new_file(&self, path: &Path) -> io::Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        file.write_all(&self.0)?;
        // fsync before the caller goes on to hand out an address derived
        // from this secret: a secret that never reached disk derives
        // addresses nothing can sweep after a crash.
        file.sync_all()
    }

    /// The keypair controlling `deposit_id`'s escrow address.
    ///
    /// Deterministic, so the same id always yields the same address, and
    /// unguessable without the secret. Deposit ids are v4 UUIDs, so they
    /// are not attacker-chosen; even if they were, HKDF's output reveals
    /// nothing about the secret.
    pub fn derive(&self, deposit_id: Uuid) -> PrivateKey {
        let hkdf = Hkdf::<Sha256>::new(Some(HKDF_SALT), &self.0);
        // A uniformly random 32 bytes is a valid secp256k1 scalar with
        // overwhelming probability, but not certainly: zero and anything
        // at or above the curve order are rejected by `from_fixed_bytes`.
        // Each retry re-derives with a bumped counter rather than mangling
        // the bytes, so the result stays a clean HKDF output. The odds of
        // needing even one retry are about 2^-128.
        for counter in 0..=u8::MAX {
            let mut info = Vec::with_capacity(HKDF_INFO_PREFIX.len() + 17);
            info.extend_from_slice(HKDF_INFO_PREFIX);
            info.extend_from_slice(deposit_id.as_bytes());
            info.push(counter);

            let mut okm = [0u8; SECRET_LEN];
            hkdf.expand(&info, &mut okm)
                .expect("32 bytes is far below HKDF-SHA256's 8160-byte output limit");
            if let Ok(key) = PrivateKey::from_fixed_bytes(&okm) {
                return key;
            }
        }
        unreachable!(
            "256 consecutive HKDF outputs all landing outside the secp256k1 scalar range is \
             impossible short of a broken hash"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "itx_escrow_secret_test_{name}_{}_{n}.bin",
            std::process::id()
        ))
    }

    #[test]
    fn derives_the_same_address_for_the_same_id_every_time() {
        let secret = EscrowSecret::generate();
        let id = Uuid::new_v4();
        assert_eq!(
            secret.derive(id).public_key(),
            secret.derive(id).public_key(),
            "derivation must be reproducible, or a deposit's funds could not be swept twice"
        );
    }

    #[test]
    fn derives_a_different_address_for_every_id() {
        let secret = EscrowSecret::generate();
        let a = secret.derive(Uuid::new_v4()).public_key();
        let b = secret.derive(Uuid::new_v4()).public_key();
        assert_ne!(a, b, "two deposits sharing an address would comingle their funds");
    }

    #[test]
    fn a_different_secret_derives_a_different_address_for_the_same_id() {
        let id = Uuid::new_v4();
        let a = EscrowSecret::generate().derive(id).public_key();
        let b = EscrowSecret::generate().derive(id).public_key();
        assert_ne!(a, b, "the id alone must not determine the address");
    }

    #[test]
    fn generate_and_save_then_load_reads_back_the_same_secret() {
        let path = temp_path("roundtrip");
        let created = EscrowSecret::generate_and_save(&path).unwrap();
        let reloaded = EscrowSecret::load(&path).unwrap();

        let id = Uuid::new_v4();
        assert_eq!(
            created.derive(id).public_key(),
            reloaded.derive(id).public_key(),
            "a restarted hub must derive the same addresses it handed out before"
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_does_not_create_a_missing_secret() {
        let path = temp_path("missing");
        let err = EscrowSecret::load(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(!path.exists());
    }

    #[test]
    fn refuses_a_secret_file_of_the_wrong_length() {
        let path = temp_path("truncated");
        std::fs::write(&path, b"too short").unwrap();

        let err = EscrowSecret::load(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);

        std::fs::remove_file(&path).ok();
    }

    #[cfg(unix)]
    #[test]
    fn creates_the_secret_file_readable_only_by_its_owner() {
        use std::os::unix::fs::PermissionsExt;

        let path = temp_path("permissions");
        EscrowSecret::generate_and_save(&path).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "a fresh escrow secret must never be group- or world-readable"
        );

        std::fs::remove_file(&path).ok();
    }
}

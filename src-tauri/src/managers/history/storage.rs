//! Encryption at rest for the dictation history database.
//!
//! Opening the database never touches the OS credential store, so the first
//! window paint cannot wait behind a keychain prompt. Startup order:
//!
//! 1. [`HistoryStorage::at_startup`] reads the file header and picks a state. A
//!    plaintext file serves reads and writes immediately; a missing or already
//!    encrypted file starts locked.
//! 2. The app builds and shows its window.
//! 3. [`HistoryStorage::unlock`] resolves the key off the critical path, applies
//!    the one time plaintext to encrypted migration, and publishes the state
//!    every later connection uses.
//!
//! A key or migration failure keeps a plaintext database readable and writable
//! and records the reason, so history never disappears because a keychain is
//! locked.

use crate::secrets::SecretManager;
use anyhow::{anyhow, Context, Result};
use log::{info, warn};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;
use zeroize::Zeroizing;

/// The 16 bytes every plaintext SQLite file starts with. An encrypted database
/// starts with its random salt instead, so the header is the one honest way to
/// tell the two apart without a key.
const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";

/// Storage-owned metadata table. It is created by this module rather than by the
/// history migrations because it describes the file, not the app's data.
const META_TABLE: &str = "history_storage_meta";
const ENCRYPTED_AT_KEY: &str = "encrypted_at_utc_ms";

/// How long a connection waits for a key resolution that is already running
/// before reporting a locked database. Only a [`HistoryStorageReason::Resolving`]
/// state waits: a resolution nobody started is reported at once, because
/// waiting for a step that is not running is how a headless corpus read used to
/// spend five seconds and then fail.
const UNLOCK_WAIT: Duration = Duration::from_secs(5);

/// How long the query connection waits out a locked file before reporting the
/// error. SQLite's default is to fail instantly, which turns the moments the
/// unlock path's own connections or a write-ahead-log checkpoint hold the file
/// into a failed history read.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Why the history database is not encrypted at rest right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HistoryStorageReason {
    /// Nothing has begun resolving the key. This is the startup state, and in
    /// a process with a startup unlock it becomes [`Self::Resolving`] within
    /// moments of the window appearing. In a process without one — the headless
    /// corpus verbs — it is the read's own signal that nothing else will
    /// resolve the key and it has to.
    Unresolved,
    /// A resolution is running right now and owns the credential-store round
    /// trip. A read waits for it rather than starting a second one.
    Resolving,
    /// The OS credential store returned no usable key.
    KeyUnavailable,
    /// This build cannot open a SQLCipher database.
    EncryptionUnavailable,
    /// The one time plaintext to encrypted migration failed. The plaintext
    /// database is intact and still serving.
    MigrationFailed,
    /// The file is encrypted and the stored key does not open it.
    KeyRejected,
}

impl HistoryStorageReason {
    /// The wire word for [`HistoryStorageStatus::reason`].
    ///
    /// Both pre-key states report `unlocking`, because that is the one the
    /// settings row reads (`PrivacyHistoryStorage.tsx`) and it is translated in
    /// 28 locales. Which of the two a process is in is an internal scheduling
    /// detail, not something a reader can act on differently.
    fn as_str(self) -> &'static str {
        match self {
            Self::Unresolved | Self::Resolving => "unlocking",
            Self::KeyUnavailable => "key_unavailable",
            Self::EncryptionUnavailable => "encryption_unavailable",
            Self::MigrationFailed => "migration_failed",
            Self::KeyRejected => "key_rejected",
        }
    }
}

/// Whether dictation history is encrypted at rest, and why not when it is not.
/// `migrated_at` is the moment encryption was established for this file.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct HistoryStorageStatus {
    pub encrypted: bool,
    pub migrated_at: Option<i64>,
    pub reason: Option<String>,
}

/// The database key. It is a copy of the credential-store key, cleared on drop,
/// and never formatted or logged.
type DatabaseKey = Zeroizing<[u8; 32]>;

/// The published storage state, and with it the one connection every history
/// query runs on.
///
/// The connection lives inside the state that authorizes it, so replacing the
/// state drops the connection that belonged to it. Locking, relocking, and the
/// plaintext to encrypted migration cannot leave a connection open against a
/// file the published key no longer describes, because there is no way to
/// publish a new state and keep the old connection.
enum State {
    /// The file is plaintext and serves reads and writes without a key.
    Plaintext {
        reason: HistoryStorageReason,
        connection: Option<Connection>,
    },
    /// The file is encrypted and serves reads and writes on a connection keyed
    /// with `key`, opened on first use.
    Encrypted {
        key: DatabaseKey,
        encrypted_at_utc_ms: Option<i64>,
        connection: Option<Connection>,
    },
    /// The file is encrypted, or has yet to be created, and no key is available.
    Locked(HistoryStorageReason),
}

/// What the file on disk is, before any key is available.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileState {
    Absent,
    Plaintext,
    Encrypted,
}

pub(crate) struct HistoryStorage {
    path: PathBuf,
    state: Mutex<State>,
    unlocked: Condvar,
}

impl HistoryStorage {
    /// Pick the startup state from the file on disk. Deliberately does no
    /// credential-store work, so it cannot block startup.
    pub(crate) fn at_startup(path: PathBuf) -> Self {
        let state = match file_state(&path) {
            // A database that does not exist yet is created encrypted by the
            // unlock step, so a fresh install never writes plaintext history.
            FileState::Absent | FileState::Encrypted => {
                State::Locked(HistoryStorageReason::Unresolved)
            }
            FileState::Plaintext => State::Plaintext {
                reason: HistoryStorageReason::Unresolved,
                connection: None,
            },
        };
        Self {
            path,
            state: Mutex::new(state),
            unlocked: Condvar::new(),
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// True when a connection can be opened right now, without waiting. False
    /// only while the file is encrypted and no key has been resolved.
    pub(crate) fn is_ready(&self) -> bool {
        !matches!(&*self.lock_state(), State::Locked(_))
    }

    /// True when a read has to resolve the key itself, because nothing else
    /// has begun to.
    ///
    /// [`Self::at_startup`] resolves nothing, and in the app a startup unlock
    /// follows within moments. The headless corpus verbs have no startup step,
    /// so there the read is the only thing left that can resolve the key. A
    /// plaintext file is deliberately excluded: it already serves reads and
    /// writes, so a read needs no key and must not trigger the one time
    /// encryption migration on its way to a `SELECT`.
    pub(crate) fn needs_resolution(&self) -> bool {
        matches!(
            &*self.lock_state(),
            State::Locked(HistoryStorageReason::Unresolved)
        )
    }

    /// Run one action on the single history connection.
    ///
    /// The connection is opened on first use and reused afterwards, so
    /// `PRAGMA key` and the schema read that proves it cost once per published
    /// state instead of once per query. Blocks while another caller is using
    /// the connection, for the duration of the one time migration, and for at
    /// most [`UNLOCK_WAIT`] while the startup unlock is still running.
    ///
    /// `action` is the only thing that ever sees the connection, so it cannot
    /// be held across an `emit`, an `.await`, or a second history query. One
    /// connection behind one lock is not reentrant, and a caller that nested
    /// two of them would deadlock rather than open a second connection.
    pub(crate) fn with_connection<T>(
        &self,
        action: impl FnOnce(&mut Connection) -> Result<T>,
    ) -> Result<T> {
        let mut state = self.wait_for_unlock();
        let (slot, key) = match &mut *state {
            State::Plaintext { connection, .. } => (connection, None),
            State::Encrypted {
                key, connection, ..
            } => (connection, Some(&*key)),
            State::Locked(reason) => {
                return Err(anyhow!("history database is locked ({})", reason.as_str()))
            }
        };
        // Taking the connection out ends the slot's borrow, so the open path
        // needs no second lookup into a slot it just filled.
        let mut connection = match slot.take() {
            Some(connection) => connection,
            None => {
                let connection = match key {
                    Some(key) => open_keyed(&self.path, key)?,
                    None => Connection::open(&self.path)?,
                };
                connection.busy_timeout(BUSY_TIMEOUT)?;
                connection
            }
        };
        let outcome = action(&mut connection);
        // Kept even when the action failed: a statement error leaves the
        // connection itself usable, and reopening on every constraint
        // violation would reintroduce the per-query open this replaces.
        *slot = Some(connection);
        outcome
    }

    pub(crate) fn status(&self) -> HistoryStorageStatus {
        status_of(&self.lock_state())
    }

    /// Take the state lock, waiting out a resolution that is already running.
    /// Without this, a query issued in the first milliseconds after launch
    /// would report a locked database.
    ///
    /// Only [`HistoryStorageReason::Resolving`] waits. An `Unresolved` state
    /// means no resolution is running, so there is nothing to wait for and the
    /// caller is told at once.
    fn wait_for_unlock(&self) -> MutexGuard<'_, State> {
        let state = self.lock_state();
        if !matches!(&*state, State::Locked(HistoryStorageReason::Resolving)) {
            return state;
        }
        let (state, _elapsed) = self
            .unlocked
            .wait_timeout_while(state, UNLOCK_WAIT, |state| {
                matches!(state, State::Locked(HistoryStorageReason::Resolving))
            })
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
    }

    /// Take ownership of the one key resolution, or report the state that
    /// makes this call redundant.
    ///
    /// The claim happens under the state lock, so two callers arriving together
    /// cannot both go to the credential store: exactly one moves the state to
    /// [`HistoryStorageReason::Resolving`] and the other is handed the current
    /// status. That is what makes a check followed by an unlock safe on a
    /// multi-threaded runtime — the check is the claim.
    fn claim_resolution(&self) -> Option<HistoryStorageStatus> {
        let mut state = self.lock_state();
        let unresolved = matches!(
            &*state,
            State::Locked(HistoryStorageReason::Unresolved)
                | State::Plaintext {
                    reason: HistoryStorageReason::Unresolved,
                    ..
                }
        );
        if !unresolved {
            return Some(status_of(&state));
        }
        if let State::Locked(reason) | State::Plaintext { reason, .. } = &mut *state {
            *reason = HistoryStorageReason::Resolving;
        }
        None
    }

    /// Resolve the key and bring the file to its encrypted state. The
    /// credential store is awaited without the state lock held, so no
    /// connection ever waits on a keychain prompt.
    ///
    /// Redundant calls return the current status without touching the
    /// credential store, so the app's startup unlock and a read that arrives
    /// beside it cost one round trip between them rather than two.
    pub(crate) async fn unlock(
        &self,
        secrets: &SecretManager,
        now_utc_ms: i64,
    ) -> HistoryStorageStatus {
        if let Some(status) = self.claim_resolution() {
            return status;
        }
        let key = match secrets.history_storage_key().await {
            Ok(key) => Zeroizing::new(*key.as_bytes()),
            Err(error) => {
                warn!("History encryption key is unavailable: {error:?}");
                return self.degrade(HistoryStorageReason::KeyUnavailable);
            }
        };
        self.apply_key(key, now_utc_ms)
    }

    /// Bring the file to its encrypted state with an already resolved key.
    fn apply_key(&self, key: DatabaseKey, now_utc_ms: i64) -> HistoryStorageStatus {
        let mut state = self.lock_state();
        if matches!(&*state, State::Encrypted { .. }) {
            return status_of(&state);
        }
        // Release the pre-unlock connection before the file is rewritten. The
        // migration renames an encrypted copy over the original and truncates
        // its write-ahead log, and a connection still holding the plaintext file
        // would keep the log from folding into it. Only the plaintext state can
        // be holding one here, since an encrypted state returned above and a
        // locked one never opened anything.
        if let State::Plaintext { connection, .. } = &mut *state {
            connection.take();
        }

        let on_disk = file_state(&self.path);
        if !cipher_available() {
            return self.publish(
                &mut state,
                degraded(on_disk, HistoryStorageReason::EncryptionUnavailable),
            );
        }

        let encrypted_at = match on_disk {
            FileState::Absent => create_encrypted(&self.path, &key, now_utc_ms).map(Some),
            FileState::Plaintext => migrate_plaintext(&self.path, &key, now_utc_ms).map(Some),
            FileState::Encrypted => open_existing(&self.path, &key),
        };

        match encrypted_at {
            Ok(encrypted_at_utc_ms) => {
                info!(
                    "History database is encrypted at rest ({})",
                    match on_disk {
                        FileState::Absent => "created encrypted",
                        FileState::Plaintext => "migrated from plaintext",
                        FileState::Encrypted => "already encrypted",
                    }
                );
                self.publish(
                    &mut state,
                    State::Encrypted {
                        key,
                        encrypted_at_utc_ms,
                        connection: None,
                    },
                )
            }
            Err(error) => {
                warn!("History database could not be encrypted: {error:#}");
                let reason = match on_disk {
                    FileState::Absent | FileState::Plaintext => {
                        HistoryStorageReason::MigrationFailed
                    }
                    FileState::Encrypted => HistoryStorageReason::KeyRejected,
                };
                self.publish(&mut state, degraded(on_disk, reason))
            }
        }
    }

    /// Record why encryption is not in force, leaving an already encrypted
    /// database alone.
    fn degrade(&self, reason: HistoryStorageReason) -> HistoryStorageStatus {
        let mut state = self.lock_state();
        if matches!(&*state, State::Encrypted { .. }) {
            return status_of(&state);
        }
        self.publish(&mut state, degraded(file_state(&self.path), reason))
    }

    /// Publish a state and wake the connections waiting for the unlock.
    fn publish(&self, state: &mut State, next: State) -> HistoryStorageStatus {
        *state = next;
        self.unlocked.notify_all();
        status_of(state)
    }

    fn lock_state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The state to fall back to when encryption cannot be applied. An encrypted
/// file must stay locked, because serving it without a key is impossible;
/// anything else serves plaintext so history remains usable.
fn degraded(on_disk: FileState, reason: HistoryStorageReason) -> State {
    match on_disk {
        FileState::Encrypted => State::Locked(reason),
        FileState::Absent | FileState::Plaintext => State::Plaintext {
            reason,
            connection: None,
        },
    }
}

fn status_of(state: &State) -> HistoryStorageStatus {
    match state {
        State::Plaintext { reason, .. } => HistoryStorageStatus {
            encrypted: false,
            migrated_at: None,
            reason: Some(reason.as_str().to_string()),
        },
        State::Encrypted {
            encrypted_at_utc_ms,
            ..
        } => HistoryStorageStatus {
            encrypted: true,
            migrated_at: *encrypted_at_utc_ms,
            reason: None,
        },
        State::Locked(reason) => HistoryStorageStatus {
            encrypted: true,
            migrated_at: None,
            reason: Some(reason.as_str().to_string()),
        },
    }
}

fn file_state(path: &Path) -> FileState {
    let mut header = [0_u8; SQLITE_HEADER.len()];
    let Ok(mut file) = fs::File::open(path) else {
        return FileState::Absent;
    };
    // A file too short to hold a header is either empty or truncated; SQLite
    // treats both as a database it may create from scratch.
    match file.read_exact(&mut header) {
        Ok(()) if &header == SQLITE_HEADER => FileState::Plaintext,
        Ok(()) => FileState::Encrypted,
        Err(_) => FileState::Absent,
    }
}

/// Whether this build can open a SQLCipher database at all. Checked once per
/// unlock against an in-memory database so a build without SQLCipher reports
/// one clear reason instead of a per-file failure.
fn cipher_available() -> bool {
    let Ok(connection) = Connection::open_in_memory() else {
        return false;
    };
    connection
        .query_row("PRAGMA cipher_version", [], |row| row.get::<_, String>(0))
        .optional()
        .ok()
        .flatten()
        .is_some_and(|version| !version.trim().is_empty())
}

/// Open an encrypted database with a raw key. The raw form skips SQLCipher's key
/// derivation, so per-connection cost stays negligible.
fn open_keyed(path: &Path, key: &DatabaseKey) -> Result<Connection> {
    let connection = Connection::open(path)?;
    let pragma = Zeroizing::new(format!("PRAGMA key = \"x'{}'\";", hex::encode(&**key)));
    connection.execute_batch(pragma.as_str())?;
    // A wrong key only surfaces on the first read of the schema.
    connection
        .query_row("SELECT count(*) FROM sqlite_master", [], |row| {
            row.get::<_, i64>(0)
        })
        .context("history database could not be read with the stored key")?;
    Ok(connection)
}

/// Create the database encrypted, for an install with no history file yet.
///
/// A database SQLCipher creates starts in SQLite's rollback journal mode, so
/// this is the one place a fresh install can be given the write-ahead log.
/// [`restore_journal_mode`] does the same for an upgraded database; between
/// them, every encrypted history file ends up with the reader/writer
/// concurrency that the persistent journal mode then keeps.
fn create_encrypted(path: &Path, key: &DatabaseKey, now_utc_ms: i64) -> Result<i64> {
    let connection = open_keyed(path, key)?;
    connection.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
    write_encrypted_at(&connection, now_utc_ms)?;
    Ok(now_utc_ms)
}

/// Open an already encrypted database and report when it was encrypted.
fn open_existing(path: &Path, key: &DatabaseKey) -> Result<Option<i64>> {
    let connection = open_keyed(path, key)?;
    read_encrypted_at(&connection)
}

/// Replace a plaintext database with an encrypted copy of itself.
///
/// The plaintext file is copied to a timestamped backup first, the encrypted
/// copy is built and verified beside it, and the rename is the only step that
/// touches the original. A failure anywhere leaves the plaintext database in
/// place and serving.
fn migrate_plaintext(path: &Path, key: &DatabaseKey, now_utc_ms: i64) -> Result<i64> {
    let backup = sibling(path, &format!(".plaintext-backup-{now_utc_ms}"));
    let staged = sibling(path, ".sqlcipher-staged");
    let _ = fs::remove_file(&staged);

    // A database adopted from the legacy app runs in WAL mode, which keeps
    // committed rows in a sidecar file. Fold them into the main file so both the
    // backup copy and the export see every committed row.
    let journal = fold_write_ahead_log(path)?;

    fs::copy(path, &backup).with_context(|| {
        format!(
            "back up {} to {} before encrypting",
            path.display(),
            backup.display()
        )
    })?;

    match encrypt_copy(path, &staged, key, now_utc_ms) {
        Ok(encrypted_at_utc_ms) => {
            info!(
                "Plaintext history database backed up to {}",
                backup.display()
            );
            restore_journal_mode(path, key, journal);
            Ok(encrypted_at_utc_ms)
        }
        Err(error) => {
            // The original was never modified, so the copy made moments ago is
            // redundant. Keeping it would add one file per failed launch.
            let _ = fs::remove_file(&staged);
            let _ = fs::remove_file(&backup);
            Err(error)
        }
    }
}

/// Checkpoint and truncate the write-ahead log, and report the journal mode the
/// database was using.
fn fold_write_ahead_log(path: &Path) -> Result<String> {
    let connection = Connection::open(path)?;
    let journal: String = connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    connection
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
        .optional()
        .context("fold the history write-ahead log into the database")?;
    Ok(journal)
}

/// Give the encrypted database the journal mode its plaintext original had. A
/// fresh export starts in SQLite's default rollback mode, and silently dropping
/// WAL would take away the reader/writer concurrency the database had before.
///
/// Best effort by design: it runs after the rename that commits the migration,
/// so a failure here must not undo an encrypted database that already works.
fn restore_journal_mode(path: &Path, key: &DatabaseKey, journal: String) {
    // The sidecars belong to the plaintext file the rename replaced.
    let _ = fs::remove_file(sibling(path, "-wal"));
    let _ = fs::remove_file(sibling(path, "-shm"));
    if !journal.eq_ignore_ascii_case("wal") {
        return;
    }
    let restored = open_keyed(path, key).and_then(|connection| {
        connection
            .pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))
            .map_err(Into::into)
    });
    if let Err(error) = restored {
        warn!("Encrypted history database stays in rollback journal mode: {error:#}");
    }
}

fn encrypt_copy(source: &Path, staged: &Path, key: &DatabaseKey, now_utc_ms: i64) -> Result<i64> {
    let expected = export_to_staged(source, staged, key)?;
    {
        let connection = open_keyed(staged, key)?;
        let carried = schema_fingerprint(&connection)?;
        if carried != expected {
            return Err(anyhow!(
                "encrypted copy has {} schema objects at user_version {}, expected {} at {}",
                carried.objects,
                carried.user_version,
                expected.objects,
                expected.user_version
            ));
        }
        write_encrypted_at(&connection, now_utc_ms)?;
    }
    // The rename is the commit point: every earlier step wrote only the staged
    // file, so failing before here leaves the plaintext database untouched.
    fs::rename(staged, source).with_context(|| {
        format!(
            "replace {} with its encrypted copy {}",
            source.display(),
            staged.display()
        )
    })?;
    Ok(now_utc_ms)
}

/// Copy a plaintext database into a new encrypted file with SQLCipher's own
/// exporter, then carry `user_version` across because `sqlcipher_export` does
/// not copy it and the history migrations track schema state with it.
fn export_to_staged(source: &Path, staged: &Path, key: &DatabaseKey) -> Result<SchemaFingerprint> {
    let staged_path = staged
        .to_str()
        .ok_or_else(|| anyhow!("history database path is not valid UTF-8"))?;
    let connection = Connection::open(source)?;
    let fingerprint = schema_fingerprint(&connection)?;
    let key_literal = Zeroizing::new(format!("x'{}'", hex::encode(&**key)));

    connection
        .execute(
            "ATTACH DATABASE ?1 AS sqlcipher KEY ?2",
            params![staged_path, key_literal.as_str()],
        )
        .context("attach the staged encrypted database")?;
    let exported = connection
        .query_row("SELECT sqlcipher_export('sqlcipher')", [], |_| Ok(()))
        .context("export the plaintext database into the staged copy")
        .and_then(|()| {
            // A schema-qualified pragma cannot be parameterized, and the value
            // is the integer just read from this same database.
            connection
                .execute_batch(&format!(
                    "PRAGMA sqlcipher.user_version = {};",
                    fingerprint.user_version
                ))
                .context("carry user_version into the staged copy")
        });
    connection.execute_batch("DETACH DATABASE sqlcipher")?;
    exported?;
    Ok(fingerprint)
}

/// The two facts that must survive the copy: how many schema objects exist and
/// which migration the schema is at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SchemaFingerprint {
    objects: i64,
    user_version: i32,
}

fn schema_fingerprint(connection: &Connection) -> Result<SchemaFingerprint> {
    Ok(SchemaFingerprint {
        objects: connection
            .query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get(0))?,
        user_version: connection.pragma_query_value(None, "user_version", |row| row.get(0))?,
    })
}

fn write_encrypted_at(connection: &Connection, now_utc_ms: i64) -> Result<()> {
    connection.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS {META_TABLE} (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );"
    ))?;
    connection.execute(
        &format!("INSERT OR IGNORE INTO {META_TABLE} (key, value) VALUES (?1, ?2)"),
        params![ENCRYPTED_AT_KEY, now_utc_ms.to_string()],
    )?;
    Ok(())
}

fn read_encrypted_at(connection: &Connection) -> Result<Option<i64>> {
    let stored: Option<String> = connection
        .query_row(
            &format!("SELECT value FROM {META_TABLE} WHERE key = ?1"),
            params![ENCRYPTED_AT_KEY],
            |row| row.get(0),
        )
        .optional()
        .unwrap_or(None);
    Ok(stored.and_then(|value| value.parse().ok()))
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite_migration::Migrations;

    const KEY: [u8; 32] = [7; 32];

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    fn key() -> DatabaseKey {
        Zeroizing::new(KEY)
    }

    /// A plaintext database with the real history schema and one row per table
    /// the encryption path has to carry across.
    fn seed_plaintext(path: &Path) {
        let mut connection = Connection::open(path).expect("open plaintext");
        Migrations::new(super::super::MIGRATIONS.to_vec())
            .to_latest(&mut connection)
            .expect("apply history migrations");
        connection
            .execute(
                "INSERT INTO transcription_history (file_name, timestamp, saved, title, transcription_text)
                 VALUES ('a.wav', 1, 0, 'first', 'hello world')",
                [],
            )
            .expect("insert history row");
    }

    fn transcription_texts(connection: &Connection) -> Vec<String> {
        connection
            .prepare("SELECT transcription_text FROM transcription_history ORDER BY id")
            .expect("prepare")
            .query_map([], |row| row.get(0))
            .expect("query")
            .collect::<rusqlite::Result<Vec<String>>>()
            .expect("rows")
    }

    /// The rows the storage hands back through whatever state it published.
    fn stored_texts(storage: &HistoryStorage) -> Vec<String> {
        storage
            .with_connection(|connection| Ok(transcription_texts(connection)))
            .expect("connection")
    }

    /// Every schema object except the storage-owned metadata table and the
    /// implicit index its text primary key creates.
    fn app_schema_names(connection: &Connection) -> Vec<String> {
        connection
            .prepare(&format!(
                "SELECT name FROM sqlite_master WHERE tbl_name != '{META_TABLE}' ORDER BY name"
            ))
            .expect("prepare")
            .query_map([], |row| row.get(0))
            .expect("query")
            .collect::<rusqlite::Result<Vec<String>>>()
            .expect("rows")
    }

    #[test]
    fn plaintext_and_encrypted_files_are_told_apart_by_their_header() {
        let directory = temp_dir();
        let missing = directory.path().join("missing.db");
        assert_eq!(file_state(&missing), FileState::Absent);

        let plain = directory.path().join("plain.db");
        seed_plaintext(&plain);
        assert_eq!(file_state(&plain), FileState::Plaintext);

        let encrypted = directory.path().join("encrypted.db");
        create_encrypted(&encrypted, &key(), 5).expect("create encrypted");
        assert_eq!(file_state(&encrypted), FileState::Encrypted);
    }

    /// A read must resolve the key when nothing else has begun to, wait when
    /// something has, and never wait for a resolution nobody started.
    ///
    /// Only a process with a startup callback calls [`HistoryStorage::unlock`].
    /// The headless corpus verbs mount the history manager and read straight
    /// away, so there the read resolves the key itself. The bug this replaced
    /// was one state standing for both cases: a read with no unlock coming
    /// waited out `UNLOCK_WAIT` for a step that was not running and then
    /// reported a locked database, which the query plane could only report as
    /// a failure.
    #[test]
    fn an_unresolved_key_is_resolved_by_the_read_and_never_waited_on() {
        let directory = temp_dir();
        let path = directory.path().join("history.db");
        seed_plaintext(&path);
        HistoryStorage::at_startup(path.clone()).apply_key(key(), 1_700_000_000_000);

        // The state every headless corpus read starts in: an encrypted file,
        // and nothing in the process that will resolve its key.
        let storage = HistoryStorage::at_startup(path);
        assert!(
            storage.needs_resolution(),
            "an encrypted file nobody is resolving must send the read after the key"
        );
        assert!(!storage.is_ready());

        // The whole point of splitting the state: no resolution is running, so
        // there is nothing to wait for and the caller hears so immediately.
        let refused = std::time::Instant::now();
        assert!(storage.with_connection(|_| Ok(())).is_err());
        assert!(
            refused.elapsed() < UNLOCK_WAIT,
            "an unresolved key must be reported at once, not waited out"
        );

        let published = std::time::Instant::now();
        storage.apply_key(key(), 1_700_000_000_000);
        assert!(
            !storage.needs_resolution(),
            "a key that has landed must not be resolved again on the next read"
        );

        // The read the headless corpus verb actually makes, over the encrypted
        // store, with no recall model: the lexical half has to answer alone.
        let found = storage
            .with_connection(|connection| {
                super::super::HistoryManager::search_history_entries_with_connection(
                    connection,
                    "hello",
                    None,
                    Some(5),
                    None,
                )
            })
            .expect("search the encrypted store");
        assert_eq!(
            found
                .entries
                .iter()
                .map(|entry| entry.transcription_text.as_str())
                .collect::<Vec<_>>(),
            vec!["hello world"]
        );
        assert!(
            published.elapsed() < UNLOCK_WAIT,
            "a read behind a published state must not wait on the unlock condvar"
        );

        // Two callers arriving together must not both go to the credential
        // store. The claim is the check, so exactly one wins and the loser is
        // handed the current status to wait on instead of a second prompt.
        let contended = HistoryStorage::at_startup(directory.path().join("contended.db"));
        assert!(contended.needs_resolution());
        assert!(
            contended.claim_resolution().is_none(),
            "the first caller must own the resolution"
        );
        assert!(
            !contended.needs_resolution(),
            "a claimed resolution must not send a second read after the key"
        );
        assert!(
            contended.claim_resolution().is_some(),
            "a second caller must be turned away rather than prompt again"
        );

        // A plaintext history file needs no key at all, so a headless read
        // serves it without going near the credential store or tripping the one
        // time encryption migration on its way to a SELECT.
        let plain = directory.path().join("plain.db");
        seed_plaintext(&plain);
        assert!(
            !HistoryStorage::at_startup(plain).needs_resolution(),
            "a plaintext database must not send the read path after a key"
        );
    }

    #[test]
    fn plaintext_database_migrates_to_sqlcipher_with_rows_and_schema_intact() {
        let directory = temp_dir();
        let path = directory.path().join("history.db");
        seed_plaintext(&path);
        let (before, before_names) = {
            let connection = Connection::open(&path).expect("open plaintext");
            (
                schema_fingerprint(&connection).expect("fingerprint"),
                app_schema_names(&connection),
            )
        };

        let storage = HistoryStorage::at_startup(path.clone());
        assert!(!storage.status().encrypted);

        let status = storage.apply_key(key(), 1_700_000_000_000);
        assert_eq!(
            status,
            HistoryStorageStatus {
                encrypted: true,
                migrated_at: Some(1_700_000_000_000),
                reason: None,
            }
        );
        assert_eq!(file_state(&path), FileState::Encrypted);

        // The plaintext file is unreadable without the key, and readable with it.
        assert!(Connection::open(&path)
            .expect("open")
            .query_row("SELECT count(*) FROM sqlite_master", [], |row| row
                .get::<_, i64>(0))
            .is_err());

        storage
            .with_connection(|connection| {
                assert_eq!(transcription_texts(connection), vec!["hello world"]);
                let after = schema_fingerprint(connection).expect("fingerprint");
                assert_eq!(after.user_version, before.user_version);
                // Tables, the FTS5 virtual table, its shadow tables, indexes, and
                // triggers all have to survive the copy.
                assert_eq!(app_schema_names(connection), before_names);
                Ok(())
            })
            .expect("keyed connection");
        assert!(before_names
            .iter()
            .any(|name| name == "transcription_history_fts"));

        let backup = sibling(&path, ".plaintext-backup-1700000000000");
        let backup_connection = Connection::open(&backup).expect("open backup");
        assert_eq!(transcription_texts(&backup_connection), vec!["hello world"]);
    }

    #[test]
    fn a_write_ahead_log_database_keeps_its_rows_and_journal_mode() {
        let directory = temp_dir();
        let path = directory.path().join("history.db");
        seed_plaintext(&path);
        // The log file only outlives its writer while a connection is still
        // open, which is also how a crashed session leaves the database.
        let writer = Connection::open(&path).expect("open plaintext");
        writer
            .pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))
            .expect("switch to WAL");
        writer
            .execute(
                "INSERT INTO transcription_history (file_name, timestamp, saved, title, transcription_text)
                 VALUES ('b.wav', 2, 0, 'second', 'in the log')",
                [],
            )
            .expect("insert a row that lives in the log");
        assert!(sibling(&path, "-wal").exists());

        let storage = HistoryStorage::at_startup(path.clone());
        assert!(storage.apply_key(key(), 8).encrypted);
        drop(writer);

        storage
            .with_connection(|connection| {
                assert_eq!(
                    transcription_texts(connection),
                    vec!["hello world", "in the log"]
                );
                let journal: String = connection
                    .pragma_query_value(None, "journal_mode", |row| row.get(0))
                    .expect("journal mode");
                assert_eq!(journal.to_lowercase(), "wal");
                Ok(())
            })
            .expect("keyed connection");

        let backup = Connection::open(sibling(&path, ".plaintext-backup-8")).expect("open backup");
        assert_eq!(
            transcription_texts(&backup),
            vec!["hello world", "in the log"]
        );
    }

    #[test]
    fn unlock_encrypts_with_a_credential_store_key_that_reopens_the_database() {
        let directory = temp_dir();
        let path = directory.path().join("history.db");
        seed_plaintext(&path);
        let backend = std::sync::Arc::new(crate::secrets::MemorySecretBackend::new());
        let secrets = SecretManager::with_backend(backend.clone());

        let status = tauri::async_runtime::block_on(
            HistoryStorage::at_startup(path.clone()).unlock(&secrets, 1_234),
        );

        assert_eq!(
            status,
            HistoryStorageStatus {
                encrypted: true,
                migrated_at: Some(1_234),
                reason: None,
            }
        );
        assert!(backend.has("history_storage/database-key-v1"));

        // The same account has to reopen the file on the next launch.
        let next_launch = HistoryStorage::at_startup(path.clone());
        let reopened = tauri::async_runtime::block_on(next_launch.unlock(&secrets, 5_678));
        assert_eq!(reopened.migrated_at, Some(1_234));
        assert_eq!(stored_texts(&next_launch), vec!["hello world"]);
    }

    #[test]
    fn migrated_database_reopens_encrypted_on_the_next_launch() {
        let directory = temp_dir();
        let path = directory.path().join("history.db");
        seed_plaintext(&path);
        HistoryStorage::at_startup(path.clone()).apply_key(key(), 42);

        let next_launch = HistoryStorage::at_startup(path.clone());
        assert!(!next_launch.is_ready());
        assert_eq!(
            next_launch.status(),
            HistoryStorageStatus {
                encrypted: true,
                migrated_at: None,
                reason: Some("unlocking".to_string()),
            }
        );

        let status = next_launch.apply_key(key(), 99);
        assert_eq!(
            status,
            HistoryStorageStatus {
                encrypted: true,
                // The moment encryption was established, not this launch.
                migrated_at: Some(42),
                reason: None,
            }
        );
        assert!(next_launch.is_ready());
        assert_eq!(stored_texts(&next_launch), vec!["hello world"]);
    }

    #[test]
    fn a_missing_database_is_created_encrypted() {
        let directory = temp_dir();
        let path = directory.path().join("history.db");

        let storage = HistoryStorage::at_startup(path.clone());
        assert!(!storage.is_ready());

        let status = storage.apply_key(key(), 7);
        assert_eq!(status.encrypted, true);
        assert_eq!(status.migrated_at, Some(7));
        assert_eq!(file_state(&path), FileState::Encrypted);
        assert!(!sibling(&path, ".plaintext-backup-7").exists());
    }

    /// A temporary table lives and dies with the connection that created it, so
    /// finding it again is proof the second query ran on the first query's
    /// connection instead of a fresh one.
    fn marked(storage: &HistoryStorage) -> bool {
        storage
            .with_connection(|connection| {
                Ok(connection
                    .query_row(
                        "SELECT count(*) FROM sqlite_temp_master WHERE name = 'connection_marker'",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .expect("read temp schema")
                    > 0)
            })
            .expect("connection")
    }

    fn mark(storage: &HistoryStorage) {
        storage
            .with_connection(|connection| {
                connection
                    .execute_batch("CREATE TEMP TABLE connection_marker (id INTEGER)")
                    .expect("create marker");
                Ok(())
            })
            .expect("connection");
    }

    #[test]
    fn every_query_runs_on_one_retained_connection() {
        let directory = temp_dir();
        let path = directory.path().join("history.db");
        seed_plaintext(&path);
        let storage = HistoryStorage::at_startup(path);
        assert!(storage.apply_key(key(), 21).encrypted);

        assert!(!marked(&storage));
        mark(&storage);
        assert!(marked(&storage), "a second query opened its own connection");
        assert!(
            marked(&storage),
            "the connection was not kept across queries"
        );
    }

    #[test]
    fn a_failed_query_keeps_the_connection() {
        let directory = temp_dir();
        let path = directory.path().join("history.db");
        seed_plaintext(&path);
        let storage = HistoryStorage::at_startup(path);
        assert!(storage.apply_key(key(), 22).encrypted);
        mark(&storage);

        let failed: Result<()> = storage.with_connection(|connection| {
            connection.execute_batch("SELECT * FROM no_such_table")?;
            Ok(())
        });
        assert!(failed.is_err());

        // A statement error says nothing about the connection, so throwing it
        // away would reintroduce the per-query open this design removes.
        assert!(marked(&storage));
    }

    #[test]
    fn relocking_drops_the_connection_the_old_state_owned() {
        let directory = temp_dir();
        let path = directory.path().join("history.db");
        seed_plaintext(&path);
        let storage = HistoryStorage::at_startup(path.clone());

        // Serve the plaintext file, then encrypt it underneath. The connection
        // opened against the plaintext file must not survive the rename.
        mark(&storage);
        assert!(marked(&storage));

        assert!(storage.apply_key(key(), 23).encrypted);
        assert_eq!(file_state(&path), FileState::Encrypted);
        assert!(
            !marked(&storage),
            "the pre-migration connection outlived the state that authorized it"
        );
        assert_eq!(stored_texts(&storage), vec!["hello world"]);
    }

    #[test]
    fn a_database_created_encrypted_uses_the_write_ahead_log() {
        let directory = temp_dir();
        let path = directory.path().join("history.db");

        let storage = HistoryStorage::at_startup(path.clone());
        assert!(storage.apply_key(key(), 25).encrypted);

        // A fresh SQLCipher database starts in rollback mode; only
        // `create_encrypted` can give a new install the log that
        // `restore_journal_mode` gives an upgraded one.
        let journal: String = storage
            .with_connection(|connection| {
                Ok(connection
                    .pragma_query_value(None, "journal_mode", |row| row.get(0))
                    .expect("journal mode"))
            })
            .expect("connection");
        assert_eq!(journal.to_lowercase(), "wal");
    }

    #[test]
    fn an_unresolvable_key_leaves_a_plaintext_database_working() {
        let directory = temp_dir();
        let path = directory.path().join("history.db");
        seed_plaintext(&path);

        let storage = HistoryStorage::at_startup(path.clone());
        let status = storage.degrade(HistoryStorageReason::KeyUnavailable);

        assert_eq!(
            status,
            HistoryStorageStatus {
                encrypted: false,
                migrated_at: None,
                reason: Some("key_unavailable".to_string()),
            }
        );
        assert!(storage.is_ready());
        assert_eq!(file_state(&path), FileState::Plaintext);
        assert_eq!(stored_texts(&storage), vec!["hello world"]);
    }

    #[test]
    fn a_failed_migration_leaves_the_plaintext_database_serving() {
        let directory = temp_dir();
        let path = directory.path().join("history.db");
        seed_plaintext(&path);
        // A directory where the staged copy has to go makes the export fail
        // after the backup was taken.
        fs::create_dir(sibling(&path, ".sqlcipher-staged")).expect("block the staged path");

        let storage = HistoryStorage::at_startup(path.clone());
        let status = storage.apply_key(key(), 11);

        assert_eq!(
            status,
            HistoryStorageStatus {
                encrypted: false,
                migrated_at: None,
                reason: Some("migration_failed".to_string()),
            }
        );
        assert_eq!(file_state(&path), FileState::Plaintext);
        assert_eq!(stored_texts(&storage), vec!["hello world"]);
        // A failed attempt must not leave a redundant backup behind on every
        // launch.
        assert!(!sibling(&path, ".plaintext-backup-11").exists());
    }

    #[test]
    fn a_wrong_key_keeps_an_encrypted_database_locked() {
        let directory = temp_dir();
        let path = directory.path().join("history.db");
        create_encrypted(&path, &key(), 3).expect("create encrypted");

        let storage = HistoryStorage::at_startup(path.clone());
        let status = storage.apply_key(Zeroizing::new([9; 32]), 4);

        assert_eq!(
            status,
            HistoryStorageStatus {
                encrypted: true,
                migrated_at: None,
                reason: Some("key_rejected".to_string()),
            }
        );
        assert!(!storage.is_ready());
        assert!(storage.with_connection(|_| Ok(())).is_err());
        assert_eq!(file_state(&path), FileState::Encrypted);
    }
}

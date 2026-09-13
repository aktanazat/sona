use crate::analytics::{DashboardTrendRange, DashboardTrendRequest, LocalCalendarRange};
use crate::context::ContextReceipt;
use crate::delivery::{DeliveryMethod, DeliveryOutcome, DeliveryReceipt};
use crate::modes::ModeReceipt;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Local, Utc};
use log::{debug, error, info, warn};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use rusqlite_migration::{Migrations, M};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tauri_specta::Event;

pub(crate) mod semantic;
mod storage;

use semantic::{SemanticModel, SemanticModelSlot};
use storage::HistoryStorage;
pub use storage::HistoryStorageStatus;

/// Emitted after the startup unlock decides whether history is encrypted at
/// rest. The payload is [`HistoryStorageStatus`]; a listener that gets a
/// non-encrypted or locked status should surface the degraded state, and one
/// that was refused a read while locked should retry.
pub const HISTORY_STORAGE_EVENT: &str = "history-storage-changed";

/// Database migrations for transcription history.
/// Each migration is applied in order. The library tracks which migrations
/// have been applied using SQLite's user_version pragma.
///
/// Note: For users upgrading from tauri-plugin-sql, migrate_from_tauri_plugin_sql()
/// converts the old _sqlx_migrations table tracking to the user_version pragma,
/// ensuring migrations don't re-run on existing databases.
static MIGRATIONS: &[M] = &[
    M::up(
        "CREATE TABLE IF NOT EXISTS transcription_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file_name TEXT NOT NULL,
            timestamp INTEGER NOT NULL,
            saved BOOLEAN NOT NULL DEFAULT 0,
            title TEXT NOT NULL,
            transcription_text TEXT NOT NULL
        );",
    ),
    M::up("ALTER TABLE transcription_history ADD COLUMN post_processed_text TEXT;"),
    M::up("ALTER TABLE transcription_history ADD COLUMN post_process_prompt TEXT;"),
    M::up("ALTER TABLE transcription_history ADD COLUMN post_process_requested BOOLEAN NOT NULL DEFAULT 0;"),
    M::up(
        "CREATE VIRTUAL TABLE transcription_history_fts USING fts5(
            transcription_text,
            post_processed_text,
            content = 'transcription_history',
            content_rowid = 'id'
        );

        CREATE TRIGGER transcription_history_fts_insert
        AFTER INSERT ON transcription_history BEGIN
            INSERT INTO transcription_history_fts(
                rowid,
                transcription_text,
                post_processed_text
            ) VALUES (
                new.id,
                new.transcription_text,
                new.post_processed_text
            );
        END;

        CREATE TRIGGER transcription_history_fts_delete
        AFTER DELETE ON transcription_history BEGIN
            INSERT INTO transcription_history_fts(
                transcription_history_fts,
                rowid,
                transcription_text,
                post_processed_text
            ) VALUES (
                'delete',
                old.id,
                old.transcription_text,
                old.post_processed_text
            );
        END;

        CREATE TRIGGER transcription_history_fts_update
        AFTER UPDATE OF transcription_text, post_processed_text ON transcription_history BEGIN
            INSERT INTO transcription_history_fts(
                transcription_history_fts,
                rowid,
                transcription_text,
                post_processed_text
            ) VALUES (
                'delete',
                old.id,
                old.transcription_text,
                old.post_processed_text
            );
            INSERT INTO transcription_history_fts(
                rowid,
                transcription_text,
                post_processed_text
            ) VALUES (
                new.id,
                new.transcription_text,
                new.post_processed_text
            );
        END;

        INSERT INTO transcription_history_fts(transcription_history_fts)
        VALUES ('rebuild');",
    ),
    M::up(
        "CREATE TABLE transcription_runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            history_id INTEGER NOT NULL,
            run_id INTEGER NOT NULL,
            retry_of_run_id INTEGER,
            started_at_ms INTEGER NOT NULL,
            completed_at_ms INTEGER NOT NULL,
            mode_id TEXT NOT NULL,
            mode_revision INTEGER NOT NULL,
            mode_receipt_json TEXT NOT NULL,
            context_receipt_json TEXT NOT NULL,
            delivery_method TEXT NOT NULL,
            delivery_outcome TEXT NOT NULL,
            delivery_dispatched_at_ms INTEGER NOT NULL,
            FOREIGN KEY(history_id) REFERENCES transcription_history(id)
        );
        CREATE INDEX transcription_runs_history_id ON transcription_runs(history_id, id DESC);
        CREATE TRIGGER transcription_runs_history_delete
        AFTER DELETE ON transcription_history BEGIN
            DELETE FROM transcription_runs WHERE history_id = old.id;
        END;",
    ),
    // The `UPDATE ... SET post_process_prompt = NULL` below is deliberately
    // irreversible: a stored prompt body can embed captured application
    // context, which history has no business retaining. Applied migration SQL
    // is never rewritten, so the column itself is dropped by the last
    // migration in this list instead.
    M::up(
        "ALTER TABLE transcription_history ADD COLUMN duration_ms INTEGER;
         ALTER TABLE transcription_history ADD COLUMN word_count INTEGER;
         ALTER TABLE transcription_history ADD COLUMN source_kind TEXT;
         ALTER TABLE transcription_history ADD COLUMN has_audio BOOLEAN NOT NULL DEFAULT 1;
         UPDATE transcription_history SET post_process_prompt = NULL;
         CREATE TABLE delivery_attempts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            history_id INTEGER NOT NULL,
            run_receipt_id INTEGER NOT NULL,
            delivery_method TEXT NOT NULL,
            delivery_outcome TEXT NOT NULL,
            dispatched_at_ms INTEGER NOT NULL,
            FOREIGN KEY(history_id) REFERENCES transcription_history(id),
            FOREIGN KEY(run_receipt_id) REFERENCES transcription_runs(id)
         );
         CREATE INDEX delivery_attempts_run_receipt_id
            ON delivery_attempts(run_receipt_id, id ASC);
         CREATE TRIGGER delivery_attempts_run_delete
         AFTER DELETE ON transcription_runs BEGIN
            DELETE FROM delivery_attempts WHERE run_receipt_id = old.id;
         END;",
    ),
    M::up(
        "CREATE TABLE upstream_import_history (
            source_identity TEXT NOT NULL,
            source_history_id INTEGER NOT NULL,
            source_row_sha256 TEXT NOT NULL,
            history_id INTEGER NOT NULL,
            PRIMARY KEY (source_identity, source_history_id),
            FOREIGN KEY(history_id) REFERENCES transcription_history(id)
        );
        CREATE INDEX upstream_import_history_history_id
            ON upstream_import_history(history_id);
        CREATE TRIGGER upstream_import_history_delete
        AFTER DELETE ON transcription_history BEGIN
            DELETE FROM upstream_import_history WHERE history_id = old.id;
        END;",
    ),
    // `post_process_prompt` is dead storage. Migration 7 wiped every value, no
    // reader, trigger, index, or FTS content column names it, and nothing
    // writes it. `DROP COLUMN` needs SQLite >= 3.35; `rusqlite` is pinned with
    // the `bundled` feature (3.50.2), so the DDL exists on every platform this
    // fork ships.
    M::up("ALTER TABLE transcription_history DROP COLUMN post_process_prompt;"),
    // A capture overrun keeps a playable prefix, but it must remain visibly
    // distinct from a complete capture. Retries and imported rows stay NULL.
    M::up("ALTER TABLE transcription_runs ADD COLUMN capture_status TEXT;"),
    // A retry or a reprocess is a new immutable row produced from an existing
    // recording, so it points back at the row it came from. Original captures,
    // imported rows, and every row written before this column existed stay
    // NULL. Deleting a parent orphans rather than deletes the child: the
    // child's own transcript is still real output.
    M::up(
        "ALTER TABLE transcription_history ADD COLUMN parent_id INTEGER;
         CREATE TRIGGER transcription_history_parent_delete AFTER DELETE ON transcription_history BEGIN
            UPDATE transcription_history SET parent_id = NULL WHERE parent_id = old.id;
         END;",
    ),
    // Semantic recall stores one unit vector per row beside the FTS5 index.
    // Both columns are nullable and nothing backfills them here: the vectors
    // need a model that may not be on disk yet, so filling them is a resumable
    // pass (`backfill_semantic_chunk_with_connection`) rather than migration
    // SQL that would have to either block startup or invent data.
    //
    // `semantic_model_revision` records which model considered the row.
    // Together the two columns distinguish the three real states: never
    // considered (both NULL), considered and embedded (both set), and
    // considered but unembeddable — whitespace, emoji — (revision set,
    // embedding NULL). The third is why the backfill terminates instead of
    // reselecting the same rows forever.
    //
    // The FTS5 triggers fire `AFTER UPDATE OF transcription_text,
    // post_processed_text`, so writing a vector does not touch the index.
    M::up(
        "ALTER TABLE transcription_history ADD COLUMN semantic_embedding BLOB;
         ALTER TABLE transcription_history ADD COLUMN semantic_model_revision TEXT;",
    ),
];

const MAX_HISTORY_PAGE_SIZE: usize = 100;
const DEFAULT_HISTORY_SEARCH_PAGE_SIZE: usize = 50;

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct PaginatedHistory {
    pub entries: Vec<HistoryEntry>,
    pub has_more: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
#[serde(tag = "action")]
pub enum HistoryUpdatePayload {
    #[serde(rename = "added")]
    Added { entry: HistoryEntry },
    #[serde(rename = "updated")]
    Updated { entry: HistoryEntry },
    #[serde(rename = "deleted")]
    Deleted { id: i64 },
    #[serde(rename = "toggled")]
    Toggled { id: i64 },
}

/// Why a row is in a search result.
///
/// `Text` means FTS5 matched the row's own words. `Semantic` means it did not,
/// and the row was recalled by embedding similarity instead. A row FTS5
/// matched is always `Text`, even when its embedding also clears the floor:
/// lexical evidence is the stronger claim and never gets overwritten by the
/// weaker one.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum HistoryMatchKind {
    Text,
    Semantic,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct HistoryEntry {
    pub id: i64,
    pub file_name: String,
    pub timestamp: i64,
    pub saved: bool,
    pub title: String,
    pub transcription_text: String,
    pub post_processed_text: Option<String>,
    pub post_process_requested: bool,
    /// The entry this one was reprocessed from, when it was not an original
    /// capture. `None` for every row written before reprocessing existed.
    pub parent_id: Option<i64>,
    /// Why this row was returned, on rows that came from a search. `None`
    /// everywhere else — a plain listing, an added-entry event, or a lookup by
    /// id has no match to explain, and claiming one would be false.
    #[serde(default)]
    pub match_kind: Option<HistoryMatchKind>,
}

/// Where the captured audio originated. Existing history rows intentionally
/// retain `NULL`: inventing a source for old data would be false provenance.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum HistorySourceKind {
    Microphone,
    File,
}

impl HistorySourceKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Microphone => "microphone",
            Self::File => "file",
        }
    }

    fn from_stored(value: Option<String>) -> Option<Self> {
        match value.as_deref() {
            Some("microphone") => Some(Self::Microphone),
            Some("file") => Some(Self::File),
            _ => None,
        }
    }
}

/// Whether the original microphone capture was complete. This belongs to the
/// capture run only; retries and imported rows have no capture status.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CaptureStatus {
    Complete,
    Truncated,
    NoSpeechDetected,
}

impl CaptureStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Truncated => "truncated",
            Self::NoSpeechDetected => "no_speech_detected",
        }
    }

    fn from_stored(value: Option<String>) -> Option<Self> {
        match value.as_deref() {
            Some("complete") => Some(Self::Complete),
            Some("truncated") => Some(Self::Truncated),
            Some("no_speech_detected") => Some(Self::NoSpeechDetected),
            _ => None,
        }
    }
}

/// One source-kind subtotal in the all-time history summary. Older rows can
/// have no source because Sona does not invent provenance during migration.
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct HistoryStatsBySource {
    pub source_kind: Option<HistorySourceKind>,
    pub entries: u64,
    pub total_duration_ms: u64,
    pub total_words: u64,
}

/// Content-free, read-only aggregates over the retained history rows.
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct HistoryStats {
    pub entries: u64,
    pub total_duration_ms: u64,
    pub total_words: u64,
    pub by_source: Vec<HistoryStatsBySource>,
}

/// One source subtotal in a trend point or aggregate. A `None` source kind
/// represents retained rows without recognized source provenance.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct HistoryTrendSourceTotals {
    pub source_kind: Option<HistorySourceKind>,
    pub recordings: u64,
    pub duration_ms: u64,
    pub words: u64,
}

/// Content-free aggregate for either the selected trend range or all retained
/// history.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct HistoryTrendTotals {
    pub recordings: u64,
    pub duration_ms: u64,
    pub words: u64,
    pub by_source: Vec<HistoryTrendSourceTotals>,
}

/// One local-calendar day in the history trend. Every requested date is
/// present, including dates with zero recordings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct HistoryTrendPoint {
    pub local_date: String,
    pub recordings: u64,
    pub duration_ms: u64,
    pub words: u64,
    pub by_source: Vec<HistoryTrendSourceTotals>,
}

/// A bounded local-calendar projection over retained transcription history.
/// `active_days` and `current_streak_days` are calculated within `range`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct HistoryTrendProjection {
    pub range: DashboardTrendRange,
    pub range_start_local_date: String,
    pub range_end_local_date: String,
    pub all_time: HistoryTrendTotals,
    pub range_total: HistoryTrendTotals,
    pub active_days: u16,
    pub current_streak_days: u16,
    pub points: Vec<HistoryTrendPoint>,
}

#[derive(Clone, Copy, Default)]
struct HistoryTrendValues {
    recordings: u64,
    duration_ms: u64,
    words: u64,
}

impl HistoryTrendValues {
    fn add(&mut self, other: Self) -> Result<()> {
        self.recordings = self
            .recordings
            .checked_add(other.recordings)
            .ok_or_else(|| anyhow!("history trend recording count overflowed"))?;
        self.duration_ms = self
            .duration_ms
            .checked_add(other.duration_ms)
            .ok_or_else(|| anyhow!("history trend duration overflowed"))?;
        self.words = self
            .words
            .checked_add(other.words)
            .ok_or_else(|| anyhow!("history trend word count overflowed"))?;
        Ok(())
    }
}

const HISTORY_TREND_MICROPHONE: usize = 0;
const HISTORY_TREND_FILE: usize = 1;
const HISTORY_TREND_UNKNOWN: usize = 2;

fn history_trend_source_slot(source_kind: Option<&str>) -> Result<usize> {
    match source_kind {
        Some("microphone") => Ok(HISTORY_TREND_MICROPHONE),
        Some("file") => Ok(HISTORY_TREND_FILE),
        None => Ok(HISTORY_TREND_UNKNOWN),
        Some(_) => Err(anyhow!("history trend source kind is invalid")),
    }
}

fn history_trend_source_kind(slot: usize) -> Option<HistorySourceKind> {
    match slot {
        HISTORY_TREND_MICROPHONE => Some(HistorySourceKind::Microphone),
        HISTORY_TREND_FILE => Some(HistorySourceKind::File),
        _ => None,
    }
}

fn history_trend_totals(values: [HistoryTrendValues; 3]) -> Result<HistoryTrendTotals> {
    let mut total = HistoryTrendValues::default();
    let mut by_source = Vec::with_capacity(values.len());
    for (slot, value) in values.into_iter().enumerate() {
        total.add(value)?;
        by_source.push(HistoryTrendSourceTotals {
            source_kind: history_trend_source_kind(slot),
            recordings: value.recordings,
            duration_ms: value.duration_ms,
            words: value.words,
        });
    }
    Ok(HistoryTrendTotals {
        recordings: total.recordings,
        duration_ms: total.duration_ms,
        words: total.words,
        by_source,
    })
}

fn history_trend_value(value: i64, name: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| anyhow!("history trend {name} is negative"))
}

/// One immutable run receipt linked to a recording. Text remains only in the
/// history entry and FTS table; this table holds content-free provenance.
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct HistoryRunReceipt {
    pub id: i64,
    pub history_id: i64,
    pub run_id: u64,
    pub retry_of_run_id: Option<u64>,
    pub started_at_ms: u64,
    pub completed_at_ms: u64,
    pub mode: ModeReceipt,
    pub context: ContextReceipt,
    pub duration_ms: Option<u64>,
    pub word_count: Option<u64>,
    pub source_kind: Option<HistorySourceKind>,
    pub has_audio: bool,
    pub capture_status: Option<CaptureStatus>,
    pub delivery_attempts: Vec<HistoryDeliveryAttempt>,
}

/// One immutable delivery observation associated with a persisted run. A
/// second observation is another row; no outcome is ever overwritten.
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct HistoryDeliveryAttempt {
    pub id: i64,
    pub history_id: i64,
    pub run_receipt_id: i64,
    pub delivery: DeliveryReceipt,
}
/// One dictation run as the local learning loops read it: the delivered text
/// plus the content-free provenance of the run that produced it.
///
/// `id` is the `transcription_runs` rowid, which is `AUTOINCREMENT` and so is
/// the monotonic cursor the loops page through. `is_retry` folds both lineage
/// columns — a run that points back at another run, and an entry that points
/// back at another entry — because either one makes this text a second reading
/// of audio a model already read, never a human's own words.
#[derive(Clone, Debug)]
pub struct DictationRunRow {
    pub id: i64,
    pub completed_at_ms: i64,
    /// Exactly what Sona delivered: the post-processed text when a run had it,
    /// the raw transcript otherwise. Replacement rules have already run, so a
    /// literal spoken form surviving here is one no rule covers.
    pub delivered_text: String,
    pub mode: ModeReceipt,
    pub capture_status: Option<CaptureStatus>,
    pub is_retry: bool,
}

/// Data persisted atomically with a new or retried transcription result.
#[derive(Clone, Debug)]
pub struct NewRunReceipt {
    pub run: ModeReceipt,
    pub context: ContextReceipt,
    pub started_at_ms: u64,
    pub completed_at_ms: u64,
    pub duration_ms: Option<u64>,
    pub word_count: Option<u64>,
    pub source_kind: HistorySourceKind,
    pub has_audio: bool,
    pub capture_status: Option<CaptureStatus>,
}
/// A validated row from the upstream SQLite database. It is deliberately
/// internal: transcript text never crosses the importer IPC boundary.
#[derive(Clone, Debug)]
pub(crate) struct UpstreamHistoryImportEntry {
    pub source_history_id: i64,
    pub source_row_sha256: String,
    pub timestamp: i64,
    pub saved: bool,
    pub title: String,
    pub transcription_text: String,
    pub post_processed_text: Option<String>,
    pub post_process_requested: bool,
    pub duration_ms: Option<u64>,
    pub word_count: Option<u64>,
    pub source_kind: Option<HistorySourceKind>,
    pub runs: Vec<UpstreamHistoryImportRun>,
}

#[derive(Clone, Debug)]
pub(crate) struct UpstreamHistoryImportRun {
    pub source_run_receipt_id: i64,
    pub run_id: u64,
    pub retry_of_run_id: Option<u64>,
    pub started_at_ms: u64,
    pub completed_at_ms: u64,
    pub mode: ModeReceipt,
    pub context: ContextReceipt,
    pub deliveries: Vec<UpstreamHistoryImportDelivery>,
}

#[derive(Clone, Debug)]
pub(crate) struct UpstreamHistoryImportDelivery {
    pub delivery: DeliveryReceipt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UpstreamHistoryImportOutcome {
    Inserted { history_id: i64 },
    Existing { history_id: i64 },
}
/// One row queued for embedding, carrying the text so the worker never has to
/// read it back out of the database it is about to write to.
struct SemanticWork {
    history_id: i64,
    text: String,
}

pub struct HistoryManager {
    app_handle: AppHandle,
    recordings_dir: PathBuf,
    /// Shared with the semantic worker thread, which takes the same single
    /// connection through `with_connection` rather than opening a second one.
    storage: Arc<HistoryStorage>,
    semantic: Arc<SemanticModelSlot>,
    /// Dropped with the manager, which is what stops the worker: its `recv`
    /// fails once no sender remains.
    semantic_work: Sender<SemanticWork>,
}

impl HistoryManager {
    pub fn new(app_handle: &AppHandle) -> Result<Self> {
        // Create recordings directory in app data dir
        let app_data_dir = crate::portable::app_data_dir(app_handle)?;
        let recordings_dir = app_data_dir.join("recordings");
        let storage = Arc::new(HistoryStorage::at_startup(app_data_dir.join("history.db")));
        let semantic = Arc::new(SemanticModelSlot::new(app_data_dir.join("semantic-recall")));

        // Ensure recordings directory exists
        if !recordings_dir.exists() {
            fs::create_dir_all(&recordings_dir)?;
            debug!("Created recordings directory: {:?}", recordings_dir);
        }

        let (semantic_work, semantic_inbox) = std::sync::mpsc::channel();
        let manager = Self {
            app_handle: app_handle.clone(),
            recordings_dir,
            storage: Arc::clone(&storage),
            semantic: Arc::clone(&semantic),
            semantic_work,
        };
        Self::spawn_semantic_worker(storage, semantic, semantic_inbox);

        // Initialize database and run migrations synchronously, unless the file
        // is encrypted and still waiting for its key. `unlock_storage` runs
        // migrations for that one, off the startup critical path.
        if manager.storage.is_ready() {
            manager.init_database()?;
        }

        Ok(manager)
    }

    /// The one thread that writes embeddings.
    ///
    /// It exists so the save path can hand off a row and return: embedding is
    /// pure CPU work on a 30 MB table, and a receipt write must not wait for
    /// it. Serializing every vector write onto one thread also means the
    /// backfill and the per-save embeddings cannot interleave into the same
    /// connection from two directions.
    ///
    /// The worker blocks on the channel, so it costs nothing while idle, and
    /// exits when the manager drops the sender.
    fn spawn_semantic_worker(
        storage: Arc<HistoryStorage>,
        semantic: Arc<SemanticModelSlot>,
        inbox: Receiver<SemanticWork>,
    ) {
        // Failing to spawn a thread at manager construction means the process
        // cannot create threads at all, so there is no degraded mode to fall
        // back to and nothing here can recover.
        // PANIC: the process is already unusable if this fails.
        std::thread::Builder::new()
            .name("sona-history-semantic".to_string())
            .spawn(move || {
                while let Ok(work) = inbox.recv() {
                    // Checked per item, not once: the model can appear between
                    // two saves, and when it does the very next row is embedded
                    // and the backfill catches up the rest.
                    let Some(model) = semantic.model() else {
                        continue;
                    };
                    if let Err(error) = Self::embed_queued_row(&storage, &model, &work) {
                        warn!(
                            "History entry {} was not embedded: {error:#}",
                            work.history_id
                        );
                    }
                    Self::run_semantic_backfill(&storage, &model);
                }
                debug!("History semantic worker stopped");
            })
            .expect("spawn history semantic worker");
    }

    fn embed_queued_row(
        storage: &HistoryStorage,
        model: &SemanticModel,
        work: &SemanticWork,
    ) -> Result<()> {
        let vector = model.encode(&work.text);
        storage.with_connection(|conn| {
            store_semantic_vector_with_connection(
                conn,
                work.history_id,
                vector.as_deref(),
                model.revision(),
            )
        })
    }

    /// Bring every row up to the loaded model, one bounded chunk per
    /// connection acquisition.
    ///
    /// Resumability needs no bookkeeping: the selection predicate is the
    /// progress marker. A row is done when its `semantic_model_revision`
    /// equals the model's, whether it produced a vector or not, so an
    /// interrupted pass simply resumes where the rows say it stopped, and a
    /// model change re-enters every row exactly once.
    fn run_semantic_backfill(storage: &HistoryStorage, model: &SemanticModel) {
        loop {
            let outcome = storage.with_connection(|conn| {
                backfill_semantic_chunk_with_connection(conn, model, semantic::BACKFILL_CHUNK_ROWS)
            });
            match outcome {
                Ok(0) => return,
                Ok(rows) => debug!("Embedded {rows} history rows"),
                Err(error) => {
                    warn!("History semantic backfill stopped: {error:#}");
                    return;
                }
            }
        }
    }

    /// Queue one row for embedding. Never blocks and never fails the caller:
    /// the send is a pointer handoff, and a dead worker only means this row
    /// waits for the backfill.
    fn queue_semantic_embedding(&self, history_id: i64, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        if self
            .semantic_work
            .send(SemanticWork {
                history_id,
                text: text.to_string(),
            })
            .is_err()
        {
            debug!("History semantic worker is gone; entry {history_id} awaits backfill");
        }
    }

    /// Resolve the storage key, encrypt the database if it is still plaintext,
    /// and bring the schema to the latest migration.
    ///
    /// The app calls this once from its startup callback, after the window is
    /// up, because reading the OS credential store can block behind a system
    /// prompt and a prompt needs a surface to belong to. A process with no
    /// window reaches it instead through [`Self::ensure_storage_unlocked`] on
    /// the read itself. Either way the credential-store round trip happens at
    /// most once, because [`HistoryStorage::unlock`] claims the resolution
    /// under its own state lock and hands every later caller the current
    /// status.
    pub async fn unlock_storage(&self, secrets: &crate::secrets::SecretManager) {
        let status = self
            .storage
            .unlock(secrets, Utc::now().timestamp_millis())
            .await;
        if let Err(error) = self.init_database() {
            error!("History database is unavailable: {error:#}");
        }
        if let Err(error) = self.app_handle.emit(HISTORY_STORAGE_EVENT, &status) {
            error!("Failed to emit {HISTORY_STORAGE_EVENT} event: {error}");
        }
    }

    /// Resolve the storage key on the read path when nothing else will.
    ///
    /// [`Self::unlock_storage`] runs from the app's startup callback, where a
    /// credential-store prompt has a window to belong to. The headless corpus
    /// verbs have neither that callback nor that window: they mount this
    /// manager and read immediately, so the read is the only thing left that
    /// can resolve the key. Without this the state stays unresolved and every
    /// dictation read reports a locked database.
    ///
    /// The guard is a fast path, not the safety: [`HistoryStorage::unlock`]
    /// claims the resolution under the state lock, so two searches arriving
    /// together still cost one credential-store round trip. What the guard
    /// buys is that a search in the app, where startup already resolved the
    /// key, pays one state lock and no migration check.
    async fn ensure_storage_unlocked(&self) {
        if !self.storage.needs_resolution() {
            return;
        }
        let Some(secrets) = self
            .app_handle
            .try_state::<Arc<crate::secrets::SecretManager>>()
        else {
            error!("Dictation history cannot be unlocked: no secret manager is mounted");
            return;
        };
        self.unlock_storage(&secrets).await;
    }

    /// Whether dictation history is encrypted at rest right now.
    pub fn storage_status(&self) -> HistoryStorageStatus {
        self.storage.status()
    }

    /// Whether the dictation store can be opened right now. The query plane
    /// asks after a failed read, to tell a corpus it cannot open apart from a
    /// read that broke.
    pub(crate) fn storage_is_ready(&self) -> bool {
        self.storage.is_ready()
    }

    fn init_database(&self) -> Result<()> {
        info!("Initializing database at {:?}", self.storage.path());

        self.storage.with_connection(|conn| {
            // Handle migration from tauri-plugin-sql to rusqlite_migration
            // tauri-plugin-sql used _sqlx_migrations table, rusqlite_migration uses user_version pragma
            self.migrate_from_tauri_plugin_sql(conn)?;

            // Create migrations object and run to latest version
            let migrations = Migrations::new(MIGRATIONS.to_vec());

            // Validate migrations in debug builds without turning a malformed
            // checked-in migration into an unexplained process panic.
            #[cfg(debug_assertions)]
            migrations
                .validate()
                .map_err(|error| anyhow!("Invalid migrations: {error}"))?;

            // Get current version before migration
            let version_before: i32 =
                conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
            debug!("Database version before migration: {}", version_before);

            // Apply any pending migrations
            migrations.to_latest(conn)?;

            // Get version after migration
            let version_after: i32 =
                conn.pragma_query_value(None, "user_version", |row| row.get(0))?;

            if version_after > version_before {
                info!(
                    "Database migrated from version {} to {}",
                    version_before, version_after
                );
            } else {
                debug!("Database already at latest version {}", version_after);
            }

            Ok(())
        })
    }

    /// Migrate from tauri-plugin-sql's migration tracking to rusqlite_migration's.
    /// tauri-plugin-sql used a _sqlx_migrations table, while rusqlite_migration uses
    /// SQLite's user_version pragma. This function checks if the old system was in use
    /// and sets the user_version accordingly so migrations don't re-run.
    fn migrate_from_tauri_plugin_sql(&self, conn: &Connection) -> Result<()> {
        // Check if the old _sqlx_migrations table exists
        let has_sqlx_migrations: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='_sqlx_migrations'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if !has_sqlx_migrations {
            return Ok(());
        }

        // Check current user_version
        let current_version: i32 =
            conn.pragma_query_value(None, "user_version", |row| row.get(0))?;

        if current_version > 0 {
            // Already migrated to rusqlite_migration system
            return Ok(());
        }

        // Get the highest version from the old migrations table
        let old_version: i32 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM _sqlx_migrations WHERE success = 1",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if old_version > 0 {
            info!(
                "Migrating from tauri-plugin-sql (version {}) to rusqlite_migration",
                old_version
            );

            // Set user_version to match the old migration state
            conn.pragma_update(None, "user_version", old_version)?;

            // Optionally drop the old migrations table (keeping it doesn't hurt)
            // conn.execute("DROP TABLE IF EXISTS _sqlx_migrations", [])?;

            info!(
                "Migration tracking converted: user_version set to {}",
                old_version
            );
        }

        Ok(())
    }

    fn map_history_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryEntry> {
        Ok(HistoryEntry {
            id: row.get("id")?,
            file_name: row.get("file_name")?,
            timestamp: row.get("timestamp")?,
            saved: row.get("saved")?,
            title: row.get("title")?,
            transcription_text: row.get("transcription_text")?,
            post_processed_text: row.get("post_processed_text")?,
            post_process_requested: row.get("post_process_requested")?,
            parent_id: row.get("parent_id")?,
            match_kind: None,
        })
    }

    pub fn recordings_dir(&self) -> &std::path::Path {
        &self.recordings_dir
    }
    /// Insert one validated upstream row through this manager's schema. It
    /// preserves source timestamps and text while letting SQLite allocate fresh
    /// IDs, fire FTS triggers, and keep receipt foreign keys local to this DB.
    pub(crate) fn import_upstream_entry(
        &self,
        source_identity: &str,
        entry: &UpstreamHistoryImportEntry,
        file_name: &str,
        has_audio: bool,
    ) -> Result<UpstreamHistoryImportOutcome> {
        // The connection is released before the event: the emit is the one part
        // of this that does not touch the database.
        let outcome = self.storage.with_connection(|conn| {
            Self::import_upstream_entry_with_connection(
                conn,
                source_identity,
                entry,
                file_name,
                has_audio,
            )
        })?;
        if let UpstreamHistoryImportOutcome::Inserted { history_id } = outcome {
            let event = HistoryEntry {
                id: history_id,
                file_name: file_name.to_string(),
                timestamp: entry.timestamp,
                saved: entry.saved,
                title: entry.title.clone(),
                transcription_text: entry.transcription_text.clone(),
                post_processed_text: entry.post_processed_text.clone(),
                post_process_requested: entry.post_process_requested,
                parent_id: None,
                match_kind: None,
            };
            self.queue_semantic_embedding(
                history_id,
                semantic::embeddable_text(
                    &entry.transcription_text,
                    entry.post_processed_text.as_deref(),
                ),
            );
            if let Err(error) =
                (HistoryUpdatePayload::Added { entry: event }).emit(&self.app_handle)
            {
                error!("Failed to emit imported history update: {error}");
            }
        }
        Ok(outcome)
    }

    pub(crate) fn import_upstream_entry_with_connection(
        conn: &mut Connection,
        source_identity: &str,
        entry: &UpstreamHistoryImportEntry,
        file_name: &str,
        has_audio: bool,
    ) -> Result<UpstreamHistoryImportOutcome> {
        if !is_sha256_hex(source_identity) || !is_sha256_hex(&entry.source_row_sha256) {
            return Err(anyhow!("Invalid upstream import identity"));
        }
        let transaction = conn.transaction()?;
        let outcome = Self::import_upstream_entry_with_transaction(
            &transaction,
            source_identity,
            entry,
            file_name,
            has_audio,
        )?;
        transaction.commit()?;
        Ok(outcome)
    }

    fn import_upstream_entry_with_transaction(
        transaction: &rusqlite::Transaction<'_>,
        source_identity: &str,
        entry: &UpstreamHistoryImportEntry,
        file_name: &str,
        has_audio: bool,
    ) -> Result<UpstreamHistoryImportOutcome> {
        if let Some((history_id, existing_has_audio)) = transaction
            .query_row(
                "SELECT h.id, h.has_audio
                 FROM upstream_import_history AS imported
                 JOIN transcription_history AS h ON h.id = imported.history_id
                 WHERE imported.source_identity = ?1
                   AND imported.source_history_id = ?2",
                params![source_identity, entry.source_history_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, bool>(1)?)),
            )
            .optional()?
        {
            if has_audio && !existing_has_audio {
                transaction.execute(
                    "UPDATE transcription_history
                     SET file_name = ?1, has_audio = 1
                     WHERE id = ?2",
                    params![file_name, history_id],
                )?;
            }
            return Ok(UpstreamHistoryImportOutcome::Existing { history_id });
        }

        transaction.execute(
            "INSERT INTO transcription_history (
                file_name,
                timestamp,
                saved,
                title,
                transcription_text,
                post_processed_text,
                post_process_requested,
                duration_ms,
                word_count,
                source_kind,
                has_audio
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                file_name,
                entry.timestamp,
                entry.saved,
                &entry.title,
                &entry.transcription_text,
                &entry.post_processed_text,
                entry.post_process_requested,
                entry.duration_ms.map(as_sql_i64).transpose()?,
                entry.word_count.map(as_sql_i64).transpose()?,
                entry.source_kind.map(HistorySourceKind::as_str),
                has_audio,
            ],
        )?;
        let history_id = transaction.last_insert_rowid();
        Self::insert_upstream_run_receipts(transaction, history_id, entry)?;
        transaction.execute(
            "INSERT INTO upstream_import_history (
                source_identity,
                source_history_id,
                source_row_sha256,
                history_id
            ) VALUES (?1, ?2, ?3, ?4)",
            params![
                source_identity,
                entry.source_history_id,
                &entry.source_row_sha256,
                history_id,
            ],
        )?;
        Ok(UpstreamHistoryImportOutcome::Inserted { history_id })
    }

    fn insert_upstream_run_receipts(
        transaction: &rusqlite::Transaction<'_>,
        history_id: i64,
        entry: &UpstreamHistoryImportEntry,
    ) -> Result<()> {
        let mut imported_runs = HashMap::with_capacity(entry.runs.len());
        for run in &entry.runs {
            let mode_json = serde_json::to_string(&run.mode)?;
            let context_json = serde_json::to_string(&run.context)?;
            let legacy_method = serde_json::to_string(&DeliveryMethod::None)?;
            let legacy_outcome = serde_json::to_string(&DeliveryOutcome::DefinitelyNotDispatched)?;
            transaction.execute(
                "INSERT INTO transcription_runs (
                    history_id, run_id, retry_of_run_id, started_at_ms, completed_at_ms,
                    mode_id, mode_revision, mode_receipt_json, context_receipt_json,
                    delivery_method, delivery_outcome, delivery_dispatched_at_ms
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    history_id,
                    as_sql_i64(run.run_id)?,
                    run.retry_of_run_id.map(as_sql_i64).transpose()?,
                    as_sql_i64(run.started_at_ms)?,
                    as_sql_i64(run.completed_at_ms)?,
                    &run.mode.mode_id,
                    as_sql_i64(run.mode.settings_revision)?,
                    mode_json,
                    context_json,
                    legacy_method,
                    legacy_outcome,
                    as_sql_i64(run.completed_at_ms)?,
                ],
            )?;
            imported_runs.insert(run.source_run_receipt_id, transaction.last_insert_rowid());
        }

        for run in &entry.runs {
            let Some(run_receipt_id) = imported_runs.get(&run.source_run_receipt_id) else {
                continue;
            };
            for delivery in &run.deliveries {
                transaction.execute(
                    "INSERT INTO delivery_attempts (
                        history_id,
                        run_receipt_id,
                        delivery_method,
                        delivery_outcome,
                        dispatched_at_ms
                    ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        history_id,
                        run_receipt_id,
                        serde_json::to_string(&delivery.delivery.method)?,
                        serde_json::to_string(&delivery.delivery.outcome)?,
                        as_sql_i64(delivery.delivery.dispatched_at_ms)?,
                    ],
                )?;
            }
        }
        Ok(())
    }

    /// Saves a history entry and its immutable run receipt before any delivery
    /// side effect is attempted. Prompt bodies are intentionally never accepted
    /// by this API, so a caller cannot accidentally persist one.
    ///
    /// `None` is saved history turned off: nothing was written, the recording
    /// the caller put on disk is gone, and there is no row for a delivery
    /// receipt to attach to.
    pub fn save_entry_with_receipt(
        &self,
        file_name: String,
        transcription_text: String,
        post_process_requested: bool,
        post_processed_text: Option<String>,
        receipt: Option<NewRunReceipt>,
    ) -> Result<Option<HistoryEntry>> {
        self.save_entry_with_receipt_internal(
            file_name,
            transcription_text,
            post_process_requested,
            post_processed_text,
            None,
            receipt,
        )
    }

    /// A retry or a reprocess creates a new immutable history row and child run
    /// receipt, linked back to the row whose recording it reused. The original
    /// transcription and its prior receipts remain untouched. `None` is saved
    /// history turned off, and the parent's recording is left alone.
    pub fn save_derived_entry_with_receipt(
        &self,
        derived_from_history_id: i64,
        file_name: String,
        transcription_text: String,
        post_process_requested: bool,
        post_processed_text: Option<String>,
        receipt: NewRunReceipt,
    ) -> Result<Option<HistoryEntry>> {
        self.save_entry_with_receipt_internal(
            file_name,
            transcription_text,
            post_process_requested,
            post_processed_text,
            Some(derived_from_history_id),
            Some(receipt),
        )
    }

    fn save_entry_with_receipt_internal(
        &self,
        file_name: String,
        transcription_text: String,
        post_process_requested: bool,
        post_processed_text: Option<String>,
        derived_from_history_id: Option<i64>,
        receipt: Option<NewRunReceipt>,
    ) -> Result<Option<HistoryEntry>> {
        // "Dictations to keep" at zero is saved history turned off. Nothing is
        // written, not even the receipt. A fresh recording goes with the
        // refusal, since a recording without a row is one nobody can reach; a
        // derived row reuses its parent's file, which is not this call's to
        // remove.
        if crate::settings::get_settings(&self.app_handle).history_limit == 0 {
            if derived_from_history_id.is_none() {
                Self::remove_recording_file(&self.recordings_dir, &file_name)?;
            }
            debug!("Saved history is off; the dictation was not kept");
            return Ok(None);
        }
        let timestamp = Utc::now().timestamp();
        let title = self.format_timestamp_title(timestamp);
        // The insert owns the connection only until it commits. `cleanup_old_entries`
        // below takes the same single connection, and the emit after it must not
        // run while a database lock is held.
        let history_id = self.storage.with_connection(|conn| {
            Self::save_entry_with_receipt_with_connection(
                conn,
                &file_name,
                timestamp,
                &title,
                &transcription_text,
                post_process_requested,
                post_processed_text.as_deref(),
                derived_from_history_id,
                receipt.as_ref(),
            )
        })?;
        let entry = HistoryEntry {
            id: history_id,
            file_name,
            timestamp,
            saved: false,
            title,
            transcription_text,
            post_processed_text,
            post_process_requested,
            parent_id: derived_from_history_id,
            match_kind: None,
        };

        debug!("Saved history entry with id {}", entry.id);
        // After the commit, before the emit: the receipt is already durable, and
        // this is a channel send, not a database write.
        self.queue_semantic_embedding(
            entry.id,
            semantic::embeddable_text(
                &entry.transcription_text,
                entry.post_processed_text.as_deref(),
            ),
        );
        if let Err(error) = (HistoryUpdatePayload::Added {
            entry: entry.clone(),
        })
        .emit(&self.app_handle)
        {
            error!("Failed to emit history-updated event: {error}");
        }
        // After the announcement, so a listener already holds the row when the
        // sweep reports what it took from it.
        self.cleanup_old_entries()?;
        // The one place dictation history is established as having moved, and so
        // the one place the local learning loops are woken. The call is
        // day-bucketed on the other side, so this is cheap per dictation.
        crate::meeting::learning::notify_dictation_history_changed(&self.app_handle);
        Ok(Some(entry))
    }

    fn save_entry_with_receipt_with_connection(
        conn: &mut Connection,
        file_name: &str,
        timestamp: i64,
        title: &str,
        transcription_text: &str,
        post_process_requested: bool,
        post_processed_text: Option<&str>,
        derived_from_history_id: Option<i64>,
        receipt: Option<&NewRunReceipt>,
    ) -> Result<i64> {
        let transaction = conn.transaction()?;
        let derived_from_run_id = derived_from_history_id
            .map(|history_id| Self::latest_run_id(&transaction, history_id))
            .transpose()?
            .flatten();
        let (duration_ms, word_count, source_kind, has_audio) = match receipt {
            Some(receipt) => (
                receipt.duration_ms.map(as_sql_i64).transpose()?,
                receipt.word_count.map(as_sql_i64).transpose()?,
                Some(receipt.source_kind.as_str()),
                receipt.has_audio,
            ),
            None => (None, None, None, true),
        };
        transaction.execute(
            "INSERT INTO transcription_history (
                file_name,
                timestamp,
                saved,
                title,
                transcription_text,
                post_processed_text,
                post_process_requested,
                duration_ms,
                word_count,
                source_kind,
                has_audio,
                parent_id
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                file_name,
                timestamp,
                false,
                title,
                transcription_text,
                post_processed_text,
                post_process_requested,
                duration_ms,
                word_count,
                source_kind,
                has_audio,
                derived_from_history_id,
            ],
        )?;
        let history_id = transaction.last_insert_rowid();
        if let Some(receipt) = receipt {
            Self::insert_run_receipt(&transaction, history_id, derived_from_run_id, receipt)?;
        }
        transaction.commit()?;
        Ok(history_id)
    }

    /// Appends the one result of a delivery dispatch. It looks up the exact
    /// frozen run rather than a mutable "latest status" column.
    pub fn append_delivery_attempt(
        &self,
        history_id: i64,
        run_id: u64,
        delivery: DeliveryReceipt,
    ) -> Result<HistoryDeliveryAttempt> {
        let (id, run_receipt_id) = self.storage.with_connection(|conn| {
            let transaction = conn.transaction()?;
            let run_receipt_id: i64 = transaction
                .query_row(
                    "SELECT id FROM transcription_runs
                     WHERE history_id = ?1 AND run_id = ?2
                     ORDER BY id DESC LIMIT 1",
                    params![history_id, as_sql_i64(run_id)?],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or_else(|| {
                    anyhow!("Run {} for history entry {} not found", run_id, history_id)
                })?;
            let id =
                Self::insert_delivery_attempt(&transaction, history_id, run_receipt_id, &delivery)?;
            transaction.commit()?;
            Ok((id, run_receipt_id))
        })?;
        Ok(HistoryDeliveryAttempt {
            id,
            history_id,
            run_receipt_id,
            delivery,
        })
    }

    fn latest_run_id(conn: &Connection, history_id: i64) -> Result<Option<u64>> {
        conn.query_row(
            "SELECT run_id FROM transcription_runs WHERE history_id = ?1 ORDER BY id DESC LIMIT 1",
            params![history_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .map(u64::try_from)
        .transpose()
        .map_err(|_| anyhow!("Stored run id is negative"))
    }

    fn insert_run_receipt(
        conn: &Connection,
        history_id: i64,
        retry_of_run_id: Option<u64>,
        receipt: &NewRunReceipt,
    ) -> Result<i64> {
        let mode_json = serde_json::to_string(&receipt.run)?;
        let context_json = serde_json::to_string(&receipt.context)?;
        // The first interrupted migration stored delivery columns on the run
        // row. New data uses `delivery_attempts`; these content-free sentinels
        // preserve compatibility with databases that already applied it.
        let legacy_method = serde_json::to_string(&DeliveryMethod::None)?;
        let legacy_outcome = serde_json::to_string(&DeliveryOutcome::DefinitelyNotDispatched)?;
        conn.execute(
            "INSERT INTO transcription_runs (
                history_id, run_id, retry_of_run_id, started_at_ms, completed_at_ms,
                mode_id, mode_revision, mode_receipt_json, context_receipt_json,
                delivery_method, delivery_outcome, delivery_dispatched_at_ms,
                capture_status
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                history_id,
                as_sql_i64(receipt.run.run_id)?,
                retry_of_run_id.map(as_sql_i64).transpose()?,
                as_sql_i64(receipt.started_at_ms)?,
                as_sql_i64(receipt.completed_at_ms)?,
                &receipt.run.mode_id,
                as_sql_i64(receipt.run.settings_revision)?,
                mode_json,
                context_json,
                legacy_method,
                legacy_outcome,
                as_sql_i64(receipt.completed_at_ms)?,
                receipt.capture_status.map(CaptureStatus::as_str),
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    fn insert_delivery_attempt(
        conn: &Connection,
        history_id: i64,
        run_receipt_id: i64,
        delivery: &DeliveryReceipt,
    ) -> Result<i64> {
        conn.execute(
            "INSERT INTO delivery_attempts (
                history_id, run_receipt_id, delivery_method, delivery_outcome, dispatched_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                history_id,
                run_receipt_id,
                serde_json::to_string(&delivery.method)?,
                serde_json::to_string(&delivery.outcome)?,
                as_sql_i64(delivery.dispatched_at_ms)?,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub async fn get_run_receipts(&self, history_id: i64) -> Result<Vec<HistoryRunReceipt>> {
        self.storage
            .with_connection(|conn| Self::get_run_receipts_with_connection(conn, history_id))
    }

    fn get_run_receipts_with_connection(
        conn: &Connection,
        history_id: i64,
    ) -> Result<Vec<HistoryRunReceipt>> {
        let mut statement = conn.prepare(
            "SELECT
                r.id,
                r.history_id,
                r.run_id,
                r.retry_of_run_id,
                r.started_at_ms,
                r.completed_at_ms,
                r.mode_receipt_json,
                r.context_receipt_json,
                h.duration_ms,
                h.word_count,
                h.source_kind,
                h.has_audio,
                r.capture_status
             FROM transcription_runs AS r
             JOIN transcription_history AS h ON h.id = r.history_id
             WHERE r.history_id = ?1
             ORDER BY r.id ASC",
        )?;
        let rows = statement.query_map(params![history_id], map_run_receipt)?;
        let mut receipts = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);
        for receipt in &mut receipts {
            receipt.delivery_attempts =
                Self::get_delivery_attempts_with_connection(conn, receipt.id)?;
        }
        Ok(receipts)
    }
    /// One bounded page of dictation runs newer than `after`, oldest first.
    ///
    /// This is the only way the local learning loops see dictation history, and
    /// it is read-only by construction. `limit` is not a courtesy: the caller
    /// mines this page while holding a write transaction on a different
    /// database, so an unbounded scan here would stall live captures there.
    pub fn dictation_runs_after(&self, after: i64, limit: usize) -> Result<Vec<DictationRunRow>> {
        let limit = as_sql_i64(u64::try_from(limit).unwrap_or(0))?;
        self.storage.with_connection(|conn| {
            let mut statement = conn.prepare(
                "SELECT
                    r.id,
                    r.completed_at_ms,
                    COALESCE(h.post_processed_text, h.transcription_text),
                    r.mode_receipt_json,
                    r.capture_status,
                    r.retry_of_run_id IS NOT NULL OR h.parent_id IS NOT NULL
                 FROM transcription_runs AS r
                 JOIN transcription_history AS h ON h.id = r.history_id
                 WHERE r.id > ?1
                 ORDER BY r.id ASC
                 LIMIT ?2",
            )?;
            let rows = statement.query_map(params![after, limit], |row| {
                Ok(DictationRunRow {
                    id: row.get(0)?,
                    completed_at_ms: row.get(1)?,
                    delivered_text: row.get(2)?,
                    mode: decode_receipt_json(row.get::<_, String>(3)?, 3)?,
                    capture_status: CaptureStatus::from_stored(row.get(4)?),
                    is_retry: row.get(5)?,
                })
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Into::into)
        })
    }

    fn get_delivery_attempts_with_connection(
        conn: &Connection,
        run_receipt_id: i64,
    ) -> Result<Vec<HistoryDeliveryAttempt>> {
        let mut statement = conn.prepare(
            "SELECT id, history_id, run_receipt_id, delivery_method, delivery_outcome, dispatched_at_ms
             FROM delivery_attempts
             WHERE run_receipt_id = ?1
             ORDER BY id ASC",
        )?;
        let rows = statement.query_map(params![run_receipt_id], map_delivery_attempt)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Apply both policies the Library offers, each on its own terms.
    ///
    /// "Dictations to keep" is a count: the newest `history_limit` unsaved rows
    /// survive and the rest go, words and recording alike. "Recordings" is an
    /// expiry on audio alone: a row past it keeps its words and loses its
    /// recording. The two never read each other, so a zero limit empties the
    /// unsaved log under every audio choice, and "not kept" strips audio from
    /// every unsaved row the count let live. A saved row is outside both.
    ///
    /// Both run on one connection, and the rows they touched are announced
    /// once it is released, so an open Library mirrors the sweep instead of
    /// finding out at its next reload.
    pub fn cleanup_old_entries(&self) -> Result<()> {
        let settings = crate::settings::get_settings(&self.app_handle);
        let limit = settings.history_limit;
        let expiry =
            recording_expiry_cutoff(settings.recording_retention_period, Utc::now().timestamp());

        let (deleted, expired) = self.storage.with_connection(|conn| {
            let deleted = Self::sweep_by_count_with_connection(conn, &self.recordings_dir, limit)?;
            let expired = match expiry {
                Some(cutoff) => {
                    Self::expire_recordings_with_connection(conn, &self.recordings_dir, cutoff)?
                }
                None => Vec::new(),
            };
            Ok((deleted, expired))
        })?;

        if !deleted.is_empty() {
            debug!("Retention removed {} history entries", deleted.len());
        }
        if !expired.is_empty() {
            debug!("Retention removed {} recordings", expired.len());
        }
        for id in deleted {
            if let Err(error) = (HistoryUpdatePayload::Deleted { id }).emit(&self.app_handle) {
                error!("Failed to emit history-updated event: {error}");
            }
        }
        for entry in expired {
            if let Err(error) = (HistoryUpdatePayload::Updated { entry }).emit(&self.app_handle) {
                error!("Failed to emit history-updated event: {error}");
            }
        }
        Ok(())
    }

    fn remove_recording_file(recordings_dir: &Path, file_name: &str) -> Result<()> {
        // A stored name that is not a bare file name references nothing this
        // app owns, so there is no file to delete and nothing outside the
        // recordings directory may be touched. The history row still goes.
        let Some(path) = recording_path(recordings_dir, file_name) else {
            error!("Refusing to delete recording outside the recordings directory: {file_name}");
            return Ok(());
        };
        match fs::remove_file(path) {
            Ok(()) => {
                debug!("Deleted WAV file: {}", file_name);
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(anyhow!(
                "Failed to delete WAV file {}: {}",
                file_name,
                error
            )),
        }
    }

    /// Whether another row still holds this recording. A retry or a reprocess
    /// reuses its parent's file, and a row whose audio expired keeps the name
    /// without the file, so only a row that still has audio counts.
    fn has_other_recording_reference(
        conn: &Connection,
        history_id: i64,
        file_name: &str,
    ) -> Result<bool> {
        conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM transcription_history
                WHERE file_name = ?1 AND id != ?2 AND has_audio = 1
            )",
            params![file_name, history_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    /// Delete the rows and the recordings only they hold; returns the ids that
    /// went. A row whose recording refuses to go stays so a later sweep can
    /// retry it.
    fn delete_entries_and_files_with_connection(
        conn: &Connection,
        recordings_dir: &Path,
        entries: &[(i64, String)],
    ) -> Result<Vec<i64>> {
        let mut deleted = Vec::with_capacity(entries.len());

        for (id, file_name) in entries {
            if !Self::has_other_recording_reference(conn, *id, file_name)? {
                if let Err(error) = Self::remove_recording_file(recordings_dir, file_name) {
                    error!(
                        "Failed to delete WAV file {} for history entry {}; retaining it for retry: {}",
                        file_name, id, error
                    );
                    continue;
                }
            }

            if conn.execute(
                "DELETE FROM transcription_history WHERE id = ?1",
                params![id],
            )? > 0
            {
                deleted.push(*id);
            }
        }

        Ok(deleted)
    }

    /// The oldest unsaved rows beyond `limit`, deleted on the connection that
    /// selected them, so the rows that go are exactly the rows that were
    /// chosen. Reading them on one connection and deleting them on another let
    /// a row change in between.
    ///
    /// Separate from the settings read above so the selection is reachable
    /// from a test: which rows are chosen is the half of retention that needs
    /// coverage, and it is not the half that needs an `AppHandle`.
    fn sweep_by_count_with_connection(
        conn: &Connection,
        recordings_dir: &Path,
        limit: usize,
    ) -> Result<Vec<i64>> {
        // Every unsaved row, newest first, so the survivors are a prefix.
        let entries: Vec<(i64, String)> = {
            let mut stmt = conn.prepare(
                "SELECT id, file_name FROM transcription_history WHERE saved = 0 ORDER BY timestamp DESC"
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, i64>("id")?, row.get::<_, String>("file_name")?))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        if entries.len() <= limit {
            return Ok(Vec::new());
        }
        Self::delete_entries_and_files_with_connection(conn, recordings_dir, &entries[limit..])
    }

    /// Take the recording from every unsaved row older than `cutoff` and leave
    /// the row, with `has_audio` cleared so the Library stops offering a play
    /// control it cannot honour. Returns the rows as they now read.
    ///
    /// The file itself goes only when no other row still holds it: a retry or a
    /// reprocess shares its parent's recording, and the child may be saved or
    /// younger than the cutoff. A file that refuses to go keeps its row's
    /// `has_audio` so a later sweep retries it.
    fn expire_recordings_with_connection(
        conn: &Connection,
        recordings_dir: &Path,
        cutoff_timestamp: i64,
    ) -> Result<Vec<HistoryEntry>> {
        let entries: Vec<(i64, String)> = {
            let mut stmt = conn.prepare(
                "SELECT id, file_name FROM transcription_history
                 WHERE saved = 0 AND has_audio = 1 AND timestamp < ?1",
            )?;
            let rows = stmt.query_map(params![cutoff_timestamp], |row| {
                Ok((row.get::<_, i64>("id")?, row.get::<_, String>("file_name")?))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        let mut expired = Vec::with_capacity(entries.len());
        for (id, file_name) in &entries {
            // The row loses its claim first, so the reference check below sees
            // only rows that still hold the file.
            conn.execute(
                "UPDATE transcription_history SET has_audio = 0 WHERE id = ?1",
                params![id],
            )?;
            if !Self::has_other_recording_reference(conn, *id, file_name)? {
                if let Err(error) = Self::remove_recording_file(recordings_dir, file_name) {
                    error!(
                        "Failed to delete expired WAV file {} for history entry {}; retaining it for retry: {}",
                        file_name, id, error
                    );
                    conn.execute(
                        "UPDATE transcription_history SET has_audio = 1 WHERE id = ?1",
                        params![id],
                    )?;
                    continue;
                }
            }
            if let Some(entry) = Self::entry_by_id_with_connection(conn, *id)? {
                expired.push(entry);
            }
        }

        Ok(expired)
    }

    /// Read all-time aggregates from the retained history rows. The one grouped
    /// query keeps totals and source subtotals on the same snapshot.
    pub async fn get_history_stats(&self) -> Result<HistoryStats> {
        self.storage
            .with_connection(|conn| Self::get_history_stats_with_connection(conn))
    }

    fn get_history_stats_with_connection(conn: &Connection) -> Result<HistoryStats> {
        let mut statement = conn.prepare(
            "SELECT
                source_kind,
                COUNT(*) AS entries,
                COALESCE(SUM(duration_ms), 0) AS total_duration_ms,
                COALESCE(SUM(word_count), 0) AS total_words
             FROM transcription_history
             GROUP BY source_kind
             ORDER BY CASE source_kind
                 WHEN 'microphone' THEN 0
                 WHEN 'file' THEN 1
                 ELSE 2
             END",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>("source_kind")?,
                row.get::<_, i64>("entries")?,
                row.get::<_, i64>("total_duration_ms")?,
                row.get::<_, i64>("total_words")?,
            ))
        })?;

        let mut stats = HistoryStats {
            entries: 0,
            total_duration_ms: 0,
            total_words: 0,
            by_source: Vec::new(),
        };
        for row in rows {
            let (stored_source_kind, entries, duration_ms, words) = row?;
            let entries = u64::try_from(entries)
                .map_err(|_| anyhow!("history statistics contain a negative entry count"))?;
            let duration_ms = u64::try_from(duration_ms)
                .map_err(|_| anyhow!("history statistics contain a negative duration"))?;
            let words = u64::try_from(words)
                .map_err(|_| anyhow!("history statistics contain a negative word count"))?;
            stats.entries = stats.entries.saturating_add(entries);
            stats.total_duration_ms = stats.total_duration_ms.saturating_add(duration_ms);
            stats.total_words = stats.total_words.saturating_add(words);
            stats.by_source.push(HistoryStatsBySource {
                source_kind: HistorySourceKind::from_stored(stored_source_kind),
                entries,
                total_duration_ms: duration_ms,
                total_words: words,
            });
        }
        Ok(stats)
    }

    /// Return a dense, bounded local-calendar trend plus an independent
    /// all-time summary. The SQL statement is the sole database read for this
    /// projection; dense zero days and streaks are derived after its short read
    /// transaction has closed.
    pub async fn get_history_trend(
        &self,
        request: DashboardTrendRequest,
    ) -> Result<HistoryTrendProjection> {
        self.storage.with_connection(|conn| {
            Self::get_history_trend_with_connection_at(conn, request, Local::now())
        })
    }

    fn get_history_trend_with_connection_at(
        connection: &mut Connection,
        request: DashboardTrendRequest,
        now: DateTime<Local>,
    ) -> Result<HistoryTrendProjection> {
        let calendar = LocalCalendarRange::at(now, request.range)?;
        let rows = {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
            let mut statement = transaction.prepare(
                "WITH normalized_sources AS (
                    SELECT
                        timestamp,
                        CASE
                            WHEN source_kind IS NULL THEN NULL
                            WHEN source_kind = 'microphone' THEN 'microphone'
                            WHEN source_kind = 'file' THEN 'file'
                            ELSE NULL
                        END AS source_kind,
                        duration_ms,
                        word_count
                     FROM transcription_history
                 ),
                 range_days AS (
                    SELECT
                        date(timestamp, 'unixepoch', 'localtime') AS local_date,
                        source_kind,
                        COUNT(*) AS recordings,
                        COALESCE(SUM(duration_ms), 0) AS duration_ms,
                        COALESCE(SUM(word_count), 0) AS words
                     FROM normalized_sources
                     WHERE timestamp >= ?1 AND timestamp < ?2
                     GROUP BY local_date, source_kind
                 ),
                 all_time_sources AS (
                    SELECT
                        source_kind,
                        COUNT(*) AS recordings,
                        COALESCE(SUM(duration_ms), 0) AS duration_ms,
                        COALESCE(SUM(word_count), 0) AS words
                     FROM normalized_sources
                     GROUP BY source_kind
                 )
                 SELECT
                    'range_day' AS projection,
                    local_date,
                    source_kind,
                    recordings,
                    duration_ms,
                    words
                 FROM range_days
                 UNION ALL
                 SELECT
                    'all_time' AS projection,
                    NULL AS local_date,
                    source_kind,
                    recordings,
                    duration_ms,
                    words
                 FROM all_time_sources",
            )?;
            let rows = statement.query_map(
                params![
                    calendar.start_utc_seconds(),
                    calendar.end_exclusive_utc_seconds()
                ],
                |row| {
                    Ok((
                        row.get::<_, String>("projection")?,
                        row.get::<_, Option<String>>("local_date")?,
                        row.get::<_, Option<String>>("source_kind")?,
                        row.get::<_, i64>("recordings")?,
                        row.get::<_, i64>("duration_ms")?,
                        row.get::<_, i64>("words")?,
                    ))
                },
            )?;
            let collected = rows.collect::<std::result::Result<Vec<_>, _>>()?;
            drop(statement);
            transaction.commit()?;
            collected
        };

        let mut daily_values = HashMap::<String, [HistoryTrendValues; 3]>::new();
        let mut all_time_values = [HistoryTrendValues::default(); 3];
        for row in rows {
            let (projection, local_date, source_kind, recordings, duration_ms, words) = row;
            let values = HistoryTrendValues {
                recordings: history_trend_value(recordings, "recording count")?,
                duration_ms: history_trend_value(duration_ms, "duration")?,
                words: history_trend_value(words, "word count")?,
            };
            let slot = history_trend_source_slot(source_kind.as_deref())?;
            match projection.as_str() {
                "range_day" => {
                    let local_date = local_date
                        .ok_or_else(|| anyhow!("history trend range row has no local date"))?;
                    daily_values.entry(local_date).or_default()[slot].add(values)?;
                }
                "all_time" => all_time_values[slot].add(values)?,
                _ => {
                    return Err(anyhow!(
                        "history trend query returned an unknown projection"
                    ))
                }
            }
        }

        let mut range_values = [HistoryTrendValues::default(); 3];
        let mut active_days = 0_u16;
        let mut points = Vec::with_capacity(request.range.days());
        for date in calendar.local_dates()? {
            let values = daily_values
                .remove(&date.format("%F").to_string())
                .unwrap_or([HistoryTrendValues::default(); 3]);
            for (slot, value) in values.iter().copied().enumerate() {
                range_values[slot].add(value)?;
            }
            let totals = history_trend_totals(values)?;
            if totals.recordings > 0 {
                active_days = active_days
                    .checked_add(1)
                    .ok_or_else(|| anyhow!("history trend active-day count overflowed"))?;
            }
            points.push(HistoryTrendPoint {
                local_date: date.format("%F").to_string(),
                recordings: totals.recordings,
                duration_ms: totals.duration_ms,
                words: totals.words,
                by_source: totals.by_source,
            });
        }
        if !daily_values.is_empty() {
            return Err(anyhow!(
                "history trend query returned a day outside the requested local range"
            ));
        }
        let current_streak_days = u16::try_from(
            points
                .iter()
                .rev()
                .take_while(|point| point.recordings > 0)
                .count(),
        )
        .map_err(|_| anyhow!("history trend streak exceeds the supported range"))?;

        Ok(HistoryTrendProjection {
            range: request.range,
            range_start_local_date: calendar.start_local_date(),
            range_end_local_date: calendar.end_local_date(),
            all_time: history_trend_totals(all_time_values)?,
            range_total: history_trend_totals(range_values)?,
            active_days,
            current_streak_days,
            points,
        })
    }

    pub async fn get_history_entries(
        &self,
        cursor: Option<i64>,
        limit: Option<usize>,
    ) -> Result<PaginatedHistory> {
        let limit = limit.map(|l| l.min(MAX_HISTORY_PAGE_SIZE));

        self.storage.with_connection(|conn| {
            let mut entries: Vec<HistoryEntry> = match (cursor, limit) {
                (Some(cursor_id), Some(lim)) => {
                    let fetch_count = i64::try_from(lim + 1)?;
                    let mut stmt = conn.prepare(
                        "SELECT id, file_name, timestamp, saved, title, transcription_text, post_processed_text, post_process_requested, parent_id
                         FROM transcription_history
                         WHERE id < ?1
                         ORDER BY id DESC
                         LIMIT ?2",
                    )?;
                    let result = stmt
                        .query_map(params![cursor_id, fetch_count], Self::map_history_entry)?
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    result
                }
                (None, Some(lim)) => {
                    let fetch_count = i64::try_from(lim + 1)?;
                    let mut stmt = conn.prepare(
                        "SELECT id, file_name, timestamp, saved, title, transcription_text, post_processed_text, post_process_requested, parent_id
                         FROM transcription_history
                         ORDER BY id DESC
                         LIMIT ?1",
                    )?;
                    let result = stmt
                        .query_map(params![fetch_count], Self::map_history_entry)?
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    result
                }
                (_, None) => {
                    let mut stmt = conn.prepare(
                        "SELECT id, file_name, timestamp, saved, title, transcription_text, post_processed_text, post_process_requested, parent_id
                         FROM transcription_history
                         ORDER BY id DESC",
                    )?;
                    let result = stmt
                        .query_map([], Self::map_history_entry)?
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    result
                }
            };

            let has_more = limit.is_some_and(|lim| entries.len() > lim);
            if has_more {
                entries.pop();
            }

            Ok(PaginatedHistory { entries, has_more })
        })
    }

    /// Search transcription history by raw or post-processed text, with
    /// semantic recall filling the space lexical search leaves.
    ///
    /// The caller passes raw user text. `fts_match_query` turns it into a
    /// quoted FTS5 expression, so search input is always data and never query
    /// syntax. Search pages are bounded even when the caller omits a limit. The
    /// cursor is the last returned history entry id and preserves the existing
    /// newest-first pagination convention.
    ///
    /// The semantic half is skipped entirely when the model is not on disk, and
    /// the first search that could have used it starts the one fetch. A search
    /// run before the model arrives, or over rows the backfill has not reached,
    /// returns exactly the lexical result it always did.
    pub async fn search_history_entries(
        &self,
        query: &str,
        cursor: Option<i64>,
        limit: Option<usize>,
    ) -> Result<PaginatedHistory> {
        self.ensure_storage_unlocked().await;
        let model = self.semantic.model();
        let page = self.storage.with_connection(|conn| {
            Self::search_history_entries_with_connection(
                conn,
                query,
                cursor,
                limit,
                model.as_deref(),
            )
        })?;
        // Demand-driven: only a search that ran out of lexical matches is
        // evidence that this user would benefit from the recall model.
        if model.is_none() && page.entries.is_empty() {
            self.semantic.ensure_fetch_started();
        }
        Ok(page)
    }

    /// The recall model, when it is already on disk and loaded.
    ///
    /// Exposed for the query plane, which embeds meeting text with the same
    /// model so one query vector can be compared against both corpora. It
    /// deliberately does not start a fetch: whether this machine downloads the
    /// model stays a decision of the search above, driven by a user who
    /// searched and found nothing.
    pub(crate) fn semantic_model(&self) -> Option<Arc<SemanticModel>> {
        self.semantic.model()
    }

    /// FTS5 first, then semantic, merged into one newest-first list.
    ///
    /// Three rules make the blend safe rather than clever:
    ///
    /// 1. **One order, not two.** The merged list stays strictly `id DESC`, the
    ///    order this search already had, so `cursor` (the last returned id)
    ///    keeps its exact meaning and the next page can neither repeat nor skip
    ///    a row. Ranking a semantic tail by similarity instead would need a
    ///    compound cursor — a wire change — and `similarity_floor`, not
    ///    position, is what keeps weak matches out.
    /// 2. **Lexical wins the row.** A row FTS5 matched is reported as
    ///    [`HistoryMatchKind::Text`] even when its embedding also clears the
    ///    floor, and is excluded from the semantic candidate set, so the two
    ///    halves can neither double-count a row nor disagree about it.
    /// 3. **A page FTS5 fills on its own is left alone.** When lexical alone
    ///    overflows the page there is no query embedding and no scan: a query
    ///    with a full page of literal matches pays nothing.
    ///
    /// Rules 1 and 3 have one honest consequence worth naming: a semantic hit
    /// newer than an overflowing page's last id is not shown for that query,
    /// because the cursor has already moved past it. Recency is the list's only
    /// order, and a query that already returns a full page of literal matches
    /// is not the query this feature exists for.
    fn search_history_entries_with_connection(
        conn: &Connection,
        query: &str,
        cursor: Option<i64>,
        limit: Option<usize>,
        semantic_model: Option<&SemanticModel>,
    ) -> Result<PaginatedHistory> {
        let Some(match_query) = fts_match_query(query) else {
            return Ok(PaginatedHistory {
                entries: Vec::new(),
                has_more: false,
            });
        };

        let limit = limit
            .unwrap_or(DEFAULT_HISTORY_SEARCH_PAGE_SIZE)
            .clamp(1, MAX_HISTORY_PAGE_SIZE);
        let fetch_count = i64::try_from(limit + 1)?;
        let mut entries: Vec<HistoryEntry> = match cursor {
            Some(cursor_id) => {
                let mut stmt = conn.prepare(
                    "SELECT
                        h.id,
                        h.file_name,
                        h.timestamp,
                        h.saved,
                        h.title,
                        h.transcription_text,
                        h.post_processed_text,
                        h.post_process_requested,
                        h.parent_id
                     FROM transcription_history_fts
                     JOIN transcription_history AS h
                       ON h.id = transcription_history_fts.rowid
                     WHERE transcription_history_fts MATCH ?1
                       AND h.id < ?2
                     ORDER BY h.id DESC
                     LIMIT ?3",
                )?;
                let rows = stmt.query_map(
                    params![match_query, cursor_id, fetch_count],
                    Self::map_history_entry,
                )?;
                rows.collect::<std::result::Result<Vec<_>, _>>()?
            }
            None => {
                let mut stmt = conn.prepare(
                    "SELECT
                        h.id,
                        h.file_name,
                        h.timestamp,
                        h.saved,
                        h.title,
                        h.transcription_text,
                        h.post_processed_text,
                        h.post_process_requested,
                        h.parent_id
                     FROM transcription_history_fts
                     JOIN transcription_history AS h
                       ON h.id = transcription_history_fts.rowid
                     WHERE transcription_history_fts MATCH ?1
                     ORDER BY h.id DESC
                     LIMIT ?2",
                )?;
                let rows =
                    stmt.query_map(params![match_query, fetch_count], Self::map_history_entry)?;
                rows.collect::<std::result::Result<Vec<_>, _>>()?
            }
        };
        for entry in &mut entries {
            entry.match_kind = Some(HistoryMatchKind::Text);
        }

        // A full lexical page means there is no room to fill, and the next page
        // will re-decide with its own cursor.
        let lexical_filled_the_page = entries.len() > limit;
        if let (Some(model), false) = (semantic_model, lexical_filled_the_page) {
            let matched: HashSet<i64> = entries.iter().map(|entry| entry.id).collect();
            if let Some(vector) = model.encode(query) {
                let recalled = semantic_candidates_with_connection(
                    conn, model, &vector, cursor, limit, &matched,
                )?;
                if !recalled.is_empty() {
                    entries.extend(recalled);
                    entries.sort_unstable_by(|left, right| right.id.cmp(&left.id));
                    entries.truncate(limit + 1);
                }
            }
        }

        let has_more = entries.len() > limit;
        if has_more {
            entries.pop();
        }

        Ok(PaginatedHistory { entries, has_more })
    }

    #[cfg(test)]
    fn get_latest_entry_with_conn(conn: &Connection) -> Result<Option<HistoryEntry>> {
        let mut stmt = conn.prepare(
            "SELECT
                id,
                file_name,
                timestamp,
                saved,
                title,
                transcription_text,
                post_processed_text,
                post_process_requested,
                parent_id
             FROM transcription_history
             ORDER BY timestamp DESC
             LIMIT 1",
        )?;

        let entry = stmt.query_row([], Self::map_history_entry).optional()?;
        Ok(entry)
    }

    /// Get the latest entry with non-empty transcription text.
    pub fn get_latest_completed_entry(&self) -> Result<Option<HistoryEntry>> {
        self.storage
            .with_connection(|conn| Self::get_latest_completed_entry_with_conn(conn))
    }

    fn get_latest_completed_entry_with_conn(conn: &Connection) -> Result<Option<HistoryEntry>> {
        let mut stmt = conn.prepare(
            "SELECT
                id,
                file_name,
                timestamp,
                saved,
                title,
                transcription_text,
                post_processed_text,
                post_process_requested,
                parent_id
             FROM transcription_history
             WHERE transcription_text != ''
             ORDER BY timestamp DESC
             LIMIT 1",
        )?;

        let entry = stmt.query_row([], Self::map_history_entry).optional()?;
        Ok(entry)
    }

    /// The newest `limit` entries recorded at or after `since`, a UNIX-seconds
    /// timestamp, newest first.
    ///
    /// The read behind the chat brain's word counts and its corpus card, which
    /// need the text of a period rather than a page of it. One bounded `WHERE`
    /// here instead of paging the whole table by id: ids follow insertion, not
    /// time, so an imported archive would sit above newer captures and a walk
    /// that stopped at the first old row would stop too early.
    pub async fn get_history_entries_since(
        &self,
        since: i64,
        limit: usize,
    ) -> Result<Vec<HistoryEntry>> {
        let limit = i64::try_from(limit)?;
        self.storage.with_connection(|conn| {
            let mut stmt = conn.prepare(
                "SELECT
                    id,
                    file_name,
                    timestamp,
                    saved,
                    title,
                    transcription_text,
                    post_processed_text,
                    post_process_requested,
                    parent_id
                 FROM transcription_history
                 WHERE timestamp >= ?1
                 ORDER BY timestamp DESC
                 LIMIT ?2",
            )?;
            let entries = stmt
                .query_map(params![since, limit], Self::map_history_entry)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(entries)
        })
    }

    /// The oldest and newest entry timestamps, UNIX seconds, or `None` while
    /// the history is empty.
    pub async fn get_history_span(&self) -> Result<Option<(i64, i64)>> {
        self.storage.with_connection(|conn| {
            let span = conn.query_row(
                "SELECT MIN(timestamp), MAX(timestamp) FROM transcription_history",
                [],
                |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, Option<i64>>(1)?)),
            )?;
            Ok(match span {
                (Some(first), Some(last)) => Some((first, last)),
                _ => None,
            })
        })
    }

    pub async fn toggle_saved_status(&self, id: i64) -> Result<()> {
        // The event is emitted after the connection is released; nothing in the
        // emit path reads the database.
        let new_saved = self.storage.with_connection(|conn| {
            // Get current saved status
            let current_saved: bool = conn.query_row(
                "SELECT saved FROM transcription_history WHERE id = ?1",
                params![id],
                |row| row.get("saved"),
            )?;

            let new_saved = !current_saved;

            conn.execute(
                "UPDATE transcription_history SET saved = ?1 WHERE id = ?2",
                params![new_saved, id],
            )?;
            Ok(new_saved)
        })?;

        debug!("Toggled saved status for entry {}: {}", id, new_saved);

        // Emit history updated event
        if let Err(e) = (HistoryUpdatePayload::Toggled { id }).emit(&self.app_handle) {
            error!("Failed to emit history-updated event: {}", e);
        }

        Ok(())
    }

    /// Resolve one recording inside this app's recordings directory. `None`
    /// means the caller supplied something other than a bare file name, which
    /// no history row can contain.
    pub fn get_audio_file_path(&self, file_name: &str) -> Option<PathBuf> {
        recording_path(&self.recordings_dir, file_name)
    }

    pub async fn get_entry_by_id(&self, id: i64) -> Result<Option<HistoryEntry>> {
        self.storage
            .with_connection(|conn| Self::entry_by_id_with_connection(conn, id))
    }

    fn entry_by_id_with_connection(conn: &Connection, id: i64) -> Result<Option<HistoryEntry>> {
        let mut stmt = conn.prepare(
            "SELECT
                id,
                file_name,
                timestamp,
                saved,
                title,
                transcription_text,
                post_processed_text,
                post_process_requested,
                parent_id
             FROM transcription_history
             WHERE id = ?1",
        )?;

        let entry = stmt.query_row([id], Self::map_history_entry).optional()?;

        Ok(entry)
    }

    pub async fn delete_entry(&self, id: i64) -> Result<()> {
        // The row is read, its recording is resolved, and the row is deleted on
        // one connection. Reading the entry on a second connection made the
        // reference check decide on a row that the delete no longer had to
        // match.
        self.storage.with_connection(|conn| {
            // Keep the database record when deleting its final WAV reference fails
            // so cleanup can retry it later. Retry rows share the original WAV and
            // therefore must not delete it while another row still references it.
            if let Some(entry) = Self::entry_by_id_with_connection(conn, id)? {
                if !Self::has_other_recording_reference(conn, id, &entry.file_name)? {
                    Self::remove_recording_file(&self.recordings_dir, &entry.file_name)?;
                }
            }

            conn.execute(
                "DELETE FROM transcription_history WHERE id = ?1",
                params![id],
            )?;
            Ok(())
        })?;

        debug!("Deleted history entry with id: {}", id);

        // Emit history updated event
        if let Err(e) = (HistoryUpdatePayload::Deleted { id }).emit(&self.app_handle) {
            error!("Failed to emit history-updated event: {}", e);
        }

        Ok(())
    }

    fn format_timestamp_title(&self, timestamp: i64) -> String {
        if let Some(utc_datetime) = DateTime::from_timestamp(timestamp, 0) {
            // Convert UTC to local timezone
            let local_datetime = utc_datetime.with_timezone(&Local);
            local_datetime.format("%B %e, %Y - %l:%M%p").to_string()
        } else {
            format!("Recording {}", timestamp)
        }
    }
}

/// Build one FTS5 `MATCH` expression from raw user text.
///
/// Every token is wrapped in double quotes with embedded quotes doubled, so
/// the whole search box is data and never FTS5 syntax: `note:` matches the
/// literal word instead of failing with `no such column: note`, an unbalanced
/// `"` or a trailing `AND` cannot produce `fts5: syntax error`, and `NEAR`,
/// `OR`, `*`, `^`, `(`, `)` become ordinary terms. Tokens are joined by spaces,
/// which FTS5 reads as implicit AND, so every word must appear.
///
/// A token with no alphanumeric character (`--`, `🎉`, a lone quote) is dropped:
/// the tokenizer produces no term for it, and an empty quoted phrase is itself
/// a syntax error. Each phrase carries a trailing `*` so the word the user is
/// still typing matches by prefix.
///
/// `None` means the text held nothing searchable; the caller returns an empty
/// page without touching `MATCH`.
fn fts_match_query(user_text: &str) -> Option<String> {
    let mut expression = String::new();
    for token in user_text
        .split_whitespace()
        .filter(|token| token.chars().any(char::is_alphanumeric))
    {
        if !expression.is_empty() {
            expression.push(' ');
        }
        expression.push('"');
        for character in token.chars() {
            if character == '"' {
                expression.push('"');
            }
            expression.push(character);
        }
        expression.push_str("\"*");
    }

    (!expression.is_empty()).then_some(expression)
}

/// The rows semantic recall contributes to one search page, newest first and
/// already stamped [`HistoryMatchKind::Semantic`].
///
/// A store with no comparable vector at all — every row still awaiting the
/// backfill, or every stored vector written by a different model revision — is
/// reported once as a fact and then ignored. The search returns its lexical
/// result unchanged; a half-built index is a normal state during backfill, not
/// an error, and it is not the user's problem to see.
fn semantic_candidates_with_connection(
    conn: &Connection,
    model: &SemanticModel,
    query_vector: &[f32],
    cursor: Option<i64>,
    limit: usize,
    lexically_matched: &HashSet<i64>,
) -> Result<Vec<HistoryEntry>> {
    let (ids, compared) = semantic_candidate_ids_with_connection(
        conn,
        model,
        query_vector,
        cursor,
        limit + 1,
        lexically_matched,
    )?;
    if compared == 0 {
        debug!(
            "Semantic recall compared no rows for model {}; results are lexical-only",
            model.revision()
        );
    }
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut entries = entries_by_ids_with_connection(conn, &ids)?;
    for entry in &mut entries {
        entry.match_kind = Some(HistoryMatchKind::Semantic);
    }
    Ok(entries)
}

/// Ids of the newest rows whose stored vector clears [`SIMILARITY_FLOOR`], and
/// how many rows were actually compared.
///
/// The scan reads `id` and the vector and nothing else. It walks `id DESC` and
/// stops once it holds `wanted` candidates, because the merged page is ordered
/// by id: no row it has not reached can outrank one it already has.
///
/// `semantic_model_revision = ?` is not decoration. Two models produce vectors
/// of the same width and no shared meaning, so a vector written by a different
/// revision is skipped rather than compared.
///
/// The compared count is the difference between "nothing was similar enough"
/// and "nothing was comparable", which the caller needs to say something true.
fn semantic_candidate_ids_with_connection(
    conn: &Connection,
    model: &SemanticModel,
    query_vector: &[f32],
    cursor: Option<i64>,
    wanted: usize,
    lexically_matched: &HashSet<i64>,
) -> Result<(Vec<i64>, usize)> {
    let mut statement = conn.prepare(
        "SELECT id, semantic_embedding
         FROM transcription_history
         WHERE semantic_embedding IS NOT NULL
           AND semantic_model_revision = ?1
           AND (?2 IS NULL OR id < ?2)
         ORDER BY id DESC",
    )?;
    let mut rows = statement.query(params![model.revision(), cursor])?;
    let mut ids = Vec::with_capacity(wanted);
    let mut compared = 0_usize;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        if lexically_matched.contains(&id) {
            continue;
        }
        let stored: &[u8] = row.get_ref(1)?.as_blob()?;
        let Some(similarity) = semantic::cosine_similarity(stored, query_vector) else {
            continue;
        };
        compared += 1;
        if similarity < semantic::SIMILARITY_FLOOR {
            continue;
        }
        ids.push(id);
        if ids.len() == wanted {
            break;
        }
    }
    Ok((ids, compared))
}

/// Load full history rows for an explicit id list, newest first.
fn entries_by_ids_with_connection(conn: &Connection, ids: &[i64]) -> Result<Vec<HistoryEntry>> {
    let placeholders = vec!["?"; ids.len()].join(",");
    let mut statement = conn.prepare(&format!(
        "SELECT
            id,
            file_name,
            timestamp,
            saved,
            title,
            transcription_text,
            post_processed_text,
            post_process_requested,
            parent_id
         FROM transcription_history
         WHERE id IN ({placeholders})
         ORDER BY id DESC"
    ))?;
    let rows = statement.query_map(
        rusqlite::params_from_iter(ids.iter()),
        HistoryManager::map_history_entry,
    )?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Record what the model concluded about one row.
///
/// A `None` vector is stored as a NULL embedding with the revision set: the
/// model did look at this row and found nothing embeddable. That is the state
/// that lets the backfill terminate, and it is also the honest record — the
/// alternative, leaving the row untouched, claims it was never considered.
///
/// The FTS5 triggers fire `AFTER UPDATE OF transcription_text,
/// post_processed_text`, and this statement names neither, so writing a vector
/// does not re-index the row.
fn store_semantic_vector_with_connection(
    conn: &Connection,
    history_id: i64,
    vector: Option<&[f32]>,
    revision: &str,
) -> Result<()> {
    let blob = vector.map(semantic::encode_vector);
    conn.execute(
        "UPDATE transcription_history
         SET semantic_embedding = ?1, semantic_model_revision = ?2
         WHERE id = ?3",
        params![blob, revision, history_id],
    )?;
    Ok(())
}

/// Embed up to `chunk` rows the current model has not considered yet.
///
/// Returns the number of rows written, so the caller loops until it gets zero.
/// The selection predicate — `semantic_model_revision IS NOT ?` — is the whole
/// progress record: null-safe, so it covers both "never embedded" and
/// "embedded by an older model", and self-clearing, so there is no separate
/// cursor or progress table to keep in sync with the rows.
fn backfill_semantic_chunk_with_connection(
    conn: &mut Connection,
    model: &SemanticModel,
    chunk: usize,
) -> Result<usize> {
    let pending: Vec<(i64, String, Option<String>)> = {
        let mut statement = conn.prepare(
            "SELECT id, transcription_text, post_processed_text
             FROM transcription_history
             WHERE semantic_model_revision IS NOT ?1
             ORDER BY id DESC
             LIMIT ?2",
        )?;
        let rows = statement
            .query_map(params![model.revision(), i64::try_from(chunk)?], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    if pending.is_empty() {
        return Ok(0);
    }

    // One transaction per chunk: a chunk either advances entirely or not at
    // all, so an interrupted backfill leaves no row half-marked.
    let transaction = conn.transaction()?;
    for (id, transcription_text, post_processed_text) in &pending {
        let vector = model.encode(semantic::embeddable_text(
            transcription_text,
            post_processed_text.as_deref(),
        ));
        store_semantic_vector_with_connection(
            &transaction,
            *id,
            vector.as_deref(),
            model.revision(),
        )?;
    }
    transaction.commit()?;
    Ok(pending.len())
}

/// The instant before which a recording no longer belongs on disk, in the
/// same unit the `timestamp` column stores, or `None` when audio lives exactly
/// as long as its row.
///
/// That unit is the whole reason this is a named function rather than a `match`
/// inside the sweep. `timestamp` is written as `Utc::now().timestamp()`, which
/// is **seconds**, so every period here is expressed in seconds and `now` must
/// arrive in seconds too. A cutoff computed in milliseconds against a
/// seconds column would be far in the future, select every row, and delete a
/// user's whole history; one computed the other way would select nothing and
/// silently keep it forever. Taking `now` as an argument is what lets a test
/// pin both the unit and the arithmetic without waiting three days.
///
/// `Months3` is 90 days, not three calendar months. "Not kept" is a cutoff
/// after every row there is: the recording goes as soon as the words it
/// produced are written.
fn recording_expiry_cutoff(
    retention_period: crate::settings::RecordingRetentionPeriod,
    now_seconds: i64,
) -> Option<i64> {
    const DAY: i64 = 24 * 60 * 60;
    let window = match retention_period {
        crate::settings::RecordingRetentionPeriod::PreserveLimit => return None,
        crate::settings::RecordingRetentionPeriod::Never => return Some(i64::MAX),
        crate::settings::RecordingRetentionPeriod::Days3 => 3 * DAY,
        crate::settings::RecordingRetentionPeriod::Weeks2 => 14 * DAY,
        crate::settings::RecordingRetentionPeriod::Months3 => 90 * DAY,
    };
    Some(now_seconds - window)
}

/// The single owner of the recordings-directory join.
///
/// `Path::join` silently replaces the base for an absolute argument and walks
/// out of it for a `..` component, so a name that is not exactly one file-name
/// component is refused rather than resolved. Every recording this app writes,
/// and every `file_name` a history row can hold, is a bare name.
fn recording_path(recordings_dir: &Path, file_name: &str) -> Option<PathBuf> {
    (Path::new(file_name).file_name() == Some(file_name.as_ref()))
        .then(|| recordings_dir.join(file_name))
}
fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn as_sql_i64(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| anyhow!("receipt timestamp or run id exceeds SQLite INTEGER"))
}

fn as_u64(value: i64, column: usize) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

fn decode_receipt_json<T: serde::de::DeserializeOwned>(
    raw: String,
    column: usize,
) -> rusqlite::Result<T> {
    serde_json::from_str(&raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn map_run_receipt(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryRunReceipt> {
    Ok(HistoryRunReceipt {
        id: row.get(0)?,
        history_id: row.get(1)?,
        run_id: as_u64(row.get(2)?, 2)?,
        retry_of_run_id: row
            .get::<_, Option<i64>>(3)?
            .map(|value| as_u64(value, 3))
            .transpose()?,
        started_at_ms: as_u64(row.get(4)?, 4)?,
        completed_at_ms: as_u64(row.get(5)?, 5)?,
        mode: decode_receipt_json(row.get::<_, String>(6)?, 6)?,
        context: decode_receipt_json(row.get::<_, String>(7)?, 7)?,
        duration_ms: row
            .get::<_, Option<i64>>(8)?
            .map(|value| as_u64(value, 8))
            .transpose()?,
        word_count: row
            .get::<_, Option<i64>>(9)?
            .map(|value| as_u64(value, 9))
            .transpose()?,
        source_kind: HistorySourceKind::from_stored(row.get(10)?),
        has_audio: row.get(11)?,
        capture_status: CaptureStatus::from_stored(row.get(12)?),
        delivery_attempts: Vec::new(),
    })
}

fn map_delivery_attempt(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryDeliveryAttempt> {
    Ok(HistoryDeliveryAttempt {
        id: row.get(0)?,
        history_id: row.get(1)?,
        run_receipt_id: row.get(2)?,
        delivery: DeliveryReceipt {
            method: decode_receipt_json(row.get::<_, String>(3)?, 3)?,
            outcome: decode_receipt_json(row.get::<_, String>(4)?, 4)?,
            dispatched_at_ms: as_u64(row.get(5)?, 5)?,
        },
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{params, Connection};

    #[test]
    fn file_source_kind_round_trips() {
        assert_eq!(HistorySourceKind::File.as_str(), "file");
        assert_eq!(
            HistorySourceKind::from_stored(Some("file".to_string())),
            Some(HistorySourceKind::File)
        );
    }

    fn setup_conn() -> Connection {
        let mut conn = Connection::open_in_memory().expect("open in-memory db");
        Migrations::new(MIGRATIONS.to_vec())
            .to_latest(&mut conn)
            .expect("apply history migrations");
        conn
    }

    fn insert_entry(
        conn: &Connection,
        timestamp: i64,
        text: &str,
        post_processed: Option<&str>,
    ) -> i64 {
        conn.execute(
            "INSERT INTO transcription_history (
                file_name,
                timestamp,
                saved,
                title,
                transcription_text,
                post_processed_text,
                post_process_requested
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                format!("sona-{}.wav", timestamp),
                timestamp,
                false,
                format!("Recording {}", timestamp),
                text,
                post_processed,
                false,
            ],
        )
        .expect("insert history entry");

        conn.last_insert_rowid()
    }

    fn insert_trend_entry(
        conn: &Connection,
        timestamp: i64,
        text: &str,
        source_kind: Option<&str>,
        duration_ms: i64,
        words: i64,
    ) -> i64 {
        let id = insert_entry(conn, timestamp, text, None);
        conn.execute(
            "UPDATE transcription_history
             SET source_kind = ?1, duration_ms = ?2, word_count = ?3
             WHERE id = ?4",
            params![source_kind, duration_ms, words, id],
        )
        .expect("seed trend entry");
        id
    }

    fn local_noon_timestamp(date: chrono::NaiveDate) -> i64 {
        date.and_hms_opt(12, 0, 0)
            .expect("local noon")
            .and_local_timezone(Local)
            .earliest()
            .expect("representable local noon")
            .timestamp()
    }

    fn new_run_receipt(run_id: u64) -> NewRunReceipt {
        NewRunReceipt {
            run: ModeReceipt {
                run_id,
                settings_revision: 1,
                mode_selection_source: crate::modes::ModeSelectionSource::ActiveMode,
                mode_id: "message".to_string(),
                tone: crate::modes::Tone::Balanced,
                requested_context_policy: crate::context::ContextPolicy::None,
                context_policy_ceiling: crate::context::ContextPolicy::None,
                context_policy: crate::context::ContextPolicy::None,
                prompt_preset: crate::modes::PromptPreset::MinimalistCleanup,
                post_process_requested: true,
                rewrite: crate::modes::RewriteOutcome::Applied,
                provider_id: Some("local".to_string()),
                model_id: Some("model".to_string()),
                engine_requested: crate::modes::RequestedEngine::Local,
                engine_used: Some(crate::modes::RequestedEngine::Local),
                cloud_fallback: false,
                cloud_status: crate::modes::CloudReceiptStatus::NotRequested,
                local_fallback_model_id: None,
                input_peak: Some(0.1456),
                input_rms: Some(0.011),
                realtime_factor: Some(13.82),
            },
            context: ContextReceipt {
                requested_policy: crate::context::ContextPolicy::None,
                policy: crate::context::ContextPolicy::None,
                accessibility: crate::context::AccessibilityAccess::Unsupported,
                sources: crate::context::ContextSources::default(),
                captured_at_ms: 10,
                application_captured_at_ms: None,
            },
            started_at_ms: 10,
            completed_at_ms: 20,
            duration_ms: Some(1_000),
            word_count: Some(2),
            source_kind: HistorySourceKind::Microphone,
            has_audio: true,
            capture_status: Some(CaptureStatus::Complete),
        }
    }

    #[test]
    fn history_stats_aggregate_retained_rows_independently_of_fts_search() {
        let empty = setup_conn();
        let empty_stats =
            HistoryManager::get_history_stats_with_connection(&empty).expect("empty history stats");
        assert_eq!(empty_stats.entries, 0);
        assert_eq!(empty_stats.total_duration_ms, 0);
        assert_eq!(empty_stats.total_words, 0);
        assert!(empty_stats.by_source.is_empty());

        let conn = setup_conn();
        let microphone = insert_entry(&conn, 100, "microphone transcript", None);
        let file = insert_entry(&conn, 200, "file transcript", None);
        let legacy = insert_entry(&conn, 300, "legacy transcript", None);
        conn.execute(
            "UPDATE transcription_history
             SET duration_ms = ?1, word_count = ?2, source_kind = ?3
             WHERE id = ?4",
            params![1_200_i64, 3_i64, "microphone", microphone],
        )
        .expect("seed microphone receipt columns");
        conn.execute(
            "UPDATE transcription_history
             SET duration_ms = ?1, word_count = ?2, source_kind = ?3
             WHERE id = ?4",
            params![800_i64, 2_i64, "file", file],
        )
        .expect("seed file receipt columns");
        assert!(legacy > 0);

        let before =
            HistoryManager::get_history_stats_with_connection(&conn).expect("history stats");
        assert_eq!(before.entries, 3);
        assert_eq!(before.total_duration_ms, 2_000);
        assert_eq!(before.total_words, 5);
        assert_eq!(before.by_source.len(), 3);
        assert_eq!(
            before.by_source[0].source_kind,
            Some(HistorySourceKind::Microphone)
        );
        assert_eq!(before.by_source[0].entries, 1);
        assert_eq!(
            before.by_source[1].source_kind,
            Some(HistorySourceKind::File)
        );
        assert_eq!(before.by_source[1].total_duration_ms, 800);
        assert_eq!(before.by_source[2].source_kind, None);
        assert_eq!(before.by_source[2].total_words, 0);

        let page = HistoryManager::search_history_entries_with_connection(
            &conn,
            "microphone",
            None,
            Some(10),
            None,
        )
        .expect("FTS search");
        assert_eq!(page.entries.len(), 1);
        let after =
            HistoryManager::get_history_stats_with_connection(&conn).expect("history stats");
        assert_eq!(after.entries, before.entries);
        assert_eq!(after.total_duration_ms, before.total_duration_ms);
        assert_eq!(after.total_words, before.total_words);
    }

    #[test]
    fn history_trend_zero_fills_an_empty_requested_range() {
        let mut conn = setup_conn();
        let now = Local::now();
        for range in [
            DashboardTrendRange::Days7,
            DashboardTrendRange::Days30,
            DashboardTrendRange::Days180,
        ] {
            let request = DashboardTrendRequest { range };
            let trend = HistoryManager::get_history_trend_with_connection_at(
                &mut conn,
                request,
                now.clone(),
            )
            .expect("empty trend");

            assert_eq!(trend.points.len(), range.days());
            assert_eq!(trend.range_total.recordings, 0);
            assert_eq!(trend.range_total.duration_ms, 0);
            assert_eq!(trend.range_total.words, 0);
            assert_eq!(trend.all_time.recordings, 0);
            assert_eq!(trend.active_days, 0);
            assert_eq!(trend.current_streak_days, 0);
            assert!(trend.points.iter().all(|point| {
                point.recordings == 0
                    && point.duration_ms == 0
                    && point.words == 0
                    && point.by_source.len() == 3
                    && point.by_source.iter().all(|source| {
                        source.recordings == 0 && source.duration_ms == 0 && source.words == 0
                    })
            }));
        }
    }

    #[test]
    fn history_trend_groups_unrecognized_stored_source_kinds_as_unknown() {
        let mut conn = setup_conn();
        let now = Local::now();
        let request = DashboardTrendRequest {
            range: DashboardTrendRange::Days7,
        };
        let calendar =
            LocalCalendarRange::at(now.clone(), request.range).expect("local calendar range");
        let date = calendar.local_dates().expect("local dates")[0];
        insert_trend_entry(
            &conn,
            local_noon_timestamp(date),
            "unknown-source recording",
            Some("unsupported"),
            1,
            1,
        );

        let trend = HistoryManager::get_history_trend_with_connection_at(&mut conn, request, now)
            .expect("history trend");
        assert_eq!(trend.range_total.recordings, 1);
        assert_eq!(trend.range_total.by_source[2].source_kind, None);
        assert_eq!(trend.range_total.by_source[2].recordings, 1);
        assert_eq!(trend.range_total.by_source[2].duration_ms, 1);
        assert_eq!(trend.range_total.by_source[2].words, 1);
        assert_eq!(trend.all_time, trend.range_total);
        assert_eq!(trend.points[0].by_source[2].recordings, 1);
    }

    #[test]
    fn history_trend_has_dense_days_and_separate_range_source_totals() {
        let mut conn = setup_conn();
        let now = Local::now();
        let request = DashboardTrendRequest {
            range: DashboardTrendRange::Days7,
        };
        let calendar =
            LocalCalendarRange::at(now.clone(), request.range).expect("local calendar range");
        let dates = calendar.local_dates().expect("requested local dates");

        insert_trend_entry(
            &conn,
            local_noon_timestamp(dates[0]),
            "microphone recording",
            Some("microphone"),
            1_200,
            3,
        );
        insert_trend_entry(
            &conn,
            local_noon_timestamp(dates[2]),
            "file recording",
            Some("file"),
            800,
            2,
        );
        insert_trend_entry(
            &conn,
            local_noon_timestamp(dates[4]),
            "legacy recording",
            None,
            400,
            1,
        );
        insert_trend_entry(
            &conn,
            local_noon_timestamp(dates[6]),
            "latest microphone recording",
            Some("microphone"),
            200,
            1,
        );
        insert_trend_entry(
            &conn,
            calendar.end_exclusive_utc_seconds(),
            "outside the selected range",
            Some("file"),
            5_000,
            7,
        );

        let trend = HistoryManager::get_history_trend_with_connection_at(&mut conn, request, now)
            .expect("history trend");

        assert_eq!(trend.points.len(), 7);
        assert_eq!(
            trend.points[1].local_date,
            dates[1].format("%F").to_string()
        );
        assert_eq!(trend.points[1].recordings, 0);
        assert_eq!(trend.points[1].by_source.len(), 3);
        assert_eq!(
            trend.points[1].by_source[0].source_kind,
            Some(HistorySourceKind::Microphone)
        );
        assert_eq!(
            trend.points[1].by_source[1].source_kind,
            Some(HistorySourceKind::File)
        );
        assert_eq!(trend.points[1].by_source[2].source_kind, None);
        assert!(trend.points[1]
            .by_source
            .iter()
            .all(|source| source.recordings == 0));
        assert_eq!(trend.range_total.recordings, 4);
        assert_eq!(trend.range_total.duration_ms, 2_600);
        assert_eq!(trend.range_total.words, 7);
        assert_eq!(trend.range_total.by_source[0].recordings, 2);
        assert_eq!(trend.range_total.by_source[0].duration_ms, 1_400);
        assert_eq!(trend.range_total.by_source[1].recordings, 1);
        assert_eq!(trend.range_total.by_source[1].duration_ms, 800);
        assert_eq!(trend.range_total.by_source[2].recordings, 1);
        assert_eq!(trend.range_total.by_source[2].duration_ms, 400);
        assert_eq!(trend.all_time.recordings, 5);
        assert_eq!(trend.all_time.duration_ms, 7_600);
        assert_eq!(trend.all_time.words, 14);
        assert_eq!(trend.active_days, 4);
        assert_eq!(trend.current_streak_days, 1);
    }

    #[test]
    fn history_trend_uses_local_calendar_boundaries() {
        let mut conn = setup_conn();
        let now = chrono::NaiveDate::from_ymd_opt(2026, 9, 3)
            .expect("fixed local date")
            .and_hms_opt(12, 0, 0)
            .expect("fixed local noon")
            .and_local_timezone(Local)
            .earliest()
            .expect("representable fixed local noon");
        let request = DashboardTrendRequest {
            range: DashboardTrendRange::Days7,
        };
        let calendar =
            LocalCalendarRange::at(now.clone(), request.range).expect("local calendar range");

        insert_trend_entry(
            &conn,
            calendar.start_utc_seconds() - 1,
            "before local range",
            Some("microphone"),
            1,
            1,
        );
        insert_trend_entry(
            &conn,
            calendar.start_utc_seconds(),
            "first local day",
            Some("microphone"),
            1,
            1,
        );
        insert_trend_entry(
            &conn,
            calendar.end_exclusive_utc_seconds() - 1,
            "last local day",
            Some("file"),
            1,
            1,
        );
        insert_trend_entry(
            &conn,
            calendar.end_exclusive_utc_seconds(),
            "after local range",
            Some("file"),
            1,
            1,
        );

        let trend = HistoryManager::get_history_trend_with_connection_at(&mut conn, request, now)
            .expect("boundary trend");

        assert_eq!(trend.range_total.recordings, 2);
        assert_eq!(trend.all_time.recordings, 4);
        assert_eq!(trend.points.first().map(|point| point.recordings), Some(1));
        assert_eq!(trend.points.last().map(|point| point.recordings), Some(1));
        assert_eq!(trend.current_streak_days, 1);
    }

    #[test]
    fn history_trend_ignores_history_search_filters() {
        let mut conn = setup_conn();
        let now = Local::now();
        let request = DashboardTrendRequest {
            range: DashboardTrendRange::Days7,
        };
        let calendar =
            LocalCalendarRange::at(now.clone(), request.range).expect("local calendar range");
        let dates = calendar.local_dates().expect("requested local dates");
        insert_trend_entry(
            &conn,
            local_noon_timestamp(dates[1]),
            "visible to this search",
            Some("microphone"),
            100,
            1,
        );
        insert_trend_entry(
            &conn,
            local_noon_timestamp(dates[3]),
            "not in the query",
            Some("file"),
            200,
            2,
        );

        let search = HistoryManager::search_history_entries_with_connection(
            &conn,
            "visible",
            None,
            Some(10),
            None,
        )
        .expect("filtered history search");
        assert_eq!(search.entries.len(), 1);

        let trend = HistoryManager::get_history_trend_with_connection_at(&mut conn, request, now)
            .expect("unfiltered trend");
        assert_eq!(trend.range_total.recordings, 2);
        assert_eq!(trend.range_total.duration_ms, 300);
        assert_eq!(trend.range_total.words, 3);
    }

    #[test]
    fn truncated_capture_persists_audio_receipt_without_transcript_or_delivery() {
        let conn = setup_conn();
        let history_id = insert_entry(&conn, 100, "", None);
        let mut receipt = new_run_receipt(73);
        receipt.word_count = Some(0);
        receipt.capture_status = Some(CaptureStatus::Truncated);

        let run_receipt_id = HistoryManager::insert_run_receipt(&conn, history_id, None, &receipt)
            .expect("persist truncated capture receipt");
        HistoryManager::insert_delivery_attempt(
            &conn,
            history_id,
            run_receipt_id,
            &DeliveryReceipt::not_dispatched(),
        )
        .expect("persist no-delivery receipt");

        let (text, has_audio, status, method, outcome): (
            String,
            bool,
            Option<String>,
            String,
            String,
        ) = conn
            .query_row(
                "SELECT h.transcription_text, h.has_audio, r.capture_status,
                        d.delivery_method, d.delivery_outcome
                 FROM transcription_history AS h
                 JOIN transcription_runs AS r ON r.history_id = h.id
                 JOIN delivery_attempts AS d ON d.run_receipt_id = r.id
                 WHERE h.id = ?1",
                params![history_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("load truncated capture");

        assert_eq!(text, "");
        assert!(has_audio);
        assert_eq!(status.as_deref(), Some("truncated"));
        assert_eq!(
            serde_json::from_str::<DeliveryMethod>(&method).expect("delivery method"),
            DeliveryMethod::None
        );
        assert_eq!(
            serde_json::from_str::<DeliveryOutcome>(&outcome).expect("delivery outcome"),
            DeliveryOutcome::DefinitelyNotDispatched
        );
    }

    #[test]
    fn cloud_receipt_divergence_round_trips_without_collapsing_requested_and_used_engines() {
        let conn = setup_conn();
        let history_id = insert_entry(&conn, 100, "provider preview", None);

        let mut fallback = new_run_receipt(41);
        fallback.run.engine_requested = crate::modes::RequestedEngine::DeepgramNova3;
        fallback.run.engine_used = Some(crate::modes::RequestedEngine::Local);
        fallback.run.cloud_fallback = true;
        fallback.run.cloud_status = crate::modes::CloudReceiptStatus::Fallback;
        fallback.run.local_fallback_model_id = Some("frozen-fallback".to_string());
        HistoryManager::insert_run_receipt(&conn, history_id, None, &fallback)
            .expect("persist fallback receipt");

        let mut held = new_run_receipt(42);
        held.run.engine_requested = crate::modes::RequestedEngine::DeepgramNova3;
        held.run.engine_used = None;
        held.run.cloud_fallback = false;
        held.run.cloud_status = crate::modes::CloudReceiptStatus::HeldCloudUnavailable;
        HistoryManager::insert_run_receipt(&conn, history_id, Some(41), &held)
            .expect("persist held receipt");

        let mut statement = conn
            .prepare("SELECT mode_receipt_json FROM transcription_runs ORDER BY id ASC")
            .expect("receipt query");
        let receipts = statement
            .query_map([], |row| {
                let receipt: ModeReceipt = serde_json::from_str(&row.get::<_, String>(0)?)
                    .map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
                Ok(receipt)
            })
            .expect("map receipts")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("read receipts");

        assert_eq!(receipts.len(), 2);
        assert_eq!(
            receipts[0].engine_requested,
            crate::modes::RequestedEngine::DeepgramNova3
        );
        assert_eq!(
            receipts[0].engine_used,
            Some(crate::modes::RequestedEngine::Local)
        );
        assert!(receipts[0].cloud_fallback);
        assert_eq!(
            receipts[0].cloud_status,
            crate::modes::CloudReceiptStatus::Fallback
        );
        assert_eq!(
            receipts[0].local_fallback_model_id.as_deref(),
            Some("frozen-fallback")
        );
        assert_eq!(
            receipts[1].engine_requested,
            crate::modes::RequestedEngine::DeepgramNova3
        );
        assert_eq!(receipts[1].engine_used, None);
        assert!(!receipts[1].cloud_fallback);
        assert_eq!(
            receipts[1].cloud_status,
            crate::modes::CloudReceiptStatus::HeldCloudUnavailable
        );
    }

    /// The measured amplitude and decode throughput have to survive the
    /// persisted JSON, and a receipt written before those measurements existed
    /// has to keep parsing — the two halves of the `#[serde(default)]
    /// Option<f32>` contract, for all three fields.
    #[test]
    fn measured_capture_values_round_trip_and_a_legacy_receipt_reads_as_unmeasured() {
        const MEASURED: [&str; 3] = ["input_peak", "input_rms", "realtime_factor"];

        let conn = setup_conn();
        let history_id = insert_entry(&conn, 100, "measured", None);

        let measured = new_run_receipt(51);
        assert_eq!(measured.run.input_peak, Some(0.1456));
        HistoryManager::insert_run_receipt(&conn, history_id, None, &measured)
            .expect("persist measured receipt");

        let stored: String = conn
            .query_row(
                "SELECT mode_receipt_json FROM transcription_runs WHERE run_id = 51",
                [],
                |row| row.get(0),
            )
            .expect("read stored receipt");
        let reread: ModeReceipt = serde_json::from_str(&stored).expect("parse stored receipt");
        assert_eq!(reread.input_peak, Some(0.1456));
        assert_eq!(reread.input_rms, Some(0.011));
        assert_eq!(reread.realtime_factor, Some(13.82));

        let legacy = serde_json::to_value(&measured.run)
            .expect("serialize receipt")
            .as_object()
            .expect("receipt object")
            .iter()
            .filter(|(key, _)| !MEASURED.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<serde_json::Map<_, _>>();
        let parsed: ModeReceipt = serde_json::from_value(serde_json::Value::Object(legacy))
            .expect("parse legacy receipt");
        assert_eq!(parsed.input_peak, None);
        assert_eq!(parsed.input_rms, None);
        assert_eq!(parsed.realtime_factor, None);
    }

    fn history_columns(conn: &Connection) -> Vec<String> {
        let mut statement = conn
            .prepare("SELECT name FROM pragma_table_info('transcription_history')")
            .expect("prepare table info");
        let columns = statement
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query table info")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("read table info");
        columns
    }

    #[test]
    fn legacy_prompt_column_is_dropped_without_touching_text_or_receipt_columns() {
        let mut conn = Connection::open_in_memory().expect("open in-memory db");
        Migrations::new(MIGRATIONS[..6].to_vec())
            .to_latest(&mut conn)
            .expect("apply migrations before receipt hardening");
        let entry_id = insert_entry(&conn, 100, "keep raw text", Some("keep processed text"));
        conn.execute(
            "UPDATE transcription_history SET post_process_prompt = ?1 WHERE id = ?2",
            params!["private system prompt", entry_id],
        )
        .expect("seed legacy prompt body");

        Migrations::new(MIGRATIONS.to_vec())
            .to_latest(&mut conn)
            .expect("apply receipt hardening and column removal migrations");

        assert!(!history_columns(&conn).contains(&"post_process_prompt".to_string()));
        let row = conn
            .query_row(
                "SELECT transcription_text, post_processed_text,
                        duration_ms, word_count, source_kind, has_audio
                 FROM transcription_history WHERE id = ?1",
                params![entry_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, bool>(5)?,
                    ))
                },
            )
            .expect("read migrated history row");
        assert_eq!(row.0, "keep raw text");
        assert_eq!(row.1.as_deref(), Some("keep processed text"));
        assert_eq!((row.2, row.3, row.4), (None, None, None));
        assert!(row.5);

        let page = HistoryManager::search_history_entries_with_connection(
            &conn,
            "raw",
            None,
            Some(5),
            None,
        )
        .expect("search after column removal");
        assert_eq!(
            page.entries
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            vec![entry_id]
        );
    }

    /// Insert using only the columns the first migration creates, so one helper
    /// works against every historical schema version.
    fn insert_base_entry(conn: &Connection, timestamp: i64, text: &str) -> i64 {
        conn.execute(
            "INSERT INTO transcription_history (
                file_name,
                timestamp,
                saved,
                title,
                transcription_text
            ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                format!("sona-{}.wav", timestamp),
                timestamp,
                false,
                format!("Recording {}", timestamp),
                text,
            ],
        )
        .expect("insert base history entry");

        conn.last_insert_rowid()
    }

    /// A row written before reprocessing existed has no parent, and the column
    /// must arrive nullable so the migration cannot fail on a populated store.
    #[test]
    fn parent_id_is_added_as_a_nullable_column_and_existing_rows_keep_no_parent() {
        let mut conn = Connection::open_in_memory().expect("open in-memory db");
        // Absolute, not `MIGRATIONS.len() - 1`: this prefix means "the schema
        // just before `parent_id` arrived", a fixed point in history, and a
        // relative index silently retargets it every time a migration is added.
        const MIGRATIONS_BEFORE_PARENT_ID: usize = 10;
        Migrations::new(MIGRATIONS[..MIGRATIONS_BEFORE_PARENT_ID].to_vec())
            .to_latest(&mut conn)
            .expect("apply migrations up to the parent_id column");
        assert!(!history_columns(&conn).contains(&"parent_id".to_string()));
        let existing = insert_base_entry(&conn, 100, "written before reprocessing existed");

        Migrations::new(MIGRATIONS.to_vec())
            .to_latest(&mut conn)
            .expect("apply the parent_id migration");

        assert!(history_columns(&conn).contains(&"parent_id".to_string()));
        let carried: Option<i64> = conn
            .query_row(
                "SELECT parent_id FROM transcription_history WHERE id = ?1",
                params![existing],
                |row| row.get(0),
            )
            .expect("read carried parent_id");
        assert_eq!(carried, None);
    }

    /// Deleting the source of a reprocess must not delete the reprocessed
    /// transcript, which is real output in its own right. It only loses the
    /// link.
    #[test]
    fn deleting_a_parent_orphans_the_child_instead_of_removing_it() {
        let conn = setup_conn();
        let parent = insert_base_entry(&conn, 100, "original");
        let child = insert_base_entry(&conn, 200, "reprocessed");
        conn.execute(
            "UPDATE transcription_history SET parent_id = ?1 WHERE id = ?2",
            params![parent, child],
        )
        .expect("link child to parent");

        conn.execute(
            "DELETE FROM transcription_history WHERE id = ?1",
            params![parent],
        )
        .expect("delete parent");

        let (text, parent_id): (String, Option<i64>) = conn
            .query_row(
                "SELECT transcription_text, parent_id FROM transcription_history WHERE id = ?1",
                params![child],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("child survives");
        assert_eq!(text, "reprocessed");
        assert_eq!(parent_id, None);
    }

    #[test]
    fn replay_storage_reuses_parent_wav_and_keeps_retry_and_reprocess_receipts_queryable() {
        let mut conn = setup_conn();
        let recordings = tempfile::tempdir().expect("recordings directory");
        let wav_path = recordings.path().join("reused.wav");
        crate::audio_toolkit::save_wav_file(&wav_path, &[0.0; 1_600]).expect("write parent WAV");
        assert_eq!(
            crate::audio_toolkit::read_wav_samples(&wav_path)
                .expect("read parent WAV")
                .len(),
            1_600
        );
        let parent_audio = std::fs::read(&wav_path).expect("read parent WAV bytes");
        let parent_receipt = new_run_receipt(11);
        let parent = HistoryManager::save_entry_with_receipt_with_connection(
            &mut conn,
            "reused.wav",
            100,
            "parent",
            "parent raw text",
            true,
            Some("parent polished text"),
            None,
            Some(&parent_receipt),
        )
        .expect("save parent history entry");

        let settings = crate::settings::AppSettings::default();
        let retry = crate::modes::RunPlan::for_retry(&settings, true).expect("build retry plan");
        let retry_receipt = NewRunReceipt {
            run: retry.mode_receipt(),
            context: retry.context().receipt().clone(),
            started_at_ms: retry.run_started_at_ms,
            completed_at_ms: retry.run_started_at_ms,
            duration_ms: Some(100),
            word_count: Some(2),
            source_kind: HistorySourceKind::Microphone,
            has_audio: true,
            capture_status: None,
        };
        let retried = HistoryManager::save_entry_with_receipt_with_connection(
            &mut conn,
            "reused.wav",
            200,
            "retry",
            "retry transcript",
            retry.post_process_requested(),
            None,
            Some(parent),
            Some(&retry_receipt),
        )
        .expect("save retry history entry");

        let target_mode = settings
            .modes
            .first()
            .expect("default reprocess mode")
            .id
            .clone();
        let reprocess = crate::modes::RunPlan::for_reprocess(&settings, &target_mode)
            .expect("build reprocess plan");
        let reprocess_receipt = NewRunReceipt {
            run: reprocess.mode_receipt(),
            context: reprocess.context().receipt().clone(),
            started_at_ms: reprocess.run_started_at_ms,
            completed_at_ms: reprocess.run_started_at_ms,
            duration_ms: Some(100),
            word_count: Some(2),
            source_kind: HistorySourceKind::Microphone,
            has_audio: true,
            capture_status: None,
        };
        let reprocessed = HistoryManager::save_entry_with_receipt_with_connection(
            &mut conn,
            "reused.wav",
            300,
            "reprocess",
            "reprocessed transcript",
            reprocess.post_process_requested(),
            None,
            Some(parent),
            Some(&reprocess_receipt),
        )
        .expect("save reprocess history entry");

        let parent_row: (String, String, Option<String>) = conn
            .query_row(
                "SELECT file_name, transcription_text, post_processed_text
                 FROM transcription_history WHERE id = ?1",
                params![parent],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("read preserved parent");
        assert_eq!(parent_row.0, "reused.wav");
        assert_eq!(parent_row.1, "parent raw text");
        assert_eq!(parent_row.2.as_deref(), Some("parent polished text"));
        assert_eq!(
            std::fs::read(&wav_path).expect("read shared WAV bytes"),
            parent_audio
        );

        for (child, expected_text) in [
            (retried, "retry transcript"),
            (reprocessed, "reprocessed transcript"),
        ] {
            let (file_name, text, parent_id): (String, String, Option<i64>) = conn
                .query_row(
                    "SELECT file_name, transcription_text, parent_id
                     FROM transcription_history WHERE id = ?1",
                    params![child],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .expect("read derived history entry");
            assert_eq!(file_name, "reused.wav");
            assert_eq!(text, expected_text);
            assert_eq!(parent_id, Some(parent));
        }

        let retry_receipts = HistoryManager::get_run_receipts_with_connection(&conn, retried)
            .expect("query retry receipt");
        assert_eq!(retry_receipts.len(), 1);
        assert_eq!(
            retry_receipts[0].retry_of_run_id,
            Some(parent_receipt.run.run_id)
        );
        let reprocess_receipts =
            HistoryManager::get_run_receipts_with_connection(&conn, reprocessed)
                .expect("query reprocess receipt");
        assert_eq!(reprocess_receipts.len(), 1);
        assert_eq!(
            reprocess_receipts[0].retry_of_run_id,
            Some(parent_receipt.run.run_id)
        );
        assert_eq!(reprocess_receipts[0].mode.mode_id, target_mode);
    }

    /// Every period the setting offers, pinned in the unit the column stores.
    ///
    /// The arithmetic had no test, and a units mistake here is the difference
    /// between deleting a user's whole history and silently keeping it forever.
    /// Asserted against a fixed `now` so each window is an exact number of
    /// seconds rather than "about three days".
    #[test]
    fn each_retention_period_cuts_at_its_own_number_of_seconds() {
        const DAY: i64 = 86_400;
        // A fixed instant, so the assertions are arithmetic and not a clock.
        const NOW: i64 = 1_788_417_783;

        for (period, days) in [
            (crate::settings::RecordingRetentionPeriod::Days3, 3),
            (crate::settings::RecordingRetentionPeriod::Weeks2, 14),
            (crate::settings::RecordingRetentionPeriod::Months3, 90),
        ] {
            assert_eq!(
                recording_expiry_cutoff(period, NOW),
                Some(NOW - days * DAY),
                "{period:?} must cut {days} days back, in seconds"
            );
        }
        assert_eq!(
            recording_expiry_cutoff(
                crate::settings::RecordingRetentionPeriod::PreserveLimit,
                NOW
            ),
            None,
            "audio kept with the dictation has no expiry"
        );
        assert!(
            recording_expiry_cutoff(crate::settings::RecordingRetentionPeriod::Never, NOW)
                .is_some_and(|cutoff| cutoff > NOW),
            "audio not kept expires every row, including one written this second"
        );
    }

    /// A row one second inside the window keeps its recording and a row one
    /// second outside it loses only the recording, for every period; starring a
    /// row exempts it either way. The words never go: expiry is an audio policy.
    #[test]
    fn expiry_takes_the_recording_from_unsaved_rows_past_the_cutoff_and_keeps_the_words() {
        const NOW: i64 = 1_788_417_783;

        for period in [
            crate::settings::RecordingRetentionPeriod::Days3,
            crate::settings::RecordingRetentionPeriod::Weeks2,
            crate::settings::RecordingRetentionPeriod::Months3,
        ] {
            let conn = setup_conn();
            let recordings = tempfile::tempdir().expect("recordings directory");
            let cutoff = recording_expiry_cutoff(period, NOW).expect("a dated period has a cutoff");

            let inside = insert_base_entry(&conn, cutoff + 1, "inside the window");
            let on_the_boundary = insert_base_entry(&conn, cutoff, "exactly at the cutoff");
            let outside = seed_wav_retention_entry(
                &conn,
                recordings.path(),
                cutoff - 1,
                "sona-outside.wav",
                "past the window",
                false,
                None,
            );
            let starred = insert_base_entry(&conn, cutoff - 1, "starred and ancient");
            conn.execute(
                "UPDATE transcription_history SET saved = 1 WHERE id = ?1",
                params![starred],
            )
            .expect("star the ancient row");

            let expired =
                HistoryManager::expire_recordings_with_connection(&conn, recordings.path(), cutoff)
                    .expect("expiry runs");

            assert_eq!(
                expired.iter().map(|entry| entry.id).collect::<Vec<_>>(),
                vec![outside],
                "{period:?} must expire exactly the stale row"
            );
            assert!(
                !row_has_audio(&conn, outside),
                "{period:?} cleared the stale row's audio"
            );
            assert_eq!(
                expired[0].transcription_text, "past the window",
                "{period:?} kept the words"
            );
            assert!(
                !recordings.path().join("sona-outside.wav").exists(),
                "{period:?} deleted the stale recording"
            );
            for (id, why) in [
                (inside, "the recent row"),
                // `timestamp < cutoff` is strict, so the boundary row keeps its audio.
                (on_the_boundary, "the row on the cutoff itself"),
                (starred, "a starred row"),
            ] {
                assert!(row_exists(&conn, id), "{period:?} kept {why}");
                assert!(
                    row_has_audio(&conn, id),
                    "{period:?} must leave the audio of {why}"
                );
            }
        }
    }

    /// An expiry keeps a recording that another row still plays: the words of
    /// the expired row lose their audio, the shared file stays for the other.
    #[test]
    fn expiry_keeps_a_recording_another_row_still_holds() {
        let conn = setup_conn();
        let recordings = tempfile::tempdir().expect("recordings directory");
        let stale = seed_wav_retention_entry(
            &conn,
            recordings.path(),
            100,
            "sona-shared.wav",
            "stale original",
            false,
            None,
        );
        let fresh_retry = insert_base_entry(&conn, 900, "fresh retry of the same audio");
        conn.execute(
            "UPDATE transcription_history SET file_name = 'sona-shared.wav' WHERE id = ?1",
            params![fresh_retry],
        )
        .expect("make the retry share the recording");

        let expired =
            HistoryManager::expire_recordings_with_connection(&conn, recordings.path(), 500)
                .expect("expiry runs");

        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].id, stale);
        assert!(!row_has_audio(&conn, stale));
        assert!(row_has_audio(&conn, fresh_retry));
        assert!(
            recordings.path().join("sona-shared.wav").exists(),
            "the retry still plays this file"
        );

        // Once the last holder expires too, the file goes with it.
        HistoryManager::expire_recordings_with_connection(&conn, recordings.path(), 1_000)
            .expect("second expiry runs");
        assert!(!recordings.path().join("sona-shared.wav").exists());
    }

    /// The count sweep keeps the newest `limit` unsaved rows and no more, and a
    /// starred row is kept without spending one of those places.
    #[test]
    fn a_count_sweep_keeps_the_newest_unsaved_rows_and_every_starred_one() {
        let conn = setup_conn();
        let recordings = tempfile::tempdir().expect("recordings directory");

        // Oldest first, so `newest` really is the newest timestamp.
        let oldest = insert_base_entry(&conn, 100, "oldest");
        let middle = insert_base_entry(&conn, 200, "middle");
        let newest = insert_base_entry(&conn, 300, "newest");
        let starred_and_oldest = insert_base_entry(&conn, 50, "starred");
        conn.execute(
            "UPDATE transcription_history SET saved = 1 WHERE id = ?1",
            params![starred_and_oldest],
        )
        .expect("star the oldest row");

        let deleted = HistoryManager::sweep_by_count_with_connection(&conn, recordings.path(), 2)
            .expect("count sweep runs");

        assert_eq!(
            deleted,
            vec![oldest],
            "three unsaved rows at a limit of two loses one"
        );
        assert!(row_exists(&conn, newest));
        assert!(row_exists(&conn, middle));
        assert!(!row_exists(&conn, oldest), "the oldest unsaved row goes");
        assert!(
            row_exists(&conn, starred_and_oldest),
            "a starred row is exempt and does not occupy one of the two places"
        );
    }

    /// A limit the corpus has not reached must not touch the database at all.
    #[test]
    fn a_count_sweep_under_its_limit_deletes_nothing() {
        let conn = setup_conn();
        let recordings = tempfile::tempdir().expect("recordings directory");
        let only = insert_base_entry(&conn, 100, "the only dictation");

        let deleted = HistoryManager::sweep_by_count_with_connection(&conn, recordings.path(), 5)
            .expect("count sweep runs");

        assert!(deleted.is_empty());
        assert!(row_exists(&conn, only));
    }

    fn row_exists(conn: &Connection, id: i64) -> bool {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM transcription_history WHERE id = ?1)",
            params![id],
            |row| row.get(0),
        )
        .expect("read row existence")
    }

    fn row_has_audio(conn: &Connection, id: i64) -> bool {
        conn.query_row(
            "SELECT has_audio FROM transcription_history WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .expect("read row audio flag")
    }

    fn seed_wav_retention_entry(
        conn: &Connection,
        recordings_dir: &std::path::Path,
        timestamp: i64,
        file_name: &str,
        text: &str,
        saved: bool,
        parent_id: Option<i64>,
    ) -> i64 {
        let wav_path = recordings_dir.join(file_name);
        crate::audio_toolkit::save_wav_file(&wav_path, &[0.0; 1_600])
            .expect("write history WAV fixture");
        assert_eq!(
            crate::audio_toolkit::read_wav_samples(&wav_path)
                .expect("read history WAV fixture")
                .len(),
            1_600
        );

        let id = insert_base_entry(conn, timestamp, text);
        conn.execute(
            "UPDATE transcription_history
             SET file_name = ?1, saved = ?2, parent_id = ?3
             WHERE id = ?4",
            params![file_name, saved, parent_id, id],
        )
        .expect("seed retained history entry");
        id
    }

    #[test]
    fn file_backed_count_retention_keeps_newest_unsaved_and_shared_parent_audio() {
        let conn = setup_conn();
        let recordings = tempfile::tempdir().expect("recordings directory");
        let oldest = seed_wav_retention_entry(
            &conn,
            recordings.path(),
            100,
            "oldest.wav",
            "oldest unsaved text",
            false,
            None,
        );
        let parent = seed_wav_retention_entry(
            &conn,
            recordings.path(),
            200,
            "shared.wav",
            "expired parent text",
            false,
            None,
        );
        let retained_child = seed_wav_retention_entry(
            &conn,
            recordings.path(),
            250,
            "shared.wav",
            "saved child text",
            true,
            Some(parent),
        );
        let newest = seed_wav_retention_entry(
            &conn,
            recordings.path(),
            300,
            "newest.wav",
            "newest unsaved text",
            false,
            None,
        );

        let deleted = HistoryManager::sweep_by_count_with_connection(&conn, recordings.path(), 1)
            .expect("run count retention");

        // Newest first: the sweep walks the log in the order it survives.
        assert_eq!(deleted, vec![parent, oldest]);
        assert!(!row_exists(&conn, oldest));
        assert!(!row_exists(&conn, parent));
        assert!(row_exists(&conn, retained_child));
        assert!(row_exists(&conn, newest));
        assert_eq!(
            conn.query_row(
                "SELECT transcription_text FROM transcription_history WHERE id = ?1",
                params![retained_child],
                |row| row.get::<_, String>(0),
            )
            .expect("saved child remains"),
            "saved child text"
        );
        assert!(!recordings.path().join("oldest.wav").exists());
        assert!(recordings.path().join("shared.wav").exists());
        assert!(recordings.path().join("newest.wav").exists());
    }

    #[test]
    fn file_backed_expiry_keeps_saved_boundary_and_shared_parent_audio() {
        const NOW: i64 = 1_788_417_783;
        let conn = setup_conn();
        let recordings = tempfile::tempdir().expect("recordings directory");
        let cutoff = recording_expiry_cutoff(crate::settings::RecordingRetentionPeriod::Days3, NOW)
            .expect("three days has a cutoff");
        let expired = seed_wav_retention_entry(
            &conn,
            recordings.path(),
            cutoff - 1,
            "expired.wav",
            "expired unsaved text",
            false,
            None,
        );
        let saved = seed_wav_retention_entry(
            &conn,
            recordings.path(),
            cutoff - 1,
            "saved.wav",
            "saved old text",
            true,
            None,
        );
        let boundary = seed_wav_retention_entry(
            &conn,
            recordings.path(),
            cutoff,
            "boundary.wav",
            "boundary text",
            false,
            None,
        );
        let parent = seed_wav_retention_entry(
            &conn,
            recordings.path(),
            cutoff - 1,
            "shared.wav",
            "expired parent text",
            false,
            None,
        );
        let shared_child = seed_wav_retention_entry(
            &conn,
            recordings.path(),
            cutoff,
            "shared.wav",
            "recent child text",
            false,
            Some(parent),
        );

        let expired_entries =
            HistoryManager::expire_recordings_with_connection(&conn, recordings.path(), cutoff)
                .expect("run controlled audio expiry");

        assert_eq!(
            expired_entries
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            vec![expired, parent]
        );
        for (id, text, has_audio) in [
            (expired, "expired unsaved text", false),
            (parent, "expired parent text", false),
            (saved, "saved old text", true),
            (boundary, "boundary text", true),
            (shared_child, "recent child text", true),
        ] {
            assert_eq!(
                conn.query_row(
                    "SELECT transcription_text FROM transcription_history WHERE id = ?1",
                    params![id],
                    |row| row.get::<_, String>(0),
                )
                .expect("retained history text"),
                text
            );
            assert_eq!(
                row_has_audio(&conn, id),
                has_audio,
                "audio flag of {text:?}"
            );
        }
        assert!(!recordings.path().join("expired.wav").exists());
        assert!(recordings.path().join("saved.wav").exists());
        assert!(recordings.path().join("boundary.wav").exists());
        assert!(
            recordings.path().join("shared.wav").exists(),
            "the recent child still plays the shared file"
        );
    }
    #[test]
    fn every_prior_schema_version_migrates_to_the_current_head() {
        for applied in 0..=MIGRATIONS.len() {
            let mut conn = Connection::open_in_memory().expect("open in-memory db");
            // `applied == 0` is a database this app has never touched; there is
            // no prefix to apply and no row to carry.
            if applied > 0 {
                Migrations::new(MIGRATIONS[..applied].to_vec())
                    .to_latest(&mut conn)
                    .unwrap_or_else(|error| panic!("apply first {applied} migrations: {error}"));
                insert_base_entry(&conn, 100, "carried across migrations");
            }

            Migrations::new(MIGRATIONS.to_vec())
                .to_latest(&mut conn)
                .unwrap_or_else(|error| panic!("migrate from version {applied}: {error}"));

            let version: i32 = conn
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .expect("read user_version");
            assert_eq!(version as usize, MIGRATIONS.len());
            assert!(!history_columns(&conn).contains(&"post_process_prompt".to_string()));

            let carried = HistoryManager::get_latest_completed_entry_with_conn(&conn)
                .expect("read carried entry");
            if applied > 0 {
                let carried = carried.expect("entry survives migration");
                assert_eq!(carried.transcription_text, "carried across migrations");
                let page = HistoryManager::search_history_entries_with_connection(
                    &conn,
                    "carried",
                    None,
                    Some(5),
                    None,
                )
                .expect("search carried entry");
                assert_eq!(page.entries.len(), 1);
            } else {
                assert!(carried.is_none());
            }
        }
    }

    #[test]
    fn retries_append_immutable_run_and_delivery_records() {
        let mut conn = setup_conn();
        let history_id = insert_entry(&conn, 100, "original raw text", Some("original output"));
        let first = new_run_receipt(1);
        let second = new_run_receipt(2);
        let transaction = conn.transaction().expect("begin receipt transaction");
        let first_id = HistoryManager::insert_run_receipt(&transaction, history_id, None, &first)
            .expect("insert initial run");
        let second_id =
            HistoryManager::insert_run_receipt(&transaction, history_id, Some(1), &second)
                .expect("append retry run");
        HistoryManager::insert_delivery_attempt(
            &transaction,
            history_id,
            second_id,
            &DeliveryReceipt {
                method: DeliveryMethod::ClipboardPaste,
                outcome: DeliveryOutcome::DispatchedButUnconfirmed,
                dispatched_at_ms: 30,
            },
        )
        .expect("append delivery observation");
        transaction.commit().expect("commit receipt transaction");

        assert_ne!(first_id, second_id);
        assert_eq!(
            conn.query_row(
                "SELECT transcription_text FROM transcription_history WHERE id = ?1",
                params![history_id],
                |row| row.get::<_, String>(0),
            )
            .expect("read immutable original text"),
            "original raw text"
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM transcription_runs WHERE history_id = ?1",
                params![history_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("count appended runs"),
            2
        );
        let attempts = HistoryManager::get_delivery_attempts_with_connection(&conn, second_id)
            .expect("read delivery attempts");
        assert_eq!(attempts.len(), 1);
        assert_eq!(
            attempts[0].delivery.outcome,
            DeliveryOutcome::DispatchedButUnconfirmed
        );

        let receipts = conn
            .query_row(
                "SELECT mode_receipt_json, context_receipt_json
                 FROM transcription_runs WHERE id = ?1",
                params![second_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .expect("read content-free receipts");
        assert!(!receipts.0.contains("private system prompt"));
        assert!(!receipts.1.contains("original raw text"));
        assert!(!receipts.1.contains("original output"));
    }

    #[test]
    fn get_latest_entry_returns_none_when_empty() {
        let conn = setup_conn();
        let entry = HistoryManager::get_latest_entry_with_conn(&conn).expect("fetch latest entry");
        assert!(entry.is_none());
    }

    #[test]
    fn get_latest_entry_returns_newest_entry() {
        let conn = setup_conn();
        insert_entry(&conn, 100, "first", None);
        insert_entry(&conn, 200, "second", Some("processed"));

        let entry = HistoryManager::get_latest_entry_with_conn(&conn)
            .expect("fetch latest entry")
            .expect("entry exists");

        assert_eq!(entry.timestamp, 200);
        assert_eq!(entry.transcription_text, "second");
        assert_eq!(entry.post_processed_text.as_deref(), Some("processed"));
    }

    #[test]
    fn get_latest_completed_entry_skips_empty_entries() {
        let conn = setup_conn();
        insert_entry(&conn, 100, "completed", None);
        insert_entry(&conn, 200, "", None);

        let entry = HistoryManager::get_latest_completed_entry_with_conn(&conn)
            .expect("fetch latest completed entry")
            .expect("completed entry exists");

        assert_eq!(entry.timestamp, 100);
        assert_eq!(entry.transcription_text, "completed");
    }

    #[test]
    fn search_history_entries_matches_raw_and_post_processed_text_with_pagination() {
        let conn = setup_conn();
        let raw_id = insert_entry(&conn, 100, "project lantern notes", None);
        let post_processed_id = insert_entry(
            &conn,
            200,
            "unrelated draft",
            Some("polished lantern summary"),
        );
        let latest_raw_id = insert_entry(&conn, 300, "lantern action items", None);

        let first_page = HistoryManager::search_history_entries_with_connection(
            &conn,
            "lantern",
            None,
            Some(2),
            None,
        )
        .expect("search first page");

        assert_eq!(
            first_page
                .entries
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            vec![latest_raw_id, post_processed_id]
        );
        assert!(first_page.has_more);

        let second_page = HistoryManager::search_history_entries_with_connection(
            &conn,
            "lantern",
            first_page.entries.last().map(|entry| entry.id),
            Some(2),
            None,
        )
        .expect("search second page");

        assert_eq!(
            second_page
                .entries
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            vec![raw_id]
        );
        assert!(!second_page.has_more);

        let post_processed_only = HistoryManager::search_history_entries_with_connection(
            &conn,
            "polished",
            None,
            Some(2),
            None,
        )
        .expect("search post-processed text");
        assert_eq!(post_processed_only.entries[0].id, post_processed_id);
    }

    #[test]
    fn fts_match_query_quotes_tokens_and_drops_untokenizable_ones() {
        assert_eq!(fts_match_query("note:").as_deref(), Some("\"note:\"*"));
        assert_eq!(
            fts_match_query("  lantern   notes ").as_deref(),
            Some("\"lantern\"* \"notes\"*")
        );
        assert_eq!(
            fts_match_query("lantern AND").as_deref(),
            Some("\"lantern\"* \"AND\"*")
        );
        assert_eq!(
            fts_match_query("say \"hi").as_deref(),
            Some("\"say\"* \"\"\"hi\"*")
        );
        assert_eq!(
            fts_match_query("say \"hi\"").as_deref(),
            Some("\"say\"* \"\"\"hi\"\"\"*")
        );
        assert_eq!(fts_match_query(""), None);
        assert_eq!(fts_match_query("   \t\n "), None);
        assert_eq!(fts_match_query("🎉 🙃"), None);
        assert_eq!(fts_match_query("-- ,"), None);
        assert_eq!(
            fts_match_query("🎉 lantern").as_deref(),
            Some("\"lantern\"*")
        );
    }

    #[test]
    fn search_treats_fts_operators_and_quotes_as_literal_text() {
        let conn = setup_conn();
        let colon_id = insert_entry(&conn, 100, "note: remember the lantern", None);
        let operator_id = insert_entry(&conn, 200, "lantern AND torch inventory", None);
        let quoted_id = insert_entry(&conn, 300, "she said \"hi\" twice", None);
        insert_entry(&conn, 400, "unrelated draft", None);

        // Each of these was an error dialog before the query was quoted:
        // `no such column: note`, `fts5: syntax error near "AND"`, and an
        // unterminated string.
        for (query, expected) in [
            ("note:", vec![colon_id]),
            ("note: remember", vec![colon_id]),
            ("lantern AND", vec![operator_id]),
            ("NEAR(lantern torch", vec![]),
            ("said \"hi", vec![quoted_id]),
            ("\"hi\"", vec![quoted_id]),
            ("^lantern*", vec![operator_id, colon_id]),
            ("(inventory)", vec![operator_id]),
            ("\" OR transcription_text MATCH \"lantern", vec![]),
            ("lantern' OR 1=1 --", vec![]),
        ] {
            let page = HistoryManager::search_history_entries_with_connection(
                &conn,
                query,
                None,
                Some(5),
                None,
            )
            .unwrap_or_else(|error| panic!("search {query:?} must not fail: {error}"));
            assert_eq!(
                page.entries
                    .iter()
                    .map(|entry| entry.id)
                    .collect::<Vec<_>>(),
                expected,
                "unexpected rows for {query:?}"
            );
        }
    }

    #[test]
    fn search_matches_word_prefixes_and_requires_every_token() {
        let conn = setup_conn();
        let entry_id = insert_entry(&conn, 100, "project lantern notes", None);

        for query in ["lant", "lantern not", "notes lant"] {
            let page = HistoryManager::search_history_entries_with_connection(
                &conn,
                query,
                None,
                Some(5),
                None,
            )
            .unwrap_or_else(|error| panic!("search {query:?}: {error}"));
            assert_eq!(
                page.entries
                    .iter()
                    .map(|entry| entry.id)
                    .collect::<Vec<_>>(),
                vec![entry_id],
                "unexpected rows for {query:?}"
            );
        }

        let missing_token = HistoryManager::search_history_entries_with_connection(
            &conn,
            "lantern zebra",
            None,
            Some(5),
            None,
        )
        .expect("search with one absent token");
        assert!(missing_token.entries.is_empty());
    }

    #[test]
    fn search_without_a_searchable_token_never_reaches_match() {
        let conn = setup_conn();
        insert_entry(&conn, 100, "project lantern notes", None);
        // Removing the index makes any MATCH fail, so an empty page here proves
        // the untokenizable query was answered without touching FTS at all.
        conn.execute("DROP TABLE transcription_history_fts", [])
            .expect("drop search index");

        for query in ["", "   ", "🎉", "--"] {
            let page = HistoryManager::search_history_entries_with_connection(
                &conn,
                query,
                None,
                Some(5),
                None,
            )
            .unwrap_or_else(|error| panic!("search {query:?} must not query FTS: {error}"));
            assert!(page.entries.is_empty());
            assert!(!page.has_more);
        }

        assert!(
            HistoryManager::search_history_entries_with_connection(
                &conn,
                "lantern",
                None,
                Some(5),
                None
            )
            .is_err(),
            "a real token must still reach the (now missing) index"
        );
    }

    #[test]
    fn recording_path_accepts_only_bare_file_names() {
        let recordings_dir = Path::new("/sona/recordings");

        assert_eq!(
            recording_path(recordings_dir, "sona-100.wav"),
            Some(recordings_dir.join("sona-100.wav"))
        );
        assert_eq!(
            recording_path(recordings_dir, "meeting notes.mp3"),
            Some(recordings_dir.join("meeting notes.mp3"))
        );
        for escape in [
            "",
            ".",
            "..",
            "../sona-100.wav",
            "../../etc/passwd",
            "nested/sona-100.wav",
            "/etc/passwd",
            "/sona/recordings/sona-100.wav",
        ] {
            assert_eq!(
                recording_path(recordings_dir, escape),
                None,
                "{escape:?} must not resolve"
            );
        }
    }

    #[test]
    fn search_history_entries_caps_page_size() {
        let conn = setup_conn();
        for timestamp in std::iter::successors(Some(0_i64), |timestamp| timestamp.checked_add(1))
            .take(MAX_HISTORY_PAGE_SIZE + 1)
        {
            insert_entry(&conn, timestamp, "bounded search", None);
        }

        let page = HistoryManager::search_history_entries_with_connection(
            &conn,
            "bounded",
            None,
            Some(MAX_HISTORY_PAGE_SIZE + 1),
            None,
        )
        .expect("search bounded page");

        assert_eq!(page.entries.len(), MAX_HISTORY_PAGE_SIZE);
        assert!(page.has_more);
    }

    #[test]
    fn fts_migration_indexes_existing_history_entries() {
        let mut conn = Connection::open_in_memory().expect("open in-memory db");
        Migrations::new(MIGRATIONS[..4].to_vec())
            .to_latest(&mut conn)
            .expect("apply pre-search migrations");
        let entry_id = insert_entry(&conn, 100, "legacy indexed text", None);

        Migrations::new(MIGRATIONS.to_vec())
            .to_latest(&mut conn)
            .expect("apply search migration");

        let page = HistoryManager::search_history_entries_with_connection(
            &conn,
            "legacy",
            None,
            Some(1),
            None,
        )
        .expect("search migrated entry");

        assert_eq!(page.entries[0].id, entry_id);
    }

    #[test]
    fn search_index_tracks_transcription_updates() {
        let conn = setup_conn();
        let entry_id = insert_entry(&conn, 100, "original recording", None);
        conn.execute(
            "UPDATE transcription_history
             SET transcription_text = ?1, post_processed_text = ?2
             WHERE id = ?3",
            params!["updated recording", "revised summary", entry_id],
        )
        .expect("update history entry");

        let page = HistoryManager::search_history_entries_with_connection(
            &conn,
            "revised",
            None,
            Some(1),
            None,
        )
        .expect("search updated post-processed text");
        assert_eq!(page.entries[0].id, entry_id);

        let stale = HistoryManager::search_history_entries_with_connection(
            &conn,
            "original",
            None,
            Some(1),
            None,
        )
        .expect("search stale transcription text");
        assert!(stale.entries.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn failed_wav_deletion_keeps_history_entry_for_retry() {
        use std::os::unix::fs::PermissionsExt;

        let conn = setup_conn();
        let entry_id = insert_entry(&conn, 100, "keep this entry", None);
        let recordings = tempfile::tempdir().expect("recordings directory");
        let file_name = "sona-100.wav".to_string();
        let wav_path = recordings.path().join(&file_name);
        crate::audio_toolkit::save_wav_file(&wav_path, &[0.0; 1_600]).expect("write protected WAV");
        assert_eq!(
            crate::audio_toolkit::read_wav_samples(&wav_path)
                .expect("read protected WAV")
                .len(),
            1_600
        );
        let original_permissions = std::fs::metadata(recordings.path())
            .expect("read recordings permissions")
            .permissions();
        let mut protected_permissions = original_permissions.clone();
        protected_permissions.set_mode(0o500);
        std::fs::set_permissions(recordings.path(), protected_permissions)
            .expect("make recordings directory non-removable");

        let entries = vec![(entry_id, file_name.clone())];
        let cleanup = HistoryManager::delete_entries_and_files_with_connection(
            &conn,
            recordings.path(),
            &entries,
        );
        std::fs::set_permissions(recordings.path(), original_permissions)
            .expect("restore recordings permissions");
        assert!(cleanup.expect("attempt cleanup").is_empty());
        assert!(row_exists(&conn, entry_id));
        assert!(wav_path.exists());

        let deleted = HistoryManager::delete_entries_and_files_with_connection(
            &conn,
            recordings.path(),
            &entries,
        )
        .expect("retry cleanup");
        assert_eq!(deleted, vec![entry_id]);
        assert!(!row_exists(&conn, entry_id));
        assert!(!wav_path.exists());
    }
    #[test]
    fn shared_retry_audio_is_deleted_only_after_the_last_history_row() {
        let conn = setup_conn();
        let original_id = insert_entry(&conn, 100, "original", None);
        let retry_id = insert_entry(&conn, 200, "retry", None);
        let file_name = "sona-shared.wav";
        conn.execute(
            "UPDATE transcription_history SET file_name = ?1 WHERE id IN (?2, ?3)",
            params![file_name, original_id, retry_id],
        )
        .expect("make retry rows share one recording");

        let recordings = tempfile::tempdir().expect("recordings directory");
        let wav_path = recordings.path().join(file_name);
        crate::audio_toolkit::save_wav_file(&wav_path, &[0.0; 1_600]).expect("write shared WAV");
        assert_eq!(
            crate::audio_toolkit::read_wav_samples(&wav_path)
                .expect("read shared WAV")
                .len(),
            1_600
        );

        let deleted = HistoryManager::delete_entries_and_files_with_connection(
            &conn,
            recordings.path(),
            &[(original_id, file_name.to_string())],
        )
        .expect("delete original history row");
        assert_eq!(deleted, vec![original_id]);
        assert!(wav_path.exists());

        let deleted = HistoryManager::delete_entries_and_files_with_connection(
            &conn,
            recordings.path(),
            &[(retry_id, file_name.to_string())],
        )
        .expect("delete final retry row");
        assert_eq!(deleted, vec![retry_id]);
        assert!(!wav_path.exists());
    }

    /// The pinned model, or `None` when it has not been fetched here.
    fn semantic_fixture_model() -> Option<semantic::SemanticModel> {
        semantic::tests::fixture_model()
    }

    fn embed_all(conn: &mut Connection, model: &semantic::SemanticModel) -> usize {
        let mut total = 0;
        loop {
            let written =
                backfill_semantic_chunk_with_connection(conn, model, semantic::BACKFILL_CHUNK_ROWS)
                    .expect("backfill chunk");
            if written == 0 {
                return total;
            }
            total += written;
        }
    }

    /// The whole reason this slice exists, in one test.
    ///
    /// A transcript that says "the budget for August" is unreachable by the
    /// query "spending plan" through FTS5: the two share no token, and FTS5
    /// joins query tokens with implicit AND, so the miss is structural rather
    /// than a stale index. The first assertion proves that miss on the real
    /// index instead of assuming it. The second proves the embedding closes it.
    #[test]
    fn a_paraphrase_is_recalled_where_fts_structurally_cannot_match() {
        let Some(model) = semantic_fixture_model() else {
            eprintln!("skipped: pinned semantic model not present");
            return;
        };
        let mut conn = setup_conn();
        let budget_id = insert_entry(&conn, 100, "the budget for August", None);
        insert_entry(&conn, 101, "sourdough starter feeding schedule", None);
        insert_entry(
            &conn,
            102,
            "guitar amplifier settings for the small room",
            None,
        );
        assert_eq!(embed_all(&mut conn, &model), 3);

        // The FTS miss, proven: the row exists, the index is current (the same
        // query finds it by a literal word), and "spending plan" still cannot
        // reach it.
        let literal = HistoryManager::search_history_entries_with_connection(
            &conn,
            "budget",
            None,
            Some(5),
            None,
        )
        .expect("literal search");
        assert_eq!(
            literal.entries.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![budget_id],
            "the index must be current for the miss below to mean anything"
        );
        let lexical_only = HistoryManager::search_history_entries_with_connection(
            &conn,
            "spending plan",
            None,
            Some(5),
            None,
        )
        .expect("lexical search");
        assert!(
            lexical_only.entries.is_empty(),
            "FTS5 must miss the paraphrase, but returned {:?}",
            lexical_only
                .entries
                .iter()
                .map(|e| e.id)
                .collect::<Vec<_>>()
        );

        let blended = HistoryManager::search_history_entries_with_connection(
            &conn,
            "spending plan",
            None,
            Some(5),
            Some(&model),
        )
        .expect("blended search");
        assert_eq!(
            blended.entries.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![budget_id],
            "semantic recall must find the paraphrase, and only it"
        );
        assert_eq!(
            blended.entries[0].match_kind,
            Some(HistoryMatchKind::Semantic),
            "a row FTS did not match must be labelled semantic"
        );
    }

    /// Lexical evidence outranks embedding similarity for the same row: a row
    /// FTS5 matched is reported as `Text` and is not also emitted as a semantic
    /// candidate, even though its own embedding trivially clears the floor
    /// against its own words.
    #[test]
    fn a_row_fts_matched_is_labelled_text_and_never_duplicated() {
        let Some(model) = semantic_fixture_model() else {
            eprintln!("skipped: pinned semantic model not present");
            return;
        };
        let mut conn = setup_conn();
        let id = insert_entry(&conn, 100, "the budget for August", None);
        embed_all(&mut conn, &model);

        let page = HistoryManager::search_history_entries_with_connection(
            &conn,
            "budget",
            None,
            Some(5),
            Some(&model),
        )
        .expect("blended search");
        assert_eq!(page.entries.len(), 1, "the row must appear exactly once");
        assert_eq!(page.entries[0].id, id);
        assert_eq!(page.entries[0].match_kind, Some(HistoryMatchKind::Text));
    }

    /// A row the model has not reached, and a search with no model at all, both
    /// degrade to exactly the lexical result — no error, no empty page, no
    /// invented match.
    #[test]
    fn an_unembedded_row_and_an_absent_model_both_degrade_to_lexical() {
        let conn = setup_conn();
        let id = insert_entry(&conn, 100, "the budget for August", None);

        for model in [None, semantic_fixture_model().as_ref()] {
            let page = HistoryManager::search_history_entries_with_connection(
                &conn,
                "budget",
                None,
                Some(5),
                model,
            )
            .expect("search must not fail without embeddings");
            assert_eq!(
                page.entries.iter().map(|e| e.id).collect::<Vec<_>>(),
                vec![id]
            );
            assert_eq!(page.entries[0].match_kind, Some(HistoryMatchKind::Text));

            let paraphrase = HistoryManager::search_history_entries_with_connection(
                &conn,
                "spending plan",
                None,
                Some(5),
                model,
            )
            .expect("paraphrase search must not fail without embeddings");
            assert!(
                paraphrase.entries.is_empty(),
                "an unembedded row must not be recalled"
            );
        }
    }

    /// The backfill is resumable because its selection predicate is its
    /// progress record, and it terminates because an unembeddable row is still
    /// marked as considered.
    #[test]
    fn the_backfill_resumes_in_chunks_and_terminates_on_unembeddable_rows() {
        let Some(model) = semantic_fixture_model() else {
            eprintln!("skipped: pinned semantic model not present");
            return;
        };
        let mut conn = setup_conn();
        for index in 0..5_i64 {
            insert_entry(&conn, index, &format!("meeting notes number {index}"), None);
        }
        // A row with nothing to embed: whitespace tokenizes to no term.
        let blank_id = insert_entry(&conn, 5, "   ", None);

        // Chunked: two rows at a time, and the pass advances every time.
        assert_eq!(
            backfill_semantic_chunk_with_connection(&mut conn, &model, 2).expect("chunk one"),
            2
        );
        assert_eq!(
            backfill_semantic_chunk_with_connection(&mut conn, &model, 2).expect("chunk two"),
            2
        );
        assert_eq!(
            backfill_semantic_chunk_with_connection(&mut conn, &model, 2).expect("chunk three"),
            2
        );
        // Terminates: nothing is left, including the blank row.
        assert_eq!(
            backfill_semantic_chunk_with_connection(&mut conn, &model, 2).expect("chunk four"),
            0
        );

        let (embedded, considered): (i64, i64) = conn
            .query_row(
                "SELECT
                    COUNT(semantic_embedding),
                    COUNT(semantic_model_revision)
                 FROM transcription_history",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("count embeddings");
        assert_eq!(considered, 6, "every row must be marked as considered");
        assert_eq!(embedded, 5, "the unembeddable row must hold no vector");

        let blank: Option<Vec<u8>> = conn
            .query_row(
                "SELECT semantic_embedding FROM transcription_history WHERE id = ?1",
                params![blank_id],
                |row| row.get(0),
            )
            .expect("read blank row");
        assert!(blank.is_none(), "honest absence, not a zero vector");
    }

    /// A vector round-trips through the BLOB column, survives the migration
    /// path that created the column, and writing it does not disturb the FTS5
    /// index (the triggers name only the two text columns).
    #[test]
    fn a_vector_round_trips_through_the_migrated_column_without_touching_fts() {
        let mut conn = Connection::open_in_memory().expect("open in-memory db");
        // Absolute for the same reason as `MIGRATIONS_BEFORE_PARENT_ID` above:
        // this is the schema an existing install has before the embedding
        // columns arrive, not "whatever the second-to-last migration is".
        const MIGRATIONS_BEFORE_SEMANTIC: usize = 11;
        Migrations::new(MIGRATIONS[..MIGRATIONS_BEFORE_SEMANTIC].to_vec())
            .to_latest(&mut conn)
            .expect("apply pre-semantic migrations");
        let id = insert_entry(&conn, 100, "quarterly planning notes", None);
        Migrations::new(MIGRATIONS.to_vec())
            .to_latest(&mut conn)
            .expect("apply semantic migration");

        let fts_rows_before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM transcription_history_fts",
                [],
                |row| row.get(0),
            )
            .expect("count fts rows");

        let vector: Vec<f32> = (0..256).map(|lane| (lane as f32 - 128.0) / 256.0).collect();
        store_semantic_vector_with_connection(&conn, id, Some(&vector), "test-revision")
            .expect("store vector");

        let (stored, revision): (Vec<u8>, String) = conn
            .query_row(
                "SELECT semantic_embedding, semantic_model_revision
                 FROM transcription_history WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read back vector");
        assert_eq!(revision, "test-revision");
        assert_eq!(stored, semantic::encode_vector(&vector));
        assert_eq!(
            semantic::cosine_similarity(&stored, &vector),
            Some(vector.iter().map(|v| v * v).sum::<f32>()),
            "the round-tripped lanes must reproduce the exact dot product"
        );

        let fts_rows_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM transcription_history_fts",
                [],
                |row| row.get(0),
            )
            .expect("count fts rows");
        assert_eq!(
            fts_rows_before, fts_rows_after,
            "writing a vector must not re-index the row"
        );
        let page = HistoryManager::search_history_entries_with_connection(
            &conn,
            "quarterly",
            None,
            Some(5),
            None,
        )
        .expect("lexical search after vector write");
        assert_eq!(
            page.entries.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![id]
        );
    }

    /// A vector written by a different model revision is skipped, not compared:
    /// same width, no shared meaning.
    #[test]
    fn a_vector_from_another_model_revision_is_not_compared() {
        let Some(model) = semantic_fixture_model() else {
            eprintln!("skipped: pinned semantic model not present");
            return;
        };
        let mut conn = setup_conn();
        let id = insert_entry(&conn, 100, "the budget for August", None);
        embed_all(&mut conn, &model);
        let found = HistoryManager::search_history_entries_with_connection(
            &conn,
            "spending plan",
            None,
            Some(5),
            Some(&model),
        )
        .expect("blended search");
        assert_eq!(
            found.entries.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![id]
        );

        conn.execute(
            "UPDATE transcription_history SET semantic_model_revision = 'some-other-revision'",
            [],
        )
        .expect("restamp the vector");
        let stale = HistoryManager::search_history_entries_with_connection(
            &conn,
            "spending plan",
            None,
            Some(5),
            Some(&model),
        )
        .expect("blended search over stale vectors");
        assert!(
            stale.entries.is_empty(),
            "a vector from another revision must be skipped"
        );
    }

    /// The blend is deterministic and paginates: repeated identical calls give
    /// byte-identical pages, and walking the cursor visits every matching row
    /// exactly once with no repeats and no gaps, across the lexical/semantic
    /// boundary.
    #[test]
    fn the_blend_is_deterministic_and_paginates_without_repeats_or_gaps() {
        let Some(model) = semantic_fixture_model() else {
            eprintln!("skipped: pinned semantic model not present");
            return;
        };
        let mut conn = setup_conn();
        // Two rows the query reaches lexically, three only by paraphrase, and
        // two that must never appear. Interleaved ids so a merge that sorted by
        // anything other than id would show up as a different order.
        let mut expected = Vec::new();
        expected.push(insert_entry(&conn, 1, "the budget for August", None));
        insert_entry(&conn, 2, "sourdough starter feeding schedule", None);
        expected.push(insert_entry(
            &conn,
            3,
            "spending plan for the quarter",
            None,
        ));
        expected.push(insert_entry(&conn, 4, "the budget for September", None));
        insert_entry(&conn, 5, "guitar amplifier settings", None);
        expected.push(insert_entry(
            &conn,
            6,
            "how much we can spend next month",
            None,
        ));
        embed_all(&mut conn, &model);

        let page_of = |cursor: Option<i64>| {
            HistoryManager::search_history_entries_with_connection(
                &conn,
                "spending plan",
                cursor,
                Some(2),
                Some(&model),
            )
            .expect("blended page")
        };

        // Deterministic: the same inputs give the same ids, order, and labels.
        let first = page_of(None);
        for _ in 0..4 {
            let again = page_of(None);
            assert_eq!(
                again
                    .entries
                    .iter()
                    .map(|e| (e.id, e.match_kind))
                    .collect::<Vec<_>>(),
                first
                    .entries
                    .iter()
                    .map(|e| (e.id, e.match_kind))
                    .collect::<Vec<_>>()
            );
            assert_eq!(again.has_more, first.has_more);
        }

        // Paginates: walk to exhaustion and check the whole walk.
        let mut seen = Vec::new();
        let mut cursor = None;
        for _ in 0..8 {
            let page = page_of(cursor);
            assert!(page.entries.len() <= 2, "page size must be honored");
            for entry in &page.entries {
                seen.push(entry.id);
            }
            if !page.has_more {
                break;
            }
            cursor = page.entries.last().map(|entry| entry.id);
        }

        let mut sorted = seen.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            seen.len(),
            "a row was returned twice: {seen:?}"
        );
        assert!(
            seen.windows(2).all(|pair| pair[0] > pair[1]),
            "the merged walk must stay strictly newest-first: {seen:?}"
        );
        expected.sort_unstable();
        let mut found = seen.clone();
        found.sort_unstable();
        assert_eq!(found, expected, "every matching row must be visited once");
    }

    /// When FTS5 alone overflows the page, the semantic half is not consulted
    /// at all: no query embedding, no scan, no row.
    ///
    /// This is what "lexical wins" costs and buys. A query with a full page of
    /// literal matches pays nothing for semantic recall — and, as the second
    /// half of this test pins, a semantic hit newer than the page's last id is
    /// then never shown for that query. That is deliberate: the merged list is
    /// strictly newest-first so the single-id cursor stays sound, and a user
    /// whose query already returns a full page of literal matches is not the
    /// user this feature exists for.
    #[test]
    fn a_lexically_overflowing_page_skips_the_semantic_half_entirely() {
        let Some(model) = semantic_fixture_model() else {
            eprintln!("skipped: pinned semantic model not present");
            return;
        };
        let mut conn = setup_conn();
        let lexical = [
            insert_entry(&conn, 1, "spending plan review", None),
            insert_entry(&conn, 2, "spending plan draft", None),
            insert_entry(&conn, 3, "spending plan final", None),
        ];
        // Newer than every lexical row, and reachable only by meaning.
        let semantic_row = insert_entry(&conn, 4, "the budget for August", None);
        embed_all(&mut conn, &model);

        let page = HistoryManager::search_history_entries_with_connection(
            &conn,
            "spending plan",
            None,
            Some(2),
            Some(&model),
        )
        .expect("blended search");
        assert_eq!(
            page.entries.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![lexical[2], lexical[1]],
            "an overflowing lexical page must be returned exactly as FTS ranked it"
        );
        assert!(page.has_more);
        assert!(
            page.entries
                .iter()
                .all(|entry| entry.match_kind == Some(HistoryMatchKind::Text)),
            "no semantic row may enter a page FTS already filled"
        );

        // Walking to exhaustion: every lexical row is reached, and the newer
        // semantic row stays out because the cursor has already passed it.
        let mut seen = Vec::new();
        let mut cursor = None;
        for _ in 0..6 {
            let next = HistoryManager::search_history_entries_with_connection(
                &conn,
                "spending plan",
                cursor,
                Some(2),
                Some(&model),
            )
            .expect("blended page");
            seen.extend(next.entries.iter().map(|entry| entry.id));
            if !next.has_more {
                break;
            }
            cursor = next.entries.last().map(|entry| entry.id);
        }
        let mut reached = seen.clone();
        reached.sort_unstable();
        reached.dedup();
        assert_eq!(reached.len(), seen.len(), "no row may repeat: {seen:?}");
        assert_eq!(
            reached,
            lexical.to_vec(),
            "every lexical row is reached, and only those"
        );
        assert!(
            !seen.contains(&semantic_row),
            "a semantic hit newer than the first page's cursor is not resurfaced"
        );
    }
}

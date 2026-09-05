use crate::fs_util::{copy_verified, files_equal, hex_digest, write_private_file};
use crate::managers::history::{HistoryManager, HistorySourceKind, UpstreamHistoryImportEntry};
use crate::secrets::{
    migrate_upstream_legacy_llm_secrets, SecretManager, UpstreamSecretImportStatus,
};
use crate::settings::{
    self, decode_settings_backup, decode_upstream_import_settings, get_settings,
    merge_upstream_import_settings, SettingsDocument,
};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specta::Type;
use std::collections::HashSet;
use std::env;
use std::fs;
#[cfg(test)]
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, State};
use tauri_specta::Event;
const SOURCE_BUNDLE_ID: &str = "com.pais.handy";
const SOURCE_IDENTITY_LABEL: &str = "com.pais.handy";
const RECEIPT_FILE: &str = "upstream-import-receipt.json";
const SETTINGS_BACKUP_FILE: &str = "settings-pre-import-backup.json";

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamAppState {
    Closed,
    Running,
    Unverifiable,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Type)]
pub struct UpstreamImportSelection {
    pub settings: bool,
    pub history: bool,
    pub recordings: bool,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct UpstreamImportStatus {
    pub available: bool,
    pub app_state: UpstreamAppState,
    pub settings_available: bool,
    pub history_entries: u64,
    pub recording_files: u64,
    pub recording_bytes: u64,
    pub settings_imported: bool,
    pub settings_backup_available: bool,
    pub settings_backup_saved_at_ms: Option<u64>,
    pub history_imported: u64,
    pub recordings_imported: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamImportPhase {
    Settings,
    History,
    Recordings,
}

#[derive(Clone, Debug, Serialize, Type, tauri_specta::Event)]
pub struct UpstreamImportProgressEvent {
    pub phase: UpstreamImportPhase,
    pub completed: u64,
    pub total: u64,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct UpstreamImportResult {
    pub settings_imported: bool,
    pub history_imported: u64,
    pub history_existing: u64,
    pub recordings_copied: u64,
    pub recordings_existing: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamImportError {
    SourceUnavailable,
    UpstreamRunning,
    AppStateUnverifiable,
    InvalidSelection,
    SettingsUnreadable,
    SettingsBackupWriteFailed,
    SettingsBackupUnreadable,
    SettingsNotImported,
    SecretStoreUnavailable,
    SecretConflict,
    HistoryUnreadable,
    RecordingCopyFailed,
    ReceiptWriteFailed,
    Internal,
}

/// An import whose settings write never reached the disk imported nothing.
/// There is no narrower variant for it: every named failure above is a step of
/// the import, and this one is the store underneath them all.
impl From<crate::settings::SettingsPersistError> for UpstreamImportError {
    fn from(_: crate::settings::SettingsPersistError) -> Self {
        Self::Internal
    }
}

impl std::fmt::Display for UpstreamImportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for UpstreamImportError {}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct ImportReceipt {
    source_identity: String,
    settings_imported: bool,
    #[serde(default)]
    settings_backup_path: Option<String>,
    #[serde(default)]
    settings_backup_saved_at_ms: Option<u64>,
    history_imported: u64,
    recordings_imported: u64,
    completed_at_ms: u64,
}

struct ImportPaths {
    source_root: PathBuf,
    source_settings: PathBuf,
    source_history: PathBuf,
    source_recordings: PathBuf,
    destination_recordings: PathBuf,
    receipt: PathBuf,
    settings_backup: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SettingsImportBackup {
    saved_at_ms: u64,
    settings: SettingsDocument,
}

#[derive(Default)]
struct HistoryImportSummary {
    imported: u64,
    existing: u64,
    recordings_copied: u64,
    recordings_existing: u64,
}

struct HistoryRowHashInput<'a> {
    id: i64,
    file_name: &'a str,
    timestamp: i64,
    saved: bool,
    title: &'a str,
    text: &'a str,
    processed: Option<&'a str>,
    requested: bool,
}

fn validate_selection(selection: &UpstreamImportSelection) -> Result<(), UpstreamImportError> {
    if (!selection.settings && !selection.history && !selection.recordings)
        || (selection.recordings && !selection.history)
    {
        return Err(UpstreamImportError::InvalidSelection);
    }
    Ok(())
}

fn require_upstream_closed() -> Result<(), UpstreamImportError> {
    match upstream_app_state() {
        UpstreamAppState::Closed => Ok(()),
        UpstreamAppState::Running => Err(UpstreamImportError::UpstreamRunning),
        UpstreamAppState::Unverifiable => Err(UpstreamImportError::AppStateUnverifiable),
    }
}

fn emit_progress(app: &AppHandle, phase: UpstreamImportPhase, completed: u64, total: u64) {
    debug_assert!(completed <= total);
    if let Err(error) = (UpstreamImportProgressEvent {
        phase,
        completed,
        total,
    })
    .emit(app)
    {
        log::warn!("Failed to emit upstream import progress: {error}");
    }
}

async fn import_settings(
    app: &AppHandle,
    secrets: &SecretManager,
    paths: &ImportPaths,
    receipt: &mut ImportReceipt,
) -> Result<bool, UpstreamImportError> {
    if receipt.settings_imported {
        return Ok(false);
    }

    let mut source_settings = read_source_settings(&paths.source_settings)?;
    let target_settings = get_settings(app);
    let known_provider_ids: HashSet<String> = target_settings
        .post_process_providers
        .iter()
        .map(|provider| provider.id.clone())
        .collect();
    let migrated_provider_ids = match migrate_upstream_legacy_llm_secrets(
        &mut source_settings,
        secrets,
        &known_provider_ids,
    )
    .await
    {
        UpstreamSecretImportStatus::Complete {
            migrated_provider_ids,
        } => migrated_provider_ids,
        UpstreamSecretImportStatus::Conflict { .. } => {
            return Err(UpstreamImportError::SecretConflict)
        }
        UpstreamSecretImportStatus::Pending { .. }
        | UpstreamSecretImportStatus::PortableBlocked { .. } => {
            return Err(UpstreamImportError::SecretStoreUnavailable)
        }
    };
    let source_settings: SettingsDocument = serde_json::from_value(source_settings)
        .map_err(|_| UpstreamImportError::SettingsUnreadable)?;
    let imported = decode_upstream_import_settings(source_settings)
        .map_err(|_| UpstreamImportError::SettingsUnreadable)?;
    let saved_at_ms = now_ms();
    let backup_path = paths.settings_backup.clone();
    let receipt_path = paths.receipt.clone();
    let backup_path_for_receipt = backup_path.to_string_lossy().into_owned();
    let (snapshot, previous_bindings, current_bindings) =
        settings::try_update_settings(app, |target| {
            write_settings_backup(&backup_path, target, saved_at_ms)?;
            let merged = merge_upstream_import_settings(target, imported, &migrated_provider_ids);
            let snapshot = crate::modes::mode_settings_snapshot(&merged);
            let previous_bindings = target.bindings.clone();
            let current_bindings = merged.bindings.clone();
            *target = merged;
            receipt.settings_imported = true;
            receipt.settings_backup_path = Some(backup_path_for_receipt);
            receipt.settings_backup_saved_at_ms = Some(saved_at_ms);
            write_receipt(&receipt_path, receipt)?;
            Ok::<_, UpstreamImportError>((snapshot, previous_bindings, current_bindings))
        })?;

    crate::shortcut::reconcile_mode_shortcuts(app, &previous_bindings, &current_bindings);
    crate::modes::emit_modes_changed(app, &snapshot);
    Ok(true)
}

#[tauri::command]
#[specta::specta]
pub fn get_upstream_import_status(
    app: AppHandle,
) -> Result<UpstreamImportStatus, UpstreamImportError> {
    let paths = import_paths(&app)?;
    status_for_paths(&paths)
}

#[tauri::command]
#[specta::specta]
pub fn revert_upstream_import_settings(app: AppHandle) -> Result<(), UpstreamImportError> {
    let (receipt_path, settings_backup) = local_import_paths(&app)?;
    let mut receipt =
        read_receipt(&receipt_path).ok_or(UpstreamImportError::SettingsNotImported)?;
    if !receipt.settings_imported {
        return Err(UpstreamImportError::SettingsNotImported);
    }
    let (_, backup_settings) = read_settings_backup(&settings_backup)?;

    let (snapshot, previous_bindings, current_bindings) =
        settings::try_update_settings(&app, |target| {
            let snapshot = crate::modes::mode_settings_snapshot(&backup_settings);
            let previous_bindings = target.bindings.clone();
            let current_bindings = backup_settings.bindings.clone();
            *target = backup_settings;
            receipt.settings_imported = false;
            receipt.settings_backup_path = None;
            receipt.settings_backup_saved_at_ms = None;
            write_receipt(&receipt_path, &receipt)?;
            Ok::<_, UpstreamImportError>((snapshot, previous_bindings, current_bindings))
        })?;

    crate::shortcut::reconcile_mode_shortcuts(&app, &previous_bindings, &current_bindings);
    crate::modes::emit_modes_changed(&app, &snapshot);
    if let Err(error) = fs::remove_file(&settings_backup) {
        log::warn!("Failed to remove reverted settings backup: {error}");
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn import_legacy_app(
    app: AppHandle,
    history: State<'_, Arc<HistoryManager>>,
    secrets: State<'_, Arc<SecretManager>>,
    selection: UpstreamImportSelection,
) -> Result<UpstreamImportResult, UpstreamImportError> {
    validate_selection(&selection)?;
    let paths = import_paths(&app)?;
    require_upstream_closed()?;
    if !paths.source_root.is_dir() {
        return Err(UpstreamImportError::SourceUnavailable);
    }

    let mut receipt = read_receipt(&paths.receipt).unwrap_or_default();
    let source_identity = source_identity(&paths)?;
    if !receipt.source_identity.is_empty() && receipt.source_identity != source_identity {
        receipt = ImportReceipt::default();
    }
    receipt.source_identity = source_identity.clone();

    let settings_imported = if selection.settings {
        emit_progress(&app, UpstreamImportPhase::Settings, 0, 1);
        let imported = import_settings(&app, &secrets, &paths, &mut receipt).await?;
        emit_progress(&app, UpstreamImportPhase::Settings, 1, 1);
        imported
    } else {
        false
    };

    require_upstream_closed()?;
    let history_summary = if selection.history {
        let history_manager = history.inner().clone();
        let selection_copy = selection.clone();
        let source_identity_copy = source_identity.clone();
        let source_history = paths.source_history.clone();
        let source_recordings = paths.source_recordings.clone();
        let destination_recordings = paths.destination_recordings.clone();
        let progress_app = app.clone();
        tokio::task::spawn_blocking(move || {
            import_history(
                &history_manager,
                &progress_app,
                &source_identity_copy,
                &source_history,
                &source_recordings,
                &destination_recordings,
                &selection_copy,
            )
        })
        .await
        .map_err(|_| UpstreamImportError::Internal)??
    } else {
        HistoryImportSummary::default()
    };

    receipt.history_imported = receipt.history_imported.max(
        history_summary
            .imported
            .saturating_add(history_summary.existing),
    );
    receipt.recordings_imported = receipt.recordings_imported.max(
        history_summary
            .recordings_copied
            .saturating_add(history_summary.recordings_existing),
    );
    receipt.completed_at_ms = now_ms();
    write_receipt(&paths.receipt, &receipt)?;

    Ok(UpstreamImportResult {
        settings_imported,
        history_imported: history_summary.imported,
        history_existing: history_summary.existing,
        recordings_copied: history_summary.recordings_copied,
        recordings_existing: history_summary.recordings_existing,
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

fn destination_root(app: &AppHandle) -> Result<PathBuf, UpstreamImportError> {
    crate::portable::app_data_dir(app).map_err(|_| UpstreamImportError::Internal)
}

fn local_import_paths(app: &AppHandle) -> Result<(PathBuf, PathBuf), UpstreamImportError> {
    let destination_root = destination_root(app)?;
    Ok((
        destination_root.join(RECEIPT_FILE),
        destination_root.join(SETTINGS_BACKUP_FILE),
    ))
}

fn import_paths(app: &AppHandle) -> Result<ImportPaths, UpstreamImportError> {
    let source_root = upstream_data_root().ok_or(UpstreamImportError::SourceUnavailable)?;
    let destination_root = destination_root(app)?;
    let destination_recordings = destination_root.join("recordings");
    Ok(ImportPaths {
        source_settings: source_root.join("settings_store.json"),
        source_history: source_root.join("history.db"),
        source_recordings: source_root.join("recordings"),
        receipt: destination_root.join(RECEIPT_FILE),
        settings_backup: destination_root.join(SETTINGS_BACKUP_FILE),
        source_root,
        destination_recordings,
    })
}

fn status_for_paths(paths: &ImportPaths) -> Result<UpstreamImportStatus, UpstreamImportError> {
    let receipt = read_receipt(&paths.receipt).unwrap_or_default();
    let settings_backup_available = receipt.settings_imported && paths.settings_backup.is_file();
    let (recording_files, recording_bytes) = directory_counts(&paths.source_recordings);
    let history_entries = history_row_count(&paths.source_history).unwrap_or(0);
    Ok(UpstreamImportStatus {
        available: paths.source_root.is_dir(),
        app_state: upstream_app_state(),
        settings_available: paths.source_settings.is_file(),
        history_entries,
        recording_files,
        recording_bytes,
        settings_imported: receipt.settings_imported,
        settings_backup_available,
        settings_backup_saved_at_ms: settings_backup_available
            .then_some(receipt.settings_backup_saved_at_ms)
            .flatten(),
        history_imported: receipt.history_imported,
        recordings_imported: receipt.recordings_imported,
    })
}

fn import_history(
    history: &HistoryManager,
    app: &AppHandle,
    source_identity: &str,
    source_db: &Path,
    source_recordings: &Path,
    destination_recordings: &Path,
    selection: &UpstreamImportSelection,
) -> Result<HistoryImportSummary, UpstreamImportError> {
    if !selection.history || !source_db.is_file() {
        emit_progress(app, UpstreamImportPhase::History, 0, 0);
        if selection.recordings {
            emit_progress(app, UpstreamImportPhase::Recordings, 0, 0);
        }
        return Ok(HistoryImportSummary::default());
    }

    let history_total = source_history_row_count(source_db)?;
    let recording_total = if selection.recordings {
        recording_candidate_count(source_db, source_recordings)?
    } else {
        0
    };
    emit_progress(app, UpstreamImportPhase::History, 0, history_total);
    if selection.recordings {
        emit_progress(app, UpstreamImportPhase::Recordings, 0, recording_total);
    }

    let connection = open_source_history(source_db)?;
    let mut statement = connection
        .prepare(
            "SELECT id, file_name, timestamp, saved, title,
                    transcription_text, post_processed_text, post_process_requested
             FROM transcription_history ORDER BY id ASC",
        )
        .map_err(|_| UpstreamImportError::HistoryUnreadable)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, bool>(7)?,
            ))
        })
        .map_err(|_| UpstreamImportError::HistoryUnreadable)?;

    let mut summary = HistoryImportSummary::default();
    let mut history_completed = 0_u64;
    let mut recordings_completed = 0_u64;
    for row in rows {
        let (source_id, file_name, timestamp, saved, title, text, processed, requested) =
            row.map_err(|_| UpstreamImportError::HistoryUnreadable)?;
        let row_hash = history_row_hash(HistoryRowHashInput {
            id: source_id,
            file_name: &file_name,
            timestamp,
            saved,
            title: &title,
            text: &text,
            processed: processed.as_deref(),
            requested,
        });
        let mut imported_file_name = file_name.clone();
        let mut has_audio = false;
        if selection.recordings {
            if let Some(source_audio) =
                source_recording_path(source_recordings, &file_name).filter(|path| path.is_file())
            {
                imported_file_name =
                    imported_recording_name(source_identity, source_id, &file_name);
                let destination = destination_recordings.join(&imported_file_name);
                if destination.is_file() {
                    if files_equal(&source_audio, &destination)
                        .map_err(|_| UpstreamImportError::RecordingCopyFailed)?
                    {
                        summary.recordings_existing = summary.recordings_existing.saturating_add(1);
                        has_audio = true;
                    } else {
                        return Err(UpstreamImportError::RecordingCopyFailed);
                    }
                } else {
                    copy_verified(&source_audio, &destination)
                        .map_err(|_| UpstreamImportError::RecordingCopyFailed)?;
                    summary.recordings_copied = summary.recordings_copied.saturating_add(1);
                    has_audio = true;
                }
                recordings_completed = recordings_completed.saturating_add(1);
                emit_progress(
                    app,
                    UpstreamImportPhase::Recordings,
                    recordings_completed,
                    recording_total,
                );
            }
        }
        let entry = UpstreamHistoryImportEntry {
            source_history_id: source_id,
            source_row_sha256: row_hash,
            timestamp,
            saved,
            title,
            transcription_text: text,
            post_processed_text: processed,
            post_process_requested: requested,
            duration_ms: None,
            word_count: None,
            source_kind: Some(HistorySourceKind::Microphone),
            runs: Vec::new(),
        };
        match history
            .import_upstream_entry(source_identity, &entry, &imported_file_name, has_audio)
            .map_err(|_| UpstreamImportError::HistoryUnreadable)?
        {
            crate::managers::history::UpstreamHistoryImportOutcome::Inserted { .. } => {
                summary.imported = summary.imported.saturating_add(1)
            }
            crate::managers::history::UpstreamHistoryImportOutcome::Existing { .. } => {
                summary.existing = summary.existing.saturating_add(1)
            }
        }
        history_completed = history_completed.saturating_add(1);
        emit_progress(
            app,
            UpstreamImportPhase::History,
            history_completed,
            history_total,
        );
    }
    Ok(summary)
}

fn open_source_history(path: &Path) -> Result<Connection, UpstreamImportError> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| UpstreamImportError::HistoryUnreadable)
}

fn source_history_row_count(path: &Path) -> Result<u64, UpstreamImportError> {
    open_source_history(path)?
        .query_row("SELECT COUNT(*) FROM transcription_history", [], |row| {
            row.get::<_, u64>(0)
        })
        .map_err(|_| UpstreamImportError::HistoryUnreadable)
}

fn history_row_count(path: &Path) -> Option<u64> {
    source_history_row_count(path).ok()
}

fn recording_candidate_count(
    source_db: &Path,
    source_recordings: &Path,
) -> Result<u64, UpstreamImportError> {
    let connection = open_source_history(source_db)?;
    let mut statement = connection
        .prepare("SELECT file_name FROM transcription_history")
        .map_err(|_| UpstreamImportError::HistoryUnreadable)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|_| UpstreamImportError::HistoryUnreadable)?;
    let mut count = 0_u64;
    for row in rows {
        let file_name = row.map_err(|_| UpstreamImportError::HistoryUnreadable)?;
        if source_recording_path(source_recordings, &file_name).is_some_and(|path| path.is_file()) {
            count = count.saturating_add(1);
        }
    }
    Ok(count)
}

fn source_recording_path(root: &Path, file_name: &str) -> Option<PathBuf> {
    let mut components = Path::new(file_name).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(name)), None) => Some(root.join(name)),
        _ => None,
    }
}

fn read_source_settings(path: &Path) -> Result<serde_json::Value, UpstreamImportError> {
    let bytes = fs::read(path).map_err(|_| UpstreamImportError::SettingsUnreadable)?;
    let root: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| UpstreamImportError::SettingsUnreadable)?;
    Ok(root.get("settings").cloned().unwrap_or(root))
}

fn source_identity(paths: &ImportPaths) -> Result<String, UpstreamImportError> {
    let canonical = paths
        .source_root
        .canonicalize()
        .map_err(|_| UpstreamImportError::SourceUnavailable)?;
    let mut digest = Sha256::new();
    digest.update(SOURCE_IDENTITY_LABEL.as_bytes());
    digest.update([0]);
    digest.update(canonical.to_string_lossy().as_bytes());
    if let Ok(metadata) = fs::metadata(&paths.source_history) {
        digest.update(metadata.len().to_be_bytes());
    }
    Ok(hex_digest(digest.finalize().as_slice()))
}

fn history_row_hash(row: HistoryRowHashInput<'_>) -> String {
    let mut digest = Sha256::new();
    for value in [
        row.id.to_string(),
        row.file_name.to_string(),
        row.timestamp.to_string(),
        row.saved.to_string(),
        row.title.to_string(),
        row.text.to_string(),
        row.processed.unwrap_or_default().to_string(),
        row.requested.to_string(),
    ] {
        digest.update(value.as_bytes());
        digest.update([0]);
    }
    hex_digest(digest.finalize().as_slice())
}

fn imported_recording_name(identity: &str, id: i64, original: &str) -> String {
    let basename = Path::new(original)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("recording.wav");
    let prefix = identity.get(..12).unwrap_or(identity);
    format!("upstream-{prefix}-{id}-{basename}")
}

fn directory_counts(path: &Path) -> (u64, u64) {
    let Ok(entries) = fs::read_dir(path) else {
        return (0, 0);
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.metadata().ok())
        .filter(|metadata| metadata.is_file())
        .fold((0_u64, 0_u64), |(count, bytes), metadata| {
            (
                count.saturating_add(1),
                bytes.saturating_add(metadata.len()),
            )
        })
}

fn read_receipt(path: &Path) -> Option<ImportReceipt> {
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_receipt(path: &Path, receipt: &ImportReceipt) -> Result<(), UpstreamImportError> {
    let bytes = serde_json::to_vec(receipt).map_err(|_| UpstreamImportError::ReceiptWriteFailed)?;
    write_private_file(path, &bytes).map_err(|_| UpstreamImportError::ReceiptWriteFailed)
}

fn write_settings_backup(
    path: &Path,
    settings: &crate::settings::AppSettings,
    saved_at_ms: u64,
) -> Result<(), UpstreamImportError> {
    let settings = SettingsDocument::from_settings(settings)
        .map_err(|_| UpstreamImportError::SettingsBackupWriteFailed)?;
    let backup = SettingsImportBackup {
        saved_at_ms,
        settings,
    };
    let bytes =
        serde_json::to_vec(&backup).map_err(|_| UpstreamImportError::SettingsBackupWriteFailed)?;
    write_private_file(path, &bytes).map_err(|_| UpstreamImportError::SettingsBackupWriteFailed)
}

fn read_settings_backup(
    path: &Path,
) -> Result<(u64, crate::settings::AppSettings), UpstreamImportError> {
    let bytes = fs::read(path).map_err(|_| UpstreamImportError::SettingsBackupUnreadable)?;
    let backup: SettingsImportBackup = serde_json::from_slice(&bytes)
        .map_err(|_| UpstreamImportError::SettingsBackupUnreadable)?;
    let settings = decode_settings_backup(backup.settings)
        .map_err(|_| UpstreamImportError::SettingsBackupUnreadable)?;
    Ok((backup.saved_at_ms, settings))
}

#[cfg(target_os = "macos")]
fn upstream_app_state() -> UpstreamAppState {
    use objc2_app_kit::NSRunningApplication;
    use objc2_foundation::NSString;
    let bundle = NSString::from_str(SOURCE_BUNDLE_ID);
    let applications = NSRunningApplication::runningApplicationsWithBundleIdentifier(&bundle);
    if applications.is_empty() {
        UpstreamAppState::Closed
    } else {
        UpstreamAppState::Running
    }
}

#[cfg(not(target_os = "macos"))]
fn upstream_app_state() -> UpstreamAppState {
    UpstreamAppState::Unverifiable
}

#[cfg(target_os = "macos")]
fn upstream_data_root() -> Option<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Library/Application Support/com.pais.handy"))
}

#[cfg(target_os = "windows")]
fn upstream_data_root() -> Option<PathBuf> {
    env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|root| root.join(SOURCE_BUNDLE_ID))
}

#[cfg(target_os = "linux")]
fn upstream_data_root() -> Option<PathBuf> {
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .map(|root| root.join(SOURCE_BUNDLE_ID))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    fn temp_root(name: &str) -> io::Result<PathBuf> {
        let root = env::temp_dir().join(format!(
            "handy-upstream-import-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir(&root)?;
        Ok(root)
    }

    #[test]
    fn verified_copy_preserves_source_and_detects_mismatch() -> Result<(), Box<dyn Error>> {
        let root = temp_root("copy")?;
        let source = root.join("source.wav");
        let destination = root.join("destination.wav");
        let bytes = (0_u32..100_000)
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>();
        fs::write(&source, &bytes)?;
        copy_verified(&source, &destination)?;
        assert_eq!(fs::read(&source)?, bytes);
        assert!(files_equal(&source, &destination)?);
        fs::write(&destination, b"different")?;
        assert!(!files_equal(&source, &destination)?);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn wrapped_settings_are_read_without_changing_source() -> Result<(), Box<dyn Error>> {
        let root = temp_root("settings")?;
        let path = root.join("settings_store.json");
        let original = br#"{"settings":{"settings_schema_version":1,"post_process_api_keys":{"openai":"secret"}}}"#;
        fs::write(&path, original)?;
        let settings = read_source_settings(&path)?;
        assert_eq!(
            settings
                .get("post_process_api_keys")
                .and_then(|value| value.get("openai"))
                .and_then(serde_json::Value::as_str),
            Some("secret")
        );
        assert_eq!(fs::read(&path)?, original);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn corrupt_receipt_is_disposable() -> Result<(), Box<dyn Error>> {
        let root = temp_root("receipt")?;
        let path = root.join(RECEIPT_FILE);
        fs::write(&path, b"not-json")?;
        assert!(read_receipt(&path).is_none());
        let receipt = ImportReceipt {
            source_identity: "a".repeat(64),
            settings_imported: true,
            settings_backup_path: None,
            settings_backup_saved_at_ms: None,
            history_imported: 3,
            recordings_imported: 2,
            completed_at_ms: 42,
        };
        write_receipt(&path, &receipt)?;
        let restored = read_receipt(&path).ok_or("receipt missing")?;
        assert_eq!(restored.history_imported, 3);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn settings_backup_is_secret_stripped_and_restores_pre_import_settings(
    ) -> Result<(), Box<dyn Error>> {
        let root = temp_root("settings-backup")?;
        let path = root.join(SETTINGS_BACKUP_FILE);
        let mut before = crate::settings::get_default_settings();
        before.modes[0].name = "Before import".to_string();
        before
            .bindings
            .get_mut("transcribe")
            .unwrap()
            .current_binding = "f14".to_string();
        before.custom_words = vec![crate::settings::VocabularyEntry {
            spoken: "Sona".to_string(),
            written: "Sona".to_string(),
        }];
        write_settings_backup(&path, &before, 42)?;

        let bytes = fs::read(&path)?;
        assert!(!String::from_utf8_lossy(&bytes).contains("post_process_api_keys"));
        let (saved_at_ms, backup) = read_settings_backup(&path)?;
        assert_eq!(saved_at_ms, 42);

        let mut target = crate::settings::get_default_settings();
        target.context_policy_ceiling = crate::context::ContextPolicy::Full;
        target.modes[0].name = "After import".to_string();
        target
            .bindings
            .get_mut("transcribe")
            .unwrap()
            .current_binding = "f15".to_string();
        target.custom_words.clear();
        target = backup;

        assert_eq!(target.modes, before.modes);
        assert_eq!(target.bindings, before.bindings);
        assert_eq!(target.custom_words, before.custom_words);
        assert_eq!(target.context_policy_ceiling, before.context_policy_ceiling);

        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn recordings_require_history_selection() {
        let selection = UpstreamImportSelection {
            settings: false,
            history: false,
            recordings: true,
        };
        assert!(matches!(
            validate_selection(&selection),
            Err(UpstreamImportError::InvalidSelection)
        ));

        let valid = UpstreamImportSelection {
            history: true,
            recordings: true,
            ..Default::default()
        };
        assert!(validate_selection(&valid).is_ok());
    }

    #[test]
    fn recording_paths_stay_inside_source_recordings() {
        let root = PathBuf::from("recordings");
        assert_eq!(
            source_recording_path(&root, "recording.wav"),
            Some(root.join("recording.wav"))
        );
        assert!(source_recording_path(&root, "../recording.wav").is_none());
        assert!(source_recording_path(&root, "nested/recording.wav").is_none());
        assert!(source_recording_path(&root, "/recording.wav").is_none());
    }

    #[test]
    fn progress_phase_is_typed_and_serialized() -> Result<(), Box<dyn Error>> {
        assert_eq!(
            serde_json::to_string(&UpstreamImportPhase::Recordings)?,
            "\"recordings\""
        );
        Ok(())
    }

    #[test]
    fn row_hash_and_recording_name_are_stable_and_content_bound() {
        let row = |text| HistoryRowHashInput {
            id: 1,
            file_name: "a.wav",
            timestamp: 2,
            saved: false,
            title: "",
            text,
            processed: None,
            requested: false,
        };
        let first = history_row_hash(row("hello"));
        let second = history_row_hash(row("hello"));
        let changed = history_row_hash(row("world"));
        assert_eq!(first, second);
        assert_ne!(first, changed);
        assert_eq!(
            imported_recording_name(&"b".repeat(64), 7, "../a.wav"),
            "upstream-bbbbbbbbbbbb-7-a.wav"
        );
    }
}

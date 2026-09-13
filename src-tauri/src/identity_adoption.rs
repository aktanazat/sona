use crate::fs_util::{copy_verified, file_hash, files_equal, hex_digest, write_private_file};
use crate::secrets::{
    migrate_service_account, SecretAccount, SecretKind, SecretManager, ServiceAccountMigration,
    LEGACY_FORK_SECRET_SERVICE_NAME,
};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

pub(crate) const LEGACY_FORK_BUNDLE_ID: &str = "com.aktanazat.handy-personal";
const RECEIPT_FILE: &str = "identity-adoption-receipt.json";
const JOURNAL_FILE: &str = "identity-adoption-journal.json";
const TOMBSTONE_FILE: &str = "moved-to-sona.json";
const SETTINGS_FILE: &str = "settings_store.json";
const HISTORY_FILE: &str = "history.db";
const UPSTREAM_RECEIPT_FILE: &str = "upstream-import-receipt.json";
const UPSTREAM_BACKUP_FILE: &str = "settings-pre-import-backup.json";

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum IdentityAdoptionMode {
    Portable,
    NothingToAdopt,
    SkippedNonvirgin,
    FreshStart,
    Completed,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum IdentityAdoptionAction {
    Renamed,
    Copied,
    Skipped,
    Failed,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
pub struct IdentityAdoptionEntry {
    pub path: String,
    pub action: IdentityAdoptionAction,
    pub bytes: u64,
    pub sha256: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum IdentityCredentialStatus {
    Moved,
    NotFound,
    NeedsReentry,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
pub struct IdentityCredentialReceipt {
    pub account: String,
    pub status: IdentityCredentialStatus,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
pub struct IdentityAdoptionReceipt {
    pub mode: IdentityAdoptionMode,
    pub source_identity: Option<String>,
    pub entries: Vec<IdentityAdoptionEntry>,
    pub credentials: Vec<IdentityCredentialReceipt>,
    pub completed_at_ms: u64,
    pub app_version: String,
}

/// What a rollback left behind for the reader: the folder holding the
/// settings and history Sona wrote after adopting, which the legacy folder
/// never saw. `None` when there was nothing of the kind to keep.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
pub struct IdentityRollbackReceipt {
    pub backup_dir: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum IdentityAdoptionError {
    Unavailable,
    LegacyRunning,
    CopyFailed,
    DestinationConflict,
    InvalidData,
    SecretMigrationFailed,
    RollbackUnavailable,
    RollbackFailed,
}

impl std::fmt::Display for IdentityAdoptionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for IdentityAdoptionError {}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum JournalStage {
    Intent,
    Acted,
    Done,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct JournalEntry {
    path: String,
    action: IdentityAdoptionAction,
    stage: JournalStage,
    bytes: u64,
    sha256: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct AdoptionJournal {
    entries: Vec<JournalEntry>,
    #[serde(default)]
    credentials: Vec<IdentityCredentialReceipt>,
}

struct AdoptionPaths {
    source_root: PathBuf,
    destination_root: PathBuf,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LegacyAppState {
    Closed,
    Running,
    Unverifiable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AdoptionChoice {
    Move,
    FreshStart,
    Retry,
}

/// Runs before settings, models, history, and the normal credential manager
/// touch the Sona root. The receipt is the one fast-path metadata read later.
pub(crate) fn adopt_before_startup(app: &AppHandle) -> Result<(), IdentityAdoptionError> {
    let destination_root =
        crate::portable::app_data_dir(app).map_err(|_| IdentityAdoptionError::Unavailable)?;
    if destination_root.join(RECEIPT_FILE).is_file() {
        return Ok(());
    }
    if crate::portable::is_portable() {
        return write_receipt(
            &destination_root.join(RECEIPT_FILE),
            &receipt(IdentityAdoptionMode::Portable, None, Vec::new(), Vec::new()),
        );
    }
    let Some(source_root) = legacy_data_root() else {
        return write_receipt(
            &destination_root.join(RECEIPT_FILE),
            &receipt(
                IdentityAdoptionMode::NothingToAdopt,
                Some(LEGACY_FORK_BUNDLE_ID.to_string()),
                Vec::new(),
                Vec::new(),
            ),
        );
    };
    let paths = AdoptionPaths {
        source_root,
        destination_root,
    };
    let source_secrets = SecretManager::native_for_service(LEGACY_FORK_SECRET_SERVICE_NAME);
    let destination_secrets = SecretManager::native();
    let mut prompt = |state| native_adoption_choice(app, state);
    adopt_paths(
        &paths,
        false,
        &mut prompt,
        &legacy_app_state,
        &source_secrets,
        &destination_secrets,
        |enabled| {
            if enabled {
                crate::autostart::apply_autostart(app, true);
            }
            crate::autostart::remove_legacy_autostart_artifacts(app);
        },
    )
    .map(|_| ())
}

fn native_adoption_choice(app: &AppHandle, state: LegacyAppState) -> AdoptionChoice {
    let handle = app.clone();
    let accepted = std::thread::spawn(move || match state {
        LegacyAppState::Closed => handle
            .dialog()
            .message("Move your data from the Legacy app? Settings and history are copied; recordings and models move to Sona. You may be asked to allow access to legacy provider keys.")
            .title("Sona")
            .kind(MessageDialogKind::Info)
            .buttons(MessageDialogButtons::OkCancelCustom("Move data".into(), "Start fresh".into()))
            .blocking_show(),
        LegacyAppState::Running => handle
            .dialog()
            .message("Close the Legacy app before moving data. It may still own your recordings or shortcuts.")
            .title("Sona")
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom("Retry".into(), "Start fresh".into()))
            .blocking_show(),
        LegacyAppState::Unverifiable => handle
            .dialog()
            .message("Sona cannot verify whether the Legacy app is closed. Close it, then retry before moving data.")
            .title("Sona")
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom("Retry".into(), "Start fresh".into()))
            .blocking_show(),
    })
    .join()
    .unwrap_or(false);
    match state {
        LegacyAppState::Closed if accepted => AdoptionChoice::Move,
        LegacyAppState::Closed => AdoptionChoice::FreshStart,
        _ if accepted => AdoptionChoice::Retry,
        _ => AdoptionChoice::FreshStart,
    }
}

#[allow(clippy::too_many_arguments)]
fn adopt_paths(
    paths: &AdoptionPaths,
    portable: bool,
    prompt: &mut dyn FnMut(LegacyAppState) -> AdoptionChoice,
    running_probe: &dyn Fn() -> LegacyAppState,
    source_secrets: &SecretManager,
    destination_secrets: &SecretManager,
    apply_autostart: impl Fn(bool),
) -> Result<IdentityAdoptionReceipt, IdentityAdoptionError> {
    let receipt_path = paths.destination_root.join(RECEIPT_FILE);
    if let Some(existing) = read_receipt(&receipt_path) {
        return Ok(existing);
    }
    if portable {
        let result = receipt(IdentityAdoptionMode::Portable, None, Vec::new(), Vec::new());
        write_receipt(&receipt_path, &result)?;
        return Ok(result);
    }
    if !source_has_adoptable_data(&paths.source_root) {
        let result = receipt(
            IdentityAdoptionMode::NothingToAdopt,
            Some(LEGACY_FORK_BUNDLE_ID.to_string()),
            Vec::new(),
            Vec::new(),
        );
        write_receipt(&receipt_path, &result)?;
        return Ok(result);
    }
    if paths.destination_root.join(SETTINGS_FILE).exists() {
        let result = receipt(
            IdentityAdoptionMode::SkippedNonvirgin,
            Some(LEGACY_FORK_BUNDLE_ID.to_string()),
            Vec::new(),
            Vec::new(),
        );
        write_receipt(&receipt_path, &result)?;
        return Ok(result);
    }
    loop {
        let state = running_probe();
        match (state, prompt(state)) {
            (_, AdoptionChoice::FreshStart) => {
                let result = receipt(
                    IdentityAdoptionMode::FreshStart,
                    Some(LEGACY_FORK_BUNDLE_ID.to_string()),
                    Vec::new(),
                    Vec::new(),
                );
                write_receipt(&receipt_path, &result)?;
                return Ok(result);
            }
            (LegacyAppState::Closed, AdoptionChoice::Move) => break,
            (_, AdoptionChoice::Retry) => continue,
            _ => return Err(IdentityAdoptionError::LegacyRunning),
        }
    }

    fs::create_dir_all(&paths.destination_root).map_err(|_| IdentityAdoptionError::Unavailable)?;
    let journal_path = paths.destination_root.join(JOURNAL_FILE);
    let mut journal = read_journal(&journal_path).unwrap_or_default();
    if journal.entries.is_empty() {
        journal.entries = planned_entries(paths)?;
        write_journal(&journal_path, &journal)?;
    }
    for index in 0..journal.entries.len() {
        if journal.entries[index].stage == JournalStage::Done {
            continue;
        }
        write_journal(&journal_path, &journal)?;
        let source = paths.source_root.join(&journal.entries[index].path);
        let destination = paths.destination_root.join(&journal.entries[index].path);
        let expected = journal.entries[index].action;
        match materialize_entry(&source, &destination, expected) {
            Ok((action, bytes, sha256)) => {
                journal.entries[index].action = action;
                journal.entries[index].bytes = bytes;
                journal.entries[index].sha256 = sha256;
                journal.entries[index].stage = JournalStage::Acted;
                write_journal(&journal_path, &journal)?;
                journal.entries[index].stage = JournalStage::Done;
                write_journal(&journal_path, &journal)?;
            }
            Err(error) => {
                journal.entries[index].action = IdentityAdoptionAction::Failed;
                let _ = write_journal(&journal_path, &journal);
                return Err(error);
            }
        }
    }

    rewrite_factory_vocabulary(&paths.destination_root.join(SETTINGS_FILE))?;
    migrate_credentials(
        &paths.destination_root.join(SETTINGS_FILE),
        source_secrets,
        destination_secrets,
        &mut journal,
        &journal_path,
    )?;
    apply_autostart(settings_enable_autostart(
        &paths.destination_root.join(SETTINGS_FILE),
    ));
    let result = receipt(
        IdentityAdoptionMode::Completed,
        Some(LEGACY_FORK_BUNDLE_ID.to_string()),
        journal
            .entries
            .iter()
            .map(|entry| IdentityAdoptionEntry {
                path: entry.path.clone(),
                action: entry.action,
                bytes: entry.bytes,
                sha256: entry.sha256.clone(),
            })
            .collect(),
        journal.credentials.clone(),
    );
    write_receipt(&receipt_path, &result)?;
    write_tombstone(&paths.source_root, &paths.destination_root)?;
    Ok(result)
}

fn source_has_adoptable_data(root: &Path) -> bool {
    root.join(SETTINGS_FILE).is_file()
        || root.join(HISTORY_FILE).is_file()
        || has_children(&root.join("models"))
        || has_children(&root.join("recordings"))
}

fn has_children(path: &Path) -> bool {
    fs::read_dir(path)
        .ok()
        .and_then(|mut entries| entries.next())
        .is_some()
}

fn planned_entries(paths: &AdoptionPaths) -> Result<Vec<JournalEntry>, IdentityAdoptionError> {
    let mut entries = Vec::new();
    for file in [
        SETTINGS_FILE,
        HISTORY_FILE,
        "history.db-wal",
        "history.db-shm",
        UPSTREAM_RECEIPT_FILE,
        UPSTREAM_BACKUP_FILE,
    ] {
        if paths.source_root.join(file).is_file() {
            entries.push(JournalEntry {
                path: file.to_string(),
                action: IdentityAdoptionAction::Copied,
                stage: JournalStage::Intent,
                bytes: 0,
                sha256: None,
            });
        }
    }
    for parent in ["recordings", "models"] {
        let mut children = fs::read_dir(paths.source_root.join(parent))
            .ok()
            .into_iter()
            .flat_map(|entries| entries.filter_map(Result::ok))
            .collect::<Vec<_>>();
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            entries.push(JournalEntry {
                path: Path::new(parent)
                    .join(child.file_name())
                    .to_string_lossy()
                    .into_owned(),
                action: IdentityAdoptionAction::Renamed,
                stage: JournalStage::Intent,
                bytes: 0,
                sha256: None,
            });
        }
    }
    Ok(entries)
}

fn materialize_entry(
    source: &Path,
    destination: &Path,
    expected: IdentityAdoptionAction,
) -> Result<(IdentityAdoptionAction, u64, Option<String>), IdentityAdoptionError> {
    let parent = destination
        .parent()
        .ok_or(IdentityAdoptionError::InvalidData)?;
    fs::create_dir_all(parent).map_err(|_| IdentityAdoptionError::CopyFailed)?;
    match (source.exists(), destination.exists()) {
        (false, true) => metadata_for(destination, expected),
        (true, true) if source.is_file() && destination.is_file() => {
            if !files_equal(source, destination).map_err(|_| IdentityAdoptionError::CopyFailed)? {
                return Err(IdentityAdoptionError::DestinationConflict);
            }
            if expected == IdentityAdoptionAction::Renamed {
                fs::remove_file(source).map_err(|_| IdentityAdoptionError::CopyFailed)?;
            }
            metadata_for(destination, expected)
        }
        (true, true) if source.is_dir() && destination.is_dir() => {
            if !directories_equal(source, destination)? {
                return Err(IdentityAdoptionError::DestinationConflict);
            }
            if expected == IdentityAdoptionAction::Renamed {
                fs::remove_dir_all(source).map_err(|_| IdentityAdoptionError::CopyFailed)?;
            }
            metadata_for(destination, expected)
        }
        (true, true) => Err(IdentityAdoptionError::DestinationConflict),
        (false, false) => Err(IdentityAdoptionError::InvalidData),
        (true, false) => match expected {
            IdentityAdoptionAction::Copied => {
                copy_verified(source, destination)
                    .map_err(|_| IdentityAdoptionError::CopyFailed)?;
                metadata_for(destination, IdentityAdoptionAction::Copied)
            }
            IdentityAdoptionAction::Renamed => match fs::rename(source, destination) {
                Ok(()) => metadata_for(destination, IdentityAdoptionAction::Renamed),
                Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
                    copy_then_delete(source, destination)?;
                    metadata_for(destination, IdentityAdoptionAction::Copied)
                }
                Err(_) => Err(IdentityAdoptionError::CopyFailed),
            },
            IdentityAdoptionAction::Skipped | IdentityAdoptionAction::Failed => {
                Err(IdentityAdoptionError::InvalidData)
            }
        },
    }
}

fn copy_then_delete(source: &Path, destination: &Path) -> Result<(), IdentityAdoptionError> {
    let metadata = fs::symlink_metadata(source).map_err(|_| IdentityAdoptionError::CopyFailed)?;
    if metadata.file_type().is_symlink() {
        return Err(IdentityAdoptionError::InvalidData);
    }
    if metadata.is_file() {
        copy_verified(source, destination).map_err(|_| IdentityAdoptionError::CopyFailed)?;
        fs::remove_file(source).map_err(|_| IdentityAdoptionError::CopyFailed)?;
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(IdentityAdoptionError::InvalidData);
    }
    copy_directory_verified(source, destination)?;
    fs::remove_dir_all(source).map_err(|_| IdentityAdoptionError::CopyFailed)
}

fn copy_directory_verified(source: &Path, destination: &Path) -> Result<(), IdentityAdoptionError> {
    fs::create_dir(destination).map_err(|_| IdentityAdoptionError::CopyFailed)?;
    let mut entries = fs::read_dir(source)
        .map_err(|_| IdentityAdoptionError::CopyFailed)?
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let source_child = entry.path();
        let destination_child = destination.join(entry.file_name());
        let metadata =
            fs::symlink_metadata(&source_child).map_err(|_| IdentityAdoptionError::CopyFailed)?;
        if metadata.file_type().is_symlink() {
            return Err(IdentityAdoptionError::InvalidData);
        }
        if metadata.is_dir() {
            copy_directory_verified(&source_child, &destination_child)?;
        } else if metadata.is_file() {
            copy_verified(&source_child, &destination_child)
                .map_err(|_| IdentityAdoptionError::CopyFailed)?;
        } else {
            return Err(IdentityAdoptionError::InvalidData);
        }
    }
    Ok(())
}

fn directories_equal(left: &Path, right: &Path) -> Result<bool, IdentityAdoptionError> {
    let mut left_entries = fs::read_dir(left)
        .map_err(|_| IdentityAdoptionError::CopyFailed)?
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    let mut right_entries = fs::read_dir(right)
        .map_err(|_| IdentityAdoptionError::CopyFailed)?
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    left_entries.sort_by_key(|entry| entry.file_name());
    right_entries.sort_by_key(|entry| entry.file_name());
    if left_entries.len() != right_entries.len() {
        return Ok(false);
    }
    for (left_entry, right_entry) in left_entries.iter().zip(&right_entries) {
        if left_entry.file_name() != right_entry.file_name() {
            return Ok(false);
        }
        let left_path = left_entry.path();
        let right_path = right_entry.path();
        let left_metadata =
            fs::symlink_metadata(&left_path).map_err(|_| IdentityAdoptionError::CopyFailed)?;
        let right_metadata =
            fs::symlink_metadata(&right_path).map_err(|_| IdentityAdoptionError::CopyFailed)?;
        if left_metadata.file_type().is_symlink() || right_metadata.file_type().is_symlink() {
            return Ok(false);
        }
        if left_metadata.is_dir() && right_metadata.is_dir() {
            if !directories_equal(&left_path, &right_path)? {
                return Ok(false);
            }
        } else if left_metadata.is_file() && right_metadata.is_file() {
            if !files_equal(&left_path, &right_path)
                .map_err(|_| IdentityAdoptionError::CopyFailed)?
            {
                return Ok(false);
            }
        } else {
            return Ok(false);
        }
    }
    Ok(true)
}

fn metadata_for(
    path: &Path,
    action: IdentityAdoptionAction,
) -> Result<(IdentityAdoptionAction, u64, Option<String>), IdentityAdoptionError> {
    let metadata = fs::metadata(path).map_err(|_| IdentityAdoptionError::CopyFailed)?;
    if metadata.is_file() {
        return Ok((
            action,
            metadata.len(),
            Some(hex_digest(
                &file_hash(path).map_err(|_| IdentityAdoptionError::CopyFailed)?,
            )),
        ));
    }
    if metadata.is_dir() {
        return Ok((action, directory_bytes(path)?, None));
    }
    Err(IdentityAdoptionError::InvalidData)
}

fn directory_bytes(path: &Path) -> Result<u64, IdentityAdoptionError> {
    let mut bytes = 0_u64;
    for entry in fs::read_dir(path)
        .map_err(|_| IdentityAdoptionError::CopyFailed)?
        .filter_map(Result::ok)
    {
        let child = entry.path();
        let metadata =
            fs::symlink_metadata(&child).map_err(|_| IdentityAdoptionError::CopyFailed)?;
        if metadata.file_type().is_symlink() {
            return Err(IdentityAdoptionError::InvalidData);
        }
        if metadata.is_file() {
            bytes = bytes.saturating_add(metadata.len());
        } else if metadata.is_dir() {
            bytes = bytes.saturating_add(directory_bytes(&child)?);
        } else {
            return Err(IdentityAdoptionError::InvalidData);
        }
    }
    Ok(bytes)
}

fn migrate_credentials(
    settings_path: &Path,
    source: &SecretManager,
    destination: &SecretManager,
    journal: &mut AdoptionJournal,
    journal_path: &Path,
) -> Result<(), IdentityAdoptionError> {
    let migrated = journal
        .credentials
        .iter()
        .map(|entry| entry.account.clone())
        .collect::<BTreeSet<_>>();
    for account in credential_accounts(settings_path)? {
        if migrated.contains(&account) {
            continue;
        }
        let (kind, provider) = account
            .split_once('/')
            .ok_or(IdentityAdoptionError::InvalidData)?;
        let kind = match kind {
            "llm" => SecretKind::Llm,
            "stt" => SecretKind::Stt,
            _ => return Err(IdentityAdoptionError::InvalidData),
        };
        let status = match tauri::async_runtime::block_on(migrate_service_account(
            source,
            destination,
            SecretAccount::for_provider(kind, provider)
                .map_err(|_| IdentityAdoptionError::InvalidData)?,
        )) {
            ServiceAccountMigration::Moved | ServiceAccountMigration::AlreadyMoved => {
                IdentityCredentialStatus::Moved
            }
            ServiceAccountMigration::NotFound => IdentityCredentialStatus::NotFound,
            ServiceAccountMigration::NeedsReentry(_) => IdentityCredentialStatus::NeedsReentry,
        };
        journal
            .credentials
            .push(IdentityCredentialReceipt { account, status });
        write_journal(journal_path, journal)?;
    }
    Ok(())
}

fn credential_accounts(settings_path: &Path) -> Result<Vec<String>, IdentityAdoptionError> {
    if !settings_path.is_file() {
        return Ok(Vec::new());
    }
    let root: serde_json::Value = serde_json::from_slice(
        &fs::read(settings_path).map_err(|_| IdentityAdoptionError::InvalidData)?,
    )
    .map_err(|_| IdentityAdoptionError::InvalidData)?;
    let settings = root.get("settings").unwrap_or(&root);
    let mut accounts = BTreeSet::new();
    for provider in settings
        .get("post_process_providers")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(id) = provider.get("id").and_then(serde_json::Value::as_str) {
            if SecretAccount::for_provider(SecretKind::Llm, id).is_ok() {
                accounts.insert(format!("llm/{id}"));
            }
        }
    }
    for provider in settings
        .get("cloud_stt_providers")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(id) = provider.get("provider").and_then(serde_json::Value::as_str) {
            if SecretAccount::for_provider(SecretKind::Stt, id).is_ok() {
                accounts.insert(format!("stt/{id}"));
            }
        }
    }
    Ok(accounts.into_iter().collect())
}

fn settings_enable_autostart(path: &Path) -> bool {
    let Ok(root) = fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .ok_or(())
    else {
        return false;
    };
    root.get("settings")
        .unwrap_or(&root)
        .get("autostart_enabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn write_receipt(
    path: &Path,
    receipt: &IdentityAdoptionReceipt,
) -> Result<(), IdentityAdoptionError> {
    write_private_file(
        path,
        &serde_json::to_vec(receipt).map_err(|_| IdentityAdoptionError::InvalidData)?,
    )
    .map_err(|_| IdentityAdoptionError::Unavailable)
}

fn read_receipt(path: &Path) -> Option<IdentityAdoptionReceipt> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn write_journal(path: &Path, journal: &AdoptionJournal) -> Result<(), IdentityAdoptionError> {
    write_private_file(
        path,
        &serde_json::to_vec(journal).map_err(|_| IdentityAdoptionError::InvalidData)?,
    )
    .map_err(|_| IdentityAdoptionError::Unavailable)
}

fn read_journal(path: &Path) -> Option<AdoptionJournal> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn receipt(
    mode: IdentityAdoptionMode,
    source_identity: Option<String>,
    entries: Vec<IdentityAdoptionEntry>,
    credentials: Vec<IdentityCredentialReceipt>,
) -> IdentityAdoptionReceipt {
    IdentityAdoptionReceipt {
        mode,
        source_identity,
        entries,
        credentials,
        completed_at_ms: now_ms(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
    }
}
fn rewrite_factory_vocabulary(path: &Path) -> Result<(), IdentityAdoptionError> {
    if !path.is_file() {
        return Ok(());
    }
    let bytes = fs::read(path).map_err(|_| IdentityAdoptionError::InvalidData)?;
    let mut root: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| IdentityAdoptionError::InvalidData)?;
    let changed = match root.get_mut("settings") {
        Some(settings) => replace_factory_pairs(settings),
        None => replace_factory_pairs(&mut root),
    };
    if changed {
        let bytes = serde_json::to_vec(&root).map_err(|_| IdentityAdoptionError::InvalidData)?;
        write_private_file(path, &bytes).map_err(|_| IdentityAdoptionError::CopyFailed)?;
    }
    Ok(())
}

fn replace_factory_pairs(value: &mut serde_json::Value) -> bool {
    let Some(object) = value.as_object_mut() else {
        return false;
    };
    let mut changed = false;
    if let Some(words) = object
        .get_mut("custom_words")
        .and_then(serde_json::Value::as_array_mut)
    {
        let mut removed = false;
        words.retain(|word| {
            let factory = match word {
                serde_json::Value::String(value) => value == "Handy" || value == "cjpais",
                serde_json::Value::Object(pair) => matches!(
                    (
                        pair.get("spoken").and_then(serde_json::Value::as_str),
                        pair.get("written").and_then(serde_json::Value::as_str),
                    ),
                    (Some("Handy"), Some("Handy")) | (Some("cjpais"), Some("cjpais"))
                ),
                _ => false,
            };
            removed |= factory;
            !factory
        });
        if removed {
            if !words.iter().any(|word| {
                word.get("spoken").and_then(serde_json::Value::as_str) == Some("Sona")
                    && word.get("written").and_then(serde_json::Value::as_str) == Some("Sona")
            }) {
                words.push(serde_json::json!({ "spoken": "Sona", "written": "Sona" }));
            }
            changed = true;
        }
    }
    if let Some(modes) = object
        .get_mut("modes")
        .and_then(serde_json::Value::as_array_mut)
    {
        for mode in modes {
            if let Some(asr) = mode.get_mut("asr") {
                changed |= replace_factory_pairs(asr);
            }
        }
    }
    changed
}

fn write_tombstone(
    source_root: &Path,
    destination_root: &Path,
) -> Result<(), IdentityAdoptionError> {
    let bytes = serde_json::to_vec(
        &serde_json::json!({ "destination": destination_root, "at_ms": now_ms() }),
    )
    .map_err(|_| IdentityAdoptionError::InvalidData)?;
    write_private_file(&source_root.join(TOMBSTONE_FILE), &bytes)
        .map_err(|_| IdentityAdoptionError::Unavailable)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

#[tauri::command]
#[specta::specta]
pub fn get_identity_adoption_status(
    app: AppHandle,
) -> Result<Option<IdentityAdoptionReceipt>, IdentityAdoptionError> {
    let root =
        crate::portable::app_data_dir(&app).map_err(|_| IdentityAdoptionError::Unavailable)?;
    Ok(read_receipt(&root.join(RECEIPT_FILE)))
}

/// The files Sona wrote in its own root after adopting, which a rollback
/// keeps rather than deletes: the legacy folder holds only the copies made
/// at adoption, so every dictation and setting since lives here alone.
const ROLLBACK_KEPT_FILES: [&str; 6] = [
    SETTINGS_FILE,
    HISTORY_FILE,
    "history.db-wal",
    "history.db-shm",
    UPSTREAM_RECEIPT_FILE,
    UPSTREAM_BACKUP_FILE,
];

/// Where one rollback parks those files, named by the moment so a second
/// rollback never lands on the first.
fn rollback_backup_dir(destination_root: &Path, at: chrono::DateTime<chrono::Local>) -> PathBuf {
    destination_root.join(format!("rollback-{}", at.format("%Y-%m-%d-%H%M%S")))
}

/// The file half of a rollback. Recordings and models go back to the folder
/// they were moved from; the settings and history go into `backup_dir`, which
/// is created only when there is something to keep. Returns whether it was.
///
/// Nothing is deleted, and every move is a rename: a rollback that stops
/// halfway leaves each file in exactly one of its two places.
fn rollback_paths(paths: &AdoptionPaths, backup_dir: &Path) -> Result<bool, IdentityAdoptionError> {
    for directory in ["recordings", "models"] {
        move_directory_children(
            &paths.destination_root.join(directory),
            &paths.source_root.join(directory),
        )?;
    }
    let mut kept = false;
    for file in ROLLBACK_KEPT_FILES {
        let path = paths.destination_root.join(file);
        if !path.is_file() {
            continue;
        }
        if !kept {
            create_private_dir(backup_dir).map_err(|_| IdentityAdoptionError::RollbackFailed)?;
            kept = true;
        }
        fs::rename(&path, backup_dir.join(file))
            .map_err(|_| IdentityAdoptionError::RollbackFailed)?;
    }
    Ok(kept)
}

fn create_private_dir(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Undo a completed adoption. Recordings, models and provider keys go back
/// to the legacy app; the settings and history Sona has now are kept in a
/// backup folder the receipt names. The caller ends the process afterwards:
/// the managers still hold the moved files open, and a fresh start is the
/// only state that reads them correctly.
#[tauri::command]
#[specta::specta]
pub async fn revert_identity_adoption(
    app: AppHandle,
    secrets: State<'_, std::sync::Arc<SecretManager>>,
) -> Result<IdentityRollbackReceipt, IdentityAdoptionError> {
    let destination_root =
        crate::portable::app_data_dir(&app).map_err(|_| IdentityAdoptionError::Unavailable)?;
    let receipt_path = destination_root.join(RECEIPT_FILE);
    let receipt = read_receipt(&receipt_path).ok_or(IdentityAdoptionError::RollbackUnavailable)?;
    if receipt.mode != IdentityAdoptionMode::Completed {
        return Err(IdentityAdoptionError::RollbackUnavailable);
    }
    if legacy_app_state() != LegacyAppState::Closed {
        return Err(IdentityAdoptionError::LegacyRunning);
    }
    let source_root = legacy_data_root().ok_or(IdentityAdoptionError::Unavailable)?;
    let paths = AdoptionPaths {
        source_root,
        destination_root,
    };
    let backup_dir = rollback_backup_dir(&paths.destination_root, chrono::Local::now());
    let kept = rollback_paths(&paths, &backup_dir)?;
    let AdoptionPaths {
        source_root,
        destination_root,
    } = paths;
    let legacy = SecretManager::native_for_service(LEGACY_FORK_SECRET_SERVICE_NAME);
    for credential in receipt
        .credentials
        .iter()
        .filter(|entry| entry.status == IdentityCredentialStatus::Moved)
    {
        let (kind, provider) = credential
            .account
            .split_once('/')
            .ok_or(IdentityAdoptionError::RollbackFailed)?;
        let kind = match kind {
            "llm" => SecretKind::Llm,
            "stt" => SecretKind::Stt,
            _ => return Err(IdentityAdoptionError::RollbackFailed),
        };
        match migrate_service_account(
            secrets.inner().as_ref(),
            &legacy,
            SecretAccount::for_provider(kind, provider)
                .map_err(|_| IdentityAdoptionError::RollbackFailed)?,
        )
        .await
        {
            ServiceAccountMigration::Moved | ServiceAccountMigration::AlreadyMoved => {}
            ServiceAccountMigration::NotFound | ServiceAccountMigration::NeedsReentry(_) => {
                return Err(IdentityAdoptionError::SecretMigrationFailed)
            }
        }
    }
    for path in [
        receipt_path,
        destination_root.join(JOURNAL_FILE),
        source_root.join(TOMBSTONE_FILE),
    ] {
        if path.exists() {
            fs::remove_file(path).map_err(|_| IdentityAdoptionError::RollbackFailed)?;
        }
    }
    Ok(IdentityRollbackReceipt {
        backup_dir: kept.then(|| backup_dir.to_string_lossy().into_owned()),
    })
}

fn move_directory_children(source: &Path, destination: &Path) -> Result<(), IdentityAdoptionError> {
    if !source.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(destination).map_err(|_| IdentityAdoptionError::RollbackFailed)?;
    let mut children = fs::read_dir(source)
        .map_err(|_| IdentityAdoptionError::RollbackFailed)?
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    children.sort_by_key(|entry| entry.file_name());
    for child in children {
        let source_child = child.path();
        let destination_child = destination.join(child.file_name());
        if destination_child.exists() {
            return Err(IdentityAdoptionError::DestinationConflict);
        }
        match fs::rename(&source_child, &destination_child) {
            Ok(()) => {}
            Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
                copy_then_delete(&source_child, &destination_child)
                    .map_err(|_| IdentityAdoptionError::RollbackFailed)?
            }
            Err(_) => return Err(IdentityAdoptionError::RollbackFailed),
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn legacy_app_state() -> LegacyAppState {
    use objc2_app_kit::NSRunningApplication;
    use objc2_foundation::NSString;
    let bundle = NSString::from_str(LEGACY_FORK_BUNDLE_ID);
    if NSRunningApplication::runningApplicationsWithBundleIdentifier(&bundle).is_empty() {
        LegacyAppState::Closed
    } else {
        LegacyAppState::Running
    }
}

#[cfg(target_os = "windows")]
fn legacy_app_state() -> LegacyAppState {
    use std::mem::size_of;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    let Ok(entry_size) = u32::try_from(size_of::<PROCESSENTRY32W>()) else {
        return LegacyAppState::Unverifiable;
    };
    // SAFETY: Flags and process ID are valid, and the returned handle is checked.
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return LegacyAppState::Unverifiable;
    };
    let mut entry = PROCESSENTRY32W {
        dwSize: entry_size,
        ..Default::default()
    };
    // SAFETY: snapshot is live and entry has the required size.
    let mut next = unsafe { Process32FirstW(snapshot, &mut entry).is_ok() };
    while next {
        let end = entry
            .szExeFile
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(entry.szExeFile.len());
        if String::from_utf16_lossy(&entry.szExeFile[..end]).eq_ignore_ascii_case("handy.exe") {
            // SAFETY: snapshot is live and this branch returns immediately after closing it.
            let _ = unsafe { CloseHandle(snapshot) };
            return LegacyAppState::Running;
        }
        // SAFETY: snapshot remains live and entry keeps the required size.
        next = unsafe { Process32NextW(snapshot, &mut entry).is_ok() };
    }
    // SAFETY: snapshot is live and has not been closed on this path.
    let _ = unsafe { CloseHandle(snapshot) };
    LegacyAppState::Closed
}

#[cfg(target_os = "linux")]
fn legacy_app_state() -> LegacyAppState {
    let Ok(processes) = fs::read_dir("/proc") else {
        return LegacyAppState::Unverifiable;
    };
    for process in processes.filter_map(Result::ok) {
        if !process
            .file_name()
            .to_string_lossy()
            .bytes()
            .all(|byte| byte.is_ascii_digit())
        {
            continue;
        }
        if fs::read_to_string(process.path().join("comm"))
            .ok()
            .is_some_and(|name| name.trim() == "handy")
        {
            return LegacyAppState::Running;
        }
    }
    LegacyAppState::Closed
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn legacy_app_state() -> LegacyAppState {
    LegacyAppState::Unverifiable
}

#[cfg(target_os = "macos")]
fn legacy_data_root() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from).map(|home| {
        home.join("Library/Application Support")
            .join(LEGACY_FORK_BUNDLE_ID)
    })
}

#[cfg(target_os = "windows")]
fn legacy_data_root() -> Option<PathBuf> {
    env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|root| root.join(LEGACY_FORK_BUNDLE_ID))
}

#[cfg(target_os = "linux")]
fn legacy_data_root() -> Option<PathBuf> {
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .map(|root| root.join(LEGACY_FORK_BUNDLE_ID))
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn legacy_data_root() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemorySecretBackend;
    use std::sync::Arc;

    fn test_paths(name: &str) -> (tempfile::TempDir, AdoptionPaths) {
        let root = tempfile::tempdir().expect("temporary root");
        let source_root = root.path().join(format!("legacy-{name}"));
        let destination_root = root.path().join(format!("sona-{name}"));
        fs::create_dir_all(&source_root).expect("legacy root");
        (
            root,
            AdoptionPaths {
                source_root,
                destination_root,
            },
        )
    }

    fn test_managers() -> (
        SecretManager,
        SecretManager,
        Arc<MemorySecretBackend>,
        Arc<MemorySecretBackend>,
    ) {
        let source = Arc::new(MemorySecretBackend::new());
        let destination = Arc::new(MemorySecretBackend::new());
        (
            SecretManager::with_backend(Arc::clone(&source) as Arc<_>),
            SecretManager::with_backend(Arc::clone(&destination) as Arc<_>),
            source,
            destination,
        )
    }

    fn move_data(_: LegacyAppState) -> AdoptionChoice {
        AdoptionChoice::Move
    }

    fn closed() -> LegacyAppState {
        LegacyAppState::Closed
    }

    #[test]
    fn adopts_a_virgin_target_with_copy_and_move_semantics() {
        let (_root, paths) = test_paths("full");
        fs::write(
            paths.source_root.join(SETTINGS_FILE),
            b"{\"autostart_enabled\":true}",
        )
        .expect("settings");
        fs::write(paths.source_root.join(HISTORY_FILE), b"history").expect("history");
        fs::create_dir_all(paths.source_root.join("recordings")).expect("recordings");
        fs::write(paths.source_root.join("recordings/clip.wav"), b"audio").expect("recording");
        fs::create_dir_all(paths.source_root.join("models")).expect("models");
        fs::write(paths.source_root.join("models/model.bin"), b"model").expect("model");
        let (source, destination, _, _) = test_managers();
        let receipt = adopt_paths(
            &paths,
            false,
            &mut move_data,
            &closed,
            &source,
            &destination,
            |_| {},
        )
        .expect("adoption");

        assert_eq!(receipt.mode, IdentityAdoptionMode::Completed);
        assert!(paths.source_root.join(SETTINGS_FILE).is_file());
        assert!(paths.destination_root.join(SETTINGS_FILE).is_file());
        assert!(!paths.source_root.join("recordings/clip.wav").exists());
        assert!(paths.destination_root.join("recordings/clip.wav").is_file());
        assert!(!paths.source_root.join("models/model.bin").exists());
        assert!(paths.source_root.join(TOMBSTONE_FILE).is_file());
    }

    /// After adopting, the user dictates: Sona's history grows past the copy
    /// the legacy folder kept. A rollback returns the recordings and models,
    /// keeps every byte Sona wrote since in one backup folder, and deletes
    /// nothing; the legacy folder still reads exactly as adoption left it.
    #[test]
    fn rollback_returns_moved_files_and_parks_everything_written_since() {
        let (_root, paths) = test_paths("rollback");
        fs::write(paths.source_root.join(SETTINGS_FILE), b"{}").expect("settings");
        fs::write(paths.source_root.join(HISTORY_FILE), b"legacy history").expect("history");
        fs::create_dir_all(paths.source_root.join("recordings")).expect("recordings");
        fs::write(paths.source_root.join("recordings/clip.wav"), b"audio").expect("recording");
        fs::create_dir_all(paths.source_root.join("models")).expect("models");
        fs::write(paths.source_root.join("models/model.bin"), b"model").expect("model");
        let (source, destination, _, _) = test_managers();
        adopt_paths(
            &paths,
            false,
            &mut move_data,
            &closed,
            &source,
            &destination,
            |_| {},
        )
        .expect("adoption");
        // Life after adoption: new words, a WAL, a new recording.
        fs::write(
            paths.destination_root.join(HISTORY_FILE),
            b"legacy history plus a new dictation",
        )
        .expect("grow history");
        fs::write(paths.destination_root.join("history.db-wal"), b"wal").expect("wal");
        fs::write(
            paths.destination_root.join("recordings/new.wav"),
            b"new audio",
        )
        .expect("new recording");

        let backup_dir = paths.destination_root.join("rollback-2026-09-13-101500");
        let kept = rollback_paths(&paths, &backup_dir).expect("rollback");

        assert!(kept);
        assert_eq!(
            fs::read(backup_dir.join(HISTORY_FILE)).expect("parked history"),
            b"legacy history plus a new dictation"
        );
        assert!(backup_dir.join("history.db-wal").is_file());
        assert!(backup_dir.join(SETTINGS_FILE).is_file());
        assert!(!paths.destination_root.join(HISTORY_FILE).exists());
        assert!(!paths.destination_root.join(SETTINGS_FILE).exists());
        assert_eq!(
            fs::read(paths.source_root.join(HISTORY_FILE)).expect("legacy history"),
            b"legacy history",
            "the legacy copy is not overwritten with a newer schema"
        );
        for recording in [
            "recordings/clip.wav",
            "recordings/new.wav",
            "models/model.bin",
        ] {
            assert!(
                paths.source_root.join(recording).is_file(),
                "{recording} went back"
            );
            assert!(!paths.destination_root.join(recording).exists());
        }
        // A root with nothing left to keep asks for no folder.
        let second = paths.destination_root.join("rollback-2026-09-13-101501");
        assert!(!rollback_paths(&paths, &second).expect("second rollback"));
        assert!(!second.exists());
    }

    #[test]
    fn receipt_fast_path_leaves_legacy_data_untouched() {
        let (_root, paths) = test_paths("receipt");
        fs::create_dir_all(&paths.destination_root).expect("destination");
        let expected = receipt(
            IdentityAdoptionMode::FreshStart,
            None,
            Vec::new(),
            Vec::new(),
        );
        write_receipt(&paths.destination_root.join(RECEIPT_FILE), &expected).expect("receipt");
        fs::write(paths.source_root.join(SETTINGS_FILE), b"legacy").expect("source");
        let (source, destination, _, _) = test_managers();

        let actual = adopt_paths(
            &paths,
            false,
            &mut move_data,
            &closed,
            &source,
            &destination,
            |_| panic!("receipt path must not apply autostart"),
        )
        .expect("fast path");

        assert_eq!(actual, expected);
        assert_eq!(
            fs::read(paths.source_root.join(SETTINGS_FILE)).expect("source unchanged"),
            b"legacy"
        );
    }

    #[test]
    fn journal_resume_accepts_an_already_moved_entry() {
        let (_root, paths) = test_paths("resume");
        fs::create_dir_all(paths.destination_root.join("recordings")).expect("destination");
        let destination = paths.destination_root.join("recordings/clip.wav");
        fs::write(&destination, b"audio").expect("destination file");

        let result = materialize_entry(
            &paths.source_root.join("recordings/clip.wav"),
            &destination,
            IdentityAdoptionAction::Renamed,
        )
        .expect("resume moved entry");

        assert_eq!(result.0, IdentityAdoptionAction::Renamed);
        assert_eq!(result.1, 5);
    }

    #[test]
    fn key_migration_write_reads_then_removes_legacy_value() {
        let (_root, paths) = test_paths("keys");
        let settings = serde_json::json!({
            "post_process_providers": [{"id": "openai"}],
            "cloud_stt_providers": []
        });
        fs::write(
            paths.source_root.join(SETTINGS_FILE),
            serde_json::to_vec(&settings).expect("settings JSON"),
        )
        .expect("settings");
        let (source, destination, source_backend, destination_backend) = test_managers();
        source_backend.insert("llm/openai", "legacy-key");
        let mut journal = AdoptionJournal::default();
        fs::create_dir_all(&paths.destination_root).expect("destination");

        migrate_credentials(
            &paths.source_root.join(SETTINGS_FILE),
            &source,
            &destination,
            &mut journal,
            &paths.destination_root.join(JOURNAL_FILE),
        )
        .expect("credential migration");

        assert!(!source_backend.has("llm/openai"));
        assert!(destination_backend.has("llm/openai"));
        assert_eq!(
            journal.credentials,
            vec![IdentityCredentialReceipt {
                account: "llm/openai".to_string(),
                status: IdentityCredentialStatus::Moved,
            }]
        );
    }

    #[test]
    fn portable_and_nonvirgin_guards_never_merge_data() {
        let (_root, paths) = test_paths("guards");
        fs::write(paths.source_root.join(SETTINGS_FILE), b"{}").expect("legacy settings");
        fs::create_dir_all(&paths.destination_root).expect("destination");
        fs::write(paths.destination_root.join(SETTINGS_FILE), b"{}").expect("existing settings");
        let (source, destination, _, _) = test_managers();
        let nonvirgin = adopt_paths(
            &paths,
            false,
            &mut move_data,
            &closed,
            &source,
            &destination,
            |_| {},
        )
        .expect("nonvirgin result");
        assert_eq!(nonvirgin.mode, IdentityAdoptionMode::SkippedNonvirgin);

        let (_root, portable_paths) = test_paths("portable");
        let portable = adopt_paths(
            &portable_paths,
            true,
            &mut move_data,
            &closed,
            &source,
            &destination,
            |_| {},
        )
        .expect("portable result");
        assert_eq!(portable.mode, IdentityAdoptionMode::Portable);
    }
}

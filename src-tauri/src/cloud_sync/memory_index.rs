use super::{
    map_store_error,
    revision_upload::{self, StagedRevision, UploadRefused},
    sha256_base64url, utc_now_ms, write_staged_file, CloudAccess, CloudRuntimeError,
    CloudSyncRuntime, ObjectPayloadManifest, MAX_BUNDLE_BYTES, MAX_PLAINTEXT_CHUNK_BYTES,
};
use crate::managers::history::HistoryManager;
use crate::meeting::store::memory_index::MemoryIndexRecords;
use crate::scratchpad::memory_index::{notes_in_snapshot, MemoryNote};
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
};
use tauri::{AppHandle, Manager};
use zeroize::Zeroizing;

const SOURCE_FORMAT: &str = "sona-memory-index-v1";
const CACHE_FILE: &str = "memory-index.json";

#[derive(Serialize)]
struct MemoryIndex<'a> {
    format_version: u32,
    kind: &'static str,
    source_device_id: &'a str,
    produced_at_utc_ms: i64,
    #[serde(flatten)]
    records: MemoryIndexRecords,
    notes: Vec<MemoryNote>,
}

#[derive(Default, Serialize, Deserialize)]
struct IndexState {
    vault_id: String,
    device_id: String,
    completed_digest: Option<String>,
    completed_produced_at_utc_ms: Option<i64>,
    pending: Option<PendingIndex>,
}

pub(super) fn phone_sync_directory(app: &AppHandle) -> Result<PathBuf, tauri::Error> {
    Ok(app.path().app_data_dir()?.join("phone-sync"))
}

/// When this Mac last finished publishing the phone's memory index; `None`
/// when it never has. Reads only the local state file, not the vault.
pub(crate) fn phone_snapshot_published_at(app: &AppHandle) -> io::Result<Option<i64>> {
    #[derive(Deserialize)]
    struct Published {
        completed_produced_at_utc_ms: Option<i64>,
    }
    let directory = phone_sync_directory(app).map_err(io::Error::other)?;
    match fs::read(directory.join(CACHE_FILE)) {
        Ok(bytes) => serde_json::from_slice::<Published>(&bytes)
            .map(|state| state.completed_produced_at_utc_ms)
            .map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[derive(Serialize, Deserialize)]
struct PendingIndex {
    digest: String,
    produced_at_utc_ms: i64,
    #[serde(flatten)]
    revision: StagedRevision,
}

struct SnapshotWriter<'a> {
    bytes: &'a mut Vec<u8>,
    exceeded: bool,
}

impl io::Write for SnapshotWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_BUNDLE_BYTES.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(io::Error::other("memory index exceeds 8 MiB"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode_snapshot(index: &MemoryIndex<'_>, bytes: &mut Vec<u8>) -> Result<(), CloudRuntimeError> {
    bytes.clear();
    let mut writer = SnapshotWriter {
        bytes,
        exceeded: false,
    };
    if serde_json::to_writer(&mut writer, index).is_err() {
        return Err(if writer.exceeded {
            CloudRuntimeError::MemoryIndexTooLarge
        } else {
            CloudRuntimeError::IntegrityFailure
        });
    }
    Ok(())
}

fn stage(
    access: &CloudAccess,
    plaintext: &[u8],
    digest: String,
    produced_at_utc_ms: i64,
) -> Result<PendingIndex, CloudRuntimeError> {
    let object_id =
        sha256_base64url(format!("{SOURCE_FORMAT}:{}", access.state.device_id).as_bytes());
    let plaintext_chunks: Vec<&[u8]> = plaintext.chunks(MAX_PLAINTEXT_CHUNK_BYTES).collect();
    let manifest_plaintext = serde_json::to_vec(&ObjectPayloadManifest {
        version: 1,
        kind: "memory_index".to_owned(),
        source_format: SOURCE_FORMAT.to_owned(),
        chunk_count: u32::try_from(plaintext_chunks.len())
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
        plaintext_bytes: u64::try_from(plaintext.len())
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
        plaintext_sha256: digest.clone(),
    })
    .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
    Ok(PendingIndex {
        digest,
        produced_at_utc_ms,
        revision: revision_upload::stage(
            access,
            SOURCE_FORMAT,
            object_id,
            &manifest_plaintext,
            &plaintext_chunks,
        )?,
    })
}

fn persist(directory: &Path, state: &IndexState) -> Result<(), CloudRuntimeError> {
    write_staged_file(
        directory,
        CACHE_FILE,
        &serde_json::to_vec(state).map_err(|_| CloudRuntimeError::File)?,
    )
}

pub(super) async fn sync(
    runtime: &CloudSyncRuntime,
    access: &CloudAccess,
) -> Result<(), CloudRuntimeError> {
    let directory = phone_sync_directory(&runtime.app).map_err(|_| CloudRuntimeError::File)?;
    fs::create_dir_all(&directory).map_err(|_| CloudRuntimeError::File)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
        .map_err(|_| CloudRuntimeError::File)?;
    let mut state = match fs::read(directory.join(CACHE_FILE)) {
        Ok(bytes) => serde_json::from_slice::<IndexState>(&bytes)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => IndexState::default(),
        Err(_) => return Err(CloudRuntimeError::File),
    };
    if state.vault_id != access.state.vault_id || state.device_id != access.state.device_id {
        state = IndexState {
            vault_id: access.state.vault_id.clone(),
            device_id: access.state.device_id.clone(),
            ..IndexState::default()
        };
    }
    if state.pending.is_none() {
        let history = runtime
            .app
            .try_state::<Arc<HistoryManager>>()
            .ok_or(CloudRuntimeError::SetupRequired)?;
        let mut index = access
            .store
            .with_memory_index_snapshot(|records, connection| {
                Ok(MemoryIndex {
                    format_version: 1,
                    kind: "memory_index",
                    source_device_id: &access.state.device_id,
                    produced_at_utc_ms: state.completed_produced_at_utc_ms.unwrap_or(0),
                    records,
                    notes: notes_in_snapshot(&history, connection)?,
                })
            })
            .map_err(map_store_error)?;
        let mut plaintext = Zeroizing::new(Vec::new());
        encode_snapshot(&index, &mut plaintext)?;
        let digest = sha256_base64url(&plaintext);
        if state.completed_digest.as_deref() == Some(&digest) {
            return Ok(());
        }
        // Reuse the previous timestamp for comparison, not a new revision every scan.
        index.produced_at_utc_ms = utc_now_ms();
        encode_snapshot(&index, &mut plaintext)?;
        let digest = sha256_base64url(&plaintext);
        state.pending = Some(stage(access, &plaintext, digest, index.produced_at_utc_ms)?);
        persist(&directory, &state)?;
    }
    let pending = state
        .pending
        .as_mut()
        .ok_or(CloudRuntimeError::IntegrityFailure)?;
    match revision_upload::upload(runtime, access, SOURCE_FORMAT, &mut pending.revision).await {
        Ok(()) => {}
        Err(UploadRefused::Restage(error)) => {
            state.pending = None;
            persist(&directory, &state)?;
            return Err(error);
        }
        Err(UploadRefused::Retry(error)) => return Err(error),
    }
    state.completed_digest = Some(pending.digest.clone());
    state.completed_produced_at_utc_ms = Some(pending.produced_at_utc_ms);
    state.pending = None;
    persist(&directory, &state)
}

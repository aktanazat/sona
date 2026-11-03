use super::{
    base64_url_encode, credentials, map_store_error, random_array, random_opaque_id,
    seal_object_revision_payload, sha256_base64url, sign_canonical_upload_envelope,
    stable_idempotency_value, utc_now_ms, write_staged_file, CanonicalUploadEnvelopeInput,
    CloudAccess, CloudClientError, CloudErrorCode, CloudHead, CloudRuntimeError, CloudSyncRuntime,
    IdempotencyKey, ObjectContentKind, ObjectPayloadManifest, ObjectRevisionCryptoContext,
    ObjectUploadPlan, UploadChunk, UploadChunkPlan, UploadKind, MAX_BUNDLE_BYTES,
    MAX_PLAINTEXT_CHUNK_BYTES,
};
use crate::managers::history::HistoryManager;
use crate::meeting::store::memory_index::MemoryIndexRecords;
use crate::scratchpad::memory_index::{notes_in_snapshot, MemoryNote};
use serde::{Deserialize, Serialize};
use std::{fs, io, os::unix::fs::PermissionsExt, path::Path, sync::Arc};
use tauri::Manager;
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

/// Sealed payloads include their nonces; retries never reseal a revision.
#[derive(Serialize, Deserialize)]
struct PendingIndex {
    digest: String,
    produced_at_utc_ms: i64,
    object_id: String,
    revision_id: String,
    upload_id: String,
    base_revision_id: Option<String>,
    manifest: Vec<u8>,
    chunks: Vec<Vec<u8>>,
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
    let revision_id = random_opaque_id()?;
    let plaintext_chunks = plaintext.chunks(MAX_PLAINTEXT_CHUNK_BYTES);
    let chunk_count =
        u32::try_from(plaintext_chunks.len()).map_err(|_| CloudRuntimeError::IntegrityFailure)?;
    let manifest_plaintext = serde_json::to_vec(&ObjectPayloadManifest {
        version: 1,
        kind: "memory_index".to_owned(),
        source_format: SOURCE_FORMAT.to_owned(),
        chunk_count,
        plaintext_bytes: u64::try_from(plaintext.len())
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
        plaintext_sha256: digest.clone(),
    })
    .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
    let context = ObjectRevisionCryptoContext {
        vault_id: &access.state.vault_id,
        object_id: &object_id,
        revision_id: &revision_id,
        index: 0,
        total: u64::from(chunk_count),
        content_kind: ObjectContentKind::Manifest,
        source_format: SOURCE_FORMAT,
    };
    let manifest = seal_object_revision_payload(
        &*access.keys.vault_root,
        &context,
        &random_array::<12>()?,
        &manifest_plaintext,
    )
    .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
    let mut chunks = Vec::with_capacity(plaintext_chunks.len());
    for (index, chunk) in plaintext_chunks.enumerate() {
        let index = u64::try_from(index).map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        let context = ObjectRevisionCryptoContext {
            index,
            content_kind: ObjectContentKind::Chunk,
            ..context
        };
        chunks.push(
            seal_object_revision_payload(
                &*access.keys.vault_root,
                &context,
                &random_array::<12>()?,
                chunk,
            )
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
        );
    }
    let base_revision_id = access
        .store
        .cloud_head(&object_id)
        .map_err(map_store_error)?
        .and_then(|head| head.remote_revision_id);
    Ok(PendingIndex {
        digest,
        produced_at_utc_ms,
        object_id,
        revision_id,
        upload_id: random_opaque_id()?,
        base_revision_id,
        manifest,
        chunks,
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
    let directory = runtime
        .app
        .path()
        .app_data_dir()
        .map_err(|_| CloudRuntimeError::File)?
        .join("phone-sync");
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
    let chunks: Vec<UploadChunkPlan> = pending
        .chunks
        .iter()
        .enumerate()
        .map(|(index, bytes)| {
            Ok(UploadChunkPlan {
                index: u32::try_from(index).map_err(|_| CloudRuntimeError::IntegrityFailure)?,
                size: u64::try_from(bytes.len())
                    .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
                sha256: sha256_base64url(bytes),
            })
        })
        .collect::<Result<_, CloudRuntimeError>>()?;
    let descriptors: Vec<UploadChunk<'_>> = chunks
        .iter()
        .map(|chunk| UploadChunk {
            index: u64::from(chunk.index),
            size: chunk.size,
            sha256: &chunk.sha256,
        })
        .collect();
    let manifest_digest = sha256_base64url(&pending.manifest);
    let total_bytes = chunks.iter().map(|chunk| chunk.size).sum();
    let signature = sign_canonical_upload_envelope(
        &CanonicalUploadEnvelopeInput {
            vault_id: &access.state.vault_id,
            kind: UploadKind::Object,
            object_id: Some(&pending.object_id),
            revision_id: Some(&pending.revision_id),
            base_revision_id: pending.base_revision_id.as_deref(),
            share_id: None,
            manifest_digest: &manifest_digest,
            crypto_version: 1,
            total_bytes,
            chunks: &descriptors,
        },
        &*access.keys.signing_seed,
    )
    .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
    let plan = ObjectUploadPlan {
        version: 1,
        crypto_version: 1,
        upload_id: pending.upload_id.clone(),
        object_id: pending.object_id.clone(),
        revision_id: pending.revision_id.clone(),
        base_revision_id: pending.base_revision_id.clone(),
        manifest: base64_url_encode(&pending.manifest),
        manifest_sha256: manifest_digest,
        chunk_count: u32::try_from(chunks.len())
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
        chunks,
        total_bytes,
        writer_signature: base64_url_encode(&signature),
    };
    let key = |operation: &str| {
        IdempotencyKey::new(stable_idempotency_value(&[
            SOURCE_FORMAT,
            &plan.upload_id,
            operation,
        ]))
        .map_err(CloudRuntimeError::Client)
    };
    let credentials = credentials(access)?;
    runtime.require_request_permission(&access.state).await?;
    let created = access
        .client
        .create_object_upload(&credentials, &key("create")?, &plan)
        .await;
    runtime.persist_clock(&access.store, &access.client);
    let created = match created {
        Err(CloudClientError::Api(error))
            if matches!(
                error.code,
                CloudErrorCode::NotFound | CloudErrorCode::StaleRevision
            ) =>
        {
            state.pending = None;
            persist(&directory, &state)?;
            return Err(CloudRuntimeError::Client(CloudClientError::Api(error)));
        }
        other => other.map_err(CloudRuntimeError::Client)?,
    };
    if created.upload_id != pending.upload_id {
        return Err(CloudRuntimeError::IntegrityFailure);
    }
    if created.state != "active" && created.state != "committed" {
        state.pending = None;
        persist(&directory, &state)?;
        return Err(CloudRuntimeError::Deferred);
    }
    if created.state != "committed" {
        for (index, bytes) in pending.chunks.iter_mut().enumerate() {
            let index = u32::try_from(index).map_err(|_| CloudRuntimeError::IntegrityFailure)?;
            if created.accepted_indexes.contains(&index) {
                continue;
            }
            runtime.require_request_permission(&access.state).await?;
            let response = access
                .client
                .upload_chunk(
                    &credentials,
                    &key(&format!("chunk-{index}"))?,
                    &pending.upload_id,
                    index,
                    std::mem::take(bytes),
                )
                .await
                .map_err(CloudRuntimeError::Client)?;
            if response.upload_id != pending.upload_id
                || response.index != index
                || !response.accepted
            {
                return Err(CloudRuntimeError::IntegrityFailure);
            }
        }
    }
    runtime.require_request_permission(&access.state).await?;
    let committed = access
        .client
        .commit_upload(&credentials, &key("commit")?, &pending.upload_id)
        .await
        .map_err(CloudRuntimeError::Client)?;
    if committed.state != "committed"
        || committed.upload_id != pending.upload_id
        || committed.revision_id.as_deref() != Some(&pending.revision_id)
    {
        return Err(CloudRuntimeError::IntegrityFailure);
    }
    access
        .store
        .upsert_cloud_head(&CloudHead {
            object_id: pending.object_id.clone(),
            source_session_id: None,
            remote_revision_id: Some(pending.revision_id.clone()),
            tombstone: false,
            acknowledged_revision_id: Some(pending.revision_id.clone()),
            change_sequence: committed.change_sequence.unwrap_or(0),
        })
        .map_err(map_store_error)?;
    state.completed_digest = Some(pending.digest.clone());
    state.completed_produced_at_utc_ms = Some(pending.produced_at_utc_ms);
    state.pending = None;
    persist(&directory, &state)
}

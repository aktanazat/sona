use super::{
    acknowledge_remote_revision, base64_url_decode, chunk_file_name, credentials, map_store_error,
    phone_thought_manifest::{self, AttachmentDigests, ThoughtManifest, SOURCE_FORMAT},
    sha256_base64url, verify_writer_signature, write_staged_file, CloudAccess, CloudRuntimeError,
    CloudSyncRuntime, ObjectContentKind, ObjectRevisionCryptoContext, ObjectRevisionEnvelope,
    CRYPTO_VERSION, OBJECT_MANIFEST_FILE,
};
use crate::{managers::history::HistoryManager, scratchpad::phone_import};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
};
use tauri::Manager;
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
struct StoredRevision {
    chunk_count: u32,
    manifest_sha256: String,
    writer_device_id: String,
    writer_signature: String,
    parent_revision_id: Option<String>,
    total_bytes: u64,
    chunks: Vec<(u64, u64, String)>,
}

pub(super) struct ThoughtImport {
    manifest: ThoughtManifest,
    staged: StagedDirectory,
}

struct StagedDirectory(Option<PathBuf>);

impl Drop for StagedDirectory {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            if let Err(error) = fs::remove_dir_all(path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    log::warn!("Cloud thought staging cleanup failed: {error}");
                }
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub(crate) struct PhoneThoughtSource {
    pub reference: phone_import::PhoneThoughtReference,
    pub manifest: ThoughtManifest,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum PhoneThoughtAttachment {
    Audio,
    Image { index: u32 },
}

impl CloudSyncRuntime {
    fn thought_history(&self) -> Arc<HistoryManager> {
        Arc::clone(self.app.state::<Arc<HistoryManager>>().inner())
    }

    fn thought_directory(
        &self,
        vault_id: &str,
        object_id: &str,
    ) -> Result<PathBuf, CloudRuntimeError> {
        let key = sha256_base64url(format!("{vault_id}\0{object_id}").as_bytes());
        Ok(crate::portable::app_data_dir(&self.app)
            .map_err(|_| CloudRuntimeError::File)?
            .join("phone-sync")
            .join("thoughts")
            .join(key))
    }

    fn thought_revision_directory(
        &self,
        vault_id: &str,
        object_id: &str,
        revision_id: &str,
    ) -> Result<PathBuf, CloudRuntimeError> {
        Ok(self
            .thought_directory(vault_id, object_id)?
            .join(sha256_base64url(revision_id.as_bytes())))
    }

    pub(super) async fn download_phone_thought(
        &self,
        access: &CloudAccess,
        object_id: &str,
        revision_id: &str,
        envelope: &ObjectRevisionEnvelope,
        encrypted_manifest: &[u8],
        manifest: ThoughtManifest,
    ) -> Result<ThoughtImport, CloudRuntimeError> {
        manifest.validate(envelope)?;
        let signing_key = self.writer_signing_key(access, envelope).await?;
        let directory = self.thought_directory(&access.state.vault_id, object_id)?;
        private_directory(&directory)?;
        let path = directory.join(format!(".staging-{}", Uuid::new_v4()));
        private_directory(&path)?;
        let staged = StagedDirectory(Some(path));
        let path = staged.0.as_ref().ok_or(CloudRuntimeError::File)?;
        write_staged_file(path, OBJECT_MANIFEST_FILE, encrypted_manifest)?;
        let mut digests = AttachmentDigests::new(&manifest);
        let descriptors = self
            .download_chunks(access, object_id, revision_id, envelope, |index, bytes| {
                digests.receive(
                    &*access.keys.vault_root,
                    &ObjectRevisionCryptoContext {
                        vault_id: &access.state.vault_id,
                        object_id,
                        revision_id,
                        index,
                        total: u64::from(envelope.chunk_count),
                        content_kind: ObjectContentKind::Chunk,
                        source_format: SOURCE_FORMAT,
                    },
                    &bytes,
                )?;
                let index =
                    u32::try_from(index).map_err(|_| CloudRuntimeError::IntegrityFailure)?;
                write_staged_file(path, &chunk_file_name(index), &bytes)
            })
            .await?;
        verify_writer_signature(
            &access.state.vault_id,
            object_id,
            revision_id,
            envelope,
            &descriptors,
            &signing_key,
        )?;
        write_staged_file(
            path,
            "revision.json",
            &serde_json::to_vec(&StoredRevision {
                chunk_count: envelope.chunk_count,
                manifest_sha256: envelope.manifest_sha256.clone(),
                writer_device_id: envelope.writer_device_id.clone(),
                writer_signature: envelope.writer_signature.clone(),
                parent_revision_id: envelope.parent_revision_id.clone(),
                total_bytes: envelope.total_bytes,
                chunks: descriptors,
            })
            .map_err(|_| CloudRuntimeError::File)?,
        )?;
        drop(digests);
        Ok(ThoughtImport { manifest, staged })
    }

    pub(super) fn install_phone_thought(
        &self,
        access: &CloudAccess,
        object_id: &str,
        revision_id: &str,
        sequence: u64,
        mut thought: ThoughtImport,
    ) -> Result<(), CloudRuntimeError> {
        let _guard = self
            .phone_thought_gate
            .lock()
            .map_err(|_| CloudRuntimeError::Storage)?;
        let history = self.thought_history();
        if phone_import::is_deleted(&history, &access.state.vault_id, object_id)
            .map_err(|_| CloudRuntimeError::Storage)?
        {
            return Ok(());
        }
        if let Some(reference) =
            phone_import::reference(&history, &access.state.vault_id, object_id, None)
                .map_err(|_| CloudRuntimeError::Storage)?
        {
            if reference.change_sequence
                > i64::try_from(sequence).map_err(|_| CloudRuntimeError::IntegrityFailure)?
            {
                return Ok(());
            }
            if reference.revision_id == revision_id {
                return acknowledge_remote_revision(
                    &access.store,
                    object_id,
                    revision_id,
                    sequence,
                );
            }
        }
        let current = access
            .store
            .cloud_head(object_id)
            .map_err(map_store_error)?;
        if current
            .as_ref()
            .is_some_and(|head| head.tombstone || head.change_sequence > sequence)
        {
            return Ok(());
        }
        let destination =
            self.thought_revision_directory(&access.state.vault_id, object_id, revision_id)?;
        let staged = thought.staged.0.as_ref().ok_or(CloudRuntimeError::File)?;
        if !destination.exists() {
            fs::rename(staged, &destination).map_err(|_| CloudRuntimeError::File)?;
            thought.staged.0 = None;
            if let Some(parent) = destination.parent() {
                fs::File::open(parent)
                    .and_then(|file| file.sync_all())
                    .map_err(|_| CloudRuntimeError::File)?;
            }
        }
        let manifest = &thought.manifest;
        phone_import::import(
            &self.app,
            &history,
            phone_import::PhoneThoughtNote {
                vault_id: &access.state.vault_id,
                object_id,
                revision_id,
                captured_at_utc_ms: manifest.captured_at_utc_ms,
                origin: manifest.origin.as_str(),
                device_id: &manifest.device_id,
                text: &manifest.text,
                link: manifest.link.as_ref().map(|link| link.url.as_str()),
                has_audio: manifest.audio.is_some(),
                image_count: manifest.images.len(),
                change_sequence: i64::try_from(sequence)
                    .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
            },
        )
        .map_err(|_| CloudRuntimeError::Storage)?;
        acknowledge_remote_revision(&access.store, object_id, revision_id, sequence)?;
        self.emit_changed(None, None);
        Ok(())
    }

    pub(super) fn delete_phone_thought_source(
        &self,
        access: &CloudAccess,
        object_id: &str,
        revision_id: &str,
        sequence: u64,
    ) -> Result<(), CloudRuntimeError> {
        let _guard = self
            .phone_thought_gate
            .lock()
            .map_err(|_| CloudRuntimeError::Storage)?;
        let history = self.thought_history();
        if phone_import::reference(&history, &access.state.vault_id, object_id, None)
            .map_err(|_| CloudRuntimeError::Storage)?
            .is_some_and(|reference| {
                u64::try_from(reference.change_sequence).is_ok_and(|stored| stored > sequence)
            })
        {
            return Ok(());
        }
        phone_import::mark_deleted(
            &self.app,
            &history,
            &access.state.vault_id,
            object_id,
            revision_id,
            i64::try_from(sequence).map_err(|_| CloudRuntimeError::IntegrityFailure)?,
        )
        .map_err(|_| CloudRuntimeError::Storage)?;
        let directory = self.thought_directory(&access.state.vault_id, object_id)?;
        match fs::remove_dir_all(directory) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(CloudRuntimeError::File),
        }
    }

    pub(super) async fn backfill_phone_thoughts(
        &self,
        access: &CloudAccess,
    ) -> Result<(), CloudRuntimeError> {
        let history = self.thought_history();
        if phone_import::backfill_complete(&history, &access.state.vault_id)
            .map_err(|_| CloudRuntimeError::Storage)?
        {
            return Ok(());
        }
        let mut high_water = None;
        let mut after = None;
        loop {
            self.require_request_permission(&access.state).await?;
            let page = access
                .client
                .snapshot(
                    &credentials(access)?,
                    high_water.as_deref(),
                    after.as_deref(),
                    Some(100),
                )
                .await
                .map_err(CloudRuntimeError::Client)?;
            self.persist_clock(&access.store, &access.client);
            for head in &page.heads {
                let current = access
                    .store
                    .cloud_head(&head.object_id)
                    .map_err(map_store_error)?;
                if current.as_ref().is_some_and(|current| {
                    current.change_sequence > head.sequence || current.source_session_id.is_some()
                }) {
                    continue;
                }
                if head.tombstone {
                    self.delete_phone_thought_source(
                        access,
                        &head.object_id,
                        &head.revision_id,
                        head.sequence,
                    )?;
                    continue;
                }
                if current.as_ref().is_some_and(|current| current.tombstone) {
                    continue;
                }
                if phone_import::is_deleted(&history, &access.state.vault_id, &head.object_id)
                    .map_err(|_| CloudRuntimeError::Storage)?
                {
                    continue;
                }
                if phone_import::reference(
                    &history,
                    &access.state.vault_id,
                    &head.object_id,
                    Some(&head.revision_id),
                )
                .map_err(|_| CloudRuntimeError::Storage)?
                .is_some()
                {
                    continue;
                }
                match self
                    .backfill_phone_thought_head(
                        access,
                        &head.object_id,
                        &head.revision_id,
                        head.sequence,
                    )
                    .await
                {
                    Err(CloudRuntimeError::IntegrityFailure) => {
                        log::warn!("Cloud thought backfill refused an invalid revision");
                    }
                    result => result?,
                }
            }
            high_water = Some(page.high_water);
            after = page.after;
            if !page.has_more {
                break;
            }
        }
        phone_import::finish_backfill(&history, &access.state.vault_id)
            .map_err(|_| CloudRuntimeError::Storage)
    }

    async fn backfill_phone_thought_head(
        &self,
        access: &CloudAccess,
        object_id: &str,
        revision_id: &str,
        sequence: u64,
    ) -> Result<(), CloudRuntimeError> {
        self.require_request_permission(&access.state).await?;
        let response = access
            .client
            .object_manifest(&credentials(access)?, object_id, revision_id)
            .await
            .map_err(CloudRuntimeError::Client)?;
        self.persist_clock(&access.store, &access.client);
        let encrypted = base64_url_decode(&response.manifest)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        let envelope = &response.envelope;
        if envelope.object_id != object_id
            || envelope.revision_id != revision_id
            || envelope.crypto_version != CRYPTO_VERSION
            || sha256_base64url(&encrypted) != envelope.manifest_sha256
        {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        let manifest = phone_thought_manifest::open_manifest(
            &*access.keys.vault_root,
            &ObjectRevisionCryptoContext {
                vault_id: &access.state.vault_id,
                object_id,
                revision_id,
                index: 0,
                total: u64::from(envelope.chunk_count),
                content_kind: ObjectContentKind::Manifest,
                source_format: SOURCE_FORMAT,
            },
            &encrypted,
        )?;
        if let Some(manifest) = manifest {
            let thought = self
                .download_phone_thought(
                    access,
                    object_id,
                    revision_id,
                    envelope,
                    &encrypted,
                    manifest,
                )
                .await?;
            self.install_phone_thought(access, object_id, revision_id, sequence, thought)?;
        }
        Ok(())
    }

    pub(crate) async fn phone_thought_source(
        &self,
        object_id: &str,
        revision_id: &str,
    ) -> Result<Option<PhoneThoughtSource>, CloudRuntimeError> {
        let store = self
            .meetings
            .cloud_store()
            .await
            .map_err(|_| CloudRuntimeError::SetupRequired)?;
        let state = store
            .cloud_state()
            .map_err(map_store_error)?
            .ok_or(CloudRuntimeError::SetupRequired)?;
        let keys = self
            .secrets
            .cloud_sync_keys()
            .await
            .map_err(|_| CloudRuntimeError::SecretUnavailable)?;
        let _guard = self
            .phone_thought_gate
            .lock()
            .map_err(|_| CloudRuntimeError::Storage)?;
        let Some(reference) = phone_import::reference(
            &self.thought_history(),
            &state.vault_id,
            object_id,
            Some(revision_id),
        )
        .map_err(|_| CloudRuntimeError::Storage)?
        else {
            return Ok(None);
        };
        if reference.source_deleted {
            return Ok(None);
        }
        let directory =
            self.thought_revision_directory(&state.vault_id, object_id, &reference.revision_id)?;
        let metadata: StoredRevision = serde_json::from_slice(
            &fs::read(directory.join("revision.json")).map_err(|_| CloudRuntimeError::File)?,
        )
        .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        let encrypted =
            fs::read(directory.join(OBJECT_MANIFEST_FILE)).map_err(|_| CloudRuntimeError::File)?;
        if sha256_base64url(&encrypted) != metadata.manifest_sha256 {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        let manifest = phone_thought_manifest::open_manifest(
            &*keys.vault_root,
            &ObjectRevisionCryptoContext {
                vault_id: &state.vault_id,
                object_id,
                revision_id: &reference.revision_id,
                index: 0,
                total: u64::from(metadata.chunk_count),
                content_kind: ObjectContentKind::Manifest,
                source_format: SOURCE_FORMAT,
            },
            &encrypted,
        )?
        .ok_or(CloudRuntimeError::IntegrityFailure)?;
        manifest.validate(&ObjectRevisionEnvelope {
            object_id: object_id.to_owned(),
            revision_id: reference.revision_id.clone(),
            parent_revision_id: metadata.parent_revision_id,
            manifest_sha256: metadata.manifest_sha256,
            chunk_count: metadata.chunk_count,
            total_bytes: metadata.total_bytes,
            crypto_version: CRYPTO_VERSION,
            writer_device_id: metadata.writer_device_id,
            writer_signature: metadata.writer_signature,
        })?;
        Ok(Some(PhoneThoughtSource {
            reference,
            manifest,
        }))
    }

    pub(crate) async fn phone_thought_attachment(
        &self,
        object_id: &str,
        revision_id: &str,
        selection: PhoneThoughtAttachment,
    ) -> Result<Vec<u8>, CloudRuntimeError> {
        let source = self
            .phone_thought_source(object_id, revision_id)
            .await?
            .ok_or(CloudRuntimeError::IntegrityFailure)?;
        let (start, count, length, digest) = match selection {
            PhoneThoughtAttachment::Audio => {
                let audio = source
                    .manifest
                    .audio
                    .as_ref()
                    .ok_or(CloudRuntimeError::IntegrityFailure)?;
                (
                    audio.chunk_start,
                    audio.chunk_count,
                    audio.byte_length,
                    audio.sha256.as_str(),
                )
            }
            PhoneThoughtAttachment::Image { index } => {
                let image = source
                    .manifest
                    .images
                    .get(index as usize)
                    .ok_or(CloudRuntimeError::IntegrityFailure)?;
                (
                    image.chunk_start,
                    image.chunk_count,
                    image.byte_length,
                    image.sha256.as_str(),
                )
            }
        };
        let directory = self.thought_revision_directory(
            &source.reference.vault_id,
            object_id,
            &source.reference.revision_id,
        )?;
        let metadata: StoredRevision = serde_json::from_slice(
            &fs::read(directory.join("revision.json")).map_err(|_| CloudRuntimeError::File)?,
        )
        .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        let keys = self
            .secrets
            .cloud_sync_keys()
            .await
            .map_err(|_| CloudRuntimeError::SecretUnavailable)?;
        let _guard = self
            .phone_thought_gate
            .lock()
            .map_err(|_| CloudRuntimeError::Storage)?;
        let reference = phone_import::reference(
            &self.thought_history(),
            &source.reference.vault_id,
            object_id,
            Some(revision_id),
        )
        .map_err(|_| CloudRuntimeError::Storage)?
        .ok_or(CloudRuntimeError::IntegrityFailure)?;
        if reference.source_deleted || reference.revision_id != source.reference.revision_id {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        let attachment = phone_thought_manifest::Attachment {
            start,
            count,
            length,
            sha256: digest,
        };
        let mut bytes = zeroize::Zeroizing::new(Vec::with_capacity(
            usize::try_from(length).map_err(|_| CloudRuntimeError::IntegrityFailure)?,
        ));
        for index in start..start + count {
            let encrypted = fs::read(directory.join(chunk_file_name(index)))
                .map_err(|_| CloudRuntimeError::File)?;
            bytes.extend_from_slice(&phone_thought_manifest::open_chunk(
                &*keys.vault_root,
                &ObjectRevisionCryptoContext {
                    vault_id: &source.reference.vault_id,
                    object_id,
                    revision_id: &source.reference.revision_id,
                    index: u64::from(index),
                    total: u64::from(metadata.chunk_count),
                    content_kind: ObjectContentKind::Chunk,
                    source_format: SOURCE_FORMAT,
                },
                &encrypted,
                Some(&attachment),
            )?);
        }
        if sha256_base64url(&bytes) != digest {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        Ok(std::mem::take(&mut *bytes))
    }
}

fn private_directory(path: &Path) -> Result<(), CloudRuntimeError> {
    fs::create_dir_all(path).map_err(|_| CloudRuntimeError::File)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|_| CloudRuntimeError::File)
}

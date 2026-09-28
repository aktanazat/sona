use super::*;
use std::os::unix::fs::PermissionsExt;
use tauri::Manager;
use zeroize::Zeroizing;

const SOURCE_FORMAT: &str = "sona-dictation-profile-v1";
const CACHE_FILE: &str = "dictation-profile.json";

#[derive(Serialize)]
struct Style<'a> {
    id: &'a str,
    name: &'a str,
    prompt: String,
}

#[derive(Serialize)]
struct Profile<'a> {
    version: u32,
    vocabulary: &'a [settings::VocabularyEntry],
    replacements: Vec<&'a settings::ReplacementRule>,
    snippets: Vec<&'a crate::snippets::Snippet>,
    styles: Vec<Style<'a>>,
}

#[derive(Default, Serialize, Deserialize)]
struct ProfileState {
    vault_id: String,
    device_id: String,
    completed_digest: Option<String>,
    pending: Option<PendingProfile>,
}

/// Only ciphertext is staged. Keeping the upload identifiers and nonces stable makes
/// a process restart or a lost commit response resume the same remote revision.
#[derive(Serialize, Deserialize)]
struct PendingProfile {
    digest: String,
    object_id: String,
    revision_id: String,
    upload_id: String,
    base_revision_id: Option<String>,
    manifest: Vec<u8>,
    chunks: Vec<Vec<u8>>,
}

fn profile_bytes(
    settings: &settings::AppSettings,
) -> Result<Zeroizing<Vec<u8>>, CloudRuntimeError> {
    let styles = settings
        .modes
        .iter()
        .filter(|mode| {
            mode.llm.enabled && mode.llm.cleanup_level != Some(crate::modes::CleanupLevel::None)
        })
        .map(|mode| Style {
            id: &mode.id,
            name: &mode.name,
            prompt: crate::prompt_renderer::writing_style(
                mode.tone,
                mode.prompt.preset,
                mode.prompt.custom_prompt.as_deref(),
                &settings.persona_samples,
                mode.asr.literal_punctuation,
                mode.llm.cleanup_level,
            ),
        })
        .collect();
    let profile = Profile {
        version: 1,
        vocabulary: &settings.custom_words,
        replacements: settings
            .replacements_rules
            .iter()
            .filter(|rule| settings.replacements_enabled && rule.enabled && rule.is_usable())
            .collect(),
        snippets: settings
            .snippets
            .iter()
            .filter(|snippet| settings.snippets_enabled && snippet.enabled && snippet.is_usable())
            .collect(),
        styles,
    };
    let bytes = Zeroizing::new(
        serde_json::to_vec(&profile).map_err(|_| CloudRuntimeError::IntegrityFailure)?,
    );
    if bytes.len() > MAX_BUNDLE_BYTES {
        return Err(CloudRuntimeError::IntegrityFailure);
    }
    Ok(bytes)
}

fn stage(
    access: &CloudAccess,
    plaintext: &[u8],
    digest: String,
) -> Result<PendingProfile, CloudRuntimeError> {
    let object_id =
        sha256_base64url(format!("{SOURCE_FORMAT}:{}", access.state.device_id).as_bytes());
    let revision_id = random_opaque_id()?;
    let plaintext_chunks = plaintext.chunks(MAX_PLAINTEXT_CHUNK_BYTES);
    let chunk_count =
        u32::try_from(plaintext_chunks.len()).map_err(|_| CloudRuntimeError::IntegrityFailure)?;
    let manifest_plaintext = serde_json::to_vec(&ObjectPayloadManifest {
        version: 1,
        kind: "dictation_profile".to_owned(),
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
    Ok(PendingProfile {
        digest,
        object_id,
        revision_id,
        upload_id: random_opaque_id()?,
        base_revision_id,
        manifest,
        chunks,
    })
}

fn persist(directory: &Path, state: &ProfileState) -> Result<(), CloudRuntimeError> {
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
        Ok(bytes) => serde_json::from_slice::<ProfileState>(&bytes)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ProfileState::default(),
        Err(_) => return Err(CloudRuntimeError::File),
    };
    if state.vault_id != access.state.vault_id || state.device_id != access.state.device_id {
        state = ProfileState {
            vault_id: access.state.vault_id.clone(),
            device_id: access.state.device_id.clone(),
            ..ProfileState::default()
        };
    }
    if state.pending.is_none() {
        let settings = settings::get_settings(&runtime.app);
        let plaintext = profile_bytes(&settings)?;
        let digest = sha256_base64url(&plaintext);
        if state.completed_digest.as_deref() == Some(&digest) {
            return Ok(());
        }
        state.pending = Some(stage(access, &plaintext, digest)?);
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
    state.pending = None;
    persist(&directory, &state)
}

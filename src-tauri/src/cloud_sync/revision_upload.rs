use super::{
    base64_url_encode, credentials, map_store_error, random_array, random_opaque_id,
    seal_object_revision_payload, sha256_base64url, sign_canonical_upload_envelope,
    stable_idempotency_value, CanonicalUploadEnvelopeInput, CloudAccess, CloudClientError,
    CloudErrorCode, CloudHead, CloudRuntimeError, CloudSyncRuntime, IdempotencyKey,
    ObjectContentKind, ObjectRevisionCryptoContext, ObjectUploadPlan, UploadChunk, UploadChunkPlan,
    UploadKind,
};
use serde::{Deserialize, Serialize};

/// One object revision sealed and waiting to upload. Sealed payloads include
/// their nonces and are persisted before the first request, so a retry sends
/// the same bytes instead of resealing a revision.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct StagedRevision {
    pub(super) object_id: String,
    pub(super) revision_id: String,
    pub(super) upload_id: String,
    pub(super) base_revision_id: Option<String>,
    pub(super) manifest: Vec<u8>,
    pub(super) chunks: Vec<Vec<u8>>,
}

/// How an upload of a staged revision ended without committing.
pub(super) enum UploadRefused {
    /// The service will never take these bytes: the base revision moved or
    /// the upload is gone. The caller drops the staged revision and stages
    /// again from the current head.
    Restage(CloudRuntimeError),
    /// Anything else; the same staged bytes are retried later.
    Retry(CloudRuntimeError),
}

pub(super) fn stage(
    access: &CloudAccess,
    source_format: &str,
    object_id: String,
    manifest_plaintext: &[u8],
    chunk_plaintexts: &[&[u8]],
) -> Result<StagedRevision, CloudRuntimeError> {
    let revision_id = random_opaque_id()?;
    let total =
        u64::try_from(chunk_plaintexts.len()).map_err(|_| CloudRuntimeError::IntegrityFailure)?;
    let context = ObjectRevisionCryptoContext {
        vault_id: &access.state.vault_id,
        object_id: &object_id,
        revision_id: &revision_id,
        index: 0,
        total,
        content_kind: ObjectContentKind::Manifest,
        source_format,
    };
    let manifest = seal_object_revision_payload(
        &*access.keys.vault_root,
        &context,
        &random_array::<12>()?,
        manifest_plaintext,
    )
    .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
    let mut chunks = Vec::with_capacity(chunk_plaintexts.len());
    for (index, chunk) in chunk_plaintexts.iter().enumerate() {
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
    Ok(StagedRevision {
        object_id,
        revision_id,
        upload_id: random_opaque_id()?,
        base_revision_id,
        manifest,
        chunks,
    })
}

/// Upload, commit and record the head of a staged revision. Chunks are taken
/// out of `staged` as they are sent; the caller's persisted copy keeps them.
pub(super) async fn upload(
    runtime: &CloudSyncRuntime,
    access: &CloudAccess,
    source_format: &str,
    staged: &mut StagedRevision,
) -> Result<(), UploadRefused> {
    let retry = UploadRefused::Retry;
    let chunks: Vec<UploadChunkPlan> = staged
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
        .collect::<Result<_, CloudRuntimeError>>()
        .map_err(retry)?;
    let descriptors: Vec<UploadChunk<'_>> = chunks
        .iter()
        .map(|chunk| UploadChunk {
            index: u64::from(chunk.index),
            size: chunk.size,
            sha256: &chunk.sha256,
        })
        .collect();
    let manifest_digest = sha256_base64url(&staged.manifest);
    let total_bytes = chunks.iter().map(|chunk| chunk.size).sum();
    let signature = sign_canonical_upload_envelope(
        &CanonicalUploadEnvelopeInput {
            vault_id: &access.state.vault_id,
            kind: UploadKind::Object,
            object_id: Some(&staged.object_id),
            revision_id: Some(&staged.revision_id),
            base_revision_id: staged.base_revision_id.as_deref(),
            share_id: None,
            manifest_digest: &manifest_digest,
            crypto_version: 1,
            total_bytes,
            chunks: &descriptors,
        },
        &*access.keys.signing_seed,
    )
    .map_err(|_| retry(CloudRuntimeError::IntegrityFailure))?;
    let plan = ObjectUploadPlan {
        version: 1,
        crypto_version: 1,
        upload_id: staged.upload_id.clone(),
        object_id: staged.object_id.clone(),
        revision_id: staged.revision_id.clone(),
        base_revision_id: staged.base_revision_id.clone(),
        manifest: base64_url_encode(&staged.manifest),
        manifest_sha256: manifest_digest,
        chunk_count: u32::try_from(chunks.len())
            .map_err(|_| retry(CloudRuntimeError::IntegrityFailure))?,
        chunks,
        total_bytes,
        writer_signature: base64_url_encode(&signature),
    };
    let key = |operation: &str| {
        IdempotencyKey::new(stable_idempotency_value(&[
            source_format,
            &plan.upload_id,
            operation,
        ]))
        .map_err(|error| retry(CloudRuntimeError::Client(error)))
    };
    let credentials = credentials(access).map_err(retry)?;
    runtime
        .require_request_permission(&access.state)
        .await
        .map_err(retry)?;
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
            return Err(UploadRefused::Restage(CloudRuntimeError::Client(
                CloudClientError::Api(error),
            )));
        }
        other => other.map_err(|error| retry(CloudRuntimeError::Client(error)))?,
    };
    if created.upload_id != staged.upload_id {
        return Err(retry(CloudRuntimeError::IntegrityFailure));
    }
    if created.state != "active" && created.state != "committed" {
        return Err(UploadRefused::Restage(CloudRuntimeError::Deferred));
    }
    if created.state != "committed" {
        for (index, bytes) in staged.chunks.iter_mut().enumerate() {
            let index =
                u32::try_from(index).map_err(|_| retry(CloudRuntimeError::IntegrityFailure))?;
            if created.accepted_indexes.contains(&index) {
                continue;
            }
            runtime
                .require_request_permission(&access.state)
                .await
                .map_err(retry)?;
            let response = access
                .client
                .upload_chunk(
                    &credentials,
                    &key(&format!("chunk-{index}"))?,
                    &staged.upload_id,
                    index,
                    std::mem::take(bytes),
                )
                .await
                .map_err(|error| retry(CloudRuntimeError::Client(error)))?;
            if response.upload_id != staged.upload_id
                || response.index != index
                || !response.accepted
            {
                return Err(retry(CloudRuntimeError::IntegrityFailure));
            }
        }
    }
    runtime
        .require_request_permission(&access.state)
        .await
        .map_err(retry)?;
    let committed = access
        .client
        .commit_upload(&credentials, &key("commit")?, &staged.upload_id)
        .await
        .map_err(|error| retry(CloudRuntimeError::Client(error)))?;
    if committed.state != "committed"
        || committed.upload_id != staged.upload_id
        || committed.revision_id.as_deref() != Some(&staged.revision_id)
    {
        return Err(retry(CloudRuntimeError::IntegrityFailure));
    }
    access
        .store
        .upsert_cloud_head(&CloudHead {
            object_id: staged.object_id.clone(),
            source_session_id: None,
            remote_revision_id: Some(staged.revision_id.clone()),
            tombstone: false,
            acknowledged_revision_id: Some(staged.revision_id.clone()),
            change_sequence: committed.change_sequence.unwrap_or(0),
        })
        .map_err(|error| retry(map_store_error(error)))
}

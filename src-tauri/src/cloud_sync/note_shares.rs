use super::*;
use crate::cloud_sync::share_document::scratch_document;
use crate::cloud_sync::types::{CloudNoteShareCreateRequest, CloudNoteShareListRequest};

impl CloudSyncRuntime {
    pub(crate) async fn note_share_create(
        &self,
        request: CloudNoteShareCreateRequest,
    ) -> Result<CloudBrowserShareResult, CloudRuntimeError> {
        self.reject_portable()?;
        let store = self.queueable_store().await?;
        let access = self.configured_access().await?;
        if self.meetings.is_capture_active() {
            return Err(CloudRuntimeError::Deferred);
        }
        let now = adjusted_now_ms(access.state.clock_offset_ms);
        if request.expires_at_utc_ms <= now
            || request.expires_at_utc_ms > now.saturating_add(MAX_SHARE_EXPIRY_MS)
        {
            return Err(CloudRuntimeError::IntegrityFailure);
        }
        let object_id = note_object_id(&request.note_id)?;
        let note = crate::scratchpad::read_note(&self.app, &request.note_id)
            .map_err(|_| CloudRuntimeError::File)?
            .ok_or(CloudRuntimeError::Conflict)?;
        let document = scratch_document(&note.title, &note.body)
            .map_err(|_| CloudRuntimeError::IntegrityFailure)?;
        let share_id = random_opaque_id()?;
        let mut root = random_array::<32>()?;
        let outbox = store
            .enqueue_cloud_outbox(CloudOutboxInput {
                kind: CloudOutboxKind::Share,
                object_id: share_id.clone(),
                source_session_id: None,
                source_revision: None,
                base_remote_revision_id: None,
                share_content_kind: Some(CloudShareContentKind::BrowserMarkdown),
                remote_revision_id: None,
                idempotency_key: stable_idempotency_value(&["share", &share_id]),
                next_attempt_utc_ms: utc_now_ms(),
            })
            .map_err(map_store_error)?;
        let record = store.create_cloud_share(CloudShareInput {
            share_id,
            object_id,
            source_session_id: None,
            expires_at_utc_ms: request.expires_at_utc_ms,
            content_kind: CloudShareContentKind::BrowserMarkdown,
            encrypted_link_material: base64_url_encode(&root),
            outbox_id: Some(outbox.outbox_id.clone()),
        });
        root.zeroize();
        let record = match record {
            Ok(record) => record,
            Err(error) => {
                store
                    .cancel_cloud_outbox(&outbox.outbox_id)
                    .map_err(map_store_error)?;
                return Err(map_store_error(error));
            }
        };
        let staged = self.stage_share(&store, &outbox, &record, Some(document));
        let (mut root, _) = match staged {
            Ok(staged) => staged,
            Err(error) => {
                store
                    .cancel_cloud_outbox(&outbox.outbox_id)
                    .map_err(map_store_error)?;
                store
                    .update_cloud_share(
                        &record.share_id,
                        CloudShareUpdate {
                            expires_at_utc_ms: record.expires_at_utc_ms,
                            state: CloudShareState::Failed,
                            outbox_id: Some(outbox.outbox_id),
                            revoked_at_utc_ms: None,
                        },
                    )
                    .map_err(map_store_error)?;
                return Err(error);
            }
        };
        let share_url = browser_share_url(&access.state.endpoint, &record.share_id, &root);
        root.zeroize();
        self.emit_changed(None, Some(CloudObjectState::Queued));
        Ok(CloudBrowserShareResult {
            share_id: record.share_id,
            expires_at_utc_ms: record.expires_at_utc_ms,
            share_url,
            trust_disclosure: BROWSER_SHARE_TRUST_DISCLOSURE.to_owned(),
        })
    }

    pub(crate) async fn note_share_list(
        &self,
        request: CloudNoteShareListRequest,
    ) -> Result<Vec<CloudShareSummary>, CloudRuntimeError> {
        let store = self
            .meetings
            .cloud_store()
            .await
            .map_err(|_| CloudRuntimeError::SetupRequired)?;
        let object_id = request.note_id.as_deref().map(note_object_id).transpose()?;
        let records = store
            .cloud_note_shares(object_id.as_deref())
            .map_err(map_store_error)?;
        share_summaries(&store, records)
    }

    /// Recover a previously created link from encrypted local storage, without
    /// making a second share or uploading newer content.
    pub(crate) async fn browser_share_link(
        &self,
        request: CloudShareRevokeRequest,
    ) -> Result<CloudBrowserShareResult, CloudRuntimeError> {
        let access = self.configured_access().await?;
        let record = access
            .store
            .cloud_share(&request.share_id)
            .map_err(map_store_error)?
            .ok_or(CloudRuntimeError::Conflict)?;
        if record.content_kind != CloudShareContentKind::BrowserMarkdown
            || !matches!(
                record.state,
                CloudShareState::Pending | CloudShareState::Active
            )
            || record.expires_at_utc_ms <= adjusted_now_ms(access.state.clock_offset_ms)
        {
            return Err(CloudRuntimeError::Conflict);
        }
        let mut root = fixed_array_32(
            base64_url_decode(&record.encrypted_link_material)
                .map_err(|_| CloudRuntimeError::IntegrityFailure)?,
        )?;
        let share_url = browser_share_url(&access.state.endpoint, &record.share_id, &root);
        root.zeroize();
        Ok(CloudBrowserShareResult {
            share_id: record.share_id,
            expires_at_utc_ms: record.expires_at_utc_ms,
            share_url,
            trust_disclosure: BROWSER_SHARE_TRUST_DISCLOSURE.to_owned(),
        })
    }
}

fn note_object_id(note_id: &str) -> Result<String, CloudRuntimeError> {
    let id = Uuid::parse_str(note_id).map_err(|_| CloudRuntimeError::IntegrityFailure)?;
    Ok(format!("note_{id}"))
}

pub(super) fn share_summaries(
    store: &MeetingStore,
    records: Vec<CloudShareRecord>,
) -> Result<Vec<CloudShareSummary>, CloudRuntimeError> {
    records
        .into_iter()
        .map(|record| {
            let outbox = record
                .outbox_id
                .as_deref()
                .map(|id| store.cloud_outbox(id).map_err(map_store_error))
                .transpose()?
                .flatten();
            Ok(CloudShareSummary {
                share_id: record.share_id,
                kind: match record.content_kind {
                    CloudShareContentKind::CapabilityBundle => CloudShareKind::File,
                    CloudShareContentKind::BrowserMarkdown => CloudShareKind::Browser,
                },
                expires_at_utc_ms: record.expires_at_utc_ms,
                state: share_lifecycle(record.state, outbox.map(|item| item.state)),
                revoked_at_utc_ms: record.revoked_at_utc_ms,
            })
        })
        .collect()
}

use crate::meeting::context::PlaceReading;
use crate::meeting::conversation_review::{
    ConversationAudioClip, ConversationBookmark, ConversationBookmarkRequest,
    ConversationPlaceReview, ConversationReel,
};
use crate::meeting::loop_types::MeetingLoopId;
use crate::meeting::people_types::PersonId;
use crate::meeting::session::{map_store_error, MeetingSessionManager};
use crate::meeting::types::{
    MeetingCommandError, MeetingEventPayload, MeetingSessionId, TranscriptSegmentId,
};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

#[tauri::command]
#[specta::specta]
pub async fn conversation_audio_clip(
    app: AppHandle,
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
    segment_id: Option<TranscriptSegmentId>,
    start_offset_ns: Option<u64>,
    end_offset_ns: Option<u64>,
) -> Result<ConversationAudioClip, MeetingCommandError> {
    audio_clip(
        crate::utils::capture_running(&app),
        &manager,
        session_id,
        segment_id,
        start_offset_ns,
        end_offset_ns,
    )
    .await
}

/// Nothing plays while anything records: heard aloud, the clip would be
/// recorded again.
async fn audio_clip(
    capturing: bool,
    manager: &MeetingSessionManager,
    session_id: MeetingSessionId,
    segment_id: Option<TranscriptSegmentId>,
    start_offset_ns: Option<u64>,
    end_offset_ns: Option<u64>,
) -> Result<ConversationAudioClip, MeetingCommandError> {
    if capturing {
        return Err(MeetingCommandError::CaptureLeaseBusy);
    }
    let store = manager.store().await?;
    tauri::async_runtime::spawn_blocking(move || {
        store.conversation_audio_clip(session_id, segment_id, start_offset_ns, end_offset_ns)
    })
    .await
    .map_err(|_| MeetingCommandError::StorageUnavailable)?
    .map_err(map_store_error)
}

#[tauri::command]
#[specta::specta]
pub async fn conversation_reel_for_person(
    manager: State<'_, Arc<MeetingSessionManager>>,
    person_id: PersonId,
) -> Result<ConversationReel, MeetingCommandError> {
    let store = manager.store().await?;
    let user_name = manager.user_name();
    tauri::async_runtime::spawn_blocking(move || {
        store.conversation_reel_for_person(person_id, user_name.as_deref())
    })
    .await
    .map_err(|_| MeetingCommandError::StorageUnavailable)?
    .map_err(map_store_error)
}

#[tauri::command]
#[specta::specta]
pub async fn conversation_reel_for_loop(
    manager: State<'_, Arc<MeetingSessionManager>>,
    loop_id: MeetingLoopId,
) -> Result<ConversationReel, MeetingCommandError> {
    let store = manager.store().await?;
    let user_name = manager.user_name();
    tauri::async_runtime::spawn_blocking(move || {
        store.conversation_reel_for_loop(&loop_id, user_name.as_deref())
    })
    .await
    .map_err(|_| MeetingCommandError::StorageUnavailable)?
    .map_err(map_store_error)
}

#[tauri::command]
#[specta::specta]
pub async fn conversation_audio_retained(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Result<bool, MeetingCommandError> {
    let store = manager.store().await?;
    let session = store
        .session_snapshot(session_id)
        .map_err(map_store_error)?;
    Ok(session.transcript_purged_at_utc_ms.is_none()
        && session.audio_purged_at_utc_ms.is_none()
        && session.phase != crate::meeting::types::MeetingPhase::Deleting)
}

#[tauri::command]
#[specta::specta]
pub async fn conversation_place_review(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Result<ConversationPlaceReview, MeetingCommandError> {
    manager
        .store()
        .await?
        .conversation_place_review(session_id)
        .map_err(map_store_error)
}

/// The reading a phone recording arrived with, handed over once. The place
/// card offers the places near it and keeps none of it.
#[tauri::command]
#[specta::specta]
pub fn conversation_place_reading_take(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Option<PlaceReading> {
    manager.take_place_reading(session_id)
}

#[tauri::command]
#[specta::specta]
pub async fn conversation_bookmarks(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Result<Vec<ConversationBookmark>, MeetingCommandError> {
    manager
        .store()
        .await?
        .conversation_bookmarks(session_id)
        .map_err(map_store_error)
}

#[tauri::command]
#[specta::specta]
pub async fn conversation_bookmark_create(
    app: AppHandle,
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: ConversationBookmarkRequest,
) -> Result<ConversationBookmark, MeetingCommandError> {
    let store = manager.store().await?;
    let result = store
        .conversation_bookmark_create(request)
        .map_err(map_store_error)?;
    let session = store
        .session_snapshot(result.session_id)
        .map_err(map_store_error)?;
    let _ = app.emit(
        "meeting:session-changed",
        MeetingEventPayload {
            event_schema_version: 1,
            session_id: Some(result.session_id),
            revision: session.revision,
        },
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::audio_clip;
    use crate::meeting::session::{MeetingSessionManager, NoCaptureSources};
    use crate::meeting::types::{MeetingCommandError, MeetingSessionId};
    use crate::secrets::{MemorySecretBackend, SecretManager};
    use std::sync::Arc;
    use tempfile::TempDir;

    /// The refusal is what answers while anything records; the same request
    /// with nothing recording goes on to the store, which knows no such talk.
    #[test]
    fn a_clip_is_refused_while_anything_records() {
        let directory = TempDir::new().unwrap();
        let manager = MeetingSessionManager::with_parts(
            None,
            Some(directory.path().join("meetings")),
            Arc::new(SecretManager::with_backend(Arc::new(
                MemorySecretBackend::new(),
            ))),
            Arc::new(NoCaptureSources),
        );
        let session_id = MeetingSessionId::new();
        let clip = |capturing| {
            tauri::async_runtime::block_on(audio_clip(
                capturing,
                &manager,
                session_id,
                None,
                Some(0),
                None,
            ))
            .err()
        };

        assert_eq!(clip(true), Some(MeetingCommandError::CaptureLeaseBusy));
        assert_eq!(clip(false), Some(MeetingCommandError::NotFound));
    }
}

use crate::meeting::conversation_review::{
    ConversationAudioClip, ConversationBookmark, ConversationBookmarkRequest,
    ConversationPlaceReview,
};
use crate::meeting::session::{map_store_error, MeetingSessionManager};
use crate::meeting::types::{
    MeetingCommandError, MeetingEventPayload, MeetingSessionId, TranscriptSegmentId,
};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

#[tauri::command]
#[specta::specta]
pub async fn conversation_audio_clip(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
    segment_id: Option<TranscriptSegmentId>,
    start_offset_ns: Option<u64>,
) -> Result<ConversationAudioClip, MeetingCommandError> {
    let store = manager.store().await?;
    tauri::async_runtime::spawn_blocking(move || {
        store.conversation_audio_clip(session_id, segment_id, start_offset_ns)
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

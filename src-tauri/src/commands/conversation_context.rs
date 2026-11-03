use crate::meeting::context::{ConversationContext, ConversationContextUpdate};
use crate::meeting::session::{map_store_error, MeetingSessionManager};
use crate::meeting::types::{MeetingCommandError, MeetingEventPayload, MeetingSessionId};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

#[tauri::command]
#[specta::specta]
pub async fn conversation_context(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Result<ConversationContext, MeetingCommandError> {
    manager
        .store()
        .await?
        .conversation_context(session_id)
        .map_err(map_store_error)
}

#[tauri::command]
#[specta::specta]
pub async fn conversation_context_update(
    app: AppHandle,
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: ConversationContextUpdate,
) -> Result<ConversationContext, MeetingCommandError> {
    let store = manager.store().await?;
    let result = store
        .update_conversation_context(request)
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

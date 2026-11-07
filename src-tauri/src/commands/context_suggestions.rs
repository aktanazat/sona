use crate::meeting::evening::{FilingSuggestion, MeetingMaybeChange};
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::types::MeetingSessionId;
use std::sync::Arc;
use tauri::{AppHandle, State};

const UNAVAILABLE: &str = "The meeting library is unavailable.";

/// The maybes waiting on one recording: the events near its start, the place
/// its invite pins, the kind of conversation its tracks heard, the meeting it
/// may be. None of them has changed the recording.
#[tauri::command]
#[specta::specta]
pub async fn context_suggestions(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Result<Vec<FilingSuggestion>, String> {
    waiting(&manager, session_id).await
}

/// Takes one maybe, as `change` edits a meeting maybe, or turns it down, and
/// answers with what still waits.
#[tauri::command]
#[specta::specta]
pub async fn context_suggestion_decide(
    app: AppHandle,
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
    suggestion_id: String,
    accept: bool,
    change: Option<MeetingMaybeChange>,
) -> Result<Vec<FilingSuggestion>, String> {
    manager
        .decide_filing(&app, &suggestion_id, accept, change)
        .await?;
    waiting(&manager, session_id).await
}

async fn waiting(
    manager: &MeetingSessionManager,
    session_id: MeetingSessionId,
) -> Result<Vec<FilingSuggestion>, String> {
    let store = manager.store().await.map_err(|_| UNAVAILABLE.to_string())?;
    let heard = manager.heard_voices(session_id);
    let user_name = manager.user_name();
    tauri::async_runtime::spawn_blocking(move || {
        store.context_suggestions(
            session_id,
            &heard,
            user_name.as_deref(),
            chrono::Utc::now().timestamp_millis(),
        )
    })
    .await
    .map_err(|_| UNAVAILABLE.to_string())?
    .map_err(|_| UNAVAILABLE.to_string())
}

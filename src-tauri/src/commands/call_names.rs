use crate::meeting::call_speakers::{CallNameStatus, CallNameTargets};
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::types::{MeetingCommandError, MeetingSessionId, SpeakerId};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
#[specta::specta]
pub async fn meeting_call_name_targets(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Result<CallNameTargets, MeetingCommandError> {
    manager.call_name_targets(session_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_call_name_status(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Result<CallNameStatus, MeetingCommandError> {
    manager.call_name_status(session_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_call_names_set(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
    target_id: Option<String>,
    automatically_use: bool,
) -> Result<CallNameStatus, MeetingCommandError> {
    manager.call_names_set(session_id, target_id, automatically_use).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_call_name_dismiss(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
    speaker_id: SpeakerId,
) -> Result<CallNameStatus, MeetingCommandError> {
    manager.call_name_dismiss(session_id, speaker_id).await
}

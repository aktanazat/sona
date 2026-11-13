use crate::meeting::live_doc_types::MeetingLiveDoc;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::types::{MeetingCommandError, MeetingSessionId};
use std::sync::Arc;
use tauri::State;

/// A meeting's doc: the running capture's while it records, the saved one
/// after.
#[tauri::command]
#[specta::specta]
pub async fn meeting_live_doc(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Result<MeetingLiveDoc, MeetingCommandError> {
    manager.live_doc(session_id).await
}

/// Start the doc of the meeting recording now, from everything said so far.
#[tauri::command]
#[specta::specta]
pub async fn meeting_live_doc_start(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Result<MeetingLiveDoc, MeetingCommandError> {
    manager.start_live_doc(session_id).await
}

/// Change, add (no `point_id`) or remove (blank `text`) one point of a
/// meeting's doc by hand. Later readings never change it.
#[tauri::command]
#[specta::specta]
pub async fn meeting_live_doc_edit(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
    point_id: Option<String>,
    text: String,
) -> Result<MeetingLiveDoc, MeetingCommandError> {
    manager.edit_live_doc(session_id, point_id, text).await
}

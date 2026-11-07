use crate::meeting::session::{map_store_error, MeetingSessionManager};
use crate::meeting::types::{MeetingCommandError, MeetingSessionId};
use crate::meeting::wrap_types::MeetingWrap;
use std::sync::Arc;
use tauri::State;

/// Who was there, where, the plans heard and the open promises of one
/// meeting. A read: every draft it leads to waits for the user's tap.
#[tauri::command]
#[specta::specta]
pub async fn meeting_wrap(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Result<MeetingWrap, MeetingCommandError> {
    manager
        .store()
        .await?
        .meeting_wrap(session_id)
        .map_err(map_store_error)
}

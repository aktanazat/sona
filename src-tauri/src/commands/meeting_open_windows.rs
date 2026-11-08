use std::sync::Arc;
use tauri::State;

use crate::meeting::open_windows::{
    MeetingOpenWindow, MeetingOpenWindowsService, OpenWindowAttachment, OpenWindowId,
};
use crate::meeting::types::{MeetingCommandError, MeetingSessionId};

#[tauri::command]
#[specta::specta]
pub async fn meeting_open_windows(
    service: State<'_, Arc<MeetingOpenWindowsService>>,
    session_id: MeetingSessionId,
) -> Result<Vec<MeetingOpenWindow>, MeetingCommandError> {
    service.list(session_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_open_window_attach(
    service: State<'_, Arc<MeetingOpenWindowsService>>,
    session_id: MeetingSessionId,
    item_id: OpenWindowId,
) -> Result<OpenWindowAttachment, MeetingCommandError> {
    service.attach(session_id, item_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_open_window_remove(
    service: State<'_, Arc<MeetingOpenWindowsService>>,
    session_id: MeetingSessionId,
    item_id: OpenWindowId,
) -> Result<(), MeetingCommandError> {
    service.remove(session_id, item_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_open_windows_dismiss(
    service: State<'_, Arc<MeetingOpenWindowsService>>,
    session_id: MeetingSessionId,
) -> Result<(), MeetingCommandError> {
    service.dismiss(session_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn change_meeting_open_windows_enabled_setting(
    service: State<'_, Arc<MeetingOpenWindowsService>>,
    enabled: bool,
) -> Result<(), String> {
    service.set_enabled(enabled).await
}

use std::sync::Arc;
use tauri::State;

use crate::meeting::snapshots::{
    MeetingSnapshotError, MeetingSnapshotId, MeetingSnapshotImage, MeetingSnapshotService,
    MeetingSnapshotSize, MeetingSnapshotStatus, MeetingSnapshotSummary,
};
use crate::meeting::types::MeetingSessionId;

#[tauri::command]
#[specta::specta]
pub async fn meeting_snapshot_take(
    service: State<'_, Arc<MeetingSnapshotService>>,
    session_id: MeetingSessionId,
) -> Result<MeetingSnapshotSummary, MeetingSnapshotError> {
    service.inner().take(session_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_snapshot_list(
    service: State<'_, Arc<MeetingSnapshotService>>,
    session_id: MeetingSessionId,
) -> Result<Vec<MeetingSnapshotSummary>, MeetingSnapshotError> {
    service.list(session_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_snapshot_image(
    service: State<'_, Arc<MeetingSnapshotService>>,
    session_id: MeetingSessionId,
    snapshot_id: MeetingSnapshotId,
    size: MeetingSnapshotSize,
) -> Result<MeetingSnapshotImage, MeetingSnapshotError> {
    service.image(session_id, snapshot_id, size).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_snapshot_delete(
    service: State<'_, Arc<MeetingSnapshotService>>,
    session_id: MeetingSessionId,
    snapshot_id: MeetingSnapshotId,
) -> Result<(), MeetingSnapshotError> {
    service.delete(session_id, snapshot_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_snapshot_status(
    service: State<'_, Arc<MeetingSnapshotService>>,
    session_id: MeetingSessionId,
) -> Result<MeetingSnapshotStatus, MeetingSnapshotError> {
    service.status(session_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_snapshot_automatic_set(
    service: State<'_, Arc<MeetingSnapshotService>>,
    session_id: MeetingSessionId,
    enabled: bool,
) -> Result<MeetingSnapshotStatus, MeetingSnapshotError> {
    service.set_automatic(session_id, enabled).await
}

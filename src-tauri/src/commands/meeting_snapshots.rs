use std::sync::Arc;
use tauri::State;

use crate::meeting::snapshots::{
    MeetingSnapshotError, MeetingSnapshotId, MeetingSnapshotImage, MeetingSnapshotService,
    MeetingSnapshotSize, MeetingSnapshotStatus, MeetingSnapshotSummary, MeetingSnapshotTextMark,
    MeetingSnapshotTextMoment,
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

/// Turns reading the words in meeting pictures on or off. Off forgets every
/// word read so far.
#[tauri::command]
#[specta::specta]
pub async fn meeting_snapshot_text_set(
    service: State<'_, Arc<MeetingSnapshotService>>,
    enabled: bool,
) -> Result<(), MeetingSnapshotError> {
    service.inner().set_text_reading(enabled).await
}

/// One meeting's "On screen" marks, for its transcript.
#[tauri::command]
#[specta::specta]
pub async fn meeting_snapshot_text_marks(
    service: State<'_, Arc<MeetingSnapshotService>>,
    session_id: MeetingSessionId,
) -> Result<Vec<MeetingSnapshotTextMark>, MeetingSnapshotError> {
    service.text_marks(session_id).await
}

/// The pictures whose words a palette query matches. A query under two
/// letters matches nothing.
#[tauri::command]
#[specta::specta]
pub async fn meeting_snapshot_text_search(
    service: State<'_, Arc<MeetingSnapshotService>>,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<MeetingSnapshotTextMoment>, MeetingSnapshotError> {
    service.text_search(query, limit.unwrap_or(4)).await
}

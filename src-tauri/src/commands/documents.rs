use crate::meeting::document_types::{
    DocumentDeleteRequest, DocumentIngestRequest, DocumentListResult, DocumentMutationResult,
    MarkedCopyRequest, MeetingDocument,
};
use crate::meeting::people_types::PersonId;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::types::{MeetingCommandError, MeetingSessionId};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
#[specta::specta]
pub async fn doc_ingest(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: DocumentIngestRequest,
) -> Result<DocumentMutationResult, MeetingCommandError> {
    manager.doc_ingest(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn doc_list(
    manager: State<'_, Arc<MeetingSessionManager>>,
    person_id: Option<PersonId>,
) -> Result<DocumentListResult, MeetingCommandError> {
    manager.doc_list(person_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn doc_delete(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: DocumentDeleteRequest,
) -> Result<DocumentMutationResult, MeetingCommandError> {
    manager.doc_delete(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn doc_meeting_list(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Result<Vec<MeetingDocument>, MeetingCommandError> {
    manager.doc_meeting_list(session_id).await
}

/// Save a highlighted copy of a meeting's PDF where the operator chose, on
/// the operator's tap. The PDF the meeting keeps is never changed.
#[tauri::command]
#[specta::specta]
pub async fn doc_marked_copy(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MarkedCopyRequest,
) -> Result<u32, MeetingCommandError> {
    manager.doc_marked_copy(request).await
}

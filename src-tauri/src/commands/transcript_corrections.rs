use crate::meeting::people_types::PersonId;
use crate::meeting::session::{map_store_error, MeetingSessionManager};
use crate::meeting::transcript_correction_types::{
    TranscriptCorrectionList, TranscriptCorrectionMutationResult,
    TranscriptCorrectionRemoveRequest, TranscriptCorrectionReview,
    TranscriptCorrectionTeachRequest, TranscriptCorrectionUpdateRequest,
};
use crate::meeting::types::{MeetingCommandError, MeetingEventPayload, MeetingSessionId};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

#[tauri::command]
#[specta::specta]
pub async fn transcript_correction_list(
    manager: State<'_, Arc<MeetingSessionManager>>,
    person_id: PersonId,
) -> Result<TranscriptCorrectionList, MeetingCommandError> {
    manager
        .store()
        .await?
        .transcript_corrections(person_id)
        .map_err(map_store_error)
}

#[tauri::command]
#[specta::specta]
pub async fn transcript_correction_review(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Result<TranscriptCorrectionReview, MeetingCommandError> {
    manager
        .store()
        .await?
        .transcript_correction_review(session_id)
        .map_err(map_store_error)
}

#[tauri::command]
#[specta::specta]
pub async fn transcript_correction_teach(
    app: AppHandle,
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: TranscriptCorrectionTeachRequest,
) -> Result<TranscriptCorrectionMutationResult, MeetingCommandError> {
    let result = manager
        .store()
        .await?
        .teach_transcript_correction(request)
        .map_err(map_store_error)?;
    emit_changes(&app, &result);
    Ok(result)
}

#[tauri::command]
#[specta::specta]
pub async fn transcript_correction_update(
    app: AppHandle,
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: TranscriptCorrectionUpdateRequest,
) -> Result<TranscriptCorrectionMutationResult, MeetingCommandError> {
    let result = manager
        .store()
        .await?
        .update_transcript_correction(request)
        .map_err(map_store_error)?;
    emit_changes(&app, &result);
    Ok(result)
}

#[tauri::command]
#[specta::specta]
pub async fn transcript_correction_remove(
    app: AppHandle,
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: TranscriptCorrectionRemoveRequest,
) -> Result<TranscriptCorrectionMutationResult, MeetingCommandError> {
    let result = manager
        .store()
        .await?
        .remove_transcript_correction(request)
        .map_err(map_store_error)?;
    emit_changes(&app, &result);
    Ok(result)
}

fn emit_changes(app: &AppHandle, result: &TranscriptCorrectionMutationResult) {
    for session in &result.changed_sessions {
        let _ = app.emit(
            "meeting:transcript-changed",
            MeetingEventPayload {
                event_schema_version: 1,
                session_id: Some(session.session_id),
                revision: session.revision,
            },
        );
    }
}

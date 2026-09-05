use crate::analytics::DashboardTrendRequest;
use crate::meeting::analytics::{
    KeywordTracker, MeetingActionItemState, MeetingAnalyticsSnapshot, MeetingCatchUp,
    MeetingUserNotes,
};
use crate::meeting::clock::host_monotonic_now_ns;
use crate::meeting::detection::DetectionRuntime;
use crate::meeting::series_types::{
    MeetingSeriesAlwaysRecordSetRequest, MeetingSeriesDigestSetRequest,
    MeetingSeriesMutationResult, MeetingSeriesPreferences, MeetingSeriesRemoteOptOutSetRequest,
    MeetingSeriesRemoteRoster, MeetingSeriesTemplateSetRequest,
};
use crate::meeting::session::{
    ImportRecordingRequest, MeetingActionItemDoneRequest, MeetingConsentPanelSessionState,
    MeetingConsentPanelStartRequest, MeetingMutationRequest, MeetingMutationResult,
    MeetingNoteCreateRequest, MeetingNoteDeleteRequest, MeetingNoteUpdateRequest,
    MeetingPreflightCreateRequest, MeetingPreflightRefreshRequest, MeetingReenhanceRequest,
    MeetingRemovalResult, MeetingSegmentEditRequest, MeetingSessionManager,
    MeetingSpeakerMergeRequest, MeetingSpeakerRenameRequest, MeetingStartRequest, MeetingStopCause,
    MeetingStopSurface, MeetingTitleSetRequest, MeetingUserNotesSaveRequest,
};
use crate::meeting::suggestions::MeetingSuggestion;
use crate::meeting::types::*;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::State;

#[tauri::command]
#[specta::specta]
pub fn meeting_suggestions_list(
    manager: State<'_, Arc<MeetingSessionManager>>,
) -> Vec<MeetingSuggestion> {
    manager.suggestions_list(host_monotonic_now_ns())
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_preflight_create(
    manager: State<'_, Arc<MeetingSessionManager>>,
    detection: State<'_, Arc<DetectionRuntime>>,
    request: MeetingPreflightCreateRequest,
) -> Result<MeetingMutationResult, MeetingCommandError> {
    let calendar_event = request
        .calendar_event_key
        .as_deref()
        .map(|event_key| {
            detection
                .calendar_event_for_start(event_key)
                .ok_or(MeetingCommandError::InvalidRequest)
        })
        .transpose()?;
    manager
        .create_preflight_with_calendar(request, calendar_event)
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_preflight_refresh(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingPreflightRefreshRequest,
) -> Result<MeetingMutationResult, MeetingCommandError> {
    manager.refresh_preflight(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_preflight_cancel(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingMutationRequest,
) -> Result<OperationReceipt, MeetingCommandError> {
    manager.cancel_preflight(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_start(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingStartRequest,
) -> Result<MeetingMutationResult, MeetingCommandError> {
    manager.start(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_consent_panel_start(
    manager: State<'_, Arc<MeetingSessionManager>>,
    detection: State<'_, Arc<DetectionRuntime>>,
    request: MeetingConsentPanelStartRequest,
) -> Result<MeetingMutationResult, MeetingCommandError> {
    let context = detection
        .take_for_panel_start(&request.prompt_id)
        .ok_or(MeetingCommandError::ConsentStale)?;
    let series_consent = request
        .always_record_series
        .then(|| request.consent.clone());
    let result = manager.start_from_consent_panel(&context, request).await;
    if let Ok(started) = &result {
        if started.snapshot.phase == MeetingPhase::CapturingRecording {
            if let Some(consent) = series_consent {
                if let Err(error) = manager.grant_panel_series_consent(&context, &consent).await {
                    log::warn!(
                        "Meeting started, but standing-series consent was not saved: {error:?}"
                    );
                }
            }
            detection.track_started(&context, &started.snapshot);
            manager
                .record_prompt_recorded(context.prompt_id, started.snapshot.session_id)
                .await;
        }
    }
    result
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_consent_panel_active_state(
    manager: State<'_, Arc<MeetingSessionManager>>,
) -> Result<Option<MeetingConsentPanelSessionState>, MeetingCommandError> {
    manager.consent_panel_active_state().await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_consent_panel_forget_series(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Result<bool, MeetingCommandError> {
    manager.forget_active_series(session_id).await
}

/// Re-sizes the recording window for the disclosure note the card is about to
/// draw, or drops it again.
///
/// The panel's window is sized before the webview paints, and a refused
/// disclosure arrives after the capture started: the announcement is attempted
/// from the panel itself. So the card, which is the only place that knows the
/// row is on screen, asks for the height it needs.
#[tauri::command]
#[specta::specta]
pub fn meeting_consent_panel_fit_disclosure(app: tauri::AppHandle, note: bool) {
    crate::meeting::consent_panel::show(
        &app,
        crate::meeting::consent_panel::ConsentPanelLayout::Recording {
            disclosure_note: note,
        },
    );
}

/// Post this recording's disclosure line into the meeting's chat.
///
/// The line is the caller's because it comes from the i18next catalog. Answers
/// with what the disclosure is now, so the live surface can say quietly that the
/// target would not take it.
#[tauri::command]
#[specta::specta]
pub async fn meeting_announce_disclosure(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
    line: String,
) -> Result<MeetingSessionDisclosure, MeetingCommandError> {
    manager.announce_disclosure(session_id, line).await
}

/// The meetings a person deleted and can still get back, newest first.
#[tauri::command]
#[specta::specta]
pub async fn meeting_trash_list(
    manager: State<'_, Arc<MeetingSessionManager>>,
) -> Result<Vec<MeetingTrashEntry>, MeetingCommandError> {
    manager.trash_list().await
}

/// Undo one deletion, by the deletion's own job id.
#[tauri::command]
#[specta::specta]
pub async fn meeting_trash_restore(
    manager: State<'_, Arc<MeetingSessionManager>>,
    job_id: MeetingDeletionJobId,
) -> Result<MeetingSessionSnapshot, MeetingCommandError> {
    manager.trash_restore(job_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_pause(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingMutationRequest,
) -> Result<MeetingMutationResult, MeetingCommandError> {
    manager.pause(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_resume(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingMutationRequest,
) -> Result<MeetingMutationResult, MeetingCommandError> {
    manager.resume(request).await
}

/// Stop a capture from one of the app's own windows.
///
/// The surface is the caller's to name because a stop is the one command two
/// unrelated windows issue, and a receipt that says only "an operator press"
/// cannot tell them apart afterwards.
#[tauri::command]
#[specta::specta]
pub async fn meeting_stop(
    manager: State<'_, Arc<MeetingSessionManager>>,
    detection: State<'_, Arc<DetectionRuntime>>,
    request: MeetingMutationRequest,
    surface: MeetingStopSurface,
) -> Result<MeetingMutationResult, MeetingCommandError> {
    let session_id = request.session_id;
    let result = manager
        .stop(request, MeetingStopCause::Operator(surface))
        .await;
    if result.is_ok() {
        detection.track_ended(session_id);
    }
    result
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_discard(
    manager: State<'_, Arc<MeetingSessionManager>>,
    detection: State<'_, Arc<DetectionRuntime>>,
    request: MeetingMutationRequest,
) -> Result<MeetingRemovalResult, MeetingCommandError> {
    let session_id = request.session_id;
    let result = manager.discard(request).await;
    if result.as_ref().is_ok_and(|removed| removed.removed) {
        detection.track_ended(session_id);
    }
    result
}

/// Bring a recording in as a meeting. Returns the session, already in
/// `Processing`: the frontend shows it and watches `meeting:session-changed`,
/// exactly as it does after a stop.
#[tauri::command]
#[specta::specta]
pub async fn meeting_import_recording(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: ImportRecordingRequest,
) -> Result<MeetingSessionSnapshot, MeetingCommandError> {
    manager.import_recording(request).await
}

/// Bring another note-taker's transcript export in as a meeting.
#[tauri::command]
#[specta::specta]
pub async fn meeting_import_transcript(
    manager: State<'_, Arc<MeetingSessionManager>>,
    path: PathBuf,
) -> Result<MeetingSessionSnapshot, MeetingCommandError> {
    manager.import_transcript(path).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_recovery_list(
    manager: State<'_, Arc<MeetingSessionManager>>,
) -> Result<Vec<MeetingHistorySummary>, MeetingCommandError> {
    manager.recovery_list().await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_recovery_finalize(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingMutationRequest,
) -> Result<MeetingMutationResult, MeetingCommandError> {
    manager.recovery_finalize(request).await
}

/// One page of retained meetings. `filter` is optional and every field inside
/// it defaults to "no constraint", so a caller that sends nothing still gets
/// the whole list newest-first.
#[tauri::command]
#[specta::specta]
pub async fn meeting_list(
    manager: State<'_, Arc<MeetingSessionManager>>,
    cursor_utc_ms: Option<i64>,
    limit: Option<usize>,
    filter: Option<MeetingListFilter>,
) -> Result<PaginatedMeetings, MeetingCommandError> {
    manager
        .list(
            cursor_utc_ms,
            limit.unwrap_or(50),
            filter.unwrap_or_default(),
        )
        .await
}

/// Return a tagged meeting trend. An unavailable result is a normal storage
/// state and must not be presented as a zero-valued range.
#[tauri::command]
#[specta::specta]
pub async fn meeting_trend(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: DashboardTrendRequest,
) -> Result<MeetingTrendProjection, ()> {
    Ok(manager.trend_projection(request).await)
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_get(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Result<MeetingReviewSnapshot, MeetingCommandError> {
    manager.get(session_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_search(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingSearchRequest,
) -> Result<MeetingSearchResult, MeetingCommandError> {
    manager.search(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_title_set(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingTitleSetRequest,
) -> Result<MeetingMutationResult, MeetingCommandError> {
    manager.title_set(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_speaker_rename(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingSpeakerRenameRequest,
) -> Result<MeetingMutationResult, MeetingCommandError> {
    manager.speaker_rename(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_speaker_merge(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingSpeakerMergeRequest,
) -> Result<MeetingMutationResult, MeetingCommandError> {
    manager.speaker_merge(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_segment_edit(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingSegmentEditRequest,
) -> Result<MeetingMutationResult, MeetingCommandError> {
    manager.segment_edit(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_note_create(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingNoteCreateRequest,
) -> Result<MeetingMutationResult, MeetingCommandError> {
    manager.note_create(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_note_update(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingNoteUpdateRequest,
) -> Result<MeetingMutationResult, MeetingCommandError> {
    manager.note_update(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_note_delete(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingNoteDeleteRequest,
) -> Result<MeetingMutationResult, MeetingCommandError> {
    manager.note_delete(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_artifacts_regenerate(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingMutationRequest,
) -> Result<MeetingMutationResult, MeetingCommandError> {
    manager.artifacts_regenerate(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_question_forget(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingMutationRequest,
    question_id: MeetingQuestionId,
) -> Result<MeetingMutationResult, MeetingCommandError> {
    manager.question_forget(request, question_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_export(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingExportRequest,
) -> Result<MeetingExportResult, MeetingCommandError> {
    manager.export(request).await
}

/// Write the meeting's where-did-we-land ledger to a single self-contained
/// HTML file and answer with the path it was written to.
#[tauri::command]
#[specta::specta]
pub async fn produce_ledger_html(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Result<String, MeetingCommandError> {
    manager.produce_ledger_html(session_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_delete(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingMutationRequest,
) -> Result<MeetingRemovalResult, MeetingCommandError> {
    manager.delete(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_retention_get(
    manager: State<'_, Arc<MeetingSessionManager>>,
) -> Result<MeetingRetentionSnapshot, MeetingCommandError> {
    manager.retention_get().await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_retention_set(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingRetentionSetRequest,
) -> Result<MeetingRetentionMutationResult, MeetingCommandError> {
    manager.retention_set(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_remote_cancel(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingMutationRequest,
) -> Result<(), MeetingCommandError> {
    manager.remote_cancel(request).await
}

/// Conversation metrics, tracker hits, action-item ticks and the user's notes
/// for one meeting. Metrics are derived from the transcript on every call, so
/// the answer always matches the transcript the caller can see.
#[tauri::command]
#[specta::specta]
pub async fn get_meeting_analytics(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Result<MeetingAnalyticsSnapshot, MeetingCommandError> {
    manager.analytics_get(session_id).await
}

#[tauri::command]
#[specta::specta]
pub fn list_keyword_trackers(app: tauri::AppHandle) -> Vec<KeywordTracker> {
    crate::settings::get_settings(&app).trackers_list
}

/// Replace the tracker list. Blank names and blank patterns are dropped here
/// rather than stored, so the scan never has to defend against them.
#[tauri::command]
#[specta::specta]
pub fn save_keyword_trackers(
    app: tauri::AppHandle,
    trackers: Vec<KeywordTracker>,
) -> Vec<KeywordTracker> {
    let cleaned: Vec<KeywordTracker> = trackers
        .into_iter()
        .filter_map(|tracker| {
            let name = tracker.name.trim().to_string();
            if name.is_empty() {
                return None;
            }
            let patterns: Vec<String> = tracker
                .patterns
                .into_iter()
                .map(|pattern| pattern.trim().to_string())
                .filter(|pattern| !pattern.is_empty())
                .collect();
            Some(KeywordTracker { name, patterns })
        })
        .collect();
    // Nothing in this command's shape can carry a store failure back to the
    // trackers screen. The list returned is what the running process reads,
    // which is true whether or not the disk took it.
    let _ = crate::settings::update_settings(&app, |settings| {
        settings.trackers_list = cleaned.clone();
    });
    cleaned
}

#[tauri::command]
#[specta::specta]
pub async fn set_action_item_done(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingActionItemDoneRequest,
) -> Result<Vec<MeetingActionItemState>, MeetingCommandError> {
    manager.action_item_done_set(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn get_meeting_user_notes(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Result<MeetingUserNotes, MeetingCommandError> {
    manager.user_notes_get(session_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn save_meeting_user_notes(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingUserNotesSaveRequest,
) -> Result<MeetingUserNotes, MeetingCommandError> {
    manager.user_notes_save(request).await
}

/// Save the notes layer and rebuild the generated notes from it.
#[tauri::command]
#[specta::specta]
pub async fn reenhance_meeting_with_notes(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingReenhanceRequest,
) -> Result<MeetingMutationResult, MeetingCommandError> {
    manager.artifacts_reenhance(request).await
}

/// Recap the transcript captured so far in at most six bullets.
#[tauri::command]
#[specta::specta]
pub async fn meeting_catch_up(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Result<MeetingCatchUp, MeetingCommandError> {
    manager.catch_up(session_id).await
}

/// What one calendar series has decided — template, digest inclusion, and
/// whether it records itself — by key. The pre-meeting card and D28's Upcoming
/// rows read it this way, because a calendar event carries its series key and
/// no session exists yet.
#[tauri::command]
#[specta::specta]
pub async fn meeting_series_template_get(
    manager: State<'_, Arc<MeetingSessionManager>>,
    series_key: String,
) -> Result<MeetingSeriesPreferences, MeetingCommandError> {
    manager.series_preferences(series_key).await
}

/// The same record reached from a meeting. A `series_key` of `null` in the
/// answer means this meeting belongs to no series, which is what the review
/// screen needs in order to not offer the choice.
#[tauri::command]
#[specta::specta]
pub async fn meeting_series_template_for_session(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Result<MeetingSeriesPreferences, MeetingCommandError> {
    manager.series_preferences_for_session(session_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_series_template_set(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingSeriesTemplateSetRequest,
) -> Result<MeetingSeriesMutationResult, MeetingCommandError> {
    manager.set_series_template(request).await
}

/// D28: keep this series in the evening digest, or take it out of it.
#[tauri::command]
#[specta::specta]
pub async fn meeting_series_digest_set(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingSeriesDigestSetRequest,
) -> Result<MeetingSeriesMutationResult, MeetingCommandError> {
    manager.set_series_digest(request).await
}

/// D28: grant or revoke the standing consent that lets this series record
/// itself.
///
/// The grant is written through the same rows the consent panel writes, so an
/// occurrence auto-started from it cites a grant the start transaction can
/// revalidate. `acknowledged_sources` is the operator's acknowledgement and the
/// grant is refused without one.
#[tauri::command]
#[specta::specta]
pub async fn meeting_series_always_record_set(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingSeriesAlwaysRecordSetRequest,
) -> Result<MeetingSeriesMutationResult, MeetingCommandError> {
    manager.set_series_always_record(request).await
}

/// D14: keep this series' text on this Mac, or hand it back to the global
/// meeting-intelligence setting.
///
/// Fenced on the same series revision the other two writes carry, so the three
/// controls a surface shows together cannot overwrite each other's decisions.
#[tauri::command]
#[specta::specta]
pub async fn meeting_series_remote_opt_out_set(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingSeriesRemoteOptOutSetRequest,
) -> Result<MeetingSeriesMutationResult, MeetingCommandError> {
    manager.set_series_remote_opt_out(request).await
}

/// D14: the series the meeting-intelligence section offers a per-series switch
/// for, newest first, with the fence those switches write with.
#[tauri::command]
#[specta::specta]
pub async fn meeting_series_remote_roster(
    manager: State<'_, Arc<MeetingSessionManager>>,
) -> Result<MeetingSeriesRemoteRoster, MeetingCommandError> {
    manager.series_remote_roster().await
}

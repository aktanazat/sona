mod documents;
mod people;
pub(crate) mod voice_identity;

use super::detection::machine::CalendarEventSummary;
use super::learning::{no_inputs, AppLearningInputs};
use super::learning_types::{
    LearningDecisionRequest, LearningSuggestion, LearningSuggestionsResult,
};
use super::people_types::{PersonBriefingRow, VocabularyCandidatesResult};
use super::session::{MeetingSessionManager, MEETING_EVENT_SCHEMA_VERSION};
use super::store::{MeetingStore, StoreError};
use super::types::{
    MeetingCommandError, MeetingEventPayload, MeetingOperationId, MeetingSessionId,
};
use super::workflow_types::{
    NewWorkflowEvent, PaginatedWorkflowRuns, WorkflowDispatchResult, WorkflowEventId,
    WorkflowEventKind, WorkflowRunReceipt, WorkflowRunTrend, WorkflowRunsRequest,
    WorkflowSetEnabledRequest, WorkflowsListResult,
};
use crate::analytics::DashboardTrendRequest;
use chrono::Local;
use std::collections::BTreeSet;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

impl MeetingSessionManager {
    pub async fn vocabulary_candidates(
        &self,
    ) -> Result<VocabularyCandidatesResult, MeetingCommandError> {
        self.store()
            .await?
            .vocabulary_candidates(&self.known_vocabulary())
            .map_err(map_store_error)
    }

    pub async fn workflows_list(&self) -> Result<WorkflowsListResult, MeetingCommandError> {
        self.store()
            .await?
            .workflows_list()
            .map_err(map_store_error)
    }

    pub async fn workflow_set_enabled(
        &self,
        request: WorkflowSetEnabledRequest,
    ) -> Result<WorkflowsListResult, MeetingCommandError> {
        let result = self
            .store()
            .await?
            .set_workflow_enabled(
                request.workflow_id,
                request.enabled,
                request.expected_revision,
            )
            .map_err(map_store_error)?;
        self.emit_artifact_changed(None, result.revision);
        Ok(result)
    }

    pub async fn workflow_runs(
        &self,
        request: WorkflowRunsRequest,
    ) -> Result<PaginatedWorkflowRuns, MeetingCommandError> {
        self.store()
            .await?
            .workflow_runs(request)
            .map_err(map_store_error)
    }

    pub async fn workflow_run_trend(
        &self,
        request: DashboardTrendRequest,
    ) -> Result<WorkflowRunTrend, MeetingCommandError> {
        self.store()
            .await?
            .workflow_run_trend(request)
            .map_err(map_store_error)
    }

    /// Every pending learning suggestion, with stale ones dropped.
    pub async fn learning_suggestions(
        &self,
    ) -> Result<LearningSuggestionsResult, MeetingCommandError> {
        let store = self.store().await?;
        let app = self.app_handle().cloned();
        match AppLearningInputs::resolve(app.as_ref()) {
            Some(inputs) => store.learning_suggestions(&inputs),
            None => store.learning_suggestions(&no_inputs()),
        }
        .map_err(map_store_error)
    }

    /// The suggestion behind one candidate, for the command that is about to
    /// act on it. The command needs the payload to know what to write.
    pub(crate) async fn learning_suggestion(
        &self,
        request: &LearningDecisionRequest,
    ) -> Result<Option<LearningSuggestion>, MeetingCommandError> {
        self.store()
            .await?
            .learning_suggestion(request.loop_kind, &request.candidate_key)
            .map_err(map_store_error)
    }

    /// Records a human answer. The caller has already performed whatever
    /// settings write an acceptance implies.
    pub(crate) async fn decide_learning_suggestion(
        &self,
        request: &LearningDecisionRequest,
    ) -> Result<LearningSuggestionsResult, MeetingCommandError> {
        let result = self
            .store()
            .await?
            .decide_learning_suggestion(request, now_utc_ms())
            .map_err(map_store_error)?;
        self.emit_artifact_changed(None, result.revision);
        Ok(result)
    }

    pub(crate) async fn calendar_briefing(
        &self,
        event: CalendarEventSummary,
        now_utc_ms: i64,
    ) -> Vec<PersonBriefingRow> {
        let Ok(store) = self.store().await else {
            return Vec::new();
        };
        let dispatch = match store.record_workflow_event(NewWorkflowEvent {
            kind: WorkflowEventKind::CalendarMeetingDetected,
            payload: serde_json::json!({"event": &event}),
            occurred_at_utc_ms: now_utc_ms,
            source: "meeting_detection",
            dedupe_key: format!("calendar-meeting-detected:{}", event.event_key),
        }) {
            Ok(dispatch) => dispatch,
            Err(error) => {
                log::warn!("calendar workflow event failed: {error:?}");
                return Vec::new();
            }
        };
        let runner = spawn_workflow_runner(
            Arc::clone(&store),
            dispatch.event_id,
            !dispatch.inserted,
            self.app_handle().cloned(),
        );
        if await_workflow_runner(runner, Arc::clone(&store), self.app_handle().cloned(), None)
            .await
            .is_err()
        {
            return Vec::new();
        }
        store
            .calendar_person_context(&event)
            .map(|context| context.rows)
            .unwrap_or_default()
    }

    pub(crate) async fn remember_calendar_facts(
        &self,
        session_id: MeetingSessionId,
        event: CalendarEventSummary,
    ) -> Result<(), MeetingCommandError> {
        self.store()
            .await?
            .remember_calendar_facts(session_id, &event)
            .map_err(map_store_error)
    }

    pub(crate) fn record_meeting_started(
        &self,
        store: Arc<MeetingStore>,
        session_id: MeetingSessionId,
    ) {
        self.dispatch_contained(
            store,
            NewWorkflowEvent {
                kind: WorkflowEventKind::MeetingStarted,
                payload: serde_json::json!({"session_id": session_id.uuid().to_string()}),
                occurred_at_utc_ms: now_utc_ms(),
                source: "meeting_lifecycle",
                dedupe_key: format!("meeting-started:{}", session_id.uuid()),
            },
            Some(session_id),
        );
    }

    pub(crate) fn record_speaker_renamed(
        &self,
        store: Arc<MeetingStore>,
        session_id: MeetingSessionId,
        operation_id: MeetingOperationId,
        display_name: String,
    ) {
        self.dispatch_contained(
            store,
            NewWorkflowEvent {
                kind: WorkflowEventKind::SpeakerRenamed,
                payload: serde_json::json!({
                    "session_id": session_id.uuid().to_string(),
                    "display_name": display_name,
                }),
                occurred_at_utc_ms: now_utc_ms(),
                source: "meeting_lifecycle",
                dedupe_key: format!("speaker-renamed:{}", operation_id.uuid()),
            },
            Some(session_id),
        );
    }

    /// Tells the learning loops that dictation history has moved.
    ///
    /// Called after every dictation run receipt, and deliberately coarse: the
    /// dedupe key is one local day, so a heavy dictation day produces one event
    /// and one bounded mining pass per loop instead of thousands.
    pub(crate) async fn record_dictation_corpus_swept(&self) {
        let Ok(store) = self.store().await else {
            return;
        };
        let local_day = Local::now().format("%Y-%m-%d").to_string();
        if let Err(error) = dispatch_daily_workflow_event(
            store,
            NewWorkflowEvent {
                kind: WorkflowEventKind::DictationCorpusSwept,
                payload: serde_json::json!({"local_day": &local_day}),
                occurred_at_utc_ms: now_utc_ms(),
                source: "dictation_history",
                dedupe_key: format!("dictation-corpus-swept:{local_day}"),
            },
            self.app_handle().cloned(),
        ) {
            log::warn!("dictation corpus sweep failed: {error:?}");
        }
    }

    /// Records one human dictation correction as vocabulary evidence.
    ///
    /// The dedupe key is the rewrite on the local day it happened, which is what
    /// makes "three days running" the evidence rather than "three clicks in one
    /// afternoon".
    ///
    /// macOS-only with its caller: the destination observer is the only source
    /// of a dictation correction, so on the other platforms nothing records one.
    #[cfg(target_os = "macos")]
    pub(crate) async fn record_dictation_correction(&self, spoken: String, written: String) {
        let Ok(store) = self.store().await else {
            return;
        };
        let local_day = Local::now().format("%Y-%m-%d").to_string();
        if let Err(error) = dispatch_daily_workflow_event(
            store,
            NewWorkflowEvent {
                kind: WorkflowEventKind::DictationCorrectionRecorded,
                payload: serde_json::json!({"spoken": &spoken, "written": &written}),
                occurred_at_utc_ms: now_utc_ms(),
                source: "dictation_correction",
                dedupe_key: format!(
                    "dictation-correction:{local_day}:{}->{}",
                    spoken.trim().to_lowercase(),
                    written.trim().to_lowercase()
                ),
            },
            self.app_handle().cloned(),
        ) {
            log::warn!("dictation correction event failed: {error:?}");
        }
    }

    pub(crate) async fn record_audio_imported(&self, import_id: String, source_name: String) {
        let dedupe_key = format!("audio-imported:{import_id}");
        self.record_event(
            WorkflowEventKind::AudioImported,
            serde_json::json!({"import_id": import_id, "source_name": source_name}),
            "audio_import",
            dedupe_key,
            None,
        )
        .await;
    }

    pub(crate) async fn record_agent_hook_event(&self, request_id: String, kind: String) -> bool {
        let dedupe_key = format!("agent-hook-event:{request_id}");
        self.record_event(
            WorkflowEventKind::AgentHookEvent,
            serde_json::json!({"request_id": request_id, "kind": kind}),
            "sona_agent_hook",
            dedupe_key,
            None,
        )
        .await
    }

    pub(crate) async fn record_prompt_recorded(
        &self,
        prompt_id: String,
        session_id: MeetingSessionId,
    ) -> bool {
        self.record_event(
            WorkflowEventKind::MeetingPromptRecorded,
            serde_json::json!({"prompt_id": prompt_id, "session_id": session_id.uuid().to_string()}),
            "meeting_detection",
            format!("meeting-prompt-recorded:{prompt_id}:{}", session_id.uuid()),
            Some(session_id),
        )
        .await
    }

    pub(crate) async fn record_prompt_ignored(&self, prompt_id: String) -> bool {
        self.record_event(
            WorkflowEventKind::MeetingPromptIgnored,
            serde_json::json!({"prompt_id": prompt_id}),
            "meeting_detection",
            format!("meeting-prompt-ignored:{prompt_id}"),
            None,
        )
        .await
    }

    pub(crate) async fn record_auto_record_started(
        &self,
        occurrence_key: &str,
        session_id: MeetingSessionId,
    ) -> bool {
        self.record_event(
            WorkflowEventKind::MeetingAutoRecordStarted,
            serde_json::json!({
                "occurrence_key": occurrence_key,
                "session_id": session_id.uuid().to_string(),
            }),
            "meeting_detection",
            format!(
                "meeting-auto-record-started:{occurrence_key}:{}",
                session_id.uuid()
            ),
            Some(session_id),
        )
        .await
    }

    pub(crate) async fn record_auto_record_stopped(
        &self,
        session_id: MeetingSessionId,
        trigger: crate::meeting::detection::machine::StopTrigger,
    ) -> bool {
        self.record_event(
            WorkflowEventKind::MeetingAutoRecordStopped,
            serde_json::json!({
                "session_id": session_id.uuid().to_string(),
                "trigger": trigger,
            }),
            "meeting_detection",
            format!(
                "meeting-auto-record-stopped:{}:{}",
                session_id.uuid(),
                trigger.as_str()
            ),
            Some(session_id),
        )
        .await
    }

    /// One durable meeting-activity row for a ritual presentation or action.
    /// Replaying the same occurrence returns `false` and does not add a second
    /// run-log entry.
    pub(crate) async fn record_ritual_activity(
        &self,
        kind: WorkflowEventKind,
        ritual_id: &str,
        session_id: MeetingSessionId,
        occurrence_key: &str,
    ) -> bool {
        let Ok(store) = self.store().await else {
            return false;
        };
        match dispatch_daily_workflow_event(
            store,
            NewWorkflowEvent {
                kind,
                payload: serde_json::json!({
                    "ritual_id": ritual_id,
                    "session_id": session_id.uuid().to_string(),
                }),
                occurred_at_utc_ms: now_utc_ms(),
                source: "meeting_ritual",
                dedupe_key: format!("{}:{occurrence_key}", kind.as_str()),
            },
            self.app_handle().cloned(),
        ) {
            Ok(dispatch) => dispatch.inserted,
            Err(error) => {
                log::warn!("meeting ritual workflow event failed: {error:?}");
                false
            }
        }
    }

    async fn record_event(
        &self,
        kind: WorkflowEventKind,
        payload: serde_json::Value,
        source: &'static str,
        dedupe_key: String,
        session_id: Option<MeetingSessionId>,
    ) -> bool {
        let Ok(store) = self.store().await else {
            return false;
        };
        self.dispatch_contained(
            store,
            NewWorkflowEvent {
                kind,
                payload,
                occurred_at_utc_ms: now_utc_ms(),
                source,
                dedupe_key,
            },
            session_id,
        )
    }

    fn dispatch_contained(
        &self,
        store: Arc<MeetingStore>,
        event: NewWorkflowEvent,
        session_id: Option<MeetingSessionId>,
    ) -> bool {
        match dispatch_workflow_event(store, event, self.app_handle().cloned(), session_id) {
            Ok(_) => true,
            Err(error) => {
                log::warn!("workflow event failed: {error:?}");
                false
            }
        }
    }

    pub(crate) fn known_vocabulary(&self) -> Vec<String> {
        known_vocabulary(self.app_handle())
    }
}

pub(crate) fn record_meeting_finalized(
    store: Arc<MeetingStore>,
    app: Option<AppHandle>,
    session_id: MeetingSessionId,
    known_vocabulary: Vec<String>,
) -> Result<WorkflowDispatchResult, StoreError> {
    dispatch_workflow_event(
        store,
        NewWorkflowEvent {
            kind: WorkflowEventKind::MeetingFinalized,
            payload: serde_json::json!({
                "session_id": session_id.uuid().to_string(),
                "known_vocabulary": known_vocabulary,
            }),
            occurred_at_utc_ms: now_utc_ms(),
            source: "meeting_processing",
            dedupe_key: format!("meeting-finalized:{}", session_id.uuid()),
        },
        app,
        Some(session_id),
    )
}

/// Records an event and runs it, always.
///
/// Passing `!inserted` as `record_skips` is deliberate: a duplicate dedupe key
/// means this exact event already exists, and writing a `skipped` receipt is how
/// the run log says so.
fn dispatch_workflow_event(
    store: Arc<MeetingStore>,
    event: NewWorkflowEvent,
    app: Option<AppHandle>,
    session_id: Option<MeetingSessionId>,
) -> Result<WorkflowDispatchResult, StoreError> {
    let dispatch = store.record_workflow_event(event)?;
    let runner = spawn_workflow_runner(
        Arc::clone(&store),
        dispatch.event_id,
        !dispatch.inserted,
        app.clone(),
    );
    // Logged, not dropped, for the same reason the reconciliation scan logs it:
    // a run that fails *before* it writes a receipt — the store refused a
    // connection, the event row could not be read — leaves no `failed` row and
    // is otherwise indistinguishable from an event that never ran. The scan
    // picks the work back up at the next launch; this line is what says the
    // first attempt happened at all.
    tauri::async_runtime::spawn(async move {
        if let Err(error) = await_workflow_runner(runner, store, app, session_id).await {
            log::warn!("workflow run failed before it could write a receipt: {error:?}");
        }
    });
    Ok(dispatch)
}

/// Records a coarse, day-bucketed event and runs it only when it is new.
///
/// The learning loops are woken by one event per local day per kind. Running a
/// duplicate would write a `skipped` receipt for every dictation of the day —
/// thousands of rows saying nothing — so a duplicate is recorded and left alone.
/// A run that failed is retried by the startup reconciliation scan instead —
/// see [`WorkflowEventKind::retries_after_failure`], which is what makes that
/// true for this kind.
fn dispatch_daily_workflow_event(
    store: Arc<MeetingStore>,
    event: NewWorkflowEvent,
    app: Option<AppHandle>,
) -> Result<WorkflowDispatchResult, StoreError> {
    let dispatch = store.record_workflow_event(event)?;
    if !dispatch.inserted {
        return Ok(dispatch);
    }
    let runner = spawn_workflow_runner(Arc::clone(&store), dispatch.event_id, false, app.clone());
    tauri::async_runtime::spawn(async move {
        if let Err(error) = await_workflow_runner(runner, store, app, None).await {
            log::warn!("daily workflow run failed before it could write a receipt: {error:?}");
        }
    });
    Ok(dispatch)
}

fn spawn_workflow_runner(
    store: Arc<MeetingStore>,
    event_id: WorkflowEventId,
    record_skips: bool,
    app: Option<AppHandle>,
) -> tauri::async_runtime::JoinHandle<Result<Vec<WorkflowRunReceipt>, StoreError>> {
    tauri::async_runtime::spawn_blocking(move || match AppLearningInputs::resolve(app.as_ref()) {
        Some(inputs) => store.run_workflow_event(event_id, record_skips, &inputs),
        None => store.run_workflow_event(event_id, record_skips, &no_inputs()),
    })
}

async fn await_workflow_runner(
    runner: tauri::async_runtime::JoinHandle<Result<Vec<WorkflowRunReceipt>, StoreError>>,
    store: Arc<MeetingStore>,
    app: Option<AppHandle>,
    session_id: Option<MeetingSessionId>,
) -> Result<Vec<WorkflowRunReceipt>, StoreError> {
    let receipts = runner.await.map_err(|_| StoreError::Unavailable)??;
    if receipts.is_empty() {
        return Ok(receipts);
    }
    if let Some(app) = app {
        let revision = store.workflow_run_revision().unwrap_or(0);
        let _ = app.emit(
            "meeting:artifact-changed",
            MeetingEventPayload {
                event_schema_version: MEETING_EVENT_SCHEMA_VERSION,
                session_id,
                revision,
            },
        );
    }
    Ok(receipts)
}

pub(crate) fn resume_pending_workflow_events(store: Arc<MeetingStore>, app: Option<AppHandle>) {
    let event_ids = match store.pending_workflow_event_ids() {
        Ok(event_ids) => event_ids,
        Err(error) => {
            log::warn!("workflow reconciliation scan failed: {error:?}");
            return;
        }
    };
    for event_id in event_ids {
        let runner = spawn_workflow_runner(Arc::clone(&store), event_id, false, app.clone());
        let store = Arc::clone(&store);
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(error) = await_workflow_runner(runner, store, app, None).await {
                log::warn!("workflow reconciliation failed: {error:?}");
            }
        });
    }
}

pub(crate) fn known_vocabulary(app: Option<&tauri::AppHandle>) -> Vec<String> {
    let settings = app.map(crate::settings::get_settings).unwrap_or_default();
    settings
        .custom_words
        .iter()
        .chain(
            settings
                .modes
                .iter()
                .flat_map(|mode| mode.asr.custom_words.iter()),
        )
        .flat_map(|entry| [entry.spoken.clone(), entry.written.clone()])
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub(crate) fn map_store_error(error: StoreError) -> MeetingCommandError {
    match error {
        StoreError::NotFound | StoreError::PersonNotFound | StoreError::SpeakerNotFound => {
            MeetingCommandError::NotFound
        }
        StoreError::ConsentStale => MeetingCommandError::ConsentStale,
        StoreError::ExplicitConsentRequired => MeetingCommandError::ConsentRequired,
        StoreError::Conflict | StoreError::StaleRevision => MeetingCommandError::StaleRevision,
        StoreError::Invalid => MeetingCommandError::InvalidRequest,
        StoreError::LocalModelUnavailable => MeetingCommandError::LocalModelUnavailable,
        StoreError::LocalEvidenceUnavailable => MeetingCommandError::LocalEvidenceUnavailable,
        StoreError::InsufficientEnrollmentEvidence => {
            MeetingCommandError::InsufficientEnrollmentEvidence
        }
        StoreError::ProfileModelIncompatible => MeetingCommandError::ProfileModelIncompatible,
        StoreError::ProfileMergeResolutionRequired => {
            MeetingCommandError::ProfileMergeResolutionRequired
        }
        StoreError::EncryptionUnavailable
        | StoreError::StorageUnavailable
        | StoreError::Unavailable
        | StoreError::VoiceInvariant
        | StoreError::Io
        | StoreError::Corrupt => MeetingCommandError::StorageUnavailable,
    }
}

pub(crate) fn now_utc_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

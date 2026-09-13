use super::{
    bump_workflow_run_revision_in, calendar_facts_in, jump_target, matching_enabled_workflows_in,
    outcome_projection, stored_event_in, terminal_receipt_exists_in, StoredWorkflowEvent,
};
use crate::meeting::detection::machine::CalendarEventSummary;
use crate::meeting::document_types::DocumentId;
use crate::meeting::store::digest::digest_counts_in;
use crate::meeting::store::documents::{bump_document_revision_in, document_by_id_in};
use crate::meeting::store::learning::{
    cursor_floor_in, mine_capture_advice_in, mine_dictation_correction_in, mine_meeting_edits_in,
    mine_mode_habits_in, mine_spoken_punctuation_in, prime_series_in, DictationCorpus,
    LearningInputs,
};
use crate::meeting::store::people::{
    bump_people_revision_in, calendar_context_in, continuity_summary_in, derive_calendar_links_in,
    derive_speaker_link_in, derive_title_links_in, link_document_mentions_in,
    recompute_organizations_in, vocabulary_candidates_in,
};
use crate::meeting::store::{MeetingStore, StoreError};
use crate::meeting::types::MeetingSessionId;
use crate::meeting::workflow_types::{
    WorkflowEventId, WorkflowEventKind, WorkflowId, WorkflowRunId, WorkflowRunReceipt,
    WorkflowRunStatus,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use std::panic::{catch_unwind, AssertUnwindSafe};
use uuid::Uuid;

/// Runs every enabled workflow this event matches, each in its own transaction.
///
/// The dictation corpus lives in a second database behind a second lock, and a
/// read of it can wait out that database's unlock and busy timeouts. So the
/// page a miner will work from is resolved *before* the transaction below
/// opens, and the transaction only ever touches rows in this database. Each
/// miner still re-reads its own cursor inside the transaction and filters the
/// page against it: the cursor is a row here, and that is what keeps two
/// concurrent passes from counting the same run twice.
pub(super) fn run_event(
    store: &MeetingStore,
    event_id: WorkflowEventId,
    record_skips: bool,
    inputs: &dyn LearningInputs,
) -> Result<Vec<WorkflowRunReceipt>, StoreError> {
    let (event, workflows, floor) = {
        let connection = store.connection()?;
        let event = stored_event_in(&connection, event_id)?;
        let workflows = matching_enabled_workflows_in(&connection, event.kind)?;
        // Only the workflows that still have work: a duplicate dispatch skips
        // every one of them, and must not pay for a corpus read to do it. The
        // check inside the transaction below is the authoritative one, so a
        // concurrent pass finishing between the two costs a wasted read and
        // nothing else.
        let mut unfinished = Vec::with_capacity(workflows.len());
        for workflow_id in workflows.iter().copied() {
            if !terminal_receipt_exists_in(&connection, event.id, event.kind, workflow_id)? {
                unfinished.push(workflow_id);
            }
        }
        let floor = cursor_floor_in(&connection, &unfinished)?;
        (event, workflows, floor)
    };
    let corpus = match floor {
        Some(floor) => DictationCorpus::read(inputs, floor),
        None => DictationCorpus::default(),
    };

    let mut connection = store.connection()?;
    let mut receipts = Vec::with_capacity(workflows.len());
    for workflow_id in workflows {
        let mut transaction =
            connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(receipt) = run_workflow_in(
            &mut transaction,
            &event,
            workflow_id,
            record_skips,
            |connection, event, workflow_id| {
                execute_workflow_in(connection, event, workflow_id, inputs, &corpus)
            },
        )? {
            receipts.push(receipt);
        }
        transaction.commit()?;
    }
    Ok(receipts)
}

fn run_workflow_in<F>(
    transaction: &mut Transaction<'_>,
    event: &StoredWorkflowEvent,
    workflow_id: WorkflowId,
    record_skips: bool,
    execute: F,
) -> Result<Option<WorkflowRunReceipt>, StoreError>
where
    F: FnOnce(&Connection, &StoredWorkflowEvent, WorkflowId) -> Result<String, StoreError>,
{
    if terminal_receipt_exists_in(transaction, event.id, event.kind, workflow_id)? {
        if !record_skips {
            return Ok(None);
        }
        let started_at_utc_ms = now_utc_ms();
        let receipt = insert_receipt_in(
            transaction,
            event,
            workflow_id,
            WorkflowRunStatus::Skipped,
            "already_processed".to_string(),
            None,
            started_at_utc_ms,
            now_utc_ms(),
        )?;
        return Ok(Some(receipt));
    }

    let started_at_utc_ms = now_utc_ms();
    let savepoint = transaction.savepoint()?;
    let outcome = catch_unwind(AssertUnwindSafe(|| execute(&savepoint, event, workflow_id)));
    let (status, summary, error) = match outcome {
        Ok(Ok(summary)) => {
            savepoint.commit()?;
            (WorkflowRunStatus::Ok, summary, None)
        }
        Ok(Err(error)) => {
            drop(savepoint);
            (
                WorkflowRunStatus::Failed,
                "failed".to_string(),
                Some(error_code(error).to_string()),
            )
        }
        Err(_) => {
            drop(savepoint);
            (
                WorkflowRunStatus::Failed,
                "failed".to_string(),
                Some("workflow_panicked".to_string()),
            )
        }
    };
    let receipt = insert_receipt_in(
        transaction,
        event,
        workflow_id,
        status,
        summary,
        error,
        started_at_utc_ms,
        now_utc_ms(),
    )?;
    Ok(Some(receipt))
}

fn execute_workflow_in(
    connection: &Connection,
    event: &StoredWorkflowEvent,
    workflow_id: WorkflowId,
    inputs: &dyn LearningInputs,
    corpus: &DictationCorpus,
) -> Result<String, StoreError> {
    match workflow_id {
        WorkflowId::PersonLinking => run_person_linking_in(connection, event),
        WorkflowId::PreMeetingBriefing => run_briefing_in(connection, event),
        WorkflowId::Continuity => run_continuity_in(connection, event),
        WorkflowId::VocabularyMining => run_vocabulary_in(connection, event),
        WorkflowId::DocumentLinking => run_document_linking_in(connection, event),
        WorkflowId::SpokenPunctuation => {
            let added =
                mine_spoken_punctuation_in(connection, inputs, corpus, event.occurred_at_utc_ms)?;
            Ok(format!("spoken_punctuation:suggestions={added}"))
        }
        WorkflowId::CorrectionLearning => run_correction_learning_in(connection, event, inputs),
        WorkflowId::ModeHabits => {
            let added = mine_mode_habits_in(connection, inputs, corpus, event.occurred_at_utc_ms)?;
            Ok(format!("mode_habits:suggestions={added}"))
        }
        WorkflowId::CaptureAdvisor => {
            let added = mine_capture_advice_in(connection, corpus, event.occurred_at_utc_ms)?;
            Ok(format!("capture_advice:suggestions={added}"))
        }
        WorkflowId::SeriesPriming => {
            let session_id =
                MeetingSessionId::from_uuid(payload_uuid(&event.payload, "session_id")?);
            let terms = prime_series_in(connection, session_id, event.occurred_at_utc_ms)?;
            Ok(format!("series_priming:terms={terms}"))
        }
        WorkflowId::MeetingActivity => run_meeting_activity_in(event),
        WorkflowId::DailyDigest => run_daily_digest_in(connection, event),
    }
}

/// Loop 2, from whichever human-authored source woke it.
///
/// A finalized meeting brings its own review edits; a dictation correction
/// brings the one rewrite a person just performed. Neither path reads the
/// dictation corpus, which is what keeps model-versus-model retry diffs out of
/// vocabulary evidence entirely.
fn run_correction_learning_in(
    connection: &Connection,
    event: &StoredWorkflowEvent,
    inputs: &dyn LearningInputs,
) -> Result<String, StoreError> {
    let added = match event.kind {
        WorkflowEventKind::MeetingFinalized => {
            let session_id =
                MeetingSessionId::from_uuid(payload_uuid(&event.payload, "session_id")?);
            mine_meeting_edits_in(connection, inputs, session_id, event.occurred_at_utc_ms)?
        }
        WorkflowEventKind::DictationCorrectionRecorded => {
            let spoken = payload_str(&event.payload, "spoken")?;
            let written = payload_str(&event.payload, "written")?;
            mine_dictation_correction_in(
                connection,
                inputs,
                spoken,
                written,
                event.occurred_at_utc_ms,
                event.occurred_at_utc_ms,
            )?
        }
        _ => return Err(StoreError::Invalid),
    };
    Ok(format!("correction_learning:suggestions={added}"))
}

fn run_person_linking_in(
    connection: &Connection,
    event: &StoredWorkflowEvent,
) -> Result<String, StoreError> {
    let session_id = payload_uuid(&event.payload, "session_id")?;
    let session_id = MeetingSessionId::from_uuid(session_id);
    let mut changes = match event.kind {
        WorkflowEventKind::MeetingStarted => {
            let Some(calendar) = calendar_facts_in(connection, session_id)? else {
                return Ok("person_links:changes=0".to_string());
            };
            derive_calendar_links_in(
                connection,
                session_id,
                &calendar.attendees,
                event.occurred_at_utc_ms,
            )?
        }
        WorkflowEventKind::SpeakerRenamed => {
            let display_name = event
                .payload
                .get("display_name")
                .and_then(serde_json::Value::as_str)
                .ok_or(StoreError::Invalid)?;
            derive_speaker_link_in(
                connection,
                session_id,
                display_name,
                event.occurred_at_utc_ms,
            )?
        }
        WorkflowEventKind::MeetingFinalized => {
            let title: String = connection
                .query_row(
                    "SELECT title FROM meeting_sessions WHERE id = ?1",
                    [session_id.uuid().to_string()],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(StoreError::NotFound)?;
            derive_title_links_in(connection, session_id, &title, event.occurred_at_utc_ms)?
        }
        _ => return Err(StoreError::Invalid),
    };
    changes += recompute_organizations_in(connection)?;
    if changes != 0 {
        bump_people_revision_in(connection)?;
    }
    Ok(format!("person_links:changes={changes}"))
}

fn run_briefing_in(
    connection: &Connection,
    event: &StoredWorkflowEvent,
) -> Result<String, StoreError> {
    let calendar = event
        .payload
        .get("event")
        .cloned()
        .ok_or(StoreError::Invalid)?;
    let calendar: CalendarEventSummary =
        serde_json::from_value(calendar).map_err(|_| StoreError::Invalid)?;
    let context = calendar_context_in(connection, &calendar.attendees)?;
    Ok(format!("briefing:persons={}", context.rows.len()))
}

fn run_continuity_in(
    connection: &Connection,
    event: &StoredWorkflowEvent,
) -> Result<String, StoreError> {
    let session_id = MeetingSessionId::from_uuid(payload_uuid(&event.payload, "session_id")?);
    let (series, carried) = continuity_summary_in(connection, session_id)?;
    Ok(format!("continuity:series={series},carried={carried}"))
}

fn run_vocabulary_in(
    connection: &Connection,
    event: &StoredWorkflowEvent,
) -> Result<String, StoreError> {
    let known = event
        .payload
        .get("known_vocabulary")
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let candidates = vocabulary_candidates_in(connection, &known)?;
    Ok(format!("vocabulary:candidates={}", candidates.len()))
}

fn run_document_linking_in(
    connection: &Connection,
    event: &StoredWorkflowEvent,
) -> Result<String, StoreError> {
    let document_id = DocumentId(payload_uuid(&event.payload, "document_id")?);
    let document = document_by_id_in(connection, document_id)?;
    let links = link_document_mentions_in(
        connection,
        document_id,
        &document.content,
        event.occurred_at_utc_ms,
    )?;
    if links != 0 {
        bump_people_revision_in(connection)?;
        bump_document_revision_in(connection)?;
    }
    Ok(format!("document_links:persons={links}"))
}

/// The one workflow that only narrates: the event kind *is* the outcome.
///
/// The projection reads the kind rather than this string, so the summary is
/// free to be what every other workflow's is — one `name:field=value` line —
/// and the value comes from the kind's own `as_str` instead of a second table
/// of the same words.
fn run_meeting_activity_in(event: &StoredWorkflowEvent) -> Result<String, StoreError> {
    match event.kind {
        WorkflowEventKind::MeetingPromptRecorded
        | WorkflowEventKind::MeetingPromptIgnored
        | WorkflowEventKind::MeetingAutoRecordStarted
        | WorkflowEventKind::MeetingAutoRecordStopped
        | WorkflowEventKind::MeetingPrepPresented
        | WorkflowEventKind::MeetingPrepRecordArmed
        | WorkflowEventKind::MeetingPrepBriefOpened
        | WorkflowEventKind::MeetingPrepDismissed
        | WorkflowEventKind::MeetingWrapPresented
        | WorkflowEventKind::MeetingWrapNotesOpened
        | WorkflowEventKind::MeetingWrapFollowUpCopied
        | WorkflowEventKind::MeetingWrapDone => {
            Ok(format!("meeting_activity:decision={}", event.kind.as_str()))
        }
        _ => Err(StoreError::Invalid),
    }
}

/// D20. Counts the day the event names and writes those counts into its
/// summary, which is the whole run: the digest changes nothing.
///
/// The window arrives in the payload rather than being derived here. Local
/// midnight is DST arithmetic, the scheduler already did it to decide the event
/// was due, and doing it twice is how the two answers drift apart. It also
/// makes this run replayable: the same event always counts the same day.
fn run_daily_digest_in(
    connection: &Connection,
    event: &StoredWorkflowEvent,
) -> Result<String, StoreError> {
    let day_start_utc_ms = payload_i64(&event.payload, "day_start_utc_ms")?;
    let day_end_utc_ms = payload_i64(&event.payload, "day_end_utc_ms")?;
    if day_end_utc_ms <= day_start_utc_ms {
        return Err(StoreError::Invalid);
    }
    let counts = digest_counts_in(connection, day_start_utc_ms, day_end_utc_ms)?;
    Ok(format!(
        "daily_digest:meetings={},loops_closed={},suggestions_waiting={},waiting_on_stale={}",
        counts.meetings, counts.loops_closed, counts.suggestions_waiting, counts.waiting_on_stale
    ))
}

fn insert_receipt_in(
    connection: &Connection,
    event: &StoredWorkflowEvent,
    workflow_id: WorkflowId,
    status: WorkflowRunStatus,
    outcome_summary: String,
    error: Option<String>,
    started_at_utc_ms: i64,
    finished_at_utc_ms: i64,
) -> Result<WorkflowRunReceipt, StoreError> {
    let run_id = WorkflowRunId::new();
    let status_db = match status {
        WorkflowRunStatus::Ok => "ok",
        WorkflowRunStatus::Failed => "failed",
        WorkflowRunStatus::Skipped => "skipped",
    };
    let (outcome_code, outcome_counts) =
        outcome_projection(workflow_id, event.kind, status, &outcome_summary);
    connection.execute(
        "INSERT INTO workflow_runs (
            id, workflow_id, event_id, status, started_at_utc_ms,
            finished_at_utc_ms, outcome_summary, error
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            run_id.uuid().to_string(),
            workflow_id.as_str(),
            event.id.uuid().to_string(),
            status_db,
            started_at_utc_ms,
            finished_at_utc_ms,
            outcome_summary,
            error
        ],
    )?;
    bump_workflow_run_revision_in(connection)?;
    Ok(WorkflowRunReceipt {
        id: run_id,
        workflow_id,
        event_kind: event.kind,
        jump_target: jump_target(event.kind, &event.payload),
        status,
        started_at_utc_ms,
        finished_at_utc_ms,
        outcome_summary,
        outcome_code,
        outcome_counts,
        error,
    })
}

fn now_utc_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn payload_uuid(payload: &serde_json::Value, key: &str) -> Result<Uuid, StoreError> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or(StoreError::Invalid)
        .and_then(|value| Uuid::parse_str(value).map_err(|_| StoreError::Invalid))
}

fn payload_str<'a>(payload: &'a serde_json::Value, key: &str) -> Result<&'a str, StoreError> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or(StoreError::Invalid)
}

fn payload_i64(payload: &serde_json::Value, key: &str) -> Result<i64, StoreError> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_i64)
        .ok_or(StoreError::Invalid)
}

fn error_code(error: StoreError) -> &'static str {
    match error {
        StoreError::NotFound => "not_found",
        StoreError::Conflict => "conflict",
        StoreError::Invalid => "invalid_event_payload",
        StoreError::EncryptionUnavailable => "encryption_unavailable",
        StoreError::StorageUnavailable => "storage_unavailable",
        StoreError::Unavailable => "store_unavailable",
        StoreError::Corrupt => "store_corrupt",
        StoreError::Io => "io_error",
        StoreError::ConsentStale => "consent_stale",
        StoreError::ExplicitConsentRequired => "explicit_consent_required",
        StoreError::StaleRevision => "stale_revision",
        StoreError::PersonNotFound => "person_not_found",
        StoreError::SpeakerNotFound => "speaker_not_found",
        StoreError::LocalEvidenceUnavailable => "local_evidence_unavailable",
        StoreError::LocalModelUnavailable => "local_model_unavailable",
        StoreError::InsufficientEnrollmentEvidence => "insufficient_enrollment_evidence",
        StoreError::ProfileModelIncompatible => "profile_model_incompatible",
        StoreError::ProfileMergeResolutionRequired => "profile_merge_resolution_required",
        StoreError::VoiceInvariant => "voice_invariant",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::store::workflow_core_tests::{event, store};

    #[test]
    fn panic_rolls_back_work_and_commits_failed_receipt() {
        let (_directory, store) = store();
        let dispatch = store
            .record_workflow_event(event(
                WorkflowEventKind::MeetingStarted,
                serde_json::json!({"session_id": MeetingSessionId::new().uuid().to_string()}),
                "panic-savepoint",
            ))
            .unwrap();
        let mut connection = store.connection().unwrap();
        let event = stored_event_in(&connection, dispatch.event_id).unwrap();
        let mut transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let receipt = run_workflow_in(
            &mut transaction,
            &event,
            WorkflowId::PersonLinking,
            false,
            |connection, _, _| -> Result<String, StoreError> {
                connection.execute(
                    "UPDATE workflow_state SET revision = 999 WHERE singleton = 1",
                    [],
                )?;
                panic!("workflow panic");
            },
        )
        .unwrap()
        .unwrap();
        transaction.commit().unwrap();
        drop(connection);

        assert_eq!(receipt.status, WorkflowRunStatus::Failed);
        assert_eq!(receipt.error.as_deref(), Some("workflow_panicked"));
        assert_eq!(store.workflows_list().unwrap().revision, 0);
        assert_eq!(
            store
                .workflow_runs(Default::default())
                .unwrap()
                .entries
                .len(),
            1
        );
    }

    /// A failed run of the daily sweep is not the last word, and a failed run
    /// of anything else is.
    ///
    /// The sweep is the one event no later signal re-raises: every dictation of
    /// the same local day collapses into its dedupe key, so counting a failure
    /// as terminal silences all three of its loops until tomorrow. The startup
    /// reconciliation scan is what tries again, which is what
    /// `dispatch_daily_workflow_event` has always claimed.
    #[test]
    fn a_failed_sweep_stays_unfinished_while_other_failures_are_final() {
        let (_directory, store) = store();
        let sweep = store
            .record_workflow_event(event(
                WorkflowEventKind::DictationCorpusSwept,
                serde_json::json!({"local_day": "2026-08-31"}),
                "sweep-retry",
            ))
            .unwrap();
        let finalized = store
            .record_workflow_event(event(
                WorkflowEventKind::MeetingFinalized,
                serde_json::json!({"session_id": MeetingSessionId::new().uuid().to_string()}),
                "finalized-no-retry",
            ))
            .unwrap();

        // Both events match several workflows, and the scan asks about all of
        // them, so every one has to have been attempted before "is this event
        // still pending?" says anything about failure being terminal.
        for dispatch in [&sweep, &finalized] {
            for workflow_id in matching_workflows(&store, dispatch.event_id) {
                let receipt = run_once(&store, dispatch.event_id, workflow_id, |_, _, _| {
                    Err(StoreError::Invalid)
                });
                assert_eq!(receipt.status, WorkflowRunStatus::Failed);
            }
        }

        let connection = store.connection().unwrap();
        let sweep_event = stored_event_in(&connection, sweep.event_id).unwrap();
        assert!(
            !terminal_receipt_exists_in(
                &connection,
                sweep_event.id,
                sweep_event.kind,
                WorkflowId::SpokenPunctuation
            )
            .unwrap(),
            "a failed sweep counts as finished, so nothing will retry it"
        );
        let finalized_event = stored_event_in(&connection, finalized.event_id).unwrap();
        assert!(
            terminal_receipt_exists_in(
                &connection,
                finalized_event.id,
                finalized_event.kind,
                WorkflowId::PersonLinking
            )
            .unwrap(),
            "an event its own next occurrence re-raises was queued for a retry"
        );
        drop(connection);

        let pending = store.pending_workflow_event_ids().unwrap();
        assert!(
            pending.contains(&sweep.event_id),
            "the startup reconciliation scan is not offered the failed sweep"
        );
        assert!(
            !pending.contains(&finalized.event_id),
            "an event whose every workflow failed terminally came back to the scan"
        );

        // The retries succeed, and the once-only index accepts each receipt
        // beside the failure it replaces.
        for workflow_id in matching_workflows(&store, sweep.event_id) {
            let retried = run_once(&store, sweep.event_id, workflow_id, |_, _, _| {
                Ok(format!("{}:suggestions=0", workflow_id.as_str()))
            });
            assert_eq!(retried.status, WorkflowRunStatus::Ok);
        }
        assert!(
            !store
                .pending_workflow_event_ids()
                .unwrap()
                .contains(&sweep.event_id),
            "a sweep that finally succeeded is still pending"
        );
    }

    /// A failed digest run is not the last word either, but the thing that
    /// tries again is the digest's own clock inside its day, never the
    /// startup scan: yesterday's digest written over breakfast is worse than
    /// none. So the event runs again when asked, and the scan is not offered
    /// it.
    #[test]
    fn a_failed_digest_runs_again_when_asked_but_not_at_launch() {
        let (_directory, store) = store();
        let digest = store
            .record_workflow_event(event(
                WorkflowEventKind::DailyDigestDue,
                serde_json::json!({
                    "local_day": "2026-08-31",
                    "day_start_utc_ms": 1_000,
                    "day_end_utc_ms": 2_000,
                }),
                "daily-digest:2026-08-31",
            ))
            .unwrap();
        let failed = run_once(
            &store,
            digest.event_id,
            WorkflowId::DailyDigest,
            |_, _, _| Err(StoreError::Unavailable),
        );
        assert_eq!(failed.status, WorkflowRunStatus::Failed);
        assert!(
            !store
                .pending_workflow_event_ids()
                .unwrap()
                .contains(&digest.event_id),
            "the launch scan was offered a digest, which it must never write"
        );

        let retried = store
            .run_workflow_event(
                digest.event_id,
                false,
                &crate::meeting::learning::no_inputs(),
            )
            .unwrap();
        assert_eq!(retried.len(), 1, "the failed digest counted as finished");
        assert_eq!(retried[0].status, WorkflowRunStatus::Ok);

        // Once it has succeeded there is nothing left to run, which is what
        // keeps the next tick of the evening from posting twice.
        assert!(store
            .run_workflow_event(
                digest.event_id,
                false,
                &crate::meeting::learning::no_inputs()
            )
            .unwrap()
            .is_empty());
    }

    /// A detection stop is readable through the events page, and the thing that
    /// identifies it is the summary rather than the workflow name.
    ///
    /// Four agents' conclusions about the meeting self-stop rested on this and
    /// it had to be re-derived from three files each time, because the shape is
    /// counter-intuitive: twelve event kinds all run as `meeting_activity`, so
    /// the `action` a reader sees is the same for all of them and the kind
    /// arrives in the outcome summary. Reading `action` alone makes every one
    /// of the twelve look absent from a store that holds them.
    #[test]
    fn a_detection_stop_reaches_the_events_page_named_in_its_summary() {
        let (_directory, store) = store();
        let session_id = MeetingSessionId::new();
        let dispatch = store
            .record_and_run_workflow_event(
                event(
                    WorkflowEventKind::MeetingAutoRecordStopped,
                    serde_json::json!({
                        "session_id": session_id.uuid().to_string(),
                        "trigger": "call_ended",
                    }),
                    "auto-record-stopped:projection",
                ),
                &crate::meeting::learning::no_inputs(),
            )
            .unwrap();
        assert!(dispatch.inserted);

        let rows = store.query_events(None, 10).unwrap();
        let events = rows
            .into_iter()
            .map(crate::query::event_from_row)
            .collect::<Vec<_>>();

        let run = events
            .iter()
            .find(|event| event.action == "meeting_activity")
            .expect("a permanently-enabled workflow ran and the page shows it");
        assert_eq!(
            run.detail, "meeting_activity:decision=meeting_auto_record_stopped",
            "the event kind is the discriminator and it lives in detail, not action"
        );
    }

    /// Every workflow the scan will ask this event about.
    fn matching_workflows(store: &MeetingStore, event_id: WorkflowEventId) -> Vec<WorkflowId> {
        let connection = store.connection().unwrap();
        let event = stored_event_in(&connection, event_id).unwrap();
        matching_enabled_workflows_in(&connection, event.kind).unwrap()
    }

    /// One workflow, one transaction, whatever the caller's closure decides.
    fn run_once<F>(
        store: &MeetingStore,
        event_id: WorkflowEventId,
        workflow_id: WorkflowId,
        execute: F,
    ) -> WorkflowRunReceipt
    where
        F: FnOnce(&Connection, &StoredWorkflowEvent, WorkflowId) -> Result<String, StoreError>,
    {
        let mut connection = store.connection().unwrap();
        let event = stored_event_in(&connection, event_id).unwrap();
        let mut transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let receipt = run_workflow_in(&mut transaction, &event, workflow_id, false, execute)
            .unwrap()
            .unwrap();
        transaction.commit().unwrap();
        receipt
    }
}

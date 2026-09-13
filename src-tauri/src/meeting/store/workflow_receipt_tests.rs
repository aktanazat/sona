use super::workflow_core_tests::{event, inputs, meeting, person, store, transcript};
use crate::analytics::{DashboardTrendRange, DashboardTrendRequest};
use crate::meeting::detection::machine::{
    CalendarAttendee, CalendarEventSummary, ParticipationStatus,
};
use crate::meeting::types::{DeletionCause, MeetingOperationId};
use crate::meeting::workflow_types::{
    WorkflowEventKind, WorkflowId, WorkflowOutcomeCode, WorkflowRunStatus,
};
use chrono::{Local, TimeZone};
use rusqlite::params;
use uuid::Uuid;

#[test]
fn workflow_rerun_records_skips_without_reapplying_work() {
    let (_directory, store) = store();
    let meeting_id = meeting(&store, "1:1 with Alice", 1);
    person(&store, "Alice Doe", &[], &[]);
    let dispatch = store
        .record_and_run_workflow_event(
            event(
                WorkflowEventKind::MeetingFinalized,
                serde_json::json!({
                    "session_id": meeting_id.uuid().to_string(),
                    "known_vocabulary": []
                }),
                "finalized-1",
            ),
            &inputs(),
        )
        .unwrap();
    assert!(dispatch.inserted);
    assert_eq!(dispatch.receipts.len(), 4);
    assert!(dispatch.receipts.iter().all(|receipt| {
        receipt.started_at_utc_ms > 0
            && receipt.finished_at_utc_ms >= receipt.started_at_utc_ms
            && !matches!(
                receipt.outcome_code,
                WorkflowOutcomeCode::AlreadyProcessed
                    | WorkflowOutcomeCode::Failed
                    | WorkflowOutcomeCode::Skipped
            )
    }));
    let rerun = store
        .rerun_workflow_event(dispatch.event_id, &inputs())
        .unwrap();
    assert_eq!(rerun.len(), 4);
    assert!(rerun
        .iter()
        .all(|receipt| receipt.status == WorkflowRunStatus::Skipped));
    assert!(rerun.iter().all(|receipt| {
        receipt.outcome_code == WorkflowOutcomeCode::AlreadyProcessed
            && receipt.outcome_counts.changes == 0
            && receipt.outcome_counts.persons == 0
            && receipt.outcome_counts.series == 0
            && receipt.outcome_counts.carried == 0
            && receipt.outcome_counts.candidates == 0
    }));
}

#[test]
fn malformed_event_is_contained_in_failed_receipts() {
    let (_directory, store) = store();
    let dispatch = store
        .record_and_run_workflow_event(
            event(
                WorkflowEventKind::MeetingFinalized,
                serde_json::json!({}),
                "malformed-1",
            ),
            &inputs(),
        )
        .unwrap();
    assert!(dispatch
        .receipts
        .iter()
        .any(|receipt| receipt.status == WorkflowRunStatus::Failed));
    assert!(dispatch
        .receipts
        .iter()
        .filter(|receipt| receipt.status == WorkflowRunStatus::Failed)
        .all(|receipt| receipt.error.as_deref() == Some("invalid_event_payload")));
}

#[test]
fn vocabulary_requires_repetition_across_meetings_and_excludes_known_terms() {
    let (_directory, store) = store();
    let first = meeting(&store, "First", 1);
    let second = meeting(&store, "Second", 2);
    transcript(&store, first, "North Star shipped. North Star worked.");
    transcript(&store, second, "North Star returned.");
    store
        .record_and_run_workflow_event(
            event(
                WorkflowEventKind::MeetingFinalized,
                serde_json::json!({
                    "session_id": second.uuid().to_string(),
                    "known_vocabulary": []
                }),
                "vocabulary-finalized",
            ),
            &inputs(),
        )
        .unwrap();
    let candidates = store.vocabulary_candidates(&[]).unwrap();
    assert!(candidates.entries.iter().any(|candidate| {
        candidate.text == "North Star"
            && candidate.occurrences == 3
            && candidate.meetings_count == 2
    }));
    assert!(store
        .vocabulary_candidates(&["North Star".to_string()])
        .unwrap()
        .entries
        .is_empty());
}

#[test]
fn document_workflow_links_exact_alias_mentions() {
    let (_directory, store) = store();
    let person_id = person(&store, "Alice Doe", &["Alice Jones"], &[]);
    let document = store
        .ingest_document(
            MeetingOperationId::new(),
            "Plan".to_string(),
            "plan.md".to_string(),
            "text/markdown".to_string(),
            "Alice Jones owns the rollout.".to_string(),
            10,
        )
        .unwrap()
        .document
        .unwrap();
    let dispatch = store
        .record_and_run_workflow_event(
            event(
                WorkflowEventKind::DocumentIngested,
                serde_json::json!({"document_id": document.summary.id.uuid().to_string()}),
                "doc-1",
            ),
            &inputs(),
        )
        .unwrap();
    assert_eq!(dispatch.receipts.len(), 1);
    assert_eq!(dispatch.receipts[0].status, WorkflowRunStatus::Ok);
    let documents = store.documents_list(Some(person_id)).unwrap();
    assert_eq!(documents.entries.len(), 1);
    assert_eq!(
        documents.entries[0].content,
        "Alice Jones owns the rollout."
    );
}

#[test]
fn disabled_document_linking_leaves_its_ingest_unlinked() {
    let (_directory, store) = store();
    let person_id = person(&store, "Alice Doe", &["Alice Jones"], &[]);
    let document = store
        .ingest_document(
            MeetingOperationId::new(),
            "Plan".to_string(),
            "plan.md".to_string(),
            "text/markdown".to_string(),
            "Alice Jones owns the rollout.".to_string(),
            10,
        )
        .unwrap()
        .document
        .unwrap();
    let revision = store.workflows_list().unwrap().revision;
    store
        .set_workflow_enabled(WorkflowId::DocumentLinking, false, revision)
        .unwrap();

    let dispatch = store
        .record_and_run_workflow_event(
            event(
                WorkflowEventKind::DocumentIngested,
                serde_json::json!({"document_id": document.summary.id.uuid().to_string()}),
                "disabled-doc-1",
            ),
            &inputs(),
        )
        .unwrap();

    assert!(dispatch.inserted);
    assert!(dispatch.receipts.is_empty());
    assert!(store
        .documents_list(Some(person_id))
        .unwrap()
        .entries
        .is_empty());
}

#[test]
fn finalization_does_not_confirm_default_speaker_labels_as_people() {
    let (_directory, store) = store();
    let meeting_id = meeting(&store, "Ordinary meeting", 1);
    transcript(&store, meeting_id, "A short transcript.");

    store
        .record_and_run_workflow_event(
            event(
                WorkflowEventKind::MeetingFinalized,
                serde_json::json!({
                    "session_id": meeting_id.uuid().to_string(),
                    "known_vocabulary": []
                }),
                "default-speaker-finalized",
            ),
            &inputs(),
        )
        .unwrap();

    assert!(store
        .people_list()
        .unwrap()
        .entries
        .iter()
        .all(|entry| entry.person.display_name != "Speaker 1"));
}

#[test]
fn speaker_rename_requires_a_unique_exact_identity_match() {
    let (_directory, store) = store();
    let meeting_id = meeting(&store, "Ambiguous rename", 1);
    person(&store, "Alex Kim", &[], &["alex.one@example.com"]);
    person(&store, "Alex Kim", &[], &["alex.two@example.com"]);

    store
        .record_and_run_workflow_event(
            event(
                WorkflowEventKind::SpeakerRenamed,
                serde_json::json!({
                    "session_id": meeting_id.uuid().to_string(),
                    "display_name": "Alex Kim"
                }),
                "ambiguous-speaker-rename",
            ),
            &inputs(),
        )
        .unwrap();

    let linked: i64 = store
        .connection()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM meeting_person_links WHERE meeting_id = ?1",
            [meeting_id.uuid().to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(linked, 0);
}

#[test]
fn duplicate_event_resumes_missing_terminal_receipts() {
    let (_directory, store) = store();
    let meeting_id = meeting(&store, "Stranded event", 1);
    let pending = event(
        WorkflowEventKind::MeetingFinalized,
        serde_json::json!({
            "session_id": meeting_id.uuid().to_string(),
            "known_vocabulary": []
        }),
        "stranded-finalized",
    );
    store
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO workflow_events (
                id, kind, payload_json, occurred_at_utc_ms, source, dedupe_key
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                Uuid::new_v4().to_string(),
                pending.kind.as_str(),
                pending.payload.to_string(),
                pending.occurred_at_utc_ms,
                pending.source,
                pending.dedupe_key
            ],
        )
        .unwrap();

    let dispatch = store
        .record_and_run_workflow_event(pending, &inputs())
        .unwrap();

    assert!(!dispatch.inserted);
    assert_eq!(dispatch.receipts.len(), 4);
    assert!(dispatch
        .receipts
        .iter()
        .all(|receipt| receipt.status == WorkflowRunStatus::Ok));
}

#[test]
fn disabled_vocabulary_workflow_hides_candidates() {
    let (_directory, store) = store();
    let first = meeting(&store, "First", 1);
    let second = meeting(&store, "Second", 2);
    transcript(&store, first, "North Star shipped. North Star worked.");
    transcript(&store, second, "North Star returned.");
    let revision = store.workflows_list().unwrap().revision;
    store
        .set_workflow_enabled(WorkflowId::VocabularyMining, false, revision)
        .unwrap();

    assert!(store.vocabulary_candidates(&[]).unwrap().entries.is_empty());
}

#[test]
fn workflow_receipts_do_not_advance_settings_revision() {
    let (_directory, store) = store();
    let meeting_id = meeting(&store, "Revision split", 1);
    let before = store.workflows_list().unwrap().revision;

    store
        .record_and_run_workflow_event(
            event(
                WorkflowEventKind::MeetingFinalized,
                serde_json::json!({
                    "session_id": meeting_id.uuid().to_string(),
                    "known_vocabulary": []
                }),
                "revision-split-finalized",
            ),
            &inputs(),
        )
        .unwrap();

    assert_eq!(store.workflows_list().unwrap().revision, before);
}

#[test]
fn finished_meeting_deletion_bumps_people_revision_for_cascaded_links() {
    let (_directory, store) = store();
    let meeting_id = meeting(&store, "Cascade revision", 1);
    let person_id = person(&store, "Alice Doe", &[], &[]);
    store
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO meeting_person_links (
                meeting_id, person_id, source, confidence, created_at_utc_ms
             ) VALUES (?1, ?2, 'manual', 'confirmed', 1)",
            params![meeting_id.uuid().to_string(), person_id.uuid().to_string()],
        )
        .unwrap();
    let before = store.people_list().unwrap().revision;
    let (_, job_id) = store
        .reserve_deletion(
            MeetingOperationId::new(),
            10,
            meeting_id,
            0,
            DeletionCause::User,
        )
        .unwrap();

    store.finish_deletion(job_id).unwrap();

    assert_eq!(store.people_list().unwrap().revision, before + 1);
}

#[test]
fn document_ingest_operation_id_returns_the_original_result() {
    let (_directory, store) = store();
    let operation_id = MeetingOperationId::new();
    let first = store
        .ingest_document(
            operation_id,
            "Plan".to_string(),
            "plan.md".to_string(),
            "text/markdown".to_string(),
            "Original content".to_string(),
            10,
        )
        .unwrap();
    let duplicate = store
        .ingest_document(
            operation_id,
            "Changed".to_string(),
            "changed.md".to_string(),
            "text/plain".to_string(),
            "Changed content".to_string(),
            20,
        )
        .unwrap();

    assert_eq!(duplicate, first);
    let documents = store.documents_list(None).unwrap();
    assert_eq!(documents.revision, first.revision);
    assert_eq!(documents.entries.len(), 1);
    assert_eq!(documents.entries[0].content, "Original content");
}

#[test]
fn calendar_briefing_requires_its_successful_enabled_receipt() {
    let (_directory, store) = store();
    let person_id = person(&store, "Alice Doe", &[], &["alice@example.com"]);
    let calendar = CalendarEventSummary {
        event_key: "briefing-event".to_string(),
        series_key: "briefing-series".to_string(),
        title: "Planning".to_string(),
        attendee_count: 2,
        start_utc_ms: 100,
        end_utc_ms: 200,
        attendees: vec![CalendarAttendee {
            name: "Alice Doe".to_string(),
            status: ParticipationStatus::Accepted,
            email: Some("alice@example.com".to_string()),
            is_self: false,
        }],
        notes: None,
        calendar_name: None,
        url: None,
    };
    let dispatch = store
        .record_workflow_event(event(
            WorkflowEventKind::CalendarMeetingDetected,
            serde_json::json!({"event": &calendar}),
            "calendar-briefing-gate",
        ))
        .unwrap();
    assert!(store
        .calendar_person_context(&calendar)
        .unwrap()
        .rows
        .is_empty());

    store
        .run_workflow_event(dispatch.event_id, false, &inputs())
        .unwrap();
    let rows = store.calendar_person_context(&calendar).unwrap().rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].person_id, person_id);

    let revision = store.workflows_list().unwrap().revision;
    store
        .set_workflow_enabled(WorkflowId::PreMeetingBriefing, false, revision)
        .unwrap();
    assert!(store
        .calendar_person_context(&calendar)
        .unwrap()
        .rows
        .is_empty());
}

/// The week is every run the store holds, cut at local midnight on both
/// ends: a run at the first midnight of the window is in, one a millisecond
/// earlier is out, and tomorrow's midnight is out however late today runs.
#[test]
fn workflow_run_trend_counts_every_run_inside_local_day_boundaries() {
    let (_directory, store) = store();
    let now = Local.with_ymd_and_hms(2026, 9, 3, 12, 0, 0).unwrap();
    let window_start = Local.with_ymd_and_hms(2026, 8, 28, 0, 0, 0).unwrap();
    let tomorrow = Local.with_ymd_and_hms(2026, 9, 4, 0, 0, 0).unwrap();
    let mid_week = Local.with_ymd_and_hms(2026, 8, 31, 9, 30, 0).unwrap();
    let starts = [
        window_start.timestamp_millis(),
        window_start.timestamp_millis() - 1,
        mid_week.timestamp_millis(),
        mid_week.timestamp_millis() + 60_000,
        tomorrow.timestamp_millis() - 1,
        tomorrow.timestamp_millis(),
    ];
    // One run per workflow per event is the table's rule, so each run rides
    // its own event.
    for (index, started_at_utc_ms) in starts.into_iter().enumerate() {
        let dispatch = store
            .record_workflow_event(event(
                WorkflowEventKind::MeetingFinalized,
                serde_json::json!({"session_id": Uuid::new_v4().to_string()}),
                &format!("trend-event-{index}"),
            ))
            .unwrap();
        store
            .connection()
            .unwrap()
            .execute(
                "INSERT INTO workflow_runs (
                    id, workflow_id, event_id, status, started_at_utc_ms,
                    finished_at_utc_ms, outcome_summary, error
                 ) VALUES (?1, 'person_linking', ?2, 'ok', ?3, ?3, 'person_links:changes=0', NULL)",
                params![
                    Uuid::new_v4().to_string(),
                    dispatch.event_id.uuid().to_string(),
                    started_at_utc_ms
                ],
            )
            .unwrap();
    }

    let trend = store
        .workflow_run_trend_at(
            now,
            DashboardTrendRequest {
                range: DashboardTrendRange::Days7,
            },
        )
        .unwrap();

    assert_eq!(trend.range_start_local_date, "2026-08-28");
    assert_eq!(trend.range_end_local_date, "2026-09-03");
    assert_eq!(trend.total, 4);
    assert_eq!(
        trend
            .points
            .iter()
            .map(|point| (point.local_date.as_str(), point.runs))
            .collect::<Vec<_>>(),
        vec![
            ("2026-08-28", 1),
            ("2026-08-29", 0),
            ("2026-08-30", 0),
            ("2026-08-31", 2),
            ("2026-09-01", 0),
            ("2026-09-02", 0),
            ("2026-09-03", 1),
        ]
    );
}

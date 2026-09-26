//! Per-series preferences and D20's day counts, at the store boundary.

use super::workflow_core_tests::{event, inputs, meeting, store};
use super::*;
use crate::meeting::analytics::MeetingNotesTemplate;
use crate::meeting::automation_types::{MeetingAutomationKind, MeetingSeriesAutomationSetRequest};
use crate::meeting::detection::machine::CalendarEventSummary;
use crate::meeting::series_types::{
    MeetingSeriesAlwaysRecordSetRequest, MeetingSeriesDigestSetRequest,
    MeetingSeriesMutationResult, MeetingSeriesRemoteOptOutSetRequest,
    MeetingSeriesTemplateSetRequest,
};
use crate::meeting::types::{
    MeetingCommandKind, MeetingOperationId, MeetingReasonCode, MeetingSessionId, OperationActor,
    OperationResult, SourceKind,
};
use crate::meeting::workflow_types::{WorkflowEventKind, WorkflowId, WorkflowRunStatus};
use rusqlite::params;

/// The calendar facts a finished meeting leaves behind, written through the
/// call the app itself makes: the only place a series key or a series' name is
/// ever recorded, so the roster and every preference read hang off it.
pub(super) fn calendar_facts(
    store: &MeetingStore,
    session_id: MeetingSessionId,
    series_key: &str,
    title: &str,
) {
    store
        .remember_calendar_facts(
            session_id,
            &CalendarEventSummary {
                event_key: format!("{series_key}#{}", session_id.uuid()),
                series_key: series_key.to_string(),
                title: title.to_string(),
                attendee_count: 2,
                start_utc_ms: 0,
                end_utc_ms: 0,
                attendees: Vec::new(),
                notes: None,
                calendar_name: None,
                url: None,
            },
        )
        .unwrap();
}

fn set(
    store: &MeetingStore,
    series_key: &str,
    template: Option<MeetingNotesTemplate>,
    expected_revision: u64,
) -> MeetingSeriesMutationResult {
    store
        .set_series_template(
            &MeetingSeriesTemplateSetRequest {
                operation_id: MeetingOperationId::new(),
                series_key: series_key.to_string(),
                template,
                custom_template_id: None,
                expected_revision,
            },
            1_000,
        )
        .unwrap()
}

fn set_digest(
    store: &MeetingStore,
    series_key: &str,
    digest_included: bool,
    expected_revision: u64,
) -> MeetingSeriesMutationResult {
    store
        .set_series_digest(
            &MeetingSeriesDigestSetRequest {
                operation_id: MeetingOperationId::new(),
                series_key: series_key.to_string(),
                digest_included,
                expected_revision,
            },
            1_000,
        )
        .unwrap()
}

fn set_always_record(
    store: &MeetingStore,
    series_key: &str,
    always_record: bool,
    sources: &[SourceKind],
    expected_revision: u64,
) -> Result<MeetingSeriesMutationResult, StoreError> {
    store.set_series_always_record(
        &MeetingSeriesAlwaysRecordSetRequest {
            operation_id: MeetingOperationId::new(),
            series_key: series_key.to_string(),
            always_record,
            policy_version: 1,
            acknowledged_sources: sources.to_vec(),
            expected_revision,
        },
        1_000,
    )
}

/// The one preference the meeting-intelligence roster attaches to a row.
fn exclude(store: &MeetingStore, series_key: &str) {
    store
        .set_series_remote_opt_out(
            &MeetingSeriesRemoteOptOutSetRequest {
                operation_id: MeetingOperationId::new(),
                series_key: series_key.to_string(),
                remote_intelligence_opt_out: true,
                expected_revision: store.series_revision().unwrap(),
            },
            1_000,
        )
        .unwrap();
}

/// The one the automations roster attaches, on the other screen.
fn remind(store: &MeetingStore, series_key: &str) {
    store
        .set_series_automation(
            &MeetingSeriesAutomationSetRequest {
                operation_id: MeetingOperationId::new(),
                series_key: series_key.to_string(),
                kind: MeetingAutomationKind::Reminders,
                enabled: true,
                target: None,
                expected_revision: store.series_automations(series_key).unwrap().revision,
            },
            1_000,
        )
        .unwrap();
}

#[test]
fn a_series_nobody_has_chosen_for_reports_no_template() {
    let (_directory, store) = store();
    let preferences = store.series_preferences("weekly-sync").unwrap();

    assert_eq!(preferences.series_key.as_deref(), Some("weekly-sync"));
    assert_eq!(preferences.template, None);
    assert!(preferences.digest_included, "no row means in the digest");
    assert!(!preferences.always_record);
    assert_eq!(preferences.revision, 0);
}

#[test]
fn choosing_a_template_is_receipted_and_names_the_series_it_touched() {
    let (_directory, store) = store();
    let result = set(
        &store,
        "weekly-sync",
        Some(MeetingNotesTemplate::Standup),
        0,
    );

    assert_eq!(result.receipt.result, OperationResult::Committed);
    assert_eq!(
        result.receipt.command,
        MeetingCommandKind::SeriesTemplateSet
    );
    assert_eq!(result.receipt.actor, OperationActor::User);
    assert_eq!(result.receipt.effect_ids, vec!["weekly-sync".to_string()]);
    assert_eq!(result.receipt.new_revision, Some(1));
    assert_eq!(
        result.preferences.template,
        Some(MeetingNotesTemplate::Standup)
    );
    assert_eq!(result.preferences.revision, 1);
    assert_eq!(
        store.series_preferences("weekly-sync").unwrap().template,
        Some(MeetingNotesTemplate::Standup)
    );
}

#[test]
fn a_write_against_a_revision_that_moved_is_rejected_and_changes_nothing() {
    let (_directory, store) = store();
    set(
        &store,
        "weekly-sync",
        Some(MeetingNotesTemplate::Standup),
        0,
    );

    let stale = set(
        &store,
        "weekly-sync",
        Some(MeetingNotesTemplate::Interview),
        0,
    );

    assert_eq!(stale.receipt.result, OperationResult::Rejected);
    assert_eq!(
        stale.receipt.reason_codes,
        vec![MeetingReasonCode::StaleRevision]
    );
    assert_eq!(
        store.series_preferences("weekly-sync").unwrap().template,
        Some(MeetingNotesTemplate::Standup)
    );
}

#[test]
fn a_replayed_operation_returns_the_receipt_it_already_wrote() {
    let (_directory, store) = store();
    let operation_id = MeetingOperationId::new();
    let request = MeetingSeriesTemplateSetRequest {
        operation_id,
        series_key: "weekly-sync".to_string(),
        template: Some(MeetingNotesTemplate::OneOnOne),
        custom_template_id: None,
        expected_revision: 0,
    };

    let first = store.set_series_template(&request, 1_000).unwrap();
    let replay = store.set_series_template(&request, 2_000).unwrap();

    assert_eq!(first.receipt, replay.receipt);
    // A replay that re-ran the write would have bumped the counter again.
    assert_eq!(replay.preferences.revision, 1);
}

#[test]
fn clearing_a_preference_hands_the_series_back_to_the_default() {
    let (_directory, store) = store();
    let chosen = set(
        &store,
        "weekly-sync",
        Some(MeetingNotesTemplate::SalesCall),
        0,
    );

    let cleared = set(&store, "weekly-sync", None, chosen.preferences.revision);

    assert_eq!(cleared.receipt.result, OperationResult::Committed);
    assert_eq!(cleared.preferences.template, None);
}

#[test]
fn a_meeting_with_no_calendar_event_belongs_to_no_series() {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Local notes", 1);

    let preferences = store.series_preferences_for_session(session_id).unwrap();

    assert_eq!(preferences.series_key, None);
    assert_eq!(preferences.template, None);
}

/// The middle rung of D21's precedence: what artifact generation is handed when
/// the meeting itself has chosen nothing.
#[test]
fn a_meeting_resolves_the_template_of_the_series_it_belongs_to() {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Weekly sync", 1);
    calendar_facts(&store, session_id, "weekly-sync", "Weekly sync");
    set(
        &store,
        "weekly-sync",
        Some(MeetingNotesTemplate::Standup),
        0,
    );

    let preferences = store.series_preferences_for_session(session_id).unwrap();

    assert_eq!(preferences.series_key.as_deref(), Some("weekly-sync"));
    assert_eq!(preferences.template, Some(MeetingNotesTemplate::Standup));
}

/// The top rung: a template saved on this meeting's own notes outranks its
/// series, and a meeting with no notes takes whatever it was handed — which is
/// the series' choice by the time generation calls this.
#[test]
fn a_meetings_own_notes_template_outranks_the_one_it_was_handed() {
    let (_directory, store) = store();
    let untouched = meeting(&store, "Weekly sync", 1);
    let edited = meeting(&store, "Weekly sync", 2);
    store
        .save_user_notes(edited, "ship it", MeetingNotesTemplate::Interview, None, 0)
        .unwrap();

    assert_eq!(
        store
            .user_notes(untouched, MeetingNotesTemplate::Standup.into())
            .unwrap()
            .template,
        MeetingNotesTemplate::Standup
    );
    assert_eq!(
        store
            .user_notes(edited, MeetingNotesTemplate::Standup.into())
            .unwrap()
            .template,
        MeetingNotesTemplate::Interview
    );
}

/* D28. The three decisions share one row and one fence, so a write to any of
 * them has to leave the other two exactly where they were. This is the join
 * D28's Upcoming rows read; a digest write that silently cleared a template
 * would show the operator a choice they never unmade. */
#[test]
fn each_series_decision_is_written_without_disturbing_the_others() {
    let (_directory, store) = store();
    let chosen = set(
        &store,
        "weekly-sync",
        Some(MeetingNotesTemplate::Standup),
        0,
    );
    let excluded = set_digest(&store, "weekly-sync", false, chosen.preferences.revision);
    let granted = set_always_record(
        &store,
        "weekly-sync",
        true,
        &[SourceKind::Microphone],
        excluded.preferences.revision,
    )
    .unwrap();

    assert_eq!(granted.receipt.result, OperationResult::Committed);
    assert_eq!(
        granted.receipt.command,
        MeetingCommandKind::SeriesAlwaysRecordSet
    );
    let preferences = store.series_preferences("weekly-sync").unwrap();
    assert_eq!(preferences.template, Some(MeetingNotesTemplate::Standup));
    assert!(!preferences.digest_included);
    assert!(preferences.always_record);
    assert_eq!(preferences.revision, 3, "each write moved the one fence");
}

/* Always-record is the standing grant, so turning it on has to produce the
 * same row the consent panel produces — the one an auto-started occurrence
 * revalidates against — and turning it off has to revoke it. */
#[test]
fn always_record_grants_and_revokes_the_standing_series_consent() {
    let (_directory, store) = store();
    let granted = set_always_record(
        &store,
        "weekly-sync",
        true,
        &[SourceKind::Microphone, SourceKind::SystemAudio],
        0,
    )
    .unwrap();

    let live = store
        .live_series_consent("weekly-sync")
        .unwrap()
        .expect("the toggle grants a live standing consent");
    assert_eq!(live.policy_version, 1);
    assert_eq!(
        live.acknowledged_sources,
        vec![SourceKind::Microphone, SourceKind::SystemAudio]
    );

    set_always_record(
        &store,
        "weekly-sync",
        false,
        &[],
        granted.preferences.revision,
    )
    .unwrap();

    assert!(store.live_series_consent("weekly-sync").unwrap().is_none());
    assert!(
        !store
            .series_preferences("weekly-sync")
            .unwrap()
            .always_record
    );
}

/* A grant naming no source is permission to record "something", which is not
 * permission. The primitive refuses it and the toggle inherits that refusal
 * rather than inventing a default set. */
#[test]
fn always_record_refuses_a_grant_that_acknowledges_nothing() {
    let (_directory, store) = store();

    assert_eq!(
        set_always_record(&store, "weekly-sync", true, &[], 0),
        Err(StoreError::Invalid)
    );
    assert!(store.live_series_consent("weekly-sync").unwrap().is_none());
}

/* Clearing every choice removes the row, so "has a row" keeps meaning "has
 * decided something" — which is what makes the digest's COALESCE honest. */
#[test]
fn a_series_that_has_unmade_every_choice_keeps_no_row() {
    let (_directory, store) = store();
    let excluded = set_digest(&store, "weekly-sync", false, 0);
    let included = set_digest(&store, "weekly-sync", true, excluded.preferences.revision);

    assert!(included.preferences.digest_included);
    let rows: i64 = store
        .connection()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM meeting_series_preferences WHERE series_key = ?1",
            params!["weekly-sync"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0);
}

/* One read for a whole week of calendar rows. Keys with nothing stored still
 * come back, at their defaults, so the caller never has to decide what an
 * absent key means. */
#[test]
fn a_bulk_read_answers_for_every_key_including_the_ones_with_no_row() {
    let (_directory, store) = store();
    set(
        &store,
        "weekly-sync",
        Some(MeetingNotesTemplate::Standup),
        0,
    );

    let records = store
        .series_preferences_many(&[
            "weekly-sync".to_string(),
            "never-touched".to_string(),
            "weekly-sync".to_string(),
            "   ".to_string(),
        ])
        .unwrap();

    assert_eq!(records.len(), 2, "blank keys are not series");
    assert_eq!(
        records["weekly-sync"].template,
        Some(MeetingNotesTemplate::Standup)
    );
    assert_eq!(records["never-touched"].template, None);
    assert!(records["never-touched"].digest_included);
    assert_eq!(
        records["never-touched"].revision,
        records["weekly-sync"].revision
    );
}

/// The columns both series-listing surfaces show, in the order they show them.
type RosterRows = Vec<(String, String, i64, u32)>;

fn remote_rows(store: &MeetingStore) -> RosterRows {
    store
        .series_remote_roster()
        .unwrap()
        .rows
        .into_iter()
        .map(|row| {
            (
                row.series_key,
                row.title,
                row.last_met_at_utc_ms,
                row.meetings,
            )
        })
        .collect()
}

fn automation_rows(store: &MeetingStore) -> RosterRows {
    store
        .automation_roster()
        .unwrap()
        .series
        .into_iter()
        .map(|row| {
            (
                row.series_key,
                row.title,
                row.last_met_at_utc_ms,
                row.meeting_count,
            )
        })
        .collect()
}

/// A meeting on its way out under a retention rule.
fn deleting(store: &MeetingStore, session_id: MeetingSessionId) {
    store
        .connection()
        .unwrap()
        .execute(
            "UPDATE meeting_sessions SET phase = 'deleting' WHERE id = ?1",
            params![session_id.uuid().to_string()],
        )
        .unwrap();
}

/// Two settings screens list series out of one corpus: Meeting Intelligence
/// offers an exclusion per row, Automations offers a webhook. They may attach
/// different things to a row. They may not disagree about which series there
/// are, what each is called, when it last met, or how often — which is what
/// having one roster behind both is for.
#[test]
fn both_series_surfaces_list_the_same_series() {
    let (_directory, store) = store();
    let older = meeting(&store, "Weekly sync", 1_000);
    calendar_facts(&store, older, "weekly-sync", "Weekly sync");
    let newer = meeting(&store, "Weekly sync", 5_000);
    calendar_facts(&store, newer, "weekly-sync", "Weekly sync (new room)");
    let board = meeting(&store, "Board", 3_000);
    calendar_facts(&store, board, "board", "Board");
    // Newest of all, and on its way out: it counts for neither surface.
    let dropped = meeting(&store, "Board", 9_000);
    calendar_facts(&store, dropped, "board", "Board (last one)");
    deleting(&store, dropped);
    // No calendar event at all, so no series to list it under.
    meeting(&store, "Local notes", 7_000);
    exclude(&store, "weekly-sync");
    remind(&store, "board");

    let expected: RosterRows = vec![
        (
            "weekly-sync".to_string(),
            "Weekly sync (new room)".to_string(),
            5_000,
            2,
        ),
        ("board".to_string(), "Board".to_string(), 3_000, 1),
    ];
    assert_eq!(remote_rows(&store), expected);
    assert_eq!(
        automation_rows(&store),
        expected,
        "one roster, listed by two surfaces"
    );

    // Each surface still carries only what it owns.
    let remote = store.series_remote_roster().unwrap();
    assert!(remote.rows[0].remote_intelligence_opt_out);
    assert!(!remote.rows[1].remote_intelligence_opt_out);
    let automations = store.automation_roster().unwrap();
    assert!(automations.series[0].automations.is_empty());
    assert_eq!(automations.series[1].automations.len(), 1);
}

/// An occurrence that arrives without a name does not blank the row: the title
/// is the most recent name the series actually had, which is the one the
/// operator would recognise.
#[test]
fn a_nameless_occurrence_does_not_take_the_series_name_away() {
    let (_directory, store) = store();
    let named = meeting(&store, "Board", 1_000);
    calendar_facts(&store, named, "board", "Board sync");
    let nameless = meeting(&store, "Board", 2_000);
    calendar_facts(&store, nameless, "board", "");

    assert_eq!(
        remote_rows(&store),
        vec![("board".to_string(), "Board sync".to_string(), 2_000, 2)]
    );
    assert_eq!(automation_rows(&store), remote_rows(&store));
}

/// A settings list is a place to make a decision, not an archive. The bound
/// belongs to the roster rather than to a surface, so neither screen can ask
/// for a longer list than the other.
#[test]
fn both_surfaces_stop_at_the_same_two_dozen_series() {
    let (_directory, store) = store();
    let overflowing = super::series::SERIES_ROSTER_LIMIT + 3;
    for index in 0..overflowing {
        let session_id = meeting(&store, "Sync", 1_000 + index as i64);
        calendar_facts(&store, session_id, &format!("series-{index:02}"), "Sync");
    }

    let rows = remote_rows(&store);
    assert_eq!(rows.len(), super::series::SERIES_ROSTER_LIMIT);
    assert_eq!(automation_rows(&store), rows);
    assert_eq!(
        rows[0].0,
        format!("series-{:02}", overflowing - 1),
        "newest first, so the oldest series are the ones that fall off"
    );
}

/* D28's preference has to actually reach the evening sentence, or it is a
 * switch that does nothing. A meeting in an excluded series is not counted;
 * one with no calendar event at all still is. */
#[test]
fn the_digest_skips_meetings_whose_series_was_taken_out_of_it() {
    let (_directory, store) = store();
    let excluded = meeting(&store, "Weekly sync", 1_100);
    calendar_facts(&store, excluded, "weekly-sync", "Weekly sync");
    let kept = meeting(&store, "Design review", 1_200);
    calendar_facts(&store, kept, "design-review", "Design review");
    meeting(&store, "Local notes", 1_300);
    set_digest(&store, "weekly-sync", false, 0);

    assert_eq!(store.digest_counts(1_000, 2_000).unwrap().meetings, 2);
}

#[test]
fn the_digest_counts_meetings_that_started_inside_the_day() {
    let (_directory, store) = store();
    meeting(&store, "Yesterday", 500);
    meeting(&store, "Morning", 1_500);
    meeting(&store, "Afternoon", 1_900);
    meeting(&store, "Tomorrow", 2_500);

    let counts = store.digest_counts(1_000, 2_000).unwrap();

    assert_eq!(counts.meetings, 2);
}

#[test]
fn a_preflight_that_never_started_is_not_a_meeting_the_digest_counts() {
    let (_directory, store) = store();
    let abandoned = MeetingSessionId::new();
    store
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO meeting_sessions (
                id, phase, revision, title, origin_kind, preflight_json,
                created_at_utc_ms, started_at_utc_ms, processing_status,
                retention_policy_json
             ) VALUES (?1, 'preflight', 0, 'Never started', 'manual', '{}', 1500, NULL,
                       'pending', 'forever')",
            params![abandoned.uuid().to_string()],
        )
        .unwrap();

    assert_eq!(store.digest_counts(1_000, 2_000).unwrap().meetings, 0);
}

#[test]
fn only_committed_loop_resolutions_count_as_loops_closed() {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Weekly sync", 1_100);
    let receipts = [
        (
            MeetingCommandKind::LoopResolve,
            OperationResult::Committed,
            1_100,
        ),
        (
            MeetingCommandKind::LoopResolve,
            OperationResult::Committed,
            1_200,
        ),
        // Fenced out: it changed nothing, so it closed nothing.
        (
            MeetingCommandKind::LoopResolve,
            OperationResult::Rejected,
            1_300,
        ),
        // Assigning an owner is not a closure.
        (
            MeetingCommandKind::LoopAssign,
            OperationResult::Committed,
            1_400,
        ),
        // Yesterday's closure belongs to yesterday's digest.
        (
            MeetingCommandKind::LoopResolve,
            OperationResult::Committed,
            500,
        ),
    ];
    let connection = store.connection().unwrap();
    for (command, result, at) in receipts {
        let operation_id = MeetingOperationId::new();
        let receipt = OperationReceipt {
            schema_version: STORE_SCHEMA_VERSION,
            operation_id,
            session_id: Some(session_id),
            actor: OperationActor::User,
            command,
            expected_revision: 0,
            from_phase: None,
            to_phase: None,
            requested_at_utc_ms: at,
            committed_at_utc_ms: Some(at),
            result,
            reason_codes: Vec::new(),
            new_revision: Some(1),
            effect_ids: Vec::new(),
        };
        connection
            .execute(
                "INSERT INTO meeting_operation_receipts (
                    operation_id, session_id, receipt_json, created_at_utc_ms
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![
                    operation_id.uuid().to_string(),
                    session_id.uuid().to_string(),
                    serde_json::to_string(&receipt).unwrap(),
                    at
                ],
            )
            .unwrap();
    }
    drop(connection);

    assert_eq!(store.digest_counts(1_000, 2_000).unwrap().loops_closed, 2);
}

#[test]
fn suggestions_waiting_is_the_whole_pending_queue_not_just_todays() {
    let (_directory, store) = store();
    let connection = store.connection().unwrap();
    for (candidate_key, generated_at) in [("comma", 500_i64), ("period", 1_500)] {
        connection
            .execute(
                "INSERT INTO learning_suggestions (
                    loop_kind, candidate_key, suggestion_json, evidence_json,
                    generated_at_utc_ms
                 ) VALUES ('spoken_punctuation', ?1, '{}', '{}', ?2)",
                params![candidate_key, generated_at],
            )
            .unwrap();
    }
    drop(connection);

    assert_eq!(
        store
            .digest_counts(1_000, 2_000)
            .unwrap()
            .suggestions_waiting,
        2
    );
}

/// The dedupe key is one local day, which is what makes "one evening
/// notification" true across restarts rather than only within one process.
#[test]
fn one_digest_event_and_one_run_per_local_day() {
    let (_directory, store) = store();
    meeting(&store, "Standup", 1_100);
    let payload = serde_json::json!({
        "local_day": "2026-08-31",
        "day_start_utc_ms": 1_000,
        "day_end_utc_ms": 2_000,
    });

    let first = store
        .record_workflow_event(event(
            WorkflowEventKind::DailyDigestDue,
            payload.clone(),
            "daily-digest:2026-08-31",
        ))
        .unwrap();
    let second = store
        .record_workflow_event(event(
            WorkflowEventKind::DailyDigestDue,
            payload.clone(),
            "daily-digest:2026-08-31",
        ))
        .unwrap();

    assert!(first.inserted);
    assert!(!second.inserted);
    assert_eq!(first.event_id, second.event_id);

    let receipts = store
        .run_workflow_event(first.event_id, false, &inputs())
        .unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].workflow_id, WorkflowId::DailyDigest);
    assert_eq!(receipts[0].status, WorkflowRunStatus::Ok);
    assert_eq!(receipts[0].outcome_counts.meetings, 1);

    // The second dispatch of the same day runs nothing, so no second receipt.
    let rerun = store
        .run_workflow_event(second.event_id, false, &inputs())
        .unwrap();
    assert!(rerun.is_empty());

    // A different day is a different digest.
    let tomorrow = store
        .record_workflow_event(event(
            WorkflowEventKind::DailyDigestDue,
            payload,
            "daily-digest:2026-09-01",
        ))
        .unwrap();
    assert!(tomorrow.inserted);
}

#[test]
fn an_empty_day_records_a_zero_digest_receipt() {
    let (_directory, store) = store();
    let dispatch = store
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

    let receipts = store
        .run_workflow_event(dispatch.event_id, false, &inputs())
        .unwrap();

    assert_eq!(receipts.len(), 1);
    let receipt = &receipts[0];
    assert_eq!(receipt.workflow_id, WorkflowId::DailyDigest);
    assert_eq!(receipt.status, WorkflowRunStatus::Ok);
    assert_eq!(receipt.outcome_counts.meetings, 0);
    assert_eq!(receipt.outcome_counts.loops_closed, 0);
    assert_eq!(receipt.outcome_counts.suggestions_waiting, 0);
    assert_eq!(receipt.outcome_counts.waiting_on_stale, 0);
}

/// A digest run reports the day and writes nothing, so its counts have to
/// survive the round trip through the summary string the receipt stores.
#[test]
fn the_digest_receipt_carries_the_three_counts_it_measured() {
    let (_directory, store) = store();
    meeting(&store, "Standup", 1_100);
    meeting(&store, "Review", 1_200);
    store
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO learning_suggestions (
                loop_kind, candidate_key, suggestion_json, evidence_json,
                generated_at_utc_ms
             ) VALUES ('spoken_punctuation', 'comma', '{}', '{}', 1500)",
            [],
        )
        .unwrap();
    let dispatch = store
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

    let receipts = store
        .run_workflow_event(dispatch.event_id, false, &inputs())
        .unwrap();

    let counts = &receipts[0].outcome_counts;
    assert_eq!(counts.meetings, 2);
    assert_eq!(counts.loops_closed, 0);
    assert_eq!(counts.suggestions_waiting, 1);
}

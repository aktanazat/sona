//! D22 at the store boundary: the preference, its receipt and its fence; the
//! run log, its once-per-artifact-revision gate and its outcomes.

use super::workflow_core_tests::{meeting, store};
use super::*;
use crate::meeting::automation_types::{
    MeetingAutomationFailure, MeetingAutomationKind, MeetingAutomationRunState,
    MeetingSeriesAutomation, MeetingSeriesAutomationMutationResult,
    MeetingSeriesAutomationSetRequest,
};
use crate::meeting::detection::machine::CalendarEventSummary;
use crate::meeting::types::{
    MeetingArtifactId, MeetingCommandKind, MeetingOperationId, MeetingReasonCode, OperationActor,
    OperationResult,
};

const NOW: i64 = 1_700_000_000_000;
const SERIES_KEY: &str = "weekly-pricing";
const TAILNET_URL: &str = "http://100.99.192.40:8650/hooks/meeting";

fn series_event(at_utc_ms: i64, title: &str) -> CalendarEventSummary {
    CalendarEventSummary {
        event_key: format!("{SERIES_KEY}#{at_utc_ms}"),
        series_key: SERIES_KEY.to_string(),
        title: title.to_string(),
        attendee_count: 2,
        start_utc_ms: at_utc_ms,
        end_utc_ms: at_utc_ms + 1_800_000,
        attendees: Vec::new(),
        notes: None,
        calendar_name: None,
        location: None,
        url: None,
        place: None,
    }
}

fn set(
    store: &MeetingStore,
    kind: MeetingAutomationKind,
    enabled: bool,
    target: Option<&str>,
    expected_revision: u64,
) -> Result<MeetingSeriesAutomationMutationResult, StoreError> {
    store.set_series_automation(
        &MeetingSeriesAutomationSetRequest {
            operation_id: MeetingOperationId::new(),
            series_key: SERIES_KEY.to_string(),
            kind,
            enabled,
            target: target.map(str::to_string),
            expected_revision,
        },
        NOW,
    )
}

fn webhook(target: &str) -> MeetingSeriesAutomation {
    MeetingSeriesAutomation {
        kind: MeetingAutomationKind::Webhook,
        enabled: true,
        target: Some(target.to_string()),
    }
}

#[test]
fn a_series_nobody_has_configured_reports_nothing_on() {
    let (_directory, store) = store();
    let snapshot = store.series_automations(SERIES_KEY).unwrap();

    assert_eq!(snapshot.series_key.as_deref(), Some(SERIES_KEY));
    assert!(snapshot.automations.is_empty());
    assert!(snapshot.runnable().is_empty());
    assert_eq!(snapshot.revision, 0);
}

#[test]
fn a_meeting_with_no_calendar_event_has_no_series_and_no_automations() {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Local notes", NOW);

    let snapshot = store.series_automations_for_session(session_id).unwrap();

    assert_eq!(snapshot.series_key, None);
    assert!(snapshot.automations.is_empty());
}

#[test]
fn turning_one_on_is_receipted_and_names_the_series_and_the_kind() {
    let (_directory, store) = store();

    let result = set(
        &store,
        MeetingAutomationKind::Webhook,
        true,
        Some(TAILNET_URL),
        0,
    )
    .unwrap();

    assert_eq!(result.receipt.result, OperationResult::Committed);
    assert_eq!(
        result.receipt.command,
        MeetingCommandKind::SeriesAutomationSet
    );
    assert_eq!(result.receipt.actor, OperationActor::User);
    assert_eq!(
        result.receipt.effect_ids,
        vec![SERIES_KEY.to_string(), "webhook".to_string()]
    );
    assert_eq!(result.receipt.new_revision, Some(1));
    assert_eq!(result.snapshot.revision, 1);
    assert_eq!(
        result.snapshot.get(MeetingAutomationKind::Webhook),
        Some(&webhook(TAILNET_URL))
    );
}

#[test]
fn a_write_against_a_revision_that_moved_is_rejected_and_changes_nothing() {
    let (_directory, store) = store();
    set(
        &store,
        MeetingAutomationKind::Webhook,
        true,
        Some(TAILNET_URL),
        0,
    )
    .unwrap();

    let stale = set(
        &store,
        MeetingAutomationKind::Webhook,
        true,
        Some("http://127.0.0.1:9000/other"),
        0,
    )
    .unwrap();

    assert_eq!(stale.receipt.result, OperationResult::Rejected);
    assert_eq!(
        stale.receipt.reason_codes,
        vec![MeetingReasonCode::StaleRevision]
    );
    assert_eq!(
        stale.snapshot.get(MeetingAutomationKind::Webhook),
        Some(&webhook(TAILNET_URL)),
        "a fenced-out write must not have moved the target"
    );
}

#[test]
fn a_replayed_operation_returns_the_receipt_it_already_wrote() {
    let (_directory, store) = store();
    let operation_id = MeetingOperationId::new();
    let request = MeetingSeriesAutomationSetRequest {
        operation_id,
        series_key: SERIES_KEY.to_string(),
        kind: MeetingAutomationKind::Shortcut,
        enabled: true,
        target: Some("File the meeting".to_string()),
        expected_revision: 0,
    };

    let first = store.set_series_automation(&request, NOW).unwrap();
    let replayed = store.set_series_automation(&request, NOW + 5).unwrap();

    assert_eq!(first.receipt, replayed.receipt);
    assert_eq!(
        store.series_automations(SERIES_KEY).unwrap().revision,
        1,
        "a replay must not bump the revision a second time"
    );
}

#[test]
fn switching_off_remembers_the_target_and_forgetting_removes_it() {
    let (_directory, store) = store();
    set(
        &store,
        MeetingAutomationKind::Webhook,
        true,
        Some(TAILNET_URL),
        0,
    )
    .unwrap();

    let switched_off = set(
        &store,
        MeetingAutomationKind::Webhook,
        false,
        Some(TAILNET_URL),
        1,
    )
    .unwrap();
    assert_eq!(
        switched_off.snapshot.get(MeetingAutomationKind::Webhook),
        Some(&MeetingSeriesAutomation {
            kind: MeetingAutomationKind::Webhook,
            enabled: false,
            target: Some(TAILNET_URL.to_string()),
        }),
        "turning a switch off must not make the operator retype the URL"
    );
    assert!(switched_off.snapshot.runnable().is_empty());

    let forgotten = set(&store, MeetingAutomationKind::Webhook, false, None, 2).unwrap();
    assert_eq!(
        forgotten.snapshot.get(MeetingAutomationKind::Webhook),
        None,
        "off with nothing remembered means the URL leaves the machine"
    );
}

#[test]
fn a_webhook_off_the_operators_network_is_refused_before_it_is_stored() {
    let (_directory, store) = store();

    let public = set(
        &store,
        MeetingAutomationKind::Webhook,
        true,
        Some("https://hooks.example.com/meeting"),
        0,
    );

    assert_eq!(public, Err(StoreError::Invalid));
    assert!(
        store
            .series_automations(SERIES_KEY)
            .unwrap()
            .automations
            .is_empty(),
        "a refused write must leave no row"
    );
}

#[test]
fn turning_a_kind_on_with_nothing_to_point_at_is_refused() {
    let (_directory, store) = store();

    assert_eq!(
        set(&store, MeetingAutomationKind::Shortcut, true, None, 0),
        Err(StoreError::Invalid)
    );
    assert_eq!(
        set(
            &store,
            MeetingAutomationKind::Shortcut,
            true,
            Some("   "),
            0
        ),
        Err(StoreError::Invalid)
    );
}

#[test]
fn reminders_need_no_target_and_never_keep_one() {
    let (_directory, store) = store();

    let result = set(
        &store,
        MeetingAutomationKind::Reminders,
        true,
        Some("ignored"),
        0,
    )
    .unwrap();

    assert_eq!(
        result.snapshot.get(MeetingAutomationKind::Reminders),
        Some(&MeetingSeriesAutomation {
            kind: MeetingAutomationKind::Reminders,
            enabled: true,
            target: None,
        })
    );
    assert_eq!(result.snapshot.runnable().len(), 1);
}

#[test]
fn the_series_list_is_what_has_actually_been_recorded_newest_first() {
    let (_directory, store) = store();
    let first = meeting(&store, "Pricing review", NOW);
    store
        .remember_calendar_facts(first, &series_event(NOW, "Weekly pricing"))
        .unwrap();
    let second = meeting(&store, "Pricing review", NOW + 604_800_000);
    store
        .remember_calendar_facts(
            second,
            &series_event(NOW + 604_800_000, "Weekly pricing (new time)"),
        )
        .unwrap();
    let other = meeting(&store, "Design sync", NOW - 1_000);
    store
        .remember_calendar_facts(
            other,
            &CalendarEventSummary {
                event_key: "design#1".to_string(),
                series_key: "design-sync".to_string(),
                ..series_event(NOW - 1_000, "Design sync")
            },
        )
        .unwrap();
    set(
        &store,
        MeetingAutomationKind::Reminders,
        true,
        None,
        store.series_automations(SERIES_KEY).unwrap().revision,
    )
    .unwrap();

    let listed = store.automation_roster().unwrap().series;

    assert_eq!(
        listed
            .iter()
            .map(|series| series.series_key.as_str())
            .collect::<Vec<_>>(),
        vec![SERIES_KEY, "design-sync"]
    );
    let pricing = &listed[0];
    assert_eq!(pricing.meeting_count, 2);
    assert_eq!(
        pricing.title, "Weekly pricing (new time)",
        "the title is what the most recent occurrence was called"
    );
    assert_eq!(pricing.last_met_at_utc_ms, NOW + 604_800_000);
    assert_eq!(pricing.automations.len(), 1);
    assert!(listed[1].automations.is_empty());
}

#[test]
fn one_artifact_revision_gets_one_attempt_per_kind() {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Pricing review", NOW);
    let artifact_id = MeetingArtifactId::new();
    let automation = webhook(TAILNET_URL);

    let first = store
        .claim_automation_run(artifact_id, session_id, SERIES_KEY, &automation, NOW)
        .unwrap();
    let second = store
        .claim_automation_run(artifact_id, session_id, SERIES_KEY, &automation, NOW + 10)
        .unwrap();

    assert!(first.is_some(), "the first attempt takes the slot");
    assert_eq!(second, None, "the second is refused by the store");

    // A different kind on the same notes is a different attempt, and a different
    // revision of the notes is a new attempt for the same kind.
    assert!(store
        .claim_automation_run(
            artifact_id,
            session_id,
            SERIES_KEY,
            &MeetingSeriesAutomation {
                kind: MeetingAutomationKind::Reminders,
                enabled: true,
                target: None,
            },
            NOW,
        )
        .unwrap()
        .is_some());
    assert!(store
        .claim_automation_run(
            MeetingArtifactId::new(),
            session_id,
            SERIES_KEY,
            &automation,
            NOW,
        )
        .unwrap()
        .is_some());
}

#[test]
fn a_claim_is_visible_as_started_until_it_reports() {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Pricing review", NOW);
    let artifact_id = MeetingArtifactId::new();
    store
        .claim_automation_run(
            artifact_id,
            session_id,
            SERIES_KEY,
            &webhook(TAILNET_URL),
            NOW,
        )
        .unwrap()
        .unwrap();

    let runs = store.automation_runs(session_id).unwrap();

    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, MeetingAutomationRunState::Started);
    assert_eq!(runs[0].finished_at_utc_ms, None);
    assert_eq!(runs[0].failure, None);
    assert_eq!(runs[0].series_key, SERIES_KEY);
    assert_eq!(runs[0].session_id, session_id);
}

#[test]
fn an_outcome_is_written_once_and_carries_its_reason() {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Pricing review", NOW);
    let artifact_id = MeetingArtifactId::new();
    store
        .claim_automation_run(
            artifact_id,
            session_id,
            SERIES_KEY,
            &webhook(TAILNET_URL),
            NOW,
        )
        .unwrap()
        .unwrap();

    let receipt = store
        .finish_automation_run(
            artifact_id,
            MeetingAutomationKind::Webhook,
            MeetingAutomationRunState::Failed,
            Some(MeetingAutomationFailure::HostNotAllowed),
            Some("HTTP 500"),
            0,
            NOW + 400,
        )
        .unwrap();

    assert_eq!(receipt.state, MeetingAutomationRunState::Failed);
    assert_eq!(
        receipt.failure,
        Some(MeetingAutomationFailure::HostNotAllowed)
    );
    assert_eq!(receipt.detail.as_deref(), Some("HTTP 500"));
    assert_eq!(receipt.finished_at_utc_ms, Some(NOW + 400));

    assert_eq!(
        store.finish_automation_run(
            artifact_id,
            MeetingAutomationKind::Webhook,
            MeetingAutomationRunState::Committed,
            None,
            None,
            1,
            NOW + 900,
        ),
        Err(StoreError::Conflict),
        "an attempt that already reported cannot be rewritten"
    );
}

#[test]
fn deleting_a_meeting_takes_its_run_log_with_it() {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Pricing review", NOW);
    store
        .claim_automation_run(
            MeetingArtifactId::new(),
            session_id,
            SERIES_KEY,
            &webhook(TAILNET_URL),
            NOW,
        )
        .unwrap()
        .unwrap();

    store
        .connection()
        .unwrap()
        .execute(
            "DELETE FROM meeting_sessions WHERE id = ?1",
            rusqlite::params![session_id.uuid().to_string()],
        )
        .unwrap();

    assert!(store.automation_runs(session_id).unwrap().is_empty());
}

#[test]
fn a_kind_this_build_does_not_know_cannot_be_stored() {
    let (_directory, store) = store();

    let refused = store.connection().unwrap().execute(
        "INSERT INTO meeting_series_automations (
                series_key, kind, enabled, target, updated_at_utc_ms
             ) VALUES (?1, 'telegram', 1, 'somewhere', 1)",
        rusqlite::params![SERIES_KEY],
    );

    assert!(
        refused.is_err(),
        "the CHECK constraint is what keeps a reader from having to guess"
    );
    assert!(store
        .series_automations(SERIES_KEY)
        .unwrap()
        .automations
        .is_empty());
}

#[test]
fn the_revision_is_shared_across_kinds_so_one_window_fences_another() {
    let (_directory, store) = store();
    set(&store, MeetingAutomationKind::Reminders, true, None, 0).unwrap();

    let racing = set(
        &store,
        MeetingAutomationKind::Shortcut,
        true,
        Some("File the meeting"),
        0,
    )
    .unwrap();

    assert_eq!(racing.receipt.result, OperationResult::Rejected);
    assert_eq!(racing.snapshot.revision, 1);
    let retried = set(
        &store,
        MeetingAutomationKind::Shortcut,
        true,
        Some("File the meeting"),
        1,
    )
    .unwrap();
    assert_eq!(retried.receipt.result, OperationResult::Committed);
    assert_eq!(retried.snapshot.runnable().len(), 2);
}

/// A meeting whose calendar facts name a series resolves to that series'
/// automations, which is the join the after-meeting pass depends on.
#[test]
fn the_series_behind_a_meeting_is_read_from_its_calendar_facts() {
    let (_directory, store) = store();
    let session_id = meeting(&store, "Pricing review", NOW);
    store
        .remember_calendar_facts(session_id, &series_event(NOW, "Weekly pricing"))
        .unwrap();
    set(&store, MeetingAutomationKind::Reminders, true, None, 0).unwrap();

    let snapshot = store.series_automations_for_session(session_id).unwrap();

    assert_eq!(snapshot.series_key.as_deref(), Some(SERIES_KEY));
    assert_eq!(snapshot.runnable().len(), 1);
    assert_eq!(
        snapshot.runnable()[0].kind,
        MeetingAutomationKind::Reminders
    );
}

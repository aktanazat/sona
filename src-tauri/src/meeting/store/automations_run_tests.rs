//! D22's executor: what runs, what does not, what the world outside is asked to
//! do, and what the receipt says afterwards.
//!
//! Under `store/` rather than beside `meeting/automations.rs` because every test
//! here needs a meeting that finished — a session row, calendar facts, a current
//! artifact revision with a ledger in it — and this is where the fixtures that
//! build one live. The subject is `crate::meeting::automations`; the setup is the
//! store's.
//!
//! No effect here touches the world. [`RecordingEffects`] captures exactly what
//! it was asked to do, which is what makes it possible to assert the two things
//! that actually matter about an automation — that it was asked once, and that it
//! was asked with the right bytes — without a Reminders database, a subprocess,
//! or a socket.

use super::workflow_core_tests::{meeting, store};
use super::*;
use crate::meeting::automation_types::{
    MeetingAutomationFailure, MeetingAutomationKind, MeetingAutomationRunReceipt,
    MeetingAutomationRunState, MeetingSeriesAutomationSetRequest,
};
use crate::meeting::automations::{reminders_gate, AutomationEffects, EffectOutcome, ReminderItem};
use crate::meeting::detection::calendar::CalendarAccess;
use crate::meeting::detection::machine::CalendarEventSummary;
use crate::meeting::export::render as render_export;
use crate::meeting::ledger::{
    LedgerCommitment, LedgerFirmness, LedgerReceipt, LedgerReceiptState, MeetingLedger,
};
use crate::meeting::types::{
    ArtifactCitation, CitedArtifactText, GeneratedMeetingArtifacts, MeetingActionItem,
    MeetingExportFormat, MeetingOperationId, MeetingSessionId, OperationResult,
    TranscriptSegmentId,
};
use chrono::NaiveDate;
use parking_lot::Mutex;
use rusqlite::params;
use uuid::Uuid;

const NOW: i64 = 1_700_000_000_000;
const SERIES_KEY: &str = "weekly-pricing";
const TAILNET_URL: &str = "http://100.99.192.40:8650/hooks/meeting";

/// The executor, with a headless generation service behind it.
///
/// Only the `run_prompt` kind ever reaches the service, and no test in this
/// file configures one, so a service with no engines answers every question it
/// is asked here. The wrapper exists so the kind's arrival did not have to be
/// spelled out at every call below.
fn run_for_meeting(
    store: &MeetingStore,
    session_id: MeetingSessionId,
    effects: &dyn AutomationEffects,
    started_at_utc_ms: i64,
) -> Vec<MeetingAutomationRunReceipt> {
    crate::meeting::automations::run_for_meeting(
        store,
        session_id,
        effects,
        &crate::meeting::processing::MeetingProcessingService::new(None),
        started_at_utc_ms,
    )
}

/// What the effects were asked to do, in order.
#[derive(Debug, Default)]
struct Asked {
    reminders: Vec<Vec<ReminderItem>>,
    shortcuts: Vec<(String, Vec<u8>)>,
    webhooks: Vec<(String, Vec<u8>)>,
}

/// Effects that record instead of acting, and answer however a test needs.
struct RecordingEffects {
    asked: Mutex<Asked>,
    answer: EffectOutcome,
}

impl RecordingEffects {
    fn accepting() -> Self {
        Self {
            asked: Mutex::new(Asked::default()),
            answer: EffectOutcome::committed(1, None),
        }
    }

    fn refusing(failure: MeetingAutomationFailure) -> Self {
        Self {
            asked: Mutex::new(Asked::default()),
            answer: EffectOutcome::failed(failure, None),
        }
    }

    fn asked(&self) -> parking_lot::MutexGuard<'_, Asked> {
        self.asked.lock()
    }
}

impl AutomationEffects for RecordingEffects {
    fn write_reminders(&self, items: &[ReminderItem]) -> EffectOutcome {
        self.asked().reminders.push(items.to_vec());
        self.answer.clone()
    }

    fn run_shortcut(&self, name: &str, stdin: &[u8]) -> EffectOutcome {
        self.asked()
            .shortcuts
            .push((name.to_string(), stdin.to_vec()));
        self.answer.clone()
    }

    fn post_webhook(&self, url: &str, body: &[u8]) -> EffectOutcome {
        self.asked().webhooks.push((url.to_string(), body.to_vec()));
        self.answer.clone()
    }
}

fn series_event(at_utc_ms: i64) -> CalendarEventSummary {
    CalendarEventSummary {
        event_key: format!("{SERIES_KEY}#{at_utc_ms}"),
        series_key: SERIES_KEY.to_string(),
        title: "Weekly pricing".to_string(),
        attendee_count: 2,
        start_utc_ms: at_utc_ms,
        end_utc_ms: at_utc_ms + 1_800_000,
        attendees: Vec::new(),
        notes: None,
        calendar_name: None,
        url: None,
    }
}

/// One citation of a transcript segment. The offsets are the same for every
/// citation here: nothing in this file reads them, and the segment is what joins
/// a ledger row to the notes pass's reading of the same moment.
fn citation(segment_id: TranscriptSegmentId) -> ArtifactCitation {
    ArtifactCitation {
        segment_id,
        start_offset_ns: 0,
        end_offset_ns: 1_000,
    }
}

/// A ledger with one commitment the operator made and one somebody else did, so
/// D27's direction is what decides which becomes a reminder.
fn ledger() -> MeetingLedger {
    MeetingLedger {
        headline: "Pricing stayed open.".to_string(),
        threads: Vec::new(),
        open_loops: Vec::new(),
        commitments: vec![
            LedgerCommitment {
                who: "I".to_string(),
                what: "Send the tier comparison".to_string(),
                firmness: LedgerFirmness::Firm,
                receipt: LedgerReceipt {
                    quote: "i'll send the tier comparison".to_string(),
                    speaker: None,
                    t_ms: 30_000,
                    citations: Vec::new(),
                },
            },
            LedgerCommitment {
                who: "Dana Reyes".to_string(),
                what: "Confirm the trial tier".to_string(),
                firmness: LedgerFirmness::Firm,
                receipt: LedgerReceipt {
                    quote: "i'll confirm the trial tier".to_string(),
                    speaker: Some("Dana".to_string()),
                    t_ms: 60_000,
                    citations: Vec::new(),
                },
            },
        ],
        stances: Vec::new(),
        caveats: Vec::new(),
        receipts: LedgerReceiptState::Verified,
    }
}

fn artifact_content(ledger: Option<MeetingLedger>) -> GeneratedMeetingArtifacts {
    GeneratedMeetingArtifacts {
        summary: CitedArtifactText {
            text: "Pricing stayed open.".to_string(),
            citations: Vec::new(),
        },
        summary_trace: Vec::new(),
        outline: Vec::new(),
        decisions: Vec::new(),
        action_items: Vec::new(),
        key_questions: Vec::new(),
        risks: Vec::new(),
        follow_up_draft: CitedArtifactText {
            text: String::new(),
            citations: Vec::new(),
        },
        ledger,
        ledger_failure: None,
    }
}

fn store_current_artifact(
    store: &MeetingStore,
    session_id: MeetingSessionId,
    content: &GeneratedMeetingArtifacts,
) {
    let revision_id = Uuid::new_v4();
    let artifact_id = Uuid::new_v4();
    let connection = store.connection().unwrap();
    connection
        .execute(
            "INSERT INTO meeting_transcript_revisions (
                transcript_revision_id, session_id, engine_id, destination_json,
                source_set_json, language, state, created_at_utc_ms
             ) VALUES (?1, ?2, 'test', '{}', '[]', 'en', 'complete', 1)",
            params![revision_id.to_string(), session_id.uuid().to_string()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO meeting_artifact_revisions (
                artifact_id, session_id, transcript_revision_id, input_revision,
                template_id, template_version, generation_key, state,
                content_json, generated_at_utc_ms
             ) VALUES (?1, ?2, ?3, 0, 'test', 1, ?4, 'current', ?5, 1)",
            params![
                artifact_id.to_string(),
                session_id.uuid().to_string(),
                revision_id.to_string(),
                format!("test-{artifact_id}"),
                serde_json::to_string(content).unwrap()
            ],
        )
        .unwrap();
}

/// A meeting in review, in a calendar series, with current notes and a ledger.
fn finished_meeting(store: &MeetingStore) -> MeetingSessionId {
    let session_id = reviewable_meeting(store, "Pricing review");
    store
        .remember_calendar_facts(session_id, &series_event(NOW))
        .unwrap();
    store_current_artifact(store, session_id, &artifact_content(Some(ledger())));
    session_id
}

/// A meeting whose row survives `review_snapshot`.
///
/// The shared `meeting()` fixture writes a bare `pending` into
/// `processing_status`, which is a JSON column — every reader that has needed it
/// so far reads around it, and `review_snapshot` is the first that decodes it.
/// The export payload is built from that snapshot, so these tests need a row
/// that decodes.
fn reviewable_meeting(store: &MeetingStore, title: &str) -> MeetingSessionId {
    let session_id = meeting(store, title, NOW);
    store
        .connection()
        .unwrap()
        .execute(
            "UPDATE meeting_sessions
                SET processing_status = '{\"kind\":\"succeeded\"}',
                    retention_policy_json = '{\"kind\":\"forever\"}'
              WHERE id = ?1",
            params![session_id.uuid().to_string()],
        )
        .unwrap();
    session_id
}

fn enable(store: &MeetingStore, kind: MeetingAutomationKind, target: Option<&str>) {
    let revision = store.series_automations(SERIES_KEY).unwrap().revision;
    let result = store
        .set_series_automation(
            &MeetingSeriesAutomationSetRequest {
                operation_id: MeetingOperationId::new(),
                series_key: SERIES_KEY.to_string(),
                kind,
                enabled: true,
                target: target.map(str::to_string),
                expected_revision: revision,
            },
            NOW,
        )
        .unwrap();
    assert_eq!(result.receipt.result, OperationResult::Committed);
}

#[test]
fn a_series_with_nothing_enabled_runs_nothing_and_records_nothing() {
    let (_directory, store) = store();
    let session_id = finished_meeting(&store);
    let effects = RecordingEffects::accepting();

    let receipts = run_for_meeting(&store, session_id, &effects, NOW);

    assert!(receipts.is_empty());
    assert!(store.automation_runs(session_id).unwrap().is_empty());
    let asked = effects.asked();
    assert!(asked.webhooks.is_empty());
    assert!(asked.shortcuts.is_empty());
    assert!(asked.reminders.is_empty());
}
/// The three facts every other test in this file depends on, asserted once so a
/// failure below names the layer that broke rather than "nothing ran".
#[test]
fn the_fixture_produces_a_meeting_the_pass_can_act_on() {
    let (_directory, store) = store();
    let session_id = finished_meeting(&store);
    enable(&store, MeetingAutomationKind::Webhook, Some(TAILNET_URL));

    let snapshot = store.series_automations_for_session(session_id).unwrap();
    assert_eq!(snapshot.series_key.as_deref(), Some(SERIES_KEY));
    assert_eq!(snapshot.runnable().len(), 1);

    let review = store.review_snapshot(session_id).unwrap();
    assert_eq!(
        review
            .artifacts
            .iter()
            .filter(
                |artifact| artifact.state == crate::meeting::types::MeetingArtifactState::Current
            )
            .count(),
        1
    );
    assert_eq!(
        store
            .meeting_loops(session_id)
            .unwrap()
            .rows
            .iter()
            .filter(|row| row.is_open() && row.is_mine())
            .count(),
        1
    );
}

#[test]
fn a_meeting_with_no_series_runs_nothing_even_when_a_series_is_configured() {
    let (_directory, store) = store();
    finished_meeting(&store);
    enable(&store, MeetingAutomationKind::Webhook, Some(TAILNET_URL));
    let manual = reviewable_meeting(&store, "Local notes");
    store_current_artifact(&store, manual, &artifact_content(Some(ledger())));
    let effects = RecordingEffects::accepting();

    let receipts = run_for_meeting(&store, manual, &effects, NOW);

    assert!(
        receipts.is_empty(),
        "a manual recording belongs to no series, so no series can act on it"
    );
    assert!(effects.asked().webhooks.is_empty());
    assert!(store.automation_runs(manual).unwrap().is_empty());
}

#[test]
fn only_the_enabled_kinds_run() {
    let (_directory, store) = store();
    let session_id = finished_meeting(&store);
    enable(&store, MeetingAutomationKind::Webhook, Some(TAILNET_URL));
    let effects = RecordingEffects::accepting();

    let receipts = run_for_meeting(&store, session_id, &effects, NOW);

    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].kind, MeetingAutomationKind::Webhook);
    assert_eq!(receipts[0].state, MeetingAutomationRunState::Committed);
    let asked = effects.asked();
    assert_eq!(asked.webhooks.len(), 1);
    assert!(asked.shortcuts.is_empty());
    assert!(asked.reminders.is_empty());
}

#[test]
fn one_artifact_revision_is_attempted_once_however_often_the_pass_runs() {
    let (_directory, store) = store();
    let session_id = finished_meeting(&store);
    enable(&store, MeetingAutomationKind::Webhook, Some(TAILNET_URL));
    let effects = RecordingEffects::accepting();

    let first = run_for_meeting(&store, session_id, &effects, NOW);
    let second = run_for_meeting(&store, session_id, &effects, NOW + 1_000);

    assert_eq!(first.len(), 1);
    assert!(
        second.is_empty(),
        "the same notes must not be sent to a webhook twice"
    );
    assert_eq!(effects.asked().webhooks.len(), 1);
    assert_eq!(store.automation_runs(session_id).unwrap().len(), 1);
}

#[test]
fn a_shortcut_is_asked_for_by_name_with_the_export_on_its_stdin() {
    let (_directory, store) = store();
    let session_id = finished_meeting(&store);
    enable(
        &store,
        MeetingAutomationKind::Shortcut,
        Some("File the meeting"),
    );
    let effects = RecordingEffects::accepting();

    run_for_meeting(&store, session_id, &effects, NOW);

    let asked = effects.asked();
    assert_eq!(asked.shortcuts.len(), 1);
    let (name, stdin) = &asked.shortcuts[0];
    assert_eq!(
        name, "File the meeting",
        "the name is one argument, never a shell word"
    );
    let document: serde_json::Value = serde_json::from_slice(stdin).expect("stdin is the export");
    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["review"]["session"]["title"], "Pricing review");
}

#[test]
fn a_webhook_is_posted_the_same_document_the_export_action_writes() {
    let (_directory, store) = store();
    let session_id = finished_meeting(&store);
    enable(&store, MeetingAutomationKind::Webhook, Some(TAILNET_URL));
    let effects = RecordingEffects::accepting();

    run_for_meeting(&store, session_id, &effects, NOW);

    let asked = effects.asked();
    let (url, body) = &asked.webhooks[0];
    assert_eq!(url, TAILNET_URL);
    let review = store.review_snapshot(session_id).unwrap();
    assert_eq!(
        body,
        &render_export(MeetingExportFormat::Json, &review).unwrap(),
        "one document, from the one renderer the Export action uses"
    );
}

#[test]
fn a_shortcut_and_a_webhook_on_one_series_share_one_rendered_export() {
    let (_directory, store) = store();
    let session_id = finished_meeting(&store);
    enable(&store, MeetingAutomationKind::Webhook, Some(TAILNET_URL));
    enable(
        &store,
        MeetingAutomationKind::Shortcut,
        Some("File the meeting"),
    );
    let effects = RecordingEffects::accepting();

    let receipts = run_for_meeting(&store, session_id, &effects, NOW);

    assert_eq!(receipts.len(), 2);
    let asked = effects.asked();
    assert_eq!(asked.shortcuts[0].1, asked.webhooks[0].1);
}

#[test]
fn a_webhook_whose_host_left_the_allowlist_fails_without_being_sent() {
    let (_directory, store) = store();
    let session_id = finished_meeting(&store);
    enable(&store, MeetingAutomationKind::Webhook, Some(TAILNET_URL));
    // The settings surface refuses a public host, so the only way a row can hold
    // one is a build or an allowlist older than this one. That row has to fail at
    // the boundary rather than reach the network.
    store
        .connection()
        .unwrap()
        .execute(
            "UPDATE meeting_series_automations
                SET target = 'https://hooks.example.com/meeting'
              WHERE series_key = ?1 AND kind = 'webhook'",
            params![SERIES_KEY],
        )
        .unwrap();
    let effects = RecordingEffects::accepting();

    let receipts = run_for_meeting(&store, session_id, &effects, NOW);

    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].state, MeetingAutomationRunState::Failed);
    assert_eq!(
        receipts[0].failure,
        Some(MeetingAutomationFailure::HostNotAllowed)
    );
    assert!(
        effects.asked().webhooks.is_empty(),
        "nothing may leave the machine for a host the policy refuses"
    );
}

#[test]
fn a_refused_effect_is_recorded_as_failed_with_its_reason() {
    let (_directory, store) = store();
    let session_id = finished_meeting(&store);
    enable(&store, MeetingAutomationKind::Reminders, None);
    let effects = RecordingEffects::refusing(MeetingAutomationFailure::PermissionDenied);

    let receipts = run_for_meeting(&store, session_id, &effects, NOW);

    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].state, MeetingAutomationRunState::Failed);
    assert_eq!(
        receipts[0].failure,
        Some(MeetingAutomationFailure::PermissionDenied)
    );
    assert_eq!(receipts[0].effects, 0);
    assert_eq!(
        store.automation_runs(session_id).unwrap()[0].failure,
        Some(MeetingAutomationFailure::PermissionDenied),
        "the receipt is the store's, not the return value's"
    );
}

#[test]
fn reminders_carry_only_what_the_operator_owes_and_link_back() {
    let (_directory, store) = store();
    let session_id = finished_meeting(&store);
    enable(&store, MeetingAutomationKind::Reminders, None);
    let effects = RecordingEffects::accepting();

    run_for_meeting(&store, session_id, &effects, NOW);

    let asked = effects.asked();
    assert_eq!(asked.reminders.len(), 1);
    let items = &asked.reminders[0];
    assert_eq!(
        items
            .iter()
            .map(|item| item.title.as_str())
            .collect::<Vec<_>>(),
        vec!["Send the tier comparison"],
        "a commitment somebody else made is not the operator's to do"
    );
    assert!(items[0].notes.starts_with("Pricing review\n"));
    assert!(
        items[0].notes.contains("sona://loop/"),
        "a reminder has to lead back to the sentence that made it"
    );
}

/// A ledger commitment carries no due text of its own, so a reminder's day comes
/// from the notes pass's action item read out of the same transcript segment.
/// The date here is written as an ISO day on purpose: a day word resolves
/// against the operator's local calendar day, and this assertion must not depend
/// on which timezone the test runs in.
#[test]
fn a_commitment_the_notes_dated_reaches_reminders_with_that_day() {
    let (_directory, store) = store();
    let session_id = reviewable_meeting(&store, "Pricing review");
    store
        .remember_calendar_facts(session_id, &series_event(NOW))
        .unwrap();
    let segment = TranscriptSegmentId::new();
    let mut ledger = ledger();
    ledger.commitments[0].receipt.citations = vec![citation(segment)];
    let mut content = artifact_content(Some(ledger));
    content.action_items = vec![
        MeetingActionItem {
            text: CitedArtifactText {
                text: "Send the tier comparison".to_string(),
                citations: vec![citation(segment)],
            },
            owner_text: Some("Aktan".to_string()),
            due_text: Some("by 2026-04-01".to_string()),
        },
        // Another moment in the same meeting, with a day on it. It must not
        // reach a commitment it shares no segment with.
        MeetingActionItem {
            text: CitedArtifactText {
                text: "Book the room".to_string(),
                citations: vec![citation(TranscriptSegmentId::new())],
            },
            owner_text: None,
            due_text: Some("2026-04-09".to_string()),
        },
    ];
    store_current_artifact(&store, session_id, &content);
    enable(&store, MeetingAutomationKind::Reminders, None);
    let effects = RecordingEffects::accepting();

    run_for_meeting(&store, session_id, &effects, NOW);

    let asked = effects.asked();
    let items = &asked.reminders[0];
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title, "Send the tier comparison");
    assert_eq!(
        items[0].due_on,
        NaiveDate::from_ymd_opt(2026, 4, 1),
        "the day belongs to the action item read from the same segment"
    );
}

/// The same meeting with nothing dated: the reminder is written undated rather
/// than given a day nobody said.
#[test]
fn a_commitment_the_notes_left_undated_becomes_a_reminder_with_no_day() {
    let (_directory, store) = store();
    let session_id = reviewable_meeting(&store, "Pricing review");
    store
        .remember_calendar_facts(session_id, &series_event(NOW))
        .unwrap();
    let segment = TranscriptSegmentId::new();
    let mut ledger = ledger();
    ledger.commitments[0].receipt.citations = vec![citation(segment)];
    let mut content = artifact_content(Some(ledger));
    content.action_items = vec![MeetingActionItem {
        text: CitedArtifactText {
            text: "Send the tier comparison".to_string(),
            citations: vec![citation(segment)],
        },
        owner_text: None,
        due_text: None,
    }];
    store_current_artifact(&store, session_id, &content);
    enable(&store, MeetingAutomationKind::Reminders, None);
    let effects = RecordingEffects::accepting();

    run_for_meeting(&store, session_id, &effects, NOW);

    let asked = effects.asked();
    assert_eq!(asked.reminders[0][0].due_on, None);
}

#[test]
fn a_meeting_where_nothing_is_owed_still_records_that_it_looked() {
    let (_directory, store) = store();
    let session_id = reviewable_meeting(&store, "Pricing review");
    store
        .remember_calendar_facts(session_id, &series_event(NOW))
        .unwrap();
    store_current_artifact(&store, session_id, &artifact_content(None));
    enable(&store, MeetingAutomationKind::Reminders, None);
    let effects = RecordingEffects::accepting();

    let receipts = run_for_meeting(&store, session_id, &effects, NOW);

    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].state, MeetingAutomationRunState::Committed);
    assert_eq!(receipts[0].effects, 0);
    assert_eq!(receipts[0].detail.as_deref(), Some("no open commitments"));
    assert!(
        effects.asked().reminders.is_empty(),
        "an empty batch is not worth touching Reminders for"
    );
}

#[test]
fn a_meeting_with_no_current_notes_is_not_an_after_meeting_action() {
    let (_directory, store) = store();
    let session_id = reviewable_meeting(&store, "Pricing review");
    store
        .remember_calendar_facts(session_id, &series_event(NOW))
        .unwrap();
    enable(&store, MeetingAutomationKind::Webhook, Some(TAILNET_URL));
    let effects = RecordingEffects::accepting();

    let receipts = run_for_meeting(&store, session_id, &effects, NOW);

    assert!(receipts.is_empty());
    assert!(effects.asked().webhooks.is_empty());
    assert!(store.automation_runs(session_id).unwrap().is_empty());
}

#[test]
fn a_switch_that_is_off_does_not_run_even_with_a_target_remembered() {
    let (_directory, store) = store();
    let session_id = finished_meeting(&store);
    enable(&store, MeetingAutomationKind::Webhook, Some(TAILNET_URL));
    let revision = store.series_automations(SERIES_KEY).unwrap().revision;
    store
        .set_series_automation(
            &MeetingSeriesAutomationSetRequest {
                operation_id: MeetingOperationId::new(),
                series_key: SERIES_KEY.to_string(),
                kind: MeetingAutomationKind::Webhook,
                enabled: false,
                target: Some(TAILNET_URL.to_string()),
                expected_revision: revision,
            },
            NOW,
        )
        .unwrap();
    let effects = RecordingEffects::accepting();

    let receipts = run_for_meeting(&store, session_id, &effects, NOW);

    assert!(receipts.is_empty());
    assert!(effects.asked().webhooks.is_empty());
}

/// A model that returns a paragraph where a commitment belongs must not produce
/// a reminder nobody can read in a list.
#[test]
fn a_runaway_commitment_becomes_a_reminder_title_somebody_can_read() {
    let (_directory, store) = store();
    let session_id = reviewable_meeting(&store, "Pricing review");
    store
        .remember_calendar_facts(session_id, &series_event(NOW))
        .unwrap();
    let mut ledger = ledger();
    ledger.commitments[0].what = format!("  Send  {}  ", "the tier comparison ".repeat(40));
    store_current_artifact(&store, session_id, &artifact_content(Some(ledger)));
    enable(&store, MeetingAutomationKind::Reminders, None);
    let effects = RecordingEffects::accepting();

    run_for_meeting(&store, session_id, &effects, NOW);

    let asked = effects.asked();
    let title = &asked.reminders[0][0].title;
    assert!(title.chars().count() <= 200);
    assert!(title.ends_with('…'));
    assert!(
        !title.contains("  "),
        "a title read out of a transcript arrives with the transcript's whitespace"
    );
}

#[test]
fn a_shortcut_name_that_would_read_as_a_flag_is_refused_at_the_boundary() {
    assert_eq!(
        MeetingAutomationKind::Shortcut.normalize_target(Some("--help")),
        Err(MeetingAutomationFailure::TargetInvalid)
    );
    assert_eq!(
        MeetingAutomationKind::Shortcut.normalize_target(Some("File the\nmeeting")),
        Err(MeetingAutomationFailure::TargetInvalid),
        "a control byte in a name is a shaping hazard even without a shell"
    );
    assert_eq!(
        MeetingAutomationKind::Shortcut.normalize_target(Some("  File the meeting  ")),
        Ok(Some("File the meeting".to_string()))
    );
    assert_eq!(
        MeetingAutomationKind::Shortcut.normalize_target(None),
        Err(MeetingAutomationFailure::TargetMissing)
    );
}

#[test]
fn a_webhook_url_carrying_credentials_or_a_public_host_is_refused() {
    assert_eq!(
        MeetingAutomationKind::Webhook
            .normalize_target(Some("http://user:secret@127.0.0.1:8000/hook")),
        Err(MeetingAutomationFailure::TargetInvalid),
        "a password in a URL is a secret this app would then be holding"
    );
    assert_eq!(
        MeetingAutomationKind::Webhook.normalize_target(Some("http://127.0.0.1:8000/hook#part")),
        Err(MeetingAutomationFailure::TargetInvalid)
    );
    assert_eq!(
        MeetingAutomationKind::Webhook.normalize_target(Some("ftp://127.0.0.1/hook")),
        Err(MeetingAutomationFailure::TargetInvalid)
    );
    assert_eq!(
        MeetingAutomationKind::Webhook.normalize_target(Some("https://hooks.example.com/hook")),
        Err(MeetingAutomationFailure::HostNotAllowed)
    );
    assert_eq!(
        MeetingAutomationKind::Webhook.normalize_target(Some(
            "http://hermes-agent-01.taile1234.ts.net/hooks/meeting"
        )),
        Ok(Some(
            "http://hermes-agent-01.taile1234.ts.net/hooks/meeting".to_string()
        )),
        "a path is what a webhook usually is, so it survives normalization"
    );
}

#[test]
fn reminders_asks_for_nothing_and_says_which_grant_it_found() {
    assert_eq!(reminders_gate(CalendarAccess::Authorized), None);
    assert_eq!(
        reminders_gate(CalendarAccess::Denied),
        Some(MeetingAutomationFailure::PermissionDenied)
    );
    assert_eq!(
        reminders_gate(CalendarAccess::NotDetermined),
        Some(MeetingAutomationFailure::PermissionDenied),
        "this pass never prompts, so unasked and refused have one consequence here"
    );
    assert_eq!(
        reminders_gate(CalendarAccess::Unavailable),
        Some(MeetingAutomationFailure::Unavailable),
        "no Reminders at all is a different sentence from a refusal"
    );
}

#[test]
fn a_denied_grant_lands_in_the_receipt_rather_than_in_a_dialog() {
    let (_directory, store) = store();
    let session_id = finished_meeting(&store);
    enable(&store, MeetingAutomationKind::Reminders, None);
    let denied = reminders_gate(CalendarAccess::Denied).expect("denied refuses");
    let effects = RecordingEffects::refusing(denied);

    run_for_meeting(&store, session_id, &effects, NOW);

    let recorded = store.automation_runs(session_id).unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].state, MeetingAutomationRunState::Failed);
    assert_eq!(
        recorded[0].failure,
        Some(MeetingAutomationFailure::PermissionDenied)
    );
    assert_eq!(
        run_for_meeting(&store, session_id, &effects, NOW + 1).len(),
        0,
        "a denied grant does not earn a second attempt after the next meeting"
    );
}

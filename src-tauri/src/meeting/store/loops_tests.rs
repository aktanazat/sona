//! The loop mutation matrix: resolve, reopen, assign, carry — receipts and
//! fencing on each.

use super::loops::{ledger_loop_seeds, SCHEMA_VERSION};
use super::workflow_core_tests::{meeting, person as insert_person, store};
use super::{MeetingStore, StoreError};
use crate::meeting::detection::machine::CalendarEventSummary;
use crate::meeting::ledger::{
    LedgerCommitment, LedgerFirmness, LedgerOpenLoop, LedgerReceipt, LedgerReceiptState,
    LedgerThread, LedgerThreadState, MeetingLedger,
};
use crate::meeting::loop_types::{
    MeetingLoopAssignRequest, MeetingLoopId, MeetingLoopKind, MeetingLoopReopenRequest,
    MeetingLoopResolution, MeetingLoopResolveRequest, MeetingLoopRow, MeetingLoopStatus,
};
use crate::meeting::people_types::PersonId;
use crate::meeting::types::{
    GeneratedMeetingArtifacts, MeetingCommandKind, MeetingOperationId, MeetingReasonCode,
    MeetingSessionId, OperationActor, OperationResult,
};
use rusqlite::params;
use std::sync::Arc;
use tempfile::TempDir;
use uuid::Uuid;

const NOW: i64 = 1_700_000_000_000;
const WEEK_MS: i64 = 604_800_000;
const SERIES_KEY: &str = "weekly-pricing";

fn ledger() -> MeetingLedger {
    MeetingLedger {
        headline: "Pricing stayed open and Dana took the comparison.".to_string(),
        threads: vec![LedgerThread {
            topic: "Trial conversion tier".to_string(),
            state: LedgerThreadState::Unanswered,
            substantive: true,
            receipt: LedgerReceipt {
                quote: "which tier does the trial convert into".to_string(),
                speaker: Some("Dana".to_string()),
                t_ms: 12_000,
                citations: Vec::new(),
            },
            owner: Some("Dana Reyes".to_string()),
        }],
        open_loops: vec![LedgerOpenLoop {
            question: "Trial conversion tier".to_string(),
            instead: "The meeting moved on to billing.".to_string(),
            at_ms: 12_000,
            citations: Vec::new(),
        }],
        commitments: vec![LedgerCommitment {
            who: "Dana Reyes".to_string(),
            what: "Send the tier comparison".to_string(),
            firmness: LedgerFirmness::Firm,
            receipt: LedgerReceipt {
                quote: "i'll send the tier comparison".to_string(),
                speaker: Some("Dana".to_string()),
                t_ms: 30_000,
                citations: Vec::new(),
            },
        }],
        stances: Vec::new(),
        caveats: Vec::new(),
        receipts: LedgerReceiptState::Verified,
    }
}

fn loop_id(session_id: MeetingSessionId, kind: MeetingLoopKind, text: &str) -> MeetingLoopId {
    MeetingLoopId::derive(session_id, kind, text)
}

fn row_for<'rows>(rows: &'rows [MeetingLoopRow], loop_id: &MeetingLoopId) -> &'rows MeetingLoopRow {
    rows.iter()
        .find(|row| row.loop_id == *loop_id)
        .expect("the row the loop id names")
}

fn person(store: &MeetingStore, name: &str) -> PersonId {
    insert_person(store, name, &[], &[])
}

/// A meeting in review with the ledger above already stored as its current
/// artifact revision.
fn ledger_meeting(store: &MeetingStore, at_utc_ms: i64) -> MeetingSessionId {
    let session_id = meeting(store, "Pricing review", at_utc_ms);
    let revision_id = Uuid::new_v4();
    let artifact_id = Uuid::new_v4();
    let content = GeneratedMeetingArtifacts {
        summary: crate::meeting::types::CitedArtifactText {
            text: "Pricing stayed open.".to_string(),
            citations: Vec::new(),
        },
        summary_trace: Vec::new(),
        outline: Vec::new(),
        decisions: Vec::new(),
        action_items: Vec::new(),
        key_questions: Vec::new(),
        risks: Vec::new(),
        follow_up_draft: crate::meeting::types::CitedArtifactText {
            text: String::new(),
            citations: Vec::new(),
        },
        ledger: Some(ledger()),
    };
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
                serde_json::to_string(&content).unwrap()
            ],
        )
        .unwrap();
    drop(connection);
    session_id
}

/// The first occurrence of a weekly series, in review, with its ledger stored.
fn review_ready_meeting_with_ledger() -> (TempDir, Arc<MeetingStore>, MeetingSessionId) {
    let (directory, store) = store();
    let session_id = series_meeting_with_ledger(&store, NOW);
    (directory, store, session_id)
}

/// One occurrence of the same calendar series, with the same ledger.
fn series_meeting_with_ledger(store: &MeetingStore, at_utc_ms: i64) -> MeetingSessionId {
    let session_id = ledger_meeting(store, at_utc_ms);
    store
        .remember_calendar_facts(session_id, &series_event(at_utc_ms))
        .unwrap();
    session_id
}

fn series_event(start_utc_ms: i64) -> CalendarEventSummary {
    CalendarEventSummary {
        event_key: format!("{SERIES_KEY}#{start_utc_ms}"),
        series_key: SERIES_KEY.to_string(),
        title: "Pricing review".to_string(),
        attendee_count: 2,
        start_utc_ms,
        end_utc_ms: start_utc_ms + 1_800_000,
        attendees: Vec::new(),
        notes: None,
        calendar_name: None,
        url: None,
    }
}

#[test]
fn a_thread_and_its_open_loop_entry_collapse_into_one_row() {
    let session_id = MeetingSessionId::new();
    let seeds = ledger_loop_seeds(session_id, &ledger());

    assert_eq!(
        seeds.len(),
        2,
        "one loop and one commitment, not three rows"
    );
    let question = &seeds[0];
    assert_eq!(question.kind, MeetingLoopKind::Loop);
    assert_eq!(
        question.owner_text.as_deref(),
        Some("Dana Reyes"),
        "the thread's owner survives the fold"
    );
    assert_eq!(
        question.instead.as_deref(),
        Some("The meeting moved on to billing."),
        "and so does what happened instead"
    );
    assert_eq!(
        question.quote.as_deref(),
        Some("which tier does the trial convert into")
    );
    assert_eq!(seeds[1].kind, MeetingLoopKind::Commitment);
    assert_eq!(seeds[1].firmness, Some(LedgerFirmness::Firm));
}

#[test]
fn an_untouched_loop_reads_open_at_revision_zero() {
    let (_directory, store, session_id) = review_ready_meeting_with_ledger();
    let loops = store.meeting_loops(session_id).unwrap();

    assert_eq!(loops.schema_version, SCHEMA_VERSION);
    assert_eq!(loops.rows.len(), 2);
    for row in &loops.rows {
        assert_eq!(row.status, MeetingLoopStatus::Open);
        assert_eq!(row.revision, 0, "no row means open, and open means zero");
        assert!(row.resolved_at_utc_ms.is_none());
        assert!(row.owner_person_id.is_none());
    }
}

#[test]
fn resolving_commits_a_receipt_and_moves_the_row() {
    let (_directory, store, session_id) = review_ready_meeting_with_ledger();
    let commitment = loop_id(
        session_id,
        MeetingLoopKind::Commitment,
        "Send the tier comparison",
    );
    let operation_id = MeetingOperationId::new();

    let result = store
        .resolve_loop(
            MeetingLoopResolveRequest {
                operation_id,
                loop_id: commitment.clone(),
                expected_revision: 0,
                resolution: MeetingLoopResolution::Done,
            },
            OperationActor::User,
            NOW,
        )
        .unwrap();

    assert_eq!(result.receipt.result, OperationResult::Committed);
    assert_eq!(result.receipt.command, MeetingCommandKind::LoopResolve);
    assert_eq!(result.receipt.session_id, Some(session_id));
    assert_eq!(result.receipt.expected_revision, 0);
    assert_eq!(result.receipt.new_revision, Some(1));
    assert_eq!(result.receipt.effect_ids, vec![commitment.0.clone()]);
    assert!(result.receipt.reason_codes.is_empty());

    let row = row_for(&result.loops.rows, &commitment);
    assert_eq!(row.status, MeetingLoopStatus::Done);
    assert_eq!(row.revision, 1);
    assert_eq!(
        row.resolved_at_utc_ms,
        Some(row.resolved_at_utc_ms.unwrap())
    );
    assert_eq!(
        row.resolving_operation_id.as_deref(),
        Some(operation_id.uuid().to_string().as_str()),
        "the row names the operation that moved it"
    );
}

/// Who closed a row, on the row. Two things close a loop — a press in the app
/// and `sona --loop-resolve` from outside it — and this field is the only way
/// a reader can tell which one it was looking at.
#[test]
fn a_row_names_the_actor_that_closed_it() {
    let (_directory, store, session_id) = review_ready_meeting_with_ledger();
    let question = loop_id(session_id, MeetingLoopKind::Loop, "Trial conversion tier");
    let commitment = loop_id(
        session_id,
        MeetingLoopKind::Commitment,
        "Send the tier comparison",
    );

    let untouched = store.meeting_loops(session_id).unwrap();
    assert_eq!(
        row_for(&untouched.rows, &question).resolved_by,
        None,
        "an open row was closed by nobody"
    );

    let outside = store
        .resolve_loop(
            MeetingLoopResolveRequest {
                operation_id: MeetingOperationId::new(),
                loop_id: question.clone(),
                expected_revision: 0,
                resolution: MeetingLoopResolution::Done,
            },
            OperationActor::External,
            NOW,
        )
        .unwrap();
    assert_eq!(
        row_for(&outside.loops.rows, &question).resolved_by,
        Some(OperationActor::External),
        "an outside write closed this one, and the row says so"
    );
    assert_eq!(
        row_for(&outside.loops.rows, &commitment).resolved_by,
        None,
        "the row nobody touched still names nobody"
    );

    let pressed = store
        .resolve_loop(
            MeetingLoopResolveRequest {
                operation_id: MeetingOperationId::new(),
                loop_id: commitment.clone(),
                expected_revision: 0,
                resolution: MeetingLoopResolution::Done,
            },
            OperationActor::User,
            NOW,
        )
        .unwrap();
    assert_eq!(
        row_for(&pressed.loops.rows, &commitment).resolved_by,
        Some(OperationActor::User)
    );

    let reopened = store
        .reopen_loop(
            MeetingLoopReopenRequest {
                operation_id: MeetingOperationId::new(),
                loop_id: question.clone(),
                expected_revision: 1,
            },
            NOW,
        )
        .unwrap();
    assert_eq!(
        row_for(&reopened.loops.rows, &question).resolved_by,
        None,
        "a reopened row is closed by nobody again"
    );
}

#[test]
fn dropping_is_a_resolution_and_reopening_undoes_it() {
    let (_directory, store, session_id) = review_ready_meeting_with_ledger();
    let question = loop_id(session_id, MeetingLoopKind::Loop, "Trial conversion tier");

    let dropped = store
        .resolve_loop(
            MeetingLoopResolveRequest {
                operation_id: MeetingOperationId::new(),
                loop_id: question.clone(),
                expected_revision: 0,
                resolution: MeetingLoopResolution::Dropped,
            },
            OperationActor::User,
            NOW,
        )
        .unwrap();
    assert_eq!(
        row_for(&dropped.loops.rows, &question).status,
        MeetingLoopStatus::Dropped
    );

    let reopened = store
        .reopen_loop(
            MeetingLoopReopenRequest {
                operation_id: MeetingOperationId::new(),
                loop_id: question.clone(),
                expected_revision: 1,
            },
            NOW,
        )
        .unwrap();

    assert_eq!(reopened.receipt.command, MeetingCommandKind::LoopReopen);
    let row = row_for(&reopened.loops.rows, &question);
    assert_eq!(row.status, MeetingLoopStatus::Open);
    assert_eq!(row.revision, 2);
    assert!(
        row.resolved_at_utc_ms.is_none(),
        "a reopened loop is not resolved at any time"
    );
}

#[test]
fn a_stale_revision_is_rejected_with_a_receipt_and_changes_nothing() {
    let (_directory, store, session_id) = review_ready_meeting_with_ledger();
    let question = loop_id(session_id, MeetingLoopKind::Loop, "Trial conversion tier");
    store
        .resolve_loop(
            MeetingLoopResolveRequest {
                operation_id: MeetingOperationId::new(),
                loop_id: question.clone(),
                expected_revision: 0,
                resolution: MeetingLoopResolution::Done,
            },
            OperationActor::User,
            NOW,
        )
        .unwrap();

    let stale = store
        .resolve_loop(
            MeetingLoopResolveRequest {
                operation_id: MeetingOperationId::new(),
                loop_id: question.clone(),
                expected_revision: 0,
                resolution: MeetingLoopResolution::Dropped,
            },
            OperationActor::User,
            NOW,
        )
        .unwrap();

    assert_eq!(stale.receipt.result, OperationResult::Rejected);
    assert_eq!(
        stale.receipt.reason_codes,
        vec![MeetingReasonCode::StaleRevision]
    );
    assert_eq!(
        stale.receipt.new_revision,
        Some(1),
        "the refusal reports the revision the caller should have sent"
    );
    let row = row_for(&stale.loops.rows, &question);
    assert_eq!(
        row.status,
        MeetingLoopStatus::Done,
        "the write did not land"
    );
    assert_eq!(row.revision, 1);
}

#[test]
fn replaying_one_operation_id_returns_the_first_receipt() {
    let (_directory, store, session_id) = review_ready_meeting_with_ledger();
    let question = loop_id(session_id, MeetingLoopKind::Loop, "Trial conversion tier");
    let operation_id = MeetingOperationId::new();
    let request = MeetingLoopResolveRequest {
        operation_id,
        loop_id: question.clone(),
        expected_revision: 0,
        resolution: MeetingLoopResolution::Done,
    };

    let first = store
        .resolve_loop(request.clone(), OperationActor::User, NOW)
        .unwrap();
    let replay = store
        .resolve_loop(request, OperationActor::User, NOW)
        .unwrap();

    assert_eq!(first.receipt, replay.receipt);
    assert_eq!(
        row_for(&replay.loops.rows, &question).revision,
        1,
        "a replay does not advance the row"
    );
}

#[test]
fn assigning_an_owner_keeps_the_status_and_names_the_person() {
    let (_directory, store, session_id) = review_ready_meeting_with_ledger();
    let person_id = person(&store, "Dana Reyes");
    let commitment = loop_id(
        session_id,
        MeetingLoopKind::Commitment,
        "Send the tier comparison",
    );
    store
        .resolve_loop(
            MeetingLoopResolveRequest {
                operation_id: MeetingOperationId::new(),
                loop_id: commitment.clone(),
                expected_revision: 0,
                resolution: MeetingLoopResolution::Done,
            },
            OperationActor::User,
            NOW,
        )
        .unwrap();

    let assigned = store
        .assign_loop(
            MeetingLoopAssignRequest {
                operation_id: MeetingOperationId::new(),
                loop_id: commitment.clone(),
                expected_revision: 1,
                owner_person_id: Some(person_id),
            },
            NOW,
        )
        .unwrap();

    assert_eq!(assigned.receipt.command, MeetingCommandKind::LoopAssign);
    let row = row_for(&assigned.loops.rows, &commitment);
    assert_eq!(row.owner_person_id, Some(person_id));
    assert_eq!(row.owner_display_name.as_deref(), Some("Dana Reyes"));
    assert_eq!(
        row.status,
        MeetingLoopStatus::Done,
        "assigning an owner is not a resolution"
    );

    let cleared = store
        .assign_loop(
            MeetingLoopAssignRequest {
                operation_id: MeetingOperationId::new(),
                loop_id: commitment.clone(),
                expected_revision: 2,
                owner_person_id: None,
            },
            NOW,
        )
        .unwrap();
    assert!(row_for(&cleared.loops.rows, &commitment)
        .owner_person_id
        .is_none());
}

#[test]
fn assigning_an_unknown_person_is_refused() {
    let (_directory, store, session_id) = review_ready_meeting_with_ledger();
    let commitment = loop_id(
        session_id,
        MeetingLoopKind::Commitment,
        "Send the tier comparison",
    );

    let error = store
        .assign_loop(
            MeetingLoopAssignRequest {
                operation_id: MeetingOperationId::new(),
                loop_id: commitment,
                expected_revision: 0,
                owner_person_id: Some(crate::meeting::people_types::PersonId::new()),
            },
            NOW,
        )
        .unwrap_err();

    assert_eq!(error, StoreError::NotFound);
}

#[test]
fn a_loop_id_the_current_ledger_does_not_hold_is_not_found() {
    let (_directory, store, session_id) = review_ready_meeting_with_ledger();
    let invented = loop_id(session_id, MeetingLoopKind::Loop, "Never said out loud");

    let error = store
        .resolve_loop(
            MeetingLoopResolveRequest {
                operation_id: MeetingOperationId::new(),
                loop_id: invented,
                expected_revision: 0,
                resolution: MeetingLoopResolution::Done,
            },
            OperationActor::User,
            NOW,
        )
        .unwrap_err();

    assert_eq!(error, StoreError::NotFound);
}

#[test]
fn an_id_from_another_store_is_invalid_rather_than_a_panic() {
    let (_directory, store, _session_id) = review_ready_meeting_with_ledger();

    let error = store
        .resolve_loop(
            MeetingLoopResolveRequest {
                operation_id: MeetingOperationId::new(),
                loop_id: MeetingLoopId("not-a-loop-id".to_string()),
                expected_revision: 0,
                resolution: MeetingLoopResolution::Done,
            },
            OperationActor::User,
            NOW,
        )
        .unwrap_err();

    assert_eq!(error, StoreError::Invalid);
}

#[test]
fn the_ledger_pass_carries_an_open_loop_into_the_next_session_of_a_series() {
    let (_directory, store, first) = review_ready_meeting_with_ledger();
    let second = series_meeting_with_ledger(&store, NOW + WEEK_MS);

    let receipts = store.carry_loops_forward(second).unwrap();

    let earlier = loop_id(first, MeetingLoopKind::Loop, "Trial conversion tier");
    let successor = loop_id(second, MeetingLoopKind::Loop, "Trial conversion tier");
    assert_eq!(
        receipts.len(),
        2,
        "the loop and the commitment both carried"
    );
    let carried = receipts
        .iter()
        .find(|receipt| receipt.effect_ids.first() == Some(&earlier.0))
        .expect("the earlier loop got its own receipt");
    assert_eq!(carried.command, MeetingCommandKind::LoopCarry);
    assert_eq!(carried.result, OperationResult::Committed);
    assert_eq!(carried.session_id, Some(first));
    assert_eq!(carried.effect_ids, vec![earlier.0.clone(), successor.0]);

    let earlier_rows = store.meeting_loops(first).unwrap();
    let earlier_row = row_for(&earlier_rows.rows, &earlier);
    assert_eq!(earlier_row.status, MeetingLoopStatus::Carried);
    assert!(earlier_row.carried_into_loop_id.is_some());

    let later_rows = store.meeting_loops(second).unwrap();
    let later_row = row_for(
        &later_rows.rows,
        &loop_id(second, MeetingLoopKind::Loop, "Trial conversion tier"),
    );
    assert_eq!(later_row.status, MeetingLoopStatus::Open);
    assert_eq!(
        later_row.carried_since_at_utc_ms,
        Some(NOW),
        "the successor knows when the subject was first raised"
    );
}

#[test]
fn a_resolved_loop_is_not_carried_forward() {
    let (_directory, store, first) = review_ready_meeting_with_ledger();
    let earlier = loop_id(first, MeetingLoopKind::Loop, "Trial conversion tier");
    store
        .resolve_loop(
            MeetingLoopResolveRequest {
                operation_id: MeetingOperationId::new(),
                loop_id: earlier.clone(),
                expected_revision: 0,
                resolution: MeetingLoopResolution::Done,
            },
            OperationActor::User,
            NOW,
        )
        .unwrap();
    let second = series_meeting_with_ledger(&store, NOW + WEEK_MS);

    let receipts = store.carry_loops_forward(second).unwrap();

    assert_eq!(
        receipts.len(),
        1,
        "only the commitment was still open to carry"
    );
    assert_eq!(
        row_for(&store.meeting_loops(first).unwrap().rows, &earlier).status,
        MeetingLoopStatus::Done
    );
}

#[test]
fn the_ledger_pass_does_nothing_for_the_first_occurrence() {
    let (_directory, store, first) = review_ready_meeting_with_ledger();

    assert!(
        store.carry_loops_forward(first).unwrap().is_empty(),
        "nothing came before it to carry"
    );
}

#[test]
fn a_meeting_outside_a_series_carries_nothing() {
    let (_directory, store) = store();
    let alone = ledger_meeting(&store, NOW);

    assert!(store.carry_loops_forward(alone).unwrap().is_empty());
}

/// Give this session a microphone speaker with `name` on it, which is how the
/// store knows which voice is the user's.
fn microphone_speaker(store: &MeetingStore, session_id: MeetingSessionId, name: &str) {
    store
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO meeting_speakers (
                speaker_id, session_id, source_kind, display_name, revision
             ) VALUES (?1, ?2, 'microphone', ?3, 0)",
            params![
                Uuid::new_v4().to_string(),
                session_id.uuid().to_string(),
                name
            ],
        )
        .unwrap();
}

/// D27 through the real store. The fixture ledger attributes both rows to
/// "Dana Reyes", so the same two rows read one way or the other depending on
/// nothing but who was on the microphone.
#[test]
fn the_microphone_speaker_decides_which_rows_the_user_owes() {
    let (_directory, store) = store();
    let theirs = ledger_meeting(&store, NOW);
    microphone_speaker(&store, theirs, "Amir");
    let mine = ledger_meeting(&store, NOW);
    microphone_speaker(&store, mine, "Dana Reyes");

    for row in &store.meeting_loops(theirs).unwrap().rows {
        assert!(row.is_waiting_on(), "Dana is not at this Mac: {row:?}");
        assert!(!row.is_mine());
    }
    for row in &store.meeting_loops(mine).unwrap().rows {
        assert!(row.is_mine(), "Dana is the one recording: {row:?}");
        assert!(!row.is_waiting_on());
    }
}

/// A meeting with no microphone speaker row at all still has to answer. It
/// answers "somebody else", which is the safe reading: a row wrongly called
/// mine would go into an after-meeting reminders list as the user's own work.
#[test]
fn a_session_with_no_microphone_speaker_attributes_nothing_to_the_user() {
    let (_directory, store, session_id) = review_ready_meeting_with_ledger();

    for row in &store.meeting_loops(session_id).unwrap().rows {
        assert!(row.is_waiting_on(), "{row:?}");
    }
}

/// Assigning a person is the user saying "not me, them", and it has to win
/// over the microphone. Persons never model the user, so an assignment can
/// only ever mean somebody else.
#[test]
fn assigning_a_person_moves_a_row_the_user_said_off_the_users_own_list() {
    let (_directory, store, session_id) = review_ready_meeting_with_ledger();
    microphone_speaker(&store, session_id, "Dana Reyes");
    let commitment = loop_id(
        session_id,
        MeetingLoopKind::Commitment,
        "Send the tier comparison",
    );
    assert!(row_for(&store.meeting_loops(session_id).unwrap().rows, &commitment).is_mine());
    let amir = person(&store, "Amir");

    let result = store
        .assign_loop(
            MeetingLoopAssignRequest {
                operation_id: MeetingOperationId::new(),
                loop_id: commitment.clone(),
                expected_revision: 0,
                owner_person_id: Some(amir),
            },
            NOW,
        )
        .unwrap();

    let row = row_for(&result.loops.rows, &commitment);
    assert!(row.is_waiting_on());
    assert!(!row.is_mine());
    assert_eq!(row.owner_person_id, Some(amir));
}

/// The corpus walk every cross-meeting reader shares: one entry per meeting
/// that has a current ledger, with the rows already joined to their state.
#[test]
fn the_corpus_walk_reaches_every_meeting_that_has_a_ledger() {
    let (_directory, store) = store();
    let with_ledger = ledger_meeting(&store, NOW);
    let without = meeting(&store, "Nothing generated yet", NOW + 1);

    let walked = super::loops::ledger_rows_in(&store.connection().unwrap()).unwrap();

    assert_eq!(walked.len(), 1, "a meeting with no artifact has no rows");
    assert_eq!(walked[0].session_id, with_ledger);
    assert_eq!(walked[0].at_utc_ms, NOW);
    assert_eq!(walked[0].rows.len(), 2);
    assert!(walked.iter().all(|entry| entry.session_id != without));
}

/// D27 reaching D20's evening sentence. The count is the whole corpus, not
/// the digest's day: a promise nobody kept last month is exactly what the
/// nudge is for, and it must not vanish because the meeting was not today.
#[test]
fn the_digest_counts_rows_others_have_owed_for_over_a_week() {
    let (_directory, store) = store();
    let day_start = NOW - 86_400_000;

    // Nothing in the corpus yet.
    assert_eq!(
        store
            .digest_counts(day_start, NOW)
            .unwrap()
            .waiting_on_stale,
        0
    );

    // A meeting from this morning: both rows are somebody else's, and neither
    // is overdue yet.
    let today = ledger_meeting(&store, day_start + 1);
    microphone_speaker(&store, today, "Amir");
    assert_eq!(
        store
            .digest_counts(day_start, NOW)
            .unwrap()
            .waiting_on_stale,
        0
    );

    // The same two rows, from a month ago.
    let old = ledger_meeting(&store, NOW - 30 * 86_400_000);
    microphone_speaker(&store, old, "Amir");
    assert_eq!(
        store
            .digest_counts(day_start, NOW)
            .unwrap()
            .waiting_on_stale,
        2
    );

    // Closing one takes it out of the count; the user's own backlog was never
    // in it, so putting the user on the microphone empties the count entirely.
    store
        .resolve_loop(
            MeetingLoopResolveRequest {
                operation_id: MeetingOperationId::new(),
                loop_id: loop_id(old, MeetingLoopKind::Commitment, "Send the tier comparison"),
                expected_revision: 0,
                resolution: MeetingLoopResolution::Done,
            },
            OperationActor::User,
            NOW,
        )
        .unwrap();
    assert_eq!(
        store
            .digest_counts(day_start, NOW)
            .unwrap()
            .waiting_on_stale,
        1
    );

    let mine = ledger_meeting(&store, NOW - 30 * 86_400_000);
    microphone_speaker(&store, mine, "Dana Reyes");
    assert_eq!(
        store
            .digest_counts(day_start, NOW)
            .unwrap()
            .waiting_on_stale,
        1,
        "two more open rows, but they are the user's own"
    );
}

/// D26's receipt. Worth an end-to-end call rather than only a unit test of the
/// evidence: a store method that takes a connection and then reaches for
/// another `&self` method deadlocks on the non-reentrant mutex, and the only
/// thing that catches it is somebody actually calling it.
#[test]
fn drafting_a_follow_up_records_one_receipt_naming_its_engine() {
    let (_directory, store) = store();
    let session_id = ledger_meeting(&store, NOW);
    let operation_id = MeetingOperationId::new();

    let receipt = store
        .record_follow_up_draft(operation_id, session_id, "apple-intelligence")
        .unwrap();

    assert_eq!(receipt.command, MeetingCommandKind::FollowUpDraft);
    assert_eq!(receipt.result, OperationResult::Committed);
    assert_eq!(receipt.session_id, Some(session_id));
    // Which engine wrote it is the whole point of recording the event.
    assert_eq!(receipt.effect_ids, vec!["apple-intelligence".to_string()]);
    // A draft reads the record; it does not move the meeting.
    assert_eq!(receipt.new_revision, Some(receipt.expected_revision));

    // The same press twice is one event, and the engine named on it is the
    // one that actually ran.
    let replayed = store
        .record_follow_up_draft(operation_id, session_id, "sona-relay")
        .unwrap();
    assert_eq!(replayed, receipt);
}

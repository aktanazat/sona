//! The wrap through the real store: who was there, where, the plans heard
//! and the promises each way, each with the line that made it.

use super::heard_plans_tests::{engine, local, Named, Scripted};
use super::loops_tests::{file_ledger, series_event};
use super::reel_tests::{confirm, talk};
use super::workflow_core_tests::{link, person, store};
use super::{encode_json, id, MeetingStore, StoreError};
use crate::meeting::context::{ConversationContextUpdate, ConversationKind, ConversationLocation};
use crate::meeting::detection::machine::CalendarEventSummary;
use crate::meeting::heard_plans::read;
use crate::meeting::ledger::{
    LedgerCommitment, LedgerFirmness, LedgerOpenLoop, LedgerReceipt, LedgerReceiptState,
    MeetingLedger,
};
use crate::meeting::loop_types::{
    MeetingLoopId, MeetingLoopKind, MeetingLoopResolution, MeetingLoopResolveRequest,
    MeetingLoopRow,
};
use crate::meeting::people_types::PersonId;
use crate::meeting::types::{
    ArtifactCitation, MeetingOperationId, MeetingOrigin, MeetingSessionId, OperationActor,
    SpeakerId,
};
use crate::meeting::wrap_types::{MeetingWrapPerson, MeetingWrapPlace};
use rusqlite::params;
use std::sync::Arc;
use tempfile::TempDir;

const PLAN_LINE: &str = "Let's do the pricing review with Nolan Thursday at 2 at the office.";

struct PricingReview {
    _directory: TempDir,
    store: Arc<MeetingStore>,
    session_id: MeetingSessionId,
    lines: Vec<ArtifactCitation>,
    stephen: PersonId,
    nolan: PersonId,
}

/// A recorded meeting on an invite set in Room 4B. Stephen's voice is
/// confirmed, Nolan is confirmed from the invite, and Dana is only a guess
/// from the title. The notes hold three promises, one already done, and a
/// question nobody owns.
fn pricing_review() -> Result<PricingReview, StoreError> {
    let (directory, store) = store();
    let me = SpeakerId::new();
    let stephen_voice = SpeakerId::new();
    let nolan_voice = SpeakerId::new();
    let began = local(2026, 11, 2, 10, 0);
    let (session_id, lines) = talk(
        &store,
        began,
        &[
            (me, "Speaker 1", "Thanks for making time, both of you."),
            (stephen_voice, "Speaker 2", PLAN_LINE),
            (nolan_voice, "Speaker 3", "Thursday at 2 works for me."),
            (me, "Speaker 1", "I'll send the churn sheet before then."),
            (
                nolan_voice,
                "Speaker 3",
                "And I'll get you staging access on Monday.",
            ),
            (stephen_voice, "Speaker 2", "Who owns the renewal email?"),
            (
                stephen_voice,
                "Speaker 2",
                "I'll book the room for Thursday.",
            ),
        ],
        true,
    );
    store.connection()?.execute(
        "UPDATE meeting_sessions SET origin_kind = '\"manual\"' WHERE id = ?1",
        params![id(session_id)],
    )?;
    store.remember_calendar_facts(
        session_id,
        &CalendarEventSummary {
            location: Some("Room 4B".to_string()),
            ..series_event(began)
        },
    )?;
    let stephen = person(&store, "Stephen Park", &[], &[]);
    let nolan = person(&store, "Nolan Lee", &[], &[]);
    let dana = person(&store, "Dana Reyes", &[], &[]);
    link(&store, session_id, stephen, "speaker", "confirmed");
    link(&store, session_id, nolan, "calendar", "confirmed");
    link(&store, session_id, dana, "title", "suggested");
    confirm(&store, stephen_voice, stephen);
    let promise = |who: &str, what: &str, speaker: &str, line: &ArtifactCitation, said: &str| {
        LedgerCommitment {
            who: who.to_string(),
            what: what.to_string(),
            firmness: LedgerFirmness::Firm,
            receipt: LedgerReceipt {
                quote: said.to_string(),
                speaker: Some(speaker.to_string()),
                t_ms: line.start_offset_ns / 1_000_000,
                citations: vec![line.clone()],
            },
        }
    };
    file_ledger(
        &store,
        session_id,
        MeetingLedger {
            headline: "Pricing review set for Thursday.".to_string(),
            threads: Vec::new(),
            open_loops: vec![LedgerOpenLoop {
                question: "Who owns the renewal email?".to_string(),
                instead: "The meeting ended.".to_string(),
                at_ms: lines[5].start_offset_ns / 1_000_000,
                citations: vec![lines[5].clone()],
            }],
            commitments: vec![
                promise(
                    "I",
                    "Send the churn sheet before Thursday",
                    "Speaker 1",
                    &lines[3],
                    "I'll send the churn sheet before then.",
                ),
                promise(
                    "Nolan",
                    "Get us staging access on Monday",
                    "Speaker 3",
                    &lines[4],
                    "And I'll get you staging access on Monday.",
                ),
                promise(
                    "Stephen",
                    "Book the room for Thursday",
                    "Speaker 2",
                    &lines[6],
                    "I'll book the room for Thursday.",
                ),
            ],
            stances: Vec::new(),
            caveats: Vec::new(),
            receipts: LedgerReceiptState::Verified,
        },
    );
    store.resolve_loop(
        MeetingLoopResolveRequest {
            operation_id: MeetingOperationId::new(),
            loop_id: MeetingLoopId::derive(
                session_id,
                MeetingLoopKind::Commitment,
                "Book the room for Thursday",
            ),
            expected_revision: 0,
            resolution: MeetingLoopResolution::Done,
        },
        OperationActor::User,
        local(2026, 11, 2, 10, 30),
    )?;
    read(
        &store,
        &engine(&Scripted::answering(&[Named {
            what: "pricing review",
            when: "Thursday at 2",
            people: &["Nolan"],
            place: "the office",
            quote: PLAN_LINE,
            lines: &["L2", "L3"],
        }])),
        session_id,
        local(2026, 11, 2, 11, 0),
    )?;
    Ok(PricingReview {
        _directory: directory,
        store,
        session_id,
        lines,
        stephen,
        nolan,
    })
}

fn said(rows: &[MeetingLoopRow]) -> Vec<(&str, Option<&str>, Option<&str>, &[ArtifactCitation])> {
    rows.iter()
        .map(|row| {
            (
                row.text.as_str(),
                row.owner_text.as_deref(),
                row.quote.as_deref(),
                row.citations.as_slice(),
            )
        })
        .collect()
}

#[test]
fn a_wrap_reads_who_where_plans_and_promises_with_their_lines() {
    let meeting = pricing_review().unwrap();
    let store = &meeting.store;
    let wrap = store.meeting_wrap(meeting.session_id).unwrap();
    assert_eq!(wrap.kind, ConversationKind::Unspecified);
    assert_eq!(
        wrap.place,
        Some(MeetingWrapPlace {
            name: "Room 4B".to_string(),
            from_invite: true,
        })
    );

    let context = store.conversation_context(meeting.session_id).unwrap();
    store
        .update_conversation_context(ConversationContextUpdate {
            session_id: meeting.session_id,
            expected_revision: context.revision,
            kind: ConversationKind::InPerson,
            location: Some(ConversationLocation {
                name: "Blue Bottle Coffee".to_string(),
                latitude: 37.776,
                longitude: -122.423,
            }),
        })
        .unwrap();
    let wrap = store.meeting_wrap(meeting.session_id).unwrap();

    assert_eq!(wrap.kind, ConversationKind::InPerson);
    assert_eq!(
        wrap.place,
        Some(MeetingWrapPlace {
            name: "Blue Bottle Coffee".to_string(),
            from_invite: false,
        })
    );
    assert_eq!(
        wrap.people,
        vec![
            MeetingWrapPerson {
                person_id: meeting.nolan,
                display_name: "Nolan Lee".to_string(),
                first_heard: None,
            },
            MeetingWrapPerson {
                person_id: meeting.stephen,
                display_name: "Stephen Park".to_string(),
                first_heard: Some(meeting.lines[1].clone()),
            },
        ]
    );
    let [plan] = wrap.plans.as_slice() else {
        panic!("one plan heard, got {:?}", wrap.plans);
    };
    assert_eq!(plan.title, "pricing review");
    assert_eq!(plan.quote.as_deref(), Some(PLAN_LINE));
    assert_eq!(
        plan.citation.as_ref().map(|line| line.segment_id),
        Some(meeting.lines[1].segment_id)
    );
    assert_eq!(
        said(&wrap.mine),
        vec![(
            "Send the churn sheet before Thursday",
            Some("I"),
            Some("I'll send the churn sheet before then."),
            &meeting.lines[3..4],
        )]
    );
    assert_eq!(
        said(&wrap.owed),
        vec![(
            "Get us staging access on Monday",
            Some("Nolan"),
            Some("And I'll get you staging access on Monday."),
            &meeting.lines[4..5],
        )]
    );
}

#[test]
fn reading_a_wrap_writes_nothing() {
    let meeting = pricing_review().unwrap();
    let before = meeting.store.connection().unwrap().total_changes();
    meeting.store.meeting_wrap(meeting.session_id).unwrap();
    assert_eq!(meeting.store.connection().unwrap().total_changes(), before);
}

/// An imported file puts every voice on the microphone lane, so there only
/// the name settings hold says which promises are the user's.
#[test]
fn an_imported_meeting_sorts_the_users_promises_by_name() {
    let meeting = pricing_review().unwrap();
    let connection = meeting.store.connection().unwrap();
    connection
        .execute(
            "UPDATE meeting_sessions SET origin_kind = ?1 WHERE id = ?2",
            params![
                encode_json(&MeetingOrigin::Import).unwrap(),
                id(meeting.session_id)
            ],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE meeting_speakers SET display_name = 'Aktan Azat'
              WHERE session_id = ?1 AND display_name = 'Speaker 1'",
            params![id(meeting.session_id)],
        )
        .unwrap();
    drop(connection);
    meeting
        .store
        .set_user_name_source(|| Some("Aktan Azat".to_string()));

    let wrap = meeting.store.meeting_wrap(meeting.session_id).unwrap();

    let texts =
        |rows: &[MeetingLoopRow]| rows.iter().map(|row| row.text.clone()).collect::<Vec<_>>();
    assert_eq!(texts(&wrap.mine), ["Send the churn sheet before Thursday"]);
    assert_eq!(texts(&wrap.owed), ["Get us staging access on Monday"]);
}

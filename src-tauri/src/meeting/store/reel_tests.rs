//! Reels through the real store: the order they play in, who they name, the
//! moments whose audio is gone, and a promise carried between meetings.

use super::loops_tests::{file_ledger, series_event};
use super::workflow_core_tests::{link, meeting, person, store};
use super::{id, MeetingStore};
use crate::meeting::conversation_review::ConversationReel;
use crate::meeting::ledger::{
    LedgerCommitment, LedgerFirmness, LedgerOpenLoop, LedgerReceipt, LedgerReceiptState,
    MeetingLedger,
};
use crate::meeting::loop_types::{MeetingLoopId, MeetingLoopKind};
use crate::meeting::people_types::PersonId;
use crate::meeting::types::{
    ArtifactCitation, MeetingSessionId, SaidPerson, SpeakerId, TranscriptSegmentId,
};
use rusqlite::{params, Params};
use uuid::Uuid;

const NOW: i64 = 1_700_000_000_000;
const WEEK_MS: i64 = 604_800_000;
const TURN_NS: u64 = 2_000_000_000;

/// One recorded talk on this Mac's microphone: each line a two-second turn
/// said by its speaker under the transcript's label. Its audio is kept only
/// when `kept`.
pub(super) fn talk(
    store: &MeetingStore,
    at_utc_ms: i64,
    lines: &[(SpeakerId, &str, &str)],
    kept: bool,
) -> (MeetingSessionId, Vec<ArtifactCitation>) {
    let session_id = meeting(store, "Pricing review", at_utc_ms);
    let plan_id = Uuid::new_v4().to_string();
    let track_id = Uuid::new_v4().to_string();
    let revision_id = Uuid::new_v4().to_string();
    run(
        store,
        "INSERT INTO meeting_run_plans (
            plan_id, session_id, attempt_number, schema_version, consent_id,
            canonical_plan_json, created_at_utc_ms
         ) VALUES (?1, ?2, 1, 1, ?3, '{}', 1)",
        params![plan_id, id(session_id), Uuid::new_v4().to_string()],
    );
    run(
        store,
        "INSERT INTO meeting_source_tracks (
            track_id, session_id, plan_id, source_kind, required, requested,
            descriptor_json, timestamp_bridge_json, health
         ) VALUES (?1, ?2, ?3, 'microphone', 1, 1, '{}', '{}', '\"healthy\"')",
        params![track_id, id(session_id), plan_id],
    );
    run(
        store,
        "INSERT INTO meeting_transcript_revisions (
            transcript_revision_id, session_id, engine_id, destination_json,
            source_set_json, language, state, created_at_utc_ms
         ) VALUES (?1, ?2, 'test', '{}', '[]', 'en', 'complete', 1)",
        params![revision_id, id(session_id)],
    );
    let mut citations = Vec::new();
    for (ordinal, (speaker_id, label, text)) in (0u64..).zip(lines) {
        run(
            store,
            "INSERT OR IGNORE INTO meeting_speakers (
                speaker_id, session_id, source_kind, display_name, revision
             ) VALUES (?1, ?2, 'microphone', ?3, 0)",
            params![id(*speaker_id), id(session_id), label],
        );
        let citation = ArtifactCitation {
            segment_id: TranscriptSegmentId::new(),
            start_offset_ns: ordinal * TURN_NS,
            end_offset_ns: ordinal * TURN_NS + TURN_NS / 2,
        };
        run(
            store,
            "INSERT INTO meeting_transcript_segments (
                segment_id, transcript_revision_id, track_id, ordinal,
                start_offset_ns, end_offset_ns, speaker_id, base_text
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id(citation.segment_id),
                revision_id,
                track_id,
                ordinal,
                citation.start_offset_ns,
                citation.end_offset_ns,
                id(*speaker_id),
                text
            ],
        );
        citations.push(citation);
    }
    run(
        store,
        "UPDATE meeting_sessions SET current_transcript_revision_id = ?1 WHERE id = ?2",
        params![revision_id, id(session_id)],
    );
    if kept {
        let recorded_ns = citations
            .last()
            .map_or(0, |last| last.start_offset_ns + TURN_NS);
        run(
            store,
            "INSERT INTO meeting_track_records (
                track_id, source_sequence, source_epoch, start_offset_ns, duration_ns,
                frame_count, record_offset_bytes, record_bytes, durable_at_utc_ms
             ) VALUES (?1, 0, 0, 0, ?2, 1, 0, 1, 1)",
            params![track_id, recorded_ns],
        );
    }
    (session_id, citations)
}

/// One fixture statement, on a connection of its own.
fn run(store: &MeetingStore, sql: &str, params: impl Params) {
    // PANIC: a fixture; a setup that fails should fail the test that asked.
    store.connection().unwrap().execute(sql, params).unwrap();
}

pub(super) fn confirm(store: &MeetingStore, speaker_id: SpeakerId, person_id: PersonId) {
    run(
        store,
        "INSERT INTO meeting_voice_speaker_resolutions (
            operation_id, speaker_id, resolution_kind, resolved_person_id, is_current
         ) VALUES (?1, ?2, 'identified', ?3, 1)",
        params![Uuid::new_v4().to_string(), id(speaker_id), id(person_id)],
    );
}

fn ledger(commitments: Vec<LedgerCommitment>, open_loops: Vec<LedgerOpenLoop>) -> MeetingLedger {
    MeetingLedger {
        headline: "Pricing stayed open.".to_string(),
        threads: Vec::new(),
        open_loops,
        commitments,
        stances: Vec::new(),
        caveats: Vec::new(),
        receipts: LedgerReceiptState::Verified,
    }
}

fn promise(who: &str, what: &str, line: &ArtifactCitation) -> LedgerCommitment {
    LedgerCommitment {
        who: who.to_string(),
        what: what.to_string(),
        firmness: LedgerFirmness::Firm,
        receipt: LedgerReceipt {
            quote: what.to_lowercase(),
            speaker: None,
            t_ms: line.start_offset_ns / 1_000_000,
            citations: vec![line.clone()],
        },
    }
}

fn question(text: &str, line: &ArtifactCitation) -> LedgerOpenLoop {
    LedgerOpenLoop {
        question: text.to_string(),
        instead: "The meeting moved on to billing.".to_string(),
        at_ms: line.start_offset_ns / 1_000_000,
        citations: vec![line.clone()],
    }
}

/// Which meeting each moment comes from, and where in it, in playing order.
fn order(reel: &ConversationReel) -> Vec<(MeetingSessionId, u64)> {
    reel.moments
        .iter()
        .map(|moment| (moment.said.session_id, moment.said.start_offset_ns))
        .collect()
}

#[test]
fn a_person_reel_plays_oldest_first_across_meetings() {
    let (_directory, store) = store();
    let ada = person(&store, "Ada Lovelace", &[], &[]);
    let (later, later_lines) = talk(
        &store,
        NOW + WEEK_MS,
        &[
            (
                SpeakerId::new(),
                "Speaker 1",
                "Is there an annual discount?",
            ),
            (
                SpeakerId::new(),
                "Speaker 2",
                "I'll confirm the trial tier by Friday.",
            ),
        ],
        true,
    );
    let (earlier, earlier_lines) = talk(
        &store,
        NOW,
        &[(
            SpeakerId::new(),
            "Speaker 2",
            "I'll send the tier comparison.",
        )],
        true,
    );
    file_ledger(
        &store,
        later,
        ledger(
            vec![promise(
                "Ada Lovelace",
                "Confirm the trial tier",
                &later_lines[1],
            )],
            vec![question("Annual discount", &later_lines[0])],
        ),
    );
    file_ledger(
        &store,
        earlier,
        ledger(
            vec![promise(
                "Ada Lovelace",
                "Send the tier comparison",
                &earlier_lines[0],
            )],
            Vec::new(),
        ),
    );
    link(&store, later, ada, "calendar", "confirmed");
    link(&store, earlier, ada, "calendar", "confirmed");

    let reel = store.conversation_reel_for_person(ada, None).unwrap();

    assert_eq!(
        order(&reel),
        vec![(earlier, 0), (later, 0), (later, TURN_NS)],
        "the earlier meeting first, and each meeting's lines in the order they were said"
    );
}

#[test]
fn a_speaker_nobody_confirmed_is_never_named() {
    let (_directory, store) = store();
    let ada = person(&store, "Ada Lovelace", &[], &[]);
    let confirmed = SpeakerId::new();
    let guessed = SpeakerId::new();
    let (session_id, lines) = talk(
        &store,
        NOW,
        &[
            (confirmed, "Speaker 1", "I'll send the tier comparison."),
            (guessed, "Ada Lovelace", "I'll check the annual discount."),
        ],
        true,
    );
    confirm(&store, confirmed, ada);
    file_ledger(
        &store,
        session_id,
        ledger(
            vec![
                promise("Ada Lovelace", "Send the tier comparison", &lines[0]),
                promise("Ada Lovelace", "Check the annual discount", &lines[1]),
            ],
            Vec::new(),
        ),
    );
    link(&store, session_id, ada, "speaker", "confirmed");

    let reel = store.conversation_reel_for_person(ada, None).unwrap();

    assert_eq!(reel.moments.len(), 2, "{reel:?}");
    assert_eq!(
        reel.moments[0].said.person,
        Some(SaidPerson {
            person_id: ada,
            display_name: "Ada Lovelace".to_string()
        })
    );
    assert_eq!(
        (
            reel.moments[1].said.person.as_ref(),
            reel.moments[1].said.speaker_label.as_deref()
        ),
        (None, None),
        "a label automatic matching wrote is not a confirmed name"
    );
}

#[test]
fn a_moment_whose_audio_is_gone_stays_listed_and_is_marked() {
    let (_directory, store) = store();
    let ada = person(&store, "Ada Lovelace", &[], &[]);
    let (gone, gone_lines) = talk(
        &store,
        NOW,
        &[(
            SpeakerId::new(),
            "Speaker 1",
            "I'll send the tier comparison.",
        )],
        false,
    );
    let (kept, kept_lines) = talk(
        &store,
        NOW + WEEK_MS,
        &[(
            SpeakerId::new(),
            "Speaker 1",
            "I'll confirm the trial tier.",
        )],
        true,
    );
    file_ledger(
        &store,
        gone,
        ledger(
            vec![promise(
                "Ada Lovelace",
                "Send the tier comparison",
                &gone_lines[0],
            )],
            Vec::new(),
        ),
    );
    file_ledger(
        &store,
        kept,
        ledger(
            vec![promise(
                "Ada Lovelace",
                "Confirm the trial tier",
                &kept_lines[0],
            )],
            Vec::new(),
        ),
    );
    link(&store, gone, ada, "calendar", "confirmed");
    link(&store, kept, ada, "calendar", "confirmed");

    let reel = store.conversation_reel_for_person(ada, None).unwrap();

    let audio: Vec<_> = reel
        .moments
        .iter()
        .map(|moment| (moment.said.session_id, moment.said.audio_retained))
        .collect();
    assert_eq!(audio, vec![(gone, false), (kept, true)]);
}

#[test]
fn a_promise_reel_follows_it_from_meeting_to_meeting() {
    let (_directory, store) = store();
    let (first, first_lines) = talk(
        &store,
        NOW,
        &[(SpeakerId::new(), "Dana", "I'll send the tier comparison.")],
        true,
    );
    let (second, second_lines) = talk(
        &store,
        NOW + WEEK_MS,
        &[
            (SpeakerId::new(), "Amir", "Where are we on pricing?"),
            (
                SpeakerId::new(),
                "Dana",
                "The tier comparison is still on me.",
            ),
        ],
        true,
    );
    for (session_id, line, at_utc_ms) in [
        (first, &first_lines[0], NOW),
        (second, &second_lines[1], NOW + WEEK_MS),
    ] {
        file_ledger(
            &store,
            session_id,
            ledger(
                vec![promise("Dana Reyes", "Send the tier comparison", line)],
                Vec::new(),
            ),
        );
        store
            .remember_calendar_facts(session_id, &series_event(at_utc_ms))
            .unwrap();
    }
    store.carry_loops_forward(second).unwrap();
    let promise_in = |session_id| {
        MeetingLoopId::derive(
            session_id,
            MeetingLoopKind::Commitment,
            "Send the tier comparison",
        )
    };

    let from_latest = store
        .conversation_reel_for_loop(&promise_in(second), None)
        .unwrap();
    let from_first = store
        .conversation_reel_for_loop(&promise_in(first), None)
        .unwrap();

    assert_eq!(order(&from_latest), vec![(first, 0), (second, TURN_NS)]);
    assert_eq!(
        from_first, from_latest,
        "either end of the chain plays the whole of it"
    );
}

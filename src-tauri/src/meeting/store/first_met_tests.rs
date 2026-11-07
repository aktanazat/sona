//! When the user first met a person, through the real store: the earliest
//! confirmed meeting, its place, and who the lines say introduced them.

use super::reel_tests::{confirm, talk};
use super::workflow_core_tests::{link, person, store};
use super::{id, MeetingStore};
use crate::meeting::context::{ConversationContextUpdate, ConversationKind, ConversationLocation};
use crate::meeting::evening::FilingTarget;
use crate::meeting::people_types::{PersonFirstMet, PersonIntroduction};
use crate::meeting::types::{ArtifactCitation, MeetingSessionId, SpeakerId};
use rusqlite::params;

const NOW: i64 = 1_700_000_000_000;
const WEEK_MS: i64 = 604_800_000;

/// A talk on this Mac's microphone that reads as a real recording's
/// context: its origin is stored the way a manual start stores it.
fn conversation(
    store: &MeetingStore,
    at_utc_ms: i64,
    lines: &[(SpeakerId, &str, &str)],
) -> (MeetingSessionId, Vec<ArtifactCitation>) {
    let (session_id, citations) = talk(store, at_utc_ms, lines, false);
    // PANIC: a fixture; a setup that fails should fail the test that asked.
    store
        .connection()
        .unwrap()
        .execute(
            "UPDATE meeting_sessions SET origin_kind = '\"manual\"' WHERE id = ?1",
            params![id(session_id)],
        )
        .unwrap();
    (session_id, citations)
}

fn place(store: &MeetingStore, session_id: MeetingSessionId, name: &str) {
    // PANIC: a fixture; a setup that fails should fail the test that asked.
    let current = store.conversation_context(session_id).unwrap();
    // PANIC: a fixture; a setup that fails should fail the test that asked.
    store
        .update_conversation_context(ConversationContextUpdate {
            session_id,
            expected_revision: current.revision,
            kind: ConversationKind::InPerson,
            location: Some(ConversationLocation {
                name: name.to_string(),
                latitude: 37.7946,
                longitude: -122.3954,
            }),
        })
        .unwrap();
}

fn introduction(first_met: &PersonFirstMet) -> &PersonIntroduction {
    // PANIC: a missing introduction fails the test that expected one.
    first_met.introduction.as_ref().unwrap()
}

/* Marat introduces Azamat in the lobby. Azamat naming himself and the user
 * introducing Azamat to someone else come first in the transcript, and
 * neither makes an introducer: the maybe names Marat, plays Marat's line,
 * and is confirmed only once taken. A meeting only suggested earlier, and a
 * later one, are not when they first met. */
#[test]
fn an_introduction_is_offered_as_the_speaker_who_made_it() {
    let (_directory, store) = store();
    let azamat = person(&store, "Azamat Bekov", &[], &[]);
    let marat = person(&store, "Marat Ibraev", &[], &[]);
    let (me, azamat_voice, marat_voice) = (SpeakerId::new(), SpeakerId::new(), SpeakerId::new());
    let (lobby, lines) = conversation(
        &store,
        NOW,
        &[
            (azamat_voice, "Speaker 2", "Hi, this is Azamat, sorry I'm late."),
            (me, "Speaker 1", "Dana, this is Azamat by the way."),
            (
                marat_voice,
                "Speaker 3",
                "Aktan, I want you to meet my friend Azamat. He runs the freight startup I told you about.",
            ),
            (me, "Speaker 1", "Nice to meet you, Azamat."),
        ],
    );
    confirm(&store, azamat_voice, azamat);
    confirm(&store, marat_voice, marat);
    link(&store, lobby, azamat, "speaker", "confirmed");
    link(&store, lobby, marat, "speaker", "confirmed");
    place(&store, lobby, "Hyatt Regency lobby");
    let (suggested, _) = conversation(&store, NOW - WEEK_MS, &[]);
    link(&store, suggested, azamat, "calendar", "suggested");
    let (later, _) = conversation(&store, NOW + WEEK_MS, &[]);
    link(&store, later, azamat, "manual", "confirmed");

    let first_met = store.person_first_met(azamat, NOW).unwrap().unwrap();

    assert_eq!((first_met.meeting_id, first_met.at_utc_ms), (lobby, NOW));
    assert_eq!(first_met.place.as_deref(), Some("Hyatt Regency lobby"));
    let maybe = introduction(&first_met);
    assert_eq!(
        (
            maybe.introducer_name.as_str(),
            maybe.quote.as_str(),
            maybe.segment_id,
            maybe.confirmed
        ),
        (
            "Marat Ibraev",
            "Aktan, I want you to meet my friend Azamat. He runs the freight startup I told you about.",
            Some(lines[2].segment_id),
            false
        )
    );
    assert!(!store
        .context_suggestions(lobby, NOW)
        .unwrap()
        .iter()
        .any(|suggestion| matches!(suggestion.target, FilingTarget::Introducer { .. })));

    store.accept_filing(&maybe.suggestion_id, NOW).unwrap();

    let taken = store.person_first_met(azamat, NOW).unwrap().unwrap();
    assert_eq!(
        (
            introduction(&taken).introducer_name.as_str(),
            introduction(&taken).confirmed
        ),
        ("Marat Ibraev", true)
    );
}

/* Lines that name Azamat without introducing him leave the first meeting
 * with its date and place only. */
#[test]
fn a_meeting_without_an_introduction_gives_the_date_and_place() {
    let (_directory, store) = store();
    let azamat = person(&store, "Azamat Bekov", &[], &[]);
    let marat = person(&store, "Marat Ibraev", &[], &[]);
    let marat_voice = SpeakerId::new();
    let (lobby, _) = conversation(
        &store,
        NOW,
        &[
            (
                marat_voice,
                "Speaker 2",
                "This is the deck Azamat sent over.",
            ),
            (
                marat_voice,
                "Speaker 2",
                "This is Azamat's number, call him tomorrow.",
            ),
            (
                SpeakerId::new(),
                "Speaker 1",
                "Did Azamat agree to the pilot?",
            ),
        ],
    );
    confirm(&store, marat_voice, marat);
    link(&store, lobby, azamat, "speaker", "confirmed");
    place(&store, lobby, "Hyatt Regency lobby");

    assert_eq!(
        store.person_first_met(azamat, NOW).unwrap(),
        Some(PersonFirstMet {
            meeting_id: lobby,
            at_utc_ms: NOW,
            place: Some("Hyatt Regency lobby".to_string()),
            introduction: None,
        })
    );
}

/* A person no meeting confirms has no first meeting, even when a meeting
 * they are only suggested in holds a confirmed voice introducing them, and
 * nothing is offered on their behalf. */
#[test]
fn an_unconfirmed_person_gets_nothing() {
    let (_directory, store) = store();
    let azamat = person(&store, "Azamat Bekov", &[], &[]);
    let marat = person(&store, "Marat Ibraev", &[], &[]);
    let nobody = person(&store, "Dana Whitfield", &[], &[]);
    let marat_voice = SpeakerId::new();
    let (lobby, _) = conversation(
        &store,
        NOW,
        &[(marat_voice, "Speaker 2", "Aktan, this is Azamat.")],
    );
    confirm(&store, marat_voice, marat);
    link(&store, lobby, azamat, "speaker", "suggested");

    assert_eq!(store.person_first_met(azamat, NOW).unwrap(), None);
    assert_eq!(store.person_first_met(nobody, NOW).unwrap(), None);
    let offered: i64 = store
        .connection()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM filing_suggestions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(offered, 0);
}

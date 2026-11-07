use super::evening::{StoredEveningLines, StrayEvidence};
use super::voice_identity::VoiceSpeakerSuggestion;
use super::workflow_core_tests::{meeting, person, store};
use super::MeetingStore;
use crate::meeting::evening::{EveningLines, FilingTarget};
use crate::meeting::types::{MeetingSessionId, SpeakerId, MANUAL_DEFAULT_TITLE};
use rusqlite::params;

const NOW: i64 = 1_700_000_000_000;

fn session_event(store: &MeetingStore, session_id: MeetingSessionId, sequence: i64, kind: &str) {
    // PANIC: a fixture that cannot write its event is a broken test.
    store
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO meeting_session_events (
                session_id, sequence, prior_phase, next_phase, event_kind, observed_at_utc_ms,
                details_json
             ) VALUES (?1, ?2, 'review_ready', 'review_ready', ?3, ?4, '{}')",
            params![session_id.uuid().to_string(), sequence, kind, NOW],
        )
        .unwrap();
}

#[test]
fn only_recordings_sona_titled_itself_with_no_event_are_strays() {
    let (_directory, store) = store();
    let untitled = meeting(&store, MANUAL_DEFAULT_TITLE, NOW);
    let derived = meeting(&store, "Coffee about the launch", NOW + 1);
    session_event(&store, derived, 1, "title_derived");
    meeting(&store, "Board prep", NOW + 2);
    let renamed = meeting(&store, "Dinner with Stephen", NOW + 3);
    session_event(&store, renamed, 1, "title_derived");
    session_event(&store, renamed, 2, "title_set");
    let on_calendar = meeting(&store, MANUAL_DEFAULT_TITLE, NOW + 4);
    store
        .connection()
        .unwrap()
        .execute(
            "INSERT INTO meeting_calendar_facts(session_id, event_key, event_json)
             VALUES (?1, 'event', '{}')",
            params![on_calendar.uuid().to_string()],
        )
        .unwrap();

    let strays = store
        .stray_candidates(0)
        .unwrap()
        .into_iter()
        .map(|stray| stray.session_id)
        .collect::<Vec<_>>();

    assert_eq!(strays, vec![untitled, derived]);
}

#[test]
fn a_review_suggests_a_voice_match_without_linking_the_person() {
    let (_directory, store) = store();
    let stray = meeting(&store, MANUAL_DEFAULT_TITLE, NOW);
    let stephen = person(&store, "Stephen Park", &[], &[]);
    let voices = [VoiceSpeakerSuggestion {
        speaker_id: SpeakerId::new(),
        person_id: Some(stephen),
        display_name: "Stephen Park".to_string(),
    }];
    let candidate = store.stray_candidates(0).unwrap().remove(0);

    store
        .record_stray_review(
            &candidate,
            &StrayEvidence {
                events: &[],
                voices: &voices,
                projects: &[],
            },
            NOW,
        )
        .unwrap();

    let suggested = store
        .unfiled_recordings()
        .unwrap()
        .into_iter()
        .flat_map(|recording| recording.suggestions)
        .map(|suggestion| suggestion.target)
        .collect::<Vec<_>>();
    assert_eq!(
        suggested,
        vec![FilingTarget::Person {
            person_id: stephen,
            name: "Stephen Park".to_string(),
        }]
    );
    let links: i64 = store
        .connection()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM meeting_person_links WHERE meeting_id = ?1",
            params![stray.uuid().to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(links, 0);
}

#[test]
fn lines_written_from_a_deleted_meeting_are_forgotten() {
    let (_directory, store) = store();
    let pricing = meeting(&store, "Pricing review", NOW);
    store
        .save_evening_lines(
            "2023-11-14",
            &StoredEveningLines {
                lines: EveningLines {
                    lines: vec!["You talked with Stephen about pricing.".to_string()],
                    written_at_utc_ms: NOW,
                    model_id: "local".to_string(),
                },
                meeting_ids: vec![pricing],
                dictation_ids: Vec::new(),
            },
        )
        .unwrap();
    store
        .connection()
        .unwrap()
        .execute(
            "DELETE FROM meeting_sessions WHERE id = ?1",
            params![pricing.uuid().to_string()],
        )
        .unwrap();

    assert!(store.evening_lines("2023-11-14").unwrap().is_none());
}

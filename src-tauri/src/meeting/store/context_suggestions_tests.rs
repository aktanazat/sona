use super::workflow_core_tests::{link, person, store};
use super::{CalendarEventSource, MeetingStore, StoreError};
use crate::meeting::context::{ConversationContextUpdate, ConversationKind, ConversationLocation};
use crate::meeting::detection::machine::CalendarEventSummary;
use crate::meeting::evening::{FilingReason, FilingSuggestion, FilingTarget};
use crate::meeting::types::MeetingSessionId;
use rusqlite::params;
use std::collections::HashMap;
use uuid::Uuid;

const NOW: i64 = 1_700_000_000_000;
const ZOOM: &str = "us.zoom.xos";

/// A finished recording that froze `apps` into its preflight, with one line
/// for each entry in `heard`, said by its own voice on a track of that source
/// kind.
fn recording(
    store: &MeetingStore,
    apps: &[&str],
    heard: &[&str],
) -> Result<MeetingSessionId, StoreError> {
    let session_id = MeetingSessionId::new();
    let session = session_id.uuid().to_string();
    let plan = Uuid::new_v4().to_string();
    let revision = Uuid::new_v4().to_string();
    let preflight = serde_json::json!({ "frozen_system_audio_application_bundle_ids": apps });
    let connection = store.connection()?;
    connection.execute(
        "INSERT INTO meeting_sessions (
            id, phase, revision, title, origin_kind, preflight_json,
            created_at_utc_ms, started_at_utc_ms, processing_status,
            retention_policy_json
         ) VALUES (?1, 'review_ready', 0, 'Meeting notes', '\"manual\"', ?2, ?3, ?3,
                   '{\"kind\":\"succeeded\"}', '{\"kind\":\"forever\"}')",
        params![session, preflight.to_string(), NOW],
    )?;
    connection.execute(
        "INSERT INTO meeting_run_plans (
            plan_id, session_id, attempt_number, schema_version, consent_id,
            canonical_plan_json, created_at_utc_ms
         ) VALUES (?1, ?2, 1, 1, ?3, '{}', 1)",
        params![plan, session, Uuid::new_v4().to_string()],
    )?;
    connection.execute(
        "INSERT INTO meeting_transcript_revisions (
            transcript_revision_id, session_id, engine_id, destination_json,
            source_set_json, language, state, created_at_utc_ms
         ) VALUES (?1, ?2, 'test', '{}', '[]', 'en', 'complete', 1)",
        params![revision, session],
    )?;
    let mut tracks = HashMap::new();
    for (ordinal, &source_kind) in (0u64..).zip(heard) {
        let track = match tracks.get(source_kind) {
            Some(track) => String::clone(track),
            None => {
                let track = Uuid::new_v4().to_string();
                connection.execute(
                    "INSERT INTO meeting_source_tracks (
                        track_id, session_id, plan_id, source_kind, required, requested,
                        descriptor_json, timestamp_bridge_json, health
                     ) VALUES (?1, ?2, ?3, ?4, 1, 1, '{}', '{}', '\"healthy\"')",
                    params![track, session, plan, source_kind],
                )?;
                tracks.insert(source_kind, track.clone());
                track
            }
        };
        let speaker = Uuid::new_v4().to_string();
        connection.execute(
            "INSERT INTO meeting_speakers (
                speaker_id, session_id, source_kind, display_name, revision
             ) VALUES (?1, ?2, ?3, ?4, 0)",
            params![speaker, session, source_kind, format!("Speaker {ordinal}")],
        )?;
        connection.execute(
            "INSERT INTO meeting_transcript_segments (
                segment_id, transcript_revision_id, track_id, ordinal,
                start_offset_ns, end_offset_ns, speaker_id, base_text
             ) VALUES (?1, ?2, ?3, ?4, 0, 1000000000, ?5, 'Shall we start?')",
            params![
                Uuid::new_v4().to_string(),
                revision,
                track,
                ordinal,
                speaker
            ],
        )?;
    }
    connection.execute(
        "UPDATE meeting_sessions SET current_transcript_revision_id = ?1 WHERE id = ?2",
        params![revision, session],
    )?;
    Ok(session_id)
}

fn offers(suggestions: &[FilingSuggestion]) -> Vec<(FilingTarget, Vec<FilingReason>)> {
    suggestions
        .iter()
        .map(|suggestion| (suggestion.target.clone(), suggestion.reasons.clone()))
        .collect()
}

/* Two voices only the microphone heard, with no call app, are offered as in
 * person, and the kind changes only when the operator takes the offer. One
 * voice alone may be a memo and is offered nothing. */
#[test]
fn two_voices_only_the_microphone_heard_are_offered_as_in_person() {
    let (_directory, store) = store();
    let memo = recording(&store, &[], &["microphone"]).unwrap();
    let session_id = recording(&store, &[], &["microphone", "microphone"]).unwrap();

    assert!(store
        .context_suggestions(memo, &[], None, NOW)
        .unwrap()
        .is_empty());

    let offered = store
        .context_suggestions(session_id, &[], None, NOW)
        .unwrap();

    assert_eq!(
        offers(&offered),
        [(
            FilingTarget::Kind {
                conversation_kind: ConversationKind::InPerson
            },
            vec![FilingReason::OnlyMicrophone]
        )]
    );
    assert_eq!(
        store.conversation_context(session_id).unwrap().kind,
        ConversationKind::Unspecified
    );

    store
        .file_context(&offered[0].id, session_id, &offered[0].target, NOW)
        .unwrap();

    assert_eq!(
        store.conversation_context(session_id).unwrap().kind,
        ConversationKind::InPerson
    );
    assert!(store
        .context_suggestions(session_id, &[], None, NOW)
        .unwrap()
        .is_empty());
}

/* Voices through a recorded call app make an online call. A call app whose
 * audio carried no voice is never read as in person, even when two voices
 * spoke into the microphone. */
#[test]
fn voices_through_a_recorded_call_app_are_offered_as_an_online_call() {
    let (_directory, store) = store();
    let call = recording(&store, &[ZOOM], &["microphone", "system_audio"]).unwrap();
    let quiet_call = recording(&store, &[ZOOM], &["microphone", "microphone"]).unwrap();

    assert_eq!(
        offers(&store.context_suggestions(call, &[], None, NOW).unwrap()),
        [(
            FilingTarget::Kind {
                conversation_kind: ConversationKind::OnlineCall
            },
            vec![FilingReason::AppAudio {
                bundle_id: ZOOM.to_string()
            }]
        )]
    );
    assert!(store
        .context_suggestions(quiet_call, &[], None, NOW)
        .unwrap()
        .is_empty());
}

/* The pin on the recording's invite is offered as its place, and the
 * recording names a place only when the operator takes the offer. */
#[test]
fn an_invite_pin_is_offered_as_the_place() {
    let (_directory, store) = store();
    let session_id = recording(&store, &[], &[]).unwrap();
    let cafe = ConversationLocation {
        name: "Blue Bottle Coffee".to_string(),
        latitude: 37.7764,
        longitude: -122.4236,
    };
    store
        .remember_calendar_event(
            session_id,
            &CalendarEventSummary {
                event_key: "coffee#1".to_string(),
                series_key: "coffee".to_string(),
                title: "Coffee with Dana".to_string(),
                attendee_count: 2,
                start_utc_ms: NOW,
                end_utc_ms: NOW + 1_800_000,
                attendees: Vec::new(),
                notes: None,
                calendar_name: Some("Personal".to_string()),
                location: Some("Blue Bottle Coffee\n315 Linden St, San Francisco".to_string()),
                url: None,
                place: Some(cafe.clone()),
            },
            CalendarEventSource::Chosen,
        )
        .unwrap();

    let offered = store
        .context_suggestions(session_id, &[], None, NOW)
        .unwrap();

    assert_eq!(
        offers(&offered),
        [(
            FilingTarget::Place {
                location: cafe.clone()
            },
            vec![FilingReason::InInvite]
        )]
    );
    assert_eq!(
        store.conversation_context(session_id).unwrap().location,
        None
    );

    store
        .file_context(&offered[0].id, session_id, &offered[0].target, NOW)
        .unwrap();

    assert_eq!(
        store.conversation_context(session_id).unwrap().location,
        Some(cafe)
    );
    assert!(store
        .context_suggestions(session_id, &[], None, NOW)
        .unwrap()
        .is_empty());
}

/* An earlier meeting within 100 m of the place he says he is at was here
 * before, named by the people confirmed in it; a maybe names no one. One
 * 150 m off was somewhere else. */
#[test]
fn here_before_is_a_meeting_within_100_m_named_by_its_people() {
    let (_directory, store) = store();
    let at = |session_id, meters_north: f64| {
        let current = store.conversation_context(session_id).unwrap();
        store
            .update_conversation_context(ConversationContextUpdate {
                session_id,
                expected_revision: current.revision,
                kind: current.kind,
                location: Some(ConversationLocation {
                    name: "Hyatt Regency San Francisco".to_string(),
                    latitude: 37.79425 + meters_north / 111_195.0,
                    longitude: -122.39556,
                }),
            })
            .unwrap();
    };
    let lobby = recording(&store, &[], &[]).unwrap();
    let before = recording(&store, &[], &[]).unwrap();
    let across_the_street = recording(&store, &[], &[]).unwrap();
    at(lobby, 0.0);
    at(before, 90.0);
    at(across_the_street, 150.0);
    let dana = person(&store, "Dana Reyes", &[], &[]);
    let ana = person(&store, "Ana Ito", &[], &[]);
    link(&store, before, dana, "speaker", "confirmed");
    link(&store, before, ana, "calendar", "suggested");

    let here_before = store.conversation_place_review(lobby).unwrap().sessions;

    assert_eq!(
        here_before
            .iter()
            .map(|session| (session.session_id, session.people.clone()))
            .collect::<Vec<_>>(),
        [(before, vec!["Dana Reyes".to_string()])]
    );
}

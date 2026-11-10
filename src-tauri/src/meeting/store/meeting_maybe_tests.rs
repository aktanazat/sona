use super::evening::confirmed_people_in;
use super::heard_plans_tests::{engine, local, Named, Scripted};
use super::loops_tests::{file_ledger, series_event};
use super::meeting_maybe::HeardVoice;
use super::people::people_revision_in;
use super::recall::RecallMemory;
use super::voice_identity::{
    MeetingSpeakerIdentifyDisposition, MeetingSpeakerIdentifyRequest, MeetingSpeakerIdentifyTarget,
};
use super::workflow_core_tests::{link, person, reviewable_meeting, store, transcript_segments};
use super::{id, MeetingStore, StoreError};
use crate::meeting::call_speakers::{CallObservation, CallParticipant};
use crate::meeting::context::{ConversationContextUpdate, ConversationKind, ConversationLocation};
use crate::meeting::diarization::{
    wespeaker_embedding_model_key, SpeakerEmbedding, SPEAKER_EMBEDDING_DIMENSIONS,
};
use crate::meeting::evening::{FilingPerson, FilingReason, FilingSuggestion, FilingTarget};
use crate::meeting::heard_plan_types::{HeardPlan, HeardPlanStatus};
use crate::meeting::heard_plans::read;
use crate::meeting::ledger::{
    LedgerCommitment, LedgerFirmness, LedgerOpenLoop, LedgerReceipt, LedgerReceiptState,
    LedgerThread, LedgerThreadState, MeetingLedger,
};
use crate::meeting::people_types::PersonId;
use crate::meeting::types::{
    MeetingOperationId, MeetingSessionId, SpeakerId, MANUAL_DEFAULT_TITLE,
};
use rusqlite::params;
use std::collections::HashSet;
use uuid::Uuid;

/// What was said on a call, a second apart from 18:00 on Wednesday
/// 4 November 2026.
const SAID: [&str; 3] = [
    "Dana, are we still on for coffee?",
    "Yes, let's meet at the Hyatt lobby tomorrow at 10.",
    "Great, see you there.",
];

/// The coffee planned in L1 and L2.
const COFFEE: Named = Named {
    what: "coffee with Dana",
    when: "tomorrow at 10",
    people: &["Dana"],
    place: "the Hyatt lobby",
    quote: SAID[1],
    lines: &["L1", "L2"],
};

/// Dana's voice in these tests.
const DANA_AXIS: usize = 3;

/// 41 seconds into the recording.
const AT_0_41: u64 = 41_000_000_000;

fn hyatt() -> ConversationLocation {
    ConversationLocation {
        name: "Hyatt Regency Sacramento".to_string(),
        latitude: 38.5793,
        longitude: -121.4979,
    }
}

fn voice(axis: usize) -> Result<SpeakerEmbedding, StoreError> {
    let mut values = [0.0_f32; SPEAKER_EMBEDDING_DIMENSIONS];
    values[axis] = 1.0;
    SpeakerEmbedding::from_normalized_slice(&values).ok_or(StoreError::VoiceInvariant)
}

fn heard(axis: usize) -> Result<HeardVoice, StoreError> {
    Ok(HeardVoice {
        voice: voice(axis)?,
        first_heard_ns: AT_0_41,
    })
}

/// The call the coffee was planned on, read for its plans, and the plan.
fn call(store: &MeetingStore) -> Result<(MeetingSessionId, HeardPlan), StoreError> {
    let call = reviewable_meeting(store, "Call with Dana", local(2026, 11, 4, 18, 0));
    transcript_segments(store, call, &SAID);
    read(
        store,
        &engine(&Scripted::answering(&[COFFEE])),
        call,
        local(2026, 11, 4, 19, 0),
    )?;
    let plan = store.heard_plans(call)?.pop().ok_or(StoreError::NotFound)?;
    Ok((call, plan))
}

/// A breakfast two weeks before, saved at the Hyatt.
fn breakfast_at_the_hyatt(store: &MeetingStore) -> Result<(), StoreError> {
    let breakfast = reviewable_meeting(store, "Breakfast with Sam", local(2026, 10, 21, 8, 30));
    store.connection()?.execute(
        "UPDATE meeting_sessions SET origin_kind = '\"manual\"' WHERE id = ?1",
        params![id(breakfast)],
    )?;
    store.update_conversation_context(ConversationContextUpdate {
        session_id: breakfast,
        expected_revision: store.conversation_context(breakfast)?.revision,
        kind: ConversationKind::InPerson,
        location: Some(hyatt()),
    })?;
    Ok(())
}

/// `person_id`'s voice, all on `axis`, named by hand on an earlier
/// recording.
fn named_voice(store: &MeetingStore, person_id: PersonId, axis: usize) -> Result<(), StoreError> {
    let intro = reviewable_meeting(store, "Intro", local(2026, 10, 14, 15, 0));
    let speaker_id = SpeakerId::new();
    store.connection()?.execute(
        "INSERT INTO meeting_speakers (
            speaker_id, session_id, source_kind, display_name, revision
         ) VALUES (?1, ?2, 'microphone', 'Speaker 2', 0)",
        params![id(speaker_id), id(intro)],
    )?;
    store.store_speaker_voiceprints(
        intro,
        wespeaker_embedding_model_key(),
        &[(speaker_id, voice(axis)?)],
        1,
    )?;
    let people_revision = people_revision_in(&*store.connection()?)?;
    store.identify_speaker(MeetingSpeakerIdentifyRequest {
        operation_id: MeetingOperationId::new(),
        requested_at_utc_ms: 2,
        session_id: intro,
        expected_meeting_revision: store.session_snapshot(intro)?.revision,
        expected_people_revision: people_revision,
        speaker_id,
        disposition: MeetingSpeakerIdentifyDisposition::Label {
            target: MeetingSpeakerIdentifyTarget::Existing(person_id),
            same_voice: Vec::new(),
        },
    })?;
    Ok(())
}

/// A recording started by hand at `started_utc_ms` and still capturing,
/// with nothing on its calendar.
fn capturing(store: &MeetingStore, started_utc_ms: i64) -> Result<MeetingSessionId, StoreError> {
    let session_id = MeetingSessionId::new();
    store.connection()?.execute(
        "INSERT INTO meeting_sessions (
            id, phase, revision, title, origin_kind, preflight_json,
            created_at_utc_ms, started_at_utc_ms, processing_status,
            retention_policy_json
         ) VALUES (?1, 'capturing_recording', 0, ?2, '\"manual\"', '{}', ?3, ?3,
                   '{\"kind\":\"pending\"}', '{\"kind\":\"forever\"}')",
        params![id(session_id), MANUAL_DEFAULT_TITLE, started_utc_ms],
    )?;
    Ok(session_id)
}

/// The call `session_id` is capturing, chosen to read names from, as its
/// roster reads: the user's own tile first, then everyone else's.
fn call_lists(
    store: &MeetingStore,
    session_id: MeetingSessionId,
    names: &[&str],
) -> Result<(), StoreError> {
    let plan = Uuid::new_v4().to_string();
    let connection = store.connection()?;
    connection.execute(
        "INSERT INTO meeting_run_plans (
            plan_id, session_id, attempt_number, schema_version, consent_id,
            canonical_plan_json, created_at_utc_ms
         ) VALUES (?1, ?2, 1, 1, ?3, '{}', 1)",
        params![plan, id(session_id), Uuid::new_v4().to_string()],
    )?;
    connection.execute(
        "INSERT INTO meeting_call_name_runs (
            session_id, plan_id, target_json, enabled, state, detail
         ) VALUES (?1, ?2, ?3, 1, 'reading', 'Waiting for the selected call.')",
        params![
            id(session_id),
            plan,
            serde_json::json!({
                "id": "huddle",
                "label": "Slack — Huddle",
                "bundle_id": "com.tinyspeck.slackmacgap",
                "provider": "slack",
            })
            .to_string()
        ],
    )?;
    drop(connection);
    let participants = names
        .iter()
        .enumerate()
        .map(|(index, name)| CallParticipant {
            id: format!("tile-{index}"),
            name: (*name).to_string(),
            is_local: index == 0,
        })
        .collect();
    store.record_call_observation(
        session_id,
        1_000_000_000,
        &CallObservation {
            state: "roster_only".to_string(),
            detail: "Names are available to pick later.".to_string(),
            participants,
            active_ids: Vec::new(),
        },
        false,
        false,
    )?;
    Ok(())
}

/// A churn review with Nolan two weeks before, recorded at this Mac: the
/// user left renewal pricing open and promised the churn sheet, and Nolan
/// promised the renewal list.
fn churn_review(store: &MeetingStore, nolan: PersonId) -> Result<(), StoreError> {
    let review = reviewable_meeting(store, "Churn review", local(2026, 10, 22, 15, 0));
    store.connection()?.execute(
        "INSERT INTO meeting_speakers (
            speaker_id, session_id, source_kind, display_name, revision
         ) VALUES (?1, ?2, 'microphone', 'Aktan Azat', 0)",
        params![SpeakerId::new().0.to_string(), id(review)],
    )?;
    let receipt = |quote: &str, t_ms| LedgerReceipt {
        quote: quote.to_string(),
        speaker: None,
        t_ms,
        citations: Vec::new(),
    };
    let promise = |who: &str, what: &str, t_ms| LedgerCommitment {
        who: who.to_string(),
        what: what.to_string(),
        firmness: LedgerFirmness::Firm,
        receipt: receipt(what, t_ms),
    };
    file_ledger(
        store,
        review,
        MeetingLedger {
            headline: "Churn is up and renewal pricing stayed open.".to_string(),
            threads: vec![LedgerThread {
                topic: "Renewal pricing".to_string(),
                state: LedgerThreadState::Unanswered,
                substantive: true,
                receipt: receipt("what do we charge at renewal", 12_000),
                owner: Some("Aktan Azat".to_string()),
            }],
            open_loops: vec![LedgerOpenLoop {
                question: "Renewal pricing".to_string(),
                instead: "The call moved on to churn.".to_string(),
                at_ms: 12_000,
                citations: Vec::new(),
            }],
            commitments: vec![
                promise("Aktan Azat", "Send the churn sheet", 30_000),
                promise("Nolan Smith", "Share the renewal list", 40_000),
            ],
            stances: Vec::new(),
            caveats: Vec::new(),
            receipts: LedgerReceiptState::Verified,
        },
    );
    link(store, review, nolan, "manual", "confirmed");
    Ok(())
}

fn people_count(store: &MeetingStore) -> Result<i64, StoreError> {
    Ok(store
        .connection()?
        .query_row("SELECT COUNT(*) FROM persons", [], |row| row.get(0))?)
}

fn offers(suggestions: &[FilingSuggestion]) -> Vec<(FilingTarget, Vec<FilingReason>)> {
    suggestions
        .iter()
        .map(|suggestion| (suggestion.target.clone(), suggestion.reasons.clone()))
        .collect()
}

fn confirmed(
    store: &MeetingStore,
    session_id: MeetingSessionId,
) -> Result<HashSet<PersonId>, StoreError> {
    confirmed_people_in(&*store.connection()?, session_id)
}

fn planned(call: MeetingSessionId, plan: &HeardPlan) -> FilingReason {
    FilingReason::Planned {
        session_id: call,
        said_at_utc_ms: local(2026, 11, 4, 18, 0),
        quote: Some(SAID[1].to_string()),
        citation: plan.citation.clone(),
    }
}

/* A recording started by hand in the time of a plan heard the evening
 * before, with nothing on its calendar, is offered as that plan: its title,
 * the one person its name answers to, and the place an earlier recording
 * saved under the place's name, because of where the plan was said. */
#[test]
fn a_recording_started_in_a_plans_time_is_offered_as_the_plan() {
    let (_directory, store) = store();
    let dana = person(&store, "Dana Reyes", &[], &[]);
    breakfast_at_the_hyatt(&store).unwrap();
    let (call, plan) = call(&store).unwrap();
    let coffee = capturing(&store, local(2026, 11, 5, 10, 3)).unwrap();

    let offered = store
        .context_suggestions(coffee, &[], None, local(2026, 11, 5, 10, 4))
        .unwrap();

    assert_eq!(
        offers(&offered),
        [(
            FilingTarget::Meeting {
                plan_id: plan.plan_id,
                title: "coffee with Dana".to_string(),
                people: vec![FilingPerson {
                    person_id: dana,
                    name: "Dana Reyes".to_string(),
                }],
                place: Some("the Hyatt lobby".to_string()),
                location: Some(hyatt()),
            },
            vec![planned(call, &plan)],
        )]
    );
}

/* Two people answer to the plan's "Dana"; the voice heard 41 seconds in
 * picks which one, and joins the maybe already waiting rather than standing
 * as a second one. */
#[test]
fn a_voice_heard_picks_out_who_the_plan_named() {
    let (_directory, store) = store();
    let dana = person(&store, "Dana Reyes", &[], &[]);
    person(&store, "Dana Kim", &[], &[]);
    named_voice(&store, dana, DANA_AXIS).unwrap();
    let (call, plan) = call(&store).unwrap();
    let coffee = capturing(&store, local(2026, 11, 5, 10, 3)).unwrap();
    let at_start = store
        .context_suggestions(coffee, &[], None, local(2026, 11, 5, 10, 3))
        .unwrap();

    let offered = store
        .context_suggestions(
            coffee,
            &[heard(DANA_AXIS).unwrap()],
            None,
            local(2026, 11, 5, 10, 4),
        )
        .unwrap();

    assert!(matches!(
        &at_start[..],
        [FilingSuggestion { target: FilingTarget::Meeting { people, .. }, .. }] if people.is_empty()
    ));
    assert_eq!(
        offers(&offered),
        [(
            FilingTarget::Meeting {
                plan_id: plan.plan_id,
                title: "coffee with Dana".to_string(),
                people: vec![FilingPerson {
                    person_id: dana,
                    name: "Dana Reyes".to_string(),
                }],
                place: Some("the Hyatt lobby".to_string()),
                location: None,
            },
            vec![
                planned(call, &plan),
                FilingReason::VoiceMatches {
                    name: "Dana Reyes".to_string(),
                    start_offset_ns: Some(AT_0_41),
                },
            ],
        )]
    );
}

/* With no plan near, a voice Sona knows is offered as who is there. */
#[test]
fn a_known_voice_with_no_plan_near_is_offered_as_who_is_there() {
    let (_directory, store) = store();
    let dana = person(&store, "Dana Reyes", &[], &[]);
    named_voice(&store, dana, DANA_AXIS).unwrap();
    call(&store).unwrap();
    let walk = capturing(&store, local(2026, 11, 7, 9, 0)).unwrap();

    let offered = store
        .context_suggestions(
            walk,
            &[heard(DANA_AXIS).unwrap()],
            None,
            local(2026, 11, 7, 9, 1),
        )
        .unwrap();

    assert_eq!(
        offers(&offered),
        [(
            FilingTarget::Person {
                person_id: dana,
                name: "Dana Reyes".to_string(),
            },
            vec![FilingReason::VoiceMatches {
                name: "Dana Reyes".to_string(),
                start_offset_ns: Some(AT_0_41),
            }],
        )]
    );
}

/* A recording started outside every plan's time, hearing only a voice Sona
 * has never been told, is offered nothing. */
#[test]
fn a_recording_no_plan_or_known_voice_points_to_is_offered_nothing() {
    let (_directory, store) = store();
    let dana = person(&store, "Dana Reyes", &[], &[]);
    named_voice(&store, dana, DANA_AXIS).unwrap();
    breakfast_at_the_hyatt(&store).unwrap();
    call(&store).unwrap();
    let lunch = capturing(&store, local(2026, 11, 5, 12, 30)).unwrap();

    let offered = store
        .context_suggestions(
            lunch,
            &[heard(9).unwrap()],
            None,
            local(2026, 11, 5, 12, 31),
        )
        .unwrap();

    assert_eq!(offers(&offered), Vec::new());
}

/* Offering changes nothing: the recording keeps its title, its people and
 * its place, the plan has not happened, and the maybe holds no words of
 * the call, so they go when the call's transcript goes. */
#[test]
fn an_offer_writes_nothing_until_it_is_kept() {
    let (_directory, store) = store();
    let dana = person(&store, "Dana Reyes", &[], &[]);
    named_voice(&store, dana, DANA_AXIS).unwrap();
    breakfast_at_the_hyatt(&store).unwrap();
    let (call, _) = call(&store).unwrap();
    let coffee = capturing(&store, local(2026, 11, 5, 10, 3)).unwrap();

    let offered = store
        .context_suggestions(
            coffee,
            &[heard(DANA_AXIS).unwrap()],
            None,
            local(2026, 11, 5, 10, 4),
        )
        .unwrap();

    assert_eq!(offered.len(), 1);
    assert_eq!(
        store.session_snapshot(coffee).unwrap().title,
        MANUAL_DEFAULT_TITLE
    );
    assert_eq!(confirmed(&store, coffee).unwrap(), HashSet::new());
    assert_eq!(store.conversation_context(coffee).unwrap().location, None);
    assert_eq!(
        store.heard_plans(call).unwrap()[0].happened_session_id,
        None
    );
    let reasons: String = store
        .connection()
        .unwrap()
        .query_row(
            "SELECT reasons_json FROM filing_suggestions WHERE session_id = ?1",
            params![id(coffee)],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!reasons.contains("Hyatt"), "{reasons}");
}

/* Keeping the maybe gives the recording the title, the person and the
 * place, and the plan it was heard as has happened. */
#[test]
fn keeping_the_maybe_makes_the_recording_the_plan() {
    let (_directory, store) = store();
    let dana = person(&store, "Dana Reyes", &[], &[]);
    breakfast_at_the_hyatt(&store).unwrap();
    let (call, _) = call(&store).unwrap();
    let coffee = capturing(&store, local(2026, 11, 5, 10, 3)).unwrap();
    let offered = store
        .context_suggestions(coffee, &[], None, local(2026, 11, 5, 10, 4))
        .unwrap();

    store
        .keep_meeting_maybe(
            &offered[0].id,
            coffee,
            "Coffee with Dana",
            &[dana],
            Some(hyatt()),
            local(2026, 11, 5, 10, 5),
        )
        .unwrap();

    assert_eq!(
        store.session_snapshot(coffee).unwrap().title,
        "Coffee with Dana"
    );
    assert_eq!(confirmed(&store, coffee).unwrap(), HashSet::from([dana]));
    assert_eq!(
        store.conversation_context(coffee).unwrap().location,
        Some(hyatt())
    );
    let plan = store.heard_plans(call).unwrap().pop().unwrap();
    assert_eq!(
        (plan.status, plan.happened_session_id),
        (HeardPlanStatus::Happened, Some(coffee))
    );
    assert_eq!(
        offers(
            &store
                .context_suggestions(coffee, &[], None, local(2026, 11, 5, 10, 6))
                .unwrap()
        ),
        Vec::new()
    );
}

/* The first recall check after the maybe is kept starts from the plan, so
 * the call it was made on comes back before a word is said here; the next
 * check does not start from it again. */
#[test]
fn keeping_the_maybe_recalls_where_the_plan_was_made_once() {
    let (_directory, store) = store();
    let (call, _) = call(&store).unwrap();
    let coffee = capturing(&store, local(2026, 11, 5, 10, 3)).unwrap();
    let offered = store
        .context_suggestions(coffee, &[], None, local(2026, 11, 5, 10, 4))
        .unwrap();
    store
        .keep_meeting_maybe(
            &offered[0].id,
            coffee,
            "Coffee with Dana",
            &[],
            None,
            local(2026, 11, 5, 10, 5),
        )
        .unwrap();
    let mut memory = RecallMemory::default();

    let first = store.live_recall(coffee, &[], &mut memory, None).unwrap();
    let again = store.live_recall(coffee, &[], &mut memory, None).unwrap();

    assert_eq!(
        first
            .iter()
            .map(|recalled| recalled.session_id)
            .collect::<Vec<_>>(),
        [call]
    );
    assert_eq!(again, Vec::new());
}

/* A recording with nothing on its calendar, on a call that lists names, is
 * offered the one person a name answers to. A name that answers to two
 * people, or to nobody, is offered nothing and makes no one. */
#[test]
fn a_name_the_call_lists_is_offered_only_for_someone_sona_knows() {
    let (_directory, store) = store();
    let nolan = person(&store, "Nolan Smith", &[], &[]);
    person(&store, "Dana Reyes", &[], &[]);
    person(&store, "Dana Kim", &[], &[]);
    let huddle = capturing(&store, local(2026, 11, 5, 14, 0)).unwrap();
    call_lists(
        &store,
        huddle,
        &["Aktan Azat", "Nolan Smith", "Dana", "Priya Patel"],
    )
    .unwrap();

    let offered = store
        .context_suggestions(huddle, &[], None, local(2026, 11, 5, 14, 1))
        .unwrap();

    assert_eq!(
        offers(&offered),
        [(
            FilingTarget::Person {
                person_id: nolan,
                name: "Nolan Smith".to_string(),
            },
            vec![FilingReason::CallLists {
                name: "Nolan Smith".to_string(),
            }],
        )]
    );
    assert_eq!(people_count(&store).unwrap(), 3);
    assert_eq!(confirmed(&store, huddle).unwrap(), HashSet::new());
}

/* Before anyone speaks, a call with nothing on its calendar shows who of its
 * names Sona knows and what the user still owes each of them out of the
 * meetings they were both in. A name Sona does not know shows nothing. Once
 * the calendar says who the recording is with, the calendar's prep does. */
#[test]
fn the_prep_shows_who_the_call_lists_and_what_the_user_owes_them() {
    let (_directory, store) = store();
    store.set_user_name_source(|| Some("Aktan Azat".to_string()));
    let nolan = person(&store, "Nolan Smith", &[], &[]);
    let stephen = person(&store, "Stephen Lee", &[], &[]);
    churn_review(&store, nolan).unwrap();
    let huddle = capturing(&store, local(2026, 11, 5, 14, 0)).unwrap();
    call_lists(
        &store,
        huddle,
        &["Aktan Azat", "Stephen Lee", "Nolan Smith", "Priya Patel"],
    )
    .unwrap();

    let prep = store.call_prep(huddle, &[]).unwrap();

    let owed = prep
        .iter()
        .map(|person| {
            (
                person.person_id,
                person.name.as_str(),
                person
                    .open_loops
                    .iter()
                    .map(|open| open.text.as_str())
                    .collect::<Vec<_>>(),
                person
                    .commitments
                    .iter()
                    .map(|commitment| commitment.text.as_str())
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        owed,
        [
            (
                nolan,
                "Nolan Smith",
                vec!["Renewal pricing"],
                vec!["Send the churn sheet"]
            ),
            (stephen, "Stephen Lee", Vec::new(), Vec::new()),
        ]
    );
    assert_eq!(people_count(&store).unwrap(), 2);

    store
        .remember_calendar_facts(huddle, &series_event(local(2026, 11, 5, 14, 0)))
        .unwrap();
    assert_eq!(store.call_prep(huddle, &[]).unwrap(), Vec::new());
}

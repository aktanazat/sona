use super::workflow_core_tests::{
    link, meeting, person, reviewable_meeting, store, transcript_segments,
};
use super::{MeetingStore, StoreError};
use crate::meeting::detection::machine::{
    CalendarAttendee, CalendarEventSummary, ParticipationStatus,
};
use crate::meeting::heard_plan_types::{
    HeardPlan, HeardPlanEdit, HeardPlanStatus, HeardPlanWindow,
};
use crate::meeting::heard_plans::read;
use crate::meeting::processing::{
    MeetingProcessingService, MeetingTextGenerationError, MeetingTextGenerator, ReplyShape,
};
use crate::meeting::types::{ArtifactCitation, MeetingSessionId};
use chrono::{Local, TimeZone};
use rusqlite::params;
use serde::Serialize;
use serde_json::json;
use std::borrow::Cow;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use uuid::Uuid;

/// What was said in the weekly sync, a second apart from 10:00 on Monday
/// 2 November 2026. Every line to L10 is read, being within two lines of
/// one that names a day or a time; L11 names neither and is not read.
const SAID: [&str; 11] = [
    "Morning all.",
    "Let's do the pricing review Thursday at 2 with Nolan at the office.",
    "Thursday at 2 works for me.",
    "We met Acme last Tuesday at 3 and they pushed back.",
    "Coffee with Dana tomorrow at 10?",
    "Sure, tomorrow at 10.",
    "And the board dinner is Friday at 7.",
    "Let's call Priya on Monday at 11 about onboarding.",
    "Good.",
    "Nice work.",
    "We should get lunch with Dana sometime.",
];

/// One plan as the engine names it in its reply.
#[derive(Serialize)]
pub(super) struct Named {
    pub(super) what: &'static str,
    pub(super) when: &'static str,
    pub(super) people: &'static [&'static str],
    pub(super) place: &'static str,
    pub(super) quote: &'static str,
    pub(super) lines: &'static [&'static str],
}

/// The pricing review proposed in L2 and agreed in L3.
const PRICING_REVIEW: Named = Named {
    what: "pricing review",
    when: "Thursday at 2",
    people: &["Nolan"],
    place: "the office",
    quote: SAID[1],
    lines: &["L2", "L3"],
};

/// An engine on this Mac that gives every request the same answer, or
/// cannot be reached, and counts the requests.
pub(super) struct Scripted {
    answer: Option<String>,
    asked: AtomicUsize,
}

impl Scripted {
    pub(super) fn answering(plans: &[Named]) -> Arc<Self> {
        Arc::new(Self {
            answer: Some(json!({ "plans": plans }).to_string()),
            asked: AtomicUsize::new(0),
        })
    }

    fn unreachable() -> Arc<Self> {
        Arc::new(Self {
            answer: None,
            asked: AtomicUsize::new(0),
        })
    }

    fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }
}

impl MeetingTextGenerator for Scripted {
    fn is_available(&self) -> bool {
        true
    }

    fn model_id(&self) -> &'static str {
        "scripted"
    }

    fn model_version(&self) -> Cow<'static, str> {
        Cow::Borrowed("scripted-1")
    }

    fn max_input_bytes(&self) -> usize {
        usize::MAX
    }

    fn context_window_bytes(&self) -> Option<usize> {
        Some(4096 * 3)
    }

    fn generate(
        &self,
        _system_prompt: &str,
        _evidence: &str,
        _max_tokens: i32,
        _shape: ReplyShape,
    ) -> Result<String, MeetingTextGenerationError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.answer
            .clone()
            .ok_or(MeetingTextGenerationError::Unreachable)
    }
}

pub(super) fn engine(generator: &Arc<Scripted>) -> MeetingProcessingService {
    let service = MeetingProcessingService::new(None);
    let generator: Arc<dyn MeetingTextGenerator> = generator.clone();
    service.set_text_generators(Arc::clone(&generator), generator);
    service
}

pub(super) fn local(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> i64 {
    Local
        .with_ymd_and_hms(year, month, day, hour, minute, 0)
        .single()
        .expect("one such moment here")
        .timestamp_millis()
}

fn notes_written() -> i64 {
    local(2026, 11, 2, 11, 0)
}

fn weekly_sync(store: &MeetingStore) -> MeetingSessionId {
    let sync = reviewable_meeting(store, "Weekly sync", local(2026, 11, 2, 10, 0));
    transcript_segments(store, sync, &SAID);
    sync
}

/// The same lines in a new transcript of `sync`, as transcribing it again
/// makes.
fn transcribed_again(store: &MeetingStore, sync: MeetingSessionId) -> Result<(), StoreError> {
    let connection = store.connection()?;
    let session = sync.uuid().to_string();
    let lines = connection
        .prepare(
            "SELECT s.track_id, s.ordinal, s.start_offset_ns, s.end_offset_ns, s.speaker_id,
                    s.base_text
               FROM meeting_transcript_segments s
               JOIN meeting_sessions m
                 ON m.current_transcript_revision_id = s.transcript_revision_id
              WHERE m.id = ?1",
        )?
        .query_map(params![session], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let revision = Uuid::new_v4().to_string();
    connection.execute(
        "INSERT INTO meeting_transcript_revisions (
                transcript_revision_id, session_id, engine_id, destination_json,
                source_set_json, language, state, created_at_utc_ms
             ) VALUES (?1, ?2, 'test', '{}', '[]', 'en', 'complete', 2)",
        params![revision, session],
    )?;
    for (track, ordinal, start, end, speaker, text) in lines {
        connection.execute(
            "INSERT INTO meeting_transcript_segments (
                    segment_id, transcript_revision_id, track_id, ordinal,
                    start_offset_ns, end_offset_ns, speaker_id, base_text
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                Uuid::new_v4().to_string(),
                revision,
                track,
                ordinal,
                start,
                end,
                speaker,
                text
            ],
        )?;
    }
    connection.execute(
        "UPDATE meeting_sessions SET current_transcript_revision_id = ?1 WHERE id = ?2",
        params![revision, session],
    )?;
    Ok(())
}

/// Read `sync` with an engine that names `plans`, and the plans kept.
fn read_with(
    store: &MeetingStore,
    sync: MeetingSessionId,
    plans: &[Named],
) -> Result<Vec<HeardPlan>, StoreError> {
    read(
        store,
        &engine(&Scripted::answering(plans)),
        sync,
        notes_written(),
    )?;
    store.heard_plans(sync)
}

/// Each plan kept, as its title, people and place.
fn kept(plans: &[HeardPlan]) -> Vec<(&str, Vec<&str>, Option<&str>)> {
    plans
        .iter()
        .map(|plan| {
            (
                plan.title.as_str(),
                plan.people.iter().map(String::as_str).collect(),
                plan.place.as_deref(),
            )
        })
        .collect()
}

/// The day comes from the moment the words were said, not from the
/// reading: "Thursday at 2" said on the Monday is that Thursday afternoon.
/// The citation spans the lines the plan cites, so its play button plays
/// them.
#[test]
fn a_plan_is_kept_on_the_day_its_words_name_when_said() {
    let (_directory, store) = store();
    let sync = weekly_sync(&store);

    let plans = read_with(&store, sync, &[PRICING_REVIEW]).unwrap();

    let segments = store.analytics_segments(sync).unwrap();
    let plan = plans.first().expect("the plan was kept");
    assert_eq!(
        (
            plan.when_text.as_str(),
            plan.window,
            plan.quote.as_deref(),
            plan.citation.clone()
        ),
        (
            "Thursday at 2",
            Some(HeardPlanWindow {
                start_utc_ms: local(2026, 11, 5, 14, 0),
                end_utc_ms: local(2026, 11, 5, 15, 0),
                all_day: false,
            }),
            Some(SAID[1]),
            Some(ArtifactCitation {
                segment_id: segments[1].segment_id,
                start_offset_ns: segments[1].start_offset_ns,
                end_offset_ns: segments[2].end_offset_ns,
            }),
        )
    );
}

/// A plan named in words that break off mid-phrase ("coffee with the"), or
/// that name no one and point back at something said before ("decide
/// that"), has no name to put on a calendar. A plan that names who it is
/// with keeps a name like "call about it".
#[test]
fn a_plan_named_in_words_cut_off_mid_phrase_is_not_kept() {
    let (_directory, store) = store();
    let sync = weekly_sync(&store);
    let cut_off = Named {
        what: "coffee with the",
        when: "tomorrow at 10",
        people: &["Dana"],
        place: "",
        quote: SAID[4],
        lines: &["L5"],
    };
    let pointing_back = Named {
        what: "decide that",
        when: "Friday at 7",
        people: &[],
        place: "",
        quote: SAID[6],
        lines: &["L7"],
    };
    let with_someone = Named {
        what: "call about it",
        when: "Monday at 11",
        people: &["Priya"],
        place: "",
        quote: SAID[7],
        lines: &["L8"],
    };

    let plans = read_with(
        &store,
        sync,
        &[PRICING_REVIEW, cut_off, pointing_back, with_someone],
    )
    .unwrap();

    assert_eq!(
        kept(&plans),
        [
            ("pricing review", vec!["Nolan"], Some("the office")),
            ("call about it", vec!["Priya"], None),
        ]
    );
}

#[test]
fn a_time_refines_the_same_plan_only_in_the_same_exchange() -> Result<(), StoreError> {
    for (overlapping, timed_first) in [(true, false), (true, true), (false, false), (false, true)] {
        let (_directory, store) = store();
        let sync = reviewable_meeting(&store, "Sync", local(2026, 11, 6, 19, 42));
        let said = ["Let's sync tomorrow morning.", "We'll do the sync at 10."];
        transcript_segments(&store, sync, &said);
        let mut reply = [
            Named {
                what: "sync",
                when: "tomorrow morning",
                people: &[],
                place: "",
                quote: said[0],
                lines: if overlapping { &["L1", "L2"] } else { &["L1"] },
            },
            Named {
                what: "sync",
                when: "at 10",
                people: &[],
                place: "",
                quote: said[1],
                lines: &["L2"],
            },
        ];
        if timed_first {
            reply.swap(0, 1);
        }
        read(
            &store,
            &engine(&Scripted::answering(&reply)),
            sync,
            local(2026, 11, 6, 20, 15),
        )?;
        let plans = store.heard_plans(sync)?;
        assert_eq!(plans.len(), if overlapping { 1 } else { 2 }, "{plans:?}");
        assert!(plans.iter().any(|plan| {
            plan.when_text == "at 10"
                && plan.window
                    == Some(HeardPlanWindow {
                        start_utc_ms: local(2026, 11, 7, 10, 0),
                        end_utc_ms: local(2026, 11, 7, 11, 0),
                        all_day: false,
                    })
                && plan.quote.as_deref() == Some(said[1])
                && !plan.on_calendar
        }));
    }
    Ok(())
}

#[test]
fn an_overlapping_day_resolves_only_the_clock_the_quote_says() -> Result<(), StoreError> {
    for (clock_quote, clock_said) in [
        ("We'll do the morning sync at 10.", true),
        ("We need 10 tickets for the sync.", false),
        (
            "We'll sync in the morning at 9 and bring 10 tickets.",
            false,
        ),
    ] {
        for (overlapping, timed_first) in
            [(true, false), (true, true), (false, false), (false, true)]
        {
            let (_directory, store) = store();
            let sync = reviewable_meeting(&store, "Sync", local(2026, 11, 6, 19, 42));
            let said = ["Let's sync Saturday morning.", clock_quote];
            transcript_segments(&store, sync, &said);
            let mut reply = [
                Named {
                    what: "sync",
                    when: "Saturday morning",
                    people: &[],
                    place: "",
                    quote: said[0],
                    lines: if overlapping { &["L1", "L2"] } else { &["L1"] },
                },
                Named {
                    what: "sync",
                    when: "10",
                    people: &[],
                    place: "",
                    quote: said[1],
                    lines: &["L2"],
                },
            ];
            if timed_first {
                reply.swap(0, 1);
            }
            let service = engine(&Scripted::answering(&reply));
            let read_at = local(2026, 11, 9, 12, 0);
            read(&store, &service, sync, read_at)?;
            let plans = store.heard_plans(sync)?;
            let refined = overlapping && clock_said;
            assert_eq!(plans.len(), if refined { 1 } else { 2 }, "{plans:?}");
            let clock = plans
                .iter()
                .find(|plan| plan.when_text == "10")
                .expect("the clock wording");
            assert_eq!(
                clock.window,
                refined.then(|| HeardPlanWindow {
                    start_utc_ms: local(2026, 11, 7, 10, 0),
                    end_utc_ms: local(2026, 11, 7, 11, 0),
                    all_day: false,
                }),
            );
            let segments = store.analytics_segments(sync)?;
            let first = if refined { &segments[0] } else { &segments[1] };
            assert_eq!(
                clock.citation,
                Some(ArtifactCitation {
                    segment_id: first.segment_id,
                    start_offset_ns: first.start_offset_ns,
                    end_offset_ns: segments[1].end_offset_ns,
                }),
            );
            assert_eq!(clock.quote.as_deref(), Some(clock_quote));
            assert!(!clock.on_calendar);
            assert!(!read(&store, &service, sync, read_at)?);
            assert_eq!(plans, store.heard_plans(sync)?);
        }
    }
    Ok(())
}

/// What the engine names is kept only as far as the lines it cites bear it
/// out. Each case is one plan the engine named.
macro_rules! kept_as_its_lines_bear_it_out {
    ($($name:ident: $plan:expr => $kept:expr;)*) => { $(
        #[test]
        fn $name() {
            let (_directory, store) = store();
            let sync = weekly_sync(&store);

            let plans = read_with(&store, sync, &[$plan]).unwrap();

            let expected: Vec<(&str, Vec<&str>, Option<&str>)> = $kept;
            assert_eq!(kept(&plans), expected);
        }
    )* };
}

kept_as_its_lines_bear_it_out! {
    a_plan_its_lines_bear_out_is_kept_with_the_names_and_place_said:
        PRICING_REVIEW => vec![("pricing review", vec!["Nolan"], Some("the office"))];
    a_name_or_place_its_lines_do_not_say_is_left_off:
        Named { people: &["Nolan", "Stephen"], place: "the Hyatt lobby", ..PRICING_REVIEW }
        => vec![("pricing review", vec!["Nolan"], None)];
    a_quote_its_lines_do_not_hold_is_dropped:
        Named {
            what: "deck review", when: "Thursday at 2", people: &[], place: "",
            quote: "Let's meet Wednesday at 4 to finalize the deck", lines: &["L2"]
        }
        => vec![];
    a_when_its_lines_do_not_say_is_dropped:
        Named { when: "Wednesday at 9", people: &[], place: "", lines: &["L2"], ..PRICING_REVIEW }
        => vec![];
    a_plan_from_a_line_the_engine_was_not_shown_is_dropped:
        Named {
            what: "lunch with Dana", when: "sometime", people: &["Dana"], place: "",
            quote: SAID[10], lines: &["L11"]
        }
        => vec![];
    a_meeting_already_over_when_it_was_said_is_dropped:
        Named {
            what: "Acme meeting", when: "last Tuesday at 3", people: &[], place: "",
            quote: SAID[3], lines: &["L4"]
        }
        => vec![];
}

/// Once for each transcript, even when the reading found nothing: the
/// engine is not asked again each time notes are written, only when the
/// transcript is made anew.
#[test]
fn a_meeting_is_read_once_for_each_transcript() {
    let (_directory, store) = store();
    let sync = weekly_sync(&store);
    let engine_here = Scripted::answering(&[]);
    let service = engine(&engine_here);

    read(&store, &service, sync, notes_written()).unwrap();
    read(&store, &service, sync, notes_written()).unwrap();
    let asked_for_one_transcript = engine_here.asked();
    transcribed_again(&store, sync).unwrap();
    read(&store, &service, sync, notes_written()).unwrap();

    assert_eq!((asked_for_one_transcript, engine_here.asked()), (1, 2));
}

/// An engine that could not be reached leaves the meeting unread, so the
/// next notes read it rather than its plans being lost.
#[test]
fn a_meeting_the_engine_could_not_be_reached_for_is_read_next_time() {
    let (_directory, store) = store();
    let sync = weekly_sync(&store);
    read(
        &store,
        &engine(&Scripted::unreachable()),
        sync,
        notes_written(),
    )
    .unwrap();

    let plans = read_with(&store, sync, &[PRICING_REVIEW]).unwrap();

    assert_eq!(
        kept(&plans),
        [("pricing review", vec!["Nolan"], Some("the office"))]
    );
}

/// A plan he edited, answered "Not now" or put on the calendar stays as he
/// left it when the meeting is read again, and the new reading's copy of it
/// is dropped. A plan he left alone is replaced by the new reading.
#[test]
fn what_he_answered_stays_through_the_next_reading_once() {
    let (_directory, store) = store();
    let sync = weekly_sync(&store);
    let reply = [
        PRICING_REVIEW,
        Named {
            what: "coffee with Dana",
            when: "tomorrow at 10",
            people: &["Dana"],
            place: "",
            quote: SAID[4],
            lines: &["L5", "L6"],
        },
        Named {
            what: "board dinner",
            when: "Friday at 7",
            people: &[],
            place: "",
            quote: SAID[6],
            lines: &["L7"],
        },
        Named {
            what: "call with Priya",
            when: "Monday at 11",
            people: &["Priya"],
            place: "",
            quote: SAID[7],
            lines: &["L8"],
        },
    ];
    let plans = read_with(&store, sync, &reply).unwrap();
    let plan = |title: &str| {
        plans
            .iter()
            .find(|plan| plan.title == title)
            .expect("the reading kept it")
    };
    let pricing = plan("pricing review");
    let window = pricing.window.expect("a time was said");
    store
        .heard_plan_edit(HeardPlanEdit {
            plan_id: pricing.plan_id,
            title: "Pricing review with Nolan".into(),
            start_utc_ms: window.start_utc_ms,
            end_utc_ms: window.end_utc_ms,
            people: vec!["Nolan".into()],
            place: None,
        })
        .unwrap();
    store
        .heard_plan_not_now(plan("coffee with Dana").plan_id)
        .unwrap();
    store
        .set_heard_plan_calendar_event(plan("board dinner").plan_id, Some("event-1"))
        .unwrap();

    transcribed_again(&store, sync).unwrap();
    let plans = read_with(&store, sync, &reply).unwrap();

    let mut answered = plans
        .iter()
        .map(|plan| {
            (
                plan.title.as_str(),
                plan.edited,
                plan.declined,
                plan.on_calendar,
            )
        })
        .collect::<Vec<_>>();
    answered.sort_unstable();
    assert_eq!(
        answered,
        [
            ("Pricing review with Nolan", true, false, false),
            ("board dinner", false, false, true),
            ("call with Priya", false, false, false),
            ("coffee with Dana", false, true, false),
        ]
    );
}

/// A plan on the calendar takes no second entry, and no edit until it is
/// taken back off, so the entry and the plan never disagree: a second tap
/// on "Add to calendar" is turned away and its entry undone.
#[test]
fn a_plan_on_the_calendar_takes_no_second_entry_or_edit_until_it_is_off() {
    let (_directory, store) = store();
    let sync = weekly_sync(&store);
    let plan = read_with(&store, sync, &[PRICING_REVIEW])
        .unwrap()
        .remove(0);
    let window = plan.window.expect("a time was said");
    let edit = HeardPlanEdit {
        plan_id: plan.plan_id,
        title: "Pricing review".into(),
        start_utc_ms: window.start_utc_ms,
        end_utc_ms: window.end_utc_ms,
        people: Vec::new(),
        place: None,
    };
    store
        .set_heard_plan_calendar_event(plan.plan_id, Some("first"))
        .unwrap();

    let second = store.set_heard_plan_calendar_event(plan.plan_id, Some("second"));
    let edited_while_on = store.heard_plan_edit(edit.clone());
    let entry = store.heard_plan_calendar_event(plan.plan_id).unwrap();
    store
        .set_heard_plan_calendar_event(plan.plan_id, None)
        .unwrap();
    let edited_once_off = store.heard_plan_edit(edit).map(|plan| plan.title);

    assert!(matches!(second, Err(StoreError::Conflict)));
    assert!(matches!(edited_while_on, Err(StoreError::Conflict)));
    assert_eq!(entry.as_deref(), Some("first"));
    assert_eq!(edited_once_off.ok().as_deref(), Some("Pricing review"));
}

/// A plan happened where a later recording overlaps its hour with someone
/// it named confirmed there; anything less is no record of it. Each case
/// is that later recording: when it began, who was linked, and how surely.
macro_rules! where_the_plan_happened {
    ($($name:ident: $began:expr, $who:expr, $confidence:expr => $happened:expr;)*) => { $(
        #[test]
        fn $name() {
            let (_directory, store) = store();
            let sync = weekly_sync(&store);
            read_with(&store, sync, &[PRICING_REVIEW]).unwrap();
            let later = meeting(&store, "Pricing", $began);
            link(&store, later, person(&store, $who, &[], &[]), "manual", $confidence);

            let plan = store.heard_plans(sync).unwrap().remove(0);

            let expected = if $happened {
                (HeardPlanStatus::Happened, Some(later))
            } else {
                (HeardPlanStatus::NoRecord, None)
            };
            assert_eq!((plan.status, plan.happened_session_id), expected);
        }
    )* };
}

where_the_plan_happened! {
    a_recording_in_its_hour_with_someone_it_named_is_where_it_happened:
        local(2026, 11, 5, 14, 5), "Nolan Reyes", "confirmed" => true;
    someone_only_suggested_there_is_not_enough:
        local(2026, 11, 5, 14, 5), "Nolan Reyes", "suggested" => false;
    a_recording_with_no_one_it_named_is_not_it:
        local(2026, 11, 5, 14, 5), "Dana Lee", "confirmed" => false;
    a_recording_after_its_hour_is_not_it:
        local(2026, 11, 5, 16, 0), "Nolan Reyes", "confirmed" => false;
}

/// Nolan's page keeps the pricing review he was named in once its hour has
/// gone by with no recording of it, and lets it go once a recording in that
/// hour has him confirmed there. Dana was never named in it.
#[test]
fn a_persons_page_keeps_a_passed_plan_until_a_recording_of_it() {
    let (_directory, store) = store();
    let sync = weekly_sync(&store);
    read_with(&store, sync, &[PRICING_REVIEW]).unwrap();
    let nolan = person(&store, "Nolan Reyes", &[], &[]);
    let dana = person(&store, "Dana Lee", &[], &[]);
    let without_record = |person_id| {
        store
            .person_detail(person_id)
            .unwrap()
            .detail
            .plans_without_record
            .into_iter()
            .map(|plan| plan.title)
            .collect::<Vec<_>>()
    };

    let before = without_record(nolan);
    let never_named = without_record(dana);
    let later = meeting(&store, "Pricing", local(2026, 11, 5, 14, 5));
    link(&store, later, nolan, "manual", "confirmed");
    let after = without_record(nolan);

    assert_eq!(before, vec!["pricing review"]);
    assert!(never_named.is_empty(), "{never_named:?}");
    assert!(after.is_empty(), "{after:?}");
}

/// An invited event on Thursday, with `invited` and the user.
fn invited_event(title: &str, start_utc_ms: i64, invited: &str) -> CalendarEventSummary {
    let attendee = |name: &str, is_self: bool| CalendarAttendee {
        name: name.into(),
        status: ParticipationStatus::Accepted,
        email: None,
        is_self,
    };
    CalendarEventSummary {
        event_key: format!("series-1#{start_utc_ms}"),
        series_key: "series-1".into(),
        title: title.into(),
        attendee_count: 2,
        start_utc_ms,
        end_utc_ms: start_utc_ms + 3_600_000,
        attendees: vec![attendee("Aktan Azat", true), attendee(invited, false)],
        notes: None,
        calendar_name: None,
        url: None,
        location: None,
        place: None,
    }
}

/// An invited event keeps the plan heard for its time when it carries the
/// plan's title or invites someone the plan names, and its prep carries
/// the words that made the plan and where they were said.
macro_rules! an_event_keeps_the_plan_heard_for_it {
    ($($name:ident: $title:expr, $start:expr, $invited:expr => $keeps:expr;)*) => { $(
        #[test]
        fn $name() {
            let (_directory, store) = store();
            let sync = weekly_sync(&store);
            let plans = read_with(&store, sync, &[PRICING_REVIEW]).unwrap();

            let kept = store
                .heard_plan_for_event(&invited_event($title, $start, $invited))
                .unwrap();

            assert_eq!(
                kept.map(|plan| (plan.plan_id, plan.quote, plan.citation.is_some())),
                $keeps.then(|| (plans[0].plan_id, Some(SAID[1].to_string()), true))
            );
        }
    )* };
}

an_event_keeps_the_plan_heard_for_it! {
    an_invite_to_someone_the_plan_names_keeps_it:
        "Catch-up", local(2026, 11, 5, 14, 0), "Nolan Reyes" => true;
    an_event_with_the_plans_title_keeps_it:
        "Pricing Review", local(2026, 11, 5, 14, 0), "Morgan Lee" => true;
    an_event_with_neither_keeps_nothing:
        "Catch-up", local(2026, 11, 5, 14, 0), "Morgan Lee" => false;
    an_invite_at_another_time_keeps_nothing:
        "Catch-up", local(2026, 11, 6, 14, 0), "Nolan Reyes" => false;
}

/// A plan with a time and no event still gets its prep: it comes due once
/// its start is within the lead, not after it has started, and not once
/// he has turned it down.
#[test]
fn a_planned_time_comes_due_for_its_prep_until_turned_down() {
    let (_directory, store) = store();
    let sync = weekly_sync(&store);
    let plan_id = read_with(&store, sync, &[PRICING_REVIEW]).unwrap()[0].plan_id;
    let due = |after_utc_ms, until_utc_ms| {
        store
            .heard_plans_starting(after_utc_ms, until_utc_ms)
            .unwrap()
            .into_iter()
            .map(|plan| plan.plan_id)
            .collect::<Vec<_>>()
    };

    assert_eq!(
        due(local(2026, 11, 5, 13, 59), local(2026, 11, 5, 14, 0)),
        [plan_id]
    );
    assert_eq!(
        due(local(2026, 11, 5, 14, 0), local(2026, 11, 5, 14, 1)),
        []
    );
    store.heard_plan_not_now(plan_id).unwrap();
    assert_eq!(
        due(local(2026, 11, 5, 13, 59), local(2026, 11, 5, 14, 0)),
        []
    );
}

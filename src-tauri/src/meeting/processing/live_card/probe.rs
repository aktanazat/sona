//! The live card over a synthesized twelve-minute call, through the code a
//! recording runs: the transcript grows the way the live pass grows it,
//! the card is read on the live pass's beat, the stop reads what is left,
//! the promises are filed in a scratch tracker, the app's own notes
//! pipeline writes the meeting's notes, and the notes intake takes their
//! commitments in. The report gives the model requests per minute of
//! meeting, what the card heard, and whether the intake adopted the heard
//! tasks or filed them twice.
//!
//! SONA_PROBE_ENGINE names the engine in the form Sona's settings keep it,
//! such as `{"kind":"local_endpoint","base_url":"http://127.0.0.1:11434/v1","model":"sona-notes","context_window_tokens":16384}`.
//!
//! SONA_PROBE_ENGINE=… <test binary> --ignored --exact \
//!     meeting::processing::live_card::probe::live_card_probe --nocapture

use super::super::{empty_local_plan, LocalEndpointGenerator, ProcessingOrigin};
use super::*;
use crate::meeting::store::workflow_core_tests;
use crate::meeting::store::{TranscriptRevisionInput, TranscriptSegmentInput};
use crate::meeting::types::{ProcessingDestination, SourceTrackId};
use crate::planning::schema;
use crate::planning::work::{live::file_promises, sync};
use crate::settings::MeetingLocalEngine;
use rusqlite::{Connection, TransactionBehavior};
use std::time::Instant;

/// How often the live pass hands the card new words, in meeting time.
const PASS_EVERY_S: u64 = 20;

#[derive(Clone, Copy)]
enum Who {
    Me,
    Dana,
    Priya,
}

/// Second of the call, who spoke, what they said.
const CALL: &[(u64, Who, &str)] = &[
    (
        4,
        Who::Me,
        "Hey both, thanks for jumping on. This is the launch sync for the beta.",
    ),
    (
        12,
        Who::Dana,
        "Hi. I have a hard stop at the half hour, so let's keep it tight.",
    ),
    (
        20,
        Who::Priya,
        "Same here. Where are we on the pricing page?",
    ),
    (
        31,
        Who::Me,
        "The copy is done but the screenshots are still the old ones.",
    ),
    (
        42,
        Who::Dana,
        "I can redo the screenshots. I'll send them over by Thursday.",
    ),
    (
        55,
        Who::Priya,
        "Great. And the FAQ still mentions the old free tier.",
    ),
    (
        66,
        Who::Me,
        "I'll fix the FAQ today, that's a five minute job.",
    ),
    (78, Who::Priya, "Perfect. Next, the waitlist email."),
    (
        90,
        Who::Dana,
        "The draft is in the doc. It reads fine to me.",
    ),
    (103, Who::Priya, "I left two comments on the subject line."),
    (
        115,
        Who::Me,
        "Okay, Priya, can you pick the final subject line?",
    ),
    (
        126,
        Who::Priya,
        "Sure, I'll pick one and schedule the send for Monday morning.",
    ),
    (140, Who::Dana, "Monday works. People read email on Monday."),
    (
        152,
        Who::Me,
        "Then the onboarding checklist. We keep losing people at step three.",
    ),
    (
        165,
        Who::Dana,
        "Step three asks for calendar access before it says why.",
    ),
    (
        178,
        Who::Priya,
        "We should explain it first. One sentence would do it.",
    ),
    (
        190,
        Who::Me,
        "Agreed. I'll write that sentence and put it behind a flag.",
    ),
    (204, Who::Dana, "Do we have numbers on step three?"),
    (
        215,
        Who::Priya,
        "About a third of people stop there, last I looked.",
    ),
    (
        228,
        Who::Dana,
        "That's a lot. It's the biggest drop we have.",
    ),
    (240, Who::Me, "It is. The rest of the funnel looks healthy."),
    (
        255,
        Who::Priya,
        "The new signups are mostly from the newsletter mention.",
    ),
    (
        268,
        Who::Dana,
        "That newsletter had a big audience. Nice to see it convert.",
    ),
    (282, Who::Me, "The support inbox was quiet this week too."),
    (
        296,
        Who::Priya,
        "A few people asked about team plans, nothing urgent.",
    ),
    (
        310,
        Who::Dana,
        "Team plans are a later problem. Let's not open that now.",
    ),
    (
        325,
        Who::Me,
        "Fine by me. The crash reports are down to almost nothing.",
    ),
    (
        340,
        Who::Priya,
        "That last build fixed the sync crash, right?",
    ),
    (
        352,
        Who::Dana,
        "Yes, the sync crash is gone since Tuesday's build.",
    ),
    (
        366,
        Who::Me,
        "Good. The beta feedback form gets maybe ten answers a day.",
    ),
    (
        380,
        Who::Priya,
        "Most of them like the meeting notes. A few want exports.",
    ),
    (
        395,
        Who::Dana,
        "Exports keep coming up. Markdown would cover most of it.",
    ),
    (
        410,
        Who::Me,
        "Markdown export is small. It's mostly a menu item.",
    ),
    (
        425,
        Who::Priya,
        "Then let's not plan it today. It's on the list.",
    ),
    (
        440,
        Who::Dana,
        "One more thing. Legal wants the privacy page reviewed before launch.",
    ),
    (
        453,
        Who::Me,
        "Dana, can you own the privacy review with legal?",
    ),
    (
        464,
        Who::Dana,
        "Yes, I'll get legal's sign-off by end of next week.",
    ),
    (478, Who::Priya, "And the launch post? Who's writing it?"),
    (
        490,
        Who::Me,
        "I'll draft the launch post and share it with you both by Wednesday.",
    ),
    (503, Who::Priya, "I'll review it the same day you share it."),
    (517, Who::Dana, "Do we need a demo video?"),
    (530, Who::Priya, "A short one. Sixty seconds, no voiceover."),
    (
        544,
        Who::Me,
        "Priya, would you record it? You know the flows best.",
    ),
    (
        556,
        Who::Priya,
        "Okay, I'll record the demo video tomorrow afternoon.",
    ),
    (570, Who::Dana, "Then I think we're in good shape."),
    (584, Who::Me, "Let's check in again on Friday at ten."),
    (598, Who::Priya, "Friday at ten works."),
    (611, Who::Dana, "Works for me. Thanks, both."),
    (624, Who::Me, "Thanks, talk soon."),
];

fn engine() -> MeetingLocalEngine {
    // PANIC: a probe run without its engine has nothing to report.
    let engine = std::env::var("SONA_PROBE_ENGINE")
        .expect("SONA_PROBE_ENGINE names the meeting engine to read the card with");
    // PANIC: as above, for an engine written in another form.
    serde_json::from_str(&engine).expect("SONA_PROBE_ENGINE in the form Sona's settings keep it")
}

fn failed(step: &str, error: &anyhow::Error) -> StoreError {
    println!("{step} failed: {error:#}");
    StoreError::Unavailable
}

/// The words the meeting keeps once it stops, the way an import stores
/// them, so the app's notes pipeline has something to write from.
fn keep_transcript(
    store: &MeetingStore,
    session_id: MeetingSessionId,
    tracks: (SourceTrackId, SourceTrackId),
) -> Result<(), StoreError> {
    let mut plan = empty_local_plan();
    plan.session_id = session_id;
    plan.attempt_number = 2;
    workflow_core_tests::processing_plan(store, &plan)?;
    let revision = store.begin_transcript_revision(TranscriptRevisionInput {
        session_id,
        engine_id: "import",
        model_version: None,
        destination: &ProcessingDestination::Local,
        source_set: &SourceKind::ALL,
        language: "en",
    })?;
    let inputs: Vec<_> = CALL
        .iter()
        .map(|&(second, who, text)| {
            let (track_id, source_kind, speaker) = match who {
                Who::Me => (tracks.0, SourceKind::Microphone, "Me"),
                Who::Dana => (tracks.1, SourceKind::SystemAudio, "Dana"),
                Who::Priya => (tracks.1, SourceKind::SystemAudio, "Priya"),
            };
            TranscriptSegmentInput {
                track_id,
                source_kind,
                start_offset_ns: second * 1_000_000_000,
                end_offset_ns: (second + 8) * 1_000_000_000,
                text: text.to_string(),
                confidence_milli: None,
                speaker: Some(speaker.to_string()),
            }
        })
        .collect();
    store.append_transcript_segments(session_id, revision, &inputs)?;
    store.complete_transcript_revision(session_id, revision)
}

fn segment(
    second: u64,
    who: Who,
    text: &str,
    tracks: (SourceTrackId, SourceTrackId),
) -> LiveSegment {
    let (track_id, source_kind, speaker) = match who {
        Who::Me => (
            tracks.0,
            SourceKind::Microphone,
            MeetingProvisionalSpeaker::Unknown,
        ),
        Who::Dana => (
            tracks.1,
            SourceKind::SystemAudio,
            MeetingProvisionalSpeaker::Cluster { index: 0 },
        ),
        Who::Priya => (
            tracks.1,
            SourceKind::SystemAudio,
            MeetingProvisionalSpeaker::Cluster { index: 1 },
        ),
    };
    LiveSegment {
        track_id,
        source_kind,
        speaker,
        start_offset_ns: second * 1_000_000_000,
        end_offset_ns: (second + 8) * 1_000_000_000,
        text: text.to_string(),
    }
}

fn describe(promise: &MeetingLivePromise) -> String {
    let due = promise.due_at_ms.map_or_else(
        || "no date".to_string(),
        |ms| {
            Local.timestamp_millis_opt(ms).single().map_or_else(
                || ms.to_string(),
                |at| at.format("%a %-d %b %H:%M").to_string(),
            )
        },
    );
    format!(
        "{} · {} · {} — \"{}\"",
        promise.what,
        promise.owner_name.as_deref().unwrap_or("no owner"),
        due,
        promise.quote
    )
}

#[ignore = "reads a synthesized call with the meeting engine SONA_PROBE_ENGINE names"]
#[test]
fn live_card_probe() {
    let engine = engine();
    let generator: Arc<dyn MeetingTextGenerator> = Arc::new(
        LocalEndpointGenerator::from_settings(&engine)
            .expect("the engine names a loopback endpoint")
            .expect("the engine is a local endpoint"),
    );
    let (_directory, store) = workflow_core_tests::store();
    let began_utc_ms = Local::now().timestamp_millis() - 20 * 60 * 1_000;
    let session_id = workflow_core_tests::reviewable_meeting(&store, "Launch sync", began_utc_ms);
    let tracks = workflow_core_tests::recording_tracks(&store, session_id);
    let service = Arc::new(MeetingProcessingService::new(None));
    service.set_text_generators(Arc::clone(&generator), Arc::clone(&generator));
    let live = LiveTranscript::new();

    let started = Instant::now();
    let mut lines = CALL.iter().peekable();
    let mut passes = 0;
    let mut minute_requests = Vec::new();
    let end_s = CALL.last().map_or(0, |line| line.0 + 8);
    let mut pass_at = 0;
    while pass_at <= end_s {
        pass_at += PASS_EVERY_S;
        while let Some(&&(second, who, text)) = lines.peek() {
            if second + 8 > pass_at {
                break;
            }
            live.append(segment(second, who, text, tracks));
            lines.next();
        }
        passes += 1;
        let before = live.card().requests;
        tick(&service, &store, session_id, &live, Moment::Pass);
        let minute = usize::try_from(pass_at.saturating_sub(1) / 60).unwrap_or(usize::MAX);
        if minute_requests.len() <= minute {
            minute_requests.resize(minute + 1, 0);
        }
        minute_requests[minute] += live.card().requests - before;
    }
    let live_requests = live.card().requests;
    let live_seconds = started.elapsed().as_secs_f64();
    let hang_up_started = Instant::now();
    tick(&service, &store, session_id, &live, Moment::HangUp);
    let hang_up_requests = live.card().requests - live_requests;
    let promises = live.live_promises();

    let minutes = f64::from(u32::try_from(end_s).unwrap_or(u32::MAX)) / 60.0;
    println!("\nsona build {}", env!("SONA_BUILD_COMMIT"));
    println!(
        "call: {} lines over {minutes:.1} min, {passes} live passes",
        CALL.len()
    );
    println!(
        "while recording: {live_requests} model requests ({:.2} a minute), {live_seconds:.0} s of model time",
        f64::from(live_requests) / minutes
    );
    println!("requests by minute: {minute_requests:?}");
    println!(
        "at the stop: {hang_up_requests} more, {:.0} s",
        hang_up_started.elapsed().as_secs_f64()
    );
    println!("card: {} promises", promises.len());
    for promise in &promises {
        println!("  {}", describe(promise));
    }

    let mut history = Connection::open_in_memory().expect("a scratch tracker");
    schema::apply_all(&history).expect("the planner schema");
    let filed_at = Local::now().timestamp_millis();
    let filed = store
        .with_planning_snapshot(|meetings| {
            file_promises(&history, meetings, session_id, &promises, filed_at)
                .map_err(|error| failed("filing", &error))
        })
        .expect("the heard promises filed");
    println!("filed in Triage: {}", filed.len());

    keep_transcript(&store, session_id, tracks).expect("the call's words kept");
    let notes_started = Instant::now();
    service.submit(
        Arc::clone(&store),
        session_id,
        ProcessingOrigin::ImportedTranscript,
    );
    service.wait_for_job(session_id);
    let commitments = store
        .meeting_loops(session_id)
        .map(|read| {
            read.rows
                .into_iter()
                .filter(|row| row.kind == crate::meeting::loop_types::MeetingLoopKind::Commitment)
                .count()
        })
        .unwrap_or(0);
    println!(
        "notes: {commitments} commitments, {:.0} s",
        notes_started.elapsed().as_secs_f64()
    );

    let taken = store
        .with_planning_snapshot(|meetings| {
            let mut run = || -> anyhow::Result<Vec<crate::planning::work::types::WorkItem>> {
                let tx = history.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let taken =
                    sync::take_in_meeting(&tx, meetings, session_id, None, filed_at + 1_000)?;
                tx.commit()?;
                Ok(taken)
            };
            run().map_err(|error| failed("intake", &error))
        })
        .expect("the notes taken in");
    let heard_ids: Vec<&str> = filed.iter().map(|item| item.id.as_str()).collect();
    let adopted = taken
        .iter()
        .filter(|item| heard_ids.contains(&item.id.as_str()))
        .count();
    let tasks: i64 = history
        .query_row("SELECT COUNT(*) FROM planning_tasks", [], |row| row.get(0))
        .expect("the tracker counted");
    println!(
        "intake: {} items from the notes, {adopted} of them heard tasks adopted, {} new; tracker holds {tasks} tasks",
        taken.len(),
        taken.len() - adopted
    );
    let mut titles: Vec<String> = history
        .prepare("SELECT title FROM planning_tasks ORDER BY created_at_ms")
        .and_then(|mut statement| {
            statement
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()
        })
        .expect("the tracker read");
    for title in &titles {
        println!("  task: {title}");
    }
    titles.sort();
    titles.dedup();
    println!(
        "tasks with the same title: {}",
        usize::try_from(tasks).unwrap_or(0) - titles.len()
    );
}

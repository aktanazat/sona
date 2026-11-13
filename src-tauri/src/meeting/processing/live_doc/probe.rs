//! The doc and the board over the synthesized twelve-minute call, through
//! the code a recording runs: the transcript grows the way the live pass
//! grows it, the card and the doc are read on the pass's beat, the user
//! asks for the doc out loud three minutes in, changes its first point a
//! little later, and the stop keeps both with the meeting. The report
//! gives what the doc held when it was asked for, whether the changed
//! point outlived later readings, the board, and what the meeting's page
//! reads back after the stop.
//!
//! SONA_PROBE_ENGINE=… <test binary> --ignored --exact \
//!     meeting::processing::live_doc::probe::live_doc_probe --nocapture
//!
//! `live_doc_replay_probe` replays short calls through a doc started before
//! anyone speaks, as the button starts it, and prints every point each
//! reading kept, so a reading that writes about its own input shows up.
//! `{"kind":"apple_intelligence"}` names the on-device model; with
//! SONA_PROBE_RAW=1 each reading's input and reply are printed too.

use super::super::live_card::probe::{engine, segment, Who, CALL, PASS_EVERY_S};
use super::super::live_card::{self, MeetingLivePromise};
use super::super::{AppleIntelligenceGenerator, LocalEndpointGenerator};
use super::*;
use crate::meeting::store::workflow_core_tests;
use crate::settings::MeetingLocalEngine;
use chrono::{Local, TimeZone};
use std::sync::Arc;
use std::time::Instant;

const START_AT_S: u64 = 196;
const START: &str = "Sona, start a doc for this plan.";
const RECAP_AT_S: u64 = 508;
const RECAP: &str = "Hey Sona, recap so far.";
const EDIT: &str = "Screenshots: Dana redoes them by Thursday (changed by hand)";

fn day(ms: Option<i64>) -> String {
    ms.and_then(|ms| Local.timestamp_millis_opt(ms).single())
        .map_or_else(|| "no date".to_string(), |at| at.format("%a %-d %b").to_string())
}

fn board_line(promise: &MeetingLivePromise) -> String {
    format!(
        "{} · {} · {}",
        promise.what,
        promise.owner_name.as_deref().unwrap_or("no owner"),
        day(promise.due_at_ms)
    )
}

fn generator() -> Arc<dyn MeetingTextGenerator> {
    // PANIC: a probe run names a local engine; anything else is a setup error.
    let engine: Arc<dyn MeetingTextGenerator> = match engine() {
        MeetingLocalEngine::AppleIntelligence => Arc::new(AppleIntelligenceGenerator),
        engine => Arc::new(
            LocalEndpointGenerator::from_settings(&engine)
                .expect("the engine names a loopback endpoint")
                .expect("the engine is a local endpoint"),
        ),
    };
    if std::env::var_os("SONA_PROBE_RAW").is_some() {
        Arc::new(Printed(engine))
    } else {
        engine
    }
}

/// The engine, printing what each reading gave it and what came back.
struct Printed(Arc<dyn MeetingTextGenerator>);

impl MeetingTextGenerator for Printed {
    fn is_available(&self) -> bool {
        self.0.is_available()
    }

    fn model_id(&self) -> &'static str {
        self.0.model_id()
    }

    fn model_version(&self) -> std::borrow::Cow<'static, str> {
        self.0.model_version()
    }

    fn max_input_bytes(&self) -> usize {
        self.0.max_input_bytes()
    }

    fn context_window_bytes(&self) -> Option<usize> {
        self.0.context_window_bytes()
    }

    fn reserved_output_tokens(&self, requested_output_tokens: i32) -> usize {
        self.0.reserved_output_tokens(requested_output_tokens)
    }

    fn generate(
        &self,
        system_prompt: &str,
        evidence: &str,
        max_tokens: i32,
        shape: super::super::ReplyShape,
    ) -> Result<String, MeetingTextGenerationError> {
        self.0.generate(system_prompt, evidence, max_tokens, shape)
    }

    fn generate_json(
        &self,
        system_prompt: &str,
        evidence: &str,
        max_tokens: i32,
        schema: fn() -> &'static schemars::schema::RootSchema,
    ) -> Result<String, MeetingTextGenerationError> {
        let answer = self.0.generate_json(system_prompt, evidence, max_tokens, schema);
        println!("      given {evidence}\n      reply {answer:?}");
        answer
    }
}

/// The pier lamps call as the scratch app heard it: room audio on the
/// microphone, cut where the live pass cut it.
const PIER: &[(u64, Who, &str)] = &[
    (2, Who::Me, "Hi Priya."),
    (6, Who::Me, "The pier lamps on berth three still flicker after dark."),
    (14, Who::Me, "Priya, can you call the electrician tomorrow"),
    (18, Who::Me, "and send the quote by Thursday?"),
    (26, Who::Me, "I will share the map"),
    (29, Who::Me, "review notes and book a follow up for Monday."),
];

/// A call that has nothing for the doc yet.
const SMALL_TALK: &[(u64, Who, &str)] = &[
    (2, Who::Me, "Hi, can you hear me?"),
    (6, Who::Priya, "Yes, loud and clear."),
    (11, Who::Me, "Great, give me one second, my screen froze."),
    (19, Who::Priya, "Sure, no rush."),
    (27, Who::Me, "Okay, I'm back. Sorry about that."),
    (35, Who::Priya, "All good."),
    (52, Who::Me, "Hello?"),
    (75, Who::Priya, "Still here."),
];

/// The harbor map call as the scratch app heard it from the display's
/// speaker: room noise heard as one word before anyone spoke, and the
/// launch date moved twice.
const HARBOR: &[(u64, Who, &str)] = &[
    (8, Who::Me, "California."),
    (40, Who::Me, "Yeah."),
    (42, Who::Me, "Thanks for joining."),
    (43, Who::Me, "Joining, Daniel. Today we need to settle the harbour map launch date and the dock survey."),
    (50, Who::Me, "The survey of Dock A is booked till Tuesday morning."),
    (53, Who::Me, "Dot B has to wait until the crane is moved off the pier."),
    (57, Who::Me, "Then the public launch of the harbour map moves to March 14 instead of March 1."),
    (64, Who::Me, "Agreed."),
    (71, Who::Me, "I will send the revised timeline to the city council by Friday."),
    (124, Who::Me, "Good, the other open item is the pier lighting, the LED fixtures came in at $40,000, which is over budget."),
    (131, Who::Me, "We decided last month to cap lighting at 30,000, so we should ask the supplier for a cheaper"),
    (145, Who::Me, "fixture."),
    (148, Who::Me, "One question is still open, does the ferry timetable change go out with the map, or a week after it?"),
    (156, Who::Me, "Let's decide that next week, once the council answers"),
    (159, Who::Me, "about the births."),
    (163, Who::Me, "One more change: the public launch of the harbour map moves again to March 21 because the country is a market."),
    (169, Who::Me, "Council meets late."),
];

#[ignore = "replays short calls with the meeting engine SONA_PROBE_ENGINE names"]
#[test]
fn live_doc_replay_probe() {
    let generator = generator();
    println!("\nsona build {}", env!("SONA_BUILD_COMMIT"));
    for (name, call) in [
        ("pier lamps", PIER),
        ("small talk", SMALL_TALK),
        ("harbor map", HARBOR),
        ("launch sync", CALL),
    ] {
        let (_directory, store) = workflow_core_tests::store();
        let began_utc_ms = Local::now().timestamp_millis() - 20 * 60 * 1_000;
        let session_id = workflow_core_tests::reviewable_meeting(&store, name, began_utc_ms);
        let tracks = workflow_core_tests::recording_tracks(&store, session_id);
        let service = Arc::new(MeetingProcessingService::new(None));
        service.set_text_generators(Arc::clone(&generator), Arc::clone(&generator));
        let live = LiveTranscript::new();
        assert!(live.start_doc());
        service.read_live_doc_now(&store, session_id, &live);
        let end_s = call.last().map_or(0, |line| line.0 + 8);
        let mut lines = call.iter().peekable();
        let mut pass_at = 0;
        let mut kept = 0;
        println!("\n{name}:");
        while pass_at <= end_s {
            pass_at += PASS_EVERY_S;
            while let Some(&&(second, who, text)) = lines.peek() {
                if second + 8 > pass_at {
                    break;
                }
                live.append(segment(second, who, text, tracks));
                lines.next();
            }
            if tick(&service, &store, session_id, &live, Moment::Pass) {
                for point in &live.live_doc(session_id).points[kept..] {
                    println!("  {pass_at:>4} s  • {}", point.text);
                }
                kept = live.doc_points().len();
            }
        }
        hang_up(&service, &store, session_id, &live);
        let doc = live.live_doc(session_id);
        for point in doc.points.iter().skip(kept) {
            println!("  stop    • {}", point.text);
        }
        println!("  {} points from {} readings; the doc at the stop:", doc.points.len(), doc.model_requests);
        for point in &doc.points {
            println!("    • {}", point.text);
        }
    }
}

#[ignore = "reads a synthesized call with the meeting engine SONA_PROBE_ENGINE names"]
#[test]
fn live_doc_probe() {
    let generator = generator();
    let (_directory, store) = workflow_core_tests::store();
    let began_utc_ms = Local::now().timestamp_millis() - 20 * 60 * 1_000;
    let session_id = workflow_core_tests::reviewable_meeting(&store, "Launch sync", began_utc_ms);
    let tracks = workflow_core_tests::recording_tracks(&store, session_id);
    let service = Arc::new(MeetingProcessingService::new(None));
    service.set_text_generators(Arc::clone(&generator), Arc::clone(&generator));
    let live = LiveTranscript::new();

    let mut call: Vec<(u64, Who, &str)> = CALL.to_vec();
    call.extend([(START_AT_S, Who::Me, START), (RECAP_AT_S, Who::Me, RECAP)]);
    call.sort_by_key(|line| line.0);
    let end_s = call.last().map_or(0, |line| line.0 + 8);

    let started = Instant::now();
    let mut lines = call.iter().peekable();
    let mut pass_at = 0;
    let mut at_start: Option<Vec<String>> = None;
    let mut edited: Option<String> = None;
    let mut readings_after_edit = 0;
    while pass_at <= end_s {
        pass_at += PASS_EVERY_S;
        while let Some(&&(second, who, text)) = lines.peek() {
            if second + 8 > pass_at {
                break;
            }
            live.append(segment(second, who, text, tracks));
            lines.next();
        }
        if hear(&service, &store, session_id, &live) {
            service.read_live_doc_now(&store, session_id, &live);
        }
        live_card::tick(&service, &store, session_id, &live, live_card::Moment::Pass);
        let before = live.live_doc(session_id).model_requests;
        tick(&service, &store, session_id, &live, Moment::Pass);
        let doc = live.live_doc(session_id);
        if at_start.is_none() && doc.started && doc.model_requests > 0 {
            at_start = Some(doc.points.iter().map(|point| point.text.clone()).collect());
            println!("\ndoc at {pass_at} s, right after \"{START}\":");
            for point in &doc.points {
                println!("  • {}", point.text);
            }
        }
        if edited.is_some() && doc.model_requests > before {
            readings_after_edit += doc.model_requests - before;
        }
        if edited.is_none() && pass_at >= START_AT_S + 60 {
            if let Some(first) = doc.points.first() {
                println!("\nat {pass_at} s the first point \"{}\" is changed to \"{EDIT}\"", first.text);
                live.edit_doc(&Author::Owner, Some(&first.id), EDIT)
                    .expect("the edit kept");
                edited = Some(first.id.clone());
            }
        }
    }
    let live_seconds = started.elapsed().as_secs_f64();
    live_card::tick(&service, &store, session_id, &live, live_card::Moment::HangUp);
    hang_up(&service, &store, session_id, &live);

    let doc = live.live_doc(session_id);
    println!("\nsona build {}", env!("SONA_BUILD_COMMIT"));
    println!(
        "{} doc readings over {:.1} min of call, {live_seconds:.0} s of model time",
        doc.model_requests,
        f64::from(u32::try_from(end_s).unwrap_or(u32::MAX)) / 60.0
    );
    println!("asks heard:");
    for ask in &doc.asks {
        println!("  {:?} at {} s: \"{}\"", ask.intent, ask.start_offset_ns / 1_000_000_000, ask.words);
    }
    println!("doc at the stop:");
    for point in &doc.points {
        println!("  • {}{}", point.text, if point.edited { "  [yours]" } else { "" });
    }
    let kept = edited
        .as_ref()
        .and_then(|id| doc.points.iter().find(|point| &point.id == id))
        .is_some_and(|point| point.text == EDIT);
    println!("changed point kept through {readings_after_edit} later readings: {kept}");
    println!("board:");
    for promise in live.live_promises() {
        println!("  {}", board_line(&promise));
    }

    let page = store.live_doc_points(session_id).expect("the page reads the doc");
    println!(
        "meeting page after the stop: {} points ({} yours)",
        page.len(),
        page.iter().filter(|point| point.edited).count(),
    );
    println!(
        "spoken asks in the doc: {}",
        doc.points
            .iter()
            .any(|point| point.text.contains("start a doc") || point.text.contains("recap so far"))
    );
}

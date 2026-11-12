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

use super::super::live_card::probe::{engine, segment, Who, CALL, PASS_EVERY_S};
use super::super::live_card::{self, MeetingLivePromise};
use super::super::LocalEndpointGenerator;
use super::*;
use crate::meeting::store::workflow_core_tests;
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

#[ignore = "reads a synthesized call with the meeting engine SONA_PROBE_ENGINE names"]
#[test]
fn live_doc_probe() {
    let generator: Arc<dyn MeetingTextGenerator> = Arc::new(
        LocalEndpointGenerator::from_settings(&engine())
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
                live.edit_doc(Some(&first.id), EDIT).expect("the edit kept");
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

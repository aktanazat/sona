use super::super::live_share::{SharePhase, SharedMeeting};
use super::super::ReplyShape;
use super::*;
use crate::meeting::analytics::MeetingProvisionalSpeaker;
use crate::meeting::live_doc_types::MeetingLiveAuthor;
use crate::meeting::store::{workflow_core_tests, LiveRoom};
use crate::meeting::types::SourceTrackId;
use chrono::Local;
use std::borrow::Cow;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use zeroize::Zeroizing;

/// An engine that answers each reading with the next scripted reply and
/// keeps what it was given. Its check keeps every note but the ones whose
/// text is in `dropped`.
#[derive(Default)]
struct Scripted {
    replies: Mutex<VecDeque<&'static str>>,
    dropped: Mutex<Vec<&'static str>>,
    given: Mutex<Vec<String>>,
}

impl Scripted {
    fn verdicts(&self, evidence: &str) -> String {
        // PANIC: the check always sends its notes as JSON.
        let notes: serde_json::Value = serde_json::from_str(evidence).unwrap();
        // PANIC: a poisoned fixture lock means an earlier assertion already failed.
        let dropped = self.dropped.lock().unwrap();
        // PANIC: the check's notes are always a list.
        let verdicts: Vec<serde_json::Value> = notes["notes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|note| {
                let keep = !dropped.iter().any(|text| note["text"] == *text);
                serde_json::json!({ "note": note["note"], "keep": keep })
            })
            .collect();
        serde_json::json!({ "notes": verdicts }).to_string()
    }
}

impl MeetingTextGenerator for Scripted {
    fn is_available(&self) -> bool {
        true
    }

    fn model_id(&self) -> &'static str {
        "apple-intelligence"
    }

    fn model_version(&self) -> Cow<'static, str> {
        Cow::Borrowed("scripted-v1")
    }

    fn max_input_bytes(&self) -> usize {
        64 * 1024
    }

    fn generate(
        &self,
        system_prompt: &str,
        evidence: &str,
        _max_tokens: i32,
        _shape: ReplyShape,
    ) -> Result<String, MeetingTextGenerationError> {
        if system_prompt == CHECK_PROMPT {
            return Ok(self.verdicts(evidence));
        }
        // PANIC: a poisoned fixture lock means an earlier assertion already failed.
        self.given.lock().unwrap().push(evidence.to_string());
        Ok(self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(r#"{"add":[],"rewrite":[]}"#)
            .to_string())
    }
}

fn line(track_id: SourceTrackId, source_kind: SourceKind, second: u64, text: &str) -> LiveSegment {
    LiveSegment {
        track_id,
        source_kind,
        speaker: MeetingProvisionalSpeaker::Unknown,
        start_offset_ns: second * 1_000_000_000,
        end_offset_ns: (second + 5) * 1_000_000_000,
        text: text.to_string(),
    }
}

#[test]
fn a_spoken_start_reads_what_was_said_and_a_hand_edit_outlives_later_readings() {
    let (_directory, store) = workflow_core_tests::store();
    let session_id =
        workflow_core_tests::reviewable_meeting(&store, "Launch sync", Local::now().timestamp_millis());
    let (microphone, call) = workflow_core_tests::recording_tracks(&store, session_id);
    let engine = Arc::new(Scripted::default());
    engine.replies.lock().unwrap().extend([
        r#"{"add":[{"text":"Beta ships Friday","lines":["L1"]},{"text":"Made up","lines":["L9"]}],"rewrite":[]}"#,
        r#"{"add":[{"text":"Dana owns the pricing page","lines":["L1"]}],"rewrite":[{"point":"P1","text":"Beta ships Friday at noon","lines":["L1"]}]}"#,
        r#"{"add":[{"text":"Legal signs off next week","lines":["L1"]}],"rewrite":[{"point":"P2","text":"Dana owns pricing","lines":["L1"]}]}"#,
    ]);
    let service = MeetingProcessingService::new(None);
    service.set_text_generators(engine.clone(), engine.clone());
    let live = LiveTranscript::new();

    live.append(line(call, SourceKind::SystemAudio, 4, "Sona, start a document."));
    live.append(line(microphone, SourceKind::Microphone, 10, "The beta ships Friday."));
    tick(&service, &store, session_id, &live, Moment::Pass);
    assert!(!hear(&service, &store, session_id, &live));
    assert!(engine.given.lock().unwrap().is_empty(), "nothing is read before the doc is asked for");

    live.append(line(microphone, SourceKind::Microphone, 20, "Okay Sona, start a doc for this plan."));
    assert!(hear(&service, &store, session_id, &live), "a spoken start starts the doc");
    assert!(!tick(&service, &store, session_id, &live, Moment::Pass), "the start holds the reading");
    service.read_live_doc_now(&store, session_id, &live);
    let doc = live.live_doc(session_id);
    assert_eq!(doc.asks.len(), 1, "the far end cannot ask: {:?}", doc.asks);
    assert_eq!(doc.asks[0].intent, MeetingVoiceIntent::StartDoc);
    let texts: Vec<&str> = doc.points.iter().map(|point| point.text.as_str()).collect();
    assert_eq!(texts, ["Beta ships Friday"], "a point citing no line it was given is dropped");
    let given = engine.given.lock().unwrap().concat();
    assert!(given.contains("The beta ships Friday."), "{given}");
    assert!(!given.contains("for this plan"), "an ask is never read into the doc: {given}");

    let first = doc.points[0].id.clone();
    live.edit_doc(
        &Author::Owner,
        Some(&first),
        "Beta ships Monday, not Friday",
    )
    .unwrap();
    live.append(line(call, SourceKind::SystemAudio, 70, "I'll own the pricing page."));
    tick(&service, &store, session_id, &live, Moment::Pass);
    let texts: Vec<String> = live.doc_points().into_iter().map(|point| point.text).collect();
    assert_eq!(texts, ["Beta ships Monday, not Friday", "Dana owns the pricing page"]);

    let mut page = store.live_doc_points(session_id).unwrap();
    let dana = page[1].id.clone();
    edit(&mut page, Some(&dana), "Dana owns pricing and the FAQ").unwrap();
    edit(&mut page, Some(&first), "").unwrap();
    store.save_live_doc_points(session_id, &page).unwrap();
    let mut changes = store.live_changes(session_id).unwrap();
    live_changes::push(
        &mut changes,
        Author::Owner.change(
            None,
            MeetingLiveTarget::Point,
            &dana,
            MeetingLiveChangeKind::Edited,
            "Dana owns pricing and the FAQ",
        ),
    );
    store.save_live_changes(session_id, &changes).unwrap();
    live.append(line(call, SourceKind::SystemAudio, 90, "Legal signs off next week."));
    hang_up(&service, &store, session_id, &live);
    let saved: Vec<(String, bool)> = store
        .live_doc_points(session_id)
        .unwrap()
        .into_iter()
        .map(|point| (point.text, point.edited))
        .collect();
    assert_eq!(
        saved,
        [
            ("Dana owns pricing and the FAQ".to_string(), true),
            ("Legal signs off next week".to_string(), false),
        ],
        "what the page changed after the stop outlives the stop's reading"
    );
    live_changes::save(&store, session_id, &live);
    let marks: Vec<(MeetingLiveAuthor, MeetingLiveChangeKind, String)> = live
        .live_changes()
        .into_iter()
        .filter(|change| change.target_id == dana)
        .map(|change| (change.by, change.kind, change.text))
        .collect();
    assert_eq!(
        marks,
        [
            (
                MeetingLiveAuthor::Sona,
                MeetingLiveChangeKind::Added,
                "Dana owns the pricing page".to_string()
            ),
            (
                MeetingLiveAuthor::Owner,
                MeetingLiveChangeKind::Edited,
                "Dana owns pricing and the FAQ".to_string()
            ),
        ],
        "the call ends marked with the page's change, not the stop's rewrite it outlived"
    );
}

#[test]
fn a_reading_keeps_only_points_its_lines_say_that_pass_the_check() {
    let (_directory, store) = workflow_core_tests::store();
    let session_id =
        workflow_core_tests::reviewable_meeting(&store, "Pier walk", Local::now().timestamp_millis());
    let (microphone, call) = workflow_core_tests::recording_tracks(&store, session_id);
    let engine = Arc::new(Scripted::default());
    engine.replies.lock().unwrap().extend([
        r#"{"add":[{"text":"No new detail from L1.","lines":["L1"]},{"text":"Screen froze again","lines":["L1"]},{"text":"California.","lines":["L1"]},{"text":"P2: The berth three lamps flicker after dark","lines":["L2"]}],"rewrite":[]}"#,
        r#"{"add":[],"rewrite":[{"point":"P1","text":"Thanks everyone for joining","lines":["L1"]}]}"#,
    ]);
    engine.dropped.lock().unwrap().push("Screen froze again");
    let service = MeetingProcessingService::new(None);
    service.set_text_generators(engine.clone(), engine.clone());
    let live = LiveTranscript::new();

    live.append(line(call, SourceKind::SystemAudio, 0, "California."));
    live.append(line(microphone, SourceKind::Microphone, 10, "The lamps on berth three still flicker after dark."));
    assert!(live.start_doc());
    service.read_live_doc_now(&store, session_id, &live);
    let texts: Vec<String> = live.doc_points().into_iter().map(|point| point.text).collect();
    assert_eq!(
        texts,
        ["The berth three lamps flicker after dark"],
        "a point about a label, of one word, or one the check drops is not kept; a leading label is set aside"
    );

    live.append(line(call, SourceKind::SystemAudio, 60, "Okay, thanks everyone, talk soon."));
    assert!(!tick(&service, &store, session_id, &live, Moment::Pass));
    let texts: Vec<String> = live.doc_points().into_iter().map(|point| point.text).collect();
    assert_eq!(
        texts,
        ["The berth three lamps flicker after dark"],
        "a rewrite onto another topic leaves the point as it was"
    );
    assert_eq!(engine.given.lock().unwrap().len(), 2, "both readings ran");
}

#[test]
fn only_a_line_that_opens_with_sona_is_an_ask() {
    let read = voice_ask;
    assert_eq!(
        read("Sona, start a doc for this plan."),
        Some((MeetingVoiceIntent::StartDoc, "start a doc for this plan.".to_string()))
    );
    assert_eq!(
        read("Hey Sona, show me the board"),
        Some((MeetingVoiceIntent::Board, "show me the board".to_string()))
    );
    assert_eq!(read("OK, Sona: recap so far").map(|ask| ask.0), Some(MeetingVoiceIntent::Recap));
    assert_eq!(
        read("Sona, what did Dana say about pricing last week?").map(|ask| ask.0),
        Some(MeetingVoiceIntent::Ask)
    );
    assert_eq!(read("We moved Sona to the new pricing page."), None);
    assert_eq!(read("So Sona already files those in Triage."), None, "talk about Sona asks nothing");
    assert_eq!(read("Sona recap so far").map(|ask| ask.0), Some(MeetingVoiceIntent::Recap));
    assert_eq!(read("So and then Sona, start a doc"), None, "at most two words before the name");
    assert_eq!(read("Sona."), None, "a name alone asks nothing");
}

/// A new point from a teammate's browser, as the page sends it.
fn teammate_point(id: &str, name: &str, text: &str) -> Vec<u8> {
    serde_json::json!({ "v": 1, "id": id, "name": name, "target": "point", "target_id": null, "text": text })
        .to_string()
        .into_bytes()
}

fn shared_meeting() -> (
    tempfile::TempDir,
    Arc<MeetingStore>,
    MeetingSessionId,
    Arc<LiveTranscript>,
    SharedMeeting,
) {
    let (directory, store) = workflow_core_tests::store();
    let session_id = workflow_core_tests::reviewable_meeting(
        &store,
        "Harbor map sync",
        Local::now().timestamp_millis(),
    );
    let (microphone, _call) = workflow_core_tests::recording_tracks(&store, session_id);
    let live = Arc::new(LiveTranscript::new());
    live.append(line(
        microphone,
        SourceKind::Microphone,
        10,
        "The launch moves to March 14.",
    ));
    let shared = SharedMeeting::new(
        Arc::new(MeetingProcessingService::new(None)),
        store.clone(),
        session_id,
        live.clone(),
    );
    (directory, store, session_id, live, shared)
}

#[test]
fn a_link_keeps_its_first_room_for_a_later_launch_to_empty() {
    let (_directory, store, session_id, _live, shared) = shared_meeting();
    let now = Local::now().timestamp_millis();
    let room = |id: &str| LiveRoom {
        id: id.to_owned(),
        socket: format!("wss://companion.example/v1/live/{id}/socket?t=host"),
        frame_key: Zeroizing::new([7; 32]),
        expires_at_utc_ms: now + 60_000,
    };
    let first = "https://companion.example/l/first#v=1&k=a".to_owned();
    assert_eq!(
        shared.keep_link(first.clone(), &room("first")).unwrap(),
        (first.clone(), true)
    );
    assert_eq!(
        shared
            .keep_link("https://companion.example/l/second#v=1&k=b".to_owned(), &room("second"))
            .unwrap(),
        (first, false)
    );
    let kept = store.kept_live_rooms(now).unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(
        (kept[0].session_id, kept[0].room.id.as_str(), kept[0].deleted),
        (session_id, "first", false)
    );
}

#[test]
fn a_teammates_edit_sent_twice_is_kept_once_under_their_name_and_call_time() {
    let (_directory, store, session_id, _live, shared) = shared_meeting();
    let edit = teammate_point(
        "Jt2ByXLZUKwIdKUh9hCF2D",
        " Priya \n Nair ",
        "Launch stays March 14",
    );

    assert!(shared.apply(&edit));
    assert!(
        !shared.apply(&edit),
        "the same edit sent again changes nothing"
    );
    let texts: Vec<String> = store
        .live_doc_points(session_id)
        .unwrap()
        .into_iter()
        .map(|point| point.text)
        .collect();
    assert_eq!(texts, ["Launch stays March 14"]);
    let marks: Vec<(MeetingLiveAuthor, Option<String>, Option<u64>)> = store
        .live_changes(session_id)
        .unwrap()
        .into_iter()
        .map(|change| (change.by, change.name, change.call_ns))
        .collect();
    assert_eq!(
        marks,
        [(
            MeetingLiveAuthor::Teammate,
            Some("Priya Nair".to_string()),
            Some(15_000_000_000)
        )],
        "one change, by the name the teammate gave, as far into the call as was heard"
    );
}

#[test]
fn the_link_takes_no_edit_once_the_call_stops() {
    let (_directory, store, session_id, live, shared) = shared_meeting();

    live.close_share();
    assert!(!shared.apply(&teammate_point(
        "Jt2ByXLZUKwIdKUh9hCF2D",
        "Priya",
        "Launch moves to April"
    )));
    assert!(live.doc_points().is_empty());
    assert!(store.live_changes(session_id).unwrap().is_empty());
}

#[test]
fn a_teammate_cannot_edit_under_the_calls_own_names() {
    let (_directory, store, session_id, live, shared) = shared_meeting();

    for name in ["Sona", " host ", "YOU"] {
        assert!(
            !shared.apply(&teammate_point(
                "Jt2ByXLZUKwIdKUh9hCF2D",
                name,
                "Launch moves to April"
            )),
            "{name}"
        );
    }
    assert!(live.doc_points().is_empty());
    assert!(store.live_changes(session_id).unwrap().is_empty());
}

#[test]
fn a_moved_date_replaces_the_point_it_moves_instead_of_adding_a_second() {
    let (_directory, store) = workflow_core_tests::store();
    let session_id =
        workflow_core_tests::reviewable_meeting(&store, "Harbor map sync", Local::now().timestamp_millis());
    let (microphone, _call) = workflow_core_tests::recording_tracks(&store, session_id);
    let engine = Arc::new(Scripted::default());
    engine.replies.lock().unwrap().extend([
        r#"{"add":[{"text":"The harbour map launch is set for March 14","lines":["L1"]}],"rewrite":[]}"#,
        r#"{"add":[{"text":"The ferry timetable change goes out with the harbour map","lines":["L2"]},{"text":"The public launch of the harbour map is now set for March 21","lines":["L1"]}],"rewrite":[{"point":"P1","text":"The harbour map launch is confirmed for March 21","lines":["L1"]}]}"#,
        r#"{"add":[{"text":"The public launch of the harbour map moves to March 25","lines":["L1"]}],"rewrite":[{"point":"P1","text":"The harbour map launch is set for March 25","lines":["L1"]}]}"#,
        r#"{"add":[{"text":"The public launch of the harbour map moves to March 28","lines":["L1"]}],"rewrite":[{"point":"P1","text":"The harbour map launch is confirmed for March 28","lines":["L1"]}]}"#,
    ]);
    engine.dropped.lock().unwrap().extend([
        "The harbour map launch is confirmed for March 21",
        "The harbour map launch is confirmed for March 28",
    ]);
    let service = MeetingProcessingService::new(None);
    service.set_text_generators(engine.clone(), engine.clone());
    let live = LiveTranscript::new();
    assert!(live.start_doc());

    live.append(line(microphone, SourceKind::Microphone, 57, "The public launch of the harbour map moves to March 14."));
    service.read_live_doc_now(&store, session_id, &live);
    let launch = live.doc_points()[0].id.clone();
    live.append(line(microphone, SourceKind::Microphone, 163, "The public launch of the harbour map moves again to March 21."));
    live.append(line(microphone, SourceKind::Microphone, 170, "Does the ferry timetable change go out with the harbour map?"));
    service.read_live_doc_now(&store, session_id, &live);

    let points: Vec<(String, String)> = live.doc_points().into_iter().map(|point| (point.id, point.text)).collect();
    let texts: Vec<&str> = points.iter().map(|(_, text)| text.as_str()).collect();
    assert_eq!(
        texts,
        ["The public launch of the harbour map is now set for March 21", "The ferry timetable change goes out with the harbour map"],
        "the doc never holds both dates, and an add that only shares the map's name stays its own point"
    );
    assert_eq!(points[0].0, launch, "the new date replaces the launch point rather than adding one");

    live.append(line(microphone, SourceKind::Microphone, 200, "The launch slips once more, to March 25."));
    service.read_live_doc_now(&store, session_id, &live);
    let texts: Vec<String> = live.doc_points().into_iter().map(|point| point.text).collect();
    assert_eq!(
        texts,
        ["The harbour map launch is set for March 25", "The ferry timetable change goes out with the harbour map"],
        "an add that repeats a rewrite the check kept goes"
    );

    live.edit_doc(&Author::Owner, Some(&launch), "Launch March 25, pending the council").unwrap();
    live.append(line(microphone, SourceKind::Microphone, 240, "The council wants another week, so the launch moves to March 28."));
    service.read_live_doc_now(&store, session_id, &live);
    let texts: Vec<String> = live.doc_points().into_iter().map(|point| point.text).collect();
    assert_eq!(
        texts,
        [
            "Launch March 25, pending the council",
            "The ferry timetable change goes out with the harbour map",
            "The public launch of the harbour map moves to March 28",
        ],
        "a point the user wrote stays theirs, and the new date still lands"
    );
}

#[test]
fn the_page_shows_a_sentence_cut_in_two_as_one_line() {
    let (_directory, _store, _session_id, live, shared) = shared_meeting();
    let (microphone, call) = (SourceTrackId::new(), SourceTrackId::new());
    let first = MeetingProvisionalSpeaker::Cluster { index: 0 };
    let unknown = MeetingProvisionalSpeaker::Unknown;
    for (track, kind, speaker, start_ms, end_ms, text) in [
        (microphone, SourceKind::Microphone, first, 20_000, 23_000, "We should ask the supplier for a cheaper"),
        (call, SourceKind::SystemAudio, unknown, 21_000, 22_000, "Mm-hmm."),
        (microphone, SourceKind::Microphone, unknown, 23_000, 25_000, "cheaper fixture."),
        (microphone, SourceKind::Microphone, first, 30_000, 33_000, "Then the public launch moves to March 14."),
        (microphone, SourceKind::Microphone, unknown, 33_000, 34_500, "Instead of March 1st."),
        (microphone, SourceKind::Microphone, first, 36_000, 39_000, "Let's decide next week, once the council answers."),
        (microphone, SourceKind::Microphone, first, 38_500, 40_000, "council answers about the berths."),
        (microphone, SourceKind::Microphone, first, 40_500, 42_000, "Let's move on"),
        (microphone, SourceKind::Microphone, first, 50_000, 53_000, "Okay, next item is the pier lighting."),
        (microphone, SourceKind::Microphone, first, 60_000, 63_000, "So the lighting budget is"),
        (call, SourceKind::SystemAudio, unknown, 63_000, 65_000, "Council meets late."),
        (call, SourceKind::SystemAudio, unknown, 66_000, 67_000, "Agreed."),
    ] {
        live.append(LiveSegment {
            speaker,
            start_offset_ns: start_ms * 1_000_000,
            end_offset_ns: end_ms * 1_000_000,
            ..line(track, kind, 0, text)
        });
    }

    let state: serde_json::Value = serde_json::from_slice(&shared.state(SharePhase::Live).unwrap()).unwrap();
    let lines: Vec<(&str, &str)> = state["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| (line["who"].as_str().unwrap(), line["text"].as_str().unwrap()))
        .collect();
    assert_eq!(
        lines,
        [
            ("You", "The launch moves to March 14."),
            ("You", "We should ask the supplier for a cheaper fixture."),
            ("Call", "Mm-hmm."),
            ("You", "Then the public launch moves to March 14. Instead of March 1st."),
            ("You", "Let's decide next week, once the council answers about the berths."),
            ("You", "Let's move on"),
            ("You", "Okay, next item is the pier lighting."),
            ("You", "So the lighting budget is"),
            ("Call", "Council meets late."),
            ("Call", "Agreed."),
        ],
        "a piece joins the line it runs on from, whatever its punctuation or speaker guess, and never across a pause or another track"
    );
}

#[test]
fn a_link_stops_showing_the_meeting_once_its_series_is_kept_on_this_mac() {
    use crate::meeting::detection::machine::CalendarEventSummary;
    use crate::meeting::series_types::MeetingSeriesRemoteOptOutSetRequest;
    use crate::meeting::types::MeetingOperationId;
    use super::super::live_share::REMOVED;

    let (_directory, store, session_id, live, shared) = shared_meeting();
    let started = Local::now().timestamp_millis();
    let event = CalendarEventSummary {
        event_key: "harbor-sync#1".to_owned(),
        series_key: "harbor-sync".to_owned(),
        title: "Harbor map sync".to_owned(),
        attendee_count: 2,
        start_utc_ms: started,
        end_utc_ms: started + 1_800_000,
        attendees: Vec::new(),
        notes: None,
        calendar_name: None,
        location: None,
        url: None,
        place: None,
    };
    store.remember_calendar_facts(session_id, &event).unwrap();
    assert_ne!(shared.state(SharePhase::Live).unwrap(), REMOVED);

    store
        .set_series_remote_opt_out(
            &MeetingSeriesRemoteOptOutSetRequest {
                operation_id: MeetingOperationId::new(),
                series_key: event.series_key.clone(),
                remote_intelligence_opt_out: true,
                expected_revision: 0,
            },
            3,
        )
        .unwrap();

    assert_eq!(shared.state(SharePhase::Live).unwrap(), REMOVED);
    assert_eq!(live.share_phase(), SharePhase::Withdrawn);
}

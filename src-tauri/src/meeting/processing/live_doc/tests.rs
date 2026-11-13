use super::super::ReplyShape;
use super::*;
use crate::meeting::analytics::MeetingProvisionalSpeaker;
use crate::meeting::store::workflow_core_tests;
use crate::meeting::types::SourceTrackId;
use chrono::Local;
use std::borrow::Cow;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

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
    live.edit_doc(Some(&first), "Beta ships Monday, not Friday").unwrap();
    live.append(line(call, SourceKind::SystemAudio, 70, "I'll own the pricing page."));
    tick(&service, &store, session_id, &live, Moment::Pass);
    let texts: Vec<String> = live.doc_points().into_iter().map(|point| point.text).collect();
    assert_eq!(texts, ["Beta ships Monday, not Friday", "Dana owns the pricing page"]);

    let mut page = store.live_doc_points(session_id).unwrap();
    let dana = page[1].id.clone();
    edit(&mut page, Some(&dana), "Dana owns pricing and the FAQ").unwrap();
    edit(&mut page, Some(&first), "").unwrap();
    store.save_live_doc_points(session_id, &page).unwrap();
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

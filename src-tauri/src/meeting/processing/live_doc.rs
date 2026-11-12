//! The doc a meeting builds on request, and what the user says to Sona.
//!
//! Nothing is read until the doc is asked for, typed or said. Then the
//! first reading takes everything said so far, and later ones take the new
//! lines at most once per `READ_EVERY_NS` of meeting. A reading may add
//! points and rewrite points it wrote; a point the user wrote or changed is
//! never rewritten or dropped.
//!
//! A line on the user's own microphone that opens with "Sona" is an ask,
//! not part of the meeting: it is never read into the doc. People on the
//! far end of a call are on the call audio, so they cannot ask.

use super::{
    evidence_budget, first_json_value, json_response_schema, LiveSegment, LiveTranscript,
    MeetingProcessingService, MeetingTextGenerationError, MeetingTextGenerator,
};
use crate::meeting::ledger::fold;
use crate::meeting::live_doc_types::{
    MeetingLiveDoc, MeetingLiveDocPoint, MeetingVoiceAsk, MeetingVoiceIntent,
};
use crate::meeting::store::MeetingStore;
use crate::meeting::types::{MeetingSessionId, SourceKind};
use schemars::{schema::RootSchema, JsonSchema};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, MutexGuard};
use std::time::Duration;
use uuid::Uuid;

const PROMPT: &str = include_str!("../../../resources/prompts/live_doc.txt");
const OUTPUT_TOKENS: i32 = 900;
pub(crate) const MAX_POINTS: usize = 40;
pub(crate) const MAX_POINT_CHARS: usize = 280;
/// Points one reading may add or rewrite.
const MAX_WRITTEN: usize = 12;
/// Meeting time between two readings once the doc has caught up.
const READ_EVERY_NS: u64 = 40_000_000_000;
/// Readings a start may make to catch up on what was already said.
const START_REQUESTS: usize = 4;
/// Readings the stop may make to read what is left.
const HANG_UP_REQUESTS: usize = 6;
const MAX_ASK_CHARS: usize = 500;
/// `{"doc":[],"lines":[]}`, the request around the points and lines.
const ENVELOPE_BYTES: usize = 22;
/// Words that may come before "Sona" in an ask.
const OPENERS: &[&str] = &["hey", "hi", "ok", "okay", "so", "and"];
/// Words that make "Sona" an ask with no pause after the name, as in "Sona
/// recap so far". Any other word is talk about Sona: "So Sona already files
/// those".
const COMMANDS: &[&str] = &[
    "start", "take", "keep", "make", "show", "open", "put", "recap", "summarize", "summarise",
    "give", "tell", "find", "look", "search", "pull", "what", "who", "when", "where", "why",
    "how", "which",
];
static RESPONSE_SCHEMA: LazyLock<RootSchema> = LazyLock::new(json_response_schema::<Reply>);

/// What the doc holds for one capture.
#[derive(Default)]
pub(super) struct Doc {
    started: bool,
    /// Segments, in the order the pass appended them, given to a reading.
    read: usize,
    asked_at_ns: Option<u64>,
    points: Vec<MeetingLiveDocPoint>,
    requests: u32,
    /// Segments, in append order, already checked for an ask.
    heard: usize,
    asks: Vec<MeetingVoiceAsk>,
    /// A reading is under way, so another would read the same lines.
    reading: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Moment {
    /// The background pass: at most one reading, at most once per beat.
    Pass,
    /// The doc was just asked for: catch up on everything said so far.
    Start,
    /// The stop: everything still unread, now.
    HangUp,
}

/// New points and rewrites come in separate lists: asked for one list with
/// an optional label, local models number their new points "P1", "P2", and
/// those labels name points the doc already has.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Reply {
    #[schemars(length(max = "MAX_WRITTEN"))]
    add: Vec<Added>,
    #[schemars(length(max = "MAX_WRITTEN"))]
    rewrite: Vec<Rewritten>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Added {
    text: String,
    lines: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Rewritten {
    point: String,
    text: String,
    lines: Vec<String>,
}

/// A point to add, when `point` is `None`, or to rewrite.
struct Written {
    point: Option<String>,
    text: String,
    lines: Vec<String>,
}

#[derive(Serialize)]
struct Shown<'a> {
    point: String,
    text: &'a str,
    locked: bool,
}

/// A line as the reading sees it, without who said it: given "Caller 1" or
/// "Speaker", local models wrote those labels into the doc's points.
#[derive(Serialize)]
struct Line<'a> {
    line: String,
    text: &'a str,
}

#[derive(Serialize)]
struct Input<'a> {
    doc: Vec<Shown<'a>>,
    lines: Vec<Line<'a>>,
}

/// One reading: the request text, the point each label names, when each
/// line was said, and the append position the doc has read through once
/// it is answered.
struct Request {
    input: String,
    points: HashMap<String, String>,
    lines: Vec<u64>,
    through: usize,
}

/// Why a hand edit was refused.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum EditRefusal {
    NotFound,
    Invalid,
    Full,
}

impl LiveTranscript {
    fn doc(&self) -> MutexGuard<'_, Doc> {
        self.doc
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The doc as the live screen shows it.
    pub(crate) fn live_doc(&self, session_id: MeetingSessionId) -> MeetingLiveDoc {
        let doc = self.doc();
        MeetingLiveDoc {
            session_id,
            started: doc.started,
            points: doc.points.clone(),
            asks: doc.asks.clone(),
            model_requests: doc.requests,
            live: true,
        }
    }

    /// Ask for the doc. Returns whether it was not started before; the
    /// caller then holds the reading and must run [`MeetingProcessingService::read_live_doc_now`].
    pub(crate) fn start_doc(&self) -> bool {
        start(&mut self.doc())
    }

    /// A hand edit to the running doc; see [`edit`].
    pub(crate) fn edit_doc(&self, point_id: Option<&str>, text: &str) -> Result<(), EditRefusal> {
        let mut doc = self.doc();
        edit(&mut doc.points, point_id, text)?;
        doc.started = true;
        Ok(())
    }

    pub(crate) fn doc_points(&self) -> Vec<MeetingLiveDocPoint> {
        self.doc().points.clone()
    }
}

/// A hand edit: new text for a point, a new point when `point_id` is
/// `None`, or the point removed when the text is blank. The point is the
/// user's from then on.
pub(crate) fn edit(
    points: &mut Vec<MeetingLiveDocPoint>,
    point_id: Option<&str>,
    text: &str,
) -> Result<(), EditRefusal> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() > MAX_POINT_CHARS || text.chars().any(char::is_control) {
        return Err(EditRefusal::Invalid);
    }
    match point_id {
        Some(point_id) => {
            let at = points
                .iter()
                .position(|point| point.id == point_id)
                .ok_or(EditRefusal::NotFound)?;
            if text.is_empty() {
                points.remove(at);
            } else {
                points[at].text = text;
                points[at].edited = true;
            }
        }
        None if text.is_empty() => return Err(EditRefusal::Invalid),
        None if points.len() >= MAX_POINTS => return Err(EditRefusal::Full),
        None => points.push(MeetingLiveDocPoint {
            id: Uuid::new_v4().to_string(),
            text,
            edited: true,
            start_offset_ns: None,
        }),
    }
    Ok(())
}

impl MeetingProcessingService {
    /// The doc was just started, which claimed the reading: read what was
    /// said so far, then tell the screens, which wait on this reading even
    /// when it added nothing.
    pub(crate) fn read_live_doc_now(
        &self,
        store: &MeetingStore,
        session_id: MeetingSessionId,
        live: &LiveTranscript,
    ) {
        read_and_save(self, store, session_id, live, Moment::Start);
        self.emit_current(store, "meeting:live-doc-changed", session_id);
    }
}

/// Start the doc and claim its reading, unless it was started before.
fn start(doc: &mut Doc) -> bool {
    if doc.started {
        return false;
    }
    doc.started = true;
    doc.reading = true;
    true
}

/// Claim the reading when no other reading holds it.
fn claim(live: &LiveTranscript) -> bool {
    let mut doc = live.doc();
    !std::mem::replace(&mut doc.reading, true)
}

/// Hear the asks in the lines the pass just added and tell the screens at
/// once, before any reading. Returns whether an ask started the doc; the
/// caller then runs [`MeetingProcessingService::read_live_doc_now`], off
/// the pass so the next asks are heard while it reads.
pub(crate) fn hear(
    service: &MeetingProcessingService,
    store: &MeetingStore,
    session_id: MeetingSessionId,
    live: &LiveTranscript,
) -> bool {
    let (heard, started) = hear_asks(live);
    if heard {
        service.emit_current(store, "meeting:live-doc-changed", session_id);
    }
    started
}

/// Read new lines into the doc when it is started, no other reading runs
/// and one is due. Returns whether the doc changed; a changed doc is saved.
pub(crate) fn tick(
    service: &MeetingProcessingService,
    store: &MeetingStore,
    session_id: MeetingSessionId,
    live: &LiveTranscript,
    moment: Moment,
) -> bool {
    if !live.doc().started || !claim(live) {
        return false;
    }
    read_and_save(service, store, session_id, live, moment)
}

/// Read with the reading claimed, save what changed, then let it go. The
/// save comes first so the stop never reads a store behind the doc.
fn read_and_save(
    service: &MeetingProcessingService,
    store: &MeetingStore,
    session_id: MeetingSessionId,
    live: &LiveTranscript,
    moment: Moment,
) -> bool {
    let changed = read(service, store, session_id, live, moment);
    if changed {
        save(store, session_id, live);
    }
    live.doc().reading = false;
    changed
}

fn read(
    service: &MeetingProcessingService,
    store: &MeetingStore,
    session_id: MeetingSessionId,
    live: &LiveTranscript,
    moment: Moment,
) -> bool {
    let Some(generator) = service.text_generator_for_session(store, session_id) else {
        return false;
    };
    let budget = evidence_budget(generator.as_ref(), PROMPT, OUTPUT_TOKENS);
    let requests = match moment {
        Moment::Pass => 1,
        Moment::Start => START_REQUESTS,
        Moment::HangUp => HANG_UP_REQUESTS,
    };
    let mut changed = false;
    for _ in 0..requests {
        let Some(request) = next_request(live, moment, budget) else {
            break;
        };
        let written = match ask(generator.as_ref(), live, &request) {
            Asked::Answered(written) => written,
            Asked::Unanswered => Vec::new(),
            Asked::NotAsked => break,
        };
        let mut doc = live.doc();
        doc.read = doc.read.max(request.through);
        changed |= merge(&mut doc.points, written, &request);
    }
    changed
}

/// The doc's points into the store, so the meeting's page has them.
pub(crate) fn save(store: &MeetingStore, session_id: MeetingSessionId, live: &LiveTranscript) {
    if let Err(error) = store.save_live_doc_points(session_id, &live.doc_points()) {
        log::warn!("Could not keep the live doc of {session_id:?}: {error:?}");
    }
}

/// The stop: once a reading still under way is done, what the last pass
/// had not reached is read into the doc. The capture is gone by then, so
/// the meeting's page edits the saved doc; what the user changed, added or
/// removed there meanwhile wins over this reading.
pub(super) fn hang_up(
    service: &MeetingProcessingService,
    store: &MeetingStore,
    session_id: MeetingSessionId,
    live: &LiveTranscript,
) {
    if !live.doc().started {
        return;
    }
    while !claim(live) {
        std::thread::sleep(Duration::from_millis(250));
    }
    let before: HashSet<String> = live.doc_points().into_iter().map(|point| point.id).collect();
    read(service, store, session_id, live, Moment::HangUp);
    let saved = store.live_doc_points(session_id).unwrap_or_default();
    let mut doc = live.doc();
    doc.points = after_stop(saved, std::mem::take(&mut doc.points), &before);
    if let Err(error) = store.save_live_doc_points(session_id, &doc.points) {
        log::warn!("Could not keep the live doc of {session_id:?}: {error:?}");
    }
    log::info!(
        "Live doc for {session_id:?}: {} points from {} model requests",
        doc.points.len(),
        doc.requests
    );
    drop(doc);
    service.emit_current(store, "meeting:live-doc-changed", session_id);
}

/// The doc after the stop's reading: the saved copy, which holds every
/// change made on the meeting's page since the stop, with the reading's
/// newer words for points nobody touched and the points it added.
fn after_stop(
    saved: Vec<MeetingLiveDocPoint>,
    read: Vec<MeetingLiveDocPoint>,
    before: &HashSet<String>,
) -> Vec<MeetingLiveDocPoint> {
    let mut kept: Vec<MeetingLiveDocPoint> = saved
        .into_iter()
        .map(|point| match read.iter().find(|newer| newer.id == point.id) {
            Some(newer) if !point.edited => newer.clone(),
            _ => point,
        })
        .collect();
    kept.extend(read.into_iter().filter(|point| !before.contains(&point.id)));
    kept
}

/// The asks in lines the pass added since the last look. An ask to start
/// the doc starts it and claims its reading. Returns whether any ask was
/// heard and whether one started the doc.
fn hear_asks(live: &LiveTranscript) -> (bool, bool) {
    let segments = live
        .segments
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut doc = live.doc();
    let mut heard = false;
    let mut started = false;
    for segment in &segments[doc.heard.min(segments.len())..] {
        if segment.source_kind != SourceKind::Microphone {
            continue;
        }
        let Some((intent, words)) = voice_ask(&segment.text) else {
            continue;
        };
        if intent == MeetingVoiceIntent::StartDoc {
            started |= start(&mut doc);
        }
        let sequence = u32::try_from(doc.asks.len() + 1).unwrap_or(u32::MAX);
        doc.asks.push(MeetingVoiceAsk {
            sequence,
            intent,
            words,
            start_offset_ns: segment.start_offset_ns,
        });
        heard = true;
    }
    doc.heard = segments.len();
    (heard, started)
}

/// The ask in a line that opens with "Sona", and the words after it.
pub(crate) fn voice_ask(text: &str) -> Option<(MeetingVoiceIntent, String)> {
    let mut words = words(text);
    let mut openers = 0;
    let end = loop {
        let (word, end) = words.next()?;
        if word == "sona" {
            break end;
        }
        openers += 1;
        if openers > 2 || !OPENERS.contains(&word.as_str()) {
            return None;
        }
    };
    let after = &text[end..];
    let paused = after.trim_start().starts_with([',', ':', '.', '!', '?']);
    let rest = after
        .trim_start_matches(|character: char| !character.is_alphanumeric())
        .trim();
    if rest.is_empty() || rest.chars().count() > MAX_ASK_CHARS {
        return None;
    }
    let said: Vec<String> = words.map(|(word, _)| word).collect();
    if !paused && !said.first().is_some_and(|word| COMMANDS.contains(&word.as_str())) {
        return None;
    }
    let has = |wanted: &[&str]| said.iter().any(|word| wanted.contains(&word.as_str()));
    let intent = if has(&["doc", "docs", "document"])
        || (has(&["take", "start", "keep"]) && has(&["notes"]))
    {
        MeetingVoiceIntent::StartDoc
    } else if has(&["board", "whiteboard"]) {
        MeetingVoiceIntent::Board
    } else if has(&["recap", "summary", "summarize", "summarise"]) {
        MeetingVoiceIntent::Recap
    } else {
        MeetingVoiceIntent::Ask
    };
    Some((intent, rest.to_string()))
}

/// Each word of `text`, lowercased, with the byte offset just past it.
fn words(text: &str) -> impl Iterator<Item = (String, usize)> + '_ {
    let in_word = |character: char| character.is_alphanumeric() || character == '\'';
    let mut from = 0;
    std::iter::from_fn(move || {
        let start = from + text[from..].find(in_word)?;
        from = text[start..]
            .find(|character: char| !in_word(character))
            .map_or(text.len(), |length| start + length);
        Some((text[start..from].to_lowercase(), from))
    })
}

fn is_ask(segment: &LiveSegment) -> bool {
    segment.source_kind == SourceKind::Microphone && voice_ask(&segment.text).is_some()
}

enum Asked {
    Answered(Vec<Written>),
    /// Asked, and the answer was not a list of points: those lines are not
    /// read again.
    Unanswered,
    /// The engine could not be reached: the lines wait for the next reading.
    NotAsked,
}

fn ask(generator: &dyn MeetingTextGenerator, live: &LiveTranscript, request: &Request) -> Asked {
    live.doc().requests += 1;
    let answer =
        match generator.generate_json(PROMPT, &request.input, OUTPUT_TOKENS, || &RESPONSE_SCHEMA) {
            Ok(answer) => answer,
            Err(MeetingTextGenerationError::Unreachable | MeetingTextGenerationError::TimedOut) => {
                return Asked::NotAsked;
            }
            Err(error) => {
                log::info!("A live doc reading got no answer: {error:?}");
                return Asked::Unanswered;
            }
        };
    match first_json_value::<Reply>(answer.trim()) {
        Ok(reply) => Asked::Answered(
            reply
                .add
                .into_iter()
                .map(|added| Written {
                    point: None,
                    text: added.text,
                    lines: added.lines,
                })
                .chain(reply.rewrite.into_iter().map(|rewritten| Written {
                    point: Some(rewritten.point),
                    text: rewritten.text,
                    lines: rewritten.lines,
                }))
                .take(MAX_WRITTEN)
                .collect(),
        ),
        Err(()) => {
            log::info!("A live doc reading answered something that is not a list");
            Asked::Unanswered
        }
    }
}

/// The next reading when one is due: the doc as it stands and the unread
/// lines, oldest first, as many as one request holds.
fn next_request(live: &LiveTranscript, moment: Moment, budget: usize) -> Option<Request> {
    let segments = live
        .segments
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut doc = live.doc();
    let from = doc.read.min(segments.len());
    if from == segments.len() || budget == 0 {
        return None;
    }
    let heard_ns = segments
        .iter()
        .map(|segment| segment.end_offset_ns)
        .max()
        .unwrap_or(0);
    if moment == Moment::Pass
        && doc
            .asked_at_ns
            .is_some_and(|asked| heard_ns < asked.saturating_add(READ_EVERY_NS))
    {
        return None;
    }
    doc.asked_at_ns = Some(heard_ns);
    let mut points = HashMap::new();
    let shown: Vec<Shown<'_>> = doc
        .points
        .iter()
        .enumerate()
        .map(|(index, point)| {
            let label = format!("P{}", index + 1);
            points.insert(label.clone(), point.id.clone());
            Shown {
                point: label,
                text: &point.text,
                locked: point.edited,
            }
        })
        .collect();
    let mut used = ENVELOPE_BYTES
        + shown
            .iter()
            .map(|point| serde_json::to_string(point).map_or(usize::MAX, |json| json.len() + 1))
            .fold(0, usize::saturating_add);
    let mut taken: Vec<&LiveSegment> = Vec::new();
    let mut through = from;
    for segment in &segments[from..] {
        if is_ask(segment) {
            through += 1;
            continue;
        }
        let bytes = serde_json::to_string(&Line {
            line: String::from("L000"),
            text: &segment.text,
        })
        .map_or(usize::MAX, |line| line.len() + 1);
        if used.saturating_add(bytes) > budget && !taken.is_empty() {
            break;
        }
        used = used.saturating_add(bytes);
        taken.push(segment);
        through += 1;
    }
    if taken.is_empty() {
        drop(shown);
        doc.read = through;
        return None;
    }
    taken.sort_by_key(|segment| (segment.start_offset_ns, segment.end_offset_ns));
    let lines: Vec<Line<'_>> = taken
        .iter()
        .enumerate()
        .map(|(index, segment)| Line {
            line: format!("L{}", index + 1),
            text: &segment.text,
        })
        .collect();
    let input = serde_json::to_string(&Input { doc: shown, lines }).ok()?;
    Some(Request {
        input,
        points,
        lines: taken
            .iter()
            .map(|segment| segment.start_offset_ns)
            .collect(),
        through,
    })
}

/// Fold a reading into the doc. A point must cite a line it was given and
/// say something the doc does not already say; a rewrite must name a point
/// the reading was shown that the user has not touched or removed since.
/// Returns whether the doc changed.
fn merge(points: &mut Vec<MeetingLiveDocPoint>, written: Vec<Written>, request: &Request) -> bool {
    let mut changed = false;
    for written in written {
        let cited: Option<Vec<u64>> = written
            .lines
            .iter()
            .map(|label| {
                let position = label.trim().strip_prefix('L')?.parse::<usize>().ok()?;
                request.lines.get(position.checked_sub(1)?).copied()
            })
            .collect();
        let Some(first_said) = cited.and_then(|cited| cited.into_iter().min()) else {
            continue;
        };
        let text = written.text.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty()
            || text.chars().count() > MAX_POINT_CHARS
            || text.chars().any(char::is_control)
            || points.iter().any(|point| fold(&point.text) == fold(&text))
        {
            continue;
        }
        let Some(label) = written.point else {
            if points.len() < MAX_POINTS {
                points.push(MeetingLiveDocPoint {
                    id: Uuid::new_v4().to_string(),
                    text,
                    edited: false,
                    start_offset_ns: Some(first_said),
                });
                changed = true;
            }
            continue;
        };
        let Some(point) = request
            .points
            .get(label.trim())
            .and_then(|id| points.iter_mut().find(|point| &point.id == id))
        else {
            continue;
        };
        if !point.edited {
            point.text = text;
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod probe;

#[cfg(test)]
mod tests;

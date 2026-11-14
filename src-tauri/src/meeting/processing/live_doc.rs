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

use super::live_changes::{self, Author};
use super::{
    evidence_budget, first_json_value, json_response_schema, LiveSegment, LiveTranscript,
    MeetingProcessingService, MeetingTextGenerationError, MeetingTextGenerator,
};
use crate::meeting::ledger::fold;
use crate::meeting::live_doc_types::{
    MeetingLiveChangeKind, MeetingLiveDoc, MeetingLiveDocPoint, MeetingLiveTarget, MeetingVoiceAsk,
    MeetingVoiceIntent,
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
const CHECK_PROMPT: &str = include_str!("../../../resources/prompts/live_doc_check.txt");
const OUTPUT_TOKENS: i32 = 900;
const CHECK_OUTPUT_TOKENS: i32 = 300;
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
/// `{"notes":[]}`, the check around its notes.
const NOTES_ENVELOPE_BYTES: usize = 12;
/// Lines of a point the check sees, besides what a rewrite replaces.
const CHECKED_LINES: usize = 3;
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
static CHECK_SCHEMA: LazyLock<RootSchema> = LazyLock::new(json_response_schema::<Verdicts>);

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

/// A written point whose lines and words checked out: the point it
/// rewrites, if any, the point it replaces when it is an add that says what
/// a dropped rewrite said, its text, the lines it cites, and when the first
/// of them was said.
struct Candidate<'a> {
    rewrites: Option<&'a Known>,
    replaces: Option<&'a Known>,
    text: String,
    cited: Vec<&'a Given>,
    first_said: u64,
}

/// What the check says of each note, by its label.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Verdicts {
    #[schemars(length(max = "MAX_WRITTEN"))]
    notes: Vec<Verdict>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Verdict {
    note: String,
    keep: bool,
}

/// A point as the check sees it: its text, and the lines it was written
/// from, led by the words of the point it rewrites.
#[derive(Serialize)]
struct Note<'a> {
    note: String,
    text: &'a str,
    lines: Vec<&'a str>,
}

#[derive(Serialize)]
struct Notes<'a> {
    notes: Vec<Note<'a>>,
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

/// One reading: the request text, the point each label names, the lines
/// it was given, and the append position the doc has read through once it
/// is answered.
struct Request {
    input: String,
    points: HashMap<String, Known>,
    lines: Vec<Given>,
    through: usize,
}

/// A doc point a reading was shown: its id, and its words then.
struct Known {
    id: String,
    text: String,
}

/// A line a reading was given: when it was said, and its words.
struct Given {
    said_ns: u64,
    text: String,
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
            changes: self.live_changes(),
        }
    }

    /// Ask for the doc. Returns whether it was not started before; the
    /// caller then holds the reading and must run [`MeetingProcessingService::read_live_doc_now`].
    pub(crate) fn start_doc(&self) -> bool {
        start(&mut self.doc())
    }

    /// A hand edit to the running doc by the user or a teammate; see
    /// [`edit`].
    pub(crate) fn edit_doc(
        &self,
        author: &Author,
        point_id: Option<&str>,
        text: &str,
    ) -> Result<(), EditRefusal> {
        let mut doc = self.doc();
        let edited = edit(&mut doc.points, point_id, text)?;
        doc.started = true;
        drop(doc);
        edited.record(self, author);
        Ok(())
    }

    pub(crate) fn doc_points(&self) -> Vec<MeetingLiveDocPoint> {
        self.doc().points.clone()
    }
}

/// What one edit did to one point.
pub(crate) struct Edited {
    pub(crate) point_id: String,
    pub(crate) kind: MeetingLiveChangeKind,
    pub(crate) text: String,
}

impl Edited {
    fn record(&self, live: &LiveTranscript, author: &Author) {
        live.record_change(
            author,
            MeetingLiveTarget::Point,
            &self.point_id,
            self.kind,
            &self.text,
        );
    }
}

/// A hand edit: new text for a point, a new point when `point_id` is
/// `None`, or the point removed when the text is blank. The point is the
/// editor's from then on. Returns what the edit did.
pub(crate) fn edit(
    points: &mut Vec<MeetingLiveDocPoint>,
    point_id: Option<&str>,
    text: &str,
) -> Result<Edited, EditRefusal> {
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
                let removed = points.remove(at);
                return Ok(Edited {
                    point_id: removed.id,
                    kind: MeetingLiveChangeKind::Removed,
                    text: removed.text,
                });
            }
            points[at].text.clone_from(&text);
            points[at].edited = true;
            Ok(Edited {
                point_id: points[at].id.clone(),
                kind: MeetingLiveChangeKind::Edited,
                text,
            })
        }
        None if text.is_empty() => Err(EditRefusal::Invalid),
        None if points.len() >= MAX_POINTS => Err(EditRefusal::Full),
        None => {
            let id = Uuid::new_v4().to_string();
            points.push(MeetingLiveDocPoint {
                id: id.clone(),
                text: text.clone(),
                edited: true,
                start_offset_ns: None,
            });
            Ok(Edited {
                point_id: id,
                kind: MeetingLiveChangeKind::Added,
                text,
            })
        }
    }
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
        let candidates = candidates(written, &request);
        let rewrites: Vec<(&Known, String)> = candidates
            .iter()
            .filter_map(|candidate| candidate.rewrites.map(|known| (known, candidate.text.clone())))
            .collect();
        let kept = folded(check(generator.as_ref(), live, candidates), rewrites);
        let mut doc = live.doc();
        doc.read = doc.read.max(request.through);
        let landed = merge(&mut doc.points, kept);
        drop(doc);
        changed |= !landed.is_empty();
        for edited in landed {
            edited.record(live, &Author::Sona);
        }
    }
    changed
}

/// The doc's points and the changes into the store, so the meeting's page
/// has them.
pub(crate) fn save(store: &MeetingStore, session_id: MeetingSessionId, live: &LiveTranscript) {
    if let Err(error) = store.save_live_doc_points(session_id, &live.doc_points()) {
        log::warn!("Could not keep the live doc of {session_id:?}: {error:?}");
    }
    live_changes::save(store, session_id, live);
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
    let recorded: HashSet<String> = live
        .live_changes()
        .into_iter()
        .map(|change| change.id)
        .collect();
    read(service, store, session_id, live, Moment::HangUp);
    let saved = store.live_doc_points(session_id).unwrap_or_default();
    let lost: HashSet<&String> = before
        .iter()
        .filter(|id| !saved.iter().any(|point| &point.id == *id && !point.edited))
        .collect();
    live.forget_changes(|change| {
        !recorded.contains(&change.id)
            && change.target == MeetingLiveTarget::Point
            && lost.contains(&change.target_id)
    });
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

/// Each word of `text`, lowercased, with the byte offset just past it. An
/// apostrophe, straight or curly, stays inside its word, and so does a
/// colon, point or comma between digits: "9:30" is one time, not 9 and 30.
fn words(text: &str) -> impl Iterator<Item = (String, usize)> + '_ {
    let in_word = move |at: usize, character: char| {
        character.is_alphanumeric()
            || matches!(character, '\'' | '\u{2019}')
            || (matches!(character, ':' | '.' | ',')
                && text[..at].ends_with(|before: char| before.is_ascii_digit())
                && text[at + 1..].starts_with(|after: char| after.is_ascii_digit()))
    };
    let mut from = 0;
    std::iter::from_fn(move || {
        let (start, _) = text[from..]
            .char_indices()
            .map(|(at, character)| (from + at, character))
            .find(|&(at, character)| in_word(at, character))?;
        from = text[start..]
            .char_indices()
            .map(|(at, character)| (start + at, character))
            .find(|&(at, character)| !in_word(at, character))
            .map_or(text.len(), |(at, _)| at);
        let word = text[start..from]
            .chars()
            .flat_map(char::to_lowercase)
            .map(|character| if character == '\u{2019}' { '\'' } else { character })
            .collect();
        Some((word, from))
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
        Ok(Reply { add, rewrite }) => Asked::Answered(
            add.into_iter()
                .map(|added| Written {
                    point: None,
                    text: added.text,
                    lines: added.lines,
                })
                .chain(rewrite.into_iter().map(|rewritten| Written {
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
            points.insert(
                label.clone(),
                Known {
                    id: point.id.clone(),
                    text: point.text.clone(),
                },
            );
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
            .map(|segment| Given {
                said_ns: segment.start_offset_ns,
                text: segment.text.clone(),
            })
            .collect(),
        through,
    })
}

/// The points of a reading that cite only lines it was given, fit the
/// doc, say something in two words or more, and name no label their lines
/// do not say once a leading one is set aside; a rewrite must name a point
/// the reading was shown and stay on its topic. A replayed call's noise,
/// heard as "California.", came back as a point of that one word.
fn candidates(written: Vec<Written>, request: &Request) -> Vec<Candidate<'_>> {
    written
        .into_iter()
        .filter_map(|written| {
            let cited = written
                .lines
                .iter()
                .map(|label| {
                    let position = label.trim().strip_prefix('L')?.parse::<usize>().ok()?;
                    request.lines.get(position.checked_sub(1)?)
                })
                .collect::<Option<Vec<&Given>>>()?;
            let first_said = cited.iter().map(|line| line.said_ns).min()?;
            let text = unlabeled(&written.text).split_whitespace().collect::<Vec<_>>().join(" ");
            if words(&text).nth(1).is_none()
                || text.chars().count() > MAX_POINT_CHARS
                || text.chars().any(char::is_control)
                || names_a_label(&text, &cited)
            {
                return None;
            }
            let rewrites = match written.point {
                None => None,
                Some(label) => {
                    let known = request.points.get(label.trim())?;
                    if !same_topic(&known.text, &text) {
                        return None;
                    }
                    Some(known)
                }
            };
            Some(Candidate {
                rewrites,
                replaces: None,
                text,
                cited,
                first_said,
            })
        })
        .collect()
}

/// One change, not two: a reading may rewrite a point and also add what the
/// rewrite says. Apple's model, told the harbour map launch moved again to
/// March 21, rewrote the launch point and also added "The public launch of
/// the harbour map is now set for March 21"; the check dropped the rewrite,
/// kept the add, and the doc held both dates. An add that says what a kept
/// rewrite says goes, and one that says what a dropped rewrite said
/// replaces that point.
fn folded<'a>(mut kept: Vec<Candidate<'a>>, rewrites: Vec<(&'a Known, String)>) -> Vec<Candidate<'a>> {
    let rewritten: Vec<String> =
        kept.iter().filter(|candidate| candidate.rewrites.is_some()).map(|candidate| candidate.text.clone()).collect();
    kept.retain(|candidate| {
        candidate.rewrites.is_some() || !rewritten.iter().any(|said| says_the_same(said, &candidate.text))
    });
    let mut dropped: Vec<(&Known, String)> = rewrites
        .into_iter()
        .filter(|(known, _)| {
            !kept
                .iter()
                .any(|candidate| candidate.rewrites.is_some_and(|kept| kept.id == known.id))
        })
        .collect();
    for add in kept.iter_mut().filter(|candidate| candidate.rewrites.is_none()) {
        if let Some(position) = dropped.iter().position(|(_, said)| says_the_same(said, &add.text)) {
            add.replaces = Some(dropped.swap_remove(position).0);
        }
    }
    kept
}

/// Whether `other` says what `said` says: it has at least two of the words
/// of five letters or more in `said`, and at least half of them. "The
/// ferry timetable change goes out with the harbour map" shares only
/// "harbour" with "The harbour map launch is confirmed for March 21".
fn says_the_same(said: &str, other: &str) -> bool {
    let mut long: Vec<String> = words(said).map(|(word, _)| word).filter(|word| is_long(word)).collect();
    long.sort();
    long.dedup();
    let other: Vec<String> = words(other).map(|(word, _)| word).collect();
    let shared = long.iter().filter(|word| other.iter().any(|heard| related(heard, word))).count();
    shared >= 2 && shared * 2 >= long.len()
}

/// `text` without a leading label such as "P2: ", which local models put
/// before a point they add: "P2: Priya will choose the final subject line".
fn unlabeled(text: &str) -> &str {
    match text.split_once(':') {
        Some((head, rest)) if is_label(&head.trim().to_lowercase()) => rest,
        _ => text,
    }
}

/// The second look: a reading's points go back to the model, each with
/// the lines it was written from, and only the ones it keeps are kept.
/// Asked to write the doc, Apple's on-device model wrote small talk such
/// as "Hello?" and "Screen froze momentarily." as points; asked whether a
/// note is worth keeping, it can say no. A check that gets no answer keeps
/// nothing.
fn check<'a>(
    generator: &dyn MeetingTextGenerator,
    live: &LiveTranscript,
    candidates: Vec<Candidate<'a>>,
) -> Vec<Candidate<'a>> {
    let budget = evidence_budget(generator, CHECK_PROMPT, CHECK_OUTPUT_TOKENS);
    let mut batches: Vec<Vec<Candidate<'a>>> = Vec::new();
    let mut used: usize = 0;
    for candidate in candidates {
        let bytes = serde_json::to_string(&note(0, &candidate)).map_or(usize::MAX, |json| json.len() + 1);
        match batches.last_mut() {
            Some(batch) if used.saturating_add(bytes) <= budget => batch.push(candidate),
            _ => {
                used = NOTES_ENVELOPE_BYTES;
                batches.push(vec![candidate]);
            }
        }
        used = used.saturating_add(bytes);
    }
    batches
        .into_iter()
        .flat_map(|batch| {
            let keep = kept_notes(generator, live, &batch);
            batch
                .into_iter()
                .enumerate()
                .filter(move |(index, _)| keep.contains(index))
                .map(|(_, candidate)| candidate)
        })
        .collect()
}

fn note<'a>(index: usize, candidate: &'a Candidate<'_>) -> Note<'a> {
    Note {
        note: format!("N{}", index + 1),
        text: &candidate.text,
        lines: candidate
            .rewrites
            .map(|known| known.text.as_str())
            .into_iter()
            .chain(candidate.cited.iter().take(CHECKED_LINES).map(|line| line.text.as_str()))
            .collect(),
    }
}

/// The positions in `batch` the check keeps: named with a keep, and never
/// with a drop.
fn kept_notes(
    generator: &dyn MeetingTextGenerator,
    live: &LiveTranscript,
    batch: &[Candidate<'_>],
) -> HashSet<usize> {
    let notes = Notes {
        notes: batch.iter().enumerate().map(|(index, candidate)| note(index, candidate)).collect(),
    };
    let Ok(input) = serde_json::to_string(&notes) else {
        return HashSet::new();
    };
    live.doc().requests += 1;
    let answer = generator.generate_json(CHECK_PROMPT, &input, CHECK_OUTPUT_TOKENS, || &CHECK_SCHEMA);
    let Ok(Verdicts { notes: verdicts }) = answer
        .map_err(|error| log::info!("A live doc check got no answer: {error:?}"))
        .and_then(|answer| first_json_value::<Verdicts>(answer.trim()))
    else {
        return HashSet::new();
    };
    let position = |verdict: &Verdict| {
        let number = verdict.note.trim().strip_prefix('N')?.parse::<usize>().ok()?;
        number.checked_sub(1).filter(|index| *index < batch.len())
    };
    let dropped: HashSet<usize> = verdicts.iter().filter(|verdict| !verdict.keep).filter_map(position).collect();
    verdicts
        .iter()
        .filter(|verdict| verdict.keep)
        .filter_map(position)
        .filter(|index| !dropped.contains(index))
        .collect()
}

/// Fold a reading's kept points into the doc. A point must say something
/// the doc does not already say; a rewrite must name a point the user has
/// not touched or removed since the reading was asked. An add that replaces
/// a point the user touched or removed meanwhile lands as a new point, so
/// the news it carries is kept. Returns what landed.
///
/// An add whose words come in a row in a point says nothing new: shown a
/// doc that had "We need to confirm the numbers for step three.", Apple's
/// model added "Confirm the numbers for step three." An add that holds a
/// point's words in a row says that point and more, so it takes its place.
/// Neither holds for a point this reading rewrites, whose old words are on
/// their way out, or for a point someone typed, which is often the question
/// an add answers.
fn merge(points: &mut Vec<MeetingLiveDocPoint>, kept: Vec<Candidate<'_>>) -> Vec<Edited> {
    let rewritten: HashSet<&str> = kept
        .iter()
        .filter_map(|candidate| candidate.rewrites.or(candidate.replaces))
        .map(|known| known.id.as_str())
        .collect();
    let mut landed = Vec::new();
    for candidate in kept {
        if points.iter().any(|point| fold(&point.text) == fold(&candidate.text)) {
            continue;
        }
        let target = match candidate.rewrites.or(candidate.replaces) {
            Some(known) => points.iter().position(|point| point.id == known.id && !point.edited),
            None if points.iter().any(|point| {
                !point.edited && !rewritten.contains(point.id.as_str()) && runs_inside(&candidate.text, &point.text)
            }) =>
            {
                continue
            }
            None => points.iter().position(|point| {
                !point.edited && !rewritten.contains(point.id.as_str()) && runs_inside(&point.text, &candidate.text)
            }),
        };
        match target {
            Some(index) => {
                let point = &mut points[index];
                landed.push(Edited {
                    point_id: point.id.clone(),
                    kind: MeetingLiveChangeKind::Rewrote,
                    text: candidate.text.clone(),
                });
                point.text = candidate.text;
            }
            None if candidate.rewrites.is_none() && points.len() < MAX_POINTS => {
                let id = Uuid::new_v4().to_string();
                landed.push(Edited {
                    point_id: id.clone(),
                    kind: MeetingLiveChangeKind::Added,
                    text: candidate.text.clone(),
                });
                points.push(MeetingLiveDocPoint {
                    id,
                    text: candidate.text,
                    edited: false,
                    start_offset_ns: Some(candidate.first_said),
                });
            }
            None => {}
        }
    }
    landed
}

/// Whether a rewrite is still about the point it replaces: the two share a
/// word of five letters or more, or a form of one. A reading rewrote "The
/// tight schedule means we must prioritize key tasks." as "Explain
/// calendar access first with one sentence behind a flag."
fn same_topic(before: &str, after: &str) -> bool {
    let before: Vec<String> = words(before).map(|(word, _)| word).collect();
    words(after).any(|(word, _)| {
        is_long(&word) && before.iter().any(|said| related(said, &word))
    })
}

/// Whether the words of `part` come in a row in `whole` with nothing around
/// them that turns what they say, as those of "Confirm the numbers for step
/// three." do in "We need to confirm the numbers for step three." Around
/// them, "Don't", "or March 28" or "whether we can" says something else,
/// and so does a question mark on one side only.
fn runs_inside(part: &str, whole: &str) -> bool {
    if part.contains('?') != whole.contains('?') {
        return false;
    }
    let part: Vec<String> = words(part).map(|(word, _)| word).collect();
    let whole: Vec<String> = words(whole).map(|(word, _)| word).collect();
    !part.is_empty()
        && whole.windows(part.len()).enumerate().any(|(at, run)| {
            run == part.as_slice() && !whole[..at].iter().chain(&whole[at + part.len()..]).any(|word| turns(word))
        })
}

/// A word that turns what the words beside it say: a "not", a choice, a
/// condition or a doubt.
fn turns(word: &str) -> bool {
    word.ends_with("n't")
        || ["not", "no", "never", "cannot", "or", "whether", "if", "unless", "maybe", "might"].contains(&word)
}

/// The same word, or two forms of one: "lamp" and "lamps", "fix" and
/// "fixed", "schedule" and "scheduling".
fn related(left: &str, right: &str) -> bool {
    let common = left.chars().zip(right.chars()).take_while(|(left, right)| left == right).count();
    let shorter = left.chars().count().min(right.chars().count());
    (common == shorter && common >= 3) || common >= 5
}

/// A word of five letters or more. A number read whole is not one:
/// "1,000,500" would pass for a form of "1,000,000", and two points that
/// name one price can still say different things.
fn is_long(word: &str) -> bool {
    word.chars().count() >= 5 && !word.contains(char::is_numeric)
}

/// Whether `text` names a label, such as "L1" or "P2", that none of the
/// lines it cites says: the reading writing about its own input, as in
/// "No new detail from L1.", rather than a "P1 bug" someone talked about.
fn names_a_label(text: &str, cited: &[&Given]) -> bool {
    words(text).any(|(word, _)| {
        is_label(&word)
            && !cited
                .iter()
                .any(|line| words(&line.text).any(|(said, _)| said == word))
    })
}

/// "l1" or "p12": a lowercased word that reads as a line or point label.
fn is_label(word: &str) -> bool {
    word.strip_prefix(['l', 'p'])
        .is_some_and(|number| !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()))
}

#[cfg(test)]
mod probe;

#[cfg(test)]
mod tests;

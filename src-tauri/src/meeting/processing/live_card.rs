//! Promises heard while a meeting records: who owes what by when, read out
//! of the provisional transcript in batches, and filed in Triage at the
//! stop.
//!
//! Only lines that sound like a promise are read, with the lines around
//! them, and at most once per minute of meeting: a call costs about one
//! request a minute while people make promises and none while they do not.
//! A promise is kept only when its quote, and the words that say when, are
//! in the lines it cites; the day those words name is worked out here,
//! against the moment they were said, never by the model.

use super::{
    evidence_budget, first_json_value, json_response_schema, LiveSegment, LiveTranscript,
    MeetingProcessingService, MeetingTextGenerationError, MeetingTextGenerator,
};
use crate::meeting::analytics::MeetingProvisionalSpeaker;
use crate::meeting::ledger::{fold, fold_haystack, quote_is_verbatim};
use crate::meeting::spoken_time::{self, SpokenTime};
use crate::meeting::store::{MeetingStore, StoreError};
use crate::meeting::types::{MeetingSessionId, SourceKind};
use crate::planning::work::parse::person_named;
use crate::planning::work::sync::People;
use crate::planning::work::types::{WorkOwner, WorkPersonChoice};
use chrono::{Local, NaiveDateTime, NaiveTime, TimeZone};
use schemars::{schema::RootSchema, JsonSchema};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::{Arc, LazyLock, MutexGuard};
use uuid::Uuid;

const PROMPT: &str = include_str!("../../../resources/prompts/live_promises.txt");
const OUTPUT_TOKENS: i32 = 600;
const MAX_PROMISES: usize = 8;
/// A promise is often the answer to the line or two before it.
const CONTEXT_LINES: usize = 2;
/// Meeting time between two requests, so a talkative minute is one request.
pub(crate) const REQUEST_EVERY_NS: u64 = 60_000_000_000;
/// Requests a stop may make to read what is left. More waiting lines than
/// that many full requests are dropped rather than holding the notes back.
const HANG_UP_REQUESTS: usize = 6;
const MAX_WHAT_CHARS: usize = 120;
const MAX_NAME_CHARS: usize = 80;
const MAX_QUOTE_CHARS: usize = 400;
/// `{"lines":[]}`, the request around the lines.
const ENVELOPE_BYTES: usize = 12;
/// Due with no time of day: the end of the working day, as a typed task.
const DAY_DUE_HOUR: u32 = 17;
static RESPONSE_SCHEMA: LazyLock<RootSchema> = LazyLock::new(json_response_schema::<Reply>);

/// The phrases that make a line worth reading, in a line lowercased with
/// its punctuation turned to spaces and a space at either end.
const CUES: &[&str] = &[
    " i'll ",
    " i will ",
    " we'll ",
    " we will ",
    " you'll ",
    " he'll ",
    " she'll ",
    " they'll ",
    " i'm going to ",
    " i am going to ",
    " let me ",
    " can you ",
    " could you ",
    " would you ",
    " will you ",
    " i can take ",
    " i'll take ",
    " take care of ",
    " on it ",
    " leave it with me ",
    " follow up ",
    " get back to ",
    " action item ",
    " i owe ",
    " owe you ",
    " make sure ",
    " i've got it ",
    " i got it ",
    " own this ",
    " owns ",
    " handle ",
    " deadline ",
];

/// What the live card holds for one capture.
#[derive(Default)]
pub(super) struct Card {
    /// Segments, in the order the pass appended them, already looked at.
    read: usize,
    /// Positions, in append order, of promise-sounding lines not yet read.
    waiting: Vec<usize>,
    /// How far into the meeting the last request had heard.
    asked_at_ns: Option<u64>,
    promises: Vec<MeetingLivePromise>,
    requests: u32,
}

/// The live card: the promises heard so far in a running meeting.
#[derive(Clone, Debug, Serialize, Type)]
pub struct MeetingLiveCard {
    pub session_id: MeetingSessionId,
    pub promises: Vec<MeetingLivePromise>,
    /// Model requests this capture has made for the card.
    pub model_requests: u32,
    /// The end of the newest line recognized, from the start of the meeting.
    pub heard_through_ns: u64,
}

/// One promise heard in a running meeting.
#[derive(Clone, Debug, Serialize, Type)]
pub struct MeetingLivePromise {
    pub id: String,
    pub what: String,
    pub owner: WorkOwner,
    /// "Me", the person picked, or a name heard that is nobody known yet.
    pub owner_name: Option<String>,
    /// The owner was picked by hand, so later readings leave it alone.
    pub owner_set: bool,
    /// The task or its date was changed by hand, so later readings leave
    /// both alone.
    pub edited: bool,
    pub due_at_ms: Option<i64>,
    /// The words that said when, as heard.
    pub due_words: Option<String>,
    pub quote: String,
    pub start_offset_ns: u64,
    pub end_offset_ns: u64,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Reply {
    #[schemars(length(max = "MAX_PROMISES"))]
    promises: Vec<Heard>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Heard {
    what: String,
    #[serde(default)]
    owner: String,
    #[serde(default)]
    when: String,
    quote: String,
    lines: Vec<String>,
}

#[derive(Serialize)]
struct Line<'a> {
    line: String,
    speaker: &'a str,
    text: &'a str,
}

#[derive(Serialize)]
struct Input<'a> {
    lines: &'a [Line<'a>],
}

/// A line copied out of the live transcript for one request.
struct Copied {
    speaker: String,
    text: String,
    start_offset_ns: u64,
    end_offset_ns: u64,
}

/// One request: its lines, and the waiting lines it answers for.
struct Request {
    lines: Vec<Copied>,
    answers: Vec<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Moment {
    /// The background pass: at most one request, at most once a minute.
    Pass,
    /// The stop: everything still waiting, now.
    HangUp,
}

impl LiveTranscript {
    fn card(&self) -> MutexGuard<'_, Card> {
        self.card
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The card as the live screen shows it.
    pub(crate) fn live_card(&self, session_id: MeetingSessionId) -> MeetingLiveCard {
        let heard_through_ns = self
            .segments
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|segment| segment.end_offset_ns)
            .max()
            .unwrap_or(0);
        let card = self.card();
        MeetingLiveCard {
            session_id,
            promises: card.promises.clone(),
            model_requests: card.requests,
            heard_through_ns,
        }
    }

    /// Who owes one promise, picked by hand. `None` when the card has no
    /// such promise.
    pub(crate) fn set_live_owner(
        &self,
        promise_id: &str,
        owner: WorkOwner,
        owner_name: Option<String>,
    ) -> Option<()> {
        let mut card = self.card();
        let promise = card
            .promises
            .iter_mut()
            .find(|promise| promise.id == promise_id)?;
        promise.owner = owner;
        promise.owner_name = owner_name;
        promise.owner_set = true;
        Some(())
    }

    /// New words and date for one promise, typed by hand. `None` when the
    /// card has no such promise or the words are blank or too long.
    pub(crate) fn edit_live_promise(
        &self,
        promise_id: &str,
        what: &str,
        due_at_ms: Option<i64>,
    ) -> Option<()> {
        let what = bounded(what, MAX_WHAT_CHARS)?;
        let mut card = self.card();
        let promise = card
            .promises
            .iter_mut()
            .find(|promise| promise.id == promise_id)?;
        if promise.due_at_ms != due_at_ms {
            promise.due_words = None;
        }
        promise.what = what;
        promise.due_at_ms = due_at_ms;
        promise.edited = true;
        Some(())
    }

    /// Every promise heard, for filing at the stop.
    pub(crate) fn live_promises(&self) -> Vec<MeetingLivePromise> {
        self.card().promises.clone()
    }
}

/// Read the promise-sounding lines the pass has added since the last
/// request, when a request is due. Returns whether the card changed.
pub(super) fn tick(
    service: &MeetingProcessingService,
    store: &MeetingStore,
    session_id: MeetingSessionId,
    live: &LiveTranscript,
    moment: Moment,
) -> bool {
    let Ok(snapshot) = store.session_snapshot(session_id) else {
        return false;
    };
    let Some(began_utc_ms) = snapshot.started_at_utc_ms else {
        return false;
    };
    let generator = service.text_generator_for_session(store, session_id);
    let budget = generator.as_deref().map_or(0, |generator| {
        evidence_budget(generator, PROMPT, OUTPUT_TOKENS)
    });
    let requests = match moment {
        Moment::Pass => 1,
        Moment::HangUp => HANG_UP_REQUESTS,
    };
    let mut changed = false;
    for _ in 0..requests {
        let Some(request) = next_request(live, began_utc_ms, moment, budget) else {
            break;
        };
        let Some(generator) = generator.as_deref() else {
            break;
        };
        match ask(generator, live, &request) {
            Asked::Answered(heard) => {
                let people = store
                    .with_planning_snapshot(|meetings| {
                        People::read(meetings).map_err(|_| StoreError::Unavailable)
                    })
                    .map(|people| people.choices)
                    .unwrap_or_default();
                let kept: Vec<_> = heard
                    .into_iter()
                    .filter_map(|heard| checked(heard, &request.lines, &people, began_utc_ms))
                    .collect();
                let mut card = live.card();
                card.waiting
                    .retain(|index| !request.answers.contains(index));
                for promise in kept {
                    changed |= merge(&mut card.promises, promise);
                }
            }
            Asked::Unanswered => {
                live.card()
                    .waiting
                    .retain(|index| !request.answers.contains(index));
            }
            Asked::NotAsked => break,
        }
    }
    changed
}

/// The stop: the words the last pass had not reached, every promise still
/// waiting to be read, and the card filed in Triage. Runs off the stop's
/// thread, so the stop answers at once.
pub(crate) fn hang_up(
    service: Arc<MeetingProcessingService>,
    store: Arc<MeetingStore>,
    session_id: MeetingSessionId,
    live: Arc<LiveTranscript>,
) {
    std::thread::spawn(move || {
        if let Err(error) = service.live_pass(&store, session_id, &live) {
            log::debug!("The last provisional pass for {session_id:?} read nothing: {error:?}");
        }
        tick(&service, &store, session_id, &live, Moment::HangUp);
        let promises = live.live_promises();
        let requests = live.card().requests;
        log::info!(
            "Live card for {session_id:?}: {} promises from {requests} model requests",
            promises.len()
        );
        if !promises.is_empty() {
            crate::planning::work::live::file_heard(
                service.app.as_ref(),
                &store,
                session_id,
                &promises,
            );
        }
        super::live_doc::hang_up(&service, &store, session_id, &live);
    });
}

enum Asked {
    Answered(Vec<Heard>),
    /// Asked, and the answer was not a list of promises: those lines are
    /// not asked about again.
    Unanswered,
    /// The engine could not be reached: the lines wait for the next request.
    NotAsked,
}

fn ask(generator: &dyn MeetingTextGenerator, live: &LiveTranscript, request: &Request) -> Asked {
    let lines: Vec<Line<'_>> = request
        .lines
        .iter()
        .enumerate()
        .map(|(index, line)| Line {
            line: format!("L{}", index + 1),
            speaker: &line.speaker,
            text: &line.text,
        })
        .collect();
    let Ok(input) = serde_json::to_string(&Input { lines: &lines }) else {
        return Asked::Unanswered;
    };
    live.card().requests += 1;
    let answer = match generator.generate_json(PROMPT, &input, OUTPUT_TOKENS, || &RESPONSE_SCHEMA) {
        Ok(answer) => answer,
        Err(MeetingTextGenerationError::Unreachable | MeetingTextGenerationError::TimedOut) => {
            return Asked::NotAsked;
        }
        Err(error) => {
            log::info!("A live promise reading got no answer: {error:?}");
            return Asked::Unanswered;
        }
    };
    match first_json_value::<Reply>(answer.trim()) {
        Ok(reply) => Asked::Answered(reply.promises.into_iter().take(MAX_PROMISES).collect()),
        Err(()) => {
            log::info!("A live promise reading answered something that is not a list");
            Asked::Unanswered
        }
    }
}

/// Look at the lines added since the last call, and copy out the next
/// request when one is due: the waiting lines with the lines around them,
/// as many as one request holds, oldest first.
fn next_request(
    live: &LiveTranscript,
    began_utc_ms: i64,
    moment: Moment,
    budget: usize,
) -> Option<Request> {
    let segments = live
        .segments
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut card = live.card();
    for index in card.read..segments.len() {
        if cue(
            &segments[index].text,
            said_at(began_utc_ms, segments[index].start_offset_ns),
        ) {
            card.waiting.push(index);
        }
    }
    card.read = segments.len();
    if card.waiting.is_empty() || budget == 0 {
        return None;
    }
    let heard_ns = segments
        .iter()
        .map(|segment| segment.end_offset_ns)
        .max()
        .unwrap_or(0);
    if moment == Moment::Pass
        && card
            .asked_at_ns
            .is_some_and(|asked| heard_ns < asked.saturating_add(REQUEST_EVERY_NS))
    {
        return None;
    }
    let mut order: Vec<usize> = (0..segments.len()).collect();
    order.sort_by_key(|&index| {
        (
            segments[index].start_offset_ns,
            segments[index].end_offset_ns,
        )
    });
    let mut position = vec![0; segments.len()];
    for (at, &index) in order.iter().enumerate() {
        position[index] = at;
    }
    let mut waiting = card.waiting.clone();
    waiting.sort_by_key(|&index| position[index]);
    let mut taken: Vec<usize> = Vec::new();
    let mut answers = Vec::new();
    let mut used = ENVELOPE_BYTES;
    for index in waiting {
        let at = position[index];
        let window = at.saturating_sub(CONTEXT_LINES)..(at + CONTEXT_LINES + 1).min(order.len());
        let added: Vec<usize> = window.filter(|at| !taken.contains(at)).collect();
        let bytes = added
            .iter()
            .map(|&at| line_bytes(&segments[order[at]]))
            .fold(0, usize::saturating_add);
        if used.saturating_add(bytes) > budget {
            if answers.is_empty() {
                answers.push(index);
                continue;
            }
            break;
        }
        used += bytes;
        taken.extend(added);
        answers.push(index);
    }
    card.asked_at_ns = Some(heard_ns);
    if taken.is_empty() {
        card.waiting.retain(|index| !answers.contains(index));
        return None;
    }
    taken.sort_unstable();
    Some(Request {
        lines: taken
            .into_iter()
            .map(|at| {
                let segment = &segments[order[at]];
                Copied {
                    speaker: speaker(segment),
                    text: segment.text.clone(),
                    start_offset_ns: segment.start_offset_ns,
                    end_offset_ns: segment.end_offset_ns,
                }
            })
            .collect(),
        answers,
    })
}

fn line_bytes(segment: &LiveSegment) -> usize {
    serde_json::to_string(&Line {
        line: String::from("L000"),
        speaker: &speaker(segment),
        text: &segment.text,
    })
    .map_or(usize::MAX, |line| line.len() + 1)
}

fn speaker(segment: &LiveSegment) -> String {
    let number = match segment.speaker {
        MeetingProvisionalSpeaker::Cluster { index } => Some(index + 1),
        _ => None,
    };
    match (segment.source_kind, number) {
        (SourceKind::SystemAudio, Some(number)) => format!("Caller {number}"),
        (SourceKind::SystemAudio, None) => "Caller".to_string(),
        (SourceKind::Microphone, Some(number)) => format!("Speaker {number}"),
        (SourceKind::Microphone, None) => "Speaker".to_string(),
    }
}

/// Whether a line sounds like a promise: one of the cue phrases, or a day
/// or time.
fn cue(text: &str, said_at: Option<NaiveDateTime>) -> bool {
    let mut plain = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\'' | '\u{2019}' => plain.push('\''),
            character if character.is_alphanumeric() => plain.extend(character.to_lowercase()),
            _ => plain.push(' '),
        }
    }
    let words = format!(
        " {} ",
        plain.split_whitespace().collect::<Vec<_>>().join(" ")
    );
    CUES.iter().any(|cue| words.contains(cue))
        || said_at.is_some_and(|said_at| spoken_time::resolve(text, said_at).is_some())
}

/// One promise the engine named, if the lines it cites bear it out: the
/// lines were in its request, and the quote and the words that say when are
/// in them. The owner is the user only when the user's own line is in the
/// request, and a name only when the lines say it.
fn checked(
    heard: Heard,
    lines: &[Copied],
    people: &[WorkPersonChoice],
    began_utc_ms: i64,
) -> Option<MeetingLivePromise> {
    let mut cited = heard
        .lines
        .iter()
        .map(|label| {
            let position = label.trim().strip_prefix('L')?.parse::<usize>().ok()?;
            let index = position.checked_sub(1)?;
            (index < lines.len()).then_some(index)
        })
        .collect::<Option<Vec<_>>>()?;
    cited.sort_unstable();
    cited.dedup();
    let first = &lines[*cited.first()?];
    let haystack = fold_haystack(cited.iter().map(|&index| lines[index].text.as_str()));
    let quote = bounded(&heard.quote, MAX_QUOTE_CHARS)?;
    if !quote_is_verbatim(&quote, &haystack) {
        return None;
    }
    let what = bounded(&heard.what, MAX_WHAT_CHARS)?;
    let mut letters = what.chars();
    let what: String = letters
        .next()
        .map(|first| first.to_uppercase().chain(letters).collect())?;
    let due_words = bounded(&heard.when, MAX_WHAT_CHARS);
    if due_words
        .as_deref()
        .is_some_and(|when| !quote_is_verbatim(when, &haystack))
    {
        return None;
    }
    let said = said_at(began_utc_ms, first.start_offset_ns)?;
    let due_at_ms = due_words
        .as_deref()
        .and_then(|when| spoken_time::resolve(when, said))
        .and_then(|time| due_ms(time, said));
    let (owner, owner_name) = owner(&heard.owner, lines, people);
    Some(MeetingLivePromise {
        id: Uuid::new_v4().to_string(),
        what,
        owner,
        owner_name,
        owner_set: false,
        edited: false,
        due_at_ms,
        due_words: due_at_ms.and(due_words),
        quote,
        start_offset_ns: cited
            .iter()
            .map(|&index| lines[index].start_offset_ns)
            .min()?,
        end_offset_ns: cited
            .iter()
            .map(|&index| lines[index].end_offset_ns)
            .max()?,
    })
}

fn owner(said: &str, lines: &[Copied], people: &[WorkPersonChoice]) -> (WorkOwner, Option<String>) {
    let Some(name) = bounded(said, MAX_NAME_CHARS) else {
        return (WorkOwner::Nobody, None);
    };
    if lines.iter().any(|line| line.speaker == name) {
        return (WorkOwner::Nobody, None);
    }
    let haystack = fold_haystack(lines.iter().map(|line| line.text.as_str()));
    if !quote_is_verbatim(&name, &haystack) {
        return (WorkOwner::Nobody, None);
    }
    match person_named(people, &name, false) {
        Some(person) => (
            WorkOwner::Person {
                person_id: person.id.clone(),
            },
            Some(person.name.clone()),
        ),
        None => (WorkOwner::Nobody, Some(name)),
    }
}

/// Keep `promise` unless the card has it: the same quote, or the same
/// moment said again by the same owner. A promise heard again fills in the
/// due date or the owner it lacked. Returns whether the card changed.
fn merge(promises: &mut Vec<MeetingLivePromise>, promise: MeetingLivePromise) -> bool {
    let same = promises.iter_mut().find(|kept| {
        fold(&kept.quote) == fold(&promise.quote)
            || (kept.start_offset_ns < promise.end_offset_ns
                && promise.start_offset_ns < kept.end_offset_ns
                && (kept.owner == promise.owner || kept.owner_set))
    });
    let Some(kept) = same else {
        promises.push(promise);
        return true;
    };
    let mut changed = false;
    if !kept.edited && kept.due_at_ms.is_none() && promise.due_at_ms.is_some() {
        kept.due_at_ms = promise.due_at_ms;
        kept.due_words = promise.due_words;
        changed = true;
    }
    if !kept.owner_set && kept.owner == WorkOwner::Nobody && promise.owner != WorkOwner::Nobody {
        kept.owner = promise.owner;
        kept.owner_name = promise.owner_name;
        changed = true;
    }
    changed
}

/// The local time a line was said, from when the meeting began.
fn said_at(began_utc_ms: i64, offset_ns: u64) -> Option<NaiveDateTime> {
    let offset_ms = i64::try_from(offset_ns / 1_000_000).ok()?;
    Local
        .timestamp_millis_opt(began_utc_ms.checked_add(offset_ms)?)
        .single()
        .map(|said_at| said_at.naive_local())
}

/// When a spoken time makes a promise due, unless it was already over when
/// it was said.
fn due_ms(time: SpokenTime, said: NaiveDateTime) -> Option<i64> {
    let at = match time {
        SpokenTime::At { start, .. } => start,
        SpokenTime::Day(day) => day.and_time(NaiveTime::from_hms_opt(DAY_DUE_HOUR, 0, 0)?),
    };
    if at <= said {
        return None;
    }
    Local
        .from_local_datetime(&at)
        .earliest()
        .map(|at| at.timestamp_millis())
}

/// `text` on one line with its spaces squeezed, or `None` when that leaves
/// nothing or more than `max_chars`.
fn bounded(text: &str, max_chars: usize) -> Option<String> {
    let text = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|character| !character.is_control())
        .collect::<String>();
    (!text.is_empty() && text.chars().count() <= max_chars).then_some(text)
}

#[cfg(test)]
pub(super) mod probe;

#[cfg(test)]
mod tests {
    use super::super::ReplyShape;
    use super::*;
    use crate::meeting::store::workflow_core_tests;
    use std::borrow::Cow;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// An engine that hears no promises and counts how often it is asked.
    struct Asked(AtomicUsize);

    impl MeetingTextGenerator for Asked {
        fn is_available(&self) -> bool {
            true
        }

        fn model_id(&self) -> &'static str {
            "apple-intelligence"
        }

        fn model_version(&self) -> Cow<'static, str> {
            Cow::Borrowed("asked-v1")
        }

        fn max_input_bytes(&self) -> usize {
            64 * 1024
        }

        fn generate(
            &self,
            _system_prompt: &str,
            _evidence: &str,
            _max_tokens: i32,
            _shape: ReplyShape,
        ) -> Result<String, MeetingTextGenerationError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(r#"{"promises":[]}"#.to_string())
        }
    }

    #[test]
    fn promises_made_every_twenty_seconds_cost_one_request_a_minute() {
        let (_directory, store) = workflow_core_tests::store();
        let session_id = workflow_core_tests::reviewable_meeting(
            &store,
            "Launch sync",
            Local::now().timestamp_millis(),
        );
        let (microphone, _) = workflow_core_tests::recording_tracks(&store, session_id);
        let engine = Arc::new(Asked(AtomicUsize::new(0)));
        let service = MeetingProcessingService::new(None);
        service.set_text_generators(engine.clone(), engine.clone());
        let live = LiveTranscript::new();

        for pass in 0..30_u64 {
            let start_offset_ns = pass * 20_000_000_000;
            live.append(LiveSegment {
                track_id: microphone,
                source_kind: SourceKind::Microphone,
                speaker: MeetingProvisionalSpeaker::Unknown,
                start_offset_ns,
                end_offset_ns: start_offset_ns + 5_000_000_000,
                text: format!("I'll send draft {pass} over."),
            });
            tick(&service, &store, session_id, &live, Moment::Pass);
        }

        assert_eq!(engine.0.load(Ordering::SeqCst), 10);
    }
}

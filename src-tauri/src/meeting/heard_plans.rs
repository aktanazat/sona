//! Plans to meet, read out of a meeting's transcript once its notes are
//! written.
//!
//! Only an engine on this Mac reads them: Apple Intelligence, or the
//! operator's own endpoint. Only the lines that name a day or a time are
//! read, with the lines around them, in requests the engine's window holds.
//! A plan is kept only when its quote, and the words that say when, are in
//! the lines it cites; the day those words name is worked out here, against
//! the moment they were said, never by the model.

use super::analytics::AnalyticsSegment;
use super::heard_plan_types::{HeardPlanDraft, HeardPlanWindow};
use super::ledger::{fold, fold_haystack, quote_is_verbatim};
use super::processing::{
    evidence_budget, first_json_value, json_response_schema, MeetingProcessingService,
    MeetingTextGenerationError, MeetingTextGenerator,
};
use super::spoken_time::{self, SpokenTime};
use super::store::{MeetingStore, StoreError};
use super::types::{
    ArtifactCitation, MeetingEventPayload, MeetingHeardPlansChangedEvent, MeetingSessionId,
    SpeakerId,
};
use super::words::{points_back, trails_off};
use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone};
use schemars::{schema::RootSchema, JsonSchema};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::LazyLock;
use tauri::{AppHandle, Emitter};

const PROMPT: &str = include_str!("../../resources/prompts/plans.txt");
const OUTPUT_TOKENS: i32 = 600;
const MAX_PLANS: usize = 8;
/// The lines read either side of one that names a day or a time: a plan is
/// often agreed a line or two after the time is said.
const CONTEXT_LINES: usize = 2;
const MAX_TITLE_CHARS: usize = 120;
const MAX_PLACE_CHARS: usize = 200;
const MAX_NAME_CHARS: usize = 80;
const MAX_PEOPLE: usize = 12;
/// `{"lines":[]}`, the request around the lines.
const ENVELOPE_BYTES: usize = 12;
static RESPONSE_SCHEMA: LazyLock<RootSchema> = LazyLock::new(json_response_schema::<Reply>);

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Reply {
    #[schemars(length(max = "MAX_PLANS"))]
    plans: Vec<Heard>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Heard {
    what: String,
    when: String,
    #[serde(default)]
    people: Vec<String>,
    #[serde(default)]
    place: String,
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

/// The engine could not be asked, or did not answer in time: the meeting
/// is read again the next time its notes are written.
#[derive(Debug)]
struct NotAsked;

/// Read the plans heard in a meeting whose notes were just written, and
/// tell the review when they changed. Never fails the meeting: a reading
/// that goes wrong is logged and the notes stand as they are.
pub(crate) fn after_notes(
    store: &MeetingStore,
    app: Option<&AppHandle>,
    processing: &MeetingProcessingService,
    session_id: MeetingSessionId,
) {
    match read(
        store,
        processing,
        session_id,
        chrono::Utc::now().timestamp_millis(),
    ) {
        Ok(false) => {}
        Ok(true) => {
            if let (Some(app), Ok(snapshot)) = (app, store.session_snapshot(session_id)) {
                let _ = app.emit(
                    <MeetingHeardPlansChangedEvent as tauri_specta::Event>::NAME,
                    MeetingEventPayload {
                        event_schema_version: 1,
                        session_id: Some(session_id),
                        revision: snapshot.revision,
                    },
                );
            }
        }
        Err(error) => log::warn!("Plans heard in {session_id:?} were not read: {error:?}"),
    }
}

/// Read one meeting's plans, once per transcript. Returns whether its plans
/// changed.
pub(crate) fn read(
    store: &MeetingStore,
    processing: &MeetingProcessingService,
    session_id: MeetingSessionId,
    now_utc_ms: i64,
) -> Result<bool, StoreError> {
    let Some((revision, began_utc_ms)) = store.heard_plans_unread(session_id)? else {
        return Ok(false);
    };
    let segments = store.analytics_segments(session_id)?;
    let windows = windows(&segments, began_utc_ms);
    let found = if windows.is_empty() {
        Vec::new()
    } else {
        let Some(generator) = processing.local_text_generator() else {
            return Ok(false);
        };
        let speakers = store.speaker_display_names(session_id)?;
        match plans_in(
            processing,
            generator.as_ref(),
            &segments,
            &speakers,
            &windows,
            began_utc_ms,
        ) {
            Ok(found) => found,
            Err(NotAsked) => return Ok(false),
        }
    };
    store.replace_heard_plans(session_id, revision, &found, now_utc_ms)
}

/// The lines worth reading, as runs of transcript positions: each line that
/// names a day or a time, with the lines around it.
fn windows(segments: &[AnalyticsSegment], began_utc_ms: i64) -> Vec<Range<usize>> {
    let mut windows: Vec<Range<usize>> = Vec::new();
    for (index, segment) in segments.iter().enumerate() {
        let Some(said_at) = said_at(began_utc_ms, segment) else {
            continue;
        };
        if spoken_time::resolve(&segment.text, said_at).is_none() {
            continue;
        }
        let window =
            index.saturating_sub(CONTEXT_LINES)..(index + CONTEXT_LINES + 1).min(segments.len());
        match windows.last_mut() {
            Some(last) if last.end >= window.start => last.end = window.end,
            _ => windows.push(window),
        }
    }
    windows
}

/// The windows packed into requests of at most `budget` bytes. A window too
/// long for one request is read line by line, and a line too long for a
/// request of its own is not read.
fn requests(
    windows: &[Range<usize>],
    line_bytes: impl Fn(usize) -> usize,
    budget: usize,
) -> Vec<Vec<usize>> {
    let mut requests = Vec::new();
    let mut request = Vec::new();
    let mut used = ENVELOPE_BYTES;
    for window in windows {
        let bytes = window
            .clone()
            .map(&line_bytes)
            .fold(0, usize::saturating_add);
        let pieces = if ENVELOPE_BYTES.saturating_add(bytes) <= budget {
            vec![window.clone()]
        } else {
            window.clone().map(|index| index..index + 1).collect()
        };
        for piece in pieces {
            let bytes = piece
                .clone()
                .map(&line_bytes)
                .fold(0, usize::saturating_add);
            if ENVELOPE_BYTES.saturating_add(bytes) > budget {
                continue;
            }
            if used.saturating_add(bytes) > budget {
                requests.push(std::mem::take(&mut request));
                used = ENVELOPE_BYTES;
            }
            request.extend(piece);
            used += bytes;
        }
    }
    if !request.is_empty() {
        requests.push(request);
    }
    requests
}

fn plans_in(
    processing: &MeetingProcessingService,
    generator: &dyn MeetingTextGenerator,
    segments: &[AnalyticsSegment],
    speakers: &HashMap<SpeakerId, String>,
    windows: &[Range<usize>],
    began_utc_ms: i64,
) -> Result<Vec<HeardPlanDraft>, NotAsked> {
    let line = |index: usize| {
        let segment = &segments[index];
        Line {
            line: format!("L{}", index + 1),
            speaker: speakers
                .get(&segment.speaker_id)
                .map_or("Speaker", String::as_str),
            text: &segment.text,
        }
    };
    let line_bytes = |index: usize| {
        serde_json::to_string(&line(index)).map_or(usize::MAX, |line| line.len() + 1)
    };
    let budget = evidence_budget(generator, PROMPT, OUTPUT_TOKENS);
    let mut found = Vec::<HeardPlanDraft>::new();
    let mut quoted = HashSet::new();
    let mut named = HashSet::new();
    for request in requests(windows, line_bytes, budget) {
        processing.wait_while_recording();
        let lines = request.iter().map(|&index| line(index)).collect::<Vec<_>>();
        let Ok(input) = serde_json::to_string(&Input { lines: &lines }) else {
            continue;
        };
        let answer = match generator
            .generate_json(PROMPT, &input, OUTPUT_TOKENS, || &RESPONSE_SCHEMA)
        {
            Ok(answer) => answer,
            Err(MeetingTextGenerationError::Unreachable | MeetingTextGenerationError::TimedOut) => {
                return Err(NotAsked);
            }
            Err(error) => {
                log::info!("A plan reading got no answer: {error:?}");
                continue;
            }
        };
        let Ok(reply) = first_json_value::<Reply>(answer.trim()) else {
            log::info!("A plan reading answered something that is not a list of plans");
            continue;
        };
        for heard in reply.plans.into_iter().take(MAX_PLANS) {
            let Some(mut draft) = checked(heard, &request, segments, began_utc_ms) else {
                continue;
            };
            let window = draft
                .window
                .map(|window| (window.start_utc_ms, window.end_utc_ms));
            if quoted.insert(fold(&draft.quote)) && named.insert((fold(&draft.title), window)) {
                let earlier = found.iter_mut().find_map(|kept| {
                    if kept.title != draft.title
                        || kept.people != draft.people
                        || kept.place != draft.place
                        || kept.citation.start_offset_ns >= draft.citation.end_offset_ns
                        || draft.citation.start_offset_ns >= kept.citation.end_offset_ns
                    {
                        return None;
                    }
                    let (day, timed) = match (kept.window, draft.window) {
                        (Some(day), _)
                            if day.all_day && !draft.window.is_some_and(|w| w.all_day) =>
                        {
                            (day, &draft)
                        }
                        (_, Some(day))
                            if day.all_day && !kept.window.is_some_and(|w| w.all_day) =>
                        {
                            (day, &*kept)
                        }
                        _ => return None,
                    };
                    let time = timed.window.or_else(|| clock_on_day(day, timed))?;
                    if day.start_utc_ms > time.start_utc_ms || time.end_utc_ms > day.end_utc_ms {
                        return None;
                    }
                    let citation = ArtifactCitation {
                        segment_id: if kept.citation.start_offset_ns
                            <= draft.citation.start_offset_ns
                        {
                            kept.citation.segment_id
                        } else {
                            draft.citation.segment_id
                        },
                        start_offset_ns: kept
                            .citation
                            .start_offset_ns
                            .min(draft.citation.start_offset_ns),
                        end_offset_ns: kept
                            .citation
                            .end_offset_ns
                            .max(draft.citation.end_offset_ns),
                    };
                    Some((kept, time, citation))
                });
                match earlier {
                    Some((earlier, time, citation))
                        if draft.window.is_some_and(|window| window.all_day) =>
                    {
                        earlier.window = Some(time);
                        earlier.citation = citation;
                    }
                    Some((earlier, time, citation)) => {
                        draft.window = Some(time);
                        draft.citation = citation;
                        *earlier = draft;
                    }
                    None => found.push(draft),
                }
            }
        }
    }
    Ok(found)
}

fn clock_on_day(day: HeardPlanWindow, timed: &HeardPlanDraft) -> Option<HeardPlanWindow> {
    let context = Local
        .timestamp_millis_opt(day.start_utc_ms)
        .single()?
        .naive_local();
    if spoken_time::day(&timed.quote, context.date()).is_some() {
        return None;
    }
    let (time, ranges) = spoken_time::find(&timed.quote, context, spoken_time::DayNames::All)?;
    let said = &timed.quote[ranges.first()?.clone()];
    quote_is_verbatim(&timed.when_text, &fold(said)).then(|| window(time))?
}

/// One plan the engine named, if the lines it cites bear it out: the lines
/// were in its request, the quote and the words that say when are in them,
/// and that time was still to come when it was said. The people and place
/// are kept only as far as those lines name them. A plan whose name breaks
/// off mid-phrase ("Send the"), or that names no one and ends on a word
/// standing for something said before it ("decide that"), has nothing to
/// call it by and drops out.
fn checked(
    heard: Heard,
    request: &[usize],
    segments: &[AnalyticsSegment],
    began_utc_ms: i64,
) -> Option<HeardPlanDraft> {
    let mut cited = heard
        .lines
        .iter()
        .map(|label| {
            let position = label.trim().strip_prefix('L')?.parse::<usize>().ok()?;
            let index = position.checked_sub(1)?;
            request.contains(&index).then_some(index)
        })
        .collect::<Option<Vec<_>>>()?;
    cited.sort_unstable();
    cited.dedup();
    let first = &segments[*cited.first()?];
    let haystack = fold_haystack(cited.iter().map(|&index| segments[index].text.as_str()));
    if !quote_is_verbatim(&heard.quote, &haystack) || !quote_is_verbatim(&heard.when, &haystack) {
        return None;
    }
    let title = bounded(&heard.what, MAX_TITLE_CHARS).filter(|title| !trails_off(title))?;
    let mut people = Vec::new();
    for name in &heard.people {
        let Some(name) = bounded(name, MAX_NAME_CHARS) else {
            continue;
        };
        if people.len() < MAX_PEOPLE
            && quote_is_verbatim(&name, &haystack)
            && !people.iter().any(|kept: &String| fold(kept) == fold(&name))
        {
            people.push(name);
        }
    }
    if people.is_empty() && points_back(&title) {
        return None;
    }
    let place =
        bounded(&heard.place, MAX_PLACE_CHARS).filter(|place| quote_is_verbatim(place, &haystack));
    let said = said_at(began_utc_ms, first)?;
    let time = spoken_time::resolve(&heard.when, said);
    if time.is_some_and(|time| over_by(time, said)) {
        return None;
    }
    let window = time.and_then(window);
    let start_offset_ns = cited
        .iter()
        .map(|&index| segments[index].start_offset_ns)
        .min()?;
    let end_offset_ns = cited
        .iter()
        .map(|&index| segments[index].end_offset_ns)
        .max()?;
    Some(HeardPlanDraft {
        title,
        when_text: bounded(&heard.when, MAX_TITLE_CHARS)?,
        window,
        people,
        place,
        quote: heard.quote.trim().to_string(),
        citation: ArtifactCitation {
            segment_id: first.segment_id,
            start_offset_ns,
            end_offset_ns,
        },
    })
}

/// The local time a line was said, from when the meeting began.
fn said_at(began_utc_ms: i64, segment: &AnalyticsSegment) -> Option<NaiveDateTime> {
    let offset_ms = i64::try_from(segment.start_offset_ns / 1_000_000).ok()?;
    Local
        .timestamp_millis_opt(began_utc_ms.checked_add(offset_ms)?)
        .single()
        .map(|said_at| said_at.naive_local())
}

/// Whether `time` was over when it was said: a meeting told of, not planned.
fn over_by(time: SpokenTime, said: NaiveDateTime) -> bool {
    match time {
        SpokenTime::At { end, .. } => end <= said,
        SpokenTime::Day(day) => day < said.date(),
    }
}

/// The window a spoken time names, in this Mac's time zone. A day is the
/// whole of it, from one local midnight to the next.
fn window(time: SpokenTime) -> Option<HeardPlanWindow> {
    let (start, end, all_day) = match time {
        SpokenTime::At { start, end } => (start, end, false),
        SpokenTime::Day(day) => (midnight(day)?, midnight(day.succ_opt()?)?, true),
    };
    let start_utc_ms = Local
        .from_local_datetime(&start)
        .earliest()?
        .timestamp_millis();
    let end_utc_ms = Local
        .from_local_datetime(&end)
        .earliest()?
        .timestamp_millis();
    (start_utc_ms < end_utc_ms).then_some(HeardPlanWindow {
        start_utc_ms,
        end_utc_ms,
        all_day,
    })
}

fn midnight(day: NaiveDate) -> Option<NaiveDateTime> {
    day.and_hms_opt(0, 0, 0)
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

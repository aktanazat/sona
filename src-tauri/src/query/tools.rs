//! The Sona tools: what an answering model can ask this Mac to run.
//!
//! A keyword pack answers "what did we say about the deck"; it cannot answer
//! "what are my most used words" or "what did Nolan and I leave open". So the
//! chat brain may reply with a `tool_calls` request instead of an answer, the
//! panel runs each call through [`run`], and the results ride back inside the
//! next round's pack. Ten tools, catalogued as [`TOOL_CATALOGUE_VERSION`], each
//! with exactly the argument names [`TOOL_ARGS`] lists. The relay carries the
//! same table (`SONA_TOOL_ARGS` in `omp_bridge/sona_chat.py`) and checks names
//! and value shapes on the way out; every bound on a value is enforced here,
//! and only here.
//!
//! # What a tool may see
//!
//! Exactly what a pack may see. Every row a result quotes goes through
//! [`without_excluded_series`] before it is rendered, so a series the operator
//! kept on this Mac (D14) is absent from a listing and refused when named
//! directly. The refusal for a named meeting reads the same as "not found":
//! the reason `pack.rs` gives for its silent header holds here too, since a
//! result is text that leaves the machine, and naming the exclusion would put
//! the fact of it on the server that was not allowed to see the series.
//!
//! # Bounds
//!
//! One result is at most [`TOOL_RESULT_MAX_BYTES`] of JSON. A result that
//! would be larger loses trailing array elements — the oldest rows of a
//! listing, the last segments of a transcript page, a meeting's notes before
//! its ledger — and says so with `truncated: true`. A bad argument, an unknown
//! tool or a store that cannot be read is `ok: false` with one line of error.
//! Nothing here panics on input and nothing retries: the model reads the error
//! in the next round and asks differently, or stops.

use super::external::{current_artifacts, speaker_names, transcript_line};
use super::pack::{one_line, without_excluded_series};
use super::{
    bounded, dictation_row, loop_link, meeting_link, person_link, token, QueryError, QueryRow,
    QueryRowKind, QueryScope, MAX_SNIPPET_CHARS, MAX_TITLE_CHARS,
};
use crate::analytics::{local_days_start_utc_ms, DashboardTrendRange, DashboardTrendRequest};
use crate::managers::history::{
    HistoryEntry, HistoryManager, HistoryRunReceipt, HistoryTrendPoint,
};
use crate::meeting::analytics::talk_metrics;
use crate::meeting::detection::calendar::{CalendarAccess, CalendarSource};
use crate::meeting::ledger::{
    LedgerFirmness, LedgerReceiptState, LedgerThreadState, MeetingLedger,
};
use crate::meeting::loop_types::{MeetingLoopRow, MeetingLoopStatus};
use crate::meeting::people_types::{PersonId, PersonLinkConfidence};
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::store::{MeetingStore, StoreError};
use crate::meeting::types::{
    MeetingHistoryHeadline, MeetingHistorySummary, MeetingListFilter, MeetingReviewSnapshot,
    MeetingSessionId, MeetingTrendPoint, MeetingTrendProjection,
};
use crate::meeting::upcoming::upcoming_window;
use crate::meeting::upcoming_types::MeetingUpcomingRow;
use chrono::{DateTime, Local, SecondsFormat, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

/// The largest result one call may return, in bytes of JSON.
pub const TOOL_RESULT_MAX_BYTES: usize = 8 * 1024;

/// Bumped when a tool's arguments or result shape change meaning. The relay
/// names the same version in its prompt.
pub const TOOL_CATALOGUE_VERSION: &str = "sona-tools/1";

/// The catalogue: every tool, in the order the relay lists them, with exactly
/// the argument names it accepts. Mirrored by `SONA_TOOL_ARGS` in
/// `omp_bridge/sona_chat.py`; a call naming an argument outside this list is
/// refused, so a catalogue that drifts fails loudly in the model's next round
/// rather than silently ignoring what it asked for.
pub const TOOL_ARGS: [(&str, &[&str]); 10] = [
    ("search", &["query", "scope", "limit"]),
    ("recent", &["scope", "limit", "days"]),
    ("meeting", &["session_id"]),
    ("transcript", &["session_id", "offset", "limit"]),
    ("person", &["person_id"]),
    ("loops", &["status", "person_id", "limit"]),
    ("upcoming", &["days"]),
    ("dictation", &["entry_id"]),
    ("word_stats", &["days", "limit"]),
    ("activity", &["days"]),
];

/// The longest search query a call may carry, in characters.
const MAX_QUERY_CHARS: usize = 256;

/// How deep the corpus-wide loop walk goes, as `external.rs` bounds it.
const LOOP_SCAN_DEPTH: usize = 500;

/// How many meeting list pages a `recent` call will turn before it stops, and
/// how many rows each holds: four hundred meetings is far more than a year of
/// them, and a listing is not the tool for reading the whole corpus.
const RECENT_MAX_PAGES: usize = 4;
const RECENT_PAGE_ROWS: usize = 100;

/// How many dictations a word count reads at most, newest first. Five thousand
/// entries is years of daily use; a window that holds more is counted from
/// its newest five thousand and the result says so.
pub(super) const WORD_STATS_MAX_ENTRIES: usize = 5_000;

/// How many meetings a person's result names, newest first. Enough to hand
/// the model ids to open; the count beside them says how many there are.
const PERSON_MEETINGS: usize = 8;

/// English words that carry no topic, plus the fragments splitting on
/// apostrophes leaves behind and the sounds a dictation makes while thinking.
/// Sorted, so membership is a binary search; a test holds the order.
const STOPWORDS: [&str; 145] = [
    "about", "above", "after", "again", "all", "also", "am", "an", "and", "any", "are", "aren",
    "as", "at", "be", "because", "been", "before", "being", "below", "between", "both", "but",
    "by", "can", "could", "couldn", "did", "didn", "do", "does", "doesn", "doing", "don", "down",
    "during", "each", "few", "for", "from", "further", "had", "hadn", "has", "hasn", "have",
    "haven", "having", "he", "her", "here", "hers", "hey", "hi", "him", "his", "hmm", "how", "if",
    "in", "into", "is", "isn", "it", "its", "just", "ll", "me", "mm", "more", "most", "my", "no",
    "nor", "not", "now", "of", "off", "oh", "ok", "okay", "on", "once", "only", "or", "other",
    "our", "ours", "out", "over", "own", "re", "same", "she", "should", "shouldn", "so", "some",
    "such", "than", "that", "the", "their", "theirs", "them", "then", "there", "these", "they",
    "this", "those", "through", "to", "too", "uh", "um", "under", "until", "up", "us", "ve",
    "very", "was", "wasn", "we", "were", "weren", "what", "when", "where", "which", "while", "who",
    "whom", "why", "will", "with", "won", "would", "wouldn", "yeah", "yes", "you", "your", "yours",
];

/// One lookup the model asked for, as the relay's `tool_calls` reply carries
/// it; `args` is whatever object the model wrote, checked here. Unknown
/// fields are refused at the decode, the way every other response shape on
/// that wire is.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    pub id: String,
    pub tool: String,
    pub args: Value,
}

/// What one call produced. `result` is JSON text when `ok`, one line of error
/// otherwise; `sources` are the rows the result quotes, so the sheet can cite
/// tool-found evidence the way it cites pack evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ToolResult {
    pub id: String,
    pub tool: String,
    pub ok: bool,
    pub result: String,
    pub sources: Vec<QueryRow>,
}

/// Run one call against the corpus this Mac holds.
///
/// The calendar read behind `upcoming` blocks on EventKit, so it runs off the
/// async runtime's worker threads, as `meeting_upcoming_events` runs it.
pub async fn run(
    meetings: &Arc<MeetingSessionManager>,
    history: &Arc<HistoryManager>,
    calendar: &Arc<dyn CalendarSource>,
    call: &ToolCall,
) -> ToolResult {
    match answer(meetings, history, calendar, call).await {
        Ok(outcome) => finish(call, outcome),
        Err(error) => failure(call, &error),
    }
}

async fn answer(
    meetings: &Arc<MeetingSessionManager>,
    history: &Arc<HistoryManager>,
    calendar: &Arc<dyn CalendarSource>,
    call: &ToolCall,
) -> Result<Outcome, String> {
    let request = parse(call)?;
    let now = Local::now();
    let store = meetings
        .store()
        .await
        .map_err(|error| refusal(QueryError::from(error)))?;
    match request {
        Request::Search {
            query,
            scope,
            limit,
        } => {
            let page = super::search(meetings, history, scope, &query, Some(limit), None)
                .await
                .map_err(refusal)?;
            Ok(search_result(
                &store,
                page.entries,
                page.next_cursor.is_some(),
            ))
        }
        Request::Recent {
            scope: RecentScope::Meetings,
            limit,
            days,
        } => recent_meetings(&store, window_start(now, days)?, limit),
        Request::Recent {
            scope: RecentScope::Dictations,
            limit,
            days,
        } => {
            let since = window_start(now, days)? / 1000;
            let entries = history
                .get_history_entries_since(since, limit + 1)
                .await
                .map_err(|_| HISTORY_UNREADABLE.to_string())?;
            Ok(recent_dictations(&store, entries, limit))
        }
        Request::Meeting { session_id } => meeting_result(&store, session_id),
        Request::Transcript {
            session_id,
            offset,
            limit,
        } => transcript_result(&store, session_id, offset, limit),
        Request::Person { person_id } => person_result(&store, person_id),
        Request::Loops {
            status,
            person_id,
            limit,
        } => loops_result(&store, status, person_id, limit),
        Request::Upcoming { days } => {
            let window = (now.timestamp_millis(), upcoming_window(now, days).1);
            let source = Arc::clone(calendar);
            let occurrences = tauri::async_runtime::spawn_blocking(move || {
                source.events_between(window.0, window.1)
            })
            .await
            .unwrap_or_default();
            let events = meetings
                .upcoming_events(calendar.access(), window, occurrences)
                .await
                .map_err(|error| refusal(QueryError::from(error)))?;
            upcoming_result(&store, events.access, events.rows)
        }
        Request::Dictation { entry_id } => {
            let entry = history
                .get_entry_by_id(entry_id)
                .await
                .map_err(|_| HISTORY_UNREADABLE.to_string())?
                .ok_or_else(|| format!("no dictation {entry_id} in this history"))?;
            let receipts = history.get_run_receipts(entry_id).await.unwrap_or_default();
            Ok(dictation_result(&entry, &receipts))
        }
        Request::WordStats { days, limit } => {
            let since = window_start(now, days)? / 1000;
            let entries = history
                .get_history_entries_since(since, WORD_STATS_MAX_ENTRIES)
                .await
                .map_err(|_| HISTORY_UNREADABLE.to_string())?;
            Ok(word_stats_result(&entries, days, limit))
        }
        Request::Activity { days } => {
            let request = DashboardTrendRequest {
                range: trend_range(days),
            };
            let dictations = history
                .get_history_trend(request)
                .await
                .map_err(|_| HISTORY_UNREADABLE.to_string())?;
            let meetings = match store.trend_projection(request).map_err(store_refusal)? {
                MeetingTrendProjection::Available { points, .. } => points,
                MeetingTrendProjection::Unavailable { .. } => {
                    return Err(refusal(QueryError::Unavailable));
                }
            };
            Ok(activity_result(&dictations.points, &meetings, days))
        }
    }
}

const HISTORY_UNREADABLE: &str = "the dictation history could not be read";

/* ------------------------------------------------------------ arguments */

enum RecentScope {
    Meetings,
    Dictations,
}

#[derive(Clone, Copy)]
enum LoopFilter {
    Open,
    Done,
    All,
}

/// One call, with every argument checked and defaulted.
enum Request {
    Search {
        query: String,
        scope: QueryScope,
        limit: usize,
    },
    Recent {
        scope: RecentScope,
        limit: usize,
        days: u32,
    },
    Meeting {
        session_id: MeetingSessionId,
    },
    Transcript {
        session_id: MeetingSessionId,
        offset: usize,
        limit: usize,
    },
    Person {
        person_id: PersonId,
    },
    Loops {
        status: LoopFilter,
        person_id: Option<PersonId>,
        limit: usize,
    },
    Upcoming {
        days: u32,
    },
    Dictation {
        entry_id: i64,
    },
    WordStats {
        days: u32,
        limit: usize,
    },
    Activity {
        days: u32,
    },
}

/// Check a call against the catalogue: the tool has to exist, its arguments
/// have to be the ones the table lists, and every value has to be the right
/// type inside the table's bounds. The first thing wrong is the error.
fn parse(call: &ToolCall) -> Result<Request, String> {
    let Some((_, names)) = TOOL_ARGS.iter().find(|(name, _)| *name == call.tool) else {
        return Err(format!("unknown tool {}", short(&call.tool)));
    };
    let empty = Map::new();
    let args = match &call.args {
        Value::Object(args) => args,
        Value::Null => &empty,
        _ => return Err("args must be an object".to_string()),
    };
    if let Some(unknown) = args.keys().find(|key| !names.contains(&key.as_str())) {
        return Err(format!(
            "{} takes no argument {}",
            call.tool,
            short(unknown)
        ));
    }
    let request = match call.tool.as_str() {
        "search" => Request::Search {
            query: required(text(args, "query", MAX_QUERY_CHARS)?, "query")?,
            scope: match choice(
                args,
                "scope",
                &["all", "meetings", "dictations", "people", "loops"],
            )? {
                None | Some("all") => QueryScope::All,
                Some("meetings") => QueryScope::Meetings,
                Some("dictations") => QueryScope::Dictations,
                Some("people") => QueryScope::People,
                Some(_) => QueryScope::Loops,
            },
            limit: count(args, "limit", 1, 25, 12)?,
        },
        "recent" => Request::Recent {
            scope: match required(choice(args, "scope", &["meetings", "dictations"])?, "scope")? {
                "meetings" => RecentScope::Meetings,
                _ => RecentScope::Dictations,
            },
            limit: count(args, "limit", 1, 25, 10)?,
            days: days(args, 365, 30)?,
        },
        "meeting" => Request::Meeting {
            session_id: MeetingSessionId::from_uuid(required(
                uuid(args, "session_id")?,
                "session_id",
            )?),
        },
        "transcript" => Request::Transcript {
            session_id: MeetingSessionId::from_uuid(required(
                uuid(args, "session_id")?,
                "session_id",
            )?),
            offset: count(args, "offset", 0, i64::MAX, 0)?,
            limit: count(args, "limit", 1, 200, 80)?,
        },
        "person" => Request::Person {
            person_id: PersonId(required(uuid(args, "person_id")?, "person_id")?),
        },
        "loops" => Request::Loops {
            status: match choice(args, "status", &["open", "done", "all"])? {
                None | Some("open") => LoopFilter::Open,
                Some("done") => LoopFilter::Done,
                Some(_) => LoopFilter::All,
            },
            person_id: uuid(args, "person_id")?.map(PersonId),
            limit: count(args, "limit", 1, 50, 20)?,
        },
        "upcoming" => Request::Upcoming {
            days: days(args, 30, 7)?,
        },
        "dictation" => Request::Dictation {
            entry_id: entry_id(args)?,
        },
        "word_stats" => Request::WordStats {
            days: days(args, 3650, 90)?,
            limit: count(args, "limit", 1, 50, 25)?,
        },
        _ => Request::Activity {
            days: days(args, 90, 14)?,
        },
    };
    Ok(request)
}

/// A string argument of one to `max_chars` characters, when present.
fn text(args: &Map<String, Value>, name: &str, max_chars: usize) -> Result<Option<String>, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if (1..=max_chars).contains(&value.chars().count()) => {
            Ok(Some(value.clone()))
        }
        Some(_) => Err(format!(
            "{name} must be a string of 1 to {max_chars} characters"
        )),
    }
}

/// An integer argument inside `min..=max`, when present.
fn integer(
    args: &Map<String, Value>,
    name: &str,
    min: i64,
    max: i64,
) -> Result<Option<i64>, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => match value.as_i64() {
            Some(value) if (min..=max).contains(&value) => Ok(Some(value)),
            _ => Err(format!("{name} must be an integer from {min} to {max}")),
        },
        Some(_) => Err(format!("{name} must be an integer from {min} to {max}")),
    }
}

/// A count argument with its default: a limit, an offset.
fn count(
    args: &Map<String, Value>,
    name: &str,
    min: i64,
    max: i64,
    default: usize,
) -> Result<usize, String> {
    Ok(integer(args, name, min, max)?
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(default))
}

/// The `days` argument every window takes: one to `max`, with its default.
fn days(args: &Map<String, Value>, max: u32, default: u32) -> Result<u32, String> {
    Ok(integer(args, "days", 1, i64::from(max))?
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(default))
}

/// One of a fixed set of words, when present.
fn choice<'a>(
    args: &Map<String, Value>,
    name: &str,
    allowed: &[&'a str],
) -> Result<Option<&'a str>, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => match allowed.iter().find(|word| **word == value) {
            Some(word) => Ok(Some(word)),
            None => Err(format!("{name} must be one of {}", allowed.join(", "))),
        },
        Some(_) => Err(format!("{name} must be one of {}", allowed.join(", "))),
    }
}

/// A uuid argument, when present. Ids come out of `sona://` links, which
/// carry them as text.
fn uuid(args: &Map<String, Value>, name: &str) -> Result<Option<Uuid>, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Uuid::parse_str(value.trim())
            .map(Some)
            .map_err(|_| format!("{name} must be a uuid")),
        Some(_) => Err(format!("{name} must be a uuid")),
    }
}

/// A dictation's row id: an integer, or the same digits as a string, since a
/// model copying `sona://dictation/42` out of a pack has only text.
fn entry_id(args: &Map<String, Value>) -> Result<i64, String> {
    let id = match args.get("entry_id") {
        Some(Value::Number(value)) => value.as_i64(),
        Some(Value::String(value)) => value.trim().parse().ok(),
        _ => None,
    };
    id.filter(|id| *id >= 1)
        .ok_or_else(|| "entry_id must be a positive integer".to_string())
}

fn required<T>(value: Option<T>, name: &str) -> Result<T, String> {
    value.ok_or_else(|| format!("{name} is required"))
}

/// A word the model wrote, made safe to echo in one line of error.
fn short(value: &str) -> String {
    bounded(&one_line(value), 32)
}

/// The first instant of the local day `days - 1` days ago, in UTC ms.
fn window_start(now: DateTime<Local>, days: u32) -> Result<i64, String> {
    local_days_start_utc_ms(now, days)
        .map_err(|_| "the window predates the supported calendar".to_string())
}

/// The dashboard range that covers `days`, since the trend projections come
/// in three sizes and a day-level answer is cut from the smallest that fits.
fn trend_range(days: u32) -> DashboardTrendRange {
    match days {
        0..=7 => DashboardTrendRange::Days7,
        8..=30 => DashboardTrendRange::Days30,
        _ => DashboardTrendRange::Days180,
    }
}

/* -------------------------------------------------------------- results */

/// A result before it is measured: the object, and the rows it quotes.
///
/// `quoted` names the arrays whose elements each quote one row, in the order
/// the rows are listed, so that when an array loses its tail to the byte
/// ceiling the sources lose the same tail and a citation never points at a
/// row the model was not shown.
#[derive(Debug)]
struct Outcome {
    value: Map<String, Value>,
    /// Rows quoted outside any array that can be cut.
    fixed: Vec<QueryRow>,
    quoted: Vec<(&'static str, Vec<QueryRow>)>,
    /// Where the ceiling cuts, first to last. Each path names an array, which
    /// loses trailing elements, or a string, which loses trailing characters.
    cuts: &'static [&'static str],
}

impl Outcome {
    fn new(value: Map<String, Value>, cuts: &'static [&'static str]) -> Self {
        Self {
            value,
            fixed: Vec::new(),
            quoted: Vec::new(),
            cuts,
        }
    }
}

fn finish(call: &ToolCall, outcome: Outcome) -> ToolResult {
    let mut value = Value::Object(outcome.value);
    fit(&mut value, outcome.cuts);
    if measure(&value) > TOOL_RESULT_MAX_BYTES {
        return failure(call, "the result does not fit in 8 KiB");
    }
    let Ok(result) = serde_json::to_string(&value) else {
        return failure(call, "the result could not be serialized");
    };
    let mut sources = outcome.fixed;
    for (key, rows) in outcome.quoted {
        let kept = value.get(key).and_then(Value::as_array).map_or(0, Vec::len);
        sources.extend(rows.into_iter().take(kept));
    }
    ToolResult {
        id: short(&call.id),
        tool: short(&call.tool),
        ok: true,
        result,
        sources,
    }
}

fn failure(call: &ToolCall, error: &str) -> ToolResult {
    ToolResult {
        id: short(&call.id),
        tool: short(&call.tool),
        ok: false,
        result: bounded(&one_line(error), 256),
        sources: Vec::new(),
    }
}

/// Cut `value` down to the ceiling along `cuts`, marking it `truncated` when
/// anything was removed. Returns whether it was.
fn fit(value: &mut Value, cuts: &[&str]) -> bool {
    let mut truncated = false;
    while measure(value) > TOOL_RESULT_MAX_BYTES {
        if !truncated {
            truncated = true;
            if let Value::Object(object) = value {
                object.insert("truncated".to_string(), Value::Bool(true));
            }
            continue;
        }
        let over = measure(value) - TOOL_RESULT_MAX_BYTES;
        if !cut_one(value, cuts, over) {
            break;
        }
    }
    truncated
}

/// Remove one element, or `over` characters, from the first cut target that
/// still has content. `false` when every target is already empty. Targets
/// are JSON pointers, and one that names nothing in this result is skipped.
fn cut_one(value: &mut Value, cuts: &[&str], over: usize) -> bool {
    for pointer in cuts {
        match value.pointer_mut(pointer) {
            Some(Value::Array(items)) if !items.is_empty() => {
                items.pop();
                return true;
            }
            Some(Value::String(text)) if !text.is_empty() => {
                let keep = text.chars().count().saturating_sub(over.max(1));
                let mut cut = text.chars().take(keep).collect::<String>();
                if keep > 0 {
                    cut.push('…');
                }
                *text = cut;
                return true;
            }
            _ => {}
        }
    }
    false
}

/// Serialized length without the serialization.
fn measure(value: &Value) -> usize {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.0 += buffer.len();
            Ok(buffer.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    if serde_json::to_writer(&mut count, value).is_err() {
        return usize::MAX;
    }
    count.0
}

fn refusal(error: QueryError) -> String {
    match error {
        QueryError::Unavailable => "the corpus is not open right now".to_string(),
        QueryError::InvalidRequest => "the corpus refused the request".to_string(),
        QueryError::UnknownCursor | QueryError::Failed => {
            "the corpus could not be read".to_string()
        }
    }
}

fn store_refusal(error: StoreError) -> String {
    refusal(QueryError::from(error))
}

/// The one refusal a named meeting gets, whether it is absent or kept on this
/// Mac; see the module header.
fn no_meeting(session_id: MeetingSessionId) -> String {
    format!("no meeting {} in this corpus", session_id.uuid())
}

/// RFC 3339 in the host zone, to the second: the form every `when` in a
/// result takes, so a model comparing two of them compares like with like.
pub(super) fn when(utc_ms: i64) -> String {
    Local.timestamp_millis_opt(utc_ms).single().map_or_else(
        || utc_ms.to_string(),
        |time| time.to_rfc3339_opts(SecondsFormat::Secs, false),
    )
}

/// One row as every listing renders it, with the ledger headline beside a
/// meeting that has one.
#[derive(Serialize)]
struct ListedRow<'a> {
    kind: String,
    id: &'a str,
    title: &'a str,
    when: String,
    snippet: &'a str,
    link: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    ledger_headline: Option<&'a str>,
}

fn row_json<'a>(row: &'a QueryRow, ledger_headline: Option<&'a str>) -> ListedRow<'a> {
    ListedRow {
        kind: token(&row.kind),
        id: &row.id,
        title: &row.title,
        when: when(row.when_utc_ms),
        snippet: &row.snippet,
        link: &row.link,
        ledger_headline,
    }
}

/// The ledger headline of each meeting row, keyed by the row's id. A read
/// that fails costs the rows their headline and nothing else.
fn ledger_headlines(store: &MeetingStore, rows: &[QueryRow]) -> HashMap<String, String> {
    let ids = rows
        .iter()
        .filter(|row| row.kind == QueryRowKind::Meeting)
        .filter_map(|row| Uuid::parse_str(&row.id).ok())
        .map(MeetingSessionId::from_uuid)
        .collect::<Vec<_>>();
    match store.query_ledger_headlines(&ids) {
        Ok(headlines) => headlines
            .into_iter()
            .map(|(session_id, headline)| (session_id.uuid().to_string(), headline))
            .collect(),
        Err(error) => {
            log::warn!("Listing meetings without their ledger headlines: {error:?}");
            HashMap::new()
        }
    }
}

/// A listing of rows: what `search` and `recent` both return.
fn listing(store: &MeetingStore, rows: Vec<QueryRow>, more: bool) -> Outcome {
    let rows = without_excluded_series(store, rows);
    let headlines = ledger_headlines(store, &rows);
    let mut value = Map::new();
    value.insert(
        "rows".to_string(),
        json!(rows
            .iter()
            .map(|row| row_json(row, headlines.get(&row.id).map(String::as_str)))
            .collect::<Vec<_>>()),
    );
    value.insert("more".to_string(), Value::Bool(more));
    let mut outcome = Outcome::new(value, &["/rows"]);
    outcome.quoted.push(("rows", rows));
    outcome
}

fn search_result(store: &MeetingStore, rows: Vec<QueryRow>, more: bool) -> Outcome {
    listing(store, rows, more)
}

/// Meetings newer than `since_utc_ms`, newest first, from the same list the
/// Library shows.
fn recent_meetings(
    store: &MeetingStore,
    since_utc_ms: i64,
    limit: usize,
) -> Result<Outcome, String> {
    let mut rows = Vec::new();
    let mut cursor = None;
    let mut exhausted = false;
    for _ in 0..RECENT_MAX_PAGES {
        let page = store
            .list_sessions(cursor, RECENT_PAGE_ROWS, &MeetingListFilter::default())
            .map_err(store_refusal)?;
        exhausted = !page.has_more;
        let mut candidates = Vec::new();
        for summary in page.entries {
            if summary.created_at_utc_ms < since_utc_ms {
                exhausted = true;
                break;
            }
            cursor = Some(summary.created_at_utc_ms);
            candidates.push(summary_row(&summary));
        }
        rows.extend(without_excluded_series(store, candidates));
        if rows.len() > limit || exhausted {
            break;
        }
    }
    let more = rows.len() > limit || !exhausted;
    rows.truncate(limit);
    Ok(listing(store, rows, more))
}

/// A meeting list row as the plane would report it: the headline is the
/// snippet, since nothing was searched for.
fn summary_row(summary: &MeetingHistorySummary) -> QueryRow {
    let snippet = match &summary.headline {
        MeetingHistoryHeadline::Ledger { text } | MeetingHistoryHeadline::Summary { text } => {
            text.as_str()
        }
        MeetingHistoryHeadline::None | MeetingHistoryHeadline::Words { .. } => "",
    };
    QueryRow {
        kind: QueryRowKind::Meeting,
        id: summary.session_id.uuid().to_string(),
        title: bounded(&summary.title, MAX_TITLE_CHARS),
        snippet: bounded(snippet, MAX_SNIPPET_CHARS),
        when_utc_ms: summary.created_at_utc_ms,
        link: meeting_link(summary.session_id),
    }
}

/// Dictations already read newest first, one more than the page so the
/// listing can say whether the window holds more.
fn recent_dictations(store: &MeetingStore, entries: Vec<HistoryEntry>, limit: usize) -> Outcome {
    let more = entries.len() > limit;
    let rows = entries.iter().take(limit).map(dictation_row).collect();
    listing(store, rows, more)
}

/// The meeting as its review screen reads it, for the model: what was said
/// about it, what it left open, and where it landed.
fn meeting_result(store: &MeetingStore, session_id: MeetingSessionId) -> Result<Outcome, String> {
    let snapshot = review(store, session_id)?;
    let artifacts = current_artifacts(&snapshot);
    let row = allowed_meeting(store, &snapshot)?;
    let facts = store.meeting_calendar_facts(session_id).ok().flatten();
    let mut attendees = store
        .meeting_people_context(session_id)
        .map(|context| {
            context
                .rows
                .into_iter()
                .map(|person| {
                    json!({
                        "name": person.display_name,
                        "person_id": person.person_id.uuid().to_string(),
                        "link": person_link(person.person_id),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(event) = &facts {
        for attendee in event.attendees.iter().filter(|attendee| !attendee.is_self) {
            let known = attendees
                .iter()
                .any(|known| known["name"].as_str() == Some(attendee.name.as_str()));
            if !known {
                attendees.push(json!({ "name": attendee.name }));
            }
        }
    }
    let series = facts
        .as_ref()
        .filter(|event| !event.series_key.trim().is_empty())
        .map(|event| json!({ "key": event.series_key, "title": event.title }));
    let loops = store
        .meeting_loops(session_id)
        .map_err(store_refusal)?
        .rows
        .into_iter()
        .filter(MeetingLoopRow::is_open)
        .collect::<Vec<_>>();
    let loop_rows = without_excluded_series(
        store,
        loops
            .iter()
            .map(|row| loop_row(row, &snapshot.session.title, row_when(&snapshot)))
            .collect(),
    );
    let open_loops = loops
        .iter()
        .filter(|row| loop_rows.iter().any(|kept| kept.id == row.loop_id.as_str()))
        .map(|row| {
            json!({
                "loop_id": row.loop_id.as_str(),
                "kind": token(&row.kind),
                "text": row.text,
                "owner": row.owner_display_name.clone().or_else(|| row.owner_text.clone()),
                "direction": token(&row.direction),
                "link": loop_link(&row.loop_id),
            })
        })
        .collect::<Vec<_>>();
    let talk_share = store
        .analytics_segments(session_id)
        .map(|segments| {
            talk_metrics(&segments)
                .speakers
                .into_iter()
                .map(|share| {
                    let speaker = snapshot
                        .speakers
                        .iter()
                        .find(|speaker| speaker.speaker_id == share.speaker_id)
                        .map(|speaker| speaker.display_name.clone())
                        .unwrap_or_default();
                    json!({
                        "speaker": speaker,
                        "percent": (share.share_permille + 5) / 10,
                        "turns": share.turn_count,
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let cited = |texts: &[crate::meeting::types::CitedArtifactText]| {
        texts
            .iter()
            .map(|text| text.text.trim())
            .filter(|text| !text.is_empty())
            .map(|text| Value::String(text.to_string()))
            .collect::<Vec<_>>()
    };
    let mut value = Map::new();
    value.insert("id".to_string(), json!(session_id.uuid().to_string()));
    value.insert("title".to_string(), json!(snapshot.session.title));
    value.insert("when".to_string(), json!(when(row.when_utc_ms)));
    value.insert(
        "duration_minutes".to_string(),
        json!(snapshot
            .session
            .elapsed_offset_ns
            .map_or(0, |elapsed| elapsed / 60_000_000_000)),
    );
    value.insert("speakers".to_string(), json!(speaker_names(&snapshot)));
    value.insert("attendees".to_string(), Value::Array(attendees));
    value.insert("series".to_string(), series.unwrap_or(Value::Null));
    value.insert(
        "summary".to_string(),
        artifacts
            .map(|content| content.summary.text.trim())
            .filter(|summary| !summary.is_empty())
            .map_or(Value::Null, |summary| json!(summary)),
    );
    value.insert(
        "notes".to_string(),
        snapshot
            .notes
            .iter()
            .map(|note| note.body.trim())
            .filter(|body| !body.is_empty())
            .map(|body| json!(body))
            .collect(),
    );
    value.insert(
        "decisions".to_string(),
        Value::Array(artifacts.map_or_else(Vec::new, |content| cited(&content.decisions))),
    );
    value.insert(
        "action_items".to_string(),
        artifacts
            .map(|content| {
                content
                    .action_items
                    .iter()
                    .map(|item| {
                        json!({
                            "text": item.text.text.trim(),
                            "owner": item.owner_text,
                            "due": item.due_text,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
            .into(),
    );
    value.insert(
        "key_questions".to_string(),
        Value::Array(artifacts.map_or_else(Vec::new, |content| cited(&content.key_questions))),
    );
    value.insert(
        "risks".to_string(),
        Value::Array(artifacts.map_or_else(Vec::new, |content| cited(&content.risks))),
    );
    value.insert("open_loops".to_string(), Value::Array(open_loops));
    value.insert("talk_share".to_string(), Value::Array(talk_share));
    if let Some(ledger) = artifacts.and_then(|content| content.ledger.as_ref()) {
        value.insert("ledger".to_string(), json!(ledger_json(ledger)));
    }
    value.insert("link".to_string(), json!(row.link));
    let mut outcome = Outcome::new(value, MEETING_CUTS);
    outcome.fixed.push(row);
    outcome.quoted.push(("open_loops", loop_rows));
    Ok(outcome)
}

/// Notes go first, generated prose second, and the ledger last: its rows are
/// what answers "where did we land", which is what a meeting is opened for.
const MEETING_CUTS: &[&str] = &[
    "/notes",
    "/summary",
    "/risks",
    "/key_questions",
    "/decisions",
    "/action_items",
    "/talk_share",
    "/open_loops",
    "/attendees",
    "/ledger/caveats",
    "/ledger/stances",
    "/ledger/open_loops",
    "/ledger/commitments",
    "/ledger/threads",
];

/// The ledger with the names its own types use, receipts folded into the row
/// they vouch for: `quote`, `speaker` and `at_ms` beside each thread and
/// commitment rather than a nested object per row.
#[derive(Serialize)]
struct LedgerJson<'a> {
    headline: &'a str,
    threads: Vec<LedgerThreadJson<'a>>,
    open_loops: Vec<LedgerOpenLoopJson<'a>>,
    commitments: Vec<LedgerCommitmentJson<'a>>,
    stances: Vec<LedgerStanceJson<'a>>,
    caveats: &'a [String],
    receipts: LedgerReceiptState,
}

#[derive(Serialize)]
struct LedgerThreadJson<'a> {
    topic: &'a str,
    state: LedgerThreadState,
    substantive: bool,
    owner: Option<&'a str>,
    quote: &'a str,
    speaker: Option<&'a str>,
    at_ms: u64,
}

#[derive(Serialize)]
struct LedgerOpenLoopJson<'a> {
    question: &'a str,
    instead: &'a str,
    at_ms: u64,
}

#[derive(Serialize)]
struct LedgerCommitmentJson<'a> {
    who: &'a str,
    what: &'a str,
    firmness: LedgerFirmness,
    quote: &'a str,
    speaker: Option<&'a str>,
    at_ms: u64,
}

#[derive(Serialize)]
struct LedgerStanceJson<'a> {
    from: &'a str,
    to: Option<&'a str>,
    what: &'a str,
    note: Option<&'a str>,
    at_ms: u64,
}

fn ledger_json(ledger: &MeetingLedger) -> LedgerJson<'_> {
    LedgerJson {
        headline: &ledger.headline,
        threads: ledger
            .threads
            .iter()
            .map(|thread| LedgerThreadJson {
                topic: &thread.topic,
                state: thread.state,
                substantive: thread.substantive,
                owner: thread.owner.as_deref(),
                quote: &thread.receipt.quote,
                speaker: thread.receipt.speaker.as_deref(),
                at_ms: thread.receipt.t_ms,
            })
            .collect(),
        open_loops: ledger
            .open_loops
            .iter()
            .map(|open_loop| LedgerOpenLoopJson {
                question: &open_loop.question,
                instead: &open_loop.instead,
                at_ms: open_loop.at_ms,
            })
            .collect(),
        commitments: ledger
            .commitments
            .iter()
            .map(|commitment| LedgerCommitmentJson {
                who: &commitment.who,
                what: &commitment.what,
                firmness: commitment.firmness,
                quote: &commitment.receipt.quote,
                speaker: commitment.receipt.speaker.as_deref(),
                at_ms: commitment.receipt.t_ms,
            })
            .collect(),
        stances: ledger
            .stances
            .iter()
            .map(|stance| LedgerStanceJson {
                from: &stance.from,
                to: stance.to.as_deref(),
                what: &stance.what,
                note: stance.note.as_deref(),
                at_ms: stance.at_ms,
            })
            .collect(),
        caveats: &ledger.caveats,
        receipts: ledger.receipts,
    }
}

/// One page of a meeting's transcript, human edits applied and empty
/// segments skipped, indexed the way the whole transcript is so a second page
/// continues where this one stopped.
fn transcript_result(
    store: &MeetingStore,
    session_id: MeetingSessionId,
    offset: usize,
    limit: usize,
) -> Result<Outcome, String> {
    let snapshot = review(store, session_id)?;
    let row = allowed_meeting(store, &snapshot)?;
    let lines = snapshot
        .transcript
        .iter()
        .filter(|segment| !segment.removed)
        .filter_map(|segment| transcript_line(segment, &snapshot))
        .collect::<Vec<_>>();
    let total = lines.len();
    let segments = lines
        .iter()
        .enumerate()
        .skip(offset)
        .take(limit)
        .map(|(index, line)| {
            json!({
                "index": index,
                "speaker": line.speaker,
                "at_ms": line.start_ms,
                "text": line.text,
            })
        })
        .collect::<Vec<_>>();
    let mut value = json!({
        "meeting_id": session_id.uuid().to_string(),
        "title": snapshot.session.title,
        "segments": segments,
        "total": total,
        // Measured with the widest value it can take, then set below once
        // the page is cut: `next_offset` must never grow the result past the
        // ceiling the cut just met.
        "next_offset": total,
        "link": row.link,
    });
    fit(&mut value, &["/segments"]);
    let kept = value["segments"].as_array().map_or(0, Vec::len);
    if let Value::Object(object) = &mut value {
        if offset + kept < total {
            object.insert("next_offset".to_string(), json!(offset + kept));
        } else {
            object.remove("next_offset");
        }
    }
    let Value::Object(object) = value else {
        unreachable!("a JSON object literal");
    };
    let mut outcome = Outcome::new(object, &[]);
    outcome.fixed.push(row);
    Ok(outcome)
}

/// One person: who they are to this corpus, and what is open with them.
fn person_result(store: &MeetingStore, person_id: PersonId) -> Result<Outcome, String> {
    let detail = store
        .person_detail(person_id)
        .map_err(|error| match error {
            StoreError::NotFound => format!("no person {} in this corpus", person_id.uuid()),
            error => store_refusal(error),
        })?
        .detail;
    let meetings = without_excluded_series(
        store,
        detail
            .links
            .iter()
            .filter(|link| link.confidence == PersonLinkConfidence::Confirmed)
            .map(|link| QueryRow {
                kind: QueryRowKind::Meeting,
                id: link.meeting.id.uuid().to_string(),
                title: bounded(&link.meeting.title, MAX_TITLE_CHARS),
                snippet: bounded(
                    link.meeting.headline.as_deref().unwrap_or_default(),
                    MAX_SNIPPET_CHARS,
                ),
                when_utc_ms: link.meeting.at_utc_ms,
                link: meeting_link(link.meeting.id),
            })
            .collect(),
    );
    let loops = without_excluded_series(
        store,
        detail
            .open_loops
            .iter()
            .filter(|open_loop| open_loop.status.is_open())
            .map(|open_loop| QueryRow {
                kind: QueryRowKind::Loop,
                id: open_loop.loop_id.as_str().to_string(),
                title: bounded(&open_loop.text, MAX_TITLE_CHARS),
                snippet: bounded(&open_loop.title, MAX_SNIPPET_CHARS),
                when_utc_ms: open_loop.at_utc_ms,
                link: loop_link(&open_loop.loop_id),
            })
            .collect(),
    );
    let last_met = meetings.iter().map(|row| row.when_utc_ms).max();
    let summary = detail
        .person
        .summary
        .as_ref()
        .map(|summary| summary.text.trim());
    // The person's row quotes what the plane would quote: the relationship
    // paragraph when there is one, the newest meeting's headline otherwise.
    let snippet = match (summary, meetings.first()) {
        (Some(summary), _) => summary.to_string(),
        (None, Some(row)) => row.snippet.clone(),
        (None, None) => detail.person.aliases.join(", "),
    };
    let person = QueryRow {
        kind: QueryRowKind::Person,
        id: person_id.uuid().to_string(),
        title: bounded(&detail.person.display_name, MAX_TITLE_CHARS),
        snippet: bounded(&snippet, MAX_SNIPPET_CHARS),
        when_utc_ms: last_met.unwrap_or(detail.person.created_at_utc_ms),
        link: person_link(person_id),
    };
    let recent = meetings
        .iter()
        .take(PERSON_MEETINGS)
        .cloned()
        .collect::<Vec<_>>();
    let value = json!({
        "id": person.id,
        "name": detail.person.display_name,
        "aliases": detail.person.aliases,
        "organization": detail.person.organization,
        "meetings": meetings.len(),
        "last_met": last_met.map(when),
        "recent_meetings": recent.iter().map(|row| json!({
            "id": row.id, "title": row.title, "when": when(row.when_utc_ms),
            "headline": row.snippet, "link": row.link,
        })).collect::<Vec<_>>(),
        "open_loops": loops.iter().map(|row| json!({
            "loop_id": row.id, "text": row.title, "meeting": row.snippet,
            "when": when(row.when_utc_ms), "link": row.link,
        })).collect::<Vec<_>>(),
        "summary": summary,
        "link": person.link,
    });
    let Value::Object(object) = value else {
        unreachable!("a JSON object literal");
    };
    let mut outcome = Outcome::new(object, &["/open_loops", "/recent_meetings", "/summary"]);
    outcome.fixed.push(person);
    outcome.quoted.push(("recent_meetings", recent));
    outcome.quoted.push(("open_loops", loops));
    Ok(outcome)
}

/// Actionable rows across the corpus, newest meeting first, as the external
/// plane walks them.
fn loops_result(
    store: &MeetingStore,
    status: LoopFilter,
    person_id: Option<PersonId>,
    limit: usize,
) -> Result<Outcome, String> {
    let mut candidates = Vec::new();
    let mut facts = HashMap::new();
    let mut scanned = 0usize;
    let mut more = false;
    'corpus: for meeting in store.corpus_loops().map_err(store_refusal)?.meetings {
        for row in meeting.rows {
            scanned += 1;
            if scanned > LOOP_SCAN_DEPTH {
                more = true;
                break 'corpus;
            }
            let wanted = match status {
                LoopFilter::Open => row.status.is_open(),
                LoopFilter::Done => matches!(row.status, MeetingLoopStatus::Done),
                LoopFilter::All => true,
            };
            if !wanted || person_id.is_some_and(|person| row.owner_person_id != Some(person)) {
                continue;
            }
            candidates.push(loop_row(&row, &meeting.title, meeting.at_utc_ms));
            facts.insert(
                row.loop_id.as_str().to_string(),
                json!({
                    "loop_id": row.loop_id.as_str(),
                    "kind": token(&row.kind),
                    "text": row.text,
                    "owner": row.owner_display_name.clone().or(row.owner_text),
                    "status": token(&row.status),
                    "direction": token(&row.direction),
                    "meeting": { "id": meeting.session_id.uuid().to_string(), "title": meeting.title },
                    "when": when(meeting.at_utc_ms),
                    "link": loop_link(&row.loop_id),
                }),
            );
        }
    }
    let mut rows = without_excluded_series(store, candidates);
    more |= rows.len() > limit;
    rows.truncate(limit);
    let mut value = Map::new();
    value.insert(
        "rows".to_string(),
        rows.iter()
            .filter_map(|row| facts.remove(&row.id))
            .collect(),
    );
    value.insert("more".to_string(), Value::Bool(more));
    let mut outcome = Outcome::new(value, &["/rows"]);
    outcome.quoted.push(("rows", rows));
    Ok(outcome)
}

fn loop_row(row: &MeetingLoopRow, meeting_title: &str, when_utc_ms: i64) -> QueryRow {
    QueryRow {
        kind: QueryRowKind::Loop,
        id: row.loop_id.as_str().to_string(),
        title: bounded(&row.text, MAX_TITLE_CHARS),
        snippet: bounded(meeting_title, MAX_SNIPPET_CHARS),
        when_utc_ms,
        link: loop_link(&row.loop_id),
    }
}

/// The calendar ahead, less the series the operator kept on this Mac.
///
/// Rows that are calendar events rather than meetings have no `sona://`
/// address, so the result quotes no source; `event_key` is what starts one.
fn upcoming_result(
    store: &MeetingStore,
    access: CalendarAccess,
    rows: Vec<MeetingUpcomingRow>,
) -> Result<Outcome, String> {
    let rows = allowed_upcoming(store, rows).map_err(store_refusal)?;
    let value = json!({
        "calendar_access": token(&access),
        "rows": rows.iter().map(|row| json!({
            "title": row.title,
            "start": when(row.start_utc_ms),
            "end": when(row.end_utc_ms),
            "attendees": row.attendees.iter()
                .filter(|attendee| !attendee.is_self)
                .map(|attendee| attendee.name.as_str())
                .collect::<Vec<_>>(),
            "attendee_count": row.attendee_count,
            "series": row.series.as_ref().map(|series| json!({
                "key": series.series_key,
                "always_record": series.always_record,
            })),
            "calendar": row.calendar_name,
            "join_url": row.join_url,
            "event_key": row.event_key,
        })).collect::<Vec<_>>(),
    });
    let Value::Object(object) = value else {
        unreachable!("a JSON object literal");
    };
    Ok(Outcome::new(object, &["/rows"]))
}

/// Drop the occurrences of a series the operator kept off the server. D14 at
/// the calendar: an upcoming row is the series' title and its attendees, and a
/// preference that cannot be read counts as kept, the way `pack.rs` leans.
pub(super) fn allowed_upcoming(
    store: &MeetingStore,
    rows: Vec<MeetingUpcomingRow>,
) -> Result<Vec<MeetingUpcomingRow>, StoreError> {
    let keys = rows
        .iter()
        .filter_map(|row| row.series.as_ref())
        .map(|series| series.series_key.clone())
        .collect::<Vec<_>>();
    let preferences = store.series_preferences_many(&keys)?;
    Ok(rows
        .into_iter()
        .filter(|row| match &row.series {
            None => true,
            Some(series) => preferences
                .get(&series.series_key)
                .is_some_and(|preference| !preference.remote_intelligence_opt_out),
        })
        .collect())
}

/// One dictation in full. The history keeps content-free provenance beside
/// the text — the mode, the source, the duration — and not the application
/// the words landed in, so that is not here.
fn dictation_result(entry: &HistoryEntry, receipts: &[HistoryRunReceipt]) -> Outcome {
    let row = dictation_row(entry);
    let raw = entry.transcription_text.trim();
    let text = entry
        .post_processed_text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(raw);
    let receipt = receipts.last();
    let value = json!({
        "id": entry.id,
        "when": when(row.when_utc_ms),
        "title": (!entry.title.trim().is_empty()).then(|| entry.title.trim()),
        "mode": receipt.map(|receipt| receipt.mode.mode_id.as_str()),
        "source": receipt.and_then(|receipt| receipt.source_kind).map(|kind| token(&kind)),
        "duration_ms": receipt.and_then(|receipt| receipt.duration_ms),
        "text": text,
        "raw_text": (raw != text).then_some(raw),
        "link": row.link,
    });
    let Value::Object(object) = value else {
        unreachable!("a JSON object literal");
    };
    let mut outcome = Outcome::new(object, &["/raw_text", "/text"]);
    outcome.fixed.push(row);
    outcome
}

/// The words a period of dictation used most.
fn word_stats_result(entries: &[HistoryEntry], days: u32, limit: usize) -> Outcome {
    let (total, top) = top_words(entries, limit);
    let mut value = json!({
        "days": days,
        "entries": entries.len(),
        "total_words": total,
        "top": top.iter().map(|(word, count)| json!({ "word": word, "count": count })).collect::<Vec<_>>(),
    });
    if entries.len() >= WORD_STATS_MAX_ENTRIES {
        value["capped"] = Value::Bool(true);
    }
    let Value::Object(object) = value else {
        unreachable!("a JSON object literal");
    };
    Outcome::new(object, &["/top"])
}

/// Every alphabetic token of two or more letters across the delivered text of
/// `entries`, lower-cased: the total, and the `limit` most frequent that are
/// not stopwords, most frequent first and alphabetical inside a tie. One
/// rule for the tool and the corpus card, so the two never disagree about
/// what a word is.
pub(super) fn top_words(entries: &[HistoryEntry], limit: usize) -> (u64, Vec<(String, u64)>) {
    let mut counts: HashMap<String, u64> = HashMap::new();
    let mut total = 0u64;
    for entry in entries {
        let text = entry
            .post_processed_text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or(entry.transcription_text.as_str());
        for word in text
            .to_lowercase()
            .split(|character: char| !character.is_alphabetic())
            .filter(|word| word.chars().nth(1).is_some())
        {
            total += 1;
            if STOPWORDS.binary_search(&word).is_err() {
                *counts.entry(word.to_string()).or_default() += 1;
            }
        }
    }
    let mut top = counts.into_iter().collect::<Vec<_>>();
    top.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    top.truncate(limit);
    (total, top)
}

/// One row per local day, newest first, from the two dashboard projections:
/// dictations and words from the history's, meetings and their minutes from
/// the store's. Both are dense over the same calendar, so a day with nothing
/// in it is a row of zeros rather than a gap.
fn activity_result(
    dictations: &[HistoryTrendPoint],
    meetings: &[MeetingTrendPoint],
    days: u32,
) -> Outcome {
    let meetings = meetings
        .iter()
        .map(|point| (point.local_date.as_str(), point))
        .collect::<HashMap<_, _>>();
    let rows = dictations
        .iter()
        .rev()
        .take(usize::try_from(days).unwrap_or(usize::MAX))
        .map(|point| {
            let meeting = meetings.get(point.local_date.as_str());
            json!({
                "date": point.local_date,
                "dictations": point.recordings,
                "words": point.words,
                "meetings": meeting.map_or(0, |meeting| meeting.meetings),
                "meeting_minutes": meeting.map_or(0, |meeting| meeting.verified_captured_duration_ms / 60_000),
            })
        })
        .collect::<Vec<_>>();
    let mut value = Map::new();
    value.insert("days".to_string(), json!(days));
    value.insert("rows".to_string(), Value::Array(rows));
    Outcome::new(value, &["/rows"])
}

/* -------------------------------------------------------------- meetings */

fn review(
    store: &MeetingStore,
    session_id: MeetingSessionId,
) -> Result<MeetingReviewSnapshot, String> {
    store
        .review_snapshot(session_id)
        .map_err(|error| match error {
            StoreError::NotFound => no_meeting(session_id),
            error => store_refusal(error),
        })
}

/// The meeting's own row, if its series may leave this Mac.
fn allowed_meeting(
    store: &MeetingStore,
    snapshot: &MeetingReviewSnapshot,
) -> Result<QueryRow, String> {
    let session_id = snapshot.session.session_id;
    let headline = current_artifacts(snapshot)
        .and_then(|content| content.headline())
        .unwrap_or_default();
    let row = QueryRow {
        kind: QueryRowKind::Meeting,
        id: session_id.uuid().to_string(),
        title: bounded(&snapshot.session.title, MAX_TITLE_CHARS),
        snippet: bounded(headline, MAX_SNIPPET_CHARS),
        when_utc_ms: row_when(snapshot),
        link: meeting_link(session_id),
    };
    without_excluded_series(store, vec![row])
        .pop()
        .ok_or_else(|| no_meeting(session_id))
}

fn row_when(snapshot: &MeetingReviewSnapshot) -> i64 {
    snapshot.session.started_at_utc_ms.unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managers::history::HistoryTrendSourceTotals;
    use crate::meeting::detection::calendar::CalendarOccurrence;
    use crate::meeting::detection::machine::{
        CalendarAttendee, CalendarEventSummary, ParticipationStatus,
    };
    use crate::meeting::loop_types::{MeetingLoopId, MeetingLoopKind};
    use crate::meeting::series_types::MeetingSeriesRemoteOptOutSetRequest;
    use crate::meeting::store::workflow_core_tests::{
        current_artifact, event, inputs, link, person, reviewable_meeting, store, transcript,
        transcript_segments,
    };
    use crate::meeting::types::{ManualNote, ManualNoteId, MeetingOperationId};
    use crate::meeting::upcoming::upcoming_rows;
    use crate::meeting::workflow_types::WorkflowEventKind;
    use std::collections::HashMap;

    /// 2026-08-14 09:32 UTC.
    const WHEN: i64 = 1_786_699_920_000;
    const DAY: i64 = 86_400_000;

    fn call(tool: &str, args: impl serde::Serialize) -> ToolCall {
        ToolCall {
            id: "c1".to_string(),
            tool: tool.to_string(),
            args: serde_json::to_value(args).expect("test arguments serialize"),
        }
    }

    fn parsed<T: serde::de::DeserializeOwned>(value: &str) -> T {
        serde_json::from_str(value).expect("test result is JSON")
    }

    fn refused(tool: &str, args: impl serde::Serialize) -> String {
        match parse(&call(tool, args)) {
            Ok(_) => panic!("{tool} was accepted"),
            Err(error) => error,
        }
    }

    fn result_json(outcome: Outcome) -> (Value, Vec<QueryRow>) {
        let result = finish(&call("t", Value::Null), outcome);
        assert!(result.ok, "{}", result.result);
        (parsed(&result.result), result.sources)
    }

    fn entry(id: i64, timestamp: i64, text: &str) -> HistoryEntry {
        HistoryEntry {
            id,
            file_name: format!("{id}.wav"),
            timestamp,
            saved: false,
            title: String::new(),
            transcription_text: text.to_string(),
            post_processed_text: None,
            post_process_requested: false,
            parent_id: None,
            match_kind: None,
        }
    }

    /// A series key is only ever written as a calendar fact, so the fixture
    /// writes one the way an accepted detection does.
    fn in_series(
        store: &MeetingStore,
        session_id: MeetingSessionId,
        series_key: &str,
        title: &str,
    ) {
        store
            .remember_calendar_facts(
                session_id,
                &CalendarEventSummary {
                    event_key: format!("{series_key}#{title}"),
                    series_key: series_key.to_string(),
                    title: title.to_string(),
                    attendee_count: 2,
                    start_utc_ms: WHEN,
                    end_utc_ms: WHEN + 1_800_000,
                    attendees: vec![CalendarAttendee {
                        name: "Dana Reyes".to_string(),
                        status: ParticipationStatus::Accepted,
                        email: Some("dana@example.com".to_string()),
                        is_self: false,
                    }],
                    notes: None,
                    calendar_name: None,
                    url: None,
                },
            )
            .unwrap();
    }

    fn exclude(store: &MeetingStore, series_key: &str) {
        store
            .set_series_remote_opt_out(
                &MeetingSeriesRemoteOptOutSetRequest {
                    operation_id: MeetingOperationId::new(),
                    series_key: series_key.to_string(),
                    remote_intelligence_opt_out: true,
                    expected_revision: store.series_revision().unwrap(),
                },
                WHEN,
            )
            .unwrap();
    }

    /// The current artifact revision with a ledger: one open thread owned by
    /// Dana, one commitment, one decision, and a summary.
    fn artifact(store: &MeetingStore, session_id: MeetingSessionId, summary: &str, headline: &str) {
        let content = serde_json::json!({
            "summary": {"text": summary, "citations": []},
            "outline": [],
            "decisions": [{"text": "Ship the tier page Friday.", "citations": []}],
            "action_items": [{"text": {"text": "Send the deck", "citations": []}, "owner_text": "Dana", "due_text": "Friday"}],
            "key_questions": [],
            "risks": [],
            "follow_up_draft": {"text": "", "citations": []},
            "ledger": {
                "headline": headline,
                "threads": [{
                    "topic": "Enterprise tier pricing",
                    "state": "open",
                    "substantive": true,
                    "receipt": {"quote": "which tier does the trial convert into", "speaker": "Dana", "t_ms": 12000, "citations": []},
                    "owner": "Dana Reyes"
                }],
                "open_loops": [{"question": "Which tier?", "instead": "Moved to the roadmap.", "at_ms": 12000, "citations": []}],
                "commitments": [{
                    "who": "Dana Reyes",
                    "what": "Send the tier comparison",
                    "firmness": "firm",
                    "receipt": {"quote": "I will send it Friday", "speaker": "Dana", "t_ms": 30000, "citations": []}
                }],
                "stances": [],
                "caveats": ["Audio dropped for two minutes."],
                "receipts": {"status": "verified"}
            }
        });
        current_artifact(store, session_id, &content, WHEN);
    }

    /// A note the reader typed, through the mutation that rebuilds the search
    /// documents. Fenced on the session's current revision, so a test can
    /// type several; a stale fence is a rejected receipt, not an error.
    fn note(store: &MeetingStore, session_id: MeetingSessionId, body: &str) {
        let revision = store.session_snapshot(session_id).unwrap().revision;
        let receipt = store
            .create_note(
                MeetingOperationId::new(),
                WHEN,
                &ManualNote {
                    note_id: ManualNoteId::new(),
                    session_id,
                    start_offset_ns: None,
                    end_offset_ns: None,
                    body: body.to_string(),
                    revision: 0,
                    created_at_utc_ms: WHEN,
                    updated_at_utc_ms: WHEN,
                },
                revision,
            )
            .unwrap();
        assert_eq!(
            receipt.result,
            crate::meeting::types::OperationResult::Committed
        );
    }

    /// The continuity run that makes a meeting's loops reachable corpus-wide.
    fn finalize(store: &MeetingStore, session_id: MeetingSessionId) {
        store
            .record_and_run_workflow_event(
                event(
                    WorkflowEventKind::MeetingFinalized,
                    serde_json::json!({
                        "session_id": session_id.uuid().to_string(),
                        "known_vocabulary": []
                    }),
                    &format!("tools-finalized-{}", session_id.uuid()),
                ),
                &inputs(),
            )
            .unwrap();
    }

    fn meeting_row(session_id: MeetingSessionId, title: &str, when: i64) -> QueryRow {
        QueryRow {
            kind: QueryRowKind::Meeting,
            id: session_id.uuid().to_string(),
            title: title.to_string(),
            snippet: "what matched".to_string(),
            when_utc_ms: when,
            link: meeting_link(session_id),
        }
    }

    /// Two meetings in two series, one of them kept on this Mac, both with a
    /// transcript, a note and a ledger.
    struct Corpus {
        _directory: tempfile::TempDir,
        store: Arc<MeetingStore>,
        excluded: MeetingSessionId,
        allowed: MeetingSessionId,
    }

    fn corpus() -> Corpus {
        let (directory, store) = store();
        let excluded = reviewable_meeting(&store, "Pricing sync", WHEN);
        let allowed = reviewable_meeting(&store, "Design review", WHEN - DAY);
        for (session_id, series, line) in [
            (
                excluded,
                "weekly-pricing",
                "The enterprise tier lands at forty thousand.",
            ),
            (
                allowed,
                "weekly-design",
                "The empty state needs a second pass.",
            ),
        ] {
            transcript(&store, session_id, line);
            in_series(&store, session_id, series, "Weekly");
            note(&store, session_id, "Typed during the call.");
            artifact(
                &store,
                session_id,
                "Pricing stayed open.",
                "Dana's tier question stayed open.",
            );
        }
        exclude(&store, "weekly-pricing");
        Corpus {
            _directory: directory,
            store,
            excluded,
            allowed,
        }
    }

    #[test]
    fn the_catalogue_is_the_relays_table() {
        let names = TOOL_ARGS.iter().map(|(name, _)| *name).collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "search",
                "recent",
                "meeting",
                "transcript",
                "person",
                "loops",
                "upcoming",
                "dictation",
                "word_stats",
                "activity"
            ]
        );
        assert_eq!(TOOL_CATALOGUE_VERSION, "sona-tools/1");
        assert!(
            STOPWORDS.windows(2).all(|pair| pair[0] < pair[1]),
            "the stopword list is searched by bisection, so it has to be sorted"
        );
    }

    #[test]
    fn an_unknown_tool_and_a_bad_argument_are_refused_in_one_line() {
        assert_eq!(refused("summarize", json!({})), "unknown tool summarize");
        assert_eq!(
            refused("search", json!({"query": "deck", "limit": 26})),
            "limit must be an integer from 1 to 25"
        );
        assert_eq!(
            refused("search", json!({"query": "deck", "limit": "12"})),
            "limit must be an integer from 1 to 25"
        );
        assert_eq!(refused("search", json!({})), "query is required");
        assert_eq!(
            refused("search", json!({"query": "x".repeat(257)})),
            "query must be a string of 1 to 256 characters"
        );
        assert_eq!(
            refused("recent", json!({"scope": "people"})),
            "scope must be one of meetings, dictations"
        );
        assert_eq!(
            refused("meeting", json!({"session_id": "not-a-uuid"})),
            "session_id must be a uuid"
        );
        assert_eq!(
            refused("word_stats", json!({"days": 0})),
            "days must be an integer from 1 to 3650"
        );
        assert_eq!(
            refused("activity", json!({"days": 91})),
            "days must be an integer from 1 to 90"
        );
        assert_eq!(
            refused("upcoming", json!({"days": 7, "limit": 3})),
            "upcoming takes no argument limit"
        );
        assert_eq!(refused("loops", json!([])), "args must be an object");
        assert_eq!(
            refused("dictation", json!({"entry_id": -4})),
            "entry_id must be a positive integer"
        );

        let failed = failure(&call("summarize", json!({})), "unknown tool summarize");
        assert!(!failed.ok);
        assert_eq!(failed.result, "unknown tool summarize");
        assert!(failed.sources.is_empty());
    }

    #[test]
    fn defaults_fill_what_a_call_leaves_out() {
        match parse(&call("search", json!({"query": "deck"}))).unwrap() {
            Request::Search {
                query,
                scope,
                limit,
            } => {
                assert_eq!(query, "deck");
                assert_eq!(scope, QueryScope::All);
                assert_eq!(limit, 12);
            }
            _ => panic!("a search"),
        }
        match parse(&call(
            "transcript",
            json!({"session_id": Uuid::nil().to_string()}),
        ))
        .unwrap()
        {
            Request::Transcript { offset, limit, .. } => {
                assert_eq!((offset, limit), (0, 80));
            }
            _ => panic!("a transcript"),
        }
        match parse(&call("dictation", json!({"entry_id": "42"}))).unwrap() {
            Request::Dictation { entry_id } => assert_eq!(entry_id, 42),
            _ => panic!("a dictation"),
        }
        assert!(matches!(
            parse(&call("loops", Value::Null)).unwrap(),
            Request::Loops {
                status: LoopFilter::Open,
                person_id: None,
                limit: 20
            }
        ));
    }

    #[test]
    fn search_rows_carry_their_noun_time_link_and_ledger_headline() {
        let corpus = corpus();
        let (value, sources) = result_json(search_result(
            &corpus.store,
            vec![
                meeting_row(corpus.excluded, "Pricing sync", WHEN),
                meeting_row(corpus.allowed, "Design review", WHEN - DAY),
                QueryRow {
                    kind: QueryRowKind::Dictation,
                    id: "7".to_string(),
                    title: "Send the deck".to_string(),
                    snippet: "Send the deck.".to_string(),
                    when_utc_ms: WHEN,
                    link: super::super::dictation_link(7),
                },
            ],
            true,
        ));

        let rows = value["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "the excluded series is gone: {value}");
        assert_eq!(rows[0]["kind"], "meeting");
        assert_eq!(rows[0]["id"], corpus.allowed.uuid().to_string());
        assert_eq!(rows[0]["title"], "Design review");
        assert_eq!(rows[0]["when"], when(WHEN - DAY));
        assert_eq!(rows[0]["snippet"], "what matched");
        assert_eq!(rows[0]["link"], meeting_link(corpus.allowed));
        assert_eq!(
            rows[0]["ledger_headline"],
            "Dana's tier question stayed open."
        );
        assert_eq!(rows[1]["kind"], "dictation");
        assert!(rows[1].get("ledger_headline").is_none());
        assert_eq!(value["more"], true);
        assert_eq!(
            sources
                .iter()
                .map(|row| row.link.as_str())
                .collect::<Vec<_>>(),
            [meeting_link(corpus.allowed).as_str(), "sona://dictation/7"]
        );
        assert!(!value.to_string().contains("enterprise"));
    }

    #[test]
    fn recent_meetings_are_newest_first_inside_the_window_and_never_excluded() {
        let corpus = corpus();
        reviewable_meeting(&corpus.store, "Kickoff", WHEN - 40 * DAY);

        let (value, sources) =
            result_json(recent_meetings(&corpus.store, WHEN - 30 * DAY, 10).unwrap());

        let rows = value["rows"].as_array().unwrap();
        assert_eq!(
            rows.iter()
                .map(|row| row["title"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["Design review"],
            "the excluded series and the meeting outside the window are gone: {value}"
        );
        assert_eq!(
            rows[0]["ledger_headline"],
            "Dana's tier question stayed open."
        );
        assert_eq!(rows[0]["snippet"], "Dana's tier question stayed open.");
        assert_eq!(value["more"], false);
        assert_eq!(sources.len(), 1);

        let (value, _) = result_json(recent_meetings(&corpus.store, WHEN - 60 * DAY, 1).unwrap());
        let rows = value["rows"].as_array().unwrap();
        assert_eq!(rows[0]["title"], "Design review");
        assert_eq!(
            value["more"], true,
            "the kickoff is behind the page: {value}"
        );
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn recent_dictations_are_the_plane_rows_with_a_more_flag() {
        let corpus = corpus();
        let entries = vec![
            entry(3, WHEN / 1000, "Third note."),
            entry(2, WHEN / 1000 - 60, "Second note."),
            entry(1, WHEN / 1000 - 120, "First note."),
        ];

        let (value, sources) = result_json(recent_dictations(&corpus.store, entries, 2));

        let rows = value["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["id"], "3");
        assert_eq!(rows[0]["when"], when(WHEN));
        assert_eq!(rows[0]["link"], "sona://dictation/3");
        assert_eq!(rows[0]["title"], "Third note.");
        assert_eq!(value["more"], true);
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[1].when_utc_ms, WHEN - 60_000);
    }

    #[test]
    fn a_meeting_opens_with_its_ledger_and_refuses_when_its_series_is_kept_local() {
        let corpus = corpus();

        let (value, sources) = result_json(meeting_result(&corpus.store, corpus.allowed).unwrap());

        assert_eq!(value["title"], "Design review");
        assert_eq!(value["when"], when(WHEN - DAY));
        assert_eq!(value["summary"], "Pricing stayed open.");
        assert_eq!(value["notes"], json!(["Typed during the call."]));
        assert_eq!(value["decisions"], json!(["Ship the tier page Friday."]));
        assert_eq!(value["action_items"][0]["owner"], "Dana");
        assert_eq!(value["series"]["key"], "weekly-design");
        assert_eq!(value["attendees"][0]["name"], "Dana Reyes");
        assert_eq!(value["speakers"], json!(["Speaker 1"]));
        assert_eq!(
            value["ledger"]["headline"],
            "Dana's tier question stayed open."
        );
        assert_eq!(
            value["ledger"]["threads"][0]["topic"],
            "Enterprise tier pricing"
        );
        assert_eq!(value["ledger"]["threads"][0]["state"], "open");
        assert_eq!(value["ledger"]["threads"][0]["owner"], "Dana Reyes");
        assert_eq!(
            value["ledger"]["threads"][0]["quote"],
            "which tier does the trial convert into"
        );
        assert_eq!(value["ledger"]["threads"][0]["at_ms"], 12000);
        assert_eq!(
            value["ledger"]["open_loops"][0]["instead"],
            "Moved to the roadmap."
        );
        assert_eq!(value["ledger"]["commitments"][0]["who"], "Dana Reyes");
        assert_eq!(value["ledger"]["commitments"][0]["firmness"], "firm");
        assert_eq!(
            value["ledger"]["caveats"],
            json!(["Audio dropped for two minutes."])
        );
        assert_eq!(value["link"], meeting_link(corpus.allowed));
        assert_eq!(sources[0].kind, QueryRowKind::Meeting);
        assert_eq!(sources[0].snippet, "Dana's tier question stayed open.");

        assert_eq!(
            meeting_result(&corpus.store, corpus.excluded).unwrap_err(),
            no_meeting(corpus.excluded),
            "the refusal reads like not found"
        );
        let unknown = MeetingSessionId::new();
        assert_eq!(
            meeting_result(&corpus.store, unknown).unwrap_err(),
            no_meeting(unknown),
            "an unknown meeting is refused with the same sentence"
        );
    }

    #[test]
    fn a_meeting_over_the_ceiling_loses_its_notes_before_its_ledger() {
        let (_directory, store) = store();
        let session_id = reviewable_meeting(&store, "Design review", WHEN);
        transcript(&store, session_id, "The empty state needs a second pass.");
        // Notes before the artifact: a typed note marks the current artifact
        // out of date, and the test needs a ledger that is still current.
        for index in 0..6 {
            note(
                &store,
                session_id,
                &format!(
                    "note {index}: {}",
                    "a sentence typed during the call. ".repeat(60)
                ),
            );
        }
        artifact(
            &store,
            session_id,
            "Pricing stayed open.",
            "Dana's tier question stayed open.",
        );

        let result = finish(
            &call("meeting", Value::Null),
            meeting_result(&store, session_id).unwrap(),
        );

        assert!(result.ok);
        assert!(
            result.result.len() <= TOOL_RESULT_MAX_BYTES,
            "{} bytes",
            result.result.len()
        );
        let value: Value = parsed(&result.result);
        assert_eq!(value["truncated"], true);
        assert!(
            value["notes"].as_array().unwrap().len() < 6,
            "the notes were cut: {}",
            value["notes"]
        );
        assert_eq!(
            value["ledger"]["threads"][0]["topic"], "Enterprise tier pricing",
            "the ledger survives the cut"
        );
        assert_eq!(value["summary"], "Pricing stayed open.");
    }

    #[test]
    fn a_transcript_pages_by_segment_and_says_where_the_next_page_starts() {
        let corpus = corpus();

        let (value, sources) =
            result_json(transcript_result(&corpus.store, corpus.allowed, 0, 80).unwrap());

        assert_eq!(value["total"], 1);
        assert_eq!(value["segments"][0]["index"], 0);
        assert_eq!(value["segments"][0]["speaker"], "Speaker 1");
        assert_eq!(
            value["segments"][0]["text"],
            "The empty state needs a second pass."
        );
        assert_eq!(value["segments"][0]["at_ms"], 0);
        assert!(
            value.get("next_offset").is_none(),
            "one segment is the whole transcript"
        );
        assert_eq!(sources[0].link, meeting_link(corpus.allowed));

        let (value, _) =
            result_json(transcript_result(&corpus.store, corpus.allowed, 5, 80).unwrap());
        assert_eq!(value["segments"], json!([]));

        assert_eq!(
            transcript_result(&corpus.store, corpus.excluded, 0, 80).unwrap_err(),
            no_meeting(corpus.excluded)
        );
    }

    #[test]
    fn a_long_transcript_page_is_cut_to_the_ceiling_and_flagged() {
        let (_directory, store) = store();
        let session_id = reviewable_meeting(&store, "Long call", WHEN);
        let lines = (0..200)
            .map(|ordinal| format!("segment {ordinal} {}", "said a thing ".repeat(8)))
            .collect::<Vec<_>>();
        transcript_segments(
            &store,
            session_id,
            &lines.iter().map(String::as_str).collect::<Vec<_>>(),
        );

        let result = finish(
            &call("transcript", Value::Null),
            transcript_result(&store, session_id, 10, 200).unwrap(),
        );

        assert!(result.ok);
        assert!(
            result.result.len() <= TOOL_RESULT_MAX_BYTES,
            "{} bytes",
            result.result.len()
        );
        let value: Value = parsed(&result.result);
        assert_eq!(value["truncated"], true);
        assert_eq!(value["total"], 200);
        let kept = value["segments"].as_array().unwrap().len();
        assert!(kept > 0 && kept < 190, "{kept} segments");
        assert_eq!(value["segments"][0]["index"], 10);
        assert_eq!(value["next_offset"], 10 + kept);
    }

    #[test]
    fn a_person_answers_with_their_meetings_and_what_is_open() {
        let corpus = corpus();
        let person_id = person(
            &corpus.store,
            "Dana Reyes",
            &["Dana"],
            &["dana@example.com"],
        );
        for session_id in [corpus.excluded, corpus.allowed] {
            link(
                &corpus.store,
                session_id,
                person_id,
                "calendar",
                "confirmed",
            );
        }

        let (value, sources) = result_json(person_result(&corpus.store, person_id).unwrap());

        assert_eq!(value["name"], "Dana Reyes");
        assert_eq!(value["aliases"], json!(["Dana"]));
        assert_eq!(
            value["meetings"], 1,
            "the excluded series is not counted: {value}"
        );
        assert_eq!(value["last_met"], when(WHEN - DAY));
        assert_eq!(value["recent_meetings"][0]["title"], "Design review");
        assert_eq!(value["link"], person_link(person_id));
        assert_eq!(sources[0].kind, QueryRowKind::Person);
        assert_eq!(sources[1].link, meeting_link(corpus.allowed));
        assert!(!value.to_string().contains("Pricing sync"));

        assert_eq!(
            person_result(&corpus.store, PersonId::new())
                .unwrap_err()
                .starts_with("no person "),
            true
        );
    }

    #[test]
    fn loops_walk_the_corpus_and_skip_the_kept_series() {
        let corpus = corpus();
        finalize(&corpus.store, corpus.excluded);
        finalize(&corpus.store, corpus.allowed);

        let (value, sources) =
            result_json(loops_result(&corpus.store, LoopFilter::Open, None, 20).unwrap());

        let rows = value["rows"].as_array().unwrap();
        // The ledger seeds one loop per unresolved thread, open question and
        // commitment, so the allowed meeting contributes three rows; the kept
        // series contributes none.
        assert_eq!(rows.len(), 3, "{value}");
        assert!(
            rows.iter()
                .all(|row| row["meeting"]["id"] == corpus.allowed.uuid().to_string()),
            "every row is the allowed meeting's: {value}"
        );
        assert_eq!(rows[0]["text"], "Enterprise tier pricing");
        assert_eq!(rows[0]["kind"], "loop");
        assert_eq!(rows[0]["status"], "open");
        assert_eq!(rows[0]["owner"], "Dana Reyes");
        assert_eq!(rows[0]["meeting"]["title"], "Design review");
        assert_eq!(rows[0]["when"], when(WHEN - DAY));
        assert_eq!(rows[2]["kind"], "commitment");
        assert_eq!(rows[2]["text"], "Send the tier comparison");
        assert_eq!(
            rows.iter()
                .map(|row| row["link"].as_str().unwrap())
                .collect::<Vec<_>>(),
            sources
                .iter()
                .map(|row| row.link.as_str())
                .collect::<Vec<_>>(),
            "one source per row, in order"
        );
        assert!(sources.iter().all(|row| row.kind == QueryRowKind::Loop));
        assert_eq!(value["more"], false);

        let (value, _) =
            result_json(loops_result(&corpus.store, LoopFilter::Done, None, 20).unwrap());
        assert_eq!(value["rows"], json!([]));
        let (value, _) = result_json(
            loops_result(&corpus.store, LoopFilter::Open, Some(PersonId::new()), 20).unwrap(),
        );
        assert_eq!(value["rows"], json!([]), "nobody is linked as an owner yet");
    }

    #[test]
    fn upcoming_drops_the_series_kept_on_this_mac() {
        let corpus = corpus();
        let occurrence = |series_key: &str, title: &str, start: i64| CalendarOccurrence {
            summary: CalendarEventSummary {
                event_key: format!("{series_key}@{start}"),
                series_key: series_key.to_string(),
                title: title.to_string(),
                attendee_count: 3,
                start_utc_ms: start,
                end_utc_ms: start + 1_800_000,
                attendees: vec![CalendarAttendee {
                    name: "Nolan".to_string(),
                    status: ParticipationStatus::Accepted,
                    email: None,
                    is_self: false,
                }],
                notes: None,
                calendar_name: Some("Work".to_string()),
                url: None,
            },
            is_recurring: !series_key.is_empty(),
        };
        let rows = upcoming_rows(
            vec![
                occurrence("weekly-pricing", "Pricing sync", WHEN + DAY),
                occurrence("weekly-design", "Design review", WHEN + 2 * DAY),
                occurrence("", "Coffee", WHEN + 3 * DAY),
            ],
            &HashMap::new(),
            &HashMap::new(),
        );

        let (value, sources) =
            result_json(upcoming_result(&corpus.store, CalendarAccess::Authorized, rows).unwrap());

        let listed = value["rows"].as_array().unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|row| row["title"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["Design review", "Coffee"]
        );
        assert_eq!(listed[0]["start"], when(WHEN + 2 * DAY));
        assert_eq!(listed[0]["attendees"], json!(["Nolan"]));
        assert_eq!(listed[0]["series"]["key"], "weekly-design");
        assert_eq!(listed[1]["series"], Value::Null);
        assert_eq!(value["calendar_access"], "authorized");
        assert!(
            sources.is_empty(),
            "a calendar event has no sona:// address"
        );
    }

    #[test]
    fn a_dictation_is_returned_in_full_with_its_raw_text_when_it_differs() {
        let mut dictation = entry(9, WHEN / 1000, "send the deck friday");
        dictation.post_processed_text = Some("Send the deck Friday.".to_string());

        let (value, sources) = result_json(dictation_result(&dictation, &[]));

        assert_eq!(value["id"], 9);
        assert_eq!(value["when"], when(WHEN));
        assert_eq!(value["text"], "Send the deck Friday.");
        assert_eq!(value["raw_text"], "send the deck friday");
        assert_eq!(value["mode"], Value::Null);
        assert_eq!(value["link"], "sona://dictation/9");
        assert_eq!(sources[0].kind, QueryRowKind::Dictation);

        dictation.post_processed_text = None;
        let (value, _) = result_json(dictation_result(&dictation, &[]));
        assert_eq!(value["raw_text"], Value::Null);
    }

    #[test]
    fn word_stats_ignore_stopwords_case_and_one_letter_tokens() {
        let entries = vec![
            entry(
                1,
                WHEN / 1000,
                "The Deck is the deck, and I don't like THE deck.",
            ),
            entry(2, WHEN / 1000, "Friday's deck. A friday!"),
        ];

        let (value, sources) = result_json(word_stats_result(&entries, 90, 2));

        assert_eq!(value["entries"], 2);
        assert_eq!(value["days"], 90);
        assert_eq!(
            value["top"],
            json!([{"word": "deck", "count": 4}, {"word": "friday", "count": 2}])
        );
        assert_eq!(
            value["total_words"], 13,
            "every token of two or more letters: the deck is the deck and don like the deck / friday deck friday; 'i', 't', 's' and 'a' are not words"
        );
        assert!(value.get("capped").is_none());
        assert!(sources.is_empty());
    }

    #[test]
    fn activity_joins_the_two_trends_by_day_newest_first() {
        let point = |date: &str, recordings: u64, words: u64| HistoryTrendPoint {
            local_date: date.to_string(),
            recordings,
            duration_ms: 0,
            words,
            by_source: vec![HistoryTrendSourceTotals {
                source_kind: None,
                recordings,
                duration_ms: 0,
                words,
            }],
        };
        let meetings = vec![MeetingTrendPoint {
            local_date: "2026-08-14".to_string(),
            meetings: 2,
            verified_captured_duration_ms: 130 * 60_000,
            transcript_segments: 0,
            generated_action_items: 0,
        }];

        let (value, _) = result_json(activity_result(
            &[
                point("2026-08-12", 1, 10),
                point("2026-08-13", 0, 0),
                point("2026-08-14", 4, 400),
            ],
            &meetings,
            2,
        ));

        assert_eq!(
            value["rows"],
            json!([
                {"date": "2026-08-14", "dictations": 4, "words": 400, "meetings": 2, "meeting_minutes": 130},
                {"date": "2026-08-13", "dictations": 0, "words": 0, "meetings": 0, "meeting_minutes": 0},
            ])
        );
        assert_eq!(trend_range(7), DashboardTrendRange::Days7);
        assert_eq!(trend_range(8), DashboardTrendRange::Days30);
        assert_eq!(trend_range(90), DashboardTrendRange::Days180);
    }

    #[test]
    fn a_result_over_the_ceiling_is_cut_flagged_and_its_sources_follow() {
        let (_directory, store) = store();
        let rows = (0..300)
            .map(|index| QueryRow {
                kind: QueryRowKind::Dictation,
                id: index.to_string(),
                title: "a title".to_string(),
                snippet: "a sentence that was said. ".repeat(4),
                when_utc_ms: WHEN - index,
                link: super::super::dictation_link(index),
            })
            .collect::<Vec<_>>();

        let result = finish(
            &call("search", Value::Null),
            search_result(&store, rows, false),
        );

        assert!(result.ok);
        assert!(
            result.result.len() <= TOOL_RESULT_MAX_BYTES,
            "{} bytes",
            result.result.len()
        );
        let value: Value = parsed(&result.result);
        assert_eq!(value["truncated"], true);
        let kept = value["rows"].as_array().unwrap().len();
        assert!(kept > 0 && kept < 300, "{kept} rows");
        assert_eq!(result.sources.len(), kept, "one source per row shown");
        assert_eq!(value["rows"][0]["id"], "0", "the newest rows survive");
        assert_eq!(result.sources[kept - 1].id, (kept - 1).to_string());

        let small = finish(
            &call("search", Value::Null),
            search_result(&store, Vec::new(), false),
        );
        let value: Value = parsed(&small.result);
        assert!(value.get("truncated").is_none());
    }

    #[test]
    fn a_string_over_the_ceiling_is_cut_on_a_character_boundary() {
        let dictation = entry(1, WHEN / 1000, &"héllo wörld ".repeat(1_000));

        let result = finish(
            &call("dictation", Value::Null),
            dictation_result(&dictation, &[]),
        );

        assert!(result.ok);
        assert!(result.result.len() <= TOOL_RESULT_MAX_BYTES);
        let value: Value = parsed(&result.result);
        assert_eq!(value["truncated"], true);
        let text = value["text"].as_str().unwrap();
        assert!(text.ends_with('…'));
        assert!(text.starts_with("héllo wörld "));
    }

    #[test]
    fn a_when_is_rfc_3339_in_the_host_zone() {
        let rendered = when(WHEN);
        let parsed = DateTime::parse_from_rfc3339(&rendered).unwrap();
        assert_eq!(parsed.timestamp_millis(), WHEN);
        assert_eq!(
            parsed.offset().local_minus_utc(),
            Local
                .timestamp_millis_opt(WHEN)
                .unwrap()
                .offset()
                .local_minus_utc()
        );
    }

    #[test]
    fn loop_ids_derive_the_same_way_the_store_derives_them() {
        let (_directory, store) = store();
        let session_id = reviewable_meeting(&store, "Ledger", WHEN);
        artifact(&store, session_id, "Summary.", "Headline.");
        finalize(&store, session_id);
        let expected =
            MeetingLoopId::derive(session_id, MeetingLoopKind::Loop, "Enterprise tier pricing");

        let (value, _) = result_json(loops_result(&store, LoopFilter::All, None, 50).unwrap());

        assert_eq!(value["rows"][0]["loop_id"], expected.as_str());
        assert_eq!(value["rows"][0]["link"], loop_link(&expected));
    }
}

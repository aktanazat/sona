//! The corpus as a process outside this app sees it: read-only, consent-gated,
//! one JSON object per invocation.
//!
//! This is the query plane's third client, after ⌘K and the panel agent, and
//! it is deliberately the thinnest of them. Everything it answers comes out of
//! [`crate::query`] or out of a `MeetingStore` read that already exists; the
//! only thing this module owns is the projection — which fields an outside
//! reader gets, and under what version — and the refusal.
//!
//! # The gate
//!
//! Nothing here reads the corpus until [`ExternalConsent`] says the operator
//! turned external access on. That is why every read goes through [`answer`]
//! and why [`answer`] takes the consent as its first argument rather than
//! reading it itself: a caller cannot forget to ask, and there is no second
//! door. The refusal names the settings row, so an agent that hits it can tell
//! its human where to click instead of guessing that Sona is broken.
//!
//! # Why the projections are not the app's own types
//!
//! `MeetingHistorySummary`, `MeetingLoopRow` and `PersonListEntry` are shaped
//! for the views that render them and change when those views do. An external
//! contract that re-exported them would break on every UI refactor, silently,
//! in somebody else's script. So each verb has a narrow projection here, all of
//! them stamped with [`QUERY_SCHEMA_VERSION`] — the same number the plane's own
//! pages carry, because these are the same nouns out of the same corpus, and a
//! second version to bump in lockstep is a second thing to get wrong.
//!
//! # Reads and one fenced write
//!
//! Every verb except `--loop-resolve` reads. Resolving a loop first reads that
//! row's revision, then writes against it, so it requires both external-consent
//! rows and returns the store receipt verbatim.

use super::{
    loop_link, meeting_link, person_link, QueryError, QueryEventsPage, QueryScope, QuerySearchPage,
    QUERY_SCHEMA_VERSION,
};
use crate::cli::CliArgs;
use crate::managers::history::HistoryManager;
use crate::meeting::detection::calendar::{CalendarAccess, CalendarSource};
use crate::meeting::loop_types::{
    MeetingLoopDirection, MeetingLoopId, MeetingLoopKind, MeetingLoopResolution,
    MeetingLoopResolveRequest, MeetingLoopRow, MeetingLoopStatus,
};
use crate::meeting::people_types::{PersonListEntry, PersonMeetingHeadline};
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::store::MeetingStore;
use crate::meeting::types::{
    CaptureCompleteness, EffectiveTranscriptSegment, MeetingCommandError, MeetingCommandKind,
    MeetingHistoryHeadline, MeetingHistorySummary, MeetingListFilter, MeetingOperationId,
    MeetingPhase, MeetingReviewSnapshot, MeetingSessionId, OperationActor, OperationReceipt,
    OperationResult,
};
use crate::meeting::upcoming::{upcoming_window, UPCOMING_DEFAULT_DAYS};
use crate::meeting::upcoming_types::MeetingUpcomingRow;
use crate::settings::AppSettings;
use chrono::{Local, NaiveDate, TimeZone};
use serde::Serialize;
use std::sync::Arc;
use uuid::Uuid;

/// Where a person turns each scope on. Named in the refusal so an agent that is
/// refused can say where to click; written once so the CLI, the MCP server and
/// the settings rows cannot drift apart about what the rows are called.
pub const EXTERNAL_ACCESS_SETTING_PATH: &str = "Settings > Agents > External access";
pub const EXTERNAL_MUTATIONS_SETTING_PATH: &str = "Settings > Agents > External mutations";

/// How many rows a verb returns when the caller names no limit, and the most
/// it will return when they name a large one. The plane's own page sizes,
/// because these pages are the same pages.
const DEFAULT_LIMIT: usize = 25;
const MAX_LIMIT: usize = 100;

/// How many derived loop rows a page consumes after its cursor. Loop rows are
/// not indexed, so seeking an existing durable loop id walks the current
/// corpus first.
// ponytail: cursor seeks are linear over derived rows; index loop order if that
// becomes expensive for large corpora.
const LOOP_SCAN_DEPTH: usize = 500;

/// What a verb needs the operator to have allowed.
///
/// Two scopes rather than one, and not a ladder: reading the corpus and
/// changing it are different questions. `--loop-resolve` needs `Read` to
/// inspect its fence before it needs `Mutate` to write; every other verb needs
/// only `Read`. A mutation grant alone therefore cannot close a loop blindly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExternalScope {
    Read,
    Mutate,
}

const READ_SCOPES: &[ExternalScope] = &[ExternalScope::Read];
const LOOP_RESOLVE_SCOPES: &[ExternalScope] = &[ExternalScope::Read, ExternalScope::Mutate];

impl ExternalScope {
    /// The settings row that opens this scope.
    const fn settings_path(self) -> &'static str {
        match self {
            Self::Read => EXTERNAL_ACCESS_SETTING_PATH,
            Self::Mutate => EXTERNAL_MUTATIONS_SETTING_PATH,
        }
    }

    /// What the refusal says. Whole sentences rather than a fragment stitched
    /// into one template: each scope names a different row and grants a
    /// different thing, and this copy is what an agent repeats to its human.
    const fn refusal(self) -> &'static str {
        match self {
            Self::Read => "External access is off. Turn on Settings > Agents > External access in Sona to allow read-only corpus queries.",
            Self::Mutate => "External mutations are off. Turn on Settings > Agents > External mutations in Sona to allow changes to the corpus.",
        }
    }
}

/// Which scopes the operator has opened to processes outside this app.
///
/// One value carrying both answers rather than a `bool` per callsite, because
/// it is the only thing standing between an agent on this Mac and every meeting
/// on it, and a `true` at a callsite says nothing about which `true` it is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExternalConsent {
    reads: bool,
    mutations: bool,
}

impl ExternalConsent {
    pub fn from_settings(settings: &AppSettings) -> Self {
        Self {
            reads: settings.external_query_enabled,
            mutations: settings.external_mutations_enabled,
        }
    }

    const fn allows(self, scope: ExternalScope) -> bool {
        match scope {
            ExternalScope::Read => self.reads,
            ExternalScope::Mutate => self.mutations,
        }
    }
}

/// Why a request produced no answer. Machine tokens: a caller branches on
/// these, and the MCP server passes them through unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalErrorCode {
    /// The scope this verb needs is off. The one refusal that is a choice
    /// rather than a fault, which is why it carries the settings path.
    ConsentRequired,
    /// The corpus cannot be opened: the OS keychain is locked or refused, or
    /// meeting storage failed to mount.
    Unavailable,
    /// A flag combination, id, date or limit this surface will not answer.
    InvalidRequest,
    /// The id parsed but names nothing in this corpus.
    NotFound,
    Failed,
}

impl ExternalErrorCode {
    /// The process exit code this refusal leaves. Two is bad input, matching
    /// what the rest of the headless surface already means by it; everything
    /// else — including a withheld consent, which is a refusal rather than a
    /// typo — is one.
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::InvalidRequest => 2,
            _ => 1,
        }
    }
}

/// One refusal, as it is printed on stderr.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExternalError {
    pub schema_version: u32,
    pub error: ExternalErrorCode,
    pub message: String,
    /// Present only on [`ExternalErrorCode::ConsentRequired`], because it is
    /// the only refusal a human can clear by clicking something.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings_path: Option<&'static str>,
}

impl ExternalError {
    fn new(error: ExternalErrorCode, message: impl Into<String>) -> Self {
        Self {
            schema_version: QUERY_SCHEMA_VERSION,
            error,
            message: message.into(),
            settings_path: None,
        }
    }

    fn consent_required(scope: ExternalScope) -> Self {
        Self {
            schema_version: QUERY_SCHEMA_VERSION,
            error: ExternalErrorCode::ConsentRequired,
            message: scope.refusal().to_string(),
            settings_path: Some(scope.settings_path()),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new(ExternalErrorCode::InvalidRequest, message)
    }
}

impl From<QueryError> for ExternalError {
    fn from(error: QueryError) -> Self {
        match error {
            // Named as the corpus, not as one store: both the meeting store
            // and the dictation store produce this, and a search that reaches
            // the dictation half has already opened the meeting one.
            QueryError::Unavailable => Self::new(
                ExternalErrorCode::Unavailable,
                "The corpus is not open. Unlock the login keychain and make sure Sona has run at least once.",
            ),
            QueryError::InvalidRequest => Self::invalid("The query plane refused this request."),
            QueryError::UnknownCursor => Self::new(
                ExternalErrorCode::NotFound,
                "That event id is no longer in the corpus. Start again without --after.",
            ),
            QueryError::Failed => Self::new(ExternalErrorCode::Failed, "The corpus read failed."),
        }
    }
}

/// Which loop rows `--loops` keeps. Narrower than [`MeetingLoopStatus`] on
/// purpose: `dropped` and `carried` are outcomes a reader sees on the rows
/// they get back, not questions they ask.
#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
pub enum ExternalLoopStatus {
    Open,
    Done,
}

/// Which side of a loop `--mine` and `--waiting` keep.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExternalLoopSide {
    Mine,
    WaitingOn,
}

/// One thing the corpus can answer or do, after the flags have been checked.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExternalRequest {
    Search {
        scope: QueryScope,
        query: String,
        limit: usize,
    },
    Meetings {
        /// Inclusive lower bound in UTC ms, from `--from`.
        since_utc_ms: Option<i64>,
        /// Exclusive upper bound in UTC ms: the start of the day after `--to`.
        before_utc_ms: Option<i64>,
        limit: usize,
    },
    Meeting {
        session_id: MeetingSessionId,
    },
    Transcript {
        session_id: MeetingSessionId,
    },
    Loops {
        status: Option<ExternalLoopStatus>,
        side: Option<ExternalLoopSide>,
        after_id: Option<String>,
        limit: usize,
    },
    People {
        name: String,
        limit: usize,
    },
    Events {
        after_id: Option<String>,
        limit: usize,
    },
    Upcoming {
        limit: usize,
    },
    /// The one verb here that writes.
    LoopResolve {
        loop_id: MeetingLoopId,
    },
}

impl ExternalRequest {
    /// Which grants this verb needs. The loop write reads its row to fence the
    /// write, so it needs both grants; a future verb must choose explicitly.
    pub const fn required_scopes(&self) -> &'static [ExternalScope] {
        match self {
            Self::LoopResolve { .. } => LOOP_RESOLVE_SCOPES,
            Self::Search { .. }
            | Self::Meetings { .. }
            | Self::Meeting { .. }
            | Self::Transcript { .. }
            | Self::Loops { .. }
            | Self::People { .. }
            | Self::Events { .. }
            | Self::Upcoming { .. } => READ_SCOPES,
        }
    }
}

/// A request the operator has allowed.
///
/// The gate is this type rather than a check inside [`answer`]: the only way to
/// obtain one is [`AllowedRequest::new`], which takes the consent and asks the
/// request which grants it needs, so there is no signature in this module that
/// touches the corpus without one having been presented. A future verb cannot
/// forget the check, because it cannot be called without it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AllowedRequest(ExternalRequest);

impl AllowedRequest {
    pub fn new(consent: ExternalConsent, request: ExternalRequest) -> Result<Self, ExternalError> {
        for scope in request.required_scopes() {
            if !consent.allows(*scope) {
                return Err(ExternalError::consent_required(*scope));
            }
        }
        Ok(Self(request))
    }

    pub fn request(&self) -> &ExternalRequest {
        &self.0
    }
}

/// Whether this invocation is the headless corpus surface rather than a
/// dictation run.
///
/// Read by `is_headless_mode` and by the headless branch that decides which
/// managers to build, so it has to answer without touching the corpus.
pub fn is_external_query(args: &CliArgs) -> bool {
    args.query.is_some()
        || args.meetings
        || args.meeting.is_some()
        || args.transcript.is_some()
        || args.loops
        || args.people.is_some()
        || args.events
        || args.upcoming
        || args.loop_resolve.is_some()
}

/// The modifier flag a command line named that its verb does not read, if
/// there is one.
///
/// `cli.rs` hangs every modifier off its verb with clap's `requires`, and
/// that mechanism does not hold here: the nine verbs are members of the
/// `read` `ArgGroup`, and a clap requirement naming a group member is
/// satisfied by *any* member of that group. So `--last`'s
/// `requires = "meetings"` is satisfied by `--query` being present, and
/// `sona --query dana --last 3` parses, exits 0, and answers the search with
/// the `--last` silently dropped. Measured that way on the shipped binary for
/// all of `--scope`, `--last`, `--from`, `--to`, `--status`, `--mine`,
/// `--waiting` and `--after`.
///
/// The group is not the thing to change: it is what makes `--meetings
/// --loops` a usage error clap words itself. So the binding is enforced here,
/// which also puts the refusal in this surface's own JSON shape rather than
/// in clap's plain text — the shape the MCP server can read a code out of.
///
/// Each modifier's declaration in [`CliArgs`] carries a comment pointing
/// here, and the test `every_modifier_outside_the_read_group_is_bound_to_a_verb`
/// enumerates them from clap's own metadata, so a modifier added there
/// without a row below fails that test rather than being dropped in silence.
fn foreign_modifier(args: &CliArgs) -> Option<&'static str> {
    // Every verb that returns a page reads `--limit`, `--meetings` included:
    // its help line says otherwise, but `--last`'s fallback has always been
    // `--limit`, and refusing what works is not this guard's job.
    let pages = args.query.is_some()
        || args.meetings
        || args.loops
        || args.people.is_some()
        || args.events
        || args.upcoming;
    [
        (
            args.scope.is_some(),
            args.query.is_some(),
            "--scope is only read by --query.",
        ),
        (
            args.last.is_some(),
            args.meetings,
            "--last is only read by --meetings.",
        ),
        (
            args.from.is_some(),
            args.meetings,
            "--from is only read by --meetings.",
        ),
        (
            args.to.is_some(),
            args.meetings,
            "--to is only read by --meetings.",
        ),
        (
            args.status.is_some(),
            args.loops,
            "--status is only read by --loops.",
        ),
        (args.mine, args.loops, "--mine is only read by --loops."),
        (
            args.waiting,
            args.loops,
            "--waiting is only read by --loops.",
        ),
        (
            args.after.is_some(),
            args.events || args.loops,
            "--after is only read by --events or --loops.",
        ),
        (
            args.limit.is_some(),
            pages,
            "--limit is only read by a verb that returns a page: --query, --meetings, --loops, --people, --events, --upcoming.",
        ),
    ]
    .into_iter()
    .find_map(|(named, verb_reads_it, refusal)| (named && !verb_reads_it).then_some(refusal))
}

impl ExternalRequest {
    /// The request these flags name.
    ///
    /// Clap has already rejected two verbs at once, so what is left here is
    /// the checking clap cannot do: that an id is a uuid, that a date is a
    /// date, that a limit is a number of rows, and — because the `read` group
    /// defeats `requires`, see [`foreign_modifier`] — that every modifier
    /// named belongs to the verb it was named beside.
    pub fn from_args(args: &CliArgs) -> Result<Self, ExternalError> {
        if let Some(refusal) = foreign_modifier(args) {
            return Err(ExternalError::invalid(refusal));
        }
        let limit = match args.limit {
            None => DEFAULT_LIMIT,
            Some(0) => return Err(ExternalError::invalid("--limit must be at least 1.")),
            Some(limit) => limit.min(MAX_LIMIT),
        };
        if let Some(query) = args.query.as_ref() {
            return Ok(Self::Search {
                scope: args.scope.unwrap_or_default(),
                query: query.clone(),
                limit,
            });
        }
        if args.meetings {
            let last = match args.last {
                None => None,
                Some(0) => return Err(ExternalError::invalid("--last must be at least 1.")),
                Some(last) => Some(last.min(MAX_LIMIT)),
            };
            return Ok(Self::Meetings {
                since_utc_ms: args.from.as_deref().map(day_start_utc_ms).transpose()?,
                before_utc_ms: args.to.as_deref().map(next_day_start_utc_ms).transpose()?,
                limit: last.unwrap_or(limit),
            });
        }
        if let Some(id) = args.meeting.as_ref() {
            return Ok(Self::Meeting {
                session_id: session_id(id)?,
            });
        }
        if let Some(id) = args.transcript.as_ref() {
            return Ok(Self::Transcript {
                session_id: session_id(id)?,
            });
        }
        if args.loops {
            return Ok(Self::Loops {
                status: args.status,
                side: match (args.mine, args.waiting) {
                    (true, _) => Some(ExternalLoopSide::Mine),
                    (_, true) => Some(ExternalLoopSide::WaitingOn),
                    _ => None,
                },
                after_id: args.after.clone(),
                limit,
            });
        }
        if let Some(name) = args.people.as_ref() {
            return Ok(Self::People {
                name: name.clone(),
                limit,
            });
        }
        if args.events {
            return Ok(Self::Events {
                after_id: args.after.clone(),
                limit,
            });
        }
        if args.upcoming {
            return Ok(Self::Upcoming { limit });
        }
        if let Some(value) = args.loop_resolve.as_ref() {
            return Ok(Self::LoopResolve {
                loop_id: loop_id(value)?,
            });
        }
        Err(ExternalError::invalid(
            "No verb was named. Pass one of --query, --meetings, --meeting, --transcript, --loops, --people, --events, --upcoming, --loop-resolve.",
        ))
    }
}

fn session_id(value: &str) -> Result<MeetingSessionId, ExternalError> {
    Uuid::parse_str(value.trim())
        .map(MeetingSessionId::from_uuid)
        .map_err(|_| ExternalError::invalid(format!("{value:?} is not a meeting id.")))
}

/// A loop's own address, checked as far as this surface can see: the meeting it
/// names has to be a uuid. The rest of the id is the ledger's business, which is
/// where a digest that matches nothing becomes a `not_found`.
fn loop_id(value: &str) -> Result<MeetingLoopId, ExternalError> {
    let value = value.trim();
    let malformed = || ExternalError::invalid(format!("{value:?} is not a loop id."));
    let id = MeetingLoopId(value.to_string());
    id.session_id().ok_or_else(malformed)?;
    if id.content_key().is_none_or(str::is_empty) {
        return Err(malformed());
    }
    Ok(id)
}

/// Midnight local time on `date`, as UTC milliseconds.
///
/// Local rather than UTC because `--from 2026-03-01` is a person naming a day
/// on their own calendar, and the meetings list they are comparing it against
/// is grouped by local day too.
fn day_start_utc_ms(date: &str) -> Result<i64, ExternalError> {
    let parsed = NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d")
        .map_err(|_| ExternalError::invalid(format!("{date:?} is not a YYYY-MM-DD date.")))?;
    let midnight = parsed
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| ExternalError::invalid(format!("{date:?} has no midnight.")))?;
    Local
        .from_local_datetime(&midnight)
        .earliest()
        .map(|moment| moment.timestamp_millis())
        // A date whose midnight a daylight-saving jump skipped: the day still
        // starts, one hour later, and `latest` is that instant.
        .or_else(|| {
            Local
                .from_local_datetime(&midnight)
                .latest()
                .map(|moment| moment.timestamp_millis())
        })
        .ok_or_else(|| ExternalError::invalid(format!("{date:?} is not a local date.")))
}

/// Midnight local time on the day after `date`. `--to` names a day the reader
/// wants included, so the bound below it is where the next one starts.
fn next_day_start_utc_ms(date: &str) -> Result<i64, ExternalError> {
    let parsed = NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d")
        .map_err(|_| ExternalError::invalid(format!("{date:?} is not a YYYY-MM-DD date.")))?;
    let next = parsed
        .succ_opt()
        .ok_or_else(|| ExternalError::invalid(format!("{date:?} has no day after it.")))?;
    day_start_utc_ms(&next.format("%Y-%m-%d").to_string())
}

/// One meeting in a list. The row a reader scans before deciding which meeting
/// to open.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExternalMeetingRow {
    pub id: Uuid,
    pub title: String,
    pub phase: MeetingPhase,
    pub when_utc_ms: i64,
    pub recorded_duration_ms: Option<i64>,
    /// Whether the recorded duration covers every capture window.
    pub capture_completeness: CaptureCompleteness,
    pub speakers: Vec<String>,
    /// The meeting in one line, tagged with where the line came from: prose a
    /// model wrote, or a word count the store measured.
    pub headline: MeetingHistoryHeadline,
    pub link: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExternalMeetingsPage {
    pub schema_version: u32,
    pub entries: Vec<ExternalMeetingRow>,
    pub has_more: bool,
}

/// One meeting opened: what it was about, and what it left open.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExternalMeetingDetail {
    pub schema_version: u32,
    pub id: Uuid,
    pub title: String,
    pub phase: MeetingPhase,
    /// The existing processing state, including a terminal failure reason.
    pub processing_status: crate::meeting::types::ProcessingStatus,
    /// When capture began. Absent for a session that never started, which has
    /// nothing in it to read either.
    pub started_at_utc_ms: Option<i64>,
    pub speakers: Vec<String>,
    /// The generated summary of the current artifact revision. Absent until
    /// processing has produced one.
    pub summary: Option<String>,
    /// The ledger's reading of where the meeting landed, when it has one.
    pub headline: Option<String>,
    pub notes: Vec<String>,
    pub loops: Vec<ExternalLoopRow>,
    pub link: String,
}

/// One speaker-labelled line of a transcript.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExternalTranscriptLine {
    pub speaker: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExternalTranscript {
    pub schema_version: u32,
    pub meeting_id: Uuid,
    pub title: String,
    pub started_at_utc_ms: Option<i64>,
    pub lines: Vec<ExternalTranscriptLine>,
    pub link: String,
}

/// One actionable ledger row, with the meeting it was raised in.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExternalLoopRow {
    pub id: String,
    pub meeting_id: Uuid,
    pub meeting_title: String,
    pub kind: MeetingLoopKind,
    pub status: MeetingLoopStatus,
    /// Whose side of the conversation this is on: the user owes it, somebody
    /// else does, or the ledger never said.
    pub direction: MeetingLoopDirection,
    pub text: String,
    /// The person the row belongs to: the name the user picked when there is
    /// one, the name the ledger read otherwise.
    pub owner: Option<String>,
    pub when_utc_ms: i64,
    pub resolved_at_utc_ms: Option<i64>,
    pub link: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExternalLoopsPage {
    pub schema_version: u32,
    pub entries: Vec<ExternalLoopRow>,
    /// Pass to `--after` with the same filters to continue this scan.
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

/// One person, as a profile lookup answers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExternalPersonRow {
    pub id: Uuid,
    pub display_name: String,
    pub aliases: Vec<String>,
    pub calendar_emails: Vec<String>,
    pub meetings_count: u64,
    pub last_meeting_at_utc_ms: Option<i64>,
    pub last_meeting_title: Option<String>,
    pub last_meeting_headline: Option<String>,
    pub link: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExternalPeoplePage {
    pub schema_version: u32,
    pub entries: Vec<ExternalPersonRow>,
    pub has_more: bool,
}

/// One event in the week ahead.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExternalUpcomingRow {
    /// The occurrence's own key, which is what starts this specific event.
    pub event_key: String,
    pub title: String,
    pub start_utc_ms: i64,
    pub end_utc_ms: i64,
    /// Participants the calendar named, the operator's own entry excluded.
    pub attendees: Vec<String>,
    /// Participants including the ones the calendar refused to name, so it can
    /// exceed `attendees.len()`.
    pub attendee_count: u32,
    pub calendar_name: Option<String>,
    pub join_url: Option<String>,
    /// Present exactly when the event repeats.
    pub series_key: Option<String>,
    /// A standing grant covers this series, so its occurrences record
    /// themselves. `false` for a one-off, which has no series to grant.
    pub always_record: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExternalUpcomingPage {
    pub schema_version: u32,
    /// Whose TCC decision `calendar_access` reports. On macOS, a shell
    /// invocation is commonly attributed to its responsible terminal, not the
    /// installed Sona GUI.
    pub calendar_access_subject: &'static str,
    /// Whether that TCC subject can read the calendar. An empty list under
    /// `authorized` is a free week; otherwise this invocation cannot read it.
    pub calendar_access: CalendarAccess,
    pub window_start_utc_ms: i64,
    pub window_end_utc_ms: i64,
    pub entries: Vec<ExternalUpcomingRow>,
    pub has_more: bool,
}

/// One mutation, as it is printed.
///
/// The receipt is the store's own [`OperationReceipt`], verbatim rather than
/// projected — unlike every row above it. A receipt is not a view of a noun, it
/// is the audit record of a write, and an outside reader that got a reshaped
/// copy could not compare what it did against what the app's own event stream
/// says it did.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExternalReceipt {
    pub schema_version: u32,
    pub receipt: OperationReceipt,
}

/// One answer. Serialised untagged: the verb is what the caller asked for, so
/// repeating it in the payload would be a field nobody reads.
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum ExternalResponse {
    Search(QuerySearchPage),
    Meetings(ExternalMeetingsPage),
    Meeting(Box<ExternalMeetingDetail>),
    Transcript(ExternalTranscript),
    Loops(ExternalLoopsPage),
    People(ExternalPeoplePage),
    Events(QueryEventsPage),
    Upcoming(ExternalUpcomingPage),
    LoopResolve(ExternalReceipt),
}

impl ExternalResponse {
    /// The process exit code this answer leaves.
    ///
    /// Zero for every read, and for a write that landed. One for a write the
    /// fence rejected — which is an *answer*, not a refusal, so the receipt
    /// still goes to stdout and stderr stays empty. Without this a shell doing
    /// `sona --loop-resolve … && …` would read a rejected write as a closed
    /// loop, and the only thing telling it otherwise would be a JSON field it
    /// has to parse.
    const fn exit_code(&self) -> i32 {
        match self {
            Self::LoopResolve(answer) => match answer.receipt.result {
                OperationResult::Committed => 0,
                _ => 1,
            },
            _ => 0,
        }
    }
}

/// Answer one request the operator has allowed.
///
/// The store projections are this module's; `search` and `events` are the
/// plane's own pages, handed back unchanged, because a thin client that
/// reshaped them would be a second answer to a question
/// [`crate::query`] has already answered.
///
/// `calendar` is a collaborator rather than something looked up here because
/// this process builds no detection loop: `--upcoming` needs the calendar and
/// nothing else detection owns, and the headless branch hands over exactly that.
pub async fn answer(
    allowed: &AllowedRequest,
    meetings: &Arc<MeetingSessionManager>,
    history: &Arc<HistoryManager>,
    calendar: &dyn CalendarSource,
) -> Result<ExternalResponse, ExternalError> {
    match allowed.request() {
        ExternalRequest::Search {
            scope,
            query,
            limit,
        } => Ok(ExternalResponse::Search(
            super::search(meetings, history, *scope, query, Some(*limit), None).await?,
        )),
        ExternalRequest::Events { after_id, limit } => Ok(ExternalResponse::Events(
            super::events(meetings, after_id.clone(), Some(*limit)).await?,
        )),
        ExternalRequest::Meetings {
            since_utc_ms,
            before_utc_ms,
            limit,
        } => Ok(ExternalResponse::Meetings(meetings_page(
            store(meetings).await?.as_ref(),
            *since_utc_ms,
            *before_utc_ms,
            *limit,
        )?)),
        ExternalRequest::Meeting { session_id } => Ok(ExternalResponse::Meeting(Box::new(
            meeting_detail(store(meetings).await?.as_ref(), *session_id)?,
        ))),
        ExternalRequest::Transcript { session_id } => Ok(ExternalResponse::Transcript(transcript(
            store(meetings).await?.as_ref(),
            *session_id,
        )?)),
        ExternalRequest::Loops {
            status,
            side,
            after_id,
            limit,
        } => Ok(ExternalResponse::Loops(loops_page(
            store(meetings).await?.as_ref(),
            *status,
            *side,
            after_id.as_deref(),
            *limit,
        )?)),
        ExternalRequest::People { name, limit } => Ok(ExternalResponse::People(people_page(
            store(meetings).await?.as_ref(),
            name,
            *limit,
        )?)),
        ExternalRequest::Upcoming { limit } => Ok(ExternalResponse::Upcoming(
            upcoming_page(meetings, calendar, *limit).await?,
        )),
        ExternalRequest::LoopResolve { loop_id } => Ok(ExternalResponse::LoopResolve(
            resolve_loop(meetings, loop_id).await?,
        )),
    }
}

/// The mounted corpus, or the refusal that says why it is not open. Opening it
/// reads the OS credential store, which is where a locked keychain surfaces.
async fn store(meetings: &Arc<MeetingSessionManager>) -> Result<Arc<MeetingStore>, ExternalError> {
    meetings
        .store()
        .await
        .map_err(|error| QueryError::from(error).into())
}

/// The week ahead, as the Meetings home reads it, projected for an outside
/// reader.
///
/// The window is the pane's own — today plus [`UPCOMING_DEFAULT_DAYS`] more
/// local days — so a script and the screen answer the same question. `--limit`
/// cuts the rows rather than the window: a reader asking for three wants the
/// next three, not the next three days.
///
/// The calendar read blocks. It runs inline because this process has one job and
/// this is it, unlike `meeting_upcoming_events`, which is answering a window in
/// front of a webview.
async fn upcoming_page(
    meetings: &Arc<MeetingSessionManager>,
    calendar: &dyn CalendarSource,
    limit: usize,
) -> Result<ExternalUpcomingPage, ExternalError> {
    let window = upcoming_window(Local::now(), UPCOMING_DEFAULT_DAYS);
    let occurrences = calendar.events_between(window.0, window.1);
    let events = meetings
        .upcoming_events(calendar.access(), window, occurrences)
        .await
        .map_err(|error| ExternalError::from(QueryError::from(error)))?;
    let has_more = events.rows.len() > limit;
    Ok(ExternalUpcomingPage {
        schema_version: QUERY_SCHEMA_VERSION,
        calendar_access_subject: "responsible_process",
        calendar_access: events.access,
        window_start_utc_ms: events.window_start_utc_ms,
        window_end_utc_ms: events.window_end_utc_ms,
        entries: events
            .rows
            .into_iter()
            .take(limit)
            .map(upcoming_row)
            .collect(),
        has_more,
    })
}

fn upcoming_row(row: MeetingUpcomingRow) -> ExternalUpcomingRow {
    ExternalUpcomingRow {
        event_key: row.event_key,
        title: row.title,
        start_utc_ms: row.start_utc_ms,
        end_utc_ms: row.end_utc_ms,
        attendees: row
            .attendees
            .into_iter()
            .filter(|attendee| !attendee.is_self)
            .map(|attendee| attendee.name)
            .collect(),
        attendee_count: row.attendee_count,
        calendar_name: row.calendar_name,
        join_url: row.join_url,
        series_key: row.series.as_ref().map(|series| series.series_key.clone()),
        always_record: row.series.is_some_and(|series| series.always_record),
    }
}

const EXTERNAL_LOOP_RESOLVE_OPERATION_NAMESPACE: Uuid =
    Uuid::from_u128(0x9cd2_8990_44a3_4e58_97e5_4243_1e5e_5bd0);

/// Mark one loop done through the app's own resolve path.
///
/// The row's revision fences the write and identifies its external operation.
/// A retry after a resolution closed it returns that row's stored receipt;
/// reopening advances the row revision, so a later close gets a distinct
/// operation id.
pub(crate) async fn resolve_loop(
    meetings: &Arc<MeetingSessionManager>,
    loop_id: &MeetingLoopId,
) -> Result<ExternalReceipt, ExternalError> {
    let session_id = loop_id
        .session_id()
        .ok_or_else(|| ExternalError::invalid("That loop id names no meeting."))?;
    let loops = meetings
        .loops_list(session_id)
        .await
        .map_err(command_error(session_id.uuid()))?;
    let row = loops
        .rows
        .iter()
        .find(|row| &row.loop_id == loop_id)
        .ok_or_else(|| {
            ExternalError::new(
                ExternalErrorCode::NotFound,
                format!("No loop {} in this corpus.", loop_id.as_str()),
            )
        })?;
    if !row.is_open() {
        let receipt =
            existing_loop_receipt(meetings, row.resolving_operation_id.as_deref()).await?;
        // That column names whichever operation last closed the row, and a
        // carry writes its own: `write_state_in` stamps it for
        // `LoopChange::Carry` as well as a resolution
        // (`meeting/store/loops.rs`). Replaying a carry receipt would answer
        // `command: loop_carry`, `result: committed` for a subject the carry
        // left live in the next occurrence, so only a resolution replays.
        if receipt.command != MeetingCommandKind::LoopResolve {
            return Err(unresolved_close(row));
        }
        return Ok(ExternalReceipt {
            schema_version: QUERY_SCHEMA_VERSION,
            receipt,
        });
    }
    let result = meetings
        .loop_resolve_as(
            OperationActor::External,
            MeetingLoopResolveRequest {
                operation_id: external_loop_resolve_operation_id(loop_id, row.revision),
                loop_id: loop_id.clone(),
                expected_revision: row.revision,
                resolution: MeetingLoopResolution::Done,
            },
        )
        .await
        .map_err(command_error(session_id.uuid()))?;
    Ok(ExternalReceipt {
        schema_version: QUERY_SCHEMA_VERSION,
        receipt: result.receipt,
    })
}

/// The refusal for a closed loop that no resolution closed.
///
/// A carried row is the one that matters: the answer an agent needs is the
/// successor's id, which is the loop that is still live.
fn unresolved_close(row: &MeetingLoopRow) -> ExternalError {
    match &row.carried_into_loop_id {
        Some(successor) => ExternalError::invalid(format!(
            "Loop {} was carried into {}. Resolve that one instead.",
            row.loop_id.as_str(),
            successor.as_str()
        )),
        None => ExternalError::invalid(format!(
            "Loop {} is {} and holds no resolution to replay.",
            row.loop_id.as_str(),
            row.status.as_str()
        )),
    }
}

fn external_loop_resolve_operation_id(
    loop_id: &MeetingLoopId,
    open_revision: u64,
) -> MeetingOperationId {
    let mut name = Vec::with_capacity(std::mem::size_of::<u64>() + loop_id.as_str().len());
    name.extend_from_slice(&open_revision.to_be_bytes());
    name.extend_from_slice(loop_id.as_str().as_bytes());
    MeetingOperationId::from_uuid(Uuid::new_v5(
        &EXTERNAL_LOOP_RESOLVE_OPERATION_NAMESPACE,
        &name,
    ))
}

async fn existing_loop_receipt(
    meetings: &Arc<MeetingSessionManager>,
    operation_id: Option<&str>,
) -> Result<OperationReceipt, ExternalError> {
    let operation_id = operation_id.ok_or_else(|| {
        ExternalError::new(
            ExternalErrorCode::Failed,
            "The loop state has no receipt to replay.",
        )
    })?;
    let operation_id =
        MeetingOperationId::from_uuid(Uuid::parse_str(operation_id).map_err(|_| {
            ExternalError::new(
                ExternalErrorCode::Failed,
                "The loop state has an unreadable receipt id.",
            )
        })?);
    let store = store(meetings).await?;
    store
        .operation_receipt(operation_id)
        .map_err(|error| ExternalError::from(QueryError::from(error)))?
        .ok_or_else(|| {
            ExternalError::new(
                ExternalErrorCode::Failed,
                "The loop state points to a missing receipt.",
            )
        })
}

/// A meeting command's refusal, as this surface reports it. `NotFound` is the
/// one that means something specific to a caller holding an id.
fn command_error(session: Uuid) -> impl Fn(MeetingCommandError) -> ExternalError {
    move |error| match error {
        MeetingCommandError::NotFound => ExternalError::new(
            ExternalErrorCode::NotFound,
            format!("No meeting {session} in this corpus."),
        ),
        error => QueryError::from(error).into(),
    }
}

/// One page of retained meetings, newest first.
///
/// `pub(crate)` for the reason [`super::assemble`] is: everything above it is
/// state lookup and `await`, and this is the half a fixture store can
/// exercise. The four projections below it are the same.
pub(crate) fn meetings_page(
    store: &MeetingStore,
    since_utc_ms: Option<i64>,
    before_utc_ms: Option<i64>,
    limit: usize,
) -> Result<ExternalMeetingsPage, ExternalError> {
    let page = store
        .list_sessions(before_utc_ms, limit, &MeetingListFilter::default())
        .map_err(QueryError::from)?;
    // The store's cursor is the upper bound, so `--to` is already applied. The
    // lower bound is a suffix of a newest-first page, which is why it can be
    // cut here rather than becoming a second list query nobody else needs.
    let mut entries = page.entries;
    let mut has_more = page.has_more;
    if let Some(since) = since_utc_ms {
        let kept = entries
            .iter()
            .take_while(|entry| entry.created_at_utc_ms >= since)
            .count();
        if kept < entries.len() {
            has_more = false;
        }
        entries.truncate(kept);
    }
    Ok(ExternalMeetingsPage {
        schema_version: QUERY_SCHEMA_VERSION,
        entries: entries.into_iter().map(meeting_row).collect(),
        has_more,
    })
}

fn meeting_row(summary: MeetingHistorySummary) -> ExternalMeetingRow {
    ExternalMeetingRow {
        id: summary.session_id.uuid(),
        title: summary.title,
        phase: summary.phase,
        when_utc_ms: summary.created_at_utc_ms,
        recorded_duration_ms: summary.recorded_duration_ms,
        capture_completeness: summary.capture_completeness,
        speakers: summary.speaker_labels,
        headline: summary.headline,
        link: meeting_link(summary.session_id),
    }
}

pub(crate) fn meeting_detail(
    store: &MeetingStore,
    session_id: MeetingSessionId,
) -> Result<ExternalMeetingDetail, ExternalError> {
    let snapshot = review(store, session_id)?;
    let artifacts = current_artifacts(&snapshot);
    let loops = store
        .meeting_loops(session_id)
        .map_err(QueryError::from)?
        .rows;
    Ok(ExternalMeetingDetail {
        id: session_id.uuid(),
        title: snapshot.session.title.clone(),
        phase: snapshot.session.phase,
        processing_status: snapshot.session.processing_status,
        started_at_utc_ms: snapshot.session.started_at_utc_ms,
        speakers: speaker_names(&snapshot),
        summary: artifacts
            .map(|content| content.summary.text.trim())
            .filter(|summary| !summary.is_empty())
            .map(str::to_string),
        headline: artifacts
            .and_then(|content| content.headline())
            .map(str::to_string),
        notes: snapshot
            .notes
            .iter()
            .map(|note| note.body.trim().to_string())
            .filter(|body| !body.is_empty())
            .collect(),
        loops: loops
            .into_iter()
            .map(|row| {
                loop_row(
                    row,
                    &snapshot.session.title,
                    snapshot.session.started_at_utc_ms.unwrap_or_default(),
                )
            })
            .collect(),
        link: meeting_link(session_id),
        schema_version: QUERY_SCHEMA_VERSION,
    })
}

pub(crate) fn transcript(
    store: &MeetingStore,
    session_id: MeetingSessionId,
) -> Result<ExternalTranscript, ExternalError> {
    let snapshot = review(store, session_id)?;
    let lines = snapshot
        .transcript
        .iter()
        .filter(|segment| !segment.removed)
        .filter_map(|segment| transcript_line(segment, &snapshot))
        .collect();
    Ok(ExternalTranscript {
        schema_version: QUERY_SCHEMA_VERSION,
        meeting_id: session_id.uuid(),
        title: snapshot.session.title.clone(),
        started_at_utc_ms: snapshot.session.started_at_utc_ms,
        lines,
        link: meeting_link(session_id),
    })
}

/// One transcript line, with the human edit applied and the speaker resolved.
///
/// A segment whose effective text is empty is not a line: the review screen
/// renders nothing for it either, and a reader parsing this stream would count
/// it as a turn that never happened.
pub(super) fn transcript_line(
    segment: &EffectiveTranscriptSegment,
    snapshot: &MeetingReviewSnapshot,
) -> Option<ExternalTranscriptLine> {
    let text = segment
        .replacement_text
        .as_deref()
        .unwrap_or(segment.base.text.as_str())
        .trim();
    if text.is_empty() {
        return None;
    }
    let speaker = snapshot
        .speakers
        .iter()
        .find(|speaker| speaker.speaker_id == segment.assigned_speaker_id)
        .map(|speaker| speaker.display_name.clone())
        .unwrap_or_default();
    Some(ExternalTranscriptLine {
        speaker,
        start_ms: segment.base.start_offset_ns / 1_000_000,
        end_ms: segment.base.end_offset_ns / 1_000_000,
        text: text.to_string(),
    })
}

pub(crate) fn loops_page(
    store: &MeetingStore,
    status: Option<ExternalLoopStatus>,
    side: Option<ExternalLoopSide>,
    after_id: Option<&str>,
    limit: usize,
) -> Result<ExternalLoopsPage, ExternalError> {
    let mut entries = Vec::new();
    let mut found_cursor = after_id.is_none();
    let mut last_scanned_id = None;
    let mut next_cursor = None;
    let mut scanned = 0usize;
    let corpus = store.corpus_loops().map_err(QueryError::from)?;
    'corpus: for meeting in corpus.meetings {
        for row in meeting.rows {
            if !found_cursor {
                found_cursor = after_id == Some(row.loop_id.as_str());
                continue;
            }
            if scanned == LOOP_SCAN_DEPTH {
                next_cursor = last_scanned_id;
                break 'corpus;
            }
            let keeps = keeps_status(status, row.status) && keeps_side(side, row.direction);
            if keeps && entries.len() == limit {
                next_cursor = last_scanned_id;
                break 'corpus;
            }
            scanned += 1;
            last_scanned_id = Some(row.loop_id.as_str().to_owned());
            if keeps {
                entries.push(loop_row(row, &meeting.title, meeting.at_utc_ms));
            }
        }
    }
    if !found_cursor {
        return Err(ExternalError::new(
            ExternalErrorCode::NotFound,
            "That loop id is no longer in the corpus. Start again without --after.",
        ));
    }
    Ok(ExternalLoopsPage {
        schema_version: QUERY_SCHEMA_VERSION,
        entries,
        has_more: next_cursor.is_some(),
        next_cursor,
    })
}

const fn keeps_status(filter: Option<ExternalLoopStatus>, status: MeetingLoopStatus) -> bool {
    match filter {
        None => true,
        Some(ExternalLoopStatus::Open) => status.is_open(),
        Some(ExternalLoopStatus::Done) => matches!(status, MeetingLoopStatus::Done),
    }
}

const fn keeps_side(filter: Option<ExternalLoopSide>, direction: MeetingLoopDirection) -> bool {
    match filter {
        None => true,
        Some(ExternalLoopSide::Mine) => matches!(direction, MeetingLoopDirection::Mine),
        Some(ExternalLoopSide::WaitingOn) => matches!(direction, MeetingLoopDirection::WaitingOn),
    }
}

fn loop_row(row: MeetingLoopRow, meeting_title: &str, when_utc_ms: i64) -> ExternalLoopRow {
    ExternalLoopRow {
        link: loop_link(&row.loop_id),
        id: row.loop_id.as_str().to_string(),
        meeting_id: row.session_id.uuid(),
        meeting_title: meeting_title.to_string(),
        kind: row.kind,
        status: row.status,
        direction: row.direction,
        text: row.text,
        owner: row.owner_display_name.or(row.owner_text),
        when_utc_ms,
        resolved_at_utc_ms: row.resolved_at_utc_ms,
    }
}

pub(crate) fn people_page(
    store: &MeetingStore,
    name: &str,
    limit: usize,
) -> Result<ExternalPeoplePage, ExternalError> {
    let tokens = super::tokens(name);
    if tokens.is_empty() {
        return Err(ExternalError::invalid("--people needs a name to look up."));
    }
    let mut entries = Vec::new();
    let mut has_more = false;
    for entry in store.people_list().map_err(QueryError::from)?.entries {
        if !super::matches_every_token(&super::person_haystack(&entry), &tokens) {
            continue;
        }
        if entries.len() == limit {
            has_more = true;
            break;
        }
        entries.push(person_row(entry));
    }
    Ok(ExternalPeoplePage {
        schema_version: QUERY_SCHEMA_VERSION,
        entries,
        has_more,
    })
}

fn person_row(entry: PersonListEntry) -> ExternalPersonRow {
    let (last_meeting_title, last_meeting_headline) = match entry.last_meeting {
        None => (None, None),
        Some(meeting) => (
            Some(meeting.title),
            meeting.headline.map(|headline| match headline {
                PersonMeetingHeadline::Ledger { text }
                | PersonMeetingHeadline::Summary { text } => text,
            }),
        ),
    };
    ExternalPersonRow {
        id: entry.person.id.uuid(),
        display_name: entry.person.display_name,
        aliases: entry.person.aliases,
        calendar_emails: entry.person.calendar_emails,
        meetings_count: entry.meetings_count,
        last_meeting_at_utc_ms: entry.last_meeting_at_utc_ms,
        last_meeting_title,
        last_meeting_headline,
        link: person_link(entry.person.id),
    }
}

fn review(
    store: &MeetingStore,
    session_id: MeetingSessionId,
) -> Result<MeetingReviewSnapshot, ExternalError> {
    store
        .review_snapshot(session_id)
        .map_err(|error| match error {
            crate::meeting::store::StoreError::NotFound => ExternalError::new(
                ExternalErrorCode::NotFound,
                format!("No meeting {} in this corpus.", session_id.uuid()),
            ),
            error => QueryError::from(error).into(),
        })
}

/// The generated artifacts a reader is looking at: the current revision's, or
/// none while processing has not produced one.
pub(super) fn current_artifacts(
    snapshot: &MeetingReviewSnapshot,
) -> Option<&crate::meeting::types::GeneratedMeetingArtifacts> {
    snapshot
        .artifacts
        .iter()
        .find(|revision| revision.state == crate::meeting::types::MeetingArtifactState::Current)
        .and_then(|revision| revision.content.as_ref())
}

/// The diarized speaker labels of a meeting, in the order the store assigned
/// them. Empty before diarization has named anybody.
pub(super) fn speaker_names(snapshot: &MeetingReviewSnapshot) -> Vec<String> {
    snapshot
        .speakers
        .iter()
        .map(|speaker| speaker.display_name.clone())
        .collect()
}

/// Run one external request for the headless CLI, and report a process exit
/// code.
///
/// Stdout carries exactly one JSON value on success; stderr carries exactly
/// one JSON object on failure. Nothing else is printed on either, which is
/// what makes `sona --query … | jq` work while the app's own log lines are
/// going to stderr beside it.
pub fn run_cli(app: &tauri::AppHandle, args: &CliArgs) -> i32 {
    use tauri::Manager;

    let outcome = ExternalRequest::from_args(args)
        .and_then(|request| {
            AllowedRequest::new(
                ExternalConsent::from_settings(&crate::settings::get_settings(app)),
                request,
            )
        })
        .and_then(|allowed| {
            let meetings = app.state::<Arc<MeetingSessionManager>>();
            let history = app.state::<Arc<HistoryManager>>();
            let calendar = crate::meeting::detection::calendar::platform_calendar();
            tauri::async_runtime::block_on(answer(&allowed, &meetings, &history, calendar.as_ref()))
        });
    let printed = match outcome {
        Ok(response) => serde_json::to_string(&response).map(|json| {
            println!("{json}");
            response.exit_code()
        }),
        Err(error) => serde_json::to_string(&error).map(|json| {
            eprintln!("{json}");
            error.error.exit_code()
        }),
    };
    printed.unwrap_or_else(|error| {
        eprintln!(
            r#"{{"schema_version":{QUERY_SCHEMA_VERSION},"error":"failed","message":"The answer could not be serialized: {error}"}}"#
        );
        1
    })
}

#[cfg(test)]
mod tests {
    //! What the flags mean, and what happens when consent is off.
    //!
    //! Everything here is the half of the surface that needs no corpus: which
    //! request a command line names, which combinations clap refuses, and the
    //! exact refusal an outside agent gets. The store-backed half — projections
    //! over real corpus rows and loop resolution through a real manager — lives
    //! in `meeting/store/external_tests.rs`, where the encrypted-store fixture
    //! is.

    use super::*;
    use crate::meeting::types::{
        CaptureCompleteness, HistoryItemKind, MeetingHistoryHeadline, MeetingHistorySummary,
        MeetingPhase, ProcessingStatus,
    };
    use clap::{CommandFactory, Parser};

    const MEETING: &str = "1e1a5f0e-0000-4000-8000-000000000001";

    /// The loop id `--loop-resolve` is given in the tests below: any meeting
    /// uuid plus a content key, which is all this surface checks.
    const LOOP: &str = "1e1a5f0e-0000-4000-8000-000000000001:loop:0123456789abcdef";

    fn parse(argv: &[&str]) -> CliArgs {
        let mut command = vec!["sona"];
        command.extend_from_slice(argv);
        CliArgs::try_parse_from(command).expect("these flags parse")
    }

    fn request(argv: &[&str]) -> ExternalRequest {
        ExternalRequest::from_args(&parse(argv)).expect("these flags name a request")
    }

    fn refusal(argv: &[&str]) -> ExternalError {
        ExternalRequest::from_args(&parse(argv)).expect_err("these flags are refused")
    }

    /// The consent as the settings rows leave it: reads on, mutations on or
    /// off. Built from the real defaults so the test cannot disagree with the
    /// switches about what a fresh install allows.
    fn consent(reads: bool, mutations: bool) -> ExternalConsent {
        let mut settings = crate::settings::get_default_settings();
        settings.external_query_enabled = reads;
        settings.external_mutations_enabled = mutations;
        ExternalConsent::from_settings(&settings)
    }

    #[test]
    fn a_partial_capture_duration_is_published_as_partial() {
        let row = meeting_row(MeetingHistorySummary {
            kind: HistoryItemKind::Meeting,
            session_id: MeetingSessionId::from_uuid(Uuid::nil()),
            title: "Interrupted call".to_string(),
            phase: MeetingPhase::ReviewReady,
            created_at_utc_ms: 1_700_000_000_000,
            capture_completeness: CaptureCompleteness::Partial,
            processing_status: ProcessingStatus::Succeeded,
            recorded_duration_ms: Some(18_453),
            sources: Vec::new(),
            speaker_labels: Vec::new(),
            headline: MeetingHistoryHeadline::None,
        });

        let value = serde_json::to_value(row).expect("meeting row serializes");
        assert_eq!(value["recorded_duration_ms"], 18_453);
        assert_eq!(value["capture_completeness"], "partial");
    }

    /// A headless calendar read cannot speak for the installed GUI: macOS TCC
    /// attributes it to the process responsible for this invocation.
    #[test]
    fn an_upcoming_page_names_its_calendar_access_subject() {
        let page = ExternalUpcomingPage {
            schema_version: QUERY_SCHEMA_VERSION,
            calendar_access_subject: "responsible_process",
            calendar_access: CalendarAccess::Authorized,
            window_start_utc_ms: 1_700_000_000_000,
            window_end_utc_ms: 1_700_000_001_000,
            entries: Vec::new(),
            has_more: false,
        };

        let value = serde_json::to_value(page).expect("upcoming page serializes");
        assert_eq!(value["calendar_access_subject"], "responsible_process");
    }

    #[test]
    fn every_verb_names_its_own_request() {
        assert_eq!(
            request(&["--query", "dana"]),
            ExternalRequest::Search {
                scope: QueryScope::All,
                query: "dana".to_string(),
                limit: DEFAULT_LIMIT,
            }
        );
        assert_eq!(
            request(&["--meetings"]),
            ExternalRequest::Meetings {
                since_utc_ms: None,
                before_utc_ms: None,
                limit: DEFAULT_LIMIT,
            }
        );
        assert_eq!(
            request(&["--meeting", MEETING]),
            ExternalRequest::Meeting {
                session_id: MeetingSessionId::from_uuid(Uuid::parse_str(MEETING).unwrap()),
            }
        );
        assert_eq!(
            request(&["--transcript", MEETING]),
            ExternalRequest::Transcript {
                session_id: MeetingSessionId::from_uuid(Uuid::parse_str(MEETING).unwrap()),
            }
        );
        assert_eq!(
            request(&["--loops"]),
            ExternalRequest::Loops {
                status: None,
                side: None,
                after_id: None,
                limit: DEFAULT_LIMIT,
            }
        );
        assert_eq!(
            request(&["--people", "Dana Reyes"]),
            ExternalRequest::People {
                name: "Dana Reyes".to_string(),
                limit: DEFAULT_LIMIT,
            }
        );
        assert_eq!(
            request(&["--events"]),
            ExternalRequest::Events {
                after_id: None,
                limit: DEFAULT_LIMIT,
            }
        );
        assert_eq!(
            request(&["--upcoming"]),
            ExternalRequest::Upcoming {
                limit: DEFAULT_LIMIT,
            }
        );
        assert_eq!(
            request(&["--loop-resolve", LOOP]),
            ExternalRequest::LoopResolve {
                loop_id: MeetingLoopId(LOOP.to_string()),
            }
        );
    }

    #[test]
    fn modifiers_narrow_the_verb_they_belong_to() {
        assert_eq!(
            request(&["--query", "dana", "--scope", "people", "--limit", "5"]),
            ExternalRequest::Search {
                scope: QueryScope::People,
                query: "dana".to_string(),
                limit: 5,
            }
        );
        assert_eq!(
            request(&["--meetings", "--last", "3"]),
            ExternalRequest::Meetings {
                since_utc_ms: None,
                before_utc_ms: None,
                limit: 3,
            }
        );
        assert_eq!(
            request(&["--loops", "--status", "done", "--waiting"]),
            ExternalRequest::Loops {
                status: Some(ExternalLoopStatus::Done),
                side: Some(ExternalLoopSide::WaitingOn),
                after_id: None,
                limit: DEFAULT_LIMIT,
            }
        );
        assert_eq!(
            request(&["--loops", "--mine"]),
            ExternalRequest::Loops {
                status: None,
                side: Some(ExternalLoopSide::Mine),
                after_id: None,
                limit: DEFAULT_LIMIT,
            }
        );
        assert_eq!(
            request(&["--loops", "--after", "abc", "--limit", "2"]),
            ExternalRequest::Loops {
                status: None,
                side: None,
                after_id: Some("abc".to_string()),
                limit: 2,
            }
        );
        assert_eq!(
            request(&["--events", "--after", "abc", "--limit", "2"]),
            ExternalRequest::Events {
                after_id: Some("abc".to_string()),
                limit: 2,
            }
        );
    }

    /// `--to` names a day the reader wants included, so the window it opens is
    /// a whole civil day wide rather than ending at its midnight.
    #[test]
    fn a_one_day_window_includes_the_day_it_names() {
        let ExternalRequest::Meetings {
            since_utc_ms,
            before_utc_ms,
            ..
        } = request(&["--meetings", "--from", "2026-06-10", "--to", "2026-06-10"])
        else {
            panic!("--meetings names a meetings read");
        };

        assert_eq!(
            before_utc_ms.unwrap() - since_utc_ms.unwrap(),
            24 * 60 * 60 * 1_000,
            "one local day, midnight to midnight"
        );
    }

    #[test]
    fn a_page_is_bounded_and_a_zero_page_is_refused() {
        assert_eq!(
            request(&["--query", "dana", "--limit", "5000"]),
            ExternalRequest::Search {
                scope: QueryScope::All,
                query: "dana".to_string(),
                limit: MAX_LIMIT,
            }
        );
        for argv in [
            vec!["--query", "dana", "--limit", "0"],
            vec!["--meetings", "--last", "0"],
        ] {
            assert_eq!(
                refusal(&argv).error,
                ExternalErrorCode::InvalidRequest,
                "{argv:?}"
            );
        }
    }

    #[test]
    fn an_id_or_a_date_that_is_not_one_is_a_usage_error() {
        for argv in [
            vec!["--meeting", "not-a-uuid"],
            vec!["--transcript", "not-a-uuid"],
            vec!["--meetings", "--from", "yesterday", "--to", "2026-06-10"],
            vec!["--meetings", "--from", "2026-06-10", "--to", "2026-06-32"],
            vec!["--loop-resolve", "not-a-loop"],
            vec!["--loop-resolve", "loop:0123456789abcdef"],
            vec!["--loop-resolve", MEETING],
        ] {
            let refused = refusal(&argv);
            assert_eq!(refused.error, ExternalErrorCode::InvalidRequest, "{argv:?}");
            assert_eq!(refused.error.exit_code(), 2, "usage errors exit 2");
        }
    }

    /// One verb per invocation, and no modifier that means nothing on its own.
    /// Clap enforces these, so the check is that the groups are wired, not
    /// that clap works.
    #[test]
    fn contradictory_flags_do_not_parse() {
        for argv in [
            vec!["--meetings", "--loops"],
            vec!["--query", "dana", "--events"],
            vec!["--loops", "--mine", "--waiting"],
            vec!["--upcoming", "--loops"],
            vec!["--loop-resolve", LOOP, "--upcoming"],
            vec!["--loop-resolve", LOOP, "--query", "dana"],
            vec!["--scope", "people"],
            vec!["--status", "open"],
            vec!["--after", "abc"],
            vec!["--last", "3"],
            vec!["--meetings", "--last", "3", "--limit", "3"],
            vec!["--meetings", "--from", "2026-06-10"],
        ] {
            let mut command = vec!["sona"];
            command.extend_from_slice(&argv);
            assert!(
                CliArgs::try_parse_from(command).is_err(),
                "{argv:?} must not parse"
            );
        }
    }

    /// A modifier is refused beside a verb that does not read it.
    ///
    /// [`contradictory_flags_do_not_parse`] above asserts the same contract
    /// for a modifier passed *alone*, and clap does enforce that one. It does
    /// not enforce this one: every verb is a member of the `read` `ArgGroup`,
    /// and a clap requirement naming a group member is satisfied by any
    /// member — so `--last`'s `requires = "meetings"` is satisfied by
    /// `--query`, and the `--last` is then silently dropped. The group is
    /// what makes `--meetings --loops` a usage error and is worth keeping, so
    /// the binding is checked in [`ExternalRequest::from_args`] instead, where
    /// the refusal is the JSON object this surface promises rather than
    /// clap's plain-text usage error.
    ///
    /// Measured on the shipped binary before the fix: these combinations
    /// parsed, exited 0, and answered the verb with the modifier ignored.

    #[test]
    fn a_modifier_beside_the_wrong_verb_is_refused() {
        for argv in [
            vec!["--query", "dana", "--last", "3"],
            vec!["--query", "dana", "--status", "open"],
            vec!["--query", "dana", "--mine"],
            vec!["--query", "dana", "--waiting"],
            vec!["--query", "dana", "--after", "abc"],
            vec![
                "--query",
                "dana",
                "--from",
                "2026-06-10",
                "--to",
                "2026-06-11",
            ],
            vec!["--meetings", "--scope", "people"],
            vec!["--transcript", MEETING, "--limit", "3"],
            vec!["--meeting", MEETING, "--limit", "3"],
            vec!["--loop-resolve", LOOP, "--limit", "3"],
        ] {
            let refused = refusal(&argv);
            assert_eq!(refused.error, ExternalErrorCode::InvalidRequest, "{argv:?}");
            assert_eq!(refused.error.exit_code(), 2, "{argv:?} is a usage error");
        }
    }

    /// Every modifier `CliArgs` declares outside the `read` group is bound to
    /// the verb that reads it.
    ///
    /// The cases are checked against clap's own metadata before they run, so a
    /// modifier added to `CliArgs` later cannot reach the corpus unbound: it
    /// either joins the `read` group as a verb of its own, is one of the flags
    /// this surface never reads, or this assertion fails until
    /// [`foreign_modifier`] binds it and a case here proves the binding.
    #[test]
    fn every_modifier_outside_the_read_group_is_bound_to_a_verb() {
        // What is left on `CliArgs` once the verbs and their modifiers are
        // out: the window flags, the transcription CLI, and clap's own two.
        const UNREAD: &[&str] = &[
            "start_hidden",
            "no_tray",
            "toggle_transcription",
            "toggle_post_process",
            "cancel",
            "debug",
            "transcribe_file",
            "model",
            "device_index",
            "list_devices",
            "list_models",
            "agent_panel_public_identity",
            "repeat",
            "json",
            "opened_audio_files",
            "help",
            "version",
        ];
        // One invocation per modifier, beside a verb that does not read it.
        // `--from` and `--to` require each other, so they share theirs.
        let cases: [(&str, &[&str]); 9] = [
            ("scope", &["--upcoming", "--scope", "people"]),
            ("limit", &["--meeting", MEETING, "--limit", "3"]),
            ("last", &["--upcoming", "--last", "3"]),
            (
                "from",
                &["--upcoming", "--from", "2026-06-10", "--to", "2026-06-11"],
            ),
            (
                "to",
                &["--upcoming", "--from", "2026-06-10", "--to", "2026-06-11"],
            ),
            ("status", &["--upcoming", "--status", "open"]),
            ("mine", &["--upcoming", "--mine"]),
            ("waiting", &["--upcoming", "--waiting"]),
            ("after", &["--upcoming", "--after", "abc"]),
        ];

        let command = CliArgs::command();
        let verbs = command
            .get_groups()
            .find(|group| group.get_id().as_str() == "read")
            .expect("the read group names every corpus verb")
            .get_args()
            .map(|id| id.as_str().to_string())
            .collect::<Vec<_>>();
        let declared = command
            .get_arguments()
            .map(|argument| argument.get_id().as_str().to_string())
            .filter(|id| !verbs.contains(id) && !UNREAD.contains(&id.as_str()))
            .collect::<Vec<_>>();
        let covered = cases
            .iter()
            .map(|(id, _)| (*id).to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            declared, covered,
            "a modifier outside the read group is unchecked here"
        );

        for (id, argv) in cases {
            assert_eq!(
                refusal(argv).error,
                ExternalErrorCode::InvalidRequest,
                "--{id} must be refused beside a verb that does not read it"
            );
        }
    }

    /// The three verbs that do page still read `--limit`, so the guard above
    /// must not turn a working invocation into a refusal. `--meetings` is here
    /// on purpose: its help line says `--limit` is "not for --meetings", and
    /// the code has always honoured it as the fallback for `--last`.
    #[test]
    fn a_modifier_beside_its_own_verb_still_reads() {
        assert_eq!(
            request(&["--meetings", "--limit", "5"]),
            ExternalRequest::Meetings {
                since_utc_ms: None,
                before_utc_ms: None,
                limit: 5,
            }
        );
        assert_eq!(
            request(&["--people", "Dana Reyes", "--limit", "5"]),
            ExternalRequest::People {
                name: "Dana Reyes".to_string(),
                limit: 5,
            }
        );
        assert_eq!(
            request(&["--upcoming", "--limit", "5"]),
            ExternalRequest::Upcoming { limit: 5 }
        );
    }

    #[test]
    fn a_transcription_run_is_not_a_corpus_read() {
        assert!(!is_external_query(&parse(&[
            "--transcribe-file",
            "/tmp/example.wav"
        ])));
        assert!(is_external_query(&parse(&["--loops"])));
    }

    /// The refusal an outside agent gets, exactly as the MCP server passes it
    /// through: a machine token it can branch on, and the settings row a human
    /// has to click.
    #[test]
    fn a_withheld_read_consent_refuses_before_any_read() {
        let refused = AllowedRequest::new(consent(false, false), request(&["--query", "dana"]))
            .expect_err("external access is off");

        assert_eq!(
            serde_json::to_value(&refused).unwrap(),
            serde_json::json!({
                "schema_version": QUERY_SCHEMA_VERSION,
                "error": "consent_required",
                "message": "External access is off. Turn on Settings > Agents > External access in Sona to allow read-only corpus queries.",
                "settings_path": "Settings > Agents > External access",
            })
        );
        assert_eq!(refused.error.exit_code(), 1, "a refusal is not a typo");
    }

    #[test]
    fn a_granted_consent_carries_the_read_through() {
        let allowed = AllowedRequest::new(consent(true, false), request(&["--loops", "--mine"]))
            .expect("external access is on");

        assert_eq!(
            allowed.request(),
            &ExternalRequest::Loops {
                status: None,
                side: Some(ExternalLoopSide::Mine),
                after_id: None,
                limit: DEFAULT_LIMIT,
            }
        );
    }

    /// The resolver reads the loop state it fences, so each consent row must be
    /// on before the write can reach the corpus.
    #[test]
    fn resolving_a_loop_requires_read_and_mutation_consent() {
        for (grants, setting_path) in [
            (consent(false, true), EXTERNAL_ACCESS_SETTING_PATH),
            (consent(true, false), EXTERNAL_MUTATIONS_SETTING_PATH),
        ] {
            let refused = AllowedRequest::new(grants, request(&["--loop-resolve", LOOP]))
                .expect_err("one missing grant refuses the loop resolution");

            assert_eq!(refused.error, ExternalErrorCode::ConsentRequired);
            assert_eq!(refused.settings_path, Some(setting_path));
        }
        assert!(
            AllowedRequest::new(consent(true, true), request(&["--loop-resolve", LOOP])).is_ok()
        );
    }

    /// And the other direction: the mutation row alone opens nothing to read.
    #[test]
    fn changing_the_corpus_does_not_grant_reading_it() {
        let refused = AllowedRequest::new(consent(false, true), request(&["--meetings"]))
            .expect_err("external access is off");

        assert_eq!(refused.settings_path, Some(EXTERNAL_ACCESS_SETTING_PATH));
    }

    /// Every refusal but the consent one is a fault rather than a choice, so
    /// only the consent refusal points at a switch.
    #[test]
    fn only_the_consent_refusal_names_a_settings_row() {
        assert_eq!(
            ExternalError::consent_required(ExternalScope::Read).settings_path,
            Some(EXTERNAL_ACCESS_SETTING_PATH)
        );
        assert_eq!(
            ExternalError::from(QueryError::Unavailable).settings_path,
            None
        );
        assert_eq!(
            ExternalError::from(QueryError::Unavailable).error,
            ExternalErrorCode::Unavailable,
            "a locked keychain is not a missing consent"
        );
    }

    #[test]
    fn a_fresh_install_grants_neither_scope() {
        let settings = crate::settings::get_default_settings();
        let fresh = ExternalConsent::from_settings(&settings);

        assert!(!fresh.allows(ExternalScope::Read));
        assert!(!fresh.allows(ExternalScope::Mutate));
        assert!(consent(true, false).allows(ExternalScope::Read));
        assert!(consent(false, true).allows(ExternalScope::Mutate));
    }

    /// Which grants each verb asks for: every verb reads; loop resolution also writes.
    #[test]
    fn only_loop_resolve_needs_both_scopes() {
        for argv in [
            vec!["--query", "dana"],
            vec!["--meetings"],
            vec!["--meeting", MEETING],
            vec!["--transcript", MEETING],
            vec!["--loops"],
            vec!["--people", "Dana"],
            vec!["--events"],
            vec!["--upcoming"],
        ] {
            assert_eq!(
                request(&argv).required_scopes(),
                &[ExternalScope::Read],
                "{argv:?}",
            );
        }
        assert_eq!(
            request(&["--loop-resolve", LOOP]).required_scopes(),
            &[ExternalScope::Read, ExternalScope::Mutate],
        );
    }
}

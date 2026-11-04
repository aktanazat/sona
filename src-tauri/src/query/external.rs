//! Consent-gated corpus reads and loop resolution for local CLI and MCP clients.
//! JSON is the default; text is an explicit presentation of the same answer.
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
    loop_link, meeting_link, note_link, person_link, QueryError, QueryEventsPage, QueryPageReason,
    QueryScope, QuerySearchPage, QUERY_SCHEMA_VERSION,
};
use crate::analytics::{DashboardTrendRange, DashboardTrendRequest};
use crate::cli::CliArgs;
use crate::managers::history::{HistoryManager, HistoryStats};
use crate::meeting::detection::calendar::{CalendarAccess, CalendarSource};
use crate::meeting::loop_types::{
    MeetingLoopDirection, MeetingLoopId, MeetingLoopKind, MeetingLoopResolution,
    MeetingLoopResolveRequest, MeetingLoopRow, MeetingLoopStatus,
};
use crate::meeting::people_types::{PersonId, PersonListEntry, PersonMeetingHeadline};
use crate::meeting::person_memory::{self, PersonMemoryChange, PersonMemorySnapshot};
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
use crate::planning::{self, EvidenceSourceKind, EvidenceTargetKind, ProjectStatus, TaskStatus};
use crate::scratchpad::links::{ScratchConnection, ScratchSource};
use crate::scratchpad::passages::ScratchPassage;
use crate::scratchpad::{self, ScratchNote, ScratchNoteMemory, ScratchNoteVersion};
use crate::settings::AppSettings;
use chrono::{Local, NaiveDate, TimeZone};
use serde::Serialize;
use std::fmt::Write as _;
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

/// An explicit output choice; existing callers continue to receive JSON.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, clap::ValueEnum)]
pub enum ExternalFormat {
    #[default]
    Json,
    Text,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalPersonMemoryView {
    CatchUp,
    Changes,
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
    PersonMemory {
        person_id: PersonId,
        since_utc_ms: Option<i64>,
        view: ExternalPersonMemoryView,
    },
    Events {
        after_id: Option<String>,
        limit: usize,
    },
    Upcoming {
        limit: usize,
    },
    Projects {
        limit: usize,
    },
    Project {
        id: Uuid,
    },
    Notes {
        limit: usize,
    },
    Note {
        id: String,
    },
    NoteContext {
        query: String,
        limit: usize,
    },
    NoteVersions {
        id: String,
    },
    NoteDiff {
        id: String,
        from_version: i64,
        to_version: Option<i64>,
    },
    Stats,
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
            | Self::PersonMemory { .. }
            | Self::Events { .. }
            | Self::Upcoming { .. }
            | Self::Projects { .. }
            | Self::Project { .. }
            | Self::Notes { .. }
            | Self::Note { .. }
            | Self::NoteContext { .. }
            | Self::NoteVersions { .. }
            | Self::NoteDiff { .. }
            | Self::Stats => READ_SCOPES,
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
        || args.person_catch_up.is_some()
        || args.person_changes.is_some()
        || args.events
        || args.upcoming
        || args.projects
        || args.project.is_some()
        || args.loop_resolve.is_some()
        || args.notes
        || args.note.is_some()
        || args.note_context.is_some()
        || args.note_versions.is_some()
        || args.note_diff.is_some()
        || args.stats
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
        || args.upcoming
        || args.projects
        || args.notes
        || args.note_context.is_some();
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
            "--limit is only read by --query, --meetings, --loops, --people, --events, --upcoming, --projects, --notes or --note-context.",
        ),
        (
            args.from_version.is_some(),
            args.note_diff.is_some(),
            "--from-version is only read by --note-diff.",
        ),
        (
            args.to_version.is_some(),
            args.note_diff.is_some(),
            "--to-version is only read by --note-diff.",
        ),
        (
            args.since_utc_ms.is_some(),
            args.person_catch_up.is_some() || args.person_changes.is_some(),
            "--since-utc-ms is only read by --person-catch-up or --person-changes.",
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
        if let Some((id, view)) = args
            .person_catch_up
            .as_deref()
            .map(|id| (id, ExternalPersonMemoryView::CatchUp))
            .or_else(|| {
                args.person_changes
                    .as_deref()
                    .map(|id| (id, ExternalPersonMemoryView::Changes))
            })
        {
            if view == ExternalPersonMemoryView::Changes && args.since_utc_ms.is_none() {
                return Err(ExternalError::invalid(
                    "--person-changes requires --since-utc-ms.",
                ));
            }
            if args
                .since_utc_ms
                .is_some_and(|since| chrono::DateTime::from_timestamp_millis(since).is_none())
            {
                return Err(ExternalError::invalid(
                    "--since-utc-ms is outside the supported date range.",
                ));
            }
            return Ok(Self::PersonMemory {
                person_id: Uuid::parse_str(id.trim())
                    .map(PersonId::from_uuid)
                    .map_err(|_| ExternalError::invalid(format!("{id:?} is not a person id.")))?,
                since_utc_ms: args.since_utc_ms,
                view,
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
        if args.projects {
            return Ok(Self::Projects { limit });
        }
        if let Some(id) = args.project.as_deref() {
            return Ok(Self::Project {
                id: Uuid::parse_str(id.trim())
                    .map_err(|_| ExternalError::invalid(format!("{id:?} is not a project id.")))?,
            });
        }
        if args.notes {
            return Ok(Self::Notes { limit });
        }
        if let Some(id) = args.note.as_deref() {
            return Ok(Self::Note { id: note_id(id)? });
        }
        if let Some(query) = args.note_context.as_ref() {
            return Ok(Self::NoteContext {
                query: query.clone(),
                limit,
            });
        }
        if let Some(id) = args.note_versions.as_deref() {
            return Ok(Self::NoteVersions { id: note_id(id)? });
        }
        if let Some(id) = args.note_diff.as_deref() {
            let from_version = args.from_version.ok_or_else(|| {
                ExternalError::invalid(
                    "--note-diff requires --from-version. Read --note-versions first.",
                )
            })?;
            if from_version <= 0 || args.to_version.is_some_and(|version| version <= 0) {
                return Err(ExternalError::invalid(
                    "Version ids must be positive integers.",
                ));
            }
            return Ok(Self::NoteDiff {
                id: note_id(id)?,
                from_version,
                to_version: args.to_version,
            });
        }
        if args.stats {
            return Ok(Self::Stats);
        }
        if let Some(value) = args.loop_resolve.as_ref() {
            return Ok(Self::LoopResolve {
                loop_id: loop_id(value)?,
            });
        }
        Err(ExternalError::invalid(
            "No verb was named. Pass --query, --meetings, --meeting, --transcript, --loops, --people, --person-catch-up, --person-changes, --events, --upcoming, --projects, --project, --notes, --note, --note-context, --note-versions, --note-diff, --stats or --loop-resolve.",
        ))
    }
}

fn note_id(value: &str) -> Result<String, ExternalError> {
    Uuid::parse_str(value.trim())
        .map(|id| id.to_string())
        .map_err(|_| ExternalError::invalid(format!("{value:?} is not a note id.")))
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
#[derive(Clone, Debug, PartialEq, Serialize)]
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
    pub context: crate::meeting::context::ConversationContext,
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
    pub raw_text: String,
    pub corrected_text: Option<String>,
    pub segment_id: Uuid,
    pub transcript_revision_id: Uuid,
    pub edit_revision: Option<u64>,
    pub link: String,
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
    /// Why this page reads the way it does: `awaiting_continuity`,
    /// `filtered_out` or `no_rows`.
    pub reason: Option<QueryPageReason>,
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
    /// Why this page reads the way it does: `people_index_empty` or `no_rows`.
    pub reason: Option<QueryPageReason>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalPersonMemory {
    pub schema_version: u32,
    pub view: ExternalPersonMemoryView,
    pub snapshot: PersonMemorySnapshot,
    pub link: String,
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

#[derive(Clone, Debug, Serialize)]
pub struct ExternalProjectRow {
    pub id: String,
    pub title: String,
    pub status: ProjectStatus,
    pub revision: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub open_task_count: i64,
    pub completed_task_count: i64,
    pub next_due_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalProjectsPage {
    pub schema_version: u32,
    pub entries: Vec<ExternalProjectRow>,
    pub has_more: bool,
    pub complete: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalProject {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub status: ProjectStatus,
    pub revision: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalMilestone {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub due_at_ms: Option<i64>,
    pub status: TaskStatus,
    pub revision: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalPlanningTask {
    pub id: String,
    pub project_id: String,
    pub milestone_id: Option<String>,
    pub title: String,
    pub details: Option<String>,
    pub due_at_ms: Option<i64>,
    pub owner_person_id: Option<String>,
    pub owner_is_self: bool,
    pub owner_available: bool,
    pub owner_link: Option<String>,
    pub status: TaskStatus,
    pub revision: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalPlanningEvidence {
    pub id: String,
    pub target_kind: EvidenceTargetKind,
    pub target_id: String,
    pub source_kind: EvidenceSourceKind,
    pub source_id: String,
    pub at_ms: Option<i64>,
    pub revision: i64,
    pub created_at_ms: i64,
    pub available: bool,
    pub title: Option<String>,
    pub link: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalProjectDetail {
    pub schema_version: u32,
    pub project: ExternalProject,
    pub milestones: Vec<ExternalMilestone>,
    pub tasks: Vec<ExternalPlanningTask>,
    pub evidence: Vec<ExternalPlanningEvidence>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalNoteRow {
    pub id: String,
    pub title: String,
    pub pinned: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub link: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalNotesPage {
    pub schema_version: u32,
    pub entries: Vec<ExternalNoteRow>,
    pub has_more: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalNoteDetail {
    pub schema_version: u32,
    pub memory: ScratchNoteMemory,
    pub link: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalNoteContextEntry {
    pub note: ScratchNote,
    pub matched_text: String,
    pub sources: Vec<ScratchSource>,
    pub passages: Vec<ScratchPassage>,
    pub interpretation: bool,
    pub connections: Vec<ScratchConnection>,
    pub link: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalNoteContext {
    pub schema_version: u32,
    pub query: String,
    pub entries: Vec<ExternalNoteContextEntry>,
    pub has_more: bool,
    pub reason: Option<QueryPageReason>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalNoteVersion {
    pub id: i64,
    pub kind: crate::scratchpad::ScratchVersionKind,
    pub saved_at_ms: i64,
    pub words: usize,
    pub bytes: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalNoteVersions {
    pub schema_version: u32,
    pub note_id: String,
    pub entries: Vec<ExternalNoteVersion>,
    pub link: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalDiffKind {
    Context,
    Remove,
    Add,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalDiffLine {
    pub kind: ExternalDiffKind,
    /// Original bytes, including a line ending when one was present.
    pub text: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalNoteDiff {
    pub schema_version: u32,
    pub note_id: String,
    pub from_version: i64,
    pub to_version: i64,
    pub from_saved_at_ms: i64,
    pub to_saved_at_ms: i64,
    pub changed: bool,
    pub from_source: ExternalSavedNoteSource,
    pub to_source: ExternalSavedNoteSource,
    pub removed_lines: usize,
    pub added_lines: usize,
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    pub lines: Vec<ExternalDiffLine>,
    pub link: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalSavedNoteSource {
    pub version_id: i64,
    pub saved_at_ms: i64,
    pub kind: crate::scratchpad::ScratchVersionKind,
    pub link: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ExternalDatasetStats<T> {
    Available { totals: T },
    Unavailable { error: ExternalError },
}

impl<T> ExternalDatasetStats<T> {
    fn from_result(result: Result<T, ExternalError>) -> Self {
        match result {
            Ok(totals) => Self::Available { totals },
            Err(error) => Self::Unavailable { error },
        }
    }

    fn is_available(&self) -> bool {
        matches!(self, Self::Available { .. })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalNoteStats {
    pub notes: u64,
    pub pinned: u64,
    pub saved_versions: u64,
    pub characters: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalStatsEvidence {
    pub method: &'static str,
    pub sample_links: Vec<String>,
    pub has_more: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExternalStatsSources {
    pub meetings: ExternalDatasetStats<ExternalStatsEvidence>,
    pub dictations: ExternalDatasetStats<ExternalStatsEvidence>,
    pub notes: ExternalDatasetStats<ExternalStatsEvidence>,
}
#[derive(Clone, Debug, Serialize)]
pub struct ExternalCorpusStats {
    pub schema_version: u32,
    pub measured_at_utc_ms: i64,
    pub population: &'static str,
    pub complete: bool,
    pub meetings: ExternalDatasetStats<crate::meeting::types::MeetingTrendTotals>,
    pub dictations: ExternalDatasetStats<HistoryStats>,
    pub notes: ExternalDatasetStats<ExternalNoteStats>,
    pub sources: ExternalStatsSources,
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
    PersonMemory(Box<ExternalPersonMemory>),
    Events(QueryEventsPage),
    Upcoming(ExternalUpcomingPage),
    Projects(ExternalProjectsPage),
    Project(ExternalProjectDetail),
    LoopResolve(ExternalReceipt),
    Notes(ExternalNotesPage),
    Note(ExternalNoteDetail),
    NoteContext(ExternalNoteContext),
    NoteVersions(ExternalNoteVersions),
    NoteDiff(ExternalNoteDiff),
    Stats(ExternalCorpusStats),
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
    app: &tauri::AppHandle,
    meetings: &Arc<MeetingSessionManager>,
    history: &Arc<HistoryManager>,
    calendar: &dyn CalendarSource,
) -> Result<ExternalResponse, ExternalError> {
    use tauri::Manager;

    if matches!(
        allowed.request(),
        ExternalRequest::Notes { .. }
            | ExternalRequest::Note { .. }
            | ExternalRequest::NoteVersions { .. }
            | ExternalRequest::PersonMemory { .. }
            | ExternalRequest::NoteContext { .. }
            | ExternalRequest::Projects { .. }
            | ExternalRequest::Project { .. }
            | ExternalRequest::NoteDiff { .. }
            | ExternalRequest::Stats
    ) {
        history.ensure_storage_unlocked().await;
    }
    match allowed.request() {
        ExternalRequest::Search {
            scope,
            query,
            limit,
        } => Ok(ExternalResponse::Search(
            super::search(
                meetings,
                history,
                super::QueryAudience::Local,
                *scope,
                query,
                Some(*limit),
                None,
            )
            .await?,
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
        ExternalRequest::PersonMemory {
            person_id,
            since_utc_ms,
            view,
        } => {
            store(meetings)
                .await?
                .person_detail(*person_id)
                .map_err(|error| match error {
                    crate::meeting::store::StoreError::NotFound => ExternalError::new(
                        ExternalErrorCode::NotFound,
                        format!("No person {} in this corpus.", person_id.uuid()),
                    ),
                    error => ExternalError::from(QueryError::from(error)),
                })?;
            let snapshot = person_memory::read(app, *person_id, *since_utc_ms)
                .await
                .map_err(|message| note_read_error(history, message))?;
            Ok(ExternalResponse::PersonMemory(Box::new(
                ExternalPersonMemory {
                    schema_version: QUERY_SCHEMA_VERSION,
                    view: *view,
                    snapshot,
                    link: person_link(*person_id),
                },
            )))
        }
        ExternalRequest::Upcoming { limit } => Ok(ExternalResponse::Upcoming(
            upcoming_page(meetings, calendar, *limit).await?,
        )),
        ExternalRequest::Projects { limit } => {
            let projects = planning::planning_projects(app.state::<Arc<HistoryManager>>())
                .map_err(|message| project_read_error(history, message))?;
            Ok(ExternalResponse::Projects(projects_page(projects, *limit)))
        }
        ExternalRequest::Project { id } => {
            let detail = planning::planning_project(
                app.state::<Arc<HistoryManager>>(),
                app.state::<Arc<MeetingSessionManager>>(),
                id.to_string(),
            )
            .await
            .map_err(|message| project_read_error(history, message))?;
            Ok(ExternalResponse::Project(project_detail(detail)))
        }
        ExternalRequest::LoopResolve { loop_id } => Ok(ExternalResponse::LoopResolve(
            resolve_loop(meetings, loop_id).await?,
        )),
        ExternalRequest::Notes { limit } => {
            Ok(ExternalResponse::Notes(notes_page(history, *limit)?))
        }
        ExternalRequest::Note { id } => {
            let store = store(meetings).await?;
            let memory = scratchpad::read_note_with_sources(history, &store, id)
                .map_err(|message| note_read_error(history, message))?
                .ok_or_else(|| missing_note(id))?;
            Ok(ExternalResponse::Note(ExternalNoteDetail {
                schema_version: QUERY_SCHEMA_VERSION,
                memory,
                link: note_link(id),
            }))
        }
        ExternalRequest::NoteContext { query, limit } => {
            let page = super::search(
                meetings,
                history,
                super::QueryAudience::Local,
                QueryScope::Notes,
                query,
                Some(*limit),
                None,
            )
            .await?;
            let store = store(meetings).await?;
            let mut entries = Vec::with_capacity(page.entries.len());
            for row in page.entries {
                let memory = scratchpad::read_note_with_sources(history, &store, &row.id)
                    .map_err(|message| note_read_error(history, message))?
                    .ok_or_else(|| missing_note(&row.id))?;
                entries.push(ExternalNoteContextEntry {
                    note: memory.note,
                    matched_text: row.snippet,
                    sources: memory.sources,
                    passages: memory.passages,
                    interpretation: memory.interpretation,
                    connections: memory.connections,
                    link: row.link,
                });
            }
            Ok(ExternalResponse::NoteContext(ExternalNoteContext {
                schema_version: QUERY_SCHEMA_VERSION,
                query: query.clone(),
                entries,
                has_more: page.next_cursor.is_some(),
                reason: page.reason,
            }))
        }
        ExternalRequest::NoteVersions { id } => {
            let entries = note_versions(history, id)?
                .into_iter()
                .map(|version| ExternalNoteVersion {
                    id: version.id,
                    kind: version.kind,
                    saved_at_ms: version.saved_at_ms,
                    words: version.body.split_whitespace().count(),
                    bytes: version.body.len(),
                })
                .collect();
            Ok(ExternalResponse::NoteVersions(ExternalNoteVersions {
                schema_version: QUERY_SCHEMA_VERSION,
                note_id: id.clone(),
                entries,
                link: note_link(id),
            }))
        }
        ExternalRequest::NoteDiff {
            id,
            from_version,
            to_version,
        } => {
            let versions = note_versions(history, id)?;
            let from = versions
                .iter()
                .find(|version| version.id == *from_version)
                .ok_or_else(|| missing_version(id, *from_version))?;
            let to = match to_version {
                Some(version_id) => versions
                    .iter()
                    .find(|version| version.id == *version_id)
                    .ok_or_else(|| missing_version(id, *version_id))?,
                None => versions.first().ok_or_else(|| missing_note(id))?,
            };
            Ok(ExternalResponse::NoteDiff(note_diff(id, from, to)))
        }
        ExternalRequest::Stats => Ok(ExternalResponse::Stats(
            corpus_stats(meetings, history).await,
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
        context: store
            .conversation_context(session_id)
            .map_err(QueryError::from)?,
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
        raw_text: segment.base.text.clone(),
        corrected_text: segment.replacement_text.clone(),
        segment_id: segment.base.segment_id.uuid(),
        transcript_revision_id: segment.base.transcript_revision_id.uuid(),
        edit_revision: segment.edit_revision,
        link: format!(
            "{}?atMs={}&segmentId={}",
            meeting_link(snapshot.session.session_id),
            segment.base.start_offset_ns / 1_000_000,
            segment.base.segment_id.uuid(),
        ),
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
    let reason = loops_reason(entries.len(), scanned, corpus.awaiting_continuity);
    Ok(ExternalLoopsPage {
        schema_version: QUERY_SCHEMA_VERSION,
        entries,
        has_more: next_cursor.is_some(),
        next_cursor,
        reason,
    })
}

/// Why a loops page reads the way it does, from the scan that produced it.
///
/// Three different nothings reach the same empty array: a corpus with no
/// commitments in it, one whose rows are all waiting on a continuity pass, and
/// one where the filters excluded every row there was. Only the scan knows
/// which, so it says. The chat tool's `loops` walks the same corpus and
/// answers with the same word.
pub(super) fn loops_reason(
    kept: usize,
    scanned: usize,
    awaiting_continuity: bool,
) -> Option<QueryPageReason> {
    if awaiting_continuity {
        return Some(QueryPageReason::AwaitingContinuity);
    }
    if kept > 0 {
        return None;
    }
    // Every row that was scanned and not kept was excluded by a filter: with
    // none passed, `keeps` is true for all of them.
    if scanned > 0 {
        return Some(QueryPageReason::FilteredOut);
    }
    Some(QueryPageReason::NoRows)
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
    let people = store.people_list().map_err(QueryError::from)?.entries;
    let indexed = people.len();
    for entry in people {
        if !super::matches_every_token(&super::person_haystack(&entry), &tokens) {
            continue;
        }
        if entries.len() == limit {
            has_more = true;
            break;
        }
        entries.push(person_row(entry));
    }
    // A name that matched nobody and a corpus that has nobody are the same
    // empty array, and only the second one means asking again differently is
    // pointless: diarization has not named anybody yet.
    let reason = if indexed == 0 {
        Some(QueryPageReason::PeopleIndexEmpty)
    } else if entries.is_empty() {
        Some(QueryPageReason::NoRows)
    } else {
        None
    };
    Ok(ExternalPeoplePage {
        schema_version: QUERY_SCHEMA_VERSION,
        entries,
        has_more,
        reason,
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

fn missing_note(id: &str) -> ExternalError {
    ExternalError::new(
        ExternalErrorCode::NotFound,
        format!("No note {id} in this corpus."),
    )
}

fn missing_version(id: &str, version: i64) -> ExternalError {
    ExternalError::new(
        ExternalErrorCode::NotFound,
        format!("Note {id} has no retained version {version}. Read --note-versions again."),
    )
}

fn note_read_error(history: &HistoryManager, message: String) -> ExternalError {
    ExternalError::new(
        if history.storage_is_ready() {
            ExternalErrorCode::Failed
        } else {
            ExternalErrorCode::Unavailable
        },
        message,
    )
}

fn project_read_error(history: &HistoryManager, message: String) -> ExternalError {
    let code = match message.as_str() {
        "This item no longer exists." => ExternalErrorCode::NotFound,
        "Conversations are still unlocking." => ExternalErrorCode::Unavailable,
        _ if !history.storage_is_ready() => ExternalErrorCode::Unavailable,
        _ => ExternalErrorCode::Failed,
    };
    ExternalError::new(code, message)
}

fn projects_page(projects: Vec<planning::ProjectSummary>, limit: usize) -> ExternalProjectsPage {
    let limit = limit.min(MAX_LIMIT);
    let has_more = projects.len() > limit;
    ExternalProjectsPage {
        schema_version: QUERY_SCHEMA_VERSION,
        entries: projects
            .into_iter()
            .take(limit)
            .map(|summary| ExternalProjectRow {
                id: summary.project.id,
                title: summary.project.title,
                status: summary.project.status,
                revision: summary.project.revision,
                created_at_ms: summary.project.created_at_ms,
                updated_at_ms: summary.project.updated_at_ms,
                open_task_count: summary.open_task_count,
                completed_task_count: summary.completed_task_count,
                next_due_at_ms: summary.next_due_at_ms,
            })
            .collect(),
        has_more,
        complete: !has_more,
    }
}

fn project_detail(detail: planning::ProjectDetail) -> ExternalProjectDetail {
    ExternalProjectDetail {
        schema_version: QUERY_SCHEMA_VERSION,
        project: ExternalProject {
            id: detail.project.id,
            title: detail.project.title,
            description: detail.project.description,
            status: detail.project.status,
            revision: detail.project.revision,
            created_at_ms: detail.project.created_at_ms,
            updated_at_ms: detail.project.updated_at_ms,
        },
        milestones: detail
            .milestones
            .into_iter()
            .map(|milestone| ExternalMilestone {
                id: milestone.id,
                project_id: milestone.project_id,
                title: milestone.title,
                due_at_ms: milestone.due_at_ms,
                status: milestone.status,
                revision: milestone.revision,
                created_at_ms: milestone.created_at_ms,
                updated_at_ms: milestone.updated_at_ms,
            })
            .collect(),
        tasks: detail
            .tasks
            .into_iter()
            .map(|task| ExternalPlanningTask {
                owner_link: if task.owner_available {
                    task.owner_person_id
                        .as_ref()
                        .map(|id| format!("sona://person/{id}"))
                } else {
                    None
                },
                id: task.id,
                project_id: task.project_id,
                milestone_id: task.milestone_id,
                title: task.title,
                details: task.details,
                due_at_ms: task.due_at_ms,
                owner_person_id: task.owner_person_id,
                owner_is_self: task.owner_is_self,
                owner_available: task.owner_available,
                status: task.status,
                revision: task.revision,
                created_at_ms: task.created_at_ms,
                updated_at_ms: task.updated_at_ms,
            })
            .collect(),
        evidence: detail.evidence.into_iter().map(project_evidence).collect(),
    }
}

fn project_evidence(evidence: planning::EvidenceLink) -> ExternalPlanningEvidence {
    let link = evidence.available.then(|| {
        let kind = match evidence.source_kind {
            EvidenceSourceKind::Meeting => "meeting",
            EvidenceSourceKind::Note => "note",
            EvidenceSourceKind::Loop => "loop",
        };
        let mut link = format!("sona://{kind}/{}", evidence.source_id);
        if let (EvidenceSourceKind::Meeting, Some(at_ms)) = (evidence.source_kind, evidence.at_ms) {
            let _ = write!(link, "?atMs={at_ms}");
        }
        link
    });
    ExternalPlanningEvidence {
        id: evidence.id,
        target_kind: evidence.target_kind,
        target_id: evidence.target_id,
        source_kind: evidence.source_kind,
        source_id: evidence.source_id,
        at_ms: evidence.at_ms,
        revision: evidence.revision,
        created_at_ms: evidence.created_at_ms,
        available: evidence.available,
        title: evidence.title,
        link,
    }
}

fn notes_page(history: &HistoryManager, limit: usize) -> Result<ExternalNotesPage, ExternalError> {
    let mut notes = scratchpad::search_notes(history, "", None, limit + 1)
        .map_err(|message| note_read_error(history, message))?;
    let has_more = notes.len() > limit;
    notes.truncate(limit);
    Ok(ExternalNotesPage {
        schema_version: QUERY_SCHEMA_VERSION,
        entries: notes
            .into_iter()
            .map(|note| ExternalNoteRow {
                link: note_link(&note.id),
                id: note.id,
                title: note.title,
                pinned: note.pinned,
                created_at_ms: note.created_at_ms,
                updated_at_ms: note.updated_at_ms,
            })
            .collect(),
        has_more,
    })
}

fn note_versions(
    history: &HistoryManager,
    id: &str,
) -> Result<Vec<ScratchNoteVersion>, ExternalError> {
    scratchpad::read_versions(history, id)
        .map_err(|message| note_read_error(history, message))?
        .ok_or_else(|| missing_note(id))
}

fn note_diff(id: &str, from: &ScratchNoteVersion, to: &ScratchNoteVersion) -> ExternalNoteDiff {
    let old: Vec<&str> = from.body.split_inclusive('\n').collect();
    let new: Vec<&str> = to.body.split_inclusive('\n').collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let old_end = old.len() - suffix;
    let new_end = new.len() - suffix;
    let removed_lines = old_end - prefix;
    let added_lines = new_end - prefix;
    let changed = from.body != to.body;
    let context_start = prefix.saturating_sub(3);
    let context_end = suffix.min(3);
    let mut lines = Vec::new();
    if changed {
        for (kind, range) in [
            (ExternalDiffKind::Context, &old[context_start..prefix]),
            (ExternalDiffKind::Remove, &old[prefix..old_end]),
            (ExternalDiffKind::Add, &new[prefix..new_end]),
            (
                ExternalDiffKind::Context,
                &old[old_end..old_end + context_end],
            ),
        ] {
            lines.extend(range.iter().map(|text| ExternalDiffLine {
                kind,
                text: (*text).to_string(),
            }));
        }
    }
    let old_lines = if changed {
        old_end - context_start + context_end
    } else {
        0
    };
    let new_lines = if changed {
        new_end - context_start + context_end
    } else {
        0
    };
    ExternalNoteDiff {
        schema_version: QUERY_SCHEMA_VERSION,
        note_id: id.to_string(),
        from_version: from.id,
        to_version: to.id,
        from_saved_at_ms: from.saved_at_ms,
        to_saved_at_ms: to.saved_at_ms,
        from_source: saved_note_source(id, from),
        to_source: saved_note_source(id, to),
        changed,
        removed_lines,
        added_lines,
        old_start: if old_lines == 0 {
            context_start
        } else {
            context_start + 1
        },
        old_lines,
        new_start: if new_lines == 0 {
            context_start
        } else {
            context_start + 1
        },
        new_lines,
        lines,
        link: note_link(id),
    }
}

fn saved_note_source(id: &str, version: &ScratchNoteVersion) -> ExternalSavedNoteSource {
    ExternalSavedNoteSource {
        version_id: version.id,
        saved_at_ms: version.saved_at_ms,
        kind: version.kind,
        link: format!("{}?version={}", note_link(id), version.id),
    }
}

async fn statistics_sources(
    meetings: &Arc<MeetingSessionManager>,
    history: &HistoryManager,
) -> ExternalStatsSources {
    let meeting_sources = match store(meetings).await {
        Ok(store) => meetings_page(&store, None, None, DEFAULT_LIMIT).map(|page| ExternalStatsEvidence {
            method: "All retained meetings; verified audio intervals merge overlapping sources. Links sample the newest retained meetings, not every counted record.",
            sample_links: page.entries.into_iter().map(|row| row.link).collect(),
            has_more: page.has_more,
        }),
        Err(error) => Err(error),
    };
    let dictation_sources = history.with_connection(|connection| {
        let mut statement = connection.prepare(
            "SELECT id FROM transcription_history ORDER BY id DESC LIMIT ?1",
        )?;
        let ids = statement.query_map([DEFAULT_LIMIT + 1], |row| row.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    }).map(|mut ids| {
        let has_more = ids.len() > DEFAULT_LIMIT;
        ids.truncate(DEFAULT_LIMIT);
        ExternalStatsEvidence {
            method: "All retained dictation history rows, grouped by recorded source kind. Links sample the newest row ids, not every counted record.",
            sample_links: ids.into_iter().map(super::dictation_link).collect(),
            has_more,
        }
    }).map_err(|_| note_read_error(history, "Dictation source links could not be read.".to_string()));
    let note_sources = notes_page(history, DEFAULT_LIMIT).map(|page| ExternalStatsEvidence {
        method: "All retained notes and saved versions; characters count current bodies only. Links sample the most recently updated notes, not every counted record.",
        sample_links: page.entries.into_iter().map(|row| row.link).collect(),
        has_more: page.has_more,
    });
    ExternalStatsSources {
        meetings: ExternalDatasetStats::from_result(meeting_sources),
        dictations: ExternalDatasetStats::from_result(dictation_sources),
        notes: ExternalDatasetStats::from_result(note_sources),
    }
}

async fn corpus_stats(
    meetings: &Arc<MeetingSessionManager>,
    history: &HistoryManager,
) -> ExternalCorpusStats {
    let measured_at = Local::now();
    let meeting_totals = match store(meetings).await {
        Ok(store) => store
            .trend_projection_at(
                DashboardTrendRequest {
                    range: DashboardTrendRange::Days7,
                },
                measured_at,
            )
            .map_err(|error| ExternalError::from(QueryError::from(error)))
            .and_then(|projection| match projection {
                crate::meeting::types::MeetingTrendProjection::Available { all_time, .. } => {
                    Ok(all_time)
                }
                crate::meeting::types::MeetingTrendProjection::Unavailable { .. } => {
                    Err(ExternalError::new(
                        ExternalErrorCode::Unavailable,
                        "Meeting statistics are unavailable.",
                    ))
                }
            }),
        Err(error) => Err(error),
    };
    let dictation_totals = history.get_history_stats().await.map_err(|_| {
        note_read_error(
            history,
            "Dictation statistics could not be read.".to_string(),
        )
    });
    let note_totals = scratchpad::note_statistics(history)
        .map(|totals| ExternalNoteStats {
            notes: totals.notes,
            pinned: totals.pinned,
            saved_versions: totals.saved_versions,
            characters: totals.characters,
        })
        .map_err(|message| note_read_error(history, message));
    let sources = statistics_sources(meetings, history).await;
    let meetings = ExternalDatasetStats::from_result(meeting_totals);
    let dictations = ExternalDatasetStats::from_result(dictation_totals);
    let notes = ExternalDatasetStats::from_result(note_totals);
    ExternalCorpusStats {
        schema_version: QUERY_SCHEMA_VERSION,
        measured_at_utc_ms: measured_at.timestamp_millis(),
        population: "retained_records",
        complete: meetings.is_available()
            && dictations.is_available()
            && notes.is_available()
            && sources.meetings.is_available()
            && sources.dictations.is_available()
            && sources.notes.is_available(),
        meetings,
        dictations,
        notes,
        sources,
    }
}

fn local_time(milliseconds: i64) -> String {
    Local
        .timestamp_millis_opt(milliseconds)
        .single()
        .map(|moment| moment.format("%Y-%m-%d %H:%M %Z").to_string())
        .unwrap_or_else(|| format!("{milliseconds} ms since epoch"))
}

fn label(value: &impl Serialize) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => text,
        Ok(value) => value.to_string(),
        Err(_) => "unknown".to_string(),
    }
}

fn processing_text(status: crate::meeting::types::ProcessingStatus) -> String {
    use crate::meeting::types::ProcessingStatus;
    match status {
        ProcessingStatus::Pending => "pending".to_string(),
        ProcessingStatus::Running => "running".to_string(),
        ProcessingStatus::Succeeded => "succeeded".to_string(),
        ProcessingStatus::Cancelled => "cancelled".to_string(),
        ProcessingStatus::Failed { reason, cause } => {
            let reason = label(&reason).replace('_', " ");
            match cause {
                Some(cause) => format!("failed: {reason} ({})", label(&cause).replace('_', " ")),
                None => format!("failed: {reason}"),
            }
        }
    }
}

fn duration(milliseconds: u64) -> String {
    let seconds = milliseconds / 1_000;
    format!(
        "{}h {:02}m {:02}s",
        seconds / 3_600,
        seconds / 60 % 60,
        seconds % 60
    )
}

fn page_footer(text: &mut String, has_more: bool) {
    if has_more {
        text.push_str("\nmore results exist; narrow the search or raise --limit (maximum 100).\n");
    }
}

fn reason_footer(text: &mut String, reason: Option<QueryPageReason>) {
    if let Some(reason) = reason {
        let explanation = match reason {
            QueryPageReason::NoRows => "no matching records.",
            QueryPageReason::NoSearchableTokens => "the query contains no searchable words.",
            QueryPageReason::SemanticUnavailable => {
                "literal matches only; the semantic recall model is unavailable."
            }
            QueryPageReason::AwaitingContinuity => {
                "some meetings are still being connected; their loops are not included."
            }
            QueryPageReason::FilteredOut => "the filters exclude every available row.",
            QueryPageReason::PeopleIndexEmpty => "no people are indexed yet.",
        };
        let _ = writeln!(text, "\n{explanation}");
    }
}

fn text_response(response: &ExternalResponse) -> String {
    let mut text = String::new();
    match response {
        ExternalResponse::Search(page) => {
            for row in &page.entries {
                let _ = writeln!(
                    text,
                    "{}  [{}]  {}\n{}\n{}\n",
                    row.title,
                    label(&row.kind),
                    local_time(row.when_utc_ms),
                    row.snippet,
                    row.link
                );
            }
            reason_footer(&mut text, page.reason);
            page_footer(&mut text, page.next_cursor.is_some());
        }
        ExternalResponse::Meetings(page) => {
            for row in &page.entries {
                let _ = writeln!(
                    text,
                    "{}  {}\n{}  |  {}  |  {}\n{}\n",
                    local_time(row.when_utc_ms),
                    row.title,
                    label(&row.phase),
                    row.recorded_duration_ms
                        .and_then(|ms| u64::try_from(ms).ok())
                        .map(duration)
                        .unwrap_or_else(|| "duration unknown".to_string()),
                    row.speakers.join(", "),
                    row.link
                );
                match &row.headline {
                    MeetingHistoryHeadline::Ledger { text: headline }
                    | MeetingHistoryHeadline::Summary { text: headline } => {
                        let _ = writeln!(text, "{headline}\n");
                    }
                    MeetingHistoryHeadline::Words { words } => {
                        let _ = writeln!(text, "{words} transcript words\n");
                    }
                    MeetingHistoryHeadline::None => {}
                }
            }
            if page.entries.is_empty() {
                text.push_str("no retained meetings match.\n");
            }
            page_footer(&mut text, page.has_more);
        }
        ExternalResponse::Meeting(meeting) => {
            let _ = writeln!(
                text,
                "{}\n{}\nstate: {}\nprocessing: {}\n",
                meeting.title,
                meeting.link,
                label(&meeting.phase),
                processing_text(meeting.processing_status)
            );
            if !meeting.speakers.is_empty() {
                let _ = writeln!(text, "speakers: {}\n", meeting.speakers.join(", "));
            }
            let context = &meeting.context;
            let _ = writeln!(
                text,
                "conversation: {}\nsource: {}",
                label(&context.kind),
                label(&context.origin)
            );
            if !context.application_bundle_ids.is_empty() {
                let _ = writeln!(
                    text,
                    "applications: {}",
                    context.application_bundle_ids.join(", ")
                );
            }
            if context.captured_microphone {
                text.push_str("captured microphone audio\n");
            }
            if context.captured_system_audio {
                text.push_str("captured computer audio\n");
            }
            if let Some(name) = &context.calendar_name {
                let _ = writeln!(text, "calendar: {name}");
            }
            if let Some(location) = &context.calendar_location {
                let _ = writeln!(text, "calendar location: {location}");
            }
            if let Some(location) = &context.location {
                let _ = writeln!(
                    text,
                    "location: {} ({}, {})",
                    location.name, location.latitude, location.longitude
                );
            }
            text.push('\n');
            if let Some(summary) = &meeting.summary {
                let _ = writeln!(text, "{summary}\n");
            }
            if let Some(headline) = &meeting.headline {
                let _ = writeln!(text, "{headline}\n");
            }
            for note in &meeting.notes {
                let _ = writeln!(text, "- {note}");
            }
            for row in &meeting.loops {
                let _ = writeln!(text, "[{}] {}\n{}", label(&row.status), row.text, row.link);
            }
            if meeting.summary.is_none() {
                text.push_str("\nno generated summary; read --transcript for captured words.\n");
            }
        }
        ExternalResponse::Transcript(transcript) => {
            let _ = writeln!(text, "{}\n{}\n", transcript.title, transcript.link);
            for line in &transcript.lines {
                let seconds = line.start_ms / 1_000;
                let speaker = if line.speaker.is_empty() {
                    "unknown speaker"
                } else {
                    &line.speaker
                };
                let _ = writeln!(
                    text,
                    "[{:02}:{:02}:{:02}] {}: {}",
                    seconds / 3_600,
                    seconds / 60 % 60,
                    seconds % 60,
                    speaker,
                    line.text
                );
                if line.corrected_text.is_some() {
                    let _ = writeln!(
                        text,
                        "  captured: {}\n  corrected: {}\n  {}",
                        line.raw_text,
                        line.corrected_text.as_deref().unwrap_or(&line.text),
                        line.link
                    );
                }
            }
            if transcript.lines.is_empty() {
                text.push_str("no retained transcript lines.\n");
            }
        }
        ExternalResponse::Loops(page) => {
            for row in &page.entries {
                let _ = writeln!(
                    text,
                    "[{}] {}  ({})\n{}  |  {}\n{}\n",
                    label(&row.status),
                    row.text,
                    label(&row.direction),
                    row.meeting_title,
                    row.owner.as_deref().unwrap_or("unassigned"),
                    row.link
                );
            }
            reason_footer(&mut text, page.reason);
            if let Some(cursor) = &page.next_cursor {
                let _ = writeln!(
                    text,
                    "\ncontinue with the same filters and --after {cursor}"
                );
            }
        }
        ExternalResponse::People(page) => {
            for row in &page.entries {
                let _ = writeln!(
                    text,
                    "{}  |  {} meetings\n{}",
                    row.display_name, row.meetings_count, row.link
                );
                if let Some(title) = &row.last_meeting_title {
                    let _ = writeln!(text, "last: {title}");
                }
                if let Some(headline) = &row.last_meeting_headline {
                    let _ = writeln!(text, "{headline}");
                }
                text.push('\n');
            }
            reason_footer(&mut text, page.reason);
            page_footer(&mut text, page.has_more);
        }
        ExternalResponse::PersonMemory(detail) => {
            let snapshot = &detail.snapshot;
            let _ = writeln!(
                text,
                "{}\n{}\nread: {}\n",
                snapshot.display_name,
                detail.link,
                local_time(snapshot.read_at_utc_ms)
            );
            if let Some(since) = snapshot.since_utc_ms {
                let _ = writeln!(text, "comparison boundary: {}\n", local_time(since));
            }
            if detail.view == ExternalPersonMemoryView::CatchUp {
                if let Some(last) = &snapshot.catch_up.last_conversation {
                    let _ = writeln!(
                        text,
                        "last conversation: {}  {}\n{}\n",
                        last.title,
                        local_time(last.at_utc_ms),
                        last.link
                    );
                    write_person_availability(&mut text, last);
                } else {
                    text.push_str("no confirmed retained conversation.\n\n");
                }
                for id in &snapshot.catch_up.contribution_ids {
                    if let Some(entry) = snapshot.contributions.iter().find(|entry| &entry.id == id)
                    {
                        let passage = &entry.passage;
                        let _ = writeln!(
                            text,
                            "{} [{}]: {}\n{}\n",
                            passage.speaker_label,
                            label(&entry.attribution),
                            passage
                                .corrected_text
                                .as_deref()
                                .unwrap_or(&passage.raw_text),
                            passage.source.link
                        );
                        write_person_availability(&mut text, &passage.source);
                        if passage.corrected_text.is_some() {
                            let _ = writeln!(text, "captured: {}\n", passage.raw_text);
                        }
                    }
                }
                for id in &snapshot.catch_up.commitment_ids {
                    if let Some(entry) = snapshot.commitments.iter().find(|entry| &entry.id == id) {
                        use crate::meeting::person_memory::PersonMemoryCommitmentRelation;
                        let relation = match entry.relation {
                            PersonMemoryCommitmentRelation::ExplicitOwner => {
                                "owner explicitly assigned"
                            }
                            PersonMemoryCommitmentRelation::AttributedSpeech => {
                                "in attributed speech; owner not confirmed"
                            }
                            PersonMemoryCommitmentRelation::SharedConversation => {
                                "from shared conversation; owner not confirmed"
                            }
                        };
                        let _ = writeln!(
                            text,
                            "ledger interpretation [{}; {relation}]: {}",
                            label(&entry.status),
                            entry.interpretation
                        );
                        for passage in &entry.evidence {
                            let _ = writeln!(
                                text,
                                "  evidence: {}\n  {}",
                                passage
                                    .corrected_text
                                    .as_deref()
                                    .unwrap_or(&passage.raw_text),
                                passage.source.link
                            );
                            write_person_availability(&mut text, &passage.source);
                        }
                        text.push('\n');
                    }
                }
                for id in &snapshot.catch_up.note_ids {
                    if let Some(note) = snapshot.notes.iter().find(|note| &note.source.id == id) {
                        let _ = writeln!(
                            text,
                            "{}{}:\n{}\n{}\n",
                            if note.includes_ai_synthesis {
                                "note including ai synthesis: "
                            } else {
                                "saved note: "
                            },
                            note.source.title,
                            note.body,
                            note.source.link
                        );
                        write_person_availability(&mut text, &note.source);
                    }
                }
                for id in &snapshot.catch_up.change_ids {
                    if let Some(change) = snapshot.changes.iter().find(|change| &change.id == id) {
                        write_person_change(&mut text, change);
                    }
                }
            } else {
                for change in &snapshot.changes {
                    write_person_change(&mut text, change);
                }
                if snapshot.changes.is_empty() {
                    text.push_str("no comparable retained evidence differs; this does not prove nothing changed.\n");
                }
            }
            for limitation in &snapshot.limitations {
                let _ = writeln!(text, "limitation: {limitation}");
            }
        }
        ExternalResponse::Events(page) => {
            for row in &page.entries {
                let _ = writeln!(
                    text,
                    "{}  {}  [{}]\n{}\n",
                    local_time(row.when_utc_ms),
                    row.action,
                    label(&row.result),
                    row.detail
                );
                if let Some(summary) = &row.outcome_summary {
                    let _ = writeln!(text, "{summary}");
                }
                if let Some(link) = &row.link {
                    let _ = writeln!(text, "{link}");
                }
            }
            if page.entries.is_empty() {
                text.push_str("no events.\n");
            }
            if let Some(cursor) = &page.next_cursor {
                let _ = writeln!(text, "\ncontinue with --events --after {cursor}");
            }
        }
        ExternalResponse::Upcoming(page) => {
            let _ = writeln!(
                text,
                "calendar access for this caller: {}\n",
                label(&page.calendar_access)
            );
            for row in &page.entries {
                let _ = writeln!(
                    text,
                    "{}  {}\n{}\n",
                    local_time(row.start_utc_ms),
                    row.title,
                    row.attendees.join(", ")
                );
                if let Some(url) = &row.join_url {
                    let _ = writeln!(text, "{url}");
                }
            }
            if page.entries.is_empty() {
                text.push_str(if page.calendar_access == CalendarAccess::Authorized {
                    "no calendar events in this window.\n"
                } else { "this caller cannot read the calendar; an empty result does not mean a free week.\n" });
            }
            page_footer(&mut text, page.has_more);
        }
        ExternalResponse::Projects(page) => {
            for row in &page.entries {
                let _ = writeln!(
                    text,
                    "{} [{}]\nid: {}\nrevision: {}\ncreated: {}\nupdated: {}\nopen tasks: {}\ncompleted tasks: {}",
                    row.title,
                    label(&row.status),
                    row.id,
                    row.revision,
                    local_time(row.created_at_ms),
                    local_time(row.updated_at_ms),
                    row.open_task_count,
                    row.completed_task_count
                );
                if let Some(due) = row.next_due_at_ms {
                    let _ = writeln!(
                        text,
                        "next saved task or milestone due: {}",
                        local_time(due)
                    );
                }
                text.push('\n');
            }
            if page.entries.is_empty() {
                text.push_str("no saved projects.\n");
            }
            if !page.complete {
                text.push_str("more saved projects exist; --limit returns at most 100. this list is incomplete.\n");
            }
        }
        ExternalResponse::Project(detail) => {
            let project = &detail.project;
            let _ = writeln!(
                text,
                "{} [{}]\nid: {}\nrevision: {}\ncreated: {}\nupdated: {}\n",
                project.title,
                label(&project.status),
                project.id,
                project.revision,
                local_time(project.created_at_ms),
                local_time(project.updated_at_ms)
            );
            if let Some(description) = &project.description {
                let _ = writeln!(text, "{description}\n");
            }
            for milestone in &detail.milestones {
                let _ = writeln!(
                    text,
                    "milestone: {} [{}]\nid: {}\nproject: {}\nrevision: {}\ncreated: {}\nupdated: {}",
                    milestone.title,
                    label(&milestone.status),
                    milestone.id,
                    milestone.project_id,
                    milestone.revision,
                    local_time(milestone.created_at_ms),
                    local_time(milestone.updated_at_ms)
                );
                if let Some(due) = milestone.due_at_ms {
                    let _ = writeln!(text, "saved due date: {}", local_time(due));
                }
                text.push('\n');
            }
            for task in &detail.tasks {
                let _ = writeln!(
                    text,
                    "task: {} [{}]\nid: {}\nproject: {}\nrevision: {}\ncreated: {}\nupdated: {}",
                    task.title,
                    label(&task.status),
                    task.id,
                    task.project_id,
                    task.revision,
                    local_time(task.created_at_ms),
                    local_time(task.updated_at_ms)
                );
                if let Some(milestone) = &task.milestone_id {
                    let _ = writeln!(text, "milestone: {milestone}");
                }
                if task.owner_is_self {
                    text.push_str("owner: self\n");
                } else if !task.owner_available {
                    text.push_str("owner: unavailable\n");
                } else if let Some(link) = &task.owner_link {
                    let _ = writeln!(text, "owner: {link}");
                } else {
                    text.push_str("owner: unassigned\n");
                }
                if let Some(due) = task.due_at_ms {
                    let _ = writeln!(text, "saved due date: {}", local_time(due));
                }
                if let Some(details) = &task.details {
                    let _ = writeln!(text, "{details}");
                }
                text.push('\n');
            }
            for evidence in &detail.evidence {
                let _ = writeln!(
                    text,
                    "reference: {}\ntarget: {} {}\nrevision: {}\ncreated: {}\nsource: {} {}",
                    evidence.id,
                    label(&evidence.target_kind),
                    evidence.target_id,
                    evidence.revision,
                    local_time(evidence.created_at_ms),
                    label(&evidence.source_kind),
                    evidence.source_id
                );
                if let Some(at_ms) = evidence.at_ms {
                    let _ = writeln!(text, "source offset: {at_ms} ms");
                }
                if evidence.available {
                    if let Some(title) = &evidence.title {
                        let _ = writeln!(text, "{title}");
                    }
                    if let Some(link) = &evidence.link {
                        let _ = writeln!(text, "{link}");
                    }
                } else {
                    text.push_str("source unavailable\n");
                }
                text.push('\n');
            }
        }
        ExternalResponse::LoopResolve(answer) => {
            let _ = writeln!(
                text,
                "loop resolution: {}\nreceipt: {}",
                label(&answer.receipt.result),
                answer.receipt.operation_id.uuid()
            );
            if !answer.receipt.reason_codes.is_empty() {
                let reasons = answer
                    .receipt
                    .reason_codes
                    .iter()
                    .map(label)
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = writeln!(text, "reasons: {reasons}");
            }
        }
        ExternalResponse::Notes(page) => {
            for row in &page.entries {
                let _ = writeln!(
                    text,
                    "{}{}  {}\n{}\n",
                    if row.pinned { "[pinned] " } else { "" },
                    row.title,
                    local_time(row.updated_at_ms),
                    row.link
                );
            }
            if page.entries.is_empty() {
                text.push_str("no retained notes.\n");
            }
            page_footer(&mut text, page.has_more);
        }
        ExternalResponse::Note(detail) => {
            if detail.memory.interpretation {
                text.push_str("ai interpretation; not a transcript quote.\n\n");
            }
            let note = &detail.memory.note;
            let _ = writeln!(
                text,
                "{}\n{}\nupdated: {}\n\n{}\n",
                note.title,
                detail.link,
                local_time(note.updated_at_ms),
                note.body
            );
            write_note_passages(&mut text, &detail.memory.passages);
            if !detail.memory.sources.is_empty() {
                text.push_str("sources:\n");
                for source in &detail.memory.sources {
                    if source.available {
                        let _ = writeln!(text, "- {}\n  {}", source.title, source.link);
                    } else {
                        text.push_str("- unavailable source\n");
                    }
                }
            }
            if !detail.memory.related_notes.is_empty() {
                text.push_str("\nrelated notes:\n");
                for related in &detail.memory.related_notes {
                    let _ = writeln!(text, "- {}\n  {}", related.title, note_link(&related.id));
                    if let Some(connection) = detail
                        .memory
                        .connections
                        .iter()
                        .find(|connection| connection.note_id == related.id)
                    {
                        let _ = writeln!(text, "  {}", connection.reasons.join("; "));
                    }
                }
            }
        }
        ExternalResponse::NoteContext(context) => {
            for entry in &context.entries {
                if entry.interpretation {
                    text.push_str("ai interpretation; not a transcript quote.\n\n");
                }
                let _ = writeln!(
                    text,
                    "{}\n{}\nmatched: {}\n\n{}\n",
                    entry.note.title, entry.link, entry.matched_text, entry.note.body
                );
                write_note_passages(&mut text, &entry.passages);
                for source in &entry.sources {
                    if source.available {
                        let _ = writeln!(text, "source: {}\n{}\n", source.title, source.link);
                    } else {
                        text.push_str("source: unavailable\n\n");
                    }
                }
                for connection in &entry.connections {
                    let _ = writeln!(
                        text,
                        "connected note: {}\n{}\n",
                        note_link(&connection.note_id),
                        connection.reasons.join("; ")
                    );
                }
            }
            reason_footer(&mut text, context.reason);
            page_footer(&mut text, context.has_more);
        }
        ExternalResponse::NoteVersions(versions) => {
            let _ = writeln!(text, "{}\nsaved versions, newest first:\n", versions.link);
            for version in &versions.entries {
                let _ = writeln!(
                    text,
                    "{}  {}  {}  {} words  {} bytes",
                    version.id,
                    local_time(version.saved_at_ms),
                    label(&version.kind),
                    version.words,
                    version.bytes
                );
            }
            text.push_str("\nautosaves within ten minutes may share a version; at most 50 saved versions are retained.\n");
        }
        ExternalResponse::NoteDiff(diff) => {
            let _ = writeln!(
                text,
                "{}\n--- version {} ({})\n+++ version {} ({})",
                diff.link,
                diff.from_version,
                local_time(diff.from_saved_at_ms),
                diff.to_version,
                local_time(diff.to_saved_at_ms)
            );
            let _ = writeln!(
                text,
                "earlier source: {}\nlater source: {}",
                diff.from_source.link, diff.to_source.link
            );
            if diff.changed {
                let _ = writeln!(
                    text,
                    "@@ -{},{} +{},{} @@",
                    diff.old_start, diff.old_lines, diff.new_start, diff.new_lines
                );
                for line in &diff.lines {
                    text.push(match line.kind {
                        ExternalDiffKind::Context => ' ',
                        ExternalDiffKind::Remove => '-',
                        ExternalDiffKind::Add => '+',
                    });
                    text.push_str(&line.text);
                    if !line.text.ends_with('\n') {
                        text.push_str("\n\\ No newline at end of file\n");
                    }
                }
            } else {
                text.push_str("no changes.\n");
            }
        }
        ExternalResponse::Stats(stats) => {
            let _ = writeln!(
                text,
                "retained corpus as of {}\n",
                local_time(stats.measured_at_utc_ms)
            );
            match &stats.meetings {
                ExternalDatasetStats::Available { totals } => {
                    let _ = writeln!(text, "meetings: {}\n  verified audio: {}\n  transcript turns: {}\n  generated action items: {}",
                        totals.meetings, duration(totals.verified_captured_duration_ms), totals.transcript_segments, totals.generated_action_items);
                }
                ExternalDatasetStats::Unavailable { error } => {
                    let _ = writeln!(text, "meetings: unavailable ({})", error.message);
                }
            }
            match &stats.dictations {
                ExternalDatasetStats::Available { totals } => {
                    let _ = writeln!(
                        text,
                        "\ndictations: {}\n  words: {}\n  measured audio: {}",
                        totals.entries,
                        totals.total_words,
                        duration(totals.total_duration_ms)
                    );
                    for source in &totals.by_source {
                        let _ = writeln!(
                            text,
                            "  {}: {} records, {} words",
                            source
                                .source_kind
                                .as_ref()
                                .map(label)
                                .unwrap_or_else(|| "unknown source".to_string()),
                            source.entries,
                            source.total_words
                        );
                    }
                }
                ExternalDatasetStats::Unavailable { error } => {
                    let _ = writeln!(text, "\ndictations: unavailable ({})", error.message);
                }
            }
            match &stats.notes {
                ExternalDatasetStats::Available { totals } => {
                    let _ = writeln!(
                        text,
                        "\nnotes: {}\n  pinned: {}\n  saved versions: {}\n  characters: {}",
                        totals.notes, totals.pinned, totals.saved_versions, totals.characters
                    );
                }
                ExternalDatasetStats::Unavailable { error } => {
                    let _ = writeln!(text, "\nnotes: unavailable ({})", error.message);
                }
            }
            for (name, evidence) in [
                ("meetings", &stats.sources.meetings),
                ("dictations", &stats.sources.dictations),
                ("notes", &stats.sources.notes),
            ] {
                match evidence {
                    ExternalDatasetStats::Available { totals } => {
                        let _ = writeln!(text, "\n{name} evidence: {}", totals.method);
                        for link in &totals.sample_links {
                            let _ = writeln!(text, "  {link}");
                        }
                        if totals.has_more {
                            text.push_str(
                                "  source links are a sample; totals cover all retained records.\n",
                            );
                        }
                    }
                    ExternalDatasetStats::Unavailable { error } => {
                        let _ =
                            writeln!(text, "\n{name} evidence: unavailable ({})", error.message);
                    }
                }
            }
            if !stats.complete {
                text.push_str(
                    "\npartial statistics; unavailable sources are not counted as zero.\n",
                );
            }
        }
    }
    text
}

fn write_note_passages(text: &mut String, passages: &[ScratchPassage]) {
    for passage in passages {
        let _ = writeln!(
            text,
            "saved passage [{}; {}–{} ms]:\n{}\n",
            label(&passage.wording),
            passage.start_ms,
            passage.end_ms,
            passage.link
        );
    }
}

fn write_person_change(text: &mut String, change: &PersonMemoryChange) {
    let _ = writeln!(
        text,
        "evidence differs: {} [{}]",
        change.title,
        label(&change.kind)
    );
    for (label, side) in [("earlier", &change.earlier), ("later", &change.later)] {
        let _ = writeln!(
            text,
            "{label} ({}){}:\n{}\n{}\n",
            local_time(side.source.at_utc_ms),
            if side.includes_ai_interpretation {
                " [includes ai interpretation]"
            } else {
                ""
            },
            side.text,
            side.source.link
        );
        write_person_availability(text, &side.source);
    }
}

fn write_person_availability(text: &mut String, source: &person_memory::PersonMemorySource) {
    if source.transcript_available == Some(false) {
        text.push_str("source transcript unavailable.\n");
    }
    if source.audio_retained == Some(false) {
        text.push_str("source recording unavailable.\n");
    }
}

fn terminal_safe(text: String) -> String {
    let Some((start, _)) = text
        .char_indices()
        .find(|(_, character)| character.is_control() && *character != '\n' && *character != '\t')
    else {
        return text;
    };
    let mut safe = String::with_capacity(text.len());
    safe.push_str(&text[..start]);
    for character in text[start..].chars() {
        if character.is_control() && character != '\n' && character != '\t' {
            let _ = write!(safe, "{}", character.escape_unicode());
        } else {
            safe.push(character);
        }
    }
    safe
}

/// Run one external request for the headless CLI, and report a process exit
/// code.
///
/// Default stdout is one JSON value; `--format text` renders the same result
/// without ANSI or other terminal control sequences. Errors keep their exit codes.
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
            tauri::async_runtime::block_on(answer(
                &allowed,
                app,
                &meetings,
                &history,
                calendar.as_ref(),
            ))
        });
    let format = args.format.unwrap_or_default();
    match outcome {
        Ok(response) => {
            let output = match format {
                ExternalFormat::Json => serde_json::to_string(&response),
                ExternalFormat::Text => Ok(terminal_safe(text_response(&response))),
            };
            match output {
                Ok(output) => {
                    println!("{output}");
                    response.exit_code()
                }
                Err(_) => {
                    eprintln!(
                        r#"{{"schema_version":{QUERY_SCHEMA_VERSION},"error":"failed","message":"The answer could not be serialized."}}"#
                    );
                    1
                }
            }
        }
        Err(error) => {
            match format {
                ExternalFormat::Json => match serde_json::to_string(&error) {
                    Ok(json) => eprintln!("{json}"),
                    Err(_) => eprintln!(
                        r#"{{"schema_version":{QUERY_SCHEMA_VERSION},"error":"failed","message":"The refusal could not be serialized."}}"#
                    ),
                },
                ExternalFormat::Text => {
                    eprintln!("{}: {}", label(&error.error), terminal_safe(error.message))
                }
            }
            error.error.exit_code()
        }
    }
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
            application_bundle_ids: Vec::new(),
            headline: MeetingHistoryHeadline::None,
            ledger_outcomes: None,
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
        // out: the window flags (including the native shell's socket), the
        // transcription CLI, and clap's own two.
        const UNREAD: &[&str] = &[
            "start_hidden",
            "no_tray",
            "native_socket",
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
            "format",
            "opened_audio_files",
            "help",
            "version",
        ];
        // One invocation per modifier, beside a verb that does not read it.
        // `--from` and `--to` require each other, so they share theirs.
        let cases: [(&str, &[&str]); 12] = [
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
            (
                "since_utc_ms",
                &["--upcoming", "--since-utc-ms", "1700000000000"],
            ),
            ("after", &["--upcoming", "--after", "abc"]),
            ("from_version", &["--upcoming", "--from-version", "1"]),
            ("to_version", &["--upcoming", "--to-version", "2"]),
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

        let value = serde_json::to_value(&refused).unwrap();
        assert_eq!(value["schema_version"], QUERY_SCHEMA_VERSION);
        assert_eq!(value["error"], "consent_required");
        assert_eq!(value["settings_path"], EXTERNAL_ACCESS_SETTING_PATH);
        assert!(value["message"].is_string());
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

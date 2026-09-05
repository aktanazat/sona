//! The one query plane.
//!
//! Every noun Sona keeps — a meeting, a dictation, a person, an open loop, a
//! receipt — is reachable through the two commands in this module. ⌘K, the
//! panel agent, the CLI and MCP are meant to be thin clients of it, which is
//! the point: a per-surface search implementation is how the corpus ended up
//! reachable five different ways and searchable one.
//!
//! # What ranks what
//!
//! Each kind is matched by whatever index its store actually has, and nothing
//! here invents a score that spans them:
//!
//! | kind      | membership decided by                                        |
//! |-----------|--------------------------------------------------------------|
//! | meeting   | `meeting_search_fts` (title, segments, notes) via bm25, then the meeting semantic chunk index above the similarity floor |
//! | dictation | `HistoryManager::search_history_entries` — FTS5 first, static-embedding recall for what it misses |
//! | person    | every token present across display name, aliases and calendar addresses |
//! | loop      | every token present in the loop's own words                   |
//!
//! **Relevance decides membership; recency decides order.** This is the rule
//! dictation search already documents (`managers/history.rs`: "one order, not
//! two"), lifted to the plane, and it is what makes [`QueryCursor`] a position
//! rather than a guess: bm25 and cosine live in different number spaces, and
//! normalising them into one page order would need a weight nobody can defend
//! and a cursor that moves when the corpus does. The honest consequence, named
//! rather than hidden: a strong old match sorts below a weak new one, and a
//! query whose first page fills with recent rows will not show an older, more
//! relevant one until the reader pages down.
//!
//! Lexical evidence outranks semantic evidence *for the row*: when both halves
//! find the same meeting, the row is reported once, with the words that
//! literally matched as its snippet.

pub mod card;
pub mod external;
pub mod pack;
pub mod semantic;
#[cfg(test)]
mod tests;
pub mod tools;

use crate::managers::history::semantic::SemanticModel;
use crate::managers::history::{HistoryEntry, HistoryManager};
use crate::meeting::loop_types::MeetingLoopId;
use crate::meeting::people_types::{PersonId, PersonListEntry, PersonMeetingHeadline};
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::store::query_plane::MeetingQueryCandidate;
use crate::meeting::store::query_plane::QueryEventRow;
use crate::meeting::store::{MeetingStore, StoreError};
use crate::meeting::types::{MeetingCommandError, MeetingSessionId, OperationResult};
use crate::meeting::workflow_types::WorkflowId;
use log::error;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::sync::Arc;

/// Wire version of everything in this module, and of the read-only external
/// surface in [`external`] that projects the same nouns out of the same
/// corpus. Bumped whenever a public payload's bytes or interpretation changes.
pub const QUERY_SCHEMA_VERSION: u32 = 3;

const DEFAULT_PAGE_SIZE: usize = 25;
const MAX_PAGE_SIZE: usize = 100;

/// How much of a row's text the plane carries. Every row of every page ends up
/// in a context pack eventually, so this is a token budget, not a layout hint.
const MAX_TITLE_CHARS: usize = 120;
const MAX_SNIPPET_CHARS: usize = 280;

/// How deep the open-loop scan goes. Loops are derived from ledger JSON rather
/// than indexed, so this is a real bound: a corpus with more than this many
/// open loops will not surface the oldest of them through search. It is set
/// well above what a person who closes loops will ever have, and the inbox and
/// the review screen both reach them by meeting regardless.
const LOOP_SCAN_DEPTH: usize = 200;

/// Meetings whose semantic chunks may be built during one search. The push at
/// artifact completion is what normally keeps the index current; this is what
/// covers meetings that finished before the index existed, and it is bounded
/// so a search never pays for a whole corpus.
const INDEX_TOP_UP_PER_SEARCH: usize = 2;

/// What a caller wants searched. `All` is every scope this plane produces.
#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type, clap::ValueEnum,
)]
#[serde(rename_all = "snake_case")]
pub enum QueryScope {
    #[default]
    All,
    Meetings,
    Dictations,
    People,
    Loops,
}

impl QueryScope {
    fn includes(self, kind: QueryRowKind) -> bool {
        match self {
            Self::All => matches!(
                kind,
                QueryRowKind::Meeting
                    | QueryRowKind::Dictation
                    | QueryRowKind::Person
                    | QueryRowKind::Loop
            ),
            Self::Meetings => kind == QueryRowKind::Meeting,
            Self::Dictations => kind == QueryRowKind::Dictation,
            Self::People => kind == QueryRowKind::Person,
            Self::Loops => kind == QueryRowKind::Loop,
        }
    }
}

/// Which noun a row is.
///
/// Declaration order is the tie-break order inside one millisecond, so it is
/// part of the page contract: adding a variant in the middle reorders pages.
///
/// `Series` and `Receipt` are declared because the plane's row union is fixed
/// across the app and both are nouns of this system. Neither is produced by
/// [`search`]: a series has no address of its own yet, and receipts are reached
/// through [`events`], which needs a nullable link that a search row does not
/// have.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Type,
)]
#[serde(rename_all = "snake_case")]
pub enum QueryRowKind {
    Meeting,
    Dictation,
    Person,
    Series,
    Loop,
    Receipt,
}

/// One answer. `link` is always a `sona://` URL this app parses, so an agent
/// can cite it, a human can click it, and a test can assert it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct QueryRow {
    pub kind: QueryRowKind,
    pub id: String,
    pub title: String,
    /// Why this row is in front of you: the text that matched, not a summary.
    pub snippet: String,
    pub when_utc_ms: i64,
    pub link: String,
}

/// Where the next page resumes.
///
/// Every field is a position in the page order — `when_utc_ms` descending,
/// `(kind, id)` breaking ties — except `dictation_id`, which carries the one
/// source that pages by row id rather than by time. Handing back a cursor from
/// a different query is not meaningful and not supported; the page order
/// depends on the query text that produced it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct QueryCursor {
    pub when_utc_ms: i64,
    pub kind: QueryRowKind,
    pub id: String,
    /// The oldest dictation this query has already returned.
    pub dictation_id: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct QuerySearchPage {
    pub schema_version: u32,
    pub entries: Vec<QueryRow>,
    /// Absent when this page is the end of the result.
    pub next_cursor: Option<QueryCursor>,
    /// Why this page reads the way it does. `no_searchable_tokens`,
    /// `semantic_unavailable` or `no_rows` — never the loop and people values,
    /// which no search path produces.
    pub reason: Option<QueryPageReason>,
}

impl QuerySearchPage {
    /// The answer to a query that is nothing but whitespace.
    ///
    /// Dictation search answers a blank query the same way rather than
    /// matching everything, and a plane that handed back the whole corpus for
    /// a stray space would be worse than one that hands back nothing. What it
    /// must not do is look like a corpus with nothing in it, so the page names
    /// which nothing this is.
    fn nothing_searchable() -> Self {
        Self {
            schema_version: QUERY_SCHEMA_VERSION,
            entries: Vec::new(),
            next_cursor: None,
            reason: Some(QueryPageReason::NoSearchableTokens),
        }
    }
}

/// Why a page of this plane reads the way it does, when something other than
/// the corpus decided it.
///
/// `None` means every source the question reaches answered it, and the rows
/// are all there are. `NoRows` is a corpus that legitimately holds nothing for
/// the question. Every other value names something that stopped short of
/// answering it, and outranks `NoRows` on an empty page: "not everywhere was
/// read" is a different fact from "there is nothing".
///
/// One vocabulary across the plane and the external surface, the way
/// [`external::ExternalErrorCode`] is one vocabulary across every verb. Each
/// page documents the values it can produce; none produces all of them.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum QueryPageReason {
    /// Every source this question reaches was read, and nothing matched.
    NoRows,
    /// The query was whitespace, so it named no token for any index to match.
    /// A word that simply appears nowhere is `NoRows` instead: it was looked
    /// for.
    NoSearchableTokens,
    /// Recall by meaning was not available: the embedding model is not on this
    /// machine yet, so only literal-word matching ran. Reported whether or not
    /// rows came back, because a full page of literal matches is still a page
    /// that missed everything phrased differently.
    SemanticUnavailable,
    /// The corpus holds rows of this kind, and the filters passed excluded
    /// every one of them.
    FilteredOut,
    /// Rows exist and are not reportable yet: a meeting's continuity pass has
    /// not succeeded, so its ledger has not been matched against the meeting
    /// before it, and a carried-forward loop would still read as open.
    AwaitingContinuity,
    /// No person has been named in this corpus yet, so there is nothing for a
    /// name to match. Diarization names people; until it has, this index is
    /// empty however many meetings there are.
    PeopleIndexEmpty,
}

/// Which ledger a row came out of.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum QueryEventSource {
    /// A user or system mutation of a meeting, with its `OperationReceipt`.
    OperationReceipt,
    /// One local workflow run: a learning loop, a briefing, a continuity pass.
    WorkflowRun,
}

/// How an event turned out. Receipts commit or are rejected; runs succeed,
/// fail, or decline to do anything.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum QueryEventResult {
    Committed,
    Rejected,
    Ok,
    Failed,
    Skipped,
}

/// One line of "what happened since I last looked".
///
/// `action`, `detail` and `outcome_summary` are machine tokens and
/// store-authored text, never prose written here: this is the backend, and a
/// sentence invented in Rust would be a user-facing string outside the
/// translation catalogue.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct QueryEvent {
    pub id: String,
    pub source: QueryEventSource,
    /// The mutation's command kind, or the workflow's id — both stable
    /// snake_case tokens a client can translate.
    pub action: String,
    pub result: QueryEventResult,
    /// The reason codes a receipt carries, or a run's error when it failed.
    pub detail: String,
    /// The workflow run's store-authored summary. A receipt has none.
    pub outcome_summary: Option<String>,
    pub when_utc_ms: i64,
    /// The `sona://` address of the noun this event touched. Absent for events
    /// that touched no addressable noun — a default retention change, a
    /// corpus-wide mining pass — which are still events worth reporting.
    pub link: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct QueryEventsPage {
    pub schema_version: u32,
    pub entries: Vec<QueryEvent>,
    /// Pass back as `after_id` for the next page.
    pub next_cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum QueryError {
    /// The corpus cannot be opened right now — encrypted storage still locked,
    /// or a store that failed to open this launch.
    Unavailable,
    /// A limit or scope this plane will not answer.
    InvalidRequest,
    /// The cursor names a row that is no longer there. Start again from the
    /// first page rather than guessing where it used to be.
    UnknownCursor,
    Failed,
}

impl From<StoreError> for QueryError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Unavailable | StoreError::EncryptionUnavailable => Self::Unavailable,
            StoreError::Invalid => Self::InvalidRequest,
            _ => Self::Failed,
        }
    }
}

impl From<MeetingCommandError> for QueryError {
    fn from(error: MeetingCommandError) -> Self {
        match error {
            MeetingCommandError::StorageUnavailable => Self::Unavailable,
            MeetingCommandError::InvalidRequest => Self::InvalidRequest,
            _ => Self::Failed,
        }
    }
}

/// A `sona://` noun the shell has no navigation of its own for.
///
/// Meetings already have one — `meeting:navigation-requested`, which the
/// meetings surface listens to and the Overview links reuse — so
/// `sona://meeting/<id>` and `sona://loop/<id>` go through that channel and
/// this carries only the nouns that had no destination at all. The rule, so a
/// third channel never appears: meeting *lifecycle* navigation belongs to the
/// meeting event; opening a query-plane address belongs here.
#[derive(Clone, Debug, Deserialize, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QueryLinkTarget {
    Person {
        person_id: PersonId,
    },
    /// Everybody at one organization. A slug rather than an id: an
    /// organization is derived from calendar domains, not stored as a row.
    Organization {
        slug: String,
    },
    Dictation {
        history_id: i64,
    },
    /// The search surface, with the question the link carried. Empty means the
    /// link named no question, which is what the ⌘K chord does.
    Search {
        query: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct QueryLinkPayload {
    pub event_schema_version: u32,

    pub target: QueryLinkTarget,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
#[serde(transparent)]
pub struct QueryLinkRequestedEvent(pub QueryLinkPayload);

impl tauri_specta::Event for QueryLinkRequestedEvent {
    const NAME: &'static str = "query:link-requested";
}

pub const QUERY_LINK_EVENT: &str = "query:link-requested";

/// `sona://meeting/<id>` — the meeting's detail in Library.
pub fn meeting_link(session_id: MeetingSessionId) -> String {
    format!("sona://meeting/{}", session_id.uuid())
}

/// `sona://person/<id>` — the person's page.
pub fn person_link(person_id: PersonId) -> String {
    format!("sona://person/{}", person_id.uuid())
}

/// `sona://organization/<slug>` — the organization's page.
///
/// The slug is derived here rather than carried, so a caller that holds only
/// the label a person's header shows can build the address without knowing the
/// rule.
pub fn organization_link(organization: &str) -> String {
    format!(
        "sona://organization/{}",
        crate::meeting::people_types::organization_slug(organization)
    )
}

/// `sona://loop/<id>` — the loop's meeting review, at the loop.
pub fn loop_link(loop_id: &MeetingLoopId) -> String {
    format!("sona://loop/{}", loop_id.as_str())
}

/// `sona://dictation/<id>` — the dictation's row in History.
pub fn dictation_link(history_id: i64) -> String {
    format!("sona://dictation/{history_id}")
}

/// `sona://search?q=…` — this plane's own surface, with the question in it.
pub fn search_link(query: &str) -> String {
    // SAFETY: this constant ASCII URL is valid by construction.
    let mut url = url::Url::parse("sona://search").expect("a static url parses");
    url.query_pairs_mut().append_pair("q", query);
    url.to_string()
}

/// One search, as its four sources answer it.
///
/// Separate from [`search`] so the whole of the plane's behaviour can be
/// exercised over a fixture store: everything above it is state lookup and
/// `await`, everything below it is the reads, the dedupe and the order.
pub(crate) struct SearchRequest<'a> {
    pub scope: QueryScope,
    pub query: &'a str,
    pub limit: usize,
    pub cursor: Option<&'a QueryCursor>,
}

/// Search the corpus.
///
/// One page, newest first, deduplicated across every source that matched. See
/// the module header for what decides membership per kind and why the order is
/// recency rather than a blended score.
pub async fn search(
    meetings: &Arc<MeetingSessionManager>,
    history: &Arc<HistoryManager>,
    scope: QueryScope,
    query: &str,
    limit: Option<usize>,
    cursor: Option<QueryCursor>,
) -> Result<QuerySearchPage, QueryError> {
    let limit = page_size(limit)?;
    if tokens(query).is_empty() {
        return Ok(QuerySearchPage::nothing_searchable());
    }
    let store = meetings.store().await?;
    // Dictations are the one source behind an async API, and the only read the
    // assembly below cannot do itself, so it happens here.
    let dictations = if scope.includes(QueryRowKind::Dictation) {
        history
            .search_history_entries(
                query,
                cursor.as_ref().and_then(|cursor| cursor.dictation_id),
                // One more than the page, so the merge can tell a full page
                // from a last one.
                Some(limit + 1),
            )
            .await
            .map_err(|error| {
                error!("Dictation search failed: {error:#}");
                if history.storage_is_ready() {
                    QueryError::Failed
                } else {
                    QueryError::Unavailable
                }
            })?
            .entries
    } else {
        Vec::new()
    };
    let model = history.semantic_model();
    assemble(
        &store,
        model.as_deref(),
        dictations,
        SearchRequest {
            scope,
            query,
            limit,
            cursor: cursor.as_ref(),
        },
    )
}

pub(crate) fn assemble(
    store: &MeetingStore,
    model: Option<&SemanticModel>,
    dictations: Vec<HistoryEntry>,
    request: SearchRequest<'_>,
) -> Result<QuerySearchPage, QueryError> {
    let SearchRequest {
        scope,
        query,
        limit,
        cursor,
    } = request;
    let tokens = tokens(query);
    let fetch = limit + 1;
    let before = cursor.map(|cursor| cursor.when_utc_ms);
    let mut candidates = Vec::new();

    if scope.includes(QueryRowKind::Meeting) {
        for candidate in store.query_meetings_lexical(query, before, fetch)? {
            candidates.push(meeting_row(candidate));
        }
        if let Some(model) = model {
            // Bounded index maintenance for meetings that finished before this
            // index existed, before the scan reads it.
            semantic::top_up_index(store, model, INDEX_TOP_UP_PER_SEARCH);
            // Pushed after the lexical half so the dedupe keeps the literal
            // match's words as the snippet when both halves find one meeting.
            for candidate in semantic::meeting_matches(store, model, query, before, fetch)? {
                candidates.push(meeting_row(candidate));
            }
        }
    }

    for entry in dictations {
        candidates.push(dictation_row(&entry));
    }

    if scope.includes(QueryRowKind::Person) {
        for entry in store.people_list()?.entries {
            if !matches_every_token(&person_haystack(&entry), &tokens) {
                continue;
            }
            let when_utc_ms = entry
                .last_meeting_at_utc_ms
                .unwrap_or(entry.person.created_at_utc_ms);
            if before.is_some_and(|before| when_utc_ms > before) {
                continue;
            }
            let snippet = match entry.last_meeting.as_ref() {
                Some(meeting) => match meeting.headline.as_ref() {
                    Some(PersonMeetingHeadline::Ledger { text })
                    | Some(PersonMeetingHeadline::Summary { text }) => text.clone(),
                    None => meeting.title.clone(),
                },
                None => entry.person.aliases.join(", "),
            };
            candidates.push(QueryRow {
                kind: QueryRowKind::Person,
                id: entry.person.id.uuid().to_string(),
                title: bounded(&entry.person.display_name, MAX_TITLE_CHARS),
                snippet: bounded(&snippet, MAX_SNIPPET_CHARS),
                when_utc_ms,
                link: person_link(entry.person.id),
            });
        }
    }

    if scope.includes(QueryRowKind::Loop) {
        for entry in store.open_loops_inbox(LOOP_SCAN_DEPTH)?.entries {
            if !matches_every_token(&entry.text, &tokens) {
                continue;
            }
            if before.is_some_and(|before| entry.at_utc_ms > before) {
                continue;
            }
            candidates.push(QueryRow {
                kind: QueryRowKind::Loop,
                id: entry.loop_id.as_str().to_string(),
                title: bounded(&entry.text, MAX_TITLE_CHARS),
                // The meeting it was raised in: a loop's own words are its
                // title, so the snippet is where it came from.
                snippet: bounded(&entry.title, MAX_SNIPPET_CHARS),
                when_utc_ms: entry.at_utc_ms,
                link: loop_link(&entry.loop_id),
            });
        }
    }

    let (entries, next_cursor) = merge(candidates, cursor, limit);
    let reason = search_reason(&entries, model, scope);
    Ok(QuerySearchPage {
        schema_version: QUERY_SCHEMA_VERSION,
        entries,
        next_cursor,
        reason,
    })
}

/// Why a search page reads the way it does, decided beside the rows it
/// describes rather than by a caller looking at an empty array afterwards.
///
/// The model is the one source that can be missing without failing: recall by
/// meaning is either loaded or it is not, and the reader is the only one who
/// can decide whether a literal-only answer is good enough. A scope that
/// reaches neither meetings nor dictations never asks it anything, so an empty
/// people or loop page says nothing about it.
fn search_reason(
    entries: &[QueryRow],
    model: Option<&SemanticModel>,
    scope: QueryScope,
) -> Option<QueryPageReason> {
    let recalls_by_meaning =
        scope.includes(QueryRowKind::Meeting) || scope.includes(QueryRowKind::Dictation);
    if model.is_none() && recalls_by_meaning {
        return Some(QueryPageReason::SemanticUnavailable);
    }
    entries.is_empty().then_some(QueryPageReason::NoRows)
}

fn meeting_row(candidate: MeetingQueryCandidate) -> QueryRow {
    QueryRow {
        kind: QueryRowKind::Meeting,
        id: candidate.session_id.uuid().to_string(),
        title: bounded(&candidate.title, MAX_TITLE_CHARS),
        snippet: bounded(&candidate.snippet, MAX_SNIPPET_CHARS),
        when_utc_ms: candidate.when_utc_ms,
        link: meeting_link(candidate.session_id),
    }
}

/// One dictation as the plane reports it: the delivered text is the snippet,
/// and an untitled row is titled by it. Shared with the chat tools, which list
/// dictations by recency rather than by match and must render the same row.
pub(super) fn dictation_row(entry: &HistoryEntry) -> QueryRow {
    let text = entry
        .post_processed_text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(entry.transcription_text.as_str());
    let title = match entry.title.trim() {
        "" => text,
        title => title,
    };
    QueryRow {
        kind: QueryRowKind::Dictation,
        id: entry.id.to_string(),
        title: bounded(title, MAX_TITLE_CHARS),
        snippet: bounded(text, MAX_SNIPPET_CHARS),
        // History keeps UNIX seconds; the plane's order is milliseconds.
        when_utc_ms: entry.timestamp * 1000,
        link: dictation_link(entry.id),
    }
}

/// What happened, newest first: meeting operation receipts and local workflow
/// runs in one stream.
///
/// `after_id` is the id of the last event the caller has already seen, so
/// "catch me up" is one call with the id from last time and no clock
/// arithmetic. An id the corpus no longer holds — a receipt goes with the
/// meeting it described — is [`QueryError::UnknownCursor`] rather than a
/// silent restart from the top, which would replay the whole ledger as new.
pub async fn events(
    meetings: &Arc<MeetingSessionManager>,
    after_id: Option<String>,
    limit: Option<usize>,
) -> Result<QueryEventsPage, QueryError> {
    let limit = page_size(limit)?;
    let store = meetings.store().await?;
    let before = match after_id {
        Some(id) => {
            let when = store
                .query_event_position(&id)?
                .ok_or(QueryError::UnknownCursor)?;
            Some((when, id))
        }
        None => None,
    };
    let mut rows = store.query_events(before, limit + 1)?;
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    let entries = rows.into_iter().map(event_from_row).collect::<Vec<_>>();
    let next_cursor = has_more
        .then(|| entries.last().map(|event| event.id.clone()))
        .flatten();
    Ok(QueryEventsPage {
        schema_version: QUERY_SCHEMA_VERSION,
        entries,
        next_cursor,
    })
}

/// One ledger row as the plane reports it.
///
/// The events counterpart of [`assemble`], and `pub(crate)` for the same
/// reason: everything above it is state lookup and `await`, so this is where
/// the fixture store in `meeting/store/query_plane.rs` can exercise what a
/// receipt and a run actually render as.
pub(crate) fn event_from_row(row: QueryEventRow) -> QueryEvent {
    match row {
        QueryEventRow::Receipt {
            receipt,
            when_utc_ms,
        } => QueryEvent {
            id: receipt.operation_id.uuid().to_string(),
            source: QueryEventSource::OperationReceipt,
            action: token(&receipt.command),
            result: match receipt.result {
                OperationResult::Committed => QueryEventResult::Committed,
                OperationResult::Rejected => QueryEventResult::Rejected,
                OperationResult::Failed => QueryEventResult::Failed,
            },
            detail: receipt
                .reason_codes
                .iter()
                .map(token)
                .collect::<Vec<_>>()
                .join(", "),
            outcome_summary: None,
            when_utc_ms,
            link: receipt.session_id.map(meeting_link),
        },
        QueryEventRow::Run {
            run_id,
            workflow_id,
            status,
            outcome_summary,
            error,
            session_id,
            when_utc_ms,
        } => QueryEvent {
            id: run_id,
            source: QueryEventSource::WorkflowRun,
            action: WorkflowId::from_str(&workflow_id)
                .map(|workflow| workflow.as_str().to_string())
                .unwrap_or(workflow_id),
            result: match status.as_str() {
                "ok" => QueryEventResult::Ok,
                "skipped" => QueryEventResult::Skipped,
                _ => QueryEventResult::Failed,
            },
            detail: error.unwrap_or_else(|| outcome_summary.clone()),
            outcome_summary: Some(outcome_summary),
            when_utc_ms,
            link: session_id.map(meeting_link),
        },
    }
}

/// The snake_case token a specta enum serialises to, without a match arm per
/// variant in this file: the store already declares those names, and a second
/// copy here would drift from them.
fn token<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn page_size(limit: Option<usize>) -> Result<usize, QueryError> {
    match limit {
        None => Ok(DEFAULT_PAGE_SIZE),
        Some(0) => Err(QueryError::InvalidRequest),
        Some(limit) => Ok(limit.min(MAX_PAGE_SIZE)),
    }
}

/// The tokens a row has to contain, folded the way FTS5 compares them.
///
/// Whitespace-split and implicitly AND-ed, which is what `fts_match_query`
/// builds for the indexed halves — so an unindexed source (people, loops)
/// agrees with an indexed one about what "two words" means.
fn tokens(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(|token| token.to_lowercase())
        .filter(|token| !token.is_empty())
        .collect()
}

fn matches_every_token(haystack: &str, tokens: &[String]) -> bool {
    let folded = haystack.to_lowercase();
    tokens.iter().all(|token| folded.contains(token))
}

/// Every name a person answers to, as one string to match against.
///
/// People have no index of their own, so this is what the all-tokens-present
/// rule is applied to — here rather than at a call site, because a surface that
/// searched a different set of names would be a second answer to "who counts as
/// this person" without ever looking like one.
pub(super) fn person_haystack(entry: &PersonListEntry) -> String {
    std::iter::once(entry.person.display_name.as_str())
        .chain(entry.person.aliases.iter().map(String::as_str))
        .chain(entry.person.calendar_emails.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Deduplicate, order, and cut one page.
///
/// Split out so the rules are testable without a corpus: first occurrence of a
/// `(kind, id)` wins (callers push their strongest evidence first), the order
/// is newest first with `(kind, id)` breaking ties, and the cursor is a
/// position in exactly that order.
fn merge(
    candidates: Vec<QueryRow>,
    cursor: Option<&QueryCursor>,
    limit: usize,
) -> (Vec<QueryRow>, Option<QueryCursor>) {
    let mut seen = HashSet::new();
    let mut rows = candidates
        .into_iter()
        .filter(|row| seen.insert((row.kind, row.id.clone())))
        .filter(|row| follows_cursor(row, cursor))
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| compare(left, right));
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = has_more
        .then(|| {
            rows.last().map(|last| QueryCursor {
                when_utc_ms: last.when_utc_ms,
                kind: last.kind,
                id: last.id.clone(),
                dictation_id: rows
                    .iter()
                    .rev()
                    .find(|row| row.kind == QueryRowKind::Dictation)
                    .and_then(|row| row.id.parse::<i64>().ok())
                    .or_else(|| cursor.and_then(|cursor| cursor.dictation_id)),
            })
        })
        .flatten();
    (rows, next_cursor)
}

/// Newest first; inside one millisecond, kind declaration order, then id.
fn compare(left: &QueryRow, right: &QueryRow) -> Ordering {
    right
        .when_utc_ms
        .cmp(&left.when_utc_ms)
        .then_with(|| left.kind.cmp(&right.kind))
        .then_with(|| left.id.cmp(&right.id))
}

/// Whether a row sits strictly after the cursor in the page order. Sources are
/// asked for candidates up to and including the cursor's millisecond, so the
/// rows already returned are dropped here rather than in five different
/// queries.
fn follows_cursor(row: &QueryRow, cursor: Option<&QueryCursor>) -> bool {
    let Some(cursor) = cursor else {
        return true;
    };
    let boundary = QueryRow {
        kind: cursor.kind,
        id: cursor.id.clone(),
        title: String::new(),
        snippet: String::new(),
        when_utc_ms: cursor.when_utc_ms,
        link: String::new(),
    };
    compare(row, &boundary) == Ordering::Greater
}

/// Truncate on a character boundary, with an ellipsis when something was cut.
fn bounded(value: &str, limit: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_string();
    }
    let mut cut = trimmed.chars().take(limit).collect::<String>();
    cut.push('…');
    cut
}

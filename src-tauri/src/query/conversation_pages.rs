//! `sona --bookmarks`, `--roster` and `--place`: the marks pressed while
//! recording, the names a call listed, and where a meeting was recorded.

use super::external::{meeting_refusal, visible_meeting, ExternalError};
use super::{meeting_link, KeptLocal, QueryError, QUERY_SCHEMA_VERSION};
use crate::meeting::context::ConversationLocation;
use crate::meeting::conversation_review::{ConversationBookmark, ConversationBookmarkKind};
use crate::meeting::store::MeetingStore;
use crate::meeting::types::MeetingSessionId;
use serde::Serialize;
use uuid::Uuid;

/// One mark pressed during a recording.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExternalBookmarkRow {
    pub id: String,
    pub kind: ConversationBookmarkKind,
    pub label: Option<String>,
    /// How far into the recording it was pressed.
    pub at_ms: u64,
    pub meeting_id: Uuid,
    pub meeting_title: String,
    pub marked_at_utc_ms: i64,
    pub link: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExternalBookmarksPage {
    pub schema_version: u32,
    pub entries: Vec<ExternalBookmarkRow>,
    pub has_more: bool,
}

/// The other people a call listed while it was recorded.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExternalRoster {
    pub schema_version: u32,
    pub meeting_id: Uuid,
    /// The call app the names were read from. Null when none was chosen.
    pub app: Option<String>,
    /// As the call showed them, sorted. Never linked to a person here.
    pub names: Vec<String>,
    pub link: String,
}

/// Another recording within 100 m of the meeting's place.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ExternalNearbyRow {
    pub meeting_id: Uuid,
    pub title: String,
    pub started_at_utc_ms: Option<i64>,
    pub place: ConversationLocation,
    pub link: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ExternalPlace {
    pub schema_version: u32,
    pub meeting_id: Uuid,
    /// Null when no place was set for the meeting.
    pub place: Option<ConversationLocation>,
    /// Newest first.
    pub nearby: Vec<ExternalNearbyRow>,
    pub has_more: bool,
    pub link: String,
}

/// Newest meeting first, then in the order they were pressed. A meeting
/// whose series is kept on this Mac is left out.
pub(crate) fn bookmarks_page(
    store: &MeetingStore,
    limit: usize,
) -> Result<ExternalBookmarksPage, ExternalError> {
    let kept_local = KeptLocal::read(store).map_err(QueryError::from)?;
    let mut bookmarks = store
        .all_conversation_bookmarks()
        .map_err(QueryError::from)?
        .into_iter()
        .filter(|(bookmark, _)| !kept_local.hides(bookmark.session_id));
    let entries = bookmarks.by_ref().take(limit).map(bookmark_row).collect();
    Ok(ExternalBookmarksPage {
        schema_version: QUERY_SCHEMA_VERSION,
        entries,
        has_more: bookmarks.next().is_some(),
    })
}

fn bookmark_row((bookmark, meeting_title): (ConversationBookmark, String)) -> ExternalBookmarkRow {
    let at_ms = bookmark.start_offset_ns / 1_000_000;
    ExternalBookmarkRow {
        id: bookmark.bookmark_id,
        kind: bookmark.kind,
        label: bookmark.label,
        at_ms,
        meeting_id: bookmark.session_id.uuid(),
        meeting_title,
        marked_at_utc_ms: bookmark.created_at_utc_ms,
        link: format!("{}?atMs={at_ms}", meeting_link(bookmark.session_id)),
    }
}

pub(crate) fn roster(
    store: &MeetingStore,
    session_id: MeetingSessionId,
) -> Result<ExternalRoster, ExternalError> {
    visible_meeting(store, session_id)?;
    let status = store
        .call_name_status(session_id)
        .map_err(|error| meeting_refusal(session_id, error))?;
    Ok(ExternalRoster {
        schema_version: QUERY_SCHEMA_VERSION,
        meeting_id: session_id.uuid(),
        app: status.target.map(|target| target.label),
        names: status.roster,
        link: meeting_link(session_id),
    })
}

/// A meeting kept on this Mac is neither answered for nor listed nearby.
pub(crate) fn place(
    store: &MeetingStore,
    session_id: MeetingSessionId,
    limit: usize,
) -> Result<ExternalPlace, ExternalError> {
    let kept_local = visible_meeting(store, session_id)?;
    let review = store
        .conversation_place_review(session_id)
        .map_err(|error| meeting_refusal(session_id, error))?;
    let mut nearby = review
        .sessions
        .into_iter()
        .filter(|session| !kept_local.hides(session.session_id));
    let rows = nearby
        .by_ref()
        .take(limit)
        .map(|session| ExternalNearbyRow {
            meeting_id: session.session_id.uuid(),
            title: session.title,
            started_at_utc_ms: session.started_at_utc_ms,
            place: session.location,
            link: meeting_link(session.session_id),
        })
        .collect();
    Ok(ExternalPlace {
        schema_version: QUERY_SCHEMA_VERSION,
        meeting_id: session_id.uuid(),
        place: review.location,
        nearby: rows,
        has_more: nearby.next().is_some(),
        link: meeting_link(session_id),
    })
}

use crate::managers::history::HistoryManager;
use crate::meeting::detection::calendar::CalendarSource;
use crate::meeting::people_types::PersonId;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::types::MeetingFolderId;
use crate::planning::work::ask::WorkAnswer;
use crate::planning::EvidenceSourceKind;
use crate::query::pack::QueryPack;
use crate::query::system::{PersonPromises, SystemIndex, SystemWork};
use crate::query::{
    QueryAudience, QueryCursor, QueryError, QueryEventsPage, QueryScope, QuerySearchPage,
};
use std::sync::Arc;
use tauri::{AppHandle, State};

/// Search every noun this app keeps, through one API.
///
/// `cursor` comes from the previous page's `next_cursor` and belongs to the
/// query that produced it: the page order depends on the query text, so a
/// cursor from a different search is not a position in this one.
#[tauri::command]
#[specta::specta]
pub async fn sona_query_search(
    meetings: State<'_, Arc<MeetingSessionManager>>,
    history: State<'_, Arc<HistoryManager>>,
    scope: QueryScope,
    query: String,
    limit: Option<usize>,
    cursor: Option<QueryCursor>,
) -> Result<QuerySearchPage, QueryError> {
    crate::query::search(
        &meetings,
        &history,
        QueryAudience::Local,
        scope,
        &query,
        limit,
        cursor,
    )
    .await
}

/// What happened, newest first. `after_id` is the last event id the caller
/// already saw.
#[tauri::command]
#[specta::specta]
pub async fn sona_query_events(
    meetings: State<'_, Arc<MeetingSessionManager>>,
    after_id: Option<String>,
    limit: Option<usize>,
) -> Result<QueryEventsPage, QueryError> {
    crate::query::events(
        &meetings,
        crate::query::QueryAudience::Local,
        after_id,
        limit,
    )
    .await
}

/// Assemble the evidence for one Ask turn: the corpus card, then the top rows
/// that matched, quoted verbatim with their `sona://` addresses, inside the
/// ceiling the agent panel accepts on the wire.
///
/// A pack is built to be sent, so D14's per-series exclusion applies to it:
/// rows from a series the operator kept on this Mac are not in it, and the
/// card neither names nor counts that series. See
/// `query::pack::without_excluded_series`.
///
/// With `folder_id`, only that folder's meetings are searched and one line
/// naming the folder replaces the corpus card. Omitted or null is unscoped.
#[tauri::command]
#[specta::specta]
pub async fn sona_query_pack(
    meetings: State<'_, Arc<MeetingSessionManager>>,
    history: State<'_, Arc<HistoryManager>>,
    calendar: State<'_, Arc<dyn CalendarSource>>,
    question: String,
    folder_id: Option<MeetingFolderId>,
) -> Result<QueryPack, QueryError> {
    crate::query::pack::for_question(&meetings, &history, &calendar, &question, folder_id).await
}

/// Open one `sona://` address from inside the app.
///
/// The same routing an external link takes, deliberately: `deeplink.rs` owns
/// what an address means and `dispatch_deep_link` owns which surface it wakes,
/// so a row in ⌘K citing `sona://loop/<id>` lands exactly where the OS handing
/// the app that URL would land. Re-deriving either rule in a client is how a
/// second, quietly different navigation appears. Returns whether the address
/// was one of ours.
#[tauri::command]
#[specta::specta]
pub fn sona_open_link(app: AppHandle, link: String) -> bool {
    crate::dispatch_deep_link(&app, &link)
}

/// The people and meetings Siri, Spotlight and Shortcuts may list, without
/// the series kept on this Mac.
#[tauri::command]
#[specta::specta]
pub async fn sona_query_system_index(
    meetings: State<'_, Arc<MeetingSessionManager>>,
) -> Result<SystemIndex, QueryError> {
    let store = meetings.store().await?;
    crate::query::system::system_index(&store)
}

/// The tasks, projects and notes Spotlight may list, without the work drawn
/// from series kept on this Mac or the personal documents. While the meetings
/// cannot be opened, nothing says which series are kept, so every task drawn
/// from a meeting is left out and the rest still lists.
#[tauri::command]
#[specta::specta]
pub async fn sona_query_system_work(
    meetings: State<'_, Arc<MeetingSessionManager>>,
    history: State<'_, Arc<HistoryManager>>,
) -> Result<SystemWork, QueryError> {
    let kept_local = match meetings.store().await {
        Ok(store) => Some(crate::query::KeptLocal::read(&store)?),
        Err(_) => None,
    };
    history
        .with_connection(|conn| {
            crate::query::system::system_work(conn, |kind, source_id| match &kept_local {
                Some(kept_local) => {
                    crate::query::external::evidence_kept_local(kind, source_id, kept_local)
                }
                None => matches!(kind, EvidenceSourceKind::Meeting | EvidenceSourceKind::Loop),
            })
        })
        .map_err(|_| QueryError::Failed)
}

/// The lines that answer a question, for Siri and Shortcuts: read the way an
/// agent on this Mac reads them, so nothing said in a series kept on this Mac
/// and no personal document answers.
#[tauri::command]
#[specta::specta]
pub async fn sona_query_system_ask(app: AppHandle, question: String) -> Result<WorkAnswer, String> {
    crate::planning::work::ask::answer(&app, QueryAudience::External, &question).await
}

/// What one person promised, for Siri and Shortcuts: null when no person has
/// this id.
#[tauri::command]
#[specta::specta]
pub async fn sona_query_person_promises(
    meetings: State<'_, Arc<MeetingSessionManager>>,
    person_id: PersonId,
) -> Result<Option<PersonPromises>, QueryError> {
    let store = meetings.store().await?;
    crate::query::system::person_promises(&store, person_id)
}

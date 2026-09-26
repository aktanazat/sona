use crate::managers::history::HistoryManager;
use crate::meeting::detection::calendar::CalendarSource;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::types::MeetingFolderId;
use crate::query::pack::QueryPack;
use crate::query::{QueryCursor, QueryError, QueryEventsPage, QueryScope, QuerySearchPage};
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
    crate::query::search(&meetings, &history, scope, &query, limit, cursor).await
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
    crate::query::events(&meetings, after_id, limit).await
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

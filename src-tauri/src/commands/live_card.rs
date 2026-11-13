use crate::integrations::{self, types::ConnectionKind, types::SendReceipt};
use crate::meeting::processing::MeetingLiveCard;
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::types::{MeetingCommandError, MeetingSessionId};
use crate::planning::work::types::WorkOwner;
use std::sync::Arc;
use tauri::{AppHandle, State};

/// The promises the running capture has heard. `None` once the meeting has
/// stopped: its promises are then Triage tasks.
#[tauri::command]
#[specta::specta]
pub fn meeting_live_card(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Option<MeetingLiveCard> {
    manager.live_card(session_id)
}

/// Who owes one heard promise, picked by hand while the meeting records.
#[tauri::command]
#[specta::specta]
pub async fn meeting_live_card_owner(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
    promise_id: String,
    owner: WorkOwner,
) -> Result<MeetingLiveCard, MeetingCommandError> {
    manager
        .set_live_card_owner(session_id, promise_id, owner)
        .await
}

/// New words and date for one heard promise, typed while the meeting
/// records. Later readings leave it alone.
#[tauri::command]
#[specta::specta]
pub fn meeting_live_card_edit(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
    promise_id: String,
    what: String,
    due_at_ms: Option<i64>,
) -> Result<MeetingLiveCard, MeetingCommandError> {
    manager.edit_live_card(session_id, &promise_id, &what, due_at_ms)
}

/// Post a meeting's follow-up to a Slack connection. Only ever on the
/// operator's keypress; nothing sends it on its own.
#[tauri::command]
#[specta::specta]
pub async fn work_follow_up_post(
    app: AppHandle,
    manager: State<'_, Arc<MeetingSessionManager>>,
    connection_id: String,
    operation_id: String,
    text: String,
) -> Result<SendReceipt, String> {
    if text.trim().is_empty() {
        return Err("Nobody owes anything from this meeting yet.".into());
    }
    let store = manager
        .store()
        .await
        .map_err(|_| "The meeting library is unavailable.")?;
    let slack = store
        .connections()
        .map_err(|_| "Could not read connections.")?
        .into_iter()
        .any(|connection| {
            connection.id == connection_id
                && matches!(
                    connection.kind,
                    ConnectionKind::SlackWebhook | ConnectionKind::SlackBot
                )
        });
    if !slack {
        return Err("Choose a Slack connection to post the follow-up.".into());
    }
    integrations::send_slack(&app, &store, &connection_id, &text, &operation_id).await
}

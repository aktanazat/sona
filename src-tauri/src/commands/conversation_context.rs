use crate::meeting::context::{ConversationContext, ConversationContextUpdate};
use crate::meeting::hotel_booking::{self, MailHotelLookup};
use crate::meeting::mail_context::{self, MailContextState};
use crate::meeting::session::{map_store_error, MeetingSessionManager};
use crate::meeting::types::{MeetingCommandError, MeetingEventPayload, MeetingSessionId};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

#[tauri::command]
#[specta::specta]
pub async fn conversation_context(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Result<ConversationContext, MeetingCommandError> {
    manager
        .store()
        .await?
        .conversation_context(session_id)
        .map_err(map_store_error)
}

#[tauri::command]
#[specta::specta]
pub async fn conversation_context_update(
    app: AppHandle,
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: ConversationContextUpdate,
) -> Result<ConversationContext, MeetingCommandError> {
    let store = manager.store().await?;
    let result = store
        .update_conversation_context(request)
        .map_err(map_store_error)?;
    let session = store
        .session_snapshot(result.session_id)
        .map_err(map_store_error)?;
    let _ = app.emit(
        "meeting:session-changed",
        MeetingEventPayload {
            event_schema_version: 1,
            session_id: Some(result.session_id),
            revision: session.revision,
        },
    );
    Ok(result)
}

/// A hotel from a booking email already in Mail, for a recording that names
/// no place. Reads only with email context on; with `open_mail` false it
/// reads only a Mail that is already open. It writes nothing.
#[tauri::command]
#[specta::specta]
pub async fn conversation_place_from_mail(
    app: AppHandle,
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
    open_mail: bool,
) -> Result<MailHotelLookup, MeetingCommandError> {
    if !crate::settings::get_settings(&app)
        .meeting_prep
        .email_context_enabled
    {
        return Ok(MailHotelLookup::without_reading(MailContextState::Off));
    }
    let store = manager.store().await?;
    let context = store
        .conversation_context(session_id)
        .map_err(map_store_error)?;
    let day = store
        .session_snapshot(session_id)
        .map_err(map_store_error)?
        .started_at_utc_ms
        .and_then(|started| Some((started, chrono::DateTime::from_timestamp_millis(started)?)));
    let Some((started, day)) = day.filter(|_| hotel_booking::wants_place(&context)) else {
        return Ok(MailHotelLookup::without_reading(MailContextState::Empty));
    };
    let mail = mail_context::read_bookings(started, open_mail).await;
    Ok(MailHotelLookup::from_mail(
        &mail,
        day.with_timezone(&chrono::Local).date_naive(),
    ))
}

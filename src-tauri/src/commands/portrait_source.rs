use crate::meeting::people_types::{PersonAvatarSourceResult, PersonId};
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::types::MeetingCommandError;
use std::sync::Arc;
use tauri::State;

#[tauri::command]
#[specta::specta]
pub async fn person_avatar_source(
    manager: State<'_, Arc<MeetingSessionManager>>,
    person_id: PersonId,
) -> Result<PersonAvatarSourceResult, MeetingCommandError> {
    manager.person_avatar_source(person_id).await
}

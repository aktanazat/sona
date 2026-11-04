use crate::meeting::people_types::{
    PeopleMutationResult, PersonAvatarResult, PersonAvatarSetRequest, PersonContactSetRequest,
    PersonContributionsResult, PersonId,
};
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::types::MeetingCommandError;
use std::sync::Arc;
use tauri::State;

#[tauri::command]
#[specta::specta]
pub async fn person_contributions(
    manager: State<'_, Arc<MeetingSessionManager>>,
    person_id: PersonId,
    offset: Option<u64>,
    limit: Option<usize>,
) -> Result<PersonContributionsResult, MeetingCommandError> {
    manager.person_contributions(person_id, offset, limit).await
}

#[tauri::command]
#[specta::specta]
pub async fn person_avatar(
    manager: State<'_, Arc<MeetingSessionManager>>,
    person_id: PersonId,
) -> Result<PersonAvatarResult, MeetingCommandError> {
    manager.person_avatar(person_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn person_avatar_set(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: PersonAvatarSetRequest,
) -> Result<PersonAvatarResult, MeetingCommandError> {
    manager.person_avatar_set(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn person_contact_set(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: PersonContactSetRequest,
) -> Result<PeopleMutationResult, MeetingCommandError> {
    manager.person_contact_set(request).await
}

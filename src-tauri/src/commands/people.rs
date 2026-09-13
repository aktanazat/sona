use crate::meeting::people_types::{
    MeetingPeopleContextResult, OpenLoopsInboxResult, OrganizationDetailResult, PeopleListResult,
    PeopleMutationResult, PersonContextResult, PersonDeleteRequest, PersonDetailResult, PersonId,
    PersonLinkRequest, PersonMergeRequest, PersonRenameRequest, PersonSplitRequest,
    PersonSummaryRegenerateResult, VocabularyCandidatesResult,
};
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::types::{MeetingCommandError, MeetingSessionId};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
#[specta::specta]
pub async fn people_list(
    manager: State<'_, Arc<MeetingSessionManager>>,
) -> Result<PeopleListResult, MeetingCommandError> {
    manager.people_list().await
}

#[tauri::command]
#[specta::specta]
pub async fn person_detail(
    manager: State<'_, Arc<MeetingSessionManager>>,
    person_id: PersonId,
) -> Result<PersonDetailResult, MeetingCommandError> {
    manager.person_detail(person_id).await
}

/// One organization page: its people, and what they collectively left open.
///
/// `slug` is what `sona://organization/<slug>` carries. The store slugifies
/// whatever it is given, so a caller holding the label a person's header shows
/// may pass that instead of deriving the slug a second time.
#[tauri::command]
#[specta::specta]
pub async fn organization_detail(
    manager: State<'_, Arc<MeetingSessionManager>>,
    slug: String,
) -> Result<OrganizationDetailResult, MeetingCommandError> {
    manager.organization_detail(slug).await
}

#[tauri::command]
#[specta::specta]
pub async fn person_summary_regenerate(
    manager: State<'_, Arc<MeetingSessionManager>>,
    person_id: PersonId,
) -> Result<PersonSummaryRegenerateResult, MeetingCommandError> {
    manager.person_summary_regenerate(person_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn person_context(
    manager: State<'_, Arc<MeetingSessionManager>>,
    person_ids: Vec<PersonId>,
) -> Result<PersonContextResult, MeetingCommandError> {
    manager.person_context(person_ids).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_people_context(
    manager: State<'_, Arc<MeetingSessionManager>>,
    session_id: MeetingSessionId,
) -> Result<MeetingPeopleContextResult, MeetingCommandError> {
    manager.meeting_people_context(session_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn person_rename(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: PersonRenameRequest,
) -> Result<PeopleMutationResult, MeetingCommandError> {
    manager.person_rename(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn person_merge(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: PersonMergeRequest,
) -> Result<PeopleMutationResult, MeetingCommandError> {
    manager.person_merge(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn person_split(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: PersonSplitRequest,
) -> Result<PeopleMutationResult, MeetingCommandError> {
    manager.person_split(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn person_delete(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: PersonDeleteRequest,
) -> Result<PeopleMutationResult, MeetingCommandError> {
    manager.person_delete(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn link_confirm(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: PersonLinkRequest,
) -> Result<PeopleMutationResult, MeetingCommandError> {
    manager.link_confirm(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn link_remove(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: PersonLinkRequest,
) -> Result<PeopleMutationResult, MeetingCommandError> {
    manager.link_remove(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn link_add_manual(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: PersonLinkRequest,
) -> Result<PeopleMutationResult, MeetingCommandError> {
    manager.link_add_manual(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn open_loops_inbox(
    manager: State<'_, Arc<MeetingSessionManager>>,
    limit: Option<usize>,
) -> Result<OpenLoopsInboxResult, MeetingCommandError> {
    manager.open_loops_inbox(limit).await
}

#[tauri::command]
#[specta::specta]
pub async fn vocabulary_candidates(
    manager: State<'_, Arc<MeetingSessionManager>>,
) -> Result<VocabularyCandidatesResult, MeetingCommandError> {
    manager.vocabulary_candidates().await
}

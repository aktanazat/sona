use super::{map_store_error, now_utc_ms};
use crate::meeting::people_types::{
    CompaniesListResult, MeetingPeopleContextResult, OpenLoopsInboxResult,
    OrganizationDetailResult, PeopleListResult, PeopleMutationResult, PersonContextResult,
    PersonDeleteRequest, PersonDetailResult, PersonId, PersonLinkRequest, PersonMergeRequest,
    PersonRenameRequest, PersonSplitRequest,
};
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::types::{MeetingCommandError, MeetingSessionId};

impl MeetingSessionManager {
    pub async fn people_list(&self) -> Result<PeopleListResult, MeetingCommandError> {
        self.store().await?.people_list().map_err(map_store_error)
    }

    pub async fn person_detail(
        &self,
        person_id: PersonId,
    ) -> Result<PersonDetailResult, MeetingCommandError> {
        self.store()
            .await?
            .person_detail(person_id)
            .map_err(map_store_error)
    }

    pub async fn organization_detail(
        &self,
        slug: String,
    ) -> Result<OrganizationDetailResult, MeetingCommandError> {
        self.store()
            .await?
            .organization_detail(&slug)
            .map_err(map_store_error)
    }

    pub async fn companies_list(&self) -> Result<CompaniesListResult, MeetingCommandError> {
        self.store()
            .await?
            .companies_list()
            .map_err(map_store_error)
    }

    pub async fn person_context(
        &self,
        person_ids: Vec<PersonId>,
    ) -> Result<PersonContextResult, MeetingCommandError> {
        self.store()
            .await?
            .person_context(&person_ids)
            .map_err(map_store_error)
    }
    pub async fn meeting_people_context(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<MeetingPeopleContextResult, MeetingCommandError> {
        self.store()
            .await?
            .meeting_people_context(session_id)
            .map_err(map_store_error)
    }

    pub async fn person_rename(
        &self,
        request: PersonRenameRequest,
    ) -> Result<PeopleMutationResult, MeetingCommandError> {
        let result = self
            .store()
            .await?
            .rename_person(
                request.person_id,
                request.display_name,
                request.expected_revision,
                now_utc_ms(),
            )
            .map_err(map_store_error)?;
        self.emit_artifact_changed(None, result.revision);
        Ok(result)
    }

    pub async fn person_merge(
        &self,
        request: PersonMergeRequest,
    ) -> Result<PeopleMutationResult, MeetingCommandError> {
        let result = self
            .store()
            .await?
            .merge_persons_with_voice_resolution(
                request.source_person_id,
                request.target_person_id,
                request.expected_revision,
                request.voice_profile_resolution,
                now_utc_ms(),
            )
            .map_err(map_store_error)?;
        self.emit_artifact_changed(None, result.revision);
        Ok(result)
    }
    pub async fn person_split(
        &self,
        request: PersonSplitRequest,
    ) -> Result<PeopleMutationResult, MeetingCommandError> {
        let result = self
            .store()
            .await?
            .split_person(request, now_utc_ms())
            .map_err(map_store_error)?;
        self.emit_artifact_changed(None, result.revision);
        Ok(result)
    }

    pub async fn person_delete(
        &self,
        request: PersonDeleteRequest,
    ) -> Result<PeopleMutationResult, MeetingCommandError> {
        let result = self
            .store()
            .await?
            .delete_person(request.person_id, request.expected_revision)
            .map_err(map_store_error)?;
        self.emit_artifact_changed(None, result.revision);
        Ok(result)
    }

    pub async fn link_confirm(
        &self,
        request: PersonLinkRequest,
    ) -> Result<PeopleMutationResult, MeetingCommandError> {
        self.mutate_link(request, LinkMutation::Confirm).await
    }

    pub async fn link_remove(
        &self,
        request: PersonLinkRequest,
    ) -> Result<PeopleMutationResult, MeetingCommandError> {
        self.mutate_link(request, LinkMutation::Remove).await
    }

    pub async fn link_add_manual(
        &self,
        request: PersonLinkRequest,
    ) -> Result<PeopleMutationResult, MeetingCommandError> {
        self.mutate_link(request, LinkMutation::AddManual).await
    }

    async fn mutate_link(
        &self,
        request: PersonLinkRequest,
        mutation: LinkMutation,
    ) -> Result<PeopleMutationResult, MeetingCommandError> {
        let store = self.store().await?;
        let result = match mutation {
            LinkMutation::Confirm => store.confirm_person_link(
                request.meeting_id,
                request.person_id,
                request.expected_revision,
            ),
            LinkMutation::Remove => store.remove_person_link(
                request.meeting_id,
                request.person_id,
                request.expected_revision,
            ),
            LinkMutation::AddManual => store.add_manual_person_link(
                request.meeting_id,
                request.person_id,
                request.expected_revision,
                now_utc_ms(),
            ),
        }
        .map_err(map_store_error)?;
        self.emit_artifact_changed(Some(request.meeting_id), result.revision);
        Ok(result)
    }

    pub async fn open_loops_inbox(
        &self,
        limit: Option<usize>,
    ) -> Result<OpenLoopsInboxResult, MeetingCommandError> {
        self.store()
            .await?
            .open_loops_inbox(limit.unwrap_or(5).min(100))
            .map_err(map_store_error)
    }
}

#[derive(Clone, Copy)]
enum LinkMutation {
    Confirm,
    Remove,
    AddManual,
}

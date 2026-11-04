use super::people_types::{
    PeopleMutationResult, PersonAvatarResult, PersonAvatarSetRequest, PersonContactSetRequest,
    PersonContributionsResult, PersonId,
};
use super::session::MeetingSessionManager;
use super::types::MeetingCommandError;
use super::workflow_engine::{map_store_error, now_utc_ms};

impl MeetingSessionManager {
    pub async fn person_contributions(
        &self,
        person_id: PersonId,
        offset: Option<u64>,
        limit: Option<usize>,
    ) -> Result<PersonContributionsResult, MeetingCommandError> {
        self.store()
            .await?
            .person_contributions(person_id, offset.unwrap_or(0), limit.unwrap_or(30))
            .map_err(map_store_error)
    }

    pub async fn person_avatar(
        &self,
        person_id: PersonId,
    ) -> Result<PersonAvatarResult, MeetingCommandError> {
        self.store()
            .await?
            .person_avatar(person_id)
            .map_err(map_store_error)
    }

    pub async fn person_avatar_set(
        &self,
        request: PersonAvatarSetRequest,
    ) -> Result<PersonAvatarResult, MeetingCommandError> {
        let result = self
            .store()
            .await?
            .set_person_avatar(request, now_utc_ms())
            .map_err(map_store_error)?;
        self.emit_artifact_changed(None, result.revision);
        Ok(result)
    }

    pub async fn person_contact_set(
        &self,
        request: PersonContactSetRequest,
    ) -> Result<PeopleMutationResult, MeetingCommandError> {
        let result = self
            .store()
            .await?
            .set_person_contact_card(request, now_utc_ms())
            .map_err(map_store_error)?;
        self.emit_artifact_changed(None, result.revision);
        Ok(result)
    }
}

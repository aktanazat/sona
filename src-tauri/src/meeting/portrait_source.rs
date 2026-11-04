use super::people_types::{PersonAvatarSourceResult, PersonId};
use super::session::MeetingSessionManager;
use super::types::MeetingCommandError;
use super::workflow_engine::map_store_error;

impl MeetingSessionManager {
    pub async fn person_avatar_source(
        &self,
        person_id: PersonId,
    ) -> Result<PersonAvatarSourceResult, MeetingCommandError> {
        self.store()
            .await?
            .person_avatar_source(person_id)
            .map_err(map_store_error)
    }
}

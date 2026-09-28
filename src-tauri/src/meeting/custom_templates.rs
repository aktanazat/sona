use super::session::MeetingSessionManager;
use super::template_types::{
    MeetingCustomTemplateDeleteRequest, MeetingCustomTemplateSaveRequest,
    MeetingCustomTemplateSaveResult, MeetingCustomTemplates,
};
use super::types::MeetingCommandError;
use super::workflow_engine::{map_store_error, now_utc_ms};

impl MeetingSessionManager {
    pub async fn custom_templates(&self) -> Result<MeetingCustomTemplates, MeetingCommandError> {
        self.store()
            .await?
            .custom_templates()
            .map_err(map_store_error)
    }

    pub async fn save_custom_template(
        &self,
        request: MeetingCustomTemplateSaveRequest,
    ) -> Result<MeetingCustomTemplateSaveResult, MeetingCommandError> {
        self.store()
            .await?
            .save_custom_template(&request, now_utc_ms())
            .map_err(map_store_error)
    }

    pub async fn delete_custom_template(
        &self,
        request: MeetingCustomTemplateDeleteRequest,
    ) -> Result<MeetingCustomTemplates, MeetingCommandError> {
        self.store()
            .await?
            .delete_custom_template(&request)
            .map_err(map_store_error)
    }
}

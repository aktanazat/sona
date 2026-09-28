use crate::meeting::session::MeetingSessionManager;
use crate::meeting::template_types::{
    MeetingCustomTemplateDeleteRequest, MeetingCustomTemplateSaveRequest,
    MeetingCustomTemplateSaveResult, MeetingCustomTemplates,
};
use crate::meeting::types::MeetingCommandError;
use std::sync::Arc;
use tauri::State;

#[tauri::command]
#[specta::specta]
pub async fn meeting_custom_templates_list(
    manager: State<'_, Arc<MeetingSessionManager>>,
) -> Result<MeetingCustomTemplates, MeetingCommandError> {
    manager.custom_templates().await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_custom_template_save(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingCustomTemplateSaveRequest,
) -> Result<MeetingCustomTemplateSaveResult, MeetingCommandError> {
    manager.save_custom_template(request).await
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_custom_template_delete(
    manager: State<'_, Arc<MeetingSessionManager>>,
    request: MeetingCustomTemplateDeleteRequest,
) -> Result<MeetingCustomTemplates, MeetingCommandError> {
    manager.delete_custom_template(request).await
}

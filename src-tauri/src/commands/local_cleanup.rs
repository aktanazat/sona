use crate::local_cleanup::{self, CleanupStatus};
use crate::settings::{LOCAL_CLEANUP_MODEL_ID, LOCAL_CLEANUP_PROVIDER_ID};
use tauri::AppHandle;

#[tauri::command]
#[specta::specta]
pub async fn get_local_cleanup_status(app: AppHandle) -> Result<CleanupStatus, String> {
    Ok(local_cleanup::manager(&app)?.status(&app))
}

#[tauri::command]
#[specta::specta]
pub async fn download_local_cleanup(app: AppHandle) -> Result<CleanupStatus, String> {
    local_cleanup::manager(&app)?.start_download(&app)
}

#[tauri::command]
#[specta::specta]
pub async fn cancel_local_cleanup_download(app: AppHandle) -> Result<(), String> {
    local_cleanup::manager(&app)?.cancel_download();
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn remove_local_cleanup(app: AppHandle) -> Result<CleanupStatus, String> {
    local_cleanup::manager(&app)?.remove(&app).await
}

#[tauri::command]
#[specta::specta]
pub async fn select_local_cleanup(app: AppHandle) -> Result<(), String> {
    let status = local_cleanup::manager(&app)?.status(&app);
    if !status.runtime_available {
        return Err(status.error.unwrap_or_else(|| {
            "Offline cleanup is missing from this Sona installation. Reinstall Sona.".to_string()
        }));
    }
    if !matches!(status.phase, local_cleanup::CleanupPhase::Ready) {
        return Err("Download the English offline cleanup model first.".to_string());
    }
    crate::settings::update_settings(&app, |settings| {
        settings.post_process_provider_id = LOCAL_CLEANUP_PROVIDER_ID.to_string();
        settings.post_process_models.insert(
            LOCAL_CLEANUP_PROVIDER_ID.to_string(),
            LOCAL_CLEANUP_MODEL_ID.to_string(),
        );
        settings.post_process_enabled = true;
        if let Some(mode) = settings
            .modes
            .iter_mut()
            .find(|mode| mode.id == settings.active_mode_id)
        {
            mode.llm.enabled = true;
            mode.llm.cleanup_level = Some(crate::modes::CleanupLevel::Light);
            mode.llm.provider_id = None;
            mode.llm.model_id.clear();
            settings.modes_revision = settings.modes_revision.saturating_add(1);
        }
    })?;
    crate::secure_input::reconcile_fallback(&app);
    Ok(())
}

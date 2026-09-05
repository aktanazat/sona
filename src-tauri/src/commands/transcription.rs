use crate::managers::transcription::TranscriptionManager;
use crate::settings::{update_settings, ModelUnloadTimeout};
use serde::Serialize;
use specta::Type;
use tauri::{AppHandle, State};

#[derive(Serialize, Type)]
pub struct ModelLoadStatus {
    is_loaded: bool,
    current_model: Option<String>,
    /// The compute backend the loaded engine actually bound to — "MTL0" for a
    /// Metal GPU, "onnx" for an ONNX engine, a CPU string when Auto fell back.
    /// `None` when no model is loaded, because there is nothing bound to name.
    /// This is the requested accelerator's *outcome*, which is the only version
    /// of it worth showing: Auto reports what it chose.
    backend: Option<String>,
}

#[tauri::command]
#[specta::specta]
pub fn set_model_unload_timeout(app: AppHandle, timeout: ModelUnloadTimeout) -> Result<(), String> {
    update_settings(&app, |settings| {
        settings.model_unload_timeout = timeout;
    })?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn get_model_load_status(
    transcription_manager: State<TranscriptionManager>,
) -> Result<ModelLoadStatus, String> {
    Ok(ModelLoadStatus {
        is_loaded: transcription_manager.is_model_loaded(),
        current_model: transcription_manager.get_current_model(),
        backend: transcription_manager.current_backend(),
    })
}

#[tauri::command]
#[specta::specta]
pub fn unload_model_manually(
    transcription_manager: State<TranscriptionManager>,
) -> Result<(), String> {
    transcription_manager
        .unload_model()
        .map_err(|e| format!("Failed to unload model: {}", e))
}

use crate::managers::media_import::{
    validate_audio_import_path, AudioImportError, AudioImportFailureCode, AudioImportJob,
    AudioImportResult, AudioImportStatus, AudioImportUpdateEvent, ImportOrigin, MediaImportManager,
};
use crate::modes::RunPlan;
use crate::settings::get_settings;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, State};
use tauri_plugin_fs::FsExt;
use tauri_specta::Event;

const OPENED_AUDIO_EXTENSIONS: &[&str] = &["wav", "mp3", "m4a", "aac", "flac", "ogg"];
const FIRST_OPENED_AUDIO_FAILURE_ID: u64 = 8_000_000_000_000_000;
static NEXT_OPENED_AUDIO_FAILURE_ID: AtomicU64 = AtomicU64::new(FIRST_OPENED_AUDIO_FAILURE_ID);

#[derive(Clone, Debug)]
pub(crate) struct OpenedAudioImportFailure {
    code: AudioImportFailureCode,
    message: String,
}

impl OpenedAudioImportFailure {
    fn from_import_error(error: AudioImportError) -> Self {
        Self {
            code: error.code(),
            message: error.message().to_string(),
        }
    }

    fn invalid_file(message: impl Into<String>) -> Self {
        Self {
            code: AudioImportFailureCode::InvalidFile,
            message: message.into(),
        }
    }

    fn unsupported_format() -> Self {
        Self {
            code: AudioImportFailureCode::UnsupportedFormat,
            message: "This audio format is not supported.".to_string(),
        }
    }

    pub(crate) fn unavailable() -> Self {
        Self::invalid_file("Audio import manager is unavailable")
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn non_file_url() -> Self {
        Self::invalid_file("The opened URL is not a local audio file")
    }
}

impl std::fmt::Display for OpenedAudioImportFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

fn validate_opened_audio_path(path: &Path) -> Result<(), OpenedAudioImportFailure> {
    validate_audio_import_path(path).map_err(OpenedAudioImportFailure::from_import_error)?;
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some(extension) if OPENED_AUDIO_EXTENSIONS.contains(&extension) => Ok(()),
        _ => Err(OpenedAudioImportFailure::unsupported_format()),
    }
}

fn rejected_opened_audio_job(
    path: Option<&Path>,
    failure: &OpenedAudioImportFailure,
) -> AudioImportJob {
    let file_name = path
        .and_then(Path::file_name)
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Opened URL".to_string());
    AudioImportJob {
        id: NEXT_OPENED_AUDIO_FAILURE_ID.fetch_add(1, Ordering::Relaxed),
        file_name,
        status: AudioImportStatus::Failed,
        decoded_samples: 0,
        cancel_requested: false,
        result: Some(AudioImportResult::Failed {
            code: failure.code,
            message: failure.message.clone(),
        }),
    }
}

pub(crate) fn report_opened_audio_failure(
    app: &AppHandle,
    path: Option<&Path>,
    failure: OpenedAudioImportFailure,
) {
    let job = rejected_opened_audio_job(path, &failure);
    if let Err(error) = (AudioImportUpdateEvent { job }).emit(app) {
        log::warn!("Failed to emit opened audio import failure: {error}");
    }
}
/// Enqueue a path that is already in the app's file scope. The renderer and
/// operating-system Open With routes both converge here, so validation cannot
/// diverge between entry points. What cannot converge is the `origin`: the two
/// routes differ in whether anybody was asked where the file should go, and
/// the queue decides the destination of an unasked import from its length.
pub(crate) fn enqueue_scoped_audio_file(
    app: &AppHandle,
    media_import_manager: &MediaImportManager,
    path: &Path,
    origin: ImportOrigin,
) -> Result<AudioImportJob, String> {
    if !app.fs_scope().is_allowed(path) {
        return Err("Import source is outside the granted file scope".to_string());
    }
    let path = path
        .to_str()
        .ok_or_else(|| "Import source path is not valid Unicode".to_string())?;
    let run = RunPlan::for_media_import(&get_settings(app)).map_err(|error| error.to_string())?;
    media_import_manager
        .enqueue(path.to_string(), run, origin)
        .map_err(|error| error.to_string())
}

/// An operating-system file-open event is an explicit user choice. Validate a
/// regular audio file before granting that exact file to the existing scope, then
/// use the same scoped import path as the picker command. Video imports remain
/// available from the picker but are refused by the audio-only Open With route.
///
/// This is the only route into the app that carries no destination: Finder's
/// Open With, `open -a Sona` and a drop on the Dock icon all say "this file"
/// and nothing about what it is. `ImportOrigin::SystemOpen` is set here and
/// nowhere else, and it is what lets the queue send a recording to Library
/// instead of filing it as a dictation.
pub(crate) fn enqueue_opened_audio_file(
    app: &AppHandle,
    media_import_manager: &MediaImportManager,
    path: &Path,
) -> Result<AudioImportJob, OpenedAudioImportFailure> {
    validate_opened_audio_path(path)?;
    app.fs_scope().allow_file(path).map_err(|_| {
        OpenedAudioImportFailure::invalid_file("Could not grant the opened audio file to Sona")
    })?;
    enqueue_scoped_audio_file(app, media_import_manager, path, ImportOrigin::SystemOpen)
        .map_err(OpenedAudioImportFailure::invalid_file)
}

#[tauri::command]
#[specta::specta]
pub fn import_audio_file(
    app: AppHandle,
    media_import_manager: State<'_, Arc<MediaImportManager>>,
    path: String,
) -> Result<AudioImportJob, String> {
    enqueue_scoped_audio_file(
        &app,
        media_import_manager.inner().as_ref(),
        Path::new(&path),
        ImportOrigin::Picker,
    )
}

#[tauri::command]
#[specta::specta]
pub fn cancel_audio_import(
    media_import_manager: State<'_, Arc<MediaImportManager>>,
    job_id: u64,
) -> Result<AudioImportJob, String> {
    media_import_manager
        .cancel(job_id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
#[specta::specta]
pub fn list_audio_import_jobs(
    media_import_manager: State<'_, Arc<MediaImportManager>>,
) -> Vec<AudioImportJob> {
    media_import_manager.list_jobs()
}

#[cfg(test)]
mod tests {
    use super::{rejected_opened_audio_job, validate_opened_audio_path, OpenedAudioImportFailure};
    use crate::managers::media_import::{
        AudioImportFailureCode, AudioImportResult, AudioImportStatus,
    };
    use std::fs;
    use std::path::Path;

    #[test]
    fn opened_audio_path_keeps_regular_file_checks_and_refuses_video() {
        let directory = tempfile::tempdir().expect("temporary fixture directory");
        let audio = directory.path().join("recording.wav");
        let video = directory.path().join("recording.mp4");
        fs::write(&audio, b"audio").expect("write audio fixture");
        fs::write(&video, b"video").expect("write video fixture");

        assert!(validate_opened_audio_path(&audio).is_ok());
        assert_eq!(
            validate_opened_audio_path(&video)
                .expect_err("video Open With path must be rejected")
                .code,
            AudioImportFailureCode::UnsupportedFormat
        );
        assert_eq!(
            validate_opened_audio_path(directory.path())
                .expect_err("directory Open With path must be rejected")
                .code,
            AudioImportFailureCode::InvalidFile
        );
    }

    #[test]
    fn rejected_opened_audio_paths_emit_distinct_terminal_failure_jobs() {
        let first_path = Path::new("/tmp/first.wav");
        let second_path = Path::new("/tmp/second.wav");
        let failure = OpenedAudioImportFailure::unsupported_format();
        let first = rejected_opened_audio_job(Some(first_path), &failure);
        let second = rejected_opened_audio_job(Some(second_path), &failure);

        assert_ne!(first.id, second.id);
        assert_eq!(first.file_name, "first.wav");
        assert_eq!(first.status, AudioImportStatus::Failed);
        assert_eq!(
            first.result,
            Some(AudioImportResult::Failed {
                code: AudioImportFailureCode::UnsupportedFormat,
                message: "This audio format is not supported.".to_string(),
            })
        );
    }
}

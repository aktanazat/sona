use crate::actions::process_transcription_output;
use crate::analytics::DashboardTrendRequest;
use crate::managers::{
    history::{
        HistoryEntry, HistoryManager, HistorySourceKind, HistoryStats, HistoryStorageStatus,
        HistoryTrendProjection, NewRunReceipt, PaginatedHistory,
    },
    transcription::TranscriptionManager,
};
use log::error;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};

const HISTORY_AUDIO_CHUNK_BYTES: usize = 256 * 1024;
const HISTORY_AUDIO_CHUNK_BYTES_U64: u64 = 256 * 1024;

/// One bounded fragment of a history recording. The command identifies media
/// through a history row and never exposes a filesystem path to the webview.
#[derive(serde::Serialize, specta::Type)]
pub struct HistoryAudioChunk {
    pub bytes: Vec<u8>,
    pub eof: bool,
}

fn read_history_audio_chunk_from_path(
    path: &Path,
    offset: u64,
) -> std::io::Result<HistoryAudioChunk> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "history audio is not a regular file",
        ));
    }

    let file_len = metadata.len();
    if offset >= file_len {
        return Ok(HistoryAudioChunk {
            bytes: Vec::new(),
            eof: true,
        });
    }

    let bytes_to_read = usize::try_from((file_len - offset).min(HISTORY_AUDIO_CHUNK_BYTES_U64))
        .unwrap_or(HISTORY_AUDIO_CHUNK_BYTES);
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0; bytes_to_read];
    let read = file.read(&mut bytes)?;
    if read == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "history audio ended before its metadata",
        ));
    }
    bytes.truncate(read);

    Ok(HistoryAudioChunk {
        eof: offset.saturating_add(u64::try_from(read).unwrap_or(u64::MAX)) >= file_len,
        bytes,
    })
}

// The history pane's load failures deliberately carry no payload. A SQLite
// message can name file paths, schema, or a bound value, and the renderer has
// one translated "history could not be loaded" state for every cause; the
// cause itself belongs in the app log.
#[tauri::command]
#[specta::specta]
pub async fn get_history_entries(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    cursor: Option<i64>,
    limit: Option<usize>,
) -> Result<PaginatedHistory, ()> {
    history_manager
        .get_history_entries(cursor, limit)
        .await
        .map_err(|error| error!("Failed to read history entries: {error:#}"))
}

#[tauri::command]
#[specta::specta]
pub async fn search_history_entries(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    query: String,
    cursor: Option<i64>,
    limit: Option<usize>,
) -> Result<PaginatedHistory, ()> {
    history_manager
        .search_history_entries(&query, cursor, limit)
        .await
        .map_err(|error| error!("Failed to search history entries: {error:#}"))
}

#[tauri::command]
#[specta::specta]
pub async fn get_history_stats(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
) -> Result<HistoryStats, ()> {
    history_manager
        .get_history_stats()
        .await
        .map_err(|error| error!("Failed to read history statistics: {error:#}"))
}

/// Whether dictation history is encrypted at rest. Reads in-memory state, so it
/// answers while the database itself is locked or degraded.
#[tauri::command]
#[specta::specta]
pub fn history_storage_status(app: AppHandle) -> HistoryStorageStatus {
    app.state::<Arc<HistoryManager>>().storage_status()
}

#[tauri::command]
#[specta::specta]
pub async fn get_history_trend(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    request: DashboardTrendRequest,
) -> Result<HistoryTrendProjection, ()> {
    history_manager
        .get_history_trend(request)
        .await
        .map_err(|error| error!("Failed to read history trend: {error:#}"))
}

#[tauri::command]
#[specta::specta]
pub async fn get_history_run_receipts(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    history_id: i64,
) -> Result<Vec<crate::managers::history::HistoryRunReceipt>, String> {
    history_manager
        .get_run_receipts(history_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn toggle_history_entry_saved(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    id: i64,
) -> Result<(), String> {
    history_manager
        .toggle_saved_status(id)
        .await
        .map_err(|e| e.to_string())
}

/// Read a fixed-size media fragment for an existing history row. A caller
/// cannot choose a path, traversal component, or unbounded byte count.
#[tauri::command]
#[specta::specta]
pub async fn read_history_audio_chunk(
    history_manager: State<'_, Arc<HistoryManager>>,
    history_id: i64,
    offset: u64,
) -> Result<HistoryAudioChunk, ()> {
    let entry = history_manager
        .get_entry_by_id(history_id)
        .await
        .map_err(|_| ())?
        .ok_or(())?;
    let path = history_manager
        .get_audio_file_path(&entry.file_name)
        .ok_or(())?;

    tauri::async_runtime::spawn_blocking(move || read_history_audio_chunk_from_path(&path, offset))
        .await
        .map_err(|_| ())?
        .map_err(|_| ())
}

#[tauri::command]
#[specta::specta]
pub async fn delete_history_entry(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    id: i64,
) -> Result<(), String> {
    history_manager
        .delete_entry(id)
        .await
        .map_err(|e| e.to_string())
}

/// Runs a stored recording through an already-frozen plan and saves the result
/// as a new history row linked back to `source`.
///
/// Retry and reprocess differ only in which plan they freeze, so the replay
/// itself lives here once. Neither ever delivers the text: the recording is
/// historical, and pasting it into whatever happens to be focused now would be
/// a side effect the user did not ask for.
async fn replay_stored_recording(
    app: &AppHandle,
    history_manager: &Arc<HistoryManager>,
    transcription_manager: &Arc<TranscriptionManager>,
    source: HistoryEntry,
    run: crate::modes::RunPlan,
) -> Result<(), String> {
    let audio_path = history_manager
        .get_audio_file_path(&source.file_name)
        .ok_or_else(|| format!("History entry {} has no stored recording", source.id))?;
    let samples = crate::audio_toolkit::read_wav_samples(&audio_path)
        .map_err(|e| format!("Failed to load audio: {}", e))?;
    if samples.is_empty() {
        return Err("Recording has no audio samples".to_string());
    }
    let duration_ms = u64::try_from(samples.len())
        .ok()
        .and_then(|count| count.checked_mul(1_000))
        .map(|count| count / u64::from(crate::audio_toolkit::constants::WHISPER_SAMPLE_RATE));

    transcription_manager.initiate_model_load(run.asr());
    let manager = Arc::clone(transcription_manager);
    let asr = run.asr().clone();
    let decode =
        tauri::async_runtime::spawn_blocking(move || manager.transcribe_shared(&asr, &samples))
            .await
            .map_err(|error| format!("Transcription task panicked: {error}"))?
            .map_err(|error| error.to_string())?;
    let transcription = decode.text;
    if transcription.is_empty() {
        return Err("Recording contains no speech".to_string());
    }

    let processed = process_transcription_output(app, &transcription, &run).await;
    let word_count = u64::try_from(processed.final_text.split_whitespace().count()).ok();
    let saved = history_manager
        .save_derived_entry_with_receipt(
            source.id,
            source.file_name,
            transcription,
            run.post_process_requested(),
            processed.post_processed_text,
            NewRunReceipt {
                run: run
                    .mode_receipt()
                    .with_realtime_factor(decode.realtime_factor),
                context: run.context().receipt().clone(),
                started_at_ms: run.run_started_at_ms,
                completed_at_ms: current_time_ms(),
                duration_ms,
                word_count,
                source_kind: HistorySourceKind::Microphone,
                has_audio: true,
                capture_status: None,
            },
        )
        .map_err(|error| error.to_string())?;
    history_manager
        .append_delivery_attempt(
            saved.id,
            run.run_id,
            crate::delivery::DeliveryReceipt::not_dispatched(),
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

async fn history_entry_or_error(
    history_manager: &Arc<HistoryManager>,
    id: i64,
) -> Result<HistoryEntry, String> {
    history_manager
        .get_entry_by_id(id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("History entry {} not found", id))
}

#[tauri::command]
#[specta::specta]
pub async fn retry_history_entry_transcription(
    app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    transcription_manager: State<'_, Arc<TranscriptionManager>>,
    id: i64,
) -> Result<(), String> {
    let entry = history_entry_or_error(&history_manager, id).await?;
    // Retries use a new frozen run but deliberately do not capture Sona's
    // history window as application context.
    let run = crate::modes::RunPlan::for_retry(
        &crate::settings::get_settings(&app),
        entry.post_process_requested,
    )
    .map_err(|error| error.to_string())?;
    replay_stored_recording(&app, &history_manager, &transcription_manager, entry, run).await
}

/// Runs a stored recording through a different mode than the one that produced
/// it. The original entry is never mutated; the result is a new entry whose
/// `parent_id` points back at it.
#[tauri::command]
#[specta::specta]
pub async fn reprocess_history_entry(
    app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    transcription_manager: State<'_, Arc<TranscriptionManager>>,
    id: i64,
    mode_id: String,
) -> Result<(), String> {
    let entry = history_entry_or_error(&history_manager, id).await?;
    let run = crate::modes::RunPlan::for_reprocess(&crate::settings::get_settings(&app), &mode_id)
        .map_err(|error| error.to_string())?;
    replay_stored_recording(&app, &history_manager, &transcription_manager, entry, run).await
}

fn current_time_ms() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0)
}

#[tauri::command]
#[specta::specta]
pub async fn update_history_limit(
    app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    limit: usize,
) -> Result<(), String> {
    crate::settings::update_settings(&app, |settings| {
        settings.history_limit = limit;
    })?;

    history_manager
        .cleanup_old_entries()
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn update_recording_retention_period(
    app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    period: String,
) -> Result<(), String> {
    use crate::settings::RecordingRetentionPeriod;

    /* Parse through serde rather than a second hand-written table: the enum's
     * own aliases already accept both the bindings spelling the UI sends and
     * the older digit-attached one, and a table here would drift from them. */
    let retention_period: RecordingRetentionPeriod =
        serde_json::from_value(serde_json::Value::String(period.clone()))
            .map_err(|_| format!("Invalid retention period: {}", period))?;

    crate::settings::update_settings(&app, |settings| {
        settings.recording_retention_period = retention_period;
    })?;

    history_manager
        .cleanup_old_entries()
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_audio_reads_are_bounded_and_path_free() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("recording.wav");
        let payload = vec![0xA5; HISTORY_AUDIO_CHUNK_BYTES + 17];
        fs::write(&path, &payload).expect("recording fixture");

        let first = read_history_audio_chunk_from_path(&path, 0).expect("first chunk");
        assert_eq!(first.bytes.len(), HISTORY_AUDIO_CHUNK_BYTES);
        assert!(!first.eof);

        let last = read_history_audio_chunk_from_path(
            &path,
            u64::try_from(HISTORY_AUDIO_CHUNK_BYTES).expect("chunk offset"),
        )
        .expect("last chunk");
        assert_eq!(last.bytes, vec![0xA5; 17]);
        assert!(last.eof);

        let end = read_history_audio_chunk_from_path(
            &path,
            u64::try_from(payload.len()).expect("fixture length"),
        )
        .expect("end marker");
        assert!(end.bytes.is_empty());
        assert!(end.eof);

        let serialized = serde_json::to_string(&first).expect("chunk serialization");
        assert!(!serialized.contains(path.to_string_lossy().as_ref()));
    }

    #[cfg(unix)]
    #[test]
    fn history_audio_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temporary directory");
        let target = directory.path().join("recording.wav");
        let link = directory.path().join("recording-link.wav");
        fs::write(&target, [0xA5]).expect("recording fixture");
        symlink(&target, &link).expect("fixture symlink");

        assert!(read_history_audio_chunk_from_path(&link, 0).is_err());
    }
}

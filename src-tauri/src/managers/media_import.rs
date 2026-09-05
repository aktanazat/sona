use crate::audio_toolkit::{audio::FrameResampler, constants::WHISPER_SAMPLE_RATE};
use crate::context::ContextReceipt;
use crate::managers::history::{HistoryManager, HistorySourceKind, NewRunReceipt};
use crate::managers::transcription::TranscriptionManager;
use crate::meeting::session::{ImportRecordingRequest, RecordingOrigin};
use crate::meeting::types::MeetingSessionId;
use crate::modes::{AsrPlan, ModeReceipt, RunPlan};
use anyhow::Result as AnyResult;
use parking_lot::{Condvar, Mutex};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::thread;
use std::time::Duration;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use tauri::{AppHandle, Manager};
use tauri_specta::Event;

pub const MAX_MEDIA_IMPORT_SAMPLES: usize = 28_800_000;
// Five seconds of fixed 16 kHz mono ASR output.
const IMPORT_PROGRESS_SAMPLES: usize = 80_000;
const SUPPORTED_MEDIA_EXTENSIONS: &[&str] = &[
    "wav", "mp3", "m4a", "aac", "flac", "ogg", "mov", "mp4", "m4v",
];

/// How long a recording the operating system hands to Sona has to run before
/// it is a meeting rather than a dictation.
///
/// Two minutes. A dictation is spoken into a microphone in one breath or a
/// few, and the ones this app records are seconds long; a file this long that
/// arrived through Open With is a recording *of* something — a call, an
/// interview, a talk. That route offers no destination and asks no question,
/// so the length of the audio is the only evidence the import has, and two
/// minutes is where the two populations stop overlapping. It is deliberately
/// not a setting: a number the person has to guess at is a worse question
/// than the one the in-app picker already asks out loud.
const MEETING_IMPORT_MIN_DURATION: Duration = Duration::from_secs(120);

/// The same threshold counted the way this decode counts, in emitted 16 kHz
/// mono samples. Container metadata is not trusted anywhere on this path, so
/// the comparison is against audio that actually came out of the decoder.
const MEETING_IMPORT_MIN_SAMPLES: usize =
    MEETING_IMPORT_MIN_DURATION.as_secs() as usize * WHISPER_SAMPLE_RATE as usize;

static NEXT_MEDIA_IMPORT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AudioImportStatus {
    Queued,
    Decoding,
    Transcribing,
    Done,
    Cancelled,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AudioImportFailureCode {
    InvalidFile,
    UnsupportedFormat,
    NoAudio,
    Decode,
    DurationLimit,
    Transcription,
    History,
    MeetingImport,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AudioImportResult {
    Done {
        history_id: i64,
    },
    /// Long enough to be a recording of something rather than a dictation, so
    /// it became a meeting and no history row exists. Carries the id
    /// `sona://meeting/<id>` addresses.
    Meeting {
        session_id: MeetingSessionId,
    },
    Cancelled,
    Failed {
        code: AudioImportFailureCode,
        message: String,
    },
}

/// The complete public state for one GUI import. Source paths remain private;
/// only the original file name crosses the IPC boundary.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
pub struct AudioImportJob {
    pub id: u64,
    pub file_name: String,
    pub status: AudioImportStatus,
    pub decoded_samples: u64,
    pub cancel_requested: bool,
    pub result: Option<AudioImportResult>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type, tauri_specta::Event)]
pub struct AudioImportUpdateEvent {
    pub job: AudioImportJob,
}

/// Which of Sona's two homes for recorded speech one import landed in.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AudioImportDestination {
    Meeting,
    Dictation,
}

/// Where one file the operating system handed to Sona ended up.
///
/// Emitted only for that route, because it is the only one where the person
/// was never asked: they chose Open With, and the length of the audio chose
/// the destination. `link` is the `sona://` address of the meeting or the
/// dictation, so the toast that reports this can open the thing it names.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, Type, tauri_specta::Event)]
pub struct AudioImportRoutedEvent {
    pub file_name: String,
    pub destination: AudioImportDestination,
    pub link: String,
}

#[derive(Clone, Debug)]
pub struct AudioImportError {
    code: AudioImportFailureCode,
    message: &'static str,
}

impl AudioImportError {
    fn invalid_file() -> Self {
        Self {
            code: AudioImportFailureCode::InvalidFile,
            message: "Select a readable audio file.",
        }
    }

    fn unsupported_format() -> Self {
        Self {
            code: AudioImportFailureCode::UnsupportedFormat,
            message: "This media format is not supported.",
        }
    }

    fn no_audio() -> Self {
        Self {
            code: AudioImportFailureCode::NoAudio,
            message: "This media file has no audio track.",
        }
    }

    /// Also the token a streaming caller returns from its own sink to stop the
    /// decode; the caller reports its real reason itself.
    pub(crate) fn decode() -> Self {
        Self {
            code: AudioImportFailureCode::Decode,
            message: "The media file could not be decoded.",
        }
    }

    fn duration_limit() -> Self {
        Self {
            code: AudioImportFailureCode::DurationLimit,
            message: "Imported audio is limited to 30 minutes.",
        }
    }

    fn transcription() -> Self {
        Self {
            code: AudioImportFailureCode::Transcription,
            message: "The audio could not be transcribed.",
        }
    }

    fn history() -> Self {
        Self {
            code: AudioImportFailureCode::History,
            message: "The transcript could not be saved to history.",
        }
    }

    fn meeting_import() -> Self {
        Self {
            code: AudioImportFailureCode::MeetingImport,
            message: "The recording could not be saved as a meeting.",
        }
    }

    pub fn code(&self) -> AudioImportFailureCode {
        self.code
    }

    pub fn message(&self) -> &'static str {
        self.message
    }
}

impl std::fmt::Display for AudioImportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for AudioImportError {}

#[derive(Debug)]
pub(crate) struct ValidatedMediaPath {
    pub(crate) canonical_path: PathBuf,
    pub(crate) extension: String,
    pub(crate) file_name: String,
}

/// Where the request to import one file came from.
///
/// The in-app picker asked which of the two destinations the person wanted,
/// and this queue only ever receives the dictation half of that answer. An
/// operating-system open asked nothing: Finder's Open With, `open -a Sona`
/// and a drop on the Dock icon all hand over a path and no intention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImportOrigin {
    Picker,
    SystemOpen,
}

impl ImportOrigin {
    /// The decoded length at which this import stops being a dictation, or
    /// `None` when nothing the decode finds can change its destination.
    const fn meeting_threshold_samples(self) -> Option<usize> {
        match self {
            Self::Picker => None,
            Self::SystemOpen => Some(MEETING_IMPORT_MIN_SAMPLES),
        }
    }
}

struct PendingJob {
    canonical_path: PathBuf,
    extension: String,
    run: RunPlan,
    origin: ImportOrigin,
    cancellation: Arc<AtomicBool>,
    public: AudioImportJob,
}

struct WorkItem {
    id: u64,
    canonical_path: PathBuf,
    extension: String,
    run: RunPlan,
    origin: ImportOrigin,
    cancellation: Arc<AtomicBool>,
}

#[derive(Default)]
struct ImportState {
    jobs: BTreeMap<u64, PendingJob>,
    queue: VecDeque<u64>,
    active: Option<u64>,
}

struct ImportHistoryRecord {
    file_name: String,
    transcription: String,
    run: ModeReceipt,
    context: ContextReceipt,
    started_at_ms: u64,
    duration_ms: Option<u64>,
    word_count: Option<u64>,
}

/// Keeps model unloading suspended for the duration of one import job. The
/// marker has no behavior beyond Drop; it is deliberately opaque to the queue.
trait ImportActivity: Send {}
impl<T: Send> ImportActivity for T {}

trait ImportRuntime: Send + Sync {
    fn begin_job(&self) -> Box<dyn ImportActivity>;
    fn transcribe(&self, plan: &AsrPlan, audio: &[f32]) -> AnyResult<String>;
    fn save(&self, record: ImportHistoryRecord) -> AnyResult<i64>;
    /// Hand one opened recording to the meeting pipeline, blocking until the
    /// meeting exists. Blocking is what this queue's single worker is for, and
    /// the caller needs the meeting's id to report where the file went.
    fn import_meeting(&self, path: &Path) -> AnyResult<MeetingSessionId>;
    /// Report where one opened file landed.
    fn announce(&self, routed: AudioImportRoutedEvent);
}

struct AppImportRuntime {
    app_handle: AppHandle,
    transcription: Arc<TranscriptionManager>,
    history: Arc<HistoryManager>,
}

impl ImportRuntime for AppImportRuntime {
    fn begin_job(&self) -> Box<dyn ImportActivity> {
        Box::new(self.transcription.begin_media_import())
    }

    fn transcribe(&self, plan: &AsrPlan, audio: &[f32]) -> AnyResult<String> {
        self.transcription
            .transcribe_shared(plan, audio)
            .map(|decode| decode.text)
    }

    fn save(&self, record: ImportHistoryRecord) -> AnyResult<i64> {
        let entry = self.history.save_entry_with_receipt(
            record.file_name,
            record.transcription,
            false,
            None,
            Some(NewRunReceipt {
                run: record.run,
                context: record.context,
                started_at_ms: record.started_at_ms,
                completed_at_ms: current_time_ms(),
                duration_ms: record.duration_ms,
                word_count: record.word_count,
                source_kind: HistorySourceKind::File,
                has_audio: false,
                capture_status: None,
            }),
        )?;
        Ok(entry.id)
    }

    fn import_meeting(&self, path: &Path) -> AnyResult<MeetingSessionId> {
        let manager = self
            .app_handle
            .try_state::<Arc<crate::meeting::session::MeetingSessionManager>>()
            .ok_or_else(|| anyhow::anyhow!("no meeting session manager is running"))?;
        // Title and recording time are left unset on purpose: the import reads
        // the file name and the file's own modification time, which are the
        // two facts a file the operating system handed over actually carries.
        // The bytes are already on this Mac, so the origin is a local file
        // whichever application wrote them.
        let snapshot = tauri::async_runtime::block_on(Arc::clone(&manager).import_recording(
            ImportRecordingRequest {
                path: path.to_path_buf(),
                title: None,
                recorded_at_utc_ms: None,
                origin: RecordingOrigin::LocalFile,
            },
        ))
        .map_err(|error| anyhow::anyhow!("the meeting import refused the file: {error:?}"))?;
        Ok(snapshot.session_id)
    }

    fn announce(&self, routed: AudioImportRoutedEvent) {
        if let Err(error) = routed.emit(&self.app_handle) {
            log::warn!("Failed to report where an opened file landed: {error}");
        }
    }
}

struct MediaImportInner {
    app_handle: Option<AppHandle>,
    runtime: Arc<dyn ImportRuntime>,
    state: Mutex<ImportState>,
    wake: Condvar,
    shutdown: AtomicBool,
}

/// A single-worker FIFO for bounded, local media transcription.
///
/// The manager intentionally never owns source media bytes after a job ends and
/// never calls the post-processing, context, or delivery pipeline.
pub struct MediaImportManager {
    inner: Arc<MediaImportInner>,
}

impl MediaImportManager {
    pub fn new(
        app_handle: &AppHandle,
        transcription: Arc<TranscriptionManager>,
        history: Arc<HistoryManager>,
    ) -> Self {
        Self::with_runtime(
            Some(app_handle.clone()),
            Arc::new(AppImportRuntime {
                app_handle: app_handle.clone(),
                transcription,
                history,
            }),
        )
    }

    fn with_runtime(app_handle: Option<AppHandle>, runtime: Arc<dyn ImportRuntime>) -> Self {
        let inner = Arc::new(MediaImportInner {
            app_handle,
            runtime,
            state: Mutex::new(ImportState::default()),
            wake: Condvar::new(),
            shutdown: AtomicBool::new(false),
        });
        let worker_inner = Arc::downgrade(&inner);
        thread::spawn(move || worker_loop(worker_inner));
        Self { inner }
    }

    /// Enqueue an already-frozen active-mode plan after validating the supplied
    /// path. The canonical path is retained only by the worker and is never
    /// serialized or emitted.
    ///
    /// `origin` is the one thing the queue cannot work out for itself: whether
    /// a person chose this destination or the operating system chose Sona.
    pub fn enqueue(
        &self,
        path: String,
        run: RunPlan,
        origin: ImportOrigin,
    ) -> std::result::Result<AudioImportJob, AudioImportError> {
        let path = validate_media_path(Path::new(&path))?;
        let id = NEXT_MEDIA_IMPORT_ID.fetch_add(1, Ordering::Relaxed);
        let public = AudioImportJob {
            id,
            file_name: path.file_name,
            status: AudioImportStatus::Queued,
            decoded_samples: 0,
            cancel_requested: false,
            result: None,
        };
        {
            let mut state = lock_state(&self.inner);
            state.jobs.insert(
                id,
                PendingJob {
                    canonical_path: path.canonical_path,
                    extension: path.extension,
                    run,
                    origin,
                    cancellation: Arc::new(AtomicBool::new(false)),
                    public: public.clone(),
                },
            );
            state.queue.push_back(id);
        }
        emit_update(&self.inner, public.clone());
        self.inner.wake.notify_one();
        Ok(public)
    }

    pub fn cancel(&self, id: u64) -> std::result::Result<AudioImportJob, AudioImportError> {
        let update = {
            let mut state = lock_state(&self.inner);
            let was_queued = state.queue.contains(&id);
            if was_queued {
                state.queue.retain(|queued_id| *queued_id != id);
            }
            let job = state
                .jobs
                .get_mut(&id)
                .ok_or_else(AudioImportError::invalid_file)?;
            if matches!(
                job.public.status,
                AudioImportStatus::Done | AudioImportStatus::Cancelled | AudioImportStatus::Failed
            ) {
                return Ok(job.public.clone());
            }
            job.cancellation.store(true, Ordering::Release);
            job.public.cancel_requested = true;
            if was_queued {
                job.public.status = AudioImportStatus::Cancelled;
                job.public.result = Some(AudioImportResult::Cancelled);
            }
            job.public.clone()
        };
        emit_update(&self.inner, update.clone());
        self.inner.wake.notify_one();
        Ok(update)
    }

    pub fn list_jobs(&self) -> Vec<AudioImportJob> {
        let state = lock_state(&self.inner);
        state.jobs.values().map(|job| job.public.clone()).collect()
    }

    #[cfg(test)]
    fn new_for_test(runtime: Arc<dyn ImportRuntime>) -> Self {
        Self::with_runtime(None, runtime)
    }
}

impl Drop for MediaImportManager {
    fn drop(&mut self) {
        self.inner.shutdown.store(true, Ordering::Release);
        self.inner.wake.notify_all();
    }
}

fn worker_loop(inner: Weak<MediaImportInner>) {
    loop {
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let Some(work) = next_work(&inner) else {
            return;
        };
        process_work(&inner, work);
    }
}
fn next_work(inner: &Arc<MediaImportInner>) -> Option<WorkItem> {
    let mut state = lock_state(inner);
    loop {
        if inner.shutdown.load(Ordering::Acquire) {
            return None;
        }
        if let Some(id) = state.queue.pop_front() {
            let work = state.jobs.get(&id).map(|job| WorkItem {
                id,
                canonical_path: job.canonical_path.clone(),
                extension: job.extension.clone(),
                run: job.run.clone(),
                origin: job.origin,
                cancellation: Arc::clone(&job.cancellation),
            })?;
            state.active = Some(id);
            return Some(work);
        }
        inner.wake.wait(&mut state);
    }
}

fn process_work(inner: &Arc<MediaImportInner>, work: WorkItem) {
    let _activity = inner.runtime.begin_job();
    if work.cancellation.load(Ordering::Acquire) {
        finish_cancelled(inner, work.id);
        return;
    }

    set_status(inner, work.id, AudioImportStatus::Decoding);
    let decoded = decode_media(
        &work.canonical_path,
        &work.extension,
        &work.cancellation,
        work.origin.meeting_threshold_samples(),
        |emitted_samples| update_decode_progress(inner, work.id, emitted_samples),
    );

    let audio = match decoded {
        Ok(DecodedMedia::Whole(audio)) => audio,
        Ok(DecodedMedia::ReachedStopPoint) => {
            finish_as_meeting(inner, &work);
            return;
        }
        Err(DecodeFailure::Cancelled) => {
            finish_cancelled(inner, work.id);
            return;
        }
        Err(DecodeFailure::Failed(error)) => {
            finish_failed(inner, work.id, error);
            return;
        }
    };

    update_decode_progress(inner, work.id, audio.len());
    // Do not transition to native inference after a cancellation request. The
    // native engines are intentionally not interrupted mid-call because their
    // ownership is serialized by TranscriptionManager.
    if work.cancellation.load(Ordering::Acquire) {
        finish_cancelled(inner, work.id);
        return;
    }

    set_status(inner, work.id, AudioImportStatus::Transcribing);
    let transcription = inner.runtime.transcribe(work.run.asr(), &audio);
    if work.cancellation.load(Ordering::Acquire) {
        finish_cancelled(inner, work.id);
        return;
    }
    let transcription = match transcription {
        Ok(text) => text,
        Err(error) => {
            log::warn!("Media import transcription failed: {error}");
            finish_failed(inner, work.id, AudioImportError::transcription());
            return;
        }
    };

    let Some(file_name) = current_file_name(inner, work.id) else {
        finish_failed(inner, work.id, AudioImportError::history());
        return;
    };
    let source_name = file_name.clone();
    let record = ImportHistoryRecord {
        file_name,
        word_count: u64::try_from(transcription.split_whitespace().count()).ok(),
        duration_ms: duration_ms(audio.len()),
        transcription,
        run: work.run.mode_receipt(),
        context: work.run.context().receipt().clone(),
        started_at_ms: work.run.run_started_at_ms,
    };
    let history_id = match inner.runtime.save(record) {
        Ok(history_id) => history_id,
        Err(error) => {
            log::warn!("Media import history save failed: {error}");
            finish_failed(inner, work.id, AudioImportError::history());
            return;
        }
    };
    finish_done(inner, work.id, history_id, source_name, work.origin);
}

fn set_status(inner: &Arc<MediaImportInner>, id: u64, status: AudioImportStatus) {
    let update = {
        let mut state = lock_state(inner);
        let Some(job) = state.jobs.get_mut(&id) else {
            return;
        };
        job.public.status = status;
        job.public.clone()
    };
    emit_update(inner, update);
}

fn update_decode_progress(inner: &Arc<MediaImportInner>, id: u64, emitted_samples: usize) {
    let update = {
        let mut state = lock_state(inner);
        let Some(job) = state.jobs.get_mut(&id) else {
            return;
        };
        job.public.decoded_samples = u64::try_from(emitted_samples).unwrap_or(u64::MAX);
        job.public.clone()
    };
    emit_update(inner, update);
}

fn finish_done(
    inner: &Arc<MediaImportInner>,
    id: u64,
    history_id: i64,
    source_name: String,
    origin: ImportOrigin,
) {
    finish(
        inner,
        id,
        AudioImportStatus::Done,
        AudioImportResult::Done { history_id },
    );
    if origin == ImportOrigin::SystemOpen {
        inner.runtime.announce(AudioImportRoutedEvent {
            file_name: source_name.clone(),
            destination: AudioImportDestination::Dictation,
            link: crate::query::dictation_link(history_id),
        });
    }
    let Some(app) = &inner.app_handle else {
        return;
    };
    let Some(manager) = app.try_state::<Arc<crate::meeting::session::MeetingSessionManager>>()
    else {
        return;
    };
    let manager = Arc::clone(&manager);
    tauri::async_runtime::spawn(async move {
        manager
            .record_audio_imported(history_id.to_string(), source_name)
            .await;
    });
}

/// Turn one opened file into a meeting instead of a dictation, and finish the
/// queue row with the meeting it became.
///
/// The prefix the decode collected is already dropped by the time this runs:
/// the meeting import decodes the file again, straight onto its own track on
/// disk, which is the only path in the app whose ceiling is high enough for a
/// recording of any length. Reachable only from `ImportOrigin::SystemOpen`,
/// because that is the only origin that sets a stop point.
fn finish_as_meeting(inner: &Arc<MediaImportInner>, work: &WorkItem) {
    let Some(file_name) = current_file_name(inner, work.id) else {
        finish_failed(inner, work.id, AudioImportError::meeting_import());
        return;
    };
    let session_id = match inner.runtime.import_meeting(&work.canonical_path) {
        Ok(session_id) => session_id,
        Err(error) => {
            log::warn!("Meeting import of an opened file failed: {error}");
            finish_failed(inner, work.id, AudioImportError::meeting_import());
            return;
        }
    };
    finish(
        inner,
        work.id,
        AudioImportStatus::Done,
        AudioImportResult::Meeting { session_id },
    );
    inner.runtime.announce(AudioImportRoutedEvent {
        file_name,
        destination: AudioImportDestination::Meeting,
        link: crate::query::meeting_link(session_id),
    });
}

fn finish_cancelled(inner: &Arc<MediaImportInner>, id: u64) {
    finish(
        inner,
        id,
        AudioImportStatus::Cancelled,
        AudioImportResult::Cancelled,
    );
}

fn finish_failed(inner: &Arc<MediaImportInner>, id: u64, error: AudioImportError) {
    finish(
        inner,
        id,
        AudioImportStatus::Failed,
        AudioImportResult::Failed {
            code: error.code(),
            message: error.message().to_string(),
        },
    );
}

fn finish(
    inner: &Arc<MediaImportInner>,
    id: u64,
    status: AudioImportStatus,
    result: AudioImportResult,
) {
    let update = {
        let mut state = lock_state(inner);
        let update = {
            let Some(job) = state.jobs.get_mut(&id) else {
                return;
            };
            job.public.status = status;
            job.public.result = Some(result);
            job.public.clone()
        };
        if state.active == Some(id) {
            state.active = None;
        }
        update
    };
    emit_update(inner, update);
}
fn current_file_name(inner: &Arc<MediaImportInner>, id: u64) -> Option<String> {
    let state = lock_state(inner);
    state.jobs.get(&id).map(|job| job.public.file_name.clone())
}

fn emit_update(inner: &MediaImportInner, job: AudioImportJob) {
    if let Some(app_handle) = &inner.app_handle {
        let _ = AudioImportUpdateEvent { job }.emit(app_handle);
    }
}

fn lock_state(inner: &MediaImportInner) -> parking_lot::MutexGuard<'_, ImportState> {
    inner.state.lock()
}

pub(crate) fn validate_audio_import_path(path: &Path) -> std::result::Result<(), AudioImportError> {
    validate_media_path(path).map(|_| ())
}

pub(crate) fn validate_media_path(
    path: &Path,
) -> std::result::Result<ValidatedMediaPath, AudioImportError> {
    let canonical_path = fs::canonicalize(path).map_err(|_| AudioImportError::invalid_file())?;
    let metadata = fs::metadata(&canonical_path).map_err(|_| AudioImportError::invalid_file())?;
    if !metadata.file_type().is_file() {
        return Err(AudioImportError::invalid_file());
    }
    let file_name = path
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(AudioImportError::invalid_file)?;
    let extension = canonical_path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|value| is_supported_extension(value))
        .ok_or_else(AudioImportError::unsupported_format)?;
    Ok(ValidatedMediaPath {
        canonical_path,
        extension,
        file_name,
    })
}

fn is_supported_extension(extension: &str) -> bool {
    SUPPORTED_MEDIA_EXTENSIONS.contains(&extension)
}

#[derive(Debug)]
pub(crate) enum DecodeFailure {
    Cancelled,
    Failed(AudioImportError),
}

/// What a bounded dictation decode found.
enum DecodedMedia {
    /// The whole file, in one buffer, under the caller's stop point.
    Whole(Vec<f32>),
    /// The file is at least `stop_after` samples long, so the decode was
    /// abandoned there and nothing was kept. The caller asked to be told this
    /// rather than to hold the rest.
    ReachedStopPoint,
}

/// Decode one media file into a fixed 16 kHz mono f32 stream, appended whole.
/// Used by the dictation-history import, which needs the audio in one buffer to
/// hand to a single ASR call.
///
/// `stop_after` is the length at which this buffer becomes the wrong shape for
/// the file — a recording rather than a dictation. Reaching it is an answer,
/// not a failure, so the decode stops and says so.
fn decode_media(
    path: &Path,
    extension: &str,
    cancellation: &AtomicBool,
    stop_after: Option<usize>,
    mut progress: impl FnMut(usize),
) -> std::result::Result<DecodedMedia, DecodeFailure> {
    let mut output = Vec::new();
    let mut next_progress = IMPORT_PROGRESS_SAMPLES;
    let mut reached_stop_point = false;
    let decoded = decode_media_into(
        path,
        extension,
        cancellation,
        MAX_MEDIA_IMPORT_SAMPLES,
        |frame| {
            append_emitted(&mut output, frame)?;
            if output.len() >= next_progress {
                progress(output.len());
                next_progress = next_progress.saturating_add(IMPORT_PROGRESS_SAMPLES);
            }
            if stop_after.is_some_and(|stop_point| output.len() >= stop_point) {
                // The error only unwinds the decoder's loop. Nothing reads it:
                // `reached_stop_point` is the answer, and it outranks whatever
                // the unwind reports.
                reached_stop_point = true;
                return Err(AudioImportError::duration_limit());
            }
            Ok(())
        },
    );
    if reached_stop_point {
        return Ok(DecodedMedia::ReachedStopPoint);
    }
    decoded?;
    Ok(DecodedMedia::Whole(output))
}

/// The one Symphonia decode path in the app: probe, pick the audio track,
/// downmix to mono and resample to `WHISPER_SAMPLE_RATE`, handing each
/// resampled frame to `emit` as it is produced.
///
/// `max_samples` is the caller's memory or duration ceiling in emitted 16 kHz
/// samples, checked against what has actually been emitted rather than against
/// container metadata. A caller that streams its frames straight to disk can
/// pass a ceiling far above what it could hold in memory; one that accumulates
/// cannot. Returns the number of samples emitted.
pub(crate) fn decode_media_into(
    path: &Path,
    extension: &str,
    cancellation: &AtomicBool,
    max_samples: usize,
    mut emit: impl FnMut(&[f32]) -> std::result::Result<(), AudioImportError>,
) -> std::result::Result<usize, DecodeFailure> {
    let file =
        File::open(path).map_err(|_| DecodeFailure::Failed(AudioImportError::invalid_file()))?;
    let media_stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    hint.with_extension(extension);
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            media_stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|_| DecodeFailure::Failed(AudioImportError::decode()))?;
    let mut format = probed.format;
    let mut has_audio_track = false;
    let mut selected = None;
    for track in format.tracks() {
        let params = &track.codec_params;
        // Video and metadata tracks do not declare a sample rate or channel
        // layout. Try only tracks that identify themselves as audio, then let
        // the enabled Symphonia codecs decide whether the audio codec is one
        // Sona can decode.
        if params.sample_rate.is_none() && params.channels.is_none() {
            continue;
        }
        has_audio_track = true;
        if let Ok(decoder) =
            symphonia::default::get_codecs().make(params, &DecoderOptions::default())
        {
            selected = Some((track.id, params.clone(), decoder));
            break;
        }
    }
    let (track_id, codec_params, mut decoder) = selected.ok_or_else(|| {
        DecodeFailure::Failed(if has_audio_track {
            AudioImportError::unsupported_format()
        } else {
            AudioImportError::no_audio()
        })
    })?;
    // WAV exposes an exact PCM frame count in its container header. Use it only
    // to reject truncation; duration admission remains based on emitted PCM.
    let expected_wav_frames = (extension == "wav")
        .then_some(codec_params.n_frames)
        .flatten();

    let mut resampler: Option<FrameResampler> = None;
    let mut source_rate = 0_u32;
    let mut channels = 0_usize;
    let mut sample_buffer: Option<SampleBuffer<f32>> = None;
    let mut sample_capacity = 0_usize;
    let mut downmixed = Vec::new();
    let mut sink = FrameSink {
        emit: &mut emit,
        max_samples,
        emitted: 0,
        failure: None,
    };
    let mut decoded_source_frames = 0_u64;

    loop {
        if cancellation.load(Ordering::Acquire) {
            return Err(DecodeFailure::Cancelled);
        }
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(_) => return Err(DecodeFailure::Failed(AudioImportError::decode())),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(SymphoniaError::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(_) => return Err(DecodeFailure::Failed(AudioImportError::decode())),
        };
        if cancellation.load(Ordering::Acquire) {
            return Err(DecodeFailure::Cancelled);
        }
        let packet_frames = u64::try_from(decoded.frames())
            .map_err(|_| DecodeFailure::Failed(AudioImportError::decode()))?;
        decoded_source_frames = decoded_source_frames
            .checked_add(packet_frames)
            .ok_or_else(|| DecodeFailure::Failed(AudioImportError::decode()))?;

        let spec = *decoded.spec();
        let packet_channels = spec.channels.count();
        if spec.rate == 0 || packet_channels == 0 {
            return Err(DecodeFailure::Failed(AudioImportError::decode()));
        }
        let channel_count = u16::try_from(packet_channels)
            .map_err(|_| DecodeFailure::Failed(AudioImportError::decode()))?;
        let downmix_scale = 1.0 / f32::from(channel_count);
        if source_rate == 0 {
            source_rate = spec.rate;
            channels = packet_channels;
            let input_sample_rate = usize::try_from(source_rate)
                .map_err(|_| DecodeFailure::Failed(AudioImportError::decode()))?;
            let output_sample_rate = usize::try_from(WHISPER_SAMPLE_RATE)
                .map_err(|_| DecodeFailure::Failed(AudioImportError::decode()))?;
            resampler = Some(FrameResampler::new(
                input_sample_rate,
                output_sample_rate,
                Duration::from_millis(30),
            ));
        } else if source_rate != spec.rate || channels != packet_channels {
            return Err(DecodeFailure::Failed(AudioImportError::decode()));
        }

        let buffer_frames = decoded.capacity();
        let packet_capacity = buffer_frames
            .checked_mul(packet_channels)
            .ok_or_else(|| DecodeFailure::Failed(AudioImportError::decode()))?;
        if sample_buffer.is_none() || sample_capacity < packet_capacity {
            let duration = u64::try_from(buffer_frames)
                .map_err(|_| DecodeFailure::Failed(AudioImportError::decode()))?;
            sample_buffer = Some(SampleBuffer::<f32>::new(duration, spec));
            sample_capacity = packet_capacity;
        }
        let Some(buffer) = sample_buffer.as_mut() else {
            return Err(DecodeFailure::Failed(AudioImportError::decode()));
        };
        buffer.copy_interleaved_ref(decoded);
        let samples = buffer.samples();
        if samples.len() % packet_channels != 0 {
            return Err(DecodeFailure::Failed(AudioImportError::decode()));
        }

        downmixed.clear();
        downmixed
            .try_reserve_exact(samples.len() / packet_channels)
            .map_err(|_| DecodeFailure::Failed(AudioImportError::decode()))?;
        for frame in samples.chunks_exact(packet_channels) {
            downmixed.push(frame.iter().copied().sum::<f32>() * downmix_scale);
        }

        let Some(resampler) = resampler.as_mut() else {
            return Err(DecodeFailure::Failed(AudioImportError::decode()));
        };
        resampler.push(&downmixed, |frame| sink.accept(frame));
        if let Some(error) = sink.failure.take() {
            return Err(DecodeFailure::Failed(error));
        }
    }

    if cancellation.load(Ordering::Acquire) {
        return Err(DecodeFailure::Cancelled);
    }
    if expected_wav_frames.is_some_and(|expected| expected != decoded_source_frames) {
        return Err(DecodeFailure::Failed(AudioImportError::decode()));
    }
    let Some(resampler) = resampler.as_mut() else {
        return Err(DecodeFailure::Failed(AudioImportError::decode()));
    };
    resampler.finish(|frame| sink.accept(frame));
    if let Some(error) = sink.failure.take() {
        return Err(DecodeFailure::Failed(error));
    }
    if sink.emitted == 0 {
        return Err(DecodeFailure::Failed(AudioImportError::decode()));
    }
    Ok(sink.emitted)
}

/// The resampler's output side of one decode.
///
/// `FrameResampler` emits through an infallible callback, so a refusal — the
/// caller's ceiling, or the caller's own write failing — is parked here and
/// collected by the decode loop that owns the error type. Once parked, later
/// frames are dropped rather than partially applied.
struct FrameSink<'emit, F> {
    emit: &'emit mut F,
    max_samples: usize,
    emitted: usize,
    failure: Option<AudioImportError>,
}

impl<F: FnMut(&[f32]) -> std::result::Result<(), AudioImportError>> FrameSink<'_, F> {
    fn accept(&mut self, frame: &[f32]) {
        if self.failure.is_some() {
            return;
        }
        if exceeds_sample_cap(self.emitted, frame.len(), self.max_samples) {
            self.failure = Some(AudioImportError::duration_limit());
            return;
        }
        match (self.emit)(frame) {
            Ok(()) => self.emitted = self.emitted.saturating_add(frame.len()),
            Err(error) => self.failure = Some(error),
        }
    }
}

fn exceeds_sample_cap(emitted_samples: usize, incoming_samples: usize, max_samples: usize) -> bool {
    emitted_samples
        .checked_add(incoming_samples)
        .is_none_or(|length| length > max_samples)
}

fn append_emitted(
    output: &mut Vec<f32>,
    frame: &[f32],
) -> std::result::Result<(), AudioImportError> {
    output
        .try_reserve_exact(frame.len())
        .map_err(|_| AudioImportError::decode())?;
    output.extend_from_slice(frame);
    Ok(())
}

fn duration_ms(samples: usize) -> Option<u64> {
    u64::try_from(samples)
        .ok()?
        .checked_mul(1_000)
        .map(|value| value / u64::from(WHISPER_SAMPLE_RATE))
}

fn current_time_ms() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::RunPlan;
    use crate::settings::get_default_settings;
    use hound::{SampleFormat, WavSpec, WavWriter};
    use std::collections::BTreeSet;
    use std::process::Command;

    #[derive(Deserialize)]
    struct TauriConfig {
        bundle: BundleConfig,
    }

    #[derive(Deserialize)]
    struct BundleConfig {
        #[serde(rename = "fileAssociations")]
        file_associations: Vec<FileAssociation>,
    }

    #[derive(Deserialize)]
    struct FileAssociation {
        ext: Vec<String>,
    }

    struct QueueGate {
        state: Mutex<(bool, bool)>,
        wake: Condvar,
    }

    impl QueueGate {
        fn new() -> Self {
            Self {
                state: Mutex::new((false, false)),
                wake: Condvar::new(),
            }
        }

        fn wait_until_entered(&self) {
            let mut state = self.state.lock();
            while !state.0 {
                if self
                    .wake
                    .wait_for(&mut state, Duration::from_secs(2))
                    .timed_out()
                {
                    panic!("fake transcriber did not start");
                }
            }
        }

        fn release(&self) {
            let mut state = self.state.lock();
            state.1 = true;
            self.wake.notify_all();
        }

        fn wait_for_release(&self) {
            let mut state = self.state.lock();
            state.0 = true;
            self.wake.notify_all();
            while !state.1 {
                self.wake.wait(&mut state);
            }
        }
    }

    #[derive(Default)]
    struct FakeRuntime {
        transcripts: Mutex<Vec<Vec<f32>>>,
        saved_names: Mutex<Vec<String>>,
        gate: Option<Arc<QueueGate>>,
        /// The paths handed to the meeting pipeline, in order. A path, not a
        /// buffer: the meeting import decodes the file itself.
        meeting_imports: Mutex<Vec<PathBuf>>,
        /// The meeting this fake creates, fixed for the life of the fake so a
        /// test can address the meeting it expects.
        meeting_id: MeetingSessionId,
        /// True when the meeting pipeline is unreachable, which is what an
        /// import that arrives before the meeting manager is running sees.
        refuse_meetings: bool,
        /// Every destination the queue reported, in order.
        announced: Mutex<Vec<AudioImportRoutedEvent>>,
    }

    impl FakeRuntime {
        fn blocking() -> (Self, Arc<QueueGate>) {
            let gate = Arc::new(QueueGate::new());
            (
                Self {
                    gate: Some(Arc::clone(&gate)),
                    ..Self::default()
                },
                gate,
            )
        }

        fn refusing_meetings() -> Self {
            Self {
                refuse_meetings: true,
                ..Self::default()
            }
        }
    }

    impl ImportRuntime for FakeRuntime {
        fn begin_job(&self) -> Box<dyn ImportActivity> {
            Box::new(())
        }

        fn transcribe(&self, _plan: &AsrPlan, audio: &[f32]) -> AnyResult<String> {
            if let Some(gate) = &self.gate {
                gate.wait_for_release();
            }
            self.transcripts.lock().push(audio.to_vec());
            Ok(format!("{} samples", audio.len()))
        }

        fn save(&self, record: ImportHistoryRecord) -> AnyResult<i64> {
            let mut saved_names = self.saved_names.lock();
            saved_names.push(record.file_name);
            Ok(i64::try_from(saved_names.len()).unwrap_or(i64::MAX))
        }

        fn import_meeting(&self, path: &Path) -> AnyResult<MeetingSessionId> {
            self.meeting_imports.lock().push(path.to_path_buf());
            if self.refuse_meetings {
                anyhow::bail!("no meeting session manager is running");
            }
            Ok(self.meeting_id)
        }

        fn announce(&self, routed: AudioImportRoutedEvent) {
            self.announced.lock().push(routed);
        }
    }

    fn import_plan() -> RunPlan {
        RunPlan::for_media_import(&get_default_settings()).expect("configured media-import plan")
    }

    fn write_stereo_wav(path: &Path, seconds: usize, left: f32, right: f32) {
        write_stereo_wav_at(path, 48_000, seconds, left, right);
    }

    /// The two-minute cases are written at the ASR rate on purpose. The
    /// boundary this route turns on is counted in emitted 16 kHz samples, and
    /// a 48 kHz source would make the resampler chew through three times the
    /// audio to arrive at the same count.
    fn write_asr_rate_wav(path: &Path, seconds: usize) {
        write_stereo_wav_at(path, WHISPER_SAMPLE_RATE, seconds, 0.1, 0.1);
    }

    fn write_stereo_wav_at(path: &Path, rate: u32, seconds: usize, left: f32, right: f32) {
        let spec = WavSpec {
            channels: 2,
            sample_rate: rate,
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        };
        let mut writer = WavWriter::create(path, spec).expect("create wav fixture");
        for _ in 0..(seconds * rate as usize) {
            writer.write_sample(left).expect("write left sample");
            writer.write_sample(right).expect("write right sample");
        }
        writer.finalize().expect("finalize wav fixture");
    }

    fn ffmpeg_fixture(source: &Path, output: &Path, codec: &[&str]) {
        let status = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(source)
            .args(codec)
            .arg(output)
            .status()
            .expect("ffmpeg is required to generate local codec fixtures");
        assert!(status.success(), "ffmpeg failed to generate fixture");
    }

    fn ffmpeg_video_fixture(source: &Path, output: &Path) {
        let status = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=16x16:r=1",
                "-i",
            ])
            .arg(source)
            .args([
                "-map",
                "0:v:0",
                "-map",
                "1:a:0",
                "-shortest",
                "-c:v",
                "mpeg4",
                "-c:a",
                "aac",
            ])
            .arg(output)
            .status()
            .expect("ffmpeg is required to generate local video fixtures");
        assert!(status.success(), "ffmpeg failed to generate video fixture");
    }

    fn ffmpeg_silent_video_fixture(output: &Path) {
        let status = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=16x16:r=1",
                "-t",
                "1",
                "-c:v",
                "mpeg4",
                "-an",
            ])
            .arg(output)
            .status()
            .expect("ffmpeg is required to generate local video fixtures");
        assert!(
            status.success(),
            "ffmpeg failed to generate silent video fixture"
        );
    }

    fn wait_for_terminal(manager: &MediaImportManager, id: u64) -> AudioImportJob {
        // Two minutes of audio has to decode inside this budget, so it is
        // patience for a slow machine, not a guess at how long a job takes.
        for _ in 0..3_000 {
            let job = manager
                .list_jobs()
                .into_iter()
                .find(|job| job.id == id)
                .expect("job exists");
            if matches!(
                job.status,
                AudioImportStatus::Done | AudioImportStatus::Cancelled | AudioImportStatus::Failed
            ) {
                return job;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("import job did not terminate");
    }

    /// The unbounded decode the picker route performs, with the outcome the
    /// caller of a stop-free decode expects: the whole file, in one buffer.
    fn decode_whole(
        path: &Path,
        extension: &str,
        cancellation: &AtomicBool,
    ) -> std::result::Result<Vec<f32>, DecodeFailure> {
        match decode_media(path, extension, cancellation, None, |_| {})? {
            DecodedMedia::Whole(audio) => Ok(audio),
            DecodedMedia::ReachedStopPoint => panic!("an unbounded decode has no stop point"),
        }
    }

    #[test]
    fn viewer_associations_keep_sona_out_of_audio_imports() {
        let config: TauriConfig = serde_json::from_str(include_str!("../../tauri.conf.json"))
            .expect("read Tauri bundle configuration");
        let associated = config
            .bundle
            .file_associations
            .into_iter()
            .flat_map(|association| association.ext)
            .collect::<BTreeSet<_>>();
        let supported = SUPPORTED_MEDIA_EXTENSIONS
            .iter()
            .map(|extension| (*extension).to_string())
            .collect::<BTreeSet<_>>();
        let non_audio = associated
            .difference(&supported)
            .cloned()
            .collect::<Vec<_>>();

        assert!(supported.is_subset(&associated));
        assert_eq!(non_audio, ["sona".to_string()]);
    }

    #[test]
    fn downmixes_and_resamples_deterministically() {
        let directory = tempfile::tempdir().expect("temporary fixture directory");
        let path = directory.path().join("stereo.wav");
        write_stereo_wav(&path, 3, 0.75, -0.25);
        let cancellation = AtomicBool::new(false);
        let decoded = decode_whole(&path, "wav", &cancellation).expect("decode wav");
        assert!(
            (47_000..=49_500).contains(&decoded.len()),
            "expected about 3 seconds at 16 kHz, got {} samples",
            decoded.len()
        );
        let average = decoded.iter().skip(1_000).take(1_000).copied().sum::<f32>() / 1_000.0;
        assert!(
            (average - 0.25).abs() < 0.02,
            "deterministic average {average}"
        );
    }

    #[test]
    fn codecs_are_generated_and_decoded_locally() {
        let directory = tempfile::tempdir().expect("temporary fixture directory");
        let source = directory.path().join("source.wav");
        write_stereo_wav(&source, 2, 0.3, 0.3);
        let cases: [(&str, &[&str]); 5] = [
            ("fixture.mp3", &["-c:a", "libmp3lame"]),
            ("fixture.m4a", &["-c:a", "aac"]),
            ("fixture.aac", &["-c:a", "aac", "-f", "adts"]),
            ("fixture.flac", &["-c:a", "flac"]),
            ("fixture.ogg", &["-strict", "-2", "-c:a", "vorbis"]),
        ];
        for (name, codec) in cases {
            let output = directory.path().join(name);
            ffmpeg_fixture(&source, &output, codec);
            let cancellation = AtomicBool::new(false);
            let extension = output.extension().unwrap().to_str().unwrap();
            let decoded = decode_whole(&output, extension, &cancellation)
                .unwrap_or_else(|_| panic!("failed to decode {name}"));
            assert!(
                (30_000..=38_400).contains(&decoded.len()),
                "{name} did not resample near 16 kHz: {} samples",
                decoded.len()
            );
        }
    }

    #[test]
    fn video_containers_transcribe_their_audio_track_with_original_name() {
        let directory = tempfile::tempdir().expect("temporary fixture directory");
        let source = directory.path().join("source.wav");
        write_stereo_wav(&source, 2, 0.3, 0.3);
        let runtime = Arc::new(FakeRuntime::default());
        let manager = MediaImportManager::new_for_test(runtime.clone());

        for extension in ["mov", "mp4", "m4v"] {
            let name = format!("meeting.{extension}");
            let video = directory.path().join(&name);
            ffmpeg_video_fixture(&source, &video);
            let job = manager
                .enqueue(
                    video.to_string_lossy().into_owned(),
                    import_plan(),
                    ImportOrigin::Picker,
                )
                .expect("enqueue supported video");

            assert_eq!(
                wait_for_terminal(&manager, job.id).status,
                AudioImportStatus::Done,
                "{name} should transcribe its audio track"
            );
        }

        let saved_names = runtime.saved_names.lock();
        assert_eq!(
            saved_names.as_slice(),
            [
                "meeting.mov".to_string(),
                "meeting.mp4".to_string(),
                "meeting.m4v".to_string(),
            ]
        );
        let transcripts = runtime.transcripts.lock();
        assert_eq!(transcripts.len(), 3);
        assert!(
            transcripts
                .iter()
                .all(|audio| (30_000..=38_400).contains(&audio.len())),
            "each video should yield approximately two seconds of 16 kHz audio"
        );
    }

    #[test]
    fn video_without_audio_returns_a_typed_no_audio_failure() {
        let directory = tempfile::tempdir().expect("temporary fixture directory");
        let video = directory.path().join("silent.mov");
        ffmpeg_silent_video_fixture(&video);
        let runtime = Arc::new(FakeRuntime::default());
        let manager = MediaImportManager::new_for_test(runtime.clone());
        let job = manager
            .enqueue(
                video.to_string_lossy().into_owned(),
                import_plan(),
                ImportOrigin::Picker,
            )
            .expect("enqueue supported video container");

        let completed = wait_for_terminal(&manager, job.id);
        assert_eq!(
            completed.result,
            Some(AudioImportResult::Failed {
                code: AudioImportFailureCode::NoAudio,
                message: "This media file has no audio track.".to_string(),
            })
        );
        assert!(runtime.saved_names.lock().is_empty());
    }

    #[test]
    fn corrupt_truncated_and_unsupported_files_fail_safely() {
        let directory = tempfile::tempdir().expect("temporary fixture directory");
        let corrupt = directory.path().join("corrupt.mp3");
        fs::write(&corrupt, b"not audio").expect("write corrupt fixture");
        let cancellation = AtomicBool::new(false);
        assert!(matches!(
            decode_media(&corrupt, "mp3", &cancellation, None, |_| {}),
            Err(DecodeFailure::Failed(_))
        ));

        let complete = directory.path().join("complete.wav");
        let truncated = directory.path().join("truncated.wav");
        write_stereo_wav(&complete, 1, 0.2, 0.2);
        let bytes = fs::read(&complete).expect("read complete fixture");
        fs::write(&truncated, &bytes[..bytes.len() / 2]).expect("write truncated fixture");
        assert!(matches!(
            decode_media(&truncated, "wav", &AtomicBool::new(false), None, |_| {}),
            Err(DecodeFailure::Failed(_))
        ));

        let unsupported = directory.path().join("unsupported.avi");
        fs::write(&unsupported, b"not a supported container").expect("write unsupported fixture");
        assert_eq!(
            validate_media_path(&unsupported).unwrap_err().code(),
            AudioImportFailureCode::UnsupportedFormat
        );
    }

    #[test]
    fn cap_is_checked_from_emitted_samples_without_metadata() {
        let cap = MAX_MEDIA_IMPORT_SAMPLES;
        assert!(!exceeds_sample_cap(cap, 0, cap));
        assert!(exceeds_sample_cap(cap, 1, cap));
        assert!(exceeds_sample_cap(usize::MAX, 1, cap));
        assert_eq!(duration_ms(MAX_MEDIA_IMPORT_SAMPLES), Some(1_800_000));
    }

    #[test]
    fn cancellation_stops_between_packets() {
        let directory = tempfile::tempdir().expect("temporary fixture directory");
        let path = directory.path().join("long.wav");
        write_stereo_wav(&path, 6, 0.1, 0.1);
        let cancellation = AtomicBool::new(false);
        let result = decode_media(&path, "wav", &cancellation, None, |_| {
            cancellation.store(true, Ordering::Release);
        });
        assert!(matches!(result, Err(DecodeFailure::Cancelled)));
    }

    #[test]
    fn manager_preserves_fifo_order_and_discards_cancelled_work() {
        let directory = tempfile::tempdir().expect("temporary fixture directory");
        let first = directory.path().join("first.wav");
        let second = directory.path().join("second.wav");
        let third = directory.path().join("third.wav");
        write_stereo_wav(&first, 1, 0.2, 0.2);
        write_stereo_wav(&second, 1, 0.2, 0.2);
        write_stereo_wav(&third, 1, 0.2, 0.2);
        let (fake_runtime, gate) = FakeRuntime::blocking();
        let runtime = Arc::new(fake_runtime);
        let manager = MediaImportManager::new_for_test(runtime.clone());

        let first_job = manager
            .enqueue(
                first.to_string_lossy().into_owned(),
                import_plan(),
                ImportOrigin::Picker,
            )
            .expect("enqueue first");
        gate.wait_until_entered();
        let second_job = manager
            .enqueue(
                second.to_string_lossy().into_owned(),
                import_plan(),
                ImportOrigin::Picker,
            )
            .expect("enqueue second");
        let third_job = manager
            .enqueue(
                third.to_string_lossy().into_owned(),
                import_plan(),
                ImportOrigin::Picker,
            )
            .expect("enqueue third");
        assert_eq!(
            manager
                .cancel(third_job.id)
                .expect("cancel queued job")
                .status,
            AudioImportStatus::Cancelled
        );

        gate.release();
        assert_eq!(
            wait_for_terminal(&manager, first_job.id).status,
            AudioImportStatus::Done
        );
        assert_eq!(
            wait_for_terminal(&manager, second_job.id).status,
            AudioImportStatus::Done
        );
        assert_eq!(
            wait_for_terminal(&manager, third_job.id).result,
            Some(AudioImportResult::Cancelled)
        );
        let saved_names = runtime.saved_names.lock();
        assert_eq!(
            saved_names.as_slice(),
            ["first.wav".to_string(), "second.wav".to_string()]
        );
    }

    #[test]
    fn duplicate_channels_produce_the_same_fake_transcript() {
        let directory = tempfile::tempdir().expect("temporary fixture directory");
        let first = directory.path().join("first.wav");
        let second = directory.path().join("second.wav");
        write_stereo_wav(&first, 1, 0.35, 0.35);
        write_stereo_wav(&second, 1, 0.35, 0.35);
        let runtime = Arc::new(FakeRuntime::default());
        let manager = MediaImportManager::new_for_test(runtime.clone());
        let first_job = manager
            .enqueue(
                first.to_string_lossy().into_owned(),
                import_plan(),
                ImportOrigin::Picker,
            )
            .expect("enqueue first");
        let second_job = manager
            .enqueue(
                second.to_string_lossy().into_owned(),
                import_plan(),
                ImportOrigin::Picker,
            )
            .expect("enqueue second");
        assert_eq!(
            wait_for_terminal(&manager, first_job.id).status,
            AudioImportStatus::Done
        );
        assert_eq!(
            wait_for_terminal(&manager, second_job.id).status,
            AudioImportStatus::Done
        );
        let transcripts = runtime.transcripts.lock();
        assert_eq!(transcripts[0], transcripts[1]);
    }

    /// One second short of the threshold, opened from Finder: a dictation, the
    /// way every opened file used to be.
    #[test]
    fn an_opened_file_under_the_threshold_is_filed_as_a_dictation() {
        let directory = tempfile::tempdir().expect("temporary fixture directory");
        let path = directory.path().join("note.wav");
        write_asr_rate_wav(&path, 119);
        let runtime = Arc::new(FakeRuntime::default());
        let manager = MediaImportManager::new_for_test(runtime.clone());

        let job = manager
            .enqueue(
                path.to_string_lossy().into_owned(),
                import_plan(),
                ImportOrigin::SystemOpen,
            )
            .expect("enqueue an opened file");

        let completed = wait_for_terminal(&manager, job.id);
        assert_eq!(
            completed.result,
            Some(AudioImportResult::Done { history_id: 1 })
        );
        assert_eq!(runtime.saved_names.lock().as_slice(), ["note.wav"]);
        assert!(
            runtime.meeting_imports.lock().is_empty(),
            "a dictation-length file must not reach the meeting pipeline"
        );
        assert_eq!(
            runtime.announced.lock().as_slice(),
            [AudioImportRoutedEvent {
                file_name: "note.wav".to_string(),
                destination: AudioImportDestination::Dictation,
                link: "sona://dictation/1".to_string(),
            }]
        );
    }

    /// Exactly at the threshold, opened from Finder: a meeting. The file goes
    /// to the meeting pipeline as a path, and nothing is transcribed or filed
    /// here — this queue's ASR call and history row would both be wrong.
    #[test]
    fn an_opened_file_at_the_threshold_becomes_a_meeting() {
        let directory = tempfile::tempdir().expect("temporary fixture directory");
        let path = directory.path().join("call.wav");
        write_asr_rate_wav(&path, 120);
        let runtime = Arc::new(FakeRuntime::default());
        let manager = MediaImportManager::new_for_test(runtime.clone());

        let job = manager
            .enqueue(
                path.to_string_lossy().into_owned(),
                import_plan(),
                ImportOrigin::SystemOpen,
            )
            .expect("enqueue an opened file");

        let completed = wait_for_terminal(&manager, job.id);
        assert_eq!(completed.status, AudioImportStatus::Done);
        assert_eq!(
            completed.result,
            Some(AudioImportResult::Meeting {
                session_id: runtime.meeting_id,
            })
        );
        assert_eq!(
            runtime.meeting_imports.lock().as_slice(),
            [fs::canonicalize(&path).expect("canonical fixture path")]
        );
        assert!(
            runtime.transcripts.lock().is_empty(),
            "a meeting is transcribed by the meeting pipeline, not by this queue"
        );
        assert!(
            runtime.saved_names.lock().is_empty(),
            "a meeting must not also become a history row"
        );
        assert_eq!(
            runtime.announced.lock().as_slice(),
            [AudioImportRoutedEvent {
                file_name: "call.wav".to_string(),
                destination: AudioImportDestination::Meeting,
                link: format!("sona://meeting/{}", runtime.meeting_id.uuid()),
            }]
        );
    }

    /// The same length through the in-app picker stays a dictation and says
    /// nothing: that route asked which destination the person wanted, and this
    /// queue is the answer they gave.
    #[test]
    fn a_picker_import_of_the_same_length_stays_a_dictation() {
        let directory = tempfile::tempdir().expect("temporary fixture directory");
        let path = directory.path().join("chosen.wav");
        write_asr_rate_wav(&path, 120);
        let runtime = Arc::new(FakeRuntime::default());
        let manager = MediaImportManager::new_for_test(runtime.clone());

        let job = manager
            .enqueue(
                path.to_string_lossy().into_owned(),
                import_plan(),
                ImportOrigin::Picker,
            )
            .expect("enqueue a picked file");

        let completed = wait_for_terminal(&manager, job.id);
        assert_eq!(
            completed.result,
            Some(AudioImportResult::Done { history_id: 1 })
        );
        assert!(runtime.meeting_imports.lock().is_empty());
        assert!(
            runtime.announced.lock().is_empty(),
            "nobody needs to be told where a file they chose a destination for went"
        );
    }

    /// A refused meeting is a failure with its own code, not a quiet fall back
    /// to history: filing an hour-long recording as a dictation is the defect
    /// this route exists to fix, and a row that says so can be retried.
    #[test]
    fn a_refused_meeting_import_fails_instead_of_becoming_a_dictation() {
        let directory = tempfile::tempdir().expect("temporary fixture directory");
        let path = directory.path().join("interview.wav");
        write_asr_rate_wav(&path, 120);
        let runtime = Arc::new(FakeRuntime::refusing_meetings());
        let manager = MediaImportManager::new_for_test(runtime.clone());

        let job = manager
            .enqueue(
                path.to_string_lossy().into_owned(),
                import_plan(),
                ImportOrigin::SystemOpen,
            )
            .expect("enqueue an opened file");

        let completed = wait_for_terminal(&manager, job.id);
        assert_eq!(completed.status, AudioImportStatus::Failed);
        assert!(
            matches!(
                completed.result,
                Some(AudioImportResult::Failed {
                    code: AudioImportFailureCode::MeetingImport,
                    ..
                })
            ),
            "a refused meeting handoff must say so: {:?}",
            completed.result
        );
        assert!(runtime.saved_names.lock().is_empty());
        assert!(runtime.announced.lock().is_empty());
    }

    #[test]
    fn media_plan_never_starts_context_or_post_processing() {
        let run = RunPlan::for_media_import(&get_default_settings()).expect("media plan");
        assert!(!run.post_process_requested());
        assert!(run.context().packet().target.application_name.is_none());
    }
}

use crate::audio_toolkit::ort_session::{
    capability_label as ort_capability_label, configure_transcribe_provider,
    report_transcribe_session, OrtProvider, DEFAULT_ASR_PROVIDER,
};
use crate::audio_toolkit::{
    apply_british_spelling, apply_emoji_replacements, apply_exact_vocabulary_entries,
    apply_literal_punctuation, apply_spoken_edits, apply_text_replacements,
    apply_vocabulary_entries, detect_output_language, normalize_transcription_output,
    remove_filler_words, OutputLanguageEvidence,
};
use crate::managers::audio::AudioRecordingManager;
use crate::managers::model::{EngineType, ModelManager};
use crate::modes::AsrPlan;
#[cfg(feature = "cloud-realtime")]
use crate::modes::{CloudRunPlan, CloudSttProvider};
use crate::settings::{
    get_settings, vocabulary_initial_prompt, ModelUnloadTimeout, OrtAcceleratorSetting,
    TranscribeAcceleratorSetting,
};
use crate::snippets::apply_snippets;
use anyhow::Result;
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use tauri::{AppHandle, Emitter, Manager};
use tauri_specta::Event;
use transcribe_cpp::{
    Backend, Feature, Model, ModelOptions, RunExtension, RunOptions, Session, StreamOptions, Task,
    WhisperRunOptions,
};
use transcribe_rs::{
    onnx::{
        canary::CanaryModel,
        cohere::CohereModel,
        gigaam::GigaAMModel,
        moonshine::{MoonshineModel, MoonshineVariant, StreamingModel},
        parakeet::{ParakeetModel, ParakeetParams, TimestampGranularity},
        sense_voice::{SenseVoiceModel, SenseVoiceParams},
        Quantization,
    },
    SpeechModel, TranscribeOptions,
};
#[cfg(feature = "cloud-realtime")]
use zeroize::Zeroizing;

const STREAM_PERF_LOG_INTERVAL: Duration = Duration::from_secs(5);
const STREAM_FINALIZE_REPLY_TIMEOUT: Duration = Duration::from_secs(30);
const STREAM_PREVIEW_QUEUE_SAMPLES: usize = 2 * 16_000;
const STREAM_PREVIEW_FRAME_SAMPLES: usize = 480;
const STREAM_PREVIEW_QUEUE_CAPACITY: usize =
    STREAM_PREVIEW_QUEUE_SAMPLES / STREAM_PREVIEW_FRAME_SAMPLES;
const STREAM_CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(20);
#[cfg(feature = "cloud-realtime")]
const CLOUD_EVENT_POLL_INTERVAL: Duration = Duration::from_millis(20);
#[cfg(feature = "cloud-realtime")]
const CLOUD_FINALIZE_TIMEOUT: Duration = Duration::from_secs(8);

const ENGINE_PANIC_LOG_MESSAGE: &str = "Transcription engine panicked; the model has been unloaded";
const ENGINE_PANIC_EVENT_MESSAGE: &str = "Transcription engine failed and the model was unloaded";
const ENGINE_PANIC_ERROR_MESSAGE: &str =
    "Transcription engine failed; the model was unloaded and will reload on the next attempt";

#[derive(Clone, Debug, Serialize)]
pub struct ModelStateEvent {
    pub event_type: String,
    pub model_id: Option<String>,
    pub model_name: Option<String>,
    pub error: Option<String>,
}

fn engine_panic_model_state_event() -> ModelStateEvent {
    ModelStateEvent {
        event_type: "unloaded".to_string(),
        model_id: None,
        model_name: None,
        error: Some(ENGINE_PANIC_EVENT_MESSAGE.to_string()),
    }
}

/// Live transcription snapshot emitted to the overlay during a streaming run.
/// `committed` is the append-only, flicker-free prefix; `tentative` is the
/// volatile suffix the model may still rewrite.
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct StreamTextEvent {
    pub committed: String,
    pub tentative: String,
}

/// Phase of the streaming overlay card, emitted to drive its UI state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum StreamPhase {
    /// Receiving audio / live text (or waiting for the stream to begin). Rust
    /// does not emit this today; the frontend starts in this phase and Rust only
    /// emits transitions away from it.
    Listening,
    /// Finalizing or post-processing — show a spinner.
    Working,
}

/// Semantic kind of "working" phase, used to localize the spinner label.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum StreamWorkKind {
    Transcribing,
    Polishing,
}

/// The source currently shown by the live overlay. Cloud failures switch to
/// local_fallback before any batch decoding begins; provider partials never
/// become delivery text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum StreamEngine {
    Local,
    Cloud,
    LocalFallback,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct StreamEngineEvent {
    pub engine: StreamEngine,
}

/// Emitted to switch the streaming overlay to a working spinner.
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct StreamPhaseEvent {
    pub phase: StreamPhase,
    /// Present only when `phase` is `Working`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<StreamWorkKind>,
}

/// Commands sent to the streaming worker thread. Audio frames and the finalize
/// request travel the same channel so FIFO ordering guarantees every fed frame
/// is processed before finalize runs.
enum StreamCmd {
    Feed(Vec<f32>),
    Finalize(mpsc::Sender<Option<FinalizedStreamText>>),
    Cancel,
}

/// Lossless terminal controls use a separate lane from bounded preview audio.
enum StreamControl {
    Finalize(mpsc::Sender<Option<FinalizedStreamText>>),
    Cancel,
}

struct StreamRoute {
    audio_tx: mpsc::SyncSender<Vec<f32>>,
    control_tx: mpsc::Sender<StreamControl>,
}

struct StreamLanes {
    audio_rx: mpsc::Receiver<Vec<f32>>,
    control_rx: mpsc::Receiver<StreamControl>,
    pending_finalize: Option<mpsc::Sender<Option<FinalizedStreamText>>>,
}

impl StreamLanes {
    fn recv(&mut self) -> Result<StreamCmd, mpsc::RecvError> {
        loop {
            if let Some(reply) = self.pending_finalize.take() {
                match self.audio_rx.try_recv() {
                    Ok(pcm) => {
                        self.pending_finalize = Some(reply);
                        return Ok(StreamCmd::Feed(pcm));
                    }
                    Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => {
                        return Ok(StreamCmd::Finalize(reply));
                    }
                }
            }

            match self.control_rx.try_recv() {
                Ok(StreamControl::Cancel) => return Ok(StreamCmd::Cancel),
                Ok(StreamControl::Finalize(reply)) => {
                    self.pending_finalize = Some(reply);
                    continue;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    return self.control_rx.recv().map(|control| match control {
                        StreamControl::Finalize(reply) => StreamCmd::Finalize(reply),
                        StreamControl::Cancel => StreamCmd::Cancel,
                    });
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }

            match self.audio_rx.recv_timeout(STREAM_CONTROL_POLL_INTERVAL) {
                Ok(pcm) => return Ok(StreamCmd::Feed(pcm)),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return self.control_rx.recv().map(|control| match control {
                        StreamControl::Finalize(reply) => StreamCmd::Finalize(reply),
                        StreamControl::Cancel => StreamCmd::Cancel,
                    });
                }
            }
        }
    }

    #[cfg(feature = "cloud-realtime")]
    /// Wait for one bounded audio/control command, or return None so a cloud
    /// worker can drive provider input and its required idle keepalive.
    fn poll(&mut self) -> Option<StreamCmd> {
        loop {
            if let Some(reply) = self.pending_finalize.take() {
                return match self.audio_rx.try_recv() {
                    Ok(pcm) => {
                        self.pending_finalize = Some(reply);
                        Some(StreamCmd::Feed(pcm))
                    }
                    Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => {
                        Some(StreamCmd::Finalize(reply))
                    }
                };
            }

            match self.control_rx.try_recv() {
                Ok(StreamControl::Cancel) => return Some(StreamCmd::Cancel),
                Ok(StreamControl::Finalize(reply)) => {
                    self.pending_finalize = Some(reply);
                    continue;
                }
                Err(mpsc::TryRecvError::Disconnected) => return None,
                Err(mpsc::TryRecvError::Empty) => {}
            }

            match self.audio_rx.recv_timeout(STREAM_CONTROL_POLL_INTERVAL) {
                Ok(pcm) => return Some(StreamCmd::Feed(pcm)),
                Err(mpsc::RecvTimeoutError::Timeout) => return None,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return match self.control_rx.recv_timeout(STREAM_CONTROL_POLL_INTERVAL) {
                        Ok(StreamControl::Cancel) => Some(StreamCmd::Cancel),
                        Ok(StreamControl::Finalize(reply)) => Some(StreamCmd::Finalize(reply)),
                        Err(
                            mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected,
                        ) => None,
                    };
                }
            }
        }
    }
}

struct FinalizedStreamText {
    text: String,
    output_language: OutputLanguageEvidence,
    /// The streaming model's supported languages, for text-based detection.
    supported_languages: Vec<String>,
}

/// Routes real-time audio frames to the active streaming worker. Shared between
/// the [`TranscriptionManager`] (opens/closes the route) and the audio recorder's
/// per-frame callback (feeds frames). The recorder holds an `Arc<StreamRouter>`
/// directly, so an unarmed frame costs only atomic loads — no Tauri state lookup
/// or mutex lock.
pub struct StreamRouter {
    route: Mutex<Option<StreamRoute>>,
    /// True while a worker exists, including a degraded preview awaiting its engine return.
    open: AtomicBool,
    /// Stops copies into preview as soon as its bounded lane fills.
    accepting_audio: AtomicBool,
    /// Makes finalization return batch fallback only after the worker returns its engine.
    preview_degraded: AtomicBool,
    /// Starts an ASR worker only after VAD forwards the first speech frame.
    /// Silence must not load a model or open a cloud connection.
    first_speech_start: Mutex<Option<Box<dyn FnOnce() + Send + 'static>>>,
    first_speech_armed: AtomicBool,
}

impl StreamRouter {
    fn new() -> Self {
        Self {
            route: Mutex::new(None),
            open: AtomicBool::new(false),
            accepting_audio: AtomicBool::new(false),
            preview_degraded: AtomicBool::new(false),
            first_speech_start: Mutex::new(None),
            first_speech_armed: AtomicBool::new(false),
        }
    }

    fn open(&self) -> StreamLanes {
        let (audio_tx, audio_rx) = mpsc::sync_channel(STREAM_PREVIEW_QUEUE_CAPACITY);
        let (control_tx, control_rx) = mpsc::channel();
        *self
            .route
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(StreamRoute {
            audio_tx,
            control_tx,
        });
        self.preview_degraded.store(false, Ordering::Release);
        self.accepting_audio.store(true, Ordering::Release);
        self.open.store(true, Ordering::Release);
        StreamLanes {
            audio_rx,
            control_rx,
            pending_finalize: None,
        }
    }

    fn arm_on_first_speech(&self, start: impl FnOnce() + Send + 'static) {
        let mut pending = self
            .first_speech_start
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if pending.is_some() {
            warn!("replacing a stream start that was still waiting for speech");
        }
        *pending = Some(Box::new(start));
        self.first_speech_armed.store(true, Ordering::Release);
    }

    fn take(&self) -> Option<StreamRoute> {
        // Serializing with feed preserves the data-before-finalize ordering.
        self.accepting_audio.store(false, Ordering::Release);
        self.open.store(false, Ordering::Release);
        self.first_speech_armed.store(false, Ordering::Release);
        let route = self
            .route
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        self.first_speech_start
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        route
    }

    fn clear(&self) {
        self.accepting_audio.store(false, Ordering::Release);
        self.open.store(false, Ordering::Release);
        self.first_speech_armed.store(false, Ordering::Release);
        *self
            .route
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        self.first_speech_start
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
    }

    /// Live preview is best-effort. A full lane is terminal for preview, never for capture.
    pub fn feed(&self, frame: &[f32]) {
        if self.first_speech_armed.load(Ordering::Acquire) {
            // Keep this lock across `start`: cancellation either clears this
            // one-shot before it can run, or takes the route it creates.
            let mut pending = self
                .first_speech_start
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(start) = pending.take() {
                self.first_speech_armed.store(false, Ordering::Release);
                start();
            }
        }

        if !self.accepting_audio.load(Ordering::Acquire) {
            return;
        }
        if frame.len() > STREAM_PREVIEW_FRAME_SAMPLES {
            self.disable_preview("received an oversized frame");
            return;
        }

        let send_result = {
            let route = self
                .route
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(route) = route.as_ref() else {
                return;
            };
            route.audio_tx.try_send(frame.to_vec())
        };
        if let Err(error) = send_result {
            match error {
                mpsc::TrySendError::Full(_) => {
                    self.disable_preview("decoder backlog reached the two-second limit");
                }
                mpsc::TrySendError::Disconnected(_) => {
                    self.disable_preview("preview worker stopped receiving audio");
                }
            }
        }
    }

    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::Acquire)
    }

    fn preview_degraded(&self) -> bool {
        self.preview_degraded.load(Ordering::Acquire)
    }

    fn disable_preview(&self, reason: &str) {
        if !self.preview_degraded.swap(true, Ordering::AcqRel) {
            warn!("Live preview disabled ({reason}); final transcription will use complete batch audio");
        }
        self.accepting_audio.store(false, Ordering::Release);
    }
}

#[cfg(feature = "cloud-realtime")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloudStreamFailure {
    Authentication,
    Quota,
    Network,
    Protocol,
    Disconnected,
    Backpressure,
    MissingFinal,
    /// The native credential store could not produce this provider's key on
    /// the worker: locked, unavailable, busy, or holding an unusable entry.
    KeyUnavailable,
}

#[cfg(feature = "cloud-realtime")]
impl From<crate::cloud_stt::CloudError> for CloudStreamFailure {
    fn from(error: crate::cloud_stt::CloudError) -> Self {
        match error {
            crate::cloud_stt::CloudError::Authentication => Self::Authentication,
            crate::cloud_stt::CloudError::Quota => Self::Quota,
            crate::cloud_stt::CloudError::Network => Self::Network,
            crate::cloud_stt::CloudError::Protocol
            | crate::cloud_stt::CloudError::AudioFrameTooLarge
            | crate::cloud_stt::CloudError::Finalized => Self::Protocol,
            crate::cloud_stt::CloudError::Disconnected => Self::Disconnected,
            crate::cloud_stt::CloudError::Backpressure => Self::Backpressure,
        }
    }
}

/// Key resolution happens on the cloud worker, so its failures share the one
/// terminal-state lattice the rest of the session uses.
#[cfg(feature = "cloud-realtime")]
impl From<crate::secrets::SttSecretVerificationError> for CloudStreamFailure {
    fn from(error: crate::secrets::SttSecretVerificationError) -> Self {
        use crate::secrets::SttSecretVerificationError as KeyError;
        match error {
            KeyError::NotConfigured | KeyError::Authentication => Self::Authentication,
            KeyError::Quota => Self::Quota,
            KeyError::Network => Self::Network,
            KeyError::Unavailable
            | KeyError::Locked
            | KeyError::Busy
            | KeyError::Backend
            | KeyError::Corrupt
            | KeyError::Invalid
            | KeyError::ConsentRequired
            | KeyError::Protocol => Self::KeyUnavailable,
        }
    }
}

/// Resolves this run's provider key. Called exactly once, on the cloud worker,
/// immediately before connect; the key is used and dropped there and never
/// cached by the manager.
#[cfg(feature = "cloud-realtime")]
pub type CloudKeySource = Box<dyn FnOnce() -> Result<Zeroizing<String>, CloudStreamFailure> + Send>;

/// A cloud final is usable only after the provider closed the finalized session
/// with at least one timestamped final segment. Every other terminal state is
/// deliberately routed through frozen local fallback or held history.
#[cfg(feature = "cloud-realtime")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloudStreamFinalization {
    Final(String),
    Failed {
        failure: CloudStreamFailure,
        audio_sent: bool,
    },
}

#[cfg(feature = "cloud-realtime")]
trait CloudTransport: Send {
    fn send_audio(&mut self, samples: &[f32]) -> Result<(), CloudStreamFailure>;
    fn poll_event(
        &mut self,
        wait: Duration,
    ) -> Result<Option<crate::cloud_stt::CloudEvent>, CloudStreamFailure>;
    fn finalize(&mut self) -> Result<(), CloudStreamFailure>;
}

#[cfg(feature = "cloud-realtime")]
trait CloudTransportFactory: Send + Sync {
    fn connect(
        &self,
        plan: &CloudRunPlan,
        api_key: Zeroizing<String>,
    ) -> Result<Box<dyn CloudTransport>, CloudStreamFailure>;
}

#[cfg(feature = "cloud-realtime")]
struct DirectCloudTransport(crate::cloud_stt::CloudSession);

#[cfg(feature = "cloud-realtime")]
impl CloudTransport for DirectCloudTransport {
    fn send_audio(&mut self, samples: &[f32]) -> Result<(), CloudStreamFailure> {
        tauri::async_runtime::block_on(self.0.send_audio(samples)).map_err(Into::into)
    }

    fn poll_event(
        &mut self,
        wait: Duration,
    ) -> Result<Option<crate::cloud_stt::CloudEvent>, CloudStreamFailure> {
        match tauri::async_runtime::block_on(tokio::time::timeout(wait, self.0.next_event())) {
            Ok(result) => result.map(Some).map_err(Into::into),
            Err(_) => Ok(None),
        }
    }

    fn finalize(&mut self) -> Result<(), CloudStreamFailure> {
        tauri::async_runtime::block_on(self.0.finalize()).map_err(Into::into)
    }
}

#[cfg(feature = "cloud-realtime")]
struct DirectCloudTransportFactory;

#[cfg(feature = "cloud-realtime")]
impl CloudTransportFactory for DirectCloudTransportFactory {
    fn connect(
        &self,
        plan: &CloudRunPlan,
        api_key: Zeroizing<String>,
    ) -> Result<Box<dyn CloudTransport>, CloudStreamFailure> {
        if !plan.timestamps() {
            return Err(CloudStreamFailure::Protocol);
        }
        let provider = match plan.provider() {
            CloudSttProvider::DeepgramNova3 => crate::cloud_stt::CloudProvider::DeepgramNova3,
            CloudSttProvider::ElevenLabsScribeV2 => {
                crate::cloud_stt::CloudProvider::ElevenLabsScribeV2
            }
        };
        let config = crate::cloud_stt::CloudRunConfig::new(
            provider,
            plan.language().map(str::to_owned),
            plan.keyterms().to_vec(),
            false,
        );
        tauri::async_runtime::block_on(crate::cloud_stt::CloudSession::connect(config, api_key))
            .map(|session| Box::new(DirectCloudTransport(session)) as Box<dyn CloudTransport>)
            .map_err(Into::into)
    }
}

enum LoadedEngine {
    /// Whisper-family models (whisper, breeze-asr, custom .bin/.gguf) via
    /// transcribe-cpp. Holds the live `Session`, which keeps its `Model` alive
    /// internally, so repeated dictation reuses the session without reloading.
    TranscribeCpp(Session),
    Parakeet(ParakeetModel),
    Moonshine(MoonshineModel),
    MoonshineStreaming(StreamingModel),
    SenseVoice(SenseVoiceModel),
    GigaAM(GigaAMModel),
    Canary(CanaryModel),
    Cohere(CohereModel),
}

/// The engine's transition state: how many model loads are in flight and
/// whether an unload is running. One owner for both facts, so serialization
/// (loaders wait for loaders) and status reporting (the UI shows a spinner)
/// can never disagree.
struct EngineTransition {
    /// Nested load scopes. An outer scope may be opened by a queueing caller
    /// (see [`TranscriptionManager::try_start_loading`]) before the inner load
    /// boundary opens its own.
    load_depth: Mutex<u32>,
    load_idle: Condvar,
    unloading: AtomicBool,
}

impl EngineTransition {
    fn new() -> Self {
        Self {
            load_depth: Mutex::new(0),
            load_idle: Condvar::new(),
            unloading: AtomicBool::new(false),
        }
    }

    /// Open a load scope unconditionally. Used at the real load boundary, which
    /// may run inside a caller's scope.
    fn begin_load(self: &Arc<Self>) -> LoadingGuard {
        *lock_recover(&self.load_depth) += 1;
        LoadingGuard {
            transition: Arc::clone(self),
        }
    }

    /// Open a load scope only when no load is running, so a caller can refuse
    /// to queue a second one.
    fn try_begin_load(self: &Arc<Self>) -> Option<LoadingGuard> {
        let mut depth = lock_recover(&self.load_depth);
        if *depth > 0 {
            return None;
        }
        *depth = 1;
        drop(depth);
        Some(LoadingGuard {
            transition: Arc::clone(self),
        })
    }

    fn begin_unload(self: &Arc<Self>) -> UnloadingGuard {
        self.unloading.store(true, Ordering::Release);
        UnloadingGuard {
            transition: Arc::clone(self),
        }
    }

    fn loads_in_flight(&self) -> bool {
        *lock_recover(&self.load_depth) > 0
    }

    /// Block until no load scope is open. Callers must not hold the engine
    /// lease gate, or a queued loader could never finish.
    fn wait_for_load_idle(&self) {
        let mut depth = lock_recover(&self.load_depth);
        while *depth > 0 {
            depth = self
                .load_idle
                .wait(depth)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    /// True while the engine is being loaded or unloaded. An idle engine with
    /// no model reports false, which is what "is the model loading" means.
    fn in_progress(&self) -> bool {
        self.loads_in_flight() || self.unloading.load(Ordering::Acquire)
    }
}

/// RAII guard that closes one load scope and wakes waiters when the last scope
/// closes. Ensures the load state is always released, even on early returns or
/// panics.
pub struct LoadingGuard {
    transition: Arc<EngineTransition>,
}

impl Drop for LoadingGuard {
    fn drop(&mut self) {
        // Recover from a poisoned mutex instead of panicking, because a panic
        // inside Drop calls abort().
        let mut depth = match self.transition.load_depth.lock() {
            Ok(depth) => depth,
            Err(poisoned) => {
                warn!("Recovered poisoned load_depth mutex during LoadingGuard drop after an earlier panic this session");
                poisoned.into_inner()
            }
        };
        *depth = depth.saturating_sub(1);
        if *depth == 0 {
            self.transition.load_idle.notify_all();
        }
    }
}

/// RAII guard that clears the unloading flag on drop, so a failed or panicking
/// unload cannot leave status stuck on "transitioning".
pub struct UnloadingGuard {
    transition: Arc<EngineTransition>,
}

impl Drop for UnloadingGuard {
    fn drop(&mut self) {
        self.transition.unloading.store(false, Ordering::Release);
    }
}

type IdleWakeup = (Mutex<()>, Condvar);

fn finish_counted_activity(
    active: &AtomicU64,
    last_activity: &AtomicU64,
    idle_wakeup: &IdleWakeup,
) {
    let _wake = lock_recover(&idle_wakeup.0);
    last_activity.store(TranscriptionManager::now_ms(), Ordering::Relaxed);
    active.fetch_sub(1, Ordering::AcqRel);
    idle_wakeup.1.notify_all();
}

/// Keeps automatic model unloading suspended while a media import owns its
/// bounded decode/transcription lifecycle.
pub struct MediaImportActivityGuard {
    active_media_imports: Arc<AtomicU64>,
    last_activity: Arc<AtomicU64>,
    idle_wakeup: Arc<IdleWakeup>,
}

impl Drop for MediaImportActivityGuard {
    fn drop(&mut self) {
        finish_counted_activity(
            &self.active_media_imports,
            &self.last_activity,
            &self.idle_wakeup,
        );
    }
}

struct BatchUseGuard {
    active_batch_uses: Arc<AtomicU64>,
    last_activity: Arc<AtomicU64>,
    idle_wakeup: Arc<IdleWakeup>,
}

impl Drop for BatchUseGuard {
    fn drop(&mut self) {
        finish_counted_activity(
            &self.active_batch_uses,
            &self.last_activity,
            &self.idle_wakeup,
        );
    }
}

/// RAII guard that clears the streaming worker/lease flags on any worker exit -
/// normal return, early return, or a panic in an engine call that unwinds the
/// detached worker thread. Tokens prevent an older worker from clearing a newer
/// worker's state if a start/finalize race ever slips through.
struct StreamWorkerGuard {
    worker_id: u64,
    active_stream_worker: Arc<AtomicU64>,
    active_engine_lease: Arc<AtomicU64>,
    stream_active: Arc<AtomicBool>,
    last_activity: Arc<AtomicU64>,
    idle_wakeup: Arc<IdleWakeup>,
}

impl Drop for StreamWorkerGuard {
    fn drop(&mut self) {
        let _wake = lock_recover(&self.idle_wakeup.0);
        self.last_activity
            .store(TranscriptionManager::now_ms(), Ordering::Relaxed);
        if self.active_stream_worker.load(Ordering::Acquire) == self.worker_id {
            self.stream_active.store(false, Ordering::Release);
        }
        let _ = self.active_engine_lease.compare_exchange(
            self.worker_id,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        let _ = self.active_stream_worker.compare_exchange(
            self.worker_id,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        self.idle_wakeup.1.notify_all();
    }
}

#[derive(Clone)]
pub struct TranscriptionManager {
    engine: Arc<Mutex<Option<LoadedEngine>>>,
    /// Serializes loading, streaming, and batch use of the one native engine.
    engine_lease_gate: Arc<Mutex<()>>,
    model_manager: Arc<ModelManager>,
    app_handle: AppHandle,
    current_model_id: Arc<Mutex<Option<String>>>,
    last_activity: Arc<AtomicU64>,
    shutdown_signal: Arc<AtomicBool>,
    watcher_handle: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
    idle_wakeup: Arc<IdleWakeup>,
    /// Load and unload progress; see [`EngineTransition`].
    transition: Arc<EngineTransition>,
    reload_model_on_next_use: Arc<AtomicBool>,
    /// Routes real-time audio frames to the active streaming worker; see
    /// [`StreamRouter`]. Shared with the audio recorder so per-frame feeds skip
    /// Tauri state and the manager lock.
    router: Arc<StreamRouter>,
    /// True only while a transcribe-cpp `Stream` is actually in flight (set by
    /// the worker once `stream()` succeeds). Used for overlay/UI decisions.
    stream_active: Arc<AtomicBool>,
    /// Streaming uses four independent flags: router open = frames should route,
    /// worker active = no second worker may start, engine lease = engine is out
    /// of the mutex, stream active = UI should show a live session.
    ///
    /// Monotonic id source for stream workers; zero means "no worker".
    next_stream_worker_id: Arc<AtomicU64>,
    /// Nonzero while a stream worker exists, even if it has not leased the engine
    /// yet. This prevents a second worker from starting after finalize/cancel
    /// closes the router but before the first worker has fully exited.
    active_stream_worker: Arc<AtomicU64>,
    /// Nonzero while the streaming worker has taken the engine out of `engine`.
    /// `is_model_loaded()` consults this so the model still reports "loaded"
    /// while the worker holds it.
    active_engine_lease: Arc<AtomicU64>,
    /// Active import jobs suspend automatic model unloading while they decode
    /// and wait for the engine. A manual unload remains user-controlled.
    active_media_imports: Arc<AtomicU64>,
    /// Batch calls register before waiting for the engine lease, so the idle
    /// watcher cannot unload ahead of already-queued transcription work.
    active_batch_uses: Arc<AtomicU64>,
    /// The one injectable provider connector. Production supplies the direct
    /// BYOK WebSocket path; focused manager tests supply deterministic fakes.
    #[cfg(feature = "cloud-realtime")]
    cloud_transport_factory: Arc<dyn CloudTransportFactory>,
    #[cfg(feature = "cloud-realtime")]
    cloud_finalization: Arc<Mutex<Option<mpsc::Receiver<CloudStreamFinalization>>>>,
}

/// One batch decode's transcript and the speed the engine achieved on it.
///
/// The realtime factor is audio seconds per decode second — 13.8 means the
/// model consumed 1.05 s of audio in 76 ms — and it is the only measurement of
/// this engine's actual throughput on this machine's hardware. It is carried
/// back to the caller rather than stashed on the manager because the receipt
/// that records it belongs to one run, and a shared slot would let a meeting
/// chunk's decode be filed under a dictation.
///
/// `None` means the decode was not timed: empty audio never reached the engine,
/// or the elapsed span rounded to zero, or the sample count did not fit the
/// arithmetic. A ratio nobody could compute is absent, never infinite and never
/// a stand-in zero.
#[derive(Clone, Debug, PartialEq)]
pub struct BatchDecode {
    /// What the caller delivers: the model's output after Sona's own
    /// post-processing (vocabulary correction, filler removal, spoken edits).
    pub text: String,
    pub realtime_factor: Option<f32>,
    /// Whether the *model* produced any text for this audio, measured before
    /// post-processing ran.
    ///
    /// This is the only honest answer to "did the microphone capture speech",
    /// and it is not recoverable from `text`: filler removal (on by default)
    /// and spoken edits both empty a real transcript on purpose, so an empty
    /// `text` means either "nothing was said" or "everything said was
    /// removed", and those are different outcomes for the capture receipt.
    pub model_produced_text: bool,
}

impl BatchDecode {
    /// A transcript that no timed decode produced.
    fn untimed(text: String) -> Self {
        Self {
            model_produced_text: !text.trim().is_empty(),
            text,
            realtime_factor: None,
        }
    }
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IdleWait {
    Indefinite,
    Deadline(Duration),
    Busy(Duration),
    Unload { idle_ms: u64, limit_seconds: u64 },
}

fn idle_wait(
    model_resident: bool,
    timeout: ModelUnloadTimeout,
    last_activity_ms: u64,
    now_ms: u64,
    blocked: bool,
) -> IdleWait {
    if !model_resident || timeout == ModelUnloadTimeout::Never {
        return IdleWait::Indefinite;
    }
    let idle_ms = now_ms.saturating_sub(last_activity_ms);
    if timeout == ModelUnloadTimeout::Immediately {
        return if blocked {
            IdleWait::Indefinite
        } else {
            IdleWait::Unload {
                idle_ms,
                limit_seconds: 0,
            }
        };
    }
    let Some(limit_seconds) = timeout.to_seconds() else {
        return IdleWait::Indefinite;
    };
    let limit_ms = limit_seconds.saturating_mul(1_000);
    if idle_ms <= limit_ms {
        return IdleWait::Deadline(Duration::from_millis(
            limit_ms.saturating_sub(idle_ms).saturating_add(1),
        ));
    }
    if blocked {
        return IdleWait::Busy(Duration::from_millis(limit_ms.saturating_add(1)));
    }
    IdleWait::Unload {
        idle_ms,
        limit_seconds,
    }
}

fn automatic_unload_blocked(
    app_handle: &AppHandle,
    transition: &EngineTransition,
    active_stream_worker: &AtomicU64,
    active_media_imports: &AtomicU64,
    active_batch_uses: &AtomicU64,
    current_batch_allowance: u64,
) -> bool {
    let recording = app_handle
        .try_state::<Arc<AudioRecordingManager>>()
        .is_some_and(|manager| manager.is_recording());
    recording
        || transition.loads_in_flight()
        || active_stream_worker.load(Ordering::Acquire) != 0
        || active_media_imports.load(Ordering::Acquire) != 0
        || active_batch_uses.load(Ordering::Acquire) > current_batch_allowance
}

fn unload_engine(
    app_handle: &AppHandle,
    engine: &Mutex<Option<LoadedEngine>>,
    current_model_id: &Mutex<Option<String>>,
    transition: &Arc<EngineTransition>,
) -> Result<()> {
    let _unloading = transition.begin_unload();
    let unload_start = std::time::Instant::now();
    debug!("Starting to unload model");
    *lock_recover(engine) = None;
    *lock_recover(current_model_id) = None;
    let _ = app_handle.emit(
        "model-state-changed",
        ModelStateEvent {
            event_type: "unloaded".to_string(),
            model_id: None,
            model_name: None,
            error: None,
        },
    );
    debug!(
        "Model unloaded (took {}ms)",
        unload_start.elapsed().as_millis()
    );
    Ok(())
}

struct IdleWatcherContext {
    app_handle: AppHandle,
    engine: Arc<Mutex<Option<LoadedEngine>>>,
    engine_lease_gate: Arc<Mutex<()>>,
    current_model_id: Arc<Mutex<Option<String>>>,
    last_activity: Arc<AtomicU64>,
    shutdown_signal: Arc<AtomicBool>,
    idle_wakeup: Arc<IdleWakeup>,
    transition: Arc<EngineTransition>,
    active_stream_worker: Arc<AtomicU64>,
    active_engine_lease: Arc<AtomicU64>,
    active_media_imports: Arc<AtomicU64>,
    active_batch_uses: Arc<AtomicU64>,
}

impl IdleWatcherContext {
    fn from_manager(manager: &TranscriptionManager) -> Self {
        Self {
            app_handle: manager.app_handle.clone(),
            engine: Arc::clone(&manager.engine),
            engine_lease_gate: Arc::clone(&manager.engine_lease_gate),
            current_model_id: Arc::clone(&manager.current_model_id),
            last_activity: Arc::clone(&manager.last_activity),
            shutdown_signal: Arc::clone(&manager.shutdown_signal),
            idle_wakeup: Arc::clone(&manager.idle_wakeup),
            transition: Arc::clone(&manager.transition),
            active_stream_worker: Arc::clone(&manager.active_stream_worker),
            active_engine_lease: Arc::clone(&manager.active_engine_lease),
            active_media_imports: Arc::clone(&manager.active_media_imports),
            active_batch_uses: Arc::clone(&manager.active_batch_uses),
        }
    }

    fn model_resident(&self) -> bool {
        lock_recover(&self.engine).is_some()
            || self.active_engine_lease.load(Ordering::Acquire) != 0
    }

    fn unload_blocked(&self) -> bool {
        automatic_unload_blocked(
            &self.app_handle,
            &self.transition,
            &self.active_stream_worker,
            &self.active_media_imports,
            &self.active_batch_uses,
            0,
        )
    }

    fn unload_if_idle(&self) -> Result<bool> {
        let _engine_lease = lock_recover(&self.engine_lease_gate);
        let timeout = get_settings(&self.app_handle).model_unload_timeout;
        if !matches!(
            idle_wait(
                self.model_resident(),
                timeout,
                self.last_activity.load(Ordering::Relaxed),
                TranscriptionManager::now_ms(),
                self.unload_blocked(),
            ),
            IdleWait::Unload { .. }
        ) {
            return Ok(false);
        }
        unload_engine(
            &self.app_handle,
            &self.engine,
            &self.current_model_id,
            &self.transition,
        )?;
        Ok(true)
    }

    fn run(self) {
        debug!("Idle watcher thread started");
        let mut wake = lock_recover(&self.idle_wakeup.0);
        loop {
            if self.shutdown_signal.load(Ordering::Acquire) {
                break;
            }
            if !self.model_resident() {
                wake = self
                    .idle_wakeup
                    .1
                    .wait(wake)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                continue;
            }

            let timeout = get_settings(&self.app_handle).model_unload_timeout;
            let now_ms = TranscriptionManager::now_ms();
            let decision = idle_wait(
                true,
                timeout,
                self.last_activity.load(Ordering::Relaxed),
                now_ms,
                self.unload_blocked(),
            );
            match decision {
                IdleWait::Indefinite => {
                    wake = self
                        .idle_wakeup
                        .1
                        .wait(wake)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
                IdleWait::Deadline(wait) => {
                    wake = self
                        .idle_wakeup
                        .1
                        .wait_timeout(wake, wait)
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .0;
                }
                IdleWait::Busy(wait) => {
                    self.last_activity.store(now_ms, Ordering::Relaxed);
                    wake = self
                        .idle_wakeup
                        .1
                        .wait_timeout(wake, wait)
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .0;
                }
                IdleWait::Unload {
                    idle_ms,
                    limit_seconds,
                } => {
                    drop(wake);
                    let unload_started = std::time::Instant::now();
                    match self.unload_if_idle() {
                        Ok(true) => info!(
                            "Model unloaded after {}s idle (limit: {}s, took {}ms)",
                            idle_ms / 1_000,
                            limit_seconds,
                            unload_started.elapsed().as_millis()
                        ),
                        Ok(false) => {
                            self.last_activity
                                .store(TranscriptionManager::now_ms(), Ordering::Relaxed);
                        }
                        Err(error) => error!("Failed to unload idle model: {error}"),
                    }
                    wake = lock_recover(&self.idle_wakeup.0);
                }
            }
        }
        debug!("Idle watcher thread shutting down gracefully");
    }
}
impl TranscriptionManager {
    pub fn new(app_handle: &AppHandle, model_manager: Arc<ModelManager>) -> Result<Self> {
        let manager = Self {
            engine: Arc::new(Mutex::new(None)),
            engine_lease_gate: Arc::new(Mutex::new(())),
            model_manager,
            app_handle: app_handle.clone(),
            current_model_id: Arc::new(Mutex::new(None)),
            last_activity: Arc::new(AtomicU64::new(Self::now_ms())),
            shutdown_signal: Arc::new(AtomicBool::new(false)),
            watcher_handle: Arc::new(Mutex::new(None)),
            idle_wakeup: Arc::new((Mutex::new(()), Condvar::new())),
            transition: Arc::new(EngineTransition::new()),
            reload_model_on_next_use: Arc::new(AtomicBool::new(false)),
            router: Arc::new(StreamRouter::new()),
            stream_active: Arc::new(AtomicBool::new(false)),
            next_stream_worker_id: Arc::new(AtomicU64::new(1)),
            active_stream_worker: Arc::new(AtomicU64::new(0)),
            active_engine_lease: Arc::new(AtomicU64::new(0)),
            active_media_imports: Arc::new(AtomicU64::new(0)),
            active_batch_uses: Arc::new(AtomicU64::new(0)),
            #[cfg(feature = "cloud-realtime")]
            cloud_transport_factory: Arc::new(DirectCloudTransportFactory),
            #[cfg(feature = "cloud-realtime")]
            cloud_finalization: Arc::new(Mutex::new(None)),
        };

        let watcher = IdleWatcherContext::from_manager(&manager);
        let handle = thread::Builder::new()
            .name("sona-model-idle".to_owned())
            .spawn(move || watcher.run())?;
        *lock_recover(&manager.watcher_handle) = Some(handle);

        Ok(manager)
    }

    /// Lock the engine mutex, recovering from poison if a previous transcription panicked.
    fn lock_engine(&self) -> MutexGuard<'_, Option<LoadedEngine>> {
        self.engine.lock().unwrap_or_else(|poisoned| {
            warn!("Engine mutex was poisoned by a previous panic, recovering");
            poisoned.into_inner()
        })
    }

    pub fn is_model_loaded(&self) -> bool {
        // The engine may be leased out to the streaming worker (taken out of
        // the mutex). It's still loaded, just in use, so report true.
        self.lock_engine().is_some() || self.active_engine_lease.load(Ordering::Acquire) != 0
    }

    fn lock_engine_lease_gate(&self) -> MutexGuard<'_, ()> {
        self.engine_lease_gate.lock().unwrap_or_else(|poisoned| {
            warn!("Engine lease gate was poisoned by a previous panic, recovering");
            poisoned.into_inner()
        })
    }

    /// Wait for a loader without holding the engine gate, then recheck after
    /// acquiring it so a queued loader cannot race a batch or stream owner.
    fn wait_for_load_then_lock_engine_lease(&self) -> MutexGuard<'_, ()> {
        loop {
            self.transition.wait_for_load_idle();
            let lease = self.lock_engine_lease_gate();
            if !self.transition.loads_in_flight() {
                return lease;
            }
            drop(lease);
        }
    }

    /// Begin a bounded file-import lifecycle without granting it a separate
    /// engine. The returned guard is held by MediaImportManager's one worker.
    pub fn begin_media_import(&self) -> MediaImportActivityGuard {
        self.active_media_imports.fetch_add(1, Ordering::AcqRel);
        self.touch_activity();
        self.signal_idle_watcher();
        MediaImportActivityGuard {
            active_media_imports: Arc::clone(&self.active_media_imports),
            last_activity: Arc::clone(&self.last_activity),
            idle_wakeup: Arc::clone(&self.idle_wakeup),
        }
    }

    fn begin_batch_use(&self) -> BatchUseGuard {
        self.active_batch_uses.fetch_add(1, Ordering::AcqRel);
        self.touch_activity();
        self.signal_idle_watcher();
        BatchUseGuard {
            active_batch_uses: Arc::clone(&self.active_batch_uses),
            last_activity: Arc::clone(&self.last_activity),
            idle_wakeup: Arc::clone(&self.idle_wakeup),
        }
    }

    /// Accelerator changes should not disturb the current transcription. Mark
    /// the cached engine stale; the next model-use path reloads it with the
    /// latest settings.
    pub fn reload_model_on_next_use(&self) {
        self.reload_model_on_next_use.store(true, Ordering::Release);
    }

    /// Open a load scope only when no load is in progress. Returns a
    /// [`LoadingGuard`] whose [`Drop`] impl closes the scope and wakes waiters,
    /// or `None` if a load is already running.
    pub fn try_start_loading(&self) -> Option<LoadingGuard> {
        self.transition.try_begin_load()
    }

    /// True while the engine is loading or unloading a model. An idle engine
    /// with no model loaded is not "loading".
    pub fn is_model_loading(&self) -> bool {
        self.transition.in_progress()
    }

    pub fn unload_model(&self) -> Result<()> {
        let _engine_lease = self.lock_engine_lease_gate();
        let result = unload_engine(
            &self.app_handle,
            &self.engine,
            &self.current_model_id,
            &self.transition,
        );
        self.signal_idle_watcher();
        result
    }

    fn now_ms() -> u64 {
        let elapsed = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
    }

    /// Reset the idle timer to now.
    fn touch_activity(&self) {
        self.last_activity.store(Self::now_ms(), Ordering::Relaxed);
    }

    pub(crate) fn signal_idle_watcher(&self) {
        drop(lock_recover(&self.idle_wakeup.0));
        self.idle_wakeup.1.notify_all();
    }

    /// Unloads the model immediately if the setting is enabled and no other
    /// capture, load, stream, import, or batch is waiting for it.
    pub fn maybe_unload_immediately(&self, context: &str) {
        if automatic_unload_blocked(
            &self.app_handle,
            &self.transition,
            &self.active_stream_worker,
            &self.active_media_imports,
            &self.active_batch_uses,
            1,
        ) {
            return;
        }
        let settings = get_settings(&self.app_handle);
        if settings.model_unload_timeout == ModelUnloadTimeout::Immediately
            && self.is_model_loaded()
        {
            info!("Immediately unloading model after {}", context);
            if let Err(e) = self.unload_model() {
                warn!("Failed to immediately unload model: {}", e);
            }
        }
    }

    pub fn load_model(&self, model_id: &str) -> Result<()> {
        self.load_model_with_device(model_id, None)
    }

    /// Like [`load_model`](Self::load_model), but lets a caller hard-select the
    /// compute device for this one load by its `transcribe_cpp::devices()`
    /// registry index. This command-facing path snapshots the current settings
    /// before loading; recording paths use [`load_model_for_plan`](Self::load_model_for_plan).
    pub fn load_model_with_device(
        &self,
        model_id: &str,
        device_index: Option<usize>,
    ) -> Result<()> {
        let settings = get_settings(&self.app_handle);
        self.load_model_with_configuration(
            model_id,
            device_index,
            settings.transcribe_accelerator,
            settings.transcribe_gpu_device.as_deref(),
            settings.ort_accelerator,
        )
    }

    /// Load exactly the model and accelerator choices frozen for one run.
    pub fn load_model_for_plan(&self, plan: &AsrPlan) -> Result<()> {
        self.load_model_with_configuration(
            &plan.model_id,
            None,
            plan.transcribe_accelerator,
            plan.transcribe_gpu_device.as_deref(),
            plan.ort_accelerator,
        )
    }

    fn load_model_for_plan_while_leased(&self, plan: &AsrPlan) -> Result<()> {
        self.load_model_with_configuration_while_leased(
            &plan.model_id,
            None,
            plan.transcribe_accelerator,
            plan.transcribe_gpu_device.as_deref(),
            plan.ort_accelerator,
        )
    }

    fn load_model_with_configuration(
        &self,
        model_id: &str,
        device_index: Option<usize>,
        accelerator: TranscribeAcceleratorSetting,
        selected_gpu_device: Option<&str>,
        ort_accelerator: OrtAcceleratorSetting,
    ) -> Result<()> {
        let _engine_lease = self.lock_engine_lease_gate();
        self.load_model_with_configuration_while_leased(
            model_id,
            device_index,
            accelerator,
            selected_gpu_device,
            ort_accelerator,
        )
    }

    fn load_model_with_configuration_while_leased(
        &self,
        model_id: &str,
        device_index: Option<usize>,
        accelerator: TranscribeAcceleratorSetting,
        selected_gpu_device: Option<&str>,
        ort_accelerator: OrtAcceleratorSetting,
    ) -> Result<()> {
        // Every load path funnels through here, so this is the one place that
        // has to mark the engine as loading. Nested inside a caller's scope
        // (see `initiate_model_load`) it just adds depth.
        let _loading = self.transition.begin_load();
        let ort_provider = apply_ort_accelerator(ort_accelerator);

        let load_start = std::time::Instant::now();
        debug!("Starting to load model: {}", model_id);

        // Emit loading started event
        let _ = self.app_handle.emit(
            "model-state-changed",
            ModelStateEvent {
                event_type: "loading_started".to_string(),
                model_id: Some(model_id.to_string()),
                model_name: None,
                error: None,
            },
        );

        let model_info = self.model_manager.get_model_info(model_id).ok_or_else(|| {
            if model_id.trim().is_empty() {
                anyhow::anyhow!("No transcription model is selected")
            } else {
                anyhow::anyhow!("Model not found: {model_id}")
            }
        })?;

        if !model_info.is_downloaded {
            let error_msg = "Model not downloaded";
            let _ = self.app_handle.emit(
                "model-state-changed",
                ModelStateEvent {
                    event_type: "loading_failed".to_string(),
                    model_id: Some(model_id.to_string()),
                    model_name: Some(model_info.name.clone()),
                    error: Some(error_msg.to_string()),
                },
            );
            return Err(anyhow::anyhow!(error_msg));
        }

        let model_path = self.model_manager.get_model_path(model_id)?;

        // Drop the current engine BEFORE building the new one so transcribe-cpp
        // frees the previous native context first — avoids holding two models at
        // once (peak memory on large GGUFs). Clear the id too: if the new load
        // fails, status should read "no loaded model", not the dropped engine.
        {
            let mut engine = self.lock_engine();
            *engine = None;
        }
        {
            let mut current_model = lock_recover(&self.current_model_id);
            *current_model = None;
        }

        // Create appropriate engine based on model type
        let emit_loading_failed = |error_msg: &str| {
            let _ = self.app_handle.emit(
                "model-state-changed",
                ModelStateEvent {
                    event_type: "loading_failed".to_string(),
                    model_id: Some(model_id.to_string()),
                    model_name: Some(model_info.name.clone()),
                    error: Some(error_msg.to_string()),
                },
            );
        };
        let is_onnx = !matches!(&model_info.engine_type, EngineType::TranscribeCpp);

        let loaded_engine = match model_info.engine_type {
            EngineType::TranscribeCpp => {
                // The whisper backend is selected at model-load time from the
                // already-frozen run choices, unless this is the explicit CLI
                // device-index path.
                let (backend, device) = match device_index {
                    Some(index) => resolve_device_index(index).inspect_err(|e| {
                        emit_loading_failed(&e.to_string());
                    })?,
                    None => {
                        let device = resolve_gpu_device(accelerator, selected_gpu_device);
                        let backend = if device.is_some() {
                            Backend::Auto
                        } else {
                            select_transcribe_backend(accelerator)
                        };
                        (backend, device)
                    }
                };
                let requested_device = device
                    .as_ref()
                    .map(transcribe_device_label)
                    .unwrap_or_else(|| "automatic".to_string());
                let model_options = ModelOptions { backend, device };
                let model = Model::load_with(&model_path, &model_options).map_err(|e| {
                    let error_msg = format!("Failed to load whisper model {}: {}", model_id, e);
                    emit_loading_failed(&error_msg);
                    anyhow::anyhow!(error_msg)
                })?;
                // The bound backend may differ from the request (e.g. CPU
                // fallback under Auto); log what actually loaded.
                let bound_backend = model.backend();
                let session = model.session().map_err(|e| {
                    let error_msg = format!(
                        "Failed to create session for whisper model {}: {}",
                        model_id, e
                    );
                    emit_loading_failed(&error_msg);
                    anyhow::anyhow!(error_msg)
                })?;
                // Reconcile the registry's advertised capabilities with the
                // loaded model's real ones (GGUF metadata) so badges/gating
                // reflect runtime truth, not the pre-download probe. The
                // load-completed event below triggers the frontend refresh.
                let caps = session.model().capabilities();
                self.model_manager.set_runtime_capabilities(
                    model_id,
                    caps.supports_streaming,
                    caps.supports_translate,
                    caps.supports_language_detect,
                    caps.languages.clone(),
                );
                let bound_device = model
                    .device()
                    .map(|device| transcribe_device_label(&device))
                    .unwrap_or_else(|_| "unknown".to_string());
                info!(
                    "Loaded whisper model '{}' (requested {:?}, requested device '{}', \
                     bound backend '{}', bound device '{}', supports_streaming={}, \
                     supports_translate={}, supports_language_detect={})",
                    model_id,
                    backend,
                    requested_device,
                    bound_backend,
                    bound_device,
                    caps.supports_streaming,
                    caps.supports_translate,
                    caps.supports_language_detect
                );
                LoadedEngine::TranscribeCpp(session)
            }
            EngineType::Parakeet => {
                let engine =
                    ParakeetModel::load(&model_path, &Quantization::Int8).map_err(|e| {
                        let error_msg =
                            format!("Failed to load parakeet model {}: {}", model_id, e);
                        emit_loading_failed(&error_msg);
                        anyhow::anyhow!(error_msg)
                    })?;
                LoadedEngine::Parakeet(engine)
            }
            EngineType::Moonshine => {
                let engine = MoonshineModel::load(
                    &model_path,
                    MoonshineVariant::Base,
                    &Quantization::default(),
                )
                .map_err(|e| {
                    let error_msg = format!("Failed to load moonshine model {}: {}", model_id, e);
                    emit_loading_failed(&error_msg);
                    anyhow::anyhow!(error_msg)
                })?;
                LoadedEngine::Moonshine(engine)
            }
            EngineType::MoonshineStreaming => {
                let engine = StreamingModel::load(&model_path, 0, &Quantization::default())
                    .map_err(|e| {
                        let error_msg = format!(
                            "Failed to load moonshine streaming model {}: {}",
                            model_id, e
                        );
                        emit_loading_failed(&error_msg);
                        anyhow::anyhow!(error_msg)
                    })?;
                LoadedEngine::MoonshineStreaming(engine)
            }
            EngineType::SenseVoice => {
                let engine =
                    SenseVoiceModel::load(&model_path, &Quantization::Int8).map_err(|e| {
                        let error_msg =
                            format!("Failed to load SenseVoice model {}: {}", model_id, e);
                        emit_loading_failed(&error_msg);
                        anyhow::anyhow!(error_msg)
                    })?;
                LoadedEngine::SenseVoice(engine)
            }
            EngineType::GigaAM => {
                let engine = GigaAMModel::load(&model_path, &Quantization::Int8).map_err(|e| {
                    let error_msg = format!("Failed to load gigaam model {}: {}", model_id, e);
                    emit_loading_failed(&error_msg);
                    anyhow::anyhow!(error_msg)
                })?;
                LoadedEngine::GigaAM(engine)
            }
            EngineType::Canary => {
                let engine = CanaryModel::load(&model_path, &Quantization::Int8).map_err(|e| {
                    let error_msg = format!("Failed to load canary model {}: {}", model_id, e);
                    emit_loading_failed(&error_msg);
                    anyhow::anyhow!(error_msg)
                })?;
                LoadedEngine::Canary(engine)
            }
            EngineType::Cohere => {
                let engine = CohereModel::load(&model_path, &Quantization::Int8).map_err(|e| {
                    let error_msg = format!("Failed to load cohere model {}: {}", model_id, e);
                    emit_loading_failed(&error_msg);
                    anyhow::anyhow!(error_msg)
                })?;
                LoadedEngine::Cohere(engine)
            }
        };
        if is_onnx {
            report_transcribe_session(model_id, ort_provider);
        }

        // Update the current engine and model ID
        {
            let mut engine = self.lock_engine();
            *engine = Some(loaded_engine);
        }
        {
            let mut current_model = lock_recover(&self.current_model_id);
            *current_model = Some(model_id.to_string());
        }

        // Reset idle timer so the watcher doesn't immediately unload a just-loaded model
        self.touch_activity();
        self.signal_idle_watcher();

        // Emit loading completed event
        let _ = self.app_handle.emit(
            "model-state-changed",
            ModelStateEvent {
                event_type: "loading_completed".to_string(),
                model_id: Some(model_id.to_string()),
                model_name: Some(model_info.name.clone()),
                error: None,
            },
        );

        let load_duration = load_start.elapsed();
        debug!(
            "Successfully loaded transcription model: {} (took {}ms)",
            model_id,
            load_duration.as_millis()
        );
        Ok(())
    }

    /// Kicks off loading the exact model and runtime choices frozen for a run.
    pub fn initiate_model_load(&self, plan: &AsrPlan) {
        let Some(loading) = self.transition.try_begin_load() else {
            return;
        };

        let reload_pending = self.reload_model_on_next_use.load(Ordering::Acquire);
        let loaded_for_plan = self.get_current_model().as_deref() == Some(plan.model_id.as_str());
        if !reload_pending && loaded_for_plan && self.is_model_loaded() {
            return;
        }

        let self_clone = self.clone();
        let plan = plan.clone();
        thread::spawn(move || {
            // Hold the scope opened above until this thread finishes, so a
            // waiting stream or batch owner sees no gap between the decision to
            // load and the load itself.
            let _loading = loading;
            if reload_pending {
                self_clone
                    .reload_model_on_next_use
                    .store(false, Ordering::Release);
            }
            if let Err(e) = self_clone.load_model_for_plan(&plan) {
                error!("Failed to load frozen run model: {}", e);
            }
        });
    }

    pub fn get_current_model(&self) -> Option<String> {
        let current_model = lock_recover(&self.current_model_id);
        current_model.clone()
    }

    /// Returns the configured local ASR model only when its verified asset is
    /// already installed. Meeting capture never turns an unavailable local model
    /// into a remote request.
    pub fn meeting_selected_asr_model_id(&self) -> Option<String> {
        let plan = AsrPlan::from_settings(&get_settings(&self.app_handle));
        self.model_manager
            .get_model_info(&plan.model_id)
            .filter(|model| model.is_downloaded)
            .map(|_| plan.model_id)
    }

    /// Freezes the selected local ASR model and language into a meeting run.
    /// The caller supplies both values from the immutable meeting plan, so later
    /// settings edits cannot change an in-flight or recovered meeting.
    pub fn meeting_asr_plan_for(&self, model_id: &str, language: &str) -> Option<AsrPlan> {
        let mut plan = AsrPlan::from_settings(&get_settings(&self.app_handle));
        if !self
            .model_manager
            .get_model_info(model_id)
            .is_some_and(|model| model.is_downloaded)
        {
            return None;
        }
        plan.model_id = model_id.to_string();
        if language != "und" {
            plan.language = language.to_string();
        }
        Some(plan)
    }

    /// Whether the one native engine is already committed to work somebody is
    /// waiting on: a dictation recording, a streaming worker, a media import,
    /// or a load in flight.
    ///
    /// These are the same facts the idle watcher reads to decide it must not
    /// unload — asked with a batch allowance of zero, because any batch decode
    /// in flight is also somebody's turn. A background pass over a running
    /// meeting asks before it starts and skips its turn when the answer is
    /// yes: a provisional transcript is worth less than the words a person is
    /// waiting to see appear.
    pub fn engine_busy(&self) -> bool {
        automatic_unload_blocked(
            &self.app_handle,
            &self.transition,
            &self.active_stream_worker,
            &self.active_media_imports,
            &self.active_batch_uses,
            0,
        )
    }

    /// The compute backend the currently-loaded engine is bound to, for
    /// diagnostics (e.g. confirming `--device-index` actually bound a GPU rather
    /// than falling back to CPU/auto). transcribe-cpp (whisper-family) reports
    /// its real backend string; ONNX engines report "onnx"; `None` when no
    /// model is loaded.
    pub fn current_backend(&self) -> Option<String> {
        match self.lock_engine().as_ref() {
            Some(LoadedEngine::TranscribeCpp(session)) => {
                Some(session.model().backend().to_string())
            }
            Some(_) => Some("onnx".to_string()),
            None => None,
        }
    }

    /// Whether a live streaming run is currently in flight.
    pub fn is_streaming(&self) -> bool {
        self.stream_active.load(Ordering::Acquire)
    }

    /// Shared handle to the stream router, used by the audio recorder to feed
    /// real-time frames without going through Tauri state on every frame.
    pub fn stream_router(&self) -> Arc<StreamRouter> {
        Arc::clone(&self.router)
    }

    /// Start a local streaming worker once VAD has forwarded speech.
    ///
    /// The model itself is warmed at recording start, not here: a capture that
    /// reaches the model pays the load either way, so gating it on speech only
    /// chose a later moment. What is gated is the worker, which decodes nothing
    /// on a silent capture. It waits out any in-flight warm load on the engine's
    /// existing load scope (see `run_stream_worker`).
    pub fn arm_stream_on_first_speech(&self, asr: &AsrPlan) {
        let manager = self.clone();
        let asr = asr.clone();
        self.router.arm_on_first_speech(move || {
            manager.start_stream(&asr);
        });
    }

    /// Resolve the native key and open a remote session only after VAD has
    /// forwarded speech. Unlike a local load, a silent capture wastes this
    /// outright: it costs a credential read and a network round trip and
    /// produces nothing reusable.
    #[cfg(feature = "cloud-realtime")]
    pub fn arm_cloud_stream_on_first_speech(
        &self,
        plan: &CloudRunPlan,
        key_source: CloudKeySource,
    ) {
        let manager = self.clone();
        let plan = plan.clone();
        self.router.arm_on_first_speech(move || {
            if !manager.start_cloud_stream(&plan, key_source) {
                warn!("cloud stream could not start after speech was detected");
            }
        });
    }

    /// Begin a live streaming transcription on the held engine's session.
    /// Audio frames pushed via [`StreamRouter::feed`] (captured directly by the
    /// audio recorder) are decoded incrementally and emitted to the overlay as
    /// [`StreamTextEvent`].
    ///
    /// Non-blocking: spawns a worker that waits for any in-progress model load,
    /// verifies the model supports streaming, then begins the stream. If the
    /// model can't stream, the worker idles until finalize/cancel and reports
    /// `None` so the caller falls back to batch transcription. Frames sent
    /// before the stream begins queue on the channel and are not lost.
    pub fn start_stream(&self, asr: &AsrPlan) {
        if self.router.is_open() || self.active_stream_worker.load(Ordering::Acquire) != 0 {
            warn!("start_stream called while a stream worker is already active");
            return;
        }
        let worker_id = self.next_stream_worker_id.fetch_add(1, Ordering::Relaxed);
        if self
            .active_stream_worker
            .compare_exchange(0, worker_id, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            warn!("start_stream lost a race with another stream worker");
            return;
        }
        self.touch_activity();
        self.signal_idle_watcher();
        let lanes = self.router.open();
        self.stream_active.store(false, Ordering::Release);

        let manager = self.clone();
        let asr = asr.clone();
        thread::spawn(move || manager.run_stream_worker(lanes, worker_id, asr));
    }

    /// Open one direct cloud session on a worker after VAD forwards speech. The
    /// recorder callback only arms the existing bounded router; it never owns a
    /// socket, key, allocation-heavy conversion, or network wait.
    ///
    /// `key_source` is invoked on that worker, never on the caller's thread:
    /// resolving a native credential can block on a keychain prompt or a
    /// locked secret service, and the caller is the shortcut serialization
    /// thread that also has to service cancel presses.
    #[cfg(feature = "cloud-realtime")]
    pub fn start_cloud_stream(&self, plan: &CloudRunPlan, key_source: CloudKeySource) -> bool {
        if self.router.is_open() || self.active_stream_worker.load(Ordering::Acquire) != 0 {
            warn!("start_cloud_stream called while a stream worker is already active");
            return false;
        }
        let worker_id = self.next_stream_worker_id.fetch_add(1, Ordering::Relaxed);
        if self
            .active_stream_worker
            .compare_exchange(0, worker_id, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            warn!("start_cloud_stream lost a race with another stream worker");
            return false;
        }
        self.touch_activity();
        self.signal_idle_watcher();

        let lanes = self.router.open();
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        *self
            .cloud_finalization
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(result_rx);
        self.stream_active.store(true, Ordering::Release);
        self.emit_stream_engine(StreamEngine::Cloud);

        let manager = self.clone();
        let plan = plan.clone();
        thread::spawn(move || {
            manager.run_cloud_stream_worker(lanes, worker_id, plan, key_source, result_tx)
        });
        true
    }

    #[cfg(feature = "cloud-realtime")]
    fn run_cloud_stream_worker(
        &self,
        lanes: StreamLanes,
        worker_id: u64,
        plan: CloudRunPlan,
        key_source: CloudKeySource,
        result_tx: mpsc::SyncSender<CloudStreamFinalization>,
    ) {
        let _worker = StreamWorkerGuard {
            worker_id,
            active_stream_worker: Arc::clone(&self.active_stream_worker),
            active_engine_lease: Arc::clone(&self.active_engine_lease),
            stream_active: Arc::clone(&self.stream_active),
            last_activity: Arc::clone(&self.last_activity),
            idle_wakeup: Arc::clone(&self.idle_wakeup),
        };
        run_cloud_transport_session(
            lanes,
            self.cloud_transport_factory.as_ref(),
            &plan,
            key_source,
            self.router.as_ref(),
            |event, final_segments, interim| {
                self.consume_cloud_event(event, final_segments, interim)
            },
            result_tx,
        );
    }

    fn run_stream_worker(&self, mut lanes: StreamLanes, worker_id: u64, asr: AsrPlan) {
        let _worker = StreamWorkerGuard {
            worker_id,
            active_stream_worker: Arc::clone(&self.active_stream_worker),
            active_engine_lease: Arc::clone(&self.active_engine_lease),
            stream_active: Arc::clone(&self.stream_active),
            last_activity: Arc::clone(&self.last_activity),
            idle_wakeup: Arc::clone(&self.idle_wakeup),
        };

        // start_stream races the background load kicked off when recording
        // starts, so wait that load out before taking the engine.
        let _engine_lease = self.wait_for_load_then_lock_engine_lease();
        let model_id = asr.model_id.clone();
        if self.get_current_model().as_deref() != Some(model_id.as_str()) {
            info!(
                "Live preview: frozen model '{}' is unavailable; using batch fallback",
                model_id
            );
            self.router.clear();
            drain_until_finalize(lanes);
            return;
        }

        // Take the engine out of the mutex so we own it during streaming,
        // structurally excluding any concurrent batch transcription (which
        // transcribe-cpp's compute_lock would refuse anyway). Returned when the
        // worker exits, or dropped if the model was switched/unloaded mid-stream.
        if self
            .active_engine_lease
            .compare_exchange(0, worker_id, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            warn!("Live preview: another worker already holds the transcription engine");
            self.router.clear();
            drain_until_finalize(lanes);
            return;
        }
        let mut engine = match self.lock_engine().take() {
            Some(e) => e,
            None => {
                info!(
                    "Live preview: model '{}' was unloaded before streaming could begin; \
                 falling back to batch transcription",
                    model_id
                );
                let _ = self.active_engine_lease.compare_exchange(
                    worker_id,
                    0,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
                self.router.clear();
                drain_until_finalize(lanes);
                return;
            }
        };

        // Only transcribe-cpp models expose streaming; ONNX engines fall back to
        // batch. The loaded session (not the ModelManager copy) is the source of
        // truth for run-path capabilities.
        let (supports_streaming, supports_translate, languages) = match &engine {
            LoadedEngine::TranscribeCpp(session) => {
                let model = session.model();
                let caps = model.capabilities();
                info!(
                    "Live preview: model '{}' arch='{}' variant='{}' supports_streaming={} \
                 supports_translate={} languages={:?}",
                    model_id,
                    model.arch(),
                    model.variant(),
                    caps.supports_streaming,
                    caps.supports_translate,
                    caps.languages,
                );
                (
                    caps.supports_streaming,
                    caps.supports_translate,
                    caps.languages,
                )
            }
            _ => {
                info!(
                    "Live preview: model '{}' is not a transcribe-cpp model; \
                 streaming is unavailable, using batch transcription",
                    model_id
                );
                (false, false, Vec::new())
            }
        };

        if !supports_streaming {
            self.return_engine(engine, &model_id);
            self.router.clear();
            drain_until_finalize(lanes);
            return;
        }

        // Build options from the frozen ASR plan, never from mutable settings.
        let effective_language =
            effective_language_for_plan(&asr, self.model_manager.as_ref(), &model_id);
        let run_plan = transcribe_cpp_run_plan(
            asr.translate_to_english,
            &effective_language,
            &languages,
            supports_translate,
        );
        let output_language = resolve_output_language_evidence(
            &asr,
            run_plan.language.as_deref(),
            &languages,
            run_plan.target_language.as_deref() == Some("en"),
        );
        let run_options = RunOptions {
            task: run_plan.task,
            language: run_plan.language,
            target_language: run_plan.target_language,
            ..Default::default()
        };

        // Run the stream on the held session. The Stream borrows the session
        // (and thus the engine) for its lifetime, so the feed/finalize loop
        // lives in a labeled block — when it exits, the borrow is released and
        // the engine can be moved into return_engine().
        let mut finalize_reply: Option<mpsc::Sender<Option<FinalizedStreamText>>> = None;
        let mut finalize_result: Option<Option<FinalizedStreamText>> = None;
        let stream_started = 'stream: {
            let session = match &mut engine {
                LoadedEngine::TranscribeCpp(s) => s,
                _ => break 'stream false,
            };

            // Read the backend string before beginning the stream — the
            // `Stream` borrows `session` mutably for its lifetime, so we can't
            // call `session.model()` once it exists.
            let backend = session.model().backend();

            // StreamOptions::default() uses CommitPolicy::Auto and lets the
            // family pick its own streaming strategy (no family-specific ext).
            let mut stream = match session.stream(&run_options, &StreamOptions::default()) {
                Ok(s) => s,
                Err(e) => {
                    error!("Failed to begin stream: {}", e);
                    break 'stream false;
                }
            };

            self.stream_active.store(true, Ordering::Release);
            self.touch_activity();
            info!(
                "Live streaming transcription started (model '{}', backend '{}')",
                model_id, backend
            );

            let mut perf = StreamPerf::new();
            while let Ok(cmd) = lanes.recv() {
                match cmd {
                    StreamCmd::Feed(pcm) => {
                        if self.router.preview_degraded() {
                            continue;
                        }
                        self.touch_activity();
                        perf.record_feed(pcm.len());
                        let feed_start = Instant::now();
                        match stream.feed(&pcm) {
                            Ok(update) => {
                                perf.record_compute(feed_start.elapsed());
                                perf.record_update(
                                    update.revision,
                                    update.input_received_ms,
                                    update.audio_committed_ms,
                                    update.buffered_ms,
                                );
                                if update.committed_changed || update.tentative_changed {
                                    let text = stream.text();
                                    perf.record_emit();
                                    self.emit_stream_text(&text.committed, &text.tentative);
                                }
                                perf.maybe_log();
                            }
                            Err(e) => {
                                perf.record_compute(feed_start.elapsed());
                                warn!("stream feed failed: {}", e);
                                self.router.disable_preview("stream decoder rejected audio");
                            }
                        }
                    }
                    StreamCmd::Finalize(reply) => {
                        if self.router.preview_degraded() {
                            stream.reset();
                            perf.log_finalized(0);
                            finalize_reply = Some(reply);
                            finalize_result = Some(None);
                            break;
                        }

                        let finalize_start = Instant::now();
                        let result = match stream.finalize() {
                            Ok(update) => {
                                perf.record_compute(finalize_start.elapsed());
                                perf.record_update(
                                    update.revision,
                                    update.input_received_ms,
                                    update.audio_committed_ms,
                                    update.buffered_ms,
                                );
                                let output_language = match &output_language {
                                    OutputLanguageEvidence::Unknown => {
                                        with_model_detected_language(
                                            OutputLanguageEvidence::Unknown,
                                            stream.snapshot().language,
                                        )
                                    }
                                    resolved => resolved.clone(),
                                };
                                Some(FinalizedStreamText {
                                    text: stream.text().full,
                                    output_language,
                                    supported_languages: languages.clone(),
                                })
                            }
                            Err(e) => {
                                perf.record_compute(finalize_start.elapsed());
                                error!(
                                "stream finalize failed: {}; falling back to batch transcription",
                                e
                            );
                                None
                            }
                        };
                        let chars = match &result {
                            Some(finalized) => finalized.text.len(),
                            None => 0,
                        };
                        perf.log_finalized(chars);
                        finalize_reply = Some(reply);
                        finalize_result = Some(result);
                        break;
                    }
                    StreamCmd::Cancel => {
                        stream.reset();
                        break;
                    }
                }
            }

            true
        };
        // `stream` + the `&mut engine` borrow are released here.

        if !stream_started {
            // Stream never began (model doesn't support streaming or begin
            // failed); drain so the finalize handshake still completes and the
            // caller falls back to batch transcription. Return the engine first
            // so the fallback can immediately use it.
            self.return_engine(engine, &model_id);
            drain_until_finalize(lanes);
            return;
        }

        self.return_engine(engine, &model_id);
        if let (Some(reply), Some(result)) = (finalize_reply, finalize_result) {
            let _ = reply.send(result);
        }
        // `_worker` drops here, clearing this worker's active/lease flags after
        // the engine has been returned to the pool.
    }

    /// Consume a body-free cloud protocol event without promoting preview text.
    #[cfg(feature = "cloud-realtime")]
    fn consume_cloud_event(
        &self,
        event: crate::cloud_stt::CloudEvent,
        final_segments: &mut Vec<String>,
        interim: &mut String,
    ) -> Result<bool, CloudStreamFailure> {
        consume_cloud_event_with_preview(event, final_segments, interim, |segments, preview| {
            self.emit_cloud_preview(segments, preview)
        })
    }

    #[cfg(feature = "cloud-realtime")]
    fn emit_cloud_preview(&self, final_segments: &[String], interim: &str) {
        let mut preview = final_segments.join(" ");
        if !interim.trim().is_empty() {
            if !preview.is_empty() {
                preview.push(' ');
            }
            preview.push_str(interim);
        }
        // Provider text is preview-only. Committed remains empty until the
        // post-stop outcome selects an actual final engine result.
        self.emit_stream_text("", &preview);
    }

    /// Finalize the active cloud worker. Timeout is an external recovery
    /// boundary: the worker is discarded and the caller performs one frozen
    /// local fallback (or records Held when fallback is disabled).
    #[cfg(feature = "cloud-realtime")]
    pub fn finalize_cloud_stream(&self) -> CloudStreamFinalization {
        let Some(route) = self.router.take() else {
            return CloudStreamFinalization::Failed {
                failure: CloudStreamFailure::Disconnected,
                audio_sent: false,
            };
        };
        let receiver = self
            .cloud_finalization
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let Some(receiver) = receiver else {
            let _ = route.control_tx.send(StreamControl::Cancel);
            return CloudStreamFinalization::Failed {
                failure: CloudStreamFailure::Protocol,
                audio_sent: false,
            };
        };
        let (reply_tx, _reply_rx) = mpsc::channel();
        if route
            .control_tx
            .send(StreamControl::Finalize(reply_tx))
            .is_err()
        {
            return CloudStreamFinalization::Failed {
                failure: CloudStreamFailure::Disconnected,
                audio_sent: false,
            };
        }
        match receiver.recv_timeout(STREAM_FINALIZE_REPLY_TIMEOUT) {
            Ok(outcome) => outcome,
            Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {
                CloudStreamFinalization::Failed {
                    failure: CloudStreamFailure::MissingFinal,
                    audio_sent: true,
                }
            }
        }
    }

    /// Return the leased engine unless the model was switched or unloaded.
    fn return_engine(&self, engine: LoadedEngine, expected_model_id: &str) {
        let still_current =
            lock_recover(&self.current_model_id).as_deref() == Some(expected_model_id);
        if still_current {
            *self.lock_engine() = Some(engine);
        } else {
            info!(
                "Model changed/unloaded during transcription; dropping stale engine (was '{}')",
                expected_model_id
            );
            // `engine` drops here, freeing its resources.
        }
    }

    /// Flush the active stream and return its final, post-filtered text
    /// alongside whether the model produced anything before post-processing.
    ///
    /// `Ok(None)` means no usable stream was active and the caller may fall back
    /// to batch transcription. `Err` means finalize itself failed or timed out.
    /// A timeout may still leave the worker holding the engine, so callers
    /// should surface it instead of immediately starting a batch fallback.
    ///
    /// The [`BatchDecode`] shape is shared with the batch path so callers pick
    /// between the two without a second vocabulary of outcomes. A stream times
    /// no batch decode, so its `realtime_factor` is always `None`.
    pub fn finalize_stream(&self, asr: &AsrPlan) -> Result<Option<BatchDecode>> {
        let Some(route) = self.router.take() else {
            return Ok(None);
        };
        let (reply_tx, reply_rx) = mpsc::channel();
        if route
            .control_tx
            .send(StreamControl::Finalize(reply_tx))
            .is_err()
        {
            return Ok(None);
        }
        let finalized = match reply_rx.recv_timeout(STREAM_FINALIZE_REPLY_TIMEOUT) {
            Ok(Some(finalized)) => finalized,
            Ok(None) => return Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(None),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.stream_active.store(false, Ordering::Release);
                return Err(anyhow::anyhow!(
                    "Timed out waiting {:?} for live transcription to finalize",
                    STREAM_FINALIZE_REPLY_TIMEOUT
                ));
            }
        };

        // Streaming models do not receive a decode prompt, so custom words
        // always go through the shared fuzzy post-correction path.
        let model_produced_text = !finalized.text.trim().is_empty();
        let filtered = post_process_transcription_text(
            finalized.text,
            asr,
            false,
            &finalized.output_language,
            &finalized.supported_languages,
        );

        self.maybe_unload_immediately("streaming transcription");
        Ok(Some(BatchDecode {
            text: filtered,
            realtime_factor: None,
            model_produced_text,
        }))
    }

    /// Abandon any active stream without producing text (e.g. on cancel).
    pub fn cancel_stream(&self) {
        if let Some(route) = self.router.take() {
            let _ = route.control_tx.send(StreamControl::Cancel);
        }
        #[cfg(feature = "cloud-realtime")]
        self.cloud_finalization
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        self.stream_active.store(false, Ordering::Release);
    }

    /// Emit a working-phase event to the streaming overlay (spinner + label).
    pub fn emit_stream_working(&self, kind: StreamWorkKind) {
        let _ = StreamPhaseEvent {
            phase: StreamPhase::Working,
            kind: Some(kind),
        }
        .emit(&self.app_handle);
    }
    #[cfg(feature = "cloud-realtime")]
    pub fn emit_stream_engine(&self, engine: StreamEngine) {
        let _ = StreamEngineEvent { engine }.emit(&self.app_handle);
    }
    fn emit_stream_text(&self, committed: &str, tentative: &str) {
        let _ = StreamTextEvent {
            committed: committed.to_string(),
            tentative: tentative.to_string(),
        }
        .emit(&self.app_handle);
    }
    #[cfg(feature = "cloud-realtime")]
    pub fn clear_stream_preview(&self) {
        self.emit_stream_text("", "");
    }
    /// Batch-transcribe shared completed PCM without copying it.
    pub fn transcribe_shared(&self, asr: &AsrPlan, audio: &[f32]) -> Result<BatchDecode> {
        #[cfg(debug_assertions)]
        if std::env::var("SONA_FORCE_TRANSCRIPTION_FAILURE").is_ok() {
            return Err(anyhow::anyhow!(
                "Simulated transcription failure (SONA_FORCE_TRANSCRIPTION_FAILURE)"
            ));
        }

        let _batch_use = self.begin_batch_use();

        let st = std::time::Instant::now();
        let audio_len = audio.len();

        debug!("Audio vector length: {}", audio_len);

        if audio.is_empty() {
            debug!("Empty audio vector");
            self.maybe_unload_immediately("empty audio");
            return Ok(BatchDecode::untimed(String::new()));
        }

        // The native engine has one owner. Wait without holding the loading
        // mutex, then retain the lease across a load and the complete engine
        // call so imports and dictation make forward progress in FIFO order.
        let engine_lease = self.wait_for_load_then_lock_engine_lease();
        let active_model = asr.model_id.clone();
        let model_is_ready = self.get_current_model().as_deref() == Some(active_model.as_str())
            && self.lock_engine().is_some();
        if !model_is_ready {
            self.load_model_for_plan_while_leased(asr)?;
        }
        let validated_language =
            effective_language_for_plan(asr, self.model_manager.as_ref(), &active_model);
        if validated_language != asr.language {
            debug!(
                "Frozen language intent '{}' resolved to '{}' for model '{}'",
                asr.language, validated_language, active_model
            );
        }

        // Whether the loaded model is actually whisper-family (arch string).
        // Non-whisper archs (e.g. Voxtral Small) can advertise
        // Feature::InitialPrompt yet reject the whisper-kind run extension
        // with INVALID_ARG, so the whisper extension must be gated on the
        // arch, not on the feature (see #1601).
        let mut model_is_whisper = false;
        let mut vocabulary_prompted = false;

        // Perform transcription with the appropriate engine.
        // We use catch_unwind to prevent engine panics from poisoning the mutex,
        // which would make the app hang indefinitely on subsequent operations.
        let (result, output_language, model_languages) = {
            let mut engine_guard = self.lock_engine();

            // Take the engine out so we own it during transcription.
            // If the engine panics, we simply don't put it back (effectively unloading it)
            // instead of poisoning the mutex.
            let mut engine = match engine_guard.take() {
                Some(e) => e,
                None => {
                    return Err(anyhow::anyhow!(
                        "Model failed to load after auto-load attempt. Please check your model settings."
                    ));
                }
            };

            // Release the lock before transcribing — no mutex held during the engine call
            drop(engine_guard);

            // Probe live transcribe-cpp capabilities once (cheap GGUF-metadata
            // reads); the loaded session is the source of truth, not the
            // ModelManager copy. The whisper run extension is kind-tagged, so
            // non-whisper archs (parakeet, voxtral, …) reject it with
            // INVALID_ARG; attach it — and translate — only where supported.
            let mut model_supports_translate = false;
            let mut model_languages = self
                .model_manager
                .get_model_info(&active_model)
                .map(|info| info.supported_languages)
                .unwrap_or_default();
            let mut output_was_translated = false;
            let mut applied_language_hint: Option<String> = None;
            let mut model_detected_language: Option<String> = None;
            if let LoadedEngine::TranscribeCpp(session) = &engine {
                let model = session.model();
                let caps = model.capabilities();
                let model_takes_initial_prompt = model.supports(Feature::InitialPrompt);
                model_is_whisper = model.arch() == "whisper";
                model_supports_translate = caps.supports_translate;
                model_languages = caps.languages;
                debug!(
                    "transcribe-cpp model '{}' on '{}': initial_prompt={}, translate={}, languages={:?}",
                    active_model,
                    model.backend(),
                    model_takes_initial_prompt,
                    model_supports_translate,
                    model_languages
                );
            }

            let transcribe_result = catch_unwind(AssertUnwindSafe(|| -> Result<String> {
                match &mut engine {
                    LoadedEngine::TranscribeCpp(session) => {
                        // Only actual whisper-family sessions receive a decode
                        // prompt. Its written forms still receive exact correction;
                        // only fuzzy matching is skipped for prompted runs.
                        let initial_prompt = model_is_whisper
                            .then(|| vocabulary_initial_prompt(&asr.custom_words))
                            .flatten();
                        vocabulary_prompted = initial_prompt.is_some();
                        let family = whisper_run_extension(initial_prompt);
                        let run_plan = transcribe_cpp_run_plan(
                            asr.translate_to_english,
                            &validated_language,
                            &model_languages,
                            model_supports_translate,
                        );
                        output_was_translated = run_plan.target_language.as_deref() == Some("en");
                        applied_language_hint = run_plan.language.clone();

                        let run_options = RunOptions {
                            task: run_plan.task,
                            language: run_plan.language,
                            target_language: run_plan.target_language,
                            family,
                            ..Default::default()
                        };

                        debug!(
                            "transcribe-cpp run: task={:?}, language={:?}, initial_prompt={}",
                            run_options.task,
                            run_options.language,
                            run_options.family.is_some()
                        );

                        session
                            .run(audio, &run_options)
                            .map(|t| {
                                // Whisper's audio-based LID (auto mode only;
                                // `None` when a language hint was passed).
                                model_detected_language = t.language;
                                t.text
                            })
                            .map_err(|e| {
                                anyhow::anyhow!("transcribe-cpp transcription failed: {}", e)
                            })
                    }
                    LoadedEngine::Parakeet(parakeet_engine) => {
                        let params = ParakeetParams {
                            timestamp_granularity: Some(TimestampGranularity::Segment),
                            ..Default::default()
                        };
                        parakeet_engine
                            .transcribe_with(audio, &params)
                            .map(|r| r.text)
                            .map_err(|e| anyhow::anyhow!("Parakeet transcription failed: {}", e))
                    }
                    LoadedEngine::Moonshine(moonshine_engine) => moonshine_engine
                        .transcribe(audio, &TranscribeOptions::default())
                        .map(|r| r.text)
                        .map_err(|e| anyhow::anyhow!("Moonshine transcription failed: {}", e)),
                    LoadedEngine::MoonshineStreaming(streaming_engine) => streaming_engine
                        .transcribe(audio, &TranscribeOptions::default())
                        .map(|r| r.text)
                        .map_err(|e| {
                            anyhow::anyhow!("Moonshine streaming transcription failed: {}", e)
                        }),
                    LoadedEngine::SenseVoice(sense_voice_engine) => {
                        let language = match normalize_cjk_language(&validated_language) {
                            "zh" => Some("zh".to_string()),
                            "en" => Some("en".to_string()),
                            "ja" => Some("ja".to_string()),
                            "ko" => Some("ko".to_string()),
                            "yue" => Some("yue".to_string()),
                            _ => None,
                        };
                        applied_language_hint = language.clone();
                        let params = SenseVoiceParams {
                            language,
                            use_itn: Some(true),
                        };
                        sense_voice_engine
                            .transcribe_with(audio, &params)
                            .map(|r| r.text)
                            .map_err(|e| anyhow::anyhow!("SenseVoice transcription failed: {}", e))
                    }
                    LoadedEngine::GigaAM(gigaam_engine) => gigaam_engine
                        .transcribe(audio, &TranscribeOptions::default())
                        .map(|r| r.text)
                        .map_err(|e| anyhow::anyhow!("GigaAM transcription failed: {}", e)),
                    LoadedEngine::Canary(canary_engine) => {
                        output_was_translated = asr.translate_to_english;
                        let lang = if validated_language == "auto" {
                            None
                        } else {
                            Some(validated_language.clone())
                        };
                        applied_language_hint = lang.clone();
                        let options = TranscribeOptions {
                            language: lang,
                            translate: asr.translate_to_english,
                            ..Default::default()
                        };
                        canary_engine
                            .transcribe(audio, &options)
                            .map(|r| r.text)
                            .map_err(|e| anyhow::anyhow!("Canary transcription failed: {}", e))
                    }
                    LoadedEngine::Cohere(cohere_engine) => {
                        let lang = if validated_language == "auto" {
                            None
                        } else {
                            Some(normalize_cjk_language(&validated_language).to_string())
                        };
                        applied_language_hint = lang.clone();
                        let options = TranscribeOptions {
                            language: lang,
                            ..Default::default()
                        };
                        cohere_engine
                            .transcribe(audio, &options)
                            .map(|r| r.text)
                            .map_err(|e| anyhow::anyhow!("Cohere transcription failed: {}", e))
                    }
                }
            }));

            let text = match transcribe_result {
                Ok(inner_result) => {
                    // Success or normal error: return the engine unless a model
                    // switch/unload invalidated it while it was in use.
                    self.return_engine(engine, &active_model);
                    inner_result?
                }
                Err(_) => {
                    // Engine panicked — do NOT put it back (it's in an unknown state).
                    // The engine is dropped here, effectively unloading it.
                    error!("{ENGINE_PANIC_LOG_MESSAGE}");

                    // Clear the model ID so it will be reloaded on next attempt.
                    {
                        let mut current_model = self
                            .current_model_id
                            .lock()
                            .unwrap_or_else(|error| error.into_inner());
                        *current_model = None;
                    }

                    let _ = self
                        .app_handle
                        .emit("model-state-changed", engine_panic_model_state_event());

                    return Err(anyhow::anyhow!(ENGINE_PANIC_ERROR_MESSAGE));
                }
            };

            let output_language = with_model_detected_language(
                resolve_output_language_evidence(
                    asr,
                    applied_language_hint.as_deref(),
                    &model_languages,
                    output_was_translated,
                ),
                model_detected_language,
            );
            debug!("Output language evidence: {:?}", output_language);

            (text, output_language, model_languages)
        };

        drop(engine_lease);
        // Prompted Whisper runs retain exact spoken-form correction, while
        // non-Whisper runs use fuzzy correction because they receive no decode
        // prompt. The post-processor owns that distinction for every engine.
        //
        // Whether the model produced anything is read here, before the
        // post-processor runs, because filler removal and spoken edits can
        // empty a real transcript and the capture receipt must not read that as
        // a silent microphone.
        let model_produced_text = !result.trim().is_empty();
        let filtered_result = post_process_transcription_text(
            result,
            asr,
            vocabulary_prompted,
            &output_language,
            &model_languages,
        );

        let et = std::time::Instant::now();
        let translation_note = if asr.translate_to_english {
            " (translated)"
        } else {
            ""
        };
        // Real-time factor. Input PCM is 16 kHz mono, so audio length in seconds
        // is samples / 16000. `speedup` is audio_secs / elapsed_secs — e.g. 4.00x
        // means transcribed 4x faster than real time
        let elapsed_secs = (et - st).as_secs_f64();
        let audio_secs = u64::try_from(audio_len)
            .map(samples_to_seconds)
            .unwrap_or(f64::INFINITY);
        let speedup = real_time_factor(audio_secs, elapsed_secs);
        info!(
            "Transcription completed in {:.2}s for {:.2}s of audio ({:.2}x real-time){}",
            elapsed_secs, audio_secs, speedup, translation_note
        );

        let final_result = filtered_result;

        if !final_result.is_empty() {
            info!("Transcription completed");
        } else if model_produced_text {
            info!("Transcription result was emptied by post-processing");
        } else {
            info!("Transcription result is empty");
        }

        self.maybe_unload_immediately("transcription");

        Ok(BatchDecode {
            text: final_result,
            realtime_factor: finite_realtime_factor(speedup),
            model_produced_text,
        })
    }
}

#[cfg(feature = "cloud-realtime")]
fn consume_cloud_event_with_preview<F>(
    event: crate::cloud_stt::CloudEvent,
    final_segments: &mut Vec<String>,
    interim: &mut String,
    mut emit_preview: F,
) -> Result<bool, CloudStreamFailure>
where
    F: FnMut(&[String], &str),
{
    match event {
        crate::cloud_stt::CloudEvent::Interim { text, .. } => {
            *interim = text;
            emit_preview(final_segments, interim);
            Ok(false)
        }
        crate::cloud_stt::CloudEvent::Final { text, words } => {
            if !text.trim().is_empty() && !words.is_empty() {
                final_segments.push(text.trim().to_owned());
            }
            interim.clear();
            emit_preview(final_segments, interim);
            Ok(false)
        }
        crate::cloud_stt::CloudEvent::ProviderError(error) => Err(error.into()),
        crate::cloud_stt::CloudEvent::Closed => Ok(true),
    }
}
#[cfg(feature = "cloud-realtime")]
fn run_cloud_transport_session<F>(
    mut lanes: StreamLanes,
    factory: &dyn CloudTransportFactory,
    plan: &CloudRunPlan,
    key_source: CloudKeySource,
    router: &StreamRouter,
    mut consume_event: F,
    result_tx: mpsc::SyncSender<CloudStreamFinalization>,
) where
    F: FnMut(
        crate::cloud_stt::CloudEvent,
        &mut Vec<String>,
        &mut String,
    ) -> Result<bool, CloudStreamFailure>,
{
    let mut audio_sent = false;
    // The key is resolved here, not by the caller: a native store read can
    // block, and no audio has been sent yet, so a failure lands on the same
    // pre-connect path as an unreachable provider.
    let api_key = match key_source() {
        Ok(api_key) => api_key,
        Err(failure) => {
            router.disable_preview("cloud provider key was unavailable");
            drain_cloud_until_finalize(
                lanes,
                result_tx,
                CloudStreamFinalization::Failed {
                    failure,
                    audio_sent,
                },
            );
            return;
        }
    };
    let mut transport = match factory.connect(plan, api_key) {
        Ok(transport) => transport,
        Err(failure) => {
            router.disable_preview("cloud session could not connect");
            drain_cloud_until_finalize(
                lanes,
                result_tx,
                CloudStreamFinalization::Failed {
                    failure,
                    audio_sent,
                },
            );
            return;
        }
    };
    let mut final_segments = Vec::new();
    let mut interim = String::new();

    loop {
        match lanes.poll() {
            Some(StreamCmd::Feed(frame)) => {
                if router.preview_degraded() {
                    drain_cloud_until_finalize(
                        lanes,
                        result_tx,
                        CloudStreamFinalization::Failed {
                            failure: CloudStreamFailure::Backpressure,
                            audio_sent,
                        },
                    );
                    return;
                }
                if let Err(failure) = transport.send_audio(&frame) {
                    router.disable_preview("cloud audio send failed");
                    drain_cloud_until_finalize(
                        lanes,
                        result_tx,
                        CloudStreamFinalization::Failed {
                            failure,
                            audio_sent,
                        },
                    );
                    return;
                }
                audio_sent = true;
                match transport.poll_event(Duration::from_millis(1)) {
                    Ok(Some(event)) => {
                        match consume_event(event, &mut final_segments, &mut interim) {
                            Ok(false) => {}
                            Ok(true) => {
                                drain_cloud_until_finalize(
                                    lanes,
                                    result_tx,
                                    CloudStreamFinalization::Failed {
                                        failure: CloudStreamFailure::Disconnected,
                                        audio_sent,
                                    },
                                );
                                return;
                            }
                            Err(failure) => {
                                router.disable_preview("cloud provider rejected the session");
                                drain_cloud_until_finalize(
                                    lanes,
                                    result_tx,
                                    CloudStreamFinalization::Failed {
                                        failure,
                                        audio_sent,
                                    },
                                );
                                return;
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(failure) => {
                        router.disable_preview("cloud provider disconnected");
                        drain_cloud_until_finalize(
                            lanes,
                            result_tx,
                            CloudStreamFinalization::Failed {
                                failure,
                                audio_sent,
                            },
                        );
                        return;
                    }
                }
            }
            None => match transport.poll_event(CLOUD_EVENT_POLL_INTERVAL) {
                Ok(Some(event)) => match consume_event(event, &mut final_segments, &mut interim) {
                    Ok(false) => {}
                    Ok(true) => {
                        drain_cloud_until_finalize(
                            lanes,
                            result_tx,
                            CloudStreamFinalization::Failed {
                                failure: CloudStreamFailure::Disconnected,
                                audio_sent,
                            },
                        );
                        return;
                    }
                    Err(failure) => {
                        router.disable_preview("cloud provider rejected the session");
                        drain_cloud_until_finalize(
                            lanes,
                            result_tx,
                            CloudStreamFinalization::Failed {
                                failure,
                                audio_sent,
                            },
                        );
                        return;
                    }
                },
                Ok(None) => {}
                Err(failure) => {
                    router.disable_preview("cloud provider disconnected");
                    drain_cloud_until_finalize(
                        lanes,
                        result_tx,
                        CloudStreamFinalization::Failed {
                            failure,
                            audio_sent,
                        },
                    );
                    return;
                }
            },
            Some(StreamCmd::Finalize(reply)) => {
                let _ = reply.send(None);
                if router.preview_degraded() {
                    let _ = result_tx.send(CloudStreamFinalization::Failed {
                        failure: CloudStreamFailure::Backpressure,
                        audio_sent,
                    });
                    return;
                }
                if let Err(failure) = transport.finalize() {
                    let _ = result_tx.send(CloudStreamFinalization::Failed {
                        failure,
                        audio_sent,
                    });
                    return;
                }

                let deadline = Instant::now() + CLOUD_FINALIZE_TIMEOUT;
                loop {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        let _ = result_tx.send(CloudStreamFinalization::Failed {
                            failure: CloudStreamFailure::MissingFinal,
                            audio_sent,
                        });
                        return;
                    }
                    match transport.poll_event(remaining.min(CLOUD_EVENT_POLL_INTERVAL)) {
                        Ok(Some(event)) => {
                            match consume_event(event, &mut final_segments, &mut interim) {
                                Ok(false) => {}
                                Ok(true) => {
                                    let outcome = if final_segments.is_empty() {
                                        CloudStreamFinalization::Failed {
                                            failure: CloudStreamFailure::MissingFinal,
                                            audio_sent,
                                        }
                                    } else {
                                        CloudStreamFinalization::Final(final_segments.join(" "))
                                    };
                                    let _ = result_tx.send(outcome);
                                    return;
                                }
                                Err(failure) => {
                                    let _ = result_tx.send(CloudStreamFinalization::Failed {
                                        failure,
                                        audio_sent,
                                    });
                                    return;
                                }
                            }
                        }
                        Ok(None) => {}
                        Err(failure) => {
                            let _ = result_tx.send(CloudStreamFinalization::Failed {
                                failure,
                                audio_sent,
                            });
                            return;
                        }
                    }
                }
            }
            Some(StreamCmd::Cancel) => return,
        }
    }
}

struct StreamPerf {
    feed_count: u64,
    emit_count: u64,
    streamed_samples: u64,
    stream_compute_elapsed: Duration,
    last_log: Instant,
    latest_revision: i32,
    latest_input_received_ms: i64,
    latest_audio_committed_ms: i64,
    latest_buffered_ms: i64,
}

impl StreamPerf {
    fn new() -> Self {
        Self {
            feed_count: 0,
            emit_count: 0,
            streamed_samples: 0,
            stream_compute_elapsed: Duration::ZERO,
            last_log: Instant::now(),
            latest_revision: 0,
            latest_input_received_ms: 0,
            latest_audio_committed_ms: 0,
            latest_buffered_ms: 0,
        }
    }

    fn record_feed(&mut self, samples: usize) {
        self.feed_count += 1;
        self.streamed_samples += u64::try_from(samples).unwrap_or(u64::MAX);
    }

    fn record_compute(&mut self, elapsed: Duration) {
        self.stream_compute_elapsed += elapsed;
    }

    fn record_update(
        &mut self,
        revision: i32,
        input_received_ms: i64,
        audio_committed_ms: i64,
        buffered_ms: i64,
    ) {
        self.latest_revision = revision;
        self.latest_input_received_ms = input_received_ms;
        self.latest_audio_committed_ms = audio_committed_ms;
        self.latest_buffered_ms = buffered_ms;
    }

    fn record_emit(&mut self) {
        self.emit_count += 1;
    }

    fn maybe_log(&mut self) {
        if self.last_log.elapsed() < STREAM_PERF_LOG_INTERVAL {
            return;
        }

        let audio_secs = self.audio_secs();
        let compute_secs = self.compute_secs();
        debug!(
            "Live preview perf: {:.2}s streamed audio, {:.2}s model compute ({:.2}x real-time), \
             input_received={:.2}s, committed_audio={:.2}s, buffered={}ms, revision={}, \
             {} frames fed, {} updates emitted",
            audio_secs,
            compute_secs,
            real_time_factor(audio_secs, compute_secs),
            self.latest_input_received_ms as f64 / 1000.0,
            self.latest_audio_committed_ms as f64 / 1000.0,
            self.latest_buffered_ms,
            self.latest_revision,
            self.feed_count,
            self.emit_count,
        );
        self.last_log = Instant::now();
    }

    fn log_finalized(&self, chars: usize) {
        let audio_secs = self.audio_secs();
        let compute_secs = self.compute_secs();
        info!(
            "Live preview finalized in {:.2}s model compute for {:.2}s streamed audio ({:.2}x real-time): \
             input_received={:.2}s, committed_audio={:.2}s, buffered={}ms, revision={}, \
             {} frames fed, {} updates emitted, {} chars",
            compute_secs,
            audio_secs,
            real_time_factor(audio_secs, compute_secs),
            self.latest_input_received_ms as f64 / 1000.0,
            self.latest_audio_committed_ms as f64 / 1000.0,
            self.latest_buffered_ms,
            self.latest_revision,
            self.feed_count,
            self.emit_count,
            chars
        );
    }

    fn audio_secs(&self) -> f64 {
        samples_to_seconds(self.streamed_samples)
    }

    fn compute_secs(&self) -> f64 {
        self.stream_compute_elapsed.as_secs_f64()
    }
}

fn samples_to_seconds(samples: u64) -> f64 {
    Duration::from_secs(samples / 16_000).as_secs_f64()
        + Duration::from_nanos((samples % 16_000) * 62_500).as_secs_f64()
}

fn real_time_factor(audio_secs: f64, compute_secs: f64) -> f64 {
    if compute_secs > 0.0 {
        audio_secs / compute_secs
    } else {
        0.0
    }
}

/// Narrow a logged realtime factor to the one a receipt may claim.
///
/// `real_time_factor` reports 0.0 when the decode was too fast to time, and
/// infinity when the sample count overflowed the seconds arithmetic. Neither is
/// a throughput anybody measured, so both become absent instead of being
/// persisted as a number a reader would believe.
fn finite_realtime_factor(factor: f64) -> Option<f32> {
    (factor.is_finite() && factor > 0.0).then_some(factor as f32)
}

fn normalize_cjk_language(language: &str) -> &str {
    match language {
        "zh-Hans" | "zh-Hant" => "zh",
        other => other,
    }
}

fn base_language_code(language: &str) -> &str {
    language.split(&['-', '_'][..]).next().unwrap_or(language)
}

/// Resolve the persisted language intent into the language a specific model can
/// use without writing the coerced value back to settings.
fn effective_language_for_plan(
    asr: &AsrPlan,
    model_manager: &ModelManager,
    model_id: &str,
) -> String {
    match model_manager.get_model_info(model_id) {
        Some(info) => crate::managers::model::effective_language(
            &asr.language,
            &info.supported_languages,
            info.supports_language_detection,
        ),
        None => asr.language.clone(),
    }
}

/// Resolve how confidently Sona knows the language of the text produced by a
/// transcription run. The UI language is deliberately not part of this
/// decision.
fn resolve_output_language_evidence(
    asr: &AsrPlan,
    applied_language_hint: Option<&str>,
    supported_languages: &[String],
    translated_to_english: bool,
) -> OutputLanguageEvidence {
    if translated_to_english {
        return OutputLanguageEvidence::TranslatedToEnglish;
    }

    if let Some(language) = applied_language_hint.filter(|lang| !lang.is_empty() && *lang != "auto")
    {
        if asr.language != "auto"
            && base_language_code(&asr.language) == base_language_code(language)
        {
            return OutputLanguageEvidence::UserSelected(language.to_string());
        }
        return OutputLanguageEvidence::ModelConstrained(language.to_string());
    }

    if let [language] = supported_languages {
        return OutputLanguageEvidence::ModelConstrained(language.clone());
    }

    OutputLanguageEvidence::Unknown
}

/// Upgrade [`OutputLanguageEvidence::Unknown`] with the language the model
/// itself detected during the run (audio-based LID, e.g. Whisper in auto
/// mode). Stronger evidence resolved before the run is never overridden.
fn with_model_detected_language(
    evidence: OutputLanguageEvidence,
    detected: Option<String>,
) -> OutputLanguageEvidence {
    match (evidence, detected) {
        (OutputLanguageEvidence::Unknown, Some(language))
            if !language.is_empty() && language != "auto" =>
        {
            OutputLanguageEvidence::ModelDetected(language)
        }
        (evidence, _) => evidence,
    }
}

/// Build the whisper run extension for one decode: the vocabulary prompt, and
/// nothing else.
///
/// `None` — no extension at all — when there is no prompt to carry, which is
/// also the whole answer for non-whisper archs, since the caller only resolves
/// a prompt for whisper-family sessions.
///
/// The five hallucination-suppression knobs `WhisperRunOptions` also exposes
/// (`no_speech_thold`, `logprob_thold`, `compression_ratio_thold`,
/// `temperature`, `temperature_inc`) are deliberately left unset, which reads
/// like an oversight and is not:
///
/// 1. Setting them to Whisper's published values would change nothing.
///    `transcribe_whisper_run_ext_init()` (transcribe-cpp-sys 0.2.2,
///    `src/arch/whisper/public.cpp:58-75`) already initializes them to exactly
///    0.6 / -1.0 / 2.4 / 0.0 / 0.2, `materialize()` applies only `Some` fields
///    over that init, and the native run uses the same init when no extension
///    is attached (`src/arch/whisper/model.cpp:1492-1496`). Measured: 27
///    fixture decodes across whisper-tiny.en, small.en and large-v3-turbo were
///    byte-identical with the values pinned explicitly and left unset.
/// 2. Tuning them cannot suppress silence hallucination, which is what the
///    knobs get nominated for. The gate is a conjunction —
///    `no_speech_prob > no_speech_thold && avg_logprob < logprob_thold`
///    (`model.cpp:2370`) — and `logprob_thold` is simultaneously the
///    tier-acceptance dial (`model.cpp:2363`). Measured on real captures with
///    whisper-small.en: true silence scores no_speech_prob 0.92 but avg_logprob
///    only -0.83, so the second conjunct is false and the gate never fires;
///    raising `logprob_thold` far enough to fire it also drops real quiet
///    speech (-0.30) into the temperature ladder. On large-v3-turbo the first
///    conjunct is dead outright: no_speech_prob reads 0.001 for every input
///    including digital silence, at both Q4_K_M and Q8_0.
///
/// So a phantom "Thank you." on a silent capture is a real defect, but not one
/// these five fields can fix. It is caught downstream instead, where
/// `capture_verdict` weighs the transcript against what VAD forwarded.
fn whisper_run_extension(initial_prompt: Option<String>) -> Option<RunExtension> {
    initial_prompt.map(|initial_prompt| {
        RunExtension::Whisper(WhisperRunOptions {
            initial_prompt: Some(initial_prompt),
            ..Default::default()
        })
    })
}

struct TranscribeCppRunPlan {
    task: Task,
    language: Option<String>,
    target_language: Option<String>,
}

/// Build the transcribe-cpp language/task options shared by batch and live
/// streaming paths.
fn transcribe_cpp_run_plan(
    translate_to_english: bool,
    effective_language: &str,
    model_languages: &[String],
    model_supports_translate: bool,
) -> TranscribeCppRunPlan {
    let requested_language = match effective_language {
        "auto" => None,
        other => Some(normalize_cjk_language(other).to_string()),
    };
    // Only pass a language the loaded model actually advertises (per
    // capabilities().languages); otherwise auto-detect rather than failing with
    // UNSUPPORTED_LANGUAGE. Language-agnostic models report an empty list, so
    // they always stay on auto.
    let language = requested_language.filter(|lang| model_languages.iter().any(|l| l == lang));
    let (task, target_language) = cpp_translation_task(
        translate_to_english,
        model_supports_translate,
        language.as_deref(),
    );

    TranscribeCppRunPlan {
        task,
        language,
        target_language,
    }
}

fn post_process_transcription_text(
    raw: String,
    asr: &AsrPlan,
    vocabulary_already_prompted: bool,
    output_language: &OutputLanguageEvidence,
    supported_languages: &[String],
) -> String {
    fail_open_text_transform(raw, |raw| {
        // Filler removal runs first, on the recogniser's own words, because
        // every later pass *authors* text: a replacement's written form, a
        // snippet expansion, a vocabulary correction. Rescanning that output
        // for fillers deletes spans the user never spoke. Measured on stock
        // settings, with this pass at the tail: `Ha Long Bay is beautiful.`
        // became `Long Bay is beautiful.` (`ha` is a built-in English filler),
        // and a replacement whose written form is `Ha Long Bay` lost the same
        // word. It also destroyed dictated punctuation — `a um comma b` became
        // `a b`, because literal punctuation had already produced `a um, b` and
        // the filler match took the comma with the word. From here the pass
        // sees `um` and `comma` as the two separate tokens the user spoke.
        //
        // Nothing it needs is produced by the pipeline. `output_language` and
        // `supported_languages` are both the caller's, resolved by
        // `resolve_output_language_evidence` before any text pass runs, so the
        // `Unknown` text-detection fallback moves here intact — and reads
        // better evidence than it did at the tail, where snippets and emoji
        // rules had already rewritten its sample. The upgrade stays local to
        // this pass: the passes below keep the caller's evidence exactly as
        // they had it.
        let filler_language = match output_language {
            OutputLanguageEvidence::Unknown
                if asr.filler_word_removal_enabled && asr.custom_filler_words.is_none() =>
            {
                match detect_output_language(&raw, supported_languages) {
                    Some(language) => {
                        debug!("Text-based language detection resolved '{}'", language);
                        OutputLanguageEvidence::TextDetected(language)
                    }
                    None => OutputLanguageEvidence::Unknown,
                }
            }
            other => other.clone(),
        };
        let corrected = remove_filler_words(
            &raw,
            &filler_language,
            &asr.custom_filler_words,
            asr.filler_word_removal_enabled,
        );

        let corrected = apply_literal_punctuation(
            &corrected,
            output_language,
            asr.literal_punctuation,
            &asr.custom_words,
        );
        let corrected = apply_british_spelling(
            &corrected,
            output_language,
            asr.english_spelling,
            &asr.custom_words,
        );
        let corrected = if asr.replacements_enabled {
            apply_text_replacements(&corrected, &asr.replacements_rules)
        } else {
            corrected
        };
        let corrected = if asr.custom_words.is_empty() {
            corrected
        } else if vocabulary_already_prompted {
            apply_exact_vocabulary_entries(&corrected, &asr.custom_words)
        } else {
            apply_vocabulary_entries(&corrected, &asr.custom_words, asr.correction_threshold)
        };
        // After vocabulary correction, before snippets. Correction can *create*
        // the command phrase (a misheard "scratch back" becomes "scratch that"
        // through a vocabulary entry, and the literal-punctuation stage above
        // supplies the pause boundary the matcher needs), so running earlier
        // would miss those. Snippets must not see it: a deleted clause could
        // otherwise leave a half-expanded trigger, and a command phrase is not
        // text the user asked to expand.
        let corrected = apply_spoken_edits(&corrected, output_language, asr.spoken_edits_enabled);
        let corrected = if asr.snippets_enabled {
            apply_snippets(&corrected, &asr.snippets)
        } else {
            corrected
        };
        let corrected = if asr.emoji_replacements_enabled {
            apply_emoji_replacements(&corrected, &asr.emoji_replacements)
        } else {
            corrected
        };
        normalize_transcription_output(&corrected)
    })
}

/// Optional text cleanup must never discard a successful model result. The
/// transform is pure and owns its input, so recovering the untouched text is
/// safe even if a bug in custom-word or filler filtering unwinds.
fn fail_open_text_transform<F>(raw: String, transform: F) -> String
where
    F: FnOnce(String) -> String,
{
    let fallback = raw.clone();
    match catch_unwind(AssertUnwindSafe(|| transform(raw))) {
        Ok(processed) => processed,
        Err(_) => {
            error!("Optional transcription text post-processing panicked; using the raw result");
            fallback
        }
    }
}

/// Decide a transcribe-cpp run's task + translation target from settings.
///
/// "Translate to English" only fires where the model advertises translation.
/// Unlike transcribe-rs (which forces the target to English itself when its
/// `translate` flag is set), transcribe-cpp requires an explicit
/// `target_language`: a null target defaults to the *source*, so a non-English
/// source silently becomes e.g. es→es and Canary rejects the unadvertised pair.
/// An English source is skipped entirely — en→en is not a real translation, and
/// it's reachable by default since auto-detect-less models coerce intent to "en".
///
/// Returns `(task, target_language)` ready to drop into `RunOptions`.
fn cpp_translation_task(
    translate_to_english: bool,
    model_supports_translate: bool,
    source_language: Option<&str>,
) -> (Task, Option<String>) {
    let translate_to_en =
        translate_to_english && model_supports_translate && source_language != Some("en");
    if translate_to_en {
        (Task::Translate, Some("en".to_string()))
    } else {
        (Task::Transcribe, None)
    }
}

/// Drain a stream command channel, ignoring fed audio, until the caller
/// finalizes or cancels. Used when streaming can't actually run (model not
/// loaded / not streaming-capable) so the finalize handshake still completes
/// and the caller falls back to batch transcription.
fn drain_until_finalize(mut lanes: StreamLanes) {
    while let Ok(cmd) = lanes.recv() {
        match cmd {
            StreamCmd::Feed(_) => {}
            StreamCmd::Finalize(reply) => {
                let _ = reply.send(None);
                break;
            }
            StreamCmd::Cancel => break,
        }
    }
}
#[cfg(feature = "cloud-realtime")]
fn drain_cloud_until_finalize(
    mut lanes: StreamLanes,
    result_tx: mpsc::SyncSender<CloudStreamFinalization>,
    outcome: CloudStreamFinalization,
) {
    while let Ok(command) = lanes.recv() {
        match command {
            StreamCmd::Feed(_) => {}
            StreamCmd::Finalize(reply) => {
                let _ = reply.send(None);
                let _ = result_tx.send(outcome);
                break;
            }
            StreamCmd::Cancel => break,
        }
    }
}
/// Initialize the transcribe-cpp native backend once at startup: route native +
/// ggml diagnostics into the `log` facade and register compute backend modules.
/// In a static build (macOS Metal) `init_backends_default` is a harmless no-op;
/// in a `dynamic-backends` build it loads the per-ISA CPU / GPU modules. Must run
/// before the first model load.
pub fn init_transcribe_backend() {
    transcribe_cpp::init_logging();
    match transcribe_cpp::init_backends_default() {
        Ok(()) => {
            if transcribe_gpu_disabled_for_host() {
                warn!(
                    "Windows x64 build is running under emulation on an ARM64 host; \
                     disabling transcribe.cpp GPU acceleration and using CPU"
                );
            }
            let devices = transcribe_compute_devices();
            info!(
                "transcribe-cpp initialized with {} compute device(s): [{}]",
                devices.len(),
                devices
                    .iter()
                    .map(|d| format!("{} ({})", d.name, d.kind))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        Err(e) => warn!("Failed to initialize transcribe-cpp backends: {}", e),
    }
}

/// Human-readable list of the transcribe-cpp compute devices registered at
/// startup, for the `--list-devices` flag. The reported `index` is the
/// value to pass to `--device-index`. Backends must be initialized first
/// (see [`init_transcribe_backend`]).
pub fn describe_compute_devices() -> Vec<String> {
    transcribe_compute_devices()
        .into_iter()
        .map(|d| {
            let idx = d
                .index
                .map(|i| i.to_string())
                .unwrap_or_else(|| "-".to_string());
            let name = if d.description.is_empty() {
                d.name
            } else {
                d.description
            };
            let vram_mb = d.memory_total / (1024 * 1024);
            format!(
                "index={} kind={} name={} vram={}MB",
                idx, d.kind, name, vram_mb
            )
        })
        .collect()
}

/// Resolve a `--list-devices` registry index to an exact opaque device handle
/// for a transcribe-cpp model load (the `--device-index` flag). In 0.2 index 0
/// is an exact selection too; only an omitted index requests automatic device
/// selection. Errors if the index isn't a registered, loadable primary device.
fn resolve_device_index(index: usize) -> Result<(Backend, Option<transcribe_cpp::Device>)> {
    let device = transcribe_compute_devices()
        .into_iter()
        .find(|d| d.index == Some(index))
        .ok_or_else(|| {
            anyhow::anyhow!("No compute device with index {index} (see --list-devices)")
        })?;
    if matches!(
        device.device_type,
        transcribe_cpp::DeviceType::Accel | transcribe_cpp::DeviceType::Unknown
    ) {
        return Err(anyhow::anyhow!(
            "Device index {index} ({}) cannot host a model",
            device.kind
        ));
    }

    // 0.2's opaque handle makes every index, including zero, an exact
    // selection. Backend::Auto accepts any primary device and cannot conflict
    // with the selected device's vendor backend.
    Ok((Backend::Auto, Some(device)))
}

/// Map Sona's whisper accelerator setting to a transcribe-cpp [`Backend`].
///
/// `Auto` lets the library pick the best device (with CPU fallback), while
/// `Cpu` forces strict CPU. `Gpu` only remains as the companion setting for an
/// exact device; without a valid exact device it has the retired generic GPU
/// state's new Auto semantics. An emulated x64 process on Windows ARM64 forces
/// strict CPU for every setting.
fn select_transcribe_backend(setting: TranscribeAcceleratorSetting) -> Backend {
    select_transcribe_backend_for_host(setting, transcribe_gpu_disabled_for_host())
}

fn select_transcribe_backend_for_host(
    setting: TranscribeAcceleratorSetting,
    gpu_disabled: bool,
) -> Backend {
    match effective_transcribe_accelerator(setting, gpu_disabled) {
        TranscribeAcceleratorSetting::Cpu => Backend::Cpu,
        TranscribeAcceleratorSetting::Auto | TranscribeAcceleratorSetting::Gpu => Backend::Auto,
    }
}

/// Resolve the user's persisted GPU identity to a fresh opaque 0.2 device
/// handle. Registry indices and handles are process-local, so settings store a
/// key based on the backend's stable `device_id` (falling back to name for
/// backends such as Metal that do not report one).
fn resolve_gpu_device(
    setting: TranscribeAcceleratorSetting,
    gpu_device: Option<&str>,
) -> Option<transcribe_cpp::Device> {
    if transcribe_gpu_disabled_for_host() || setting != TranscribeAcceleratorSetting::Gpu {
        return None;
    }
    let gpu_device = gpu_device?;
    let resolved = transcribe_compute_devices().into_iter().find(|device| {
        is_transcribe_gpu_device(device) && transcribe_device_key(device) == gpu_device
    });
    if resolved.is_none() {
        warn!(
            "Stored transcribe GPU device '{}' is no longer available; using automatic device selection",
            gpu_device
        );
    }
    resolved
}

fn transcribe_device_key(device: &transcribe_cpp::Device) -> String {
    let (identity_kind, identity) = match device.device_id.as_deref() {
        Some(device_id) => ("id", device_id),
        None => ("name", device.name.as_str()),
    };
    serde_json::to_string(&(device.kind.as_str(), identity_kind, identity))
        .unwrap_or_else(|error| unreachable!("device identity tuple is JSON serializable: {error}"))
}

fn transcribe_device_label(device: &transcribe_cpp::Device) -> String {
    if device.description.is_empty() {
        device.name.clone()
    } else {
        device.description.clone()
    }
}

/// Resolve a stored preference to a provider this build can initialize.
///
/// The macOS CoreML path remains benchmark-only until it clears the per-model
/// gate, so Auto is deliberately the measured CPU default.
fn configured_ort_provider(setting: OrtAcceleratorSetting) -> OrtProvider {
    match setting {
        OrtAcceleratorSetting::Auto => DEFAULT_ASR_PROVIDER,
        OrtAcceleratorSetting::Cpu
        | OrtAcceleratorSetting::Cuda
        | OrtAcceleratorSetting::DirectMl
        | OrtAcceleratorSetting::Rocm => OrtProvider::Cpu,
    }
}

fn ort_accelerator_log_line(provider: OrtProvider, capabilities: &str) -> String {
    format!("ORT accelerator set to: {provider} (compiled capability: {capabilities})")
}

/// Apply a frozen ORT provider choice before the corresponding model load.
fn apply_ort_accelerator(setting: OrtAcceleratorSetting) -> OrtProvider {
    if !matches!(
        setting,
        OrtAcceleratorSetting::Auto | OrtAcceleratorSetting::Cpu
    ) {
        warn!(
            "ORT accelerator {:?} is not a compiled provider for this target; using CPU",
            setting
        );
    }
    let provider = configure_transcribe_provider(configured_ort_provider(setting));
    info!(
        "{}",
        ort_accelerator_log_line(provider, ort_capability_label())
    );
    provider
}

/// Apply the currently persisted accelerator preference outside a run.
pub fn apply_accelerator_settings(app: &tauri::AppHandle) {
    let settings = get_settings(app);
    info!(
        "transcribe.cpp accelerator preference: {:?} (applied on next model load)",
        settings.transcribe_accelerator
    );
    apply_ort_accelerator(settings.ort_accelerator);
}

#[derive(Serialize, Clone, Debug, Type)]
pub struct GpuDeviceOption {
    pub id: String,
    pub name: String,
    pub total_vram_mb: usize,
}

static GPU_DEVICES: OnceLock<Vec<GpuDeviceOption>> = OnceLock::new();

fn transcribe_gpu_disabled_for_host() -> bool {
    crate::utils::is_windows_x64_emulated_on_arm64()
}

fn effective_transcribe_accelerator(
    setting: TranscribeAcceleratorSetting,
    gpu_disabled: bool,
) -> TranscribeAcceleratorSetting {
    if gpu_disabled {
        TranscribeAcceleratorSetting::Cpu
    } else {
        setting
    }
}

fn is_transcribe_gpu_device(device: &transcribe_cpp::Device) -> bool {
    matches!(
        device.device_type,
        transcribe_cpp::DeviceType::Gpu | transcribe_cpp::DeviceType::Igpu
    )
}

fn transcribe_device_allowed(kind: &str, gpu_disabled: bool) -> bool {
    !gpu_disabled || matches!(kind, "cpu" | "accel")
}

fn transcribe_compute_devices() -> Vec<transcribe_cpp::Device> {
    let devices = transcribe_cpp::devices();
    let gpu_disabled = transcribe_gpu_disabled_for_host();
    if !gpu_disabled {
        return devices;
    }

    devices
        .into_iter()
        .filter(|device| transcribe_device_allowed(&device.kind, gpu_disabled))
        .collect()
}

fn available_transcribe_accelerators(gpu_disabled: bool) -> Vec<String> {
    if gpu_disabled {
        vec!["cpu".to_string()]
    } else {
        vec!["auto".to_string(), "cpu".to_string(), "gpu".to_string()]
    }
}

fn cached_gpu_devices() -> &'static [GpuDeviceOption] {
    // GPU compute devices transcribe-cpp registered at startup. `id` is a
    // persistent identity key, never the process-local registry index. It uses
    // the backend's device_id where available and its name otherwise (Metal).
    // `total_vram_mb` is 0 when the backend does not report capacity.
    GPU_DEVICES.get_or_init(|| {
        transcribe_compute_devices()
            .into_iter()
            .filter(is_transcribe_gpu_device)
            .map(|d| GpuDeviceOption {
                id: transcribe_device_key(&d),
                name: transcribe_device_label(&d),
                total_vram_mb: usize::try_from(d.memory_total / (1024 * 1024))
                    .unwrap_or(usize::MAX),
            })
            .collect()
    })
}

#[derive(Serialize, Clone, Debug, Type)]
pub struct AvailableAccelerators {
    pub transcribe: Vec<String>,
    pub ort: Vec<String>,
    pub gpu_devices: Vec<GpuDeviceOption>,
}

/// Return the accelerators available to this process on its current host.
pub fn get_available_accelerators() -> AvailableAccelerators {
    use transcribe_rs::accel::OrtAccelerator;

    let ort_options: Vec<String> = OrtAccelerator::available()
        .into_iter()
        .map(|a| a.to_string())
        .collect();

    let transcribe_options = available_transcribe_accelerators(transcribe_gpu_disabled_for_host());

    AvailableAccelerators {
        transcribe: transcribe_options,
        ort: ort_options,
        gpu_devices: cached_gpu_devices().to_vec(),
    }
}

impl Drop for TranscriptionManager {
    fn drop(&mut self) {
        // The watcher owns only the engine handles it needs, not a manager
        // clone. `watcher_handle` therefore counts manager clones exactly.
        if Arc::strong_count(&self.watcher_handle) > 1 {
            return;
        }

        self.shutdown_signal.store(true, Ordering::Release);
        self.signal_idle_watcher();

        // Wait for the thread to finish gracefully.
        // Use match instead of unwrap to avoid panicking if the mutex is
        // poisoned — a panic inside Drop calls abort().
        let mut guard = match self.watcher_handle.lock() {
            Ok(g) => g,
            Err(e) => {
                warn!("Recovered poisoned watcher_handle mutex during TranscriptionManager drop — a panic occurred earlier this session");
                e.into_inner()
            }
        };
        if let Some(handle) = guard.take() {
            if let Err(e) = handle.join() {
                warn!("Failed to join idle watcher thread: {:?}", e);
            } else {
                debug!("Idle watcher thread joined successfully");
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{
        AppSettings, EmojiReplacement, EnglishSpelling, ReplacementRule, VocabularyEntry,
    };
    #[cfg(feature = "cloud-realtime")]
    use std::collections::VecDeque;
    #[cfg(feature = "cloud-realtime")]
    #[cfg(feature = "cloud-realtime")]
    use zeroize::Zeroizing;

    #[test]
    fn idle_watcher_has_no_deadline_without_a_resident_model() {
        assert_eq!(
            idle_wait(false, ModelUnloadTimeout::Sec15, 0, 60_000, false,),
            IdleWait::Indefinite
        );
    }

    #[test]
    fn idle_watcher_defers_expired_deadline_while_work_is_queued() {
        assert_eq!(
            idle_wait(true, ModelUnloadTimeout::Sec15, 0, 15_001, true),
            IdleWait::Busy(Duration::from_millis(15_001))
        );
        assert_eq!(
            idle_wait(true, ModelUnloadTimeout::Sec15, 0, 15_001, false),
            IdleWait::Unload {
                idle_ms: 15_001,
                limit_seconds: 15,
            }
        );
    }

    #[test]
    fn immediate_unload_waits_for_active_work_then_runs_on_its_signal() {
        assert_eq!(
            idle_wait(true, ModelUnloadTimeout::Immediately, 10, 20, true),
            IdleWait::Indefinite
        );
        assert_eq!(
            idle_wait(true, ModelUnloadTimeout::Immediately, 10, 20, false),
            IdleWait::Unload {
                idle_ms: 10,
                limit_seconds: 0,
            }
        );
    }

    fn languages(codes: &[&str]) -> Vec<String> {
        codes.iter().map(|code| (*code).to_string()).collect()
    }

    /// The whole point of the deterministic-replacement stage is that the text
    /// it produces survives every later stage. `normalize_transcription_output`
    /// used to collapse any run of two or more whitespace characters into one
    /// space, which silently flattened a paragraph break back into a space, so
    /// these assertions run the real pipeline rather than the stage alone.
    #[test]
    fn a_replacement_paragraph_break_survives_the_whole_pipeline() {
        let settings = AppSettings {
            replacements_rules: vec![ReplacementRule {
                spoken: "new paragraph".to_string(),
                written: "\n\n".to_string(),
                enabled: true,
            }],
            filler_word_removal_enabled: false,
            ..Default::default()
        };
        let asr = AsrPlan::from_settings(&settings);
        let english = OutputLanguageEvidence::UserSelected("en-US".to_string());

        assert_eq!(
            post_process_transcription_text(
                "first thought new paragraph second thought".to_string(),
                &asr,
                false,
                &english,
                &[],
            ),
            "first thought\n\nsecond thought"
        );
    }

    #[test]
    fn a_spoken_paragraph_break_survives_the_whole_pipeline() {
        let settings = AppSettings {
            filler_word_removal_enabled: false,
            ..Default::default()
        };
        let mut asr = AsrPlan::from_settings(&settings);
        asr.literal_punctuation = true;
        let english = OutputLanguageEvidence::UserSelected("en-US".to_string());

        assert_eq!(
            post_process_transcription_text(
                "first thought new paragraph second thought new line third".to_string(),
                &asr,
                false,
                &english,
                &[],
            ),
            "first thought\n\nsecond thought\nthird"
        );
    }

    #[test]
    fn the_shipped_starter_replacements_run_in_the_pipeline() {
        let settings = AppSettings {
            filler_word_removal_enabled: false,
            ..Default::default()
        };
        let asr = AsrPlan::from_settings(&settings);
        let english = OutputLanguageEvidence::UserSelected("en-US".to_string());

        assert!(asr.replacements_enabled);
        assert_eq!(
            post_process_transcription_text(
                "write to me at sign example dot com".to_string(),
                &asr,
                false,
                &english,
                &[],
            ),
            "write to me @ example .com"
        );
    }

    #[test]
    fn replacements_run_before_vocabulary_so_a_rewritten_phrase_is_not_also_corrected() {
        let settings = AppSettings {
            replacements_rules: vec![ReplacementRule {
                spoken: "dot com".to_string(),
                written: ".com".to_string(),
                enabled: true,
            }],
            custom_words: vec![VocabularyEntry {
                spoken: "dot com".to_string(),
                written: "DotCom".to_string(),
            }],
            filler_word_removal_enabled: false,
            ..Default::default()
        };
        let asr = AsrPlan::from_settings(&settings);
        let english = OutputLanguageEvidence::UserSelected("en-US".to_string());

        assert_eq!(
            post_process_transcription_text(
                "example dot com".to_string(),
                &asr,
                false,
                &english,
                &[],
            ),
            "example .com"
        );
    }

    #[test]
    fn disabling_replacements_leaves_the_transcript_alone() {
        let settings = AppSettings {
            replacements_enabled: false,
            filler_word_removal_enabled: false,
            ..Default::default()
        };
        let asr = AsrPlan::from_settings(&settings);
        let english = OutputLanguageEvidence::UserSelected("en-US".to_string());

        assert_eq!(
            post_process_transcription_text(
                "write to me at sign example dot com".to_string(),
                &asr,
                false,
                &english,
                &[],
            ),
            "write to me at sign example dot com"
        );
    }

    #[test]
    fn engine_transition_reports_idle_when_nothing_is_loaded() {
        let transition = Arc::new(EngineTransition::new());

        // The bug this replaces: "no model loaded" was reported as "loading".
        assert!(!transition.in_progress());
    }

    #[test]
    fn engine_transition_refuses_a_second_queued_load_until_the_scope_closes() {
        let transition = Arc::new(EngineTransition::new());

        let first = transition.try_begin_load().expect("first load scope");
        assert!(transition.in_progress());
        assert!(transition.try_begin_load().is_none());

        drop(first);
        assert!(!transition.in_progress());
        assert!(transition.try_begin_load().is_some());
    }

    #[test]
    fn engine_transition_stays_in_progress_until_the_outer_load_scope_closes() {
        let transition = Arc::new(EngineTransition::new());

        let outer = transition.try_begin_load().expect("outer load scope");
        let inner = transition.begin_load();
        drop(inner);
        assert!(transition.in_progress());

        drop(outer);
        assert!(!transition.in_progress());
    }

    #[test]
    fn engine_transition_reports_unloading_and_clears_it_after_an_idle_unload() {
        let transition = Arc::new(EngineTransition::new());

        let unloading = transition.begin_unload();
        assert!(transition.in_progress());
        assert!(!transition.loads_in_flight());

        drop(unloading);
        assert!(!transition.in_progress());
    }

    #[test]
    fn engine_transition_wakes_waiters_when_the_last_load_scope_closes() {
        let transition = Arc::new(EngineTransition::new());
        let loading = transition.begin_load();
        let (started_tx, started_rx) = mpsc::channel();

        let loader = {
            let transition = Arc::clone(&transition);
            thread::spawn(move || {
                started_tx.send(()).expect("signal the waiter");
                thread::sleep(Duration::from_millis(20));
                drop(loading);
                assert!(!transition.in_progress());
            })
        };

        started_rx.recv().expect("loader started");
        transition.wait_for_load_idle();
        assert!(!transition.loads_in_flight());
        loader.join().expect("loader finished");
    }

    #[cfg(feature = "cloud-realtime")]
    #[derive(Default)]
    struct FakeCloudTransportState {
        connects: usize,
        plans: Vec<CloudRunPlan>,
        frames: Vec<Vec<f32>>,
        connect_failure: Option<CloudStreamFailure>,
        send_failure_at: Option<(usize, CloudStreamFailure)>,
        disconnect_after_frames: Option<usize>,
        disconnect_triggered: bool,
        pre_finalize_events: VecDeque<Result<crate::cloud_stt::CloudEvent, CloudStreamFailure>>,
        post_finalize_events: VecDeque<Result<crate::cloud_stt::CloudEvent, CloudStreamFailure>>,
        finalize_failure: Option<CloudStreamFailure>,
        finalized: bool,
        finalize_calls: usize,
    }

    #[cfg(feature = "cloud-realtime")]
    #[derive(Clone)]
    struct FakeCloudTransportFactory {
        state: Arc<Mutex<FakeCloudTransportState>>,
    }

    #[cfg(feature = "cloud-realtime")]
    struct FakeCloudTransport {
        state: Arc<Mutex<FakeCloudTransportState>>,
    }

    #[cfg(feature = "cloud-realtime")]
    impl CloudTransportFactory for FakeCloudTransportFactory {
        fn connect(
            &self,
            plan: &CloudRunPlan,
            _api_key: Zeroizing<String>,
        ) -> Result<Box<dyn CloudTransport>, CloudStreamFailure> {
            let mut state = self.state.lock().expect("fake cloud state");
            state.connects += 1;
            state.plans.push(plan.clone());
            if let Some(failure) = state.connect_failure {
                return Err(failure);
            }
            Ok(Box::new(FakeCloudTransport {
                state: Arc::clone(&self.state),
            }))
        }
    }

    #[cfg(feature = "cloud-realtime")]
    impl CloudTransport for FakeCloudTransport {
        fn send_audio(&mut self, samples: &[f32]) -> Result<(), CloudStreamFailure> {
            let mut state = self.state.lock().expect("fake cloud state");
            let frame_index = state.frames.len();
            if let Some((failure_at, failure)) = state.send_failure_at {
                if frame_index == failure_at {
                    return Err(failure);
                }
            }
            state.frames.push(samples.to_vec());
            Ok(())
        }

        fn poll_event(
            &mut self,
            _wait: Duration,
        ) -> Result<Option<crate::cloud_stt::CloudEvent>, CloudStreamFailure> {
            let mut state = self.state.lock().expect("fake cloud state");
            if !state.finalized
                && !state.disconnect_triggered
                && state
                    .disconnect_after_frames
                    .is_some_and(|boundary| state.frames.len() >= boundary)
            {
                state.disconnect_triggered = true;
                return Err(CloudStreamFailure::Disconnected);
            }
            let event = if state.finalized {
                state.post_finalize_events.pop_front()
            } else {
                state.pre_finalize_events.pop_front()
            };
            event.map_or(Ok(None), |event| event.map(Some))
        }

        fn finalize(&mut self) -> Result<(), CloudStreamFailure> {
            let mut state = self.state.lock().expect("fake cloud state");
            state.finalize_calls += 1;
            state.finalized = true;
            state.finalize_failure.map_or(Ok(()), Err)
        }
    }

    #[cfg(feature = "cloud-realtime")]
    fn cloud_plan() -> CloudRunPlan {
        use crate::modes::{CloudSttProvider, RequestedEngine, RunPlan, TranscriptionIntent};

        let mut settings = crate::settings::get_default_settings();
        let mode = settings.modes.first_mut().expect("default mode");
        mode.asr.requested_engine = RequestedEngine::DeepgramNova3;
        mode.asr.local_fallback_model_id = Some("frozen-fallback".to_string());
        let provider = settings
            .cloud_stt_provider_mut(CloudSttProvider::DeepgramNova3)
            .expect("default Deepgram provider");
        provider.consent_version = crate::settings::CLOUD_STT_CONSENT_VERSION;
        provider.audio_transfer_consent = true;
        provider.privacy_consent = true;
        provider.local_fallback_consent = true;

        RunPlan::for_intent(&settings, &TranscriptionIntent::ActiveMode)
            .expect("valid cloud plan")
            .cloud()
            .expect("cloud plan")
            .clone()
    }

    #[cfg(feature = "cloud-realtime")]
    fn run_fake_cloud(
        factory: Arc<FakeCloudTransportFactory>,
        frames: &[Vec<f32>],
    ) -> CloudStreamFinalization {
        run_fake_cloud_with_key(factory, frames, || {
            Ok(Zeroizing::new("test-key".to_string()))
        })
    }

    #[cfg(feature = "cloud-realtime")]
    fn run_fake_cloud_with_key(
        factory: Arc<FakeCloudTransportFactory>,
        frames: &[Vec<f32>],
        key_source: impl FnOnce() -> Result<Zeroizing<String>, CloudStreamFailure> + Send + 'static,
    ) -> CloudStreamFinalization {
        let router = Arc::new(StreamRouter::new());
        let lanes = router.open();
        let plan = cloud_plan();
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let worker_router = Arc::clone(&router);
        let worker_factory = Arc::clone(&factory);
        let worker = thread::spawn(move || {
            run_cloud_transport_session(
                lanes,
                worker_factory.as_ref(),
                &plan,
                Box::new(key_source),
                worker_router.as_ref(),
                |event, final_segments, interim| {
                    consume_cloud_event_with_preview(event, final_segments, interim, |_, _| {})
                },
                result_tx,
            );
        });

        for frame in frames {
            router.feed(frame);
        }
        let route = router.take().expect("active cloud route");
        let (reply_tx, _reply_rx) = mpsc::channel();
        route
            .control_tx
            .send(StreamControl::Finalize(reply_tx))
            .expect("finalize cloud route");
        let finalization = result_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("cloud finalization");
        worker.join().expect("cloud worker");
        finalization
    }

    #[test]
    fn first_speech_start_is_one_shot_and_cancellation_clears_it() {
        use std::sync::atomic::AtomicUsize;

        let router = StreamRouter::new();
        let starts = Arc::new(AtomicUsize::new(0));
        let first_starts = Arc::clone(&starts);
        router.arm_on_first_speech(move || {
            first_starts.fetch_add(1, Ordering::AcqRel);
        });

        assert_eq!(starts.load(Ordering::Acquire), 0);
        router.feed(&[0.25; STREAM_PREVIEW_FRAME_SAMPLES]);
        router.feed(&[0.25; STREAM_PREVIEW_FRAME_SAMPLES]);
        assert_eq!(starts.load(Ordering::Acquire), 1);

        let cancelled_starts = Arc::new(AtomicUsize::new(0));
        let cancelled_counter = Arc::clone(&cancelled_starts);
        router.arm_on_first_speech(move || {
            cancelled_counter.fetch_add(1, Ordering::AcqRel);
        });
        assert!(router.take().is_none());
        router.feed(&[0.25; STREAM_PREVIEW_FRAME_SAMPLES]);
        assert_eq!(cancelled_starts.load(Ordering::Acquire), 0);
    }

    #[test]
    fn preview_queue_overflow_preserves_finalize_order() {
        let router = StreamRouter::new();
        let mut lanes = router.open();
        let frame = vec![0.25; STREAM_PREVIEW_FRAME_SAMPLES];

        for _ in 0..STREAM_PREVIEW_QUEUE_CAPACITY {
            router.feed(&frame);
        }
        router.feed(&frame);

        assert!(router.preview_degraded());
        let route = router.take().expect("active stream route");
        let (reply_tx, _reply_rx) = mpsc::channel();
        route
            .control_tx
            .send(StreamControl::Finalize(reply_tx))
            .expect("lossless finalize control");

        for _ in 0..STREAM_PREVIEW_QUEUE_CAPACITY {
            assert!(matches!(
                lanes.recv().expect("queued frame"),
                StreamCmd::Feed(_)
            ));
        }
        assert!(matches!(
            lanes.recv().expect("finalize after queued frames"),
            StreamCmd::Finalize(_)
        ));
    }

    #[test]
    fn decoder_stall_bounds_preview_audio_memory() {
        let router = StreamRouter::new();
        let lanes = router.open();
        let frame = vec![0.25; STREAM_PREVIEW_FRAME_SAMPLES];

        for _ in 0..(STREAM_PREVIEW_QUEUE_CAPACITY * 4) {
            router.feed(&frame);
        }

        assert!(router.preview_degraded());
        let queued_samples: usize = lanes.audio_rx.try_iter().map(|pcm| pcm.len()).sum();
        assert!(queued_samples <= STREAM_PREVIEW_QUEUE_SAMPLES);
    }

    #[test]
    fn ort_auto_log_reports_the_effective_provider_and_compiled_capability() {
        let provider = configured_ort_provider(OrtAcceleratorSetting::Auto);
        assert_eq!(provider, OrtProvider::Cpu);
        assert_eq!(
            ort_accelerator_log_line(provider, "cpu, coreml"),
            "ORT accelerator set to: cpu (compiled capability: cpu, coreml)"
        );
        for unavailable in [
            OrtAcceleratorSetting::Cuda,
            OrtAcceleratorSetting::DirectMl,
            OrtAcceleratorSetting::Rocm,
        ] {
            assert_eq!(configured_ort_provider(unavailable), OrtProvider::Cpu);
        }
    }

    #[test]
    fn normal_hosts_preserve_every_transcribe_accelerator_setting() {
        for setting in [
            TranscribeAcceleratorSetting::Auto,
            TranscribeAcceleratorSetting::Cpu,
            TranscribeAcceleratorSetting::Gpu,
        ] {
            assert_eq!(effective_transcribe_accelerator(setting, false), setting);
        }
        assert_eq!(
            available_transcribe_accelerators(false),
            ["auto", "cpu", "gpu"]
        );
        assert_eq!(
            select_transcribe_backend_for_host(TranscribeAcceleratorSetting::Auto, false),
            Backend::Auto
        );
        assert_eq!(
            select_transcribe_backend_for_host(TranscribeAcceleratorSetting::Cpu, false),
            Backend::Cpu
        );
        assert_eq!(
            select_transcribe_backend_for_host(TranscribeAcceleratorSetting::Gpu, false),
            Backend::Auto
        );
        for kind in ["cpu", "accel", "metal", "cuda", "vulkan", "gpu"] {
            assert!(transcribe_device_allowed(kind, false));
        }
    }

    #[test]
    fn emulated_x64_on_arm64_forces_every_transcribe_setting_to_cpu() {
        for setting in [
            TranscribeAcceleratorSetting::Auto,
            TranscribeAcceleratorSetting::Cpu,
            TranscribeAcceleratorSetting::Gpu,
        ] {
            assert_eq!(
                effective_transcribe_accelerator(setting, true),
                TranscribeAcceleratorSetting::Cpu
            );
            assert_eq!(
                select_transcribe_backend_for_host(setting, true),
                Backend::Cpu
            );
        }
        assert_eq!(available_transcribe_accelerators(true), ["cpu"]);
        assert!(transcribe_device_allowed("cpu", true));
        assert!(transcribe_device_allowed("accel", true));
        for kind in ["metal", "cuda", "vulkan", "gpu", "unknown"] {
            assert!(!transcribe_device_allowed(kind, true));
        }
    }

    #[test]
    fn optional_text_transform_falls_back_to_raw_text_after_panic() {
        let raw = "原始轉錄。".to_string();
        let result = fail_open_text_transform(raw.clone(), |_| {
            panic!("simulated optional cleanup failure")
        });

        assert_eq!(result, raw);
    }

    #[test]
    fn transcript_canary_is_absent_from_log_event_panic_and_report_diagnostics() {
        const CANARY: &str = "TRANSCRIPT-CANARY-4EE1";
        let event = serde_json::to_string(&engine_panic_model_state_event())
            .expect("model state event serialization");
        let support_report = serde_json::to_string(&serde_json::json!({
            "diagnostic": ENGINE_PANIC_LOG_MESSAGE,
            "event": event,
            "panic": ENGINE_PANIC_ERROR_MESSAGE,
        }))
        .expect("diagnostic report serialization");

        for (sink, diagnostic) in [
            ("log", ENGINE_PANIC_LOG_MESSAGE),
            ("event", event.as_str()),
            ("stdout", ENGINE_PANIC_LOG_MESSAGE),
            ("stderr", ENGINE_PANIC_LOG_MESSAGE),
            ("panic", ENGINE_PANIC_ERROR_MESSAGE),
            ("support report", support_report.as_str()),
        ] {
            assert!(!diagnostic.contains(CANARY), "{sink}: {diagnostic}");
        }
    }

    #[test]
    fn prompted_whisper_corrects_spoken_forms_without_reapplying_honored_forms() {
        let settings = AppSettings {
            custom_words: vec![
                VocabularyEntry {
                    spoken: "north star".to_string(),
                    written: "Northstar".to_string(),
                },
                VocabularyEntry {
                    spoken: "color".to_string(),
                    written: "BrandColor".to_string(),
                },
            ],
            filler_word_removal_enabled: false,
            ..Default::default()
        };
        let asr = AsrPlan::from_settings(&settings);

        assert_eq!(
            post_process_transcription_text(
                "north starr north star Northstar color BrandColor".to_string(),
                &asr,
                true,
                &OutputLanguageEvidence::Unknown,
                &[],
            ),
            "north starr Northstar Northstar BrandColor BrandColor",
        );
        assert_eq!(
            post_process_transcription_text(
                "north starr".to_string(),
                &asr,
                false,
                &OutputLanguageEvidence::Unknown,
                &[],
            ),
            "Northstar"
        );
    }

    #[test]
    fn literal_punctuation_then_british_spelling_then_vocabulary_is_deterministic() {
        let settings = AppSettings {
            custom_words: vec![
                VocabularyEntry {
                    spoken: "north star".to_string(),
                    written: "Northstar".to_string(),
                },
                VocabularyEntry {
                    spoken: "color".to_string(),
                    written: "BrandColor".to_string(),
                },
            ],
            english_spelling: EnglishSpelling::British,
            filler_word_removal_enabled: false,
            ..Default::default()
        };
        let mut asr = AsrPlan::from_settings(&settings);
        asr.literal_punctuation = true;
        let english = OutputLanguageEvidence::UserSelected("en-US".to_string());

        assert_eq!(
            post_process_transcription_text(
                "organize comma north star".to_string(),
                &asr,
                false,
                &english,
                &[],
            ),
            "organise, Northstar"
        );
        assert_eq!(
            post_process_transcription_text("color".to_string(), &asr, false, &english, &[]),
            "BrandColor"
        );
    }

    #[test]
    fn emoji_replacement_stays_off_without_an_explicit_toggle_even_for_code_text() {
        let disabled = AppSettings {
            emoji_replacements: vec![EmojiReplacement {
                spoken: "smiley face".to_string(),
                written: "🙂".to_string(),
            }],
            emoji_replacements_enabled: false,
            filler_word_removal_enabled: false,
            ..Default::default()
        };
        let source = "const smiley face = 1".to_string();
        assert_eq!(
            post_process_transcription_text(
                source.clone(),
                &AsrPlan::from_settings(&disabled),
                false,
                &OutputLanguageEvidence::Unknown,
                &[],
            ),
            source
        );

        let mut enabled = disabled;
        enabled.emoji_replacements_enabled = true;
        assert_eq!(
            post_process_transcription_text(
                "const smiley face = 1".to_string(),
                &AsrPlan::from_settings(&enabled),
                false,
                &OutputLanguageEvidence::Unknown,
                &[],
            ),
            "const 🙂 = 1"
        );
    }

    #[test]
    fn snippets_expand_after_vocabulary_and_before_emoji_replacement() {
        let settings = AppSettings {
            custom_words: vec![VocabularyEntry {
                spoken: "north star".to_string(),
                written: "Northstar".to_string(),
            }],
            snippets: vec![crate::snippets::Snippet {
                id: "one".to_string(),
                trigger: "northstar".to_string(),
                expansion: "the Northstar plan".to_string(),
                enabled: true,
                created_at: 0,
                updated_at: 0,
            }],
            emoji_replacements: vec![EmojiReplacement {
                spoken: "plan".to_string(),
                written: "📋".to_string(),
            }],
            emoji_replacements_enabled: true,
            filler_word_removal_enabled: false,
            ..Default::default()
        };

        assert_eq!(
            post_process_transcription_text(
                "north star".to_string(),
                &AsrPlan::from_settings(&settings),
                false,
                &OutputLanguageEvidence::Unknown,
                &[],
            ),
            "the Northstar 📋"
        );

        let mut disabled = settings;
        disabled.snippets_enabled = false;
        assert_eq!(
            post_process_transcription_text(
                "north star".to_string(),
                &AsrPlan::from_settings(&disabled),
                false,
                &OutputLanguageEvidence::Unknown,
                &[],
            ),
            "Northstar"
        );
    }

    #[test]
    fn spoken_edits_run_after_vocabulary_correction_and_before_snippets() {
        // The vocabulary entry is what turns the misheard phrase into the
        // command, so the command can only fire if spoken edits run after
        // correction. The snippet trigger is the command's own word, so it can
        // only stay unexpanded if spoken edits run before expansion.
        let settings = AppSettings {
            custom_words: vec![VocabularyEntry {
                spoken: "scratch back".to_string(),
                written: "scratch that".to_string(),
            }],
            snippets: vec![crate::snippets::Snippet {
                id: "one".to_string(),
                trigger: "that".to_string(),
                expansion: "THAT".to_string(),
                enabled: true,
                created_at: 0,
                updated_at: 0,
            }],
            spoken_edits_enabled: true,
            filler_word_removal_enabled: false,
            ..Default::default()
        };
        let english = OutputLanguageEvidence::UserSelected("en-US".to_string());

        assert_eq!(
            post_process_transcription_text(
                "Keep this. Drop this. Scratch back.".to_string(),
                &AsrPlan::from_settings(&settings),
                false,
                &english,
                &[],
            ),
            "Keep this."
        );

        // The roadmap's named false positive, through the whole pipeline.
        assert_eq!(
            post_process_transcription_text(
                "We should scratch that plan.".to_string(),
                &AsrPlan::from_settings(&settings),
                false,
                &english,
                &[],
            ),
            "We should scratch THAT plan."
        );

        // Gate off: the phrase is transcribed, and the snippet then expands the
        // word the command would have consumed — visible proof the stage was
        // skipped rather than reordered.
        let mut disabled = settings;
        disabled.spoken_edits_enabled = false;
        assert_eq!(
            post_process_transcription_text(
                "Keep this. Drop this. Scratch back.".to_string(),
                &AsrPlan::from_settings(&disabled),
                false,
                &english,
                &[],
            ),
            "Keep this. Drop this. scratch THAT."
        );
    }

    #[test]
    fn portuguese_transcription_does_not_use_english_ui_filler_words() {
        let settings = AppSettings {
            app_language: "en".to_string(),
            selected_language: "pt-BR".to_string(),
            ..Default::default()
        };
        let supported = languages(&["en", "pt"]);
        let evidence = resolve_output_language_evidence(
            &AsrPlan::from_settings(&settings),
            Some("pt"),
            &supported,
            false,
        );

        let result = post_process_transcription_text(
            "eu vi um carro".to_string(),
            &AsrPlan::from_settings(&settings),
            false,
            &evidence,
            &supported,
        );

        assert_eq!(
            evidence,
            OutputLanguageEvidence::UserSelected("pt".to_string())
        );
        assert_eq!(result, "eu vi um carro");
    }

    #[test]
    fn auto_language_without_detection_skips_gated_filler_removal() {
        let settings = AppSettings {
            selected_language: "auto".to_string(),
            ..Default::default()
        };
        let evidence = resolve_output_language_evidence(
            &AsrPlan::from_settings(&settings),
            None,
            &languages(&["en", "pt"]),
            false,
        );

        // Too short for a reliable text detection, so the gated "um" must
        // survive; the universal "uhm" is removed regardless.
        let result = post_process_transcription_text(
            "um uhm ok".to_string(),
            &AsrPlan::from_settings(&settings),
            false,
            &evidence,
            &languages(&["en", "pt"]),
        );

        assert_eq!(evidence, OutputLanguageEvidence::Unknown);
        assert_eq!(result, "um ok");
    }

    #[test]
    fn unknown_evidence_with_confident_text_detection_removes_gated_fillers() {
        let settings = AppSettings {
            selected_language: "auto".to_string(),
            ..Default::default()
        };

        let result = post_process_transcription_text(
            "um so the weather forecast said it would probably rain throughout the whole weekend"
                .to_string(),
            &AsrPlan::from_settings(&settings),
            false,
            &OutputLanguageEvidence::Unknown,
            &languages(&["en", "pt", "es", "de"]),
        );

        assert_eq!(
            result,
            "So the weather forecast said it would probably rain throughout the whole weekend"
        );
    }

    #[test]
    fn unknown_evidence_with_portuguese_text_preserves_um() {
        let settings = AppSettings {
            selected_language: "auto".to_string(),
            ..Default::default()
        };

        let result = post_process_transcription_text(
            "eu vi um carro na rua ontem de manhã quando fui ao mercado".to_string(),
            &AsrPlan::from_settings(&settings),
            false,
            &OutputLanguageEvidence::Unknown,
            &languages(&["en", "pt", "es", "de"]),
        );

        assert_eq!(
            result,
            "eu vi um carro na rua ontem de manhã quando fui ao mercado"
        );
    }

    #[test]
    fn model_detected_language_upgrades_unknown_evidence_only() {
        assert_eq!(
            with_model_detected_language(OutputLanguageEvidence::Unknown, Some("en".to_string())),
            OutputLanguageEvidence::ModelDetected("en".to_string())
        );
        assert_eq!(
            with_model_detected_language(OutputLanguageEvidence::Unknown, Some("auto".to_string())),
            OutputLanguageEvidence::Unknown
        );
        assert_eq!(
            with_model_detected_language(OutputLanguageEvidence::Unknown, None),
            OutputLanguageEvidence::Unknown
        );
        assert_eq!(
            with_model_detected_language(
                OutputLanguageEvidence::UserSelected("pt".to_string()),
                Some("en".to_string())
            ),
            OutputLanguageEvidence::UserSelected("pt".to_string())
        );
    }

    #[test]
    fn auto_language_uses_single_language_model_as_evidence() {
        let settings = AppSettings {
            selected_language: "auto".to_string(),
            ..Default::default()
        };

        let evidence = resolve_output_language_evidence(
            &AsrPlan::from_settings(&settings),
            None,
            &languages(&["en"]),
            false,
        );

        assert_eq!(
            evidence,
            OutputLanguageEvidence::ModelConstrained("en".to_string())
        );
    }

    #[test]
    fn unsupported_explicit_language_uses_model_fallback_as_evidence() {
        let settings = AppSettings {
            selected_language: "pt".to_string(),
            ..Default::default()
        };

        let evidence = resolve_output_language_evidence(
            &AsrPlan::from_settings(&settings),
            Some("en"),
            &languages(&["en", "de"]),
            false,
        );

        assert_eq!(
            evidence,
            OutputLanguageEvidence::ModelConstrained("en".to_string())
        );
    }

    #[test]
    fn ignored_user_language_is_not_output_evidence() {
        let settings = AppSettings {
            // Parakeet V3 ignores language hints and auto-detects even when a
            // selection from the previously active model remains persisted.
            selected_language: "en".to_string(),
            ..Default::default()
        };
        let supported = languages(&["en", "de", "pt"]);

        let evidence = resolve_output_language_evidence(
            &AsrPlan::from_settings(&settings),
            None,
            &supported,
            false,
        );
        assert_eq!(evidence, OutputLanguageEvidence::Unknown);

        let result = post_process_transcription_text(
            "eu vi um carro".to_string(),
            &AsrPlan::from_settings(&settings),
            false,
            &evidence,
            &supported,
        );
        assert_eq!(result, "eu vi um carro");
    }

    #[test]
    fn unapplied_transcribe_cpp_language_is_not_output_evidence() {
        let settings = AppSettings {
            selected_language: "en".to_string(),
            ..Default::default()
        };
        let supported = languages(&[]);
        let plan = transcribe_cpp_run_plan(false, "en", &supported, false);

        assert_eq!(plan.language, None);
        assert_eq!(
            resolve_output_language_evidence(
                &AsrPlan::from_settings(&settings),
                plan.language.as_deref(),
                &supported,
                false,
            ),
            OutputLanguageEvidence::Unknown
        );
    }

    #[test]
    fn translated_output_is_treated_as_english() {
        let settings = AppSettings {
            selected_language: "pt".to_string(),
            ..Default::default()
        };

        let evidence = resolve_output_language_evidence(
            &AsrPlan::from_settings(&settings),
            Some("pt"),
            &languages(&["en", "pt"]),
            true,
        );

        assert_eq!(evidence, OutputLanguageEvidence::TranslatedToEnglish);
    }

    /// The reason `BatchDecode::model_produced_text` has to exist: on a stock
    /// install (filler removal defaults on) a real one-word utterance
    /// post-processes to nothing, so an empty delivered transcript cannot tell
    /// a silent microphone from a filtered one. Both halves are asserted, so
    /// this fails if the pipeline stops emptying it *or* if the flag stops
    /// being read from the raw text.
    #[test]
    fn a_filtered_away_utterance_still_reports_that_the_model_produced_text() {
        let settings = AppSettings::default();
        assert!(
            settings.filler_word_removal_enabled,
            "this test is about the stock install"
        );
        let asr = AsrPlan::from_settings(&settings);
        let english = OutputLanguageEvidence::UserSelected("en-US".to_string());

        let raw = "Um.".to_string();
        let model_produced_text = !raw.trim().is_empty();
        let delivered =
            post_process_transcription_text(raw, &asr, false, &english, &languages(&["en"]));

        assert_eq!(delivered, "", "filler removal empties a bare hesitation");
        assert!(model_produced_text);

        let decode = BatchDecode {
            text: delivered,
            realtime_factor: None,
            model_produced_text,
        };
        assert!(decode.text.is_empty() && decode.model_produced_text);
    }

    /// Filler removal runs before every pass that authors text, and this is the
    /// dependency that decides it: `apply_literal_punctuation` turns dictated
    /// punctuation words into punctuation, so with filler removal at the tail
    /// it saw `a um, b` and took the comma away with the word — `a um comma b`
    /// delivered as `a b`, losing punctuation the user asked for out loud. From
    /// the head it sees `um` and `comma` as the two tokens actually spoken.
    ///
    /// The second case is the class this reorder was made for: a replacement's
    /// written form is text the pipeline authored, and rescanning it for
    /// fillers deleted a word the user never spoke.
    ///
    /// Both directions are asserted, so reordering this pipeline back fails
    /// here rather than in someone's dictation. Note what is deliberately NOT
    /// claimed: pass order protects text the pipeline *authored*, and a filler
    /// the recogniser itself produced is still removed. Whether a given token
    /// counts as a filler at all is the built-in list's business
    /// (`audio_toolkit::text::gated_filler_words_for_language`), which is where
    /// the other half of the `Ha Long Bay` defect was fixed. Neither half is
    /// sufficient alone.
    #[test]
    fn filler_removal_precedes_the_passes_that_author_text() {
        let settings = AppSettings {
            replacements_enabled: true,
            replacements_rules: vec![crate::settings::ReplacementRule {
                spoken: "amei".to_string(),
                written: "Ah Mei".to_string(),
                enabled: true,
            }],
            snippets: vec![crate::snippets::Snippet {
                id: "one".to_string(),
                trigger: "signoff".to_string(),
                expansion: "Yours, Ah Mei".to_string(),
                enabled: true,
                created_at: 0,
                updated_at: 0,
            }],
            ..AppSettings::default()
        };
        assert!(
            settings.filler_word_removal_enabled,
            "this test is about the stock install"
        );
        let mut asr = AsrPlan::from_settings(&settings);
        asr.literal_punctuation = true;
        let english = OutputLanguageEvidence::UserSelected("en-US".to_string());
        let deliver = |raw: &str| {
            post_process_transcription_text(
                raw.to_string(),
                &asr,
                false,
                &english,
                &languages(&["en"]),
            )
        };

        // Dictated punctuation survives a filler spoken beside it.
        assert_eq!(deliver("a um comma b"), "a, b");
        assert_eq!(deliver("hello um period"), "hello.");
        // And a filler is still removed there, so this is not just literal
        // punctuation winning: the same input without the filler agrees.
        assert_eq!(deliver("a comma b"), "a, b");

        // A written form is not rescanned for fillers, and `Ah Mei` is the
        // shape that proves it: `ah` is on the gated English list, so filler
        // removal at the tail deletes it and these two assertions are what
        // fail. `Ha Long Bay` was the reported defect, but `ha` has since left
        // the list, so a written form carrying it can no longer fail in either
        // order - the fixture has to use a token the list still gates.
        assert_eq!(deliver("amei is a singer"), "Ah Mei is a singer");
        // The same for a snippet expansion, which is authored two passes later.
        assert_eq!(deliver("signoff"), "Yours, Ah Mei");

        // A literal mention of a punctuation word is still left alone.
        assert_eq!(deliver("the word comma itself"), "the word comma itself");
    }

    /// `BatchDecode::untimed` never sees post-processing, so its flag is just
    /// the emptiness of the text it carries.
    #[test]
    fn an_untimed_decode_reports_whether_it_carries_text() {
        assert!(!BatchDecode::untimed(String::new()).model_produced_text);
        assert!(!BatchDecode::untimed("  \t\n".to_string()).model_produced_text);
        assert!(BatchDecode::untimed("Test.".to_string()).model_produced_text);
    }

    /// Sona sets exactly one whisper decode field — the vocabulary prompt — and
    /// defers every threshold to transcribe-cpp's own recipe. See
    /// `whisper_run_extension` for the measurements behind that; this pins the
    /// decision so a future edit has to argue with the evidence rather than
    /// quietly re-tune the decoder.
    #[test]
    fn the_whisper_extension_carries_only_the_vocabulary_prompt() {
        assert_eq!(whisper_run_extension(None), None);

        let Some(RunExtension::Whisper(options)) =
            whisper_run_extension(Some("Sona, Tauri.".to_string()))
        else {
            panic!("a prompt must produce a whisper run extension");
        };
        assert_eq!(options.initial_prompt.as_deref(), Some("Sona, Tauri."));
        assert_eq!(options.no_speech_thold, None);
        assert_eq!(options.logprob_thold, None);
        assert_eq!(options.compression_ratio_thold, None);
        assert_eq!(options.temperature, None);
        assert_eq!(options.temperature_inc, None);
    }

    #[test]
    fn transcribe_cpp_run_plan_maps_chinese_variants() {
        let plan = transcribe_cpp_run_plan(false, "zh-Hant", &languages(&["zh"]), true);

        assert!(matches!(plan.task, Task::Transcribe));
        assert_eq!(plan.language.as_deref(), Some("zh"));
        assert_eq!(plan.target_language, None);
    }

    #[test]
    fn transcribe_cpp_run_plan_skips_english_translation() {
        let plan = transcribe_cpp_run_plan(true, "en", &languages(&["en", "es"]), true);

        assert!(matches!(plan.task, Task::Transcribe));
        assert_eq!(plan.language.as_deref(), Some("en"));
        assert_eq!(plan.target_language, None);
    }

    #[test]
    fn transcribe_cpp_run_plan_translates_supported_non_english() {
        let plan = transcribe_cpp_run_plan(true, "es", &languages(&["en", "es"]), true);

        assert!(matches!(plan.task, Task::Translate));
        assert_eq!(plan.language.as_deref(), Some("es"));
        assert_eq!(plan.target_language.as_deref(), Some("en"));
    }

    #[test]
    fn transcribe_cpp_run_plan_requires_model_translation_support() {
        let plan = transcribe_cpp_run_plan(true, "es", &languages(&["en", "es"]), false);

        assert!(matches!(plan.task, Task::Transcribe));
        assert_eq!(plan.language.as_deref(), Some("es"));
        assert_eq!(plan.target_language, None);
    }

    #[cfg(feature = "cloud-realtime")]
    #[test]
    fn stream_engine_event_encodes_cloud_and_local_fallback() {
        assert_eq!(
            serde_json::to_value(StreamEngineEvent {
                engine: StreamEngine::Cloud,
            })
            .expect("cloud event JSON"),
            serde_json::json!({ "engine": "cloud" })
        );
        assert_eq!(
            serde_json::to_value(StreamEngineEvent {
                engine: StreamEngine::LocalFallback,
            })
            .expect("fallback event JSON"),
            serde_json::json!({ "engine": "local_fallback" })
        );
    }

    #[cfg(feature = "cloud-realtime")]
    #[test]
    fn fake_cloud_transport_returns_interim_timestamped_final_and_close_with_full_pcm() {
        use crate::cloud_stt::{CloudEvent, CloudWord};

        let state = Arc::new(Mutex::new(FakeCloudTransportState {
            pre_finalize_events: VecDeque::from([Ok(CloudEvent::Interim {
                text: "provider interim".to_string(),
                words: Vec::new(),
            })]),
            post_finalize_events: VecDeque::from([
                Ok(CloudEvent::Final {
                    text: "provider final".to_string(),
                    words: vec![CloudWord {
                        text: "provider".to_string(),
                        start: Duration::from_millis(0),
                        end: Duration::from_millis(250),
                        speaker: None,
                    }],
                }),
                Ok(CloudEvent::Closed),
            ]),
            ..Default::default()
        }));
        let factory = Arc::new(FakeCloudTransportFactory {
            state: Arc::clone(&state),
        });
        let frames = vec![vec![0.25, -0.5], vec![0.75, -1.0]];

        let finalization = run_fake_cloud(factory, &frames);

        assert_eq!(
            finalization,
            CloudStreamFinalization::Final("provider final".to_string())
        );
        let state = state.lock().expect("fake cloud state");
        assert_eq!(state.connects, 1);
        assert_eq!(state.finalize_calls, 1);
        assert_eq!(state.frames, frames);
        assert_eq!(state.plans.len(), 1);
        assert_eq!(state.plans[0].provider(), CloudSttProvider::DeepgramNova3);
        assert!(state.plans[0].timestamps());
    }

    #[cfg(feature = "cloud-realtime")]
    #[test]
    fn cloud_disconnect_maps_to_a_failed_finalization_at_every_frame_boundary() {
        let frames = vec![vec![0.1], vec![0.2], vec![0.3]];

        for boundary in 0..=frames.len() {
            let mut fake = FakeCloudTransportState::default();
            if boundary == 0 {
                fake.send_failure_at = Some((0, CloudStreamFailure::Disconnected));
            } else {
                fake.disconnect_after_frames = Some(boundary);
            }
            let state = Arc::new(Mutex::new(fake));
            let factory = Arc::new(FakeCloudTransportFactory {
                state: Arc::clone(&state),
            });

            let finalization = run_fake_cloud(factory, &frames);

            assert_eq!(
                finalization,
                CloudStreamFinalization::Failed {
                    failure: CloudStreamFailure::Disconnected,
                    audio_sent: boundary != 0,
                },
                "disconnect boundary {boundary}"
            );
            assert_eq!(
                state.lock().expect("fake cloud state").frames,
                frames[..boundary].to_vec(),
                "PCM captured before boundary {boundary}"
            );
        }
    }

    #[cfg(feature = "cloud-realtime")]
    #[test]
    fn fake_cloud_transport_maps_terminal_failures_without_provider_data() {
        let one_frame = vec![vec![0.25, -0.5]];
        let assert_failure = |fake: FakeCloudTransportState,
                              expected_failure: CloudStreamFailure,
                              audio_sent: bool| {
            let state = Arc::new(Mutex::new(fake));
            let factory = Arc::new(FakeCloudTransportFactory {
                state: Arc::clone(&state),
            });
            assert_eq!(
                run_fake_cloud(factory, &one_frame),
                CloudStreamFinalization::Failed {
                    failure: expected_failure,
                    audio_sent,
                }
            );
        };

        for failure in [
            CloudStreamFailure::Authentication,
            CloudStreamFailure::Quota,
            CloudStreamFailure::Protocol,
        ] {
            assert_failure(
                FakeCloudTransportState {
                    connect_failure: Some(failure),
                    ..Default::default()
                },
                failure,
                false,
            );
        }
        assert_failure(
            FakeCloudTransportState {
                send_failure_at: Some((0, CloudStreamFailure::Backpressure)),
                ..Default::default()
            },
            CloudStreamFailure::Backpressure,
            false,
        );
        assert_failure(
            FakeCloudTransportState {
                post_finalize_events: VecDeque::from([Ok(crate::cloud_stt::CloudEvent::Closed)]),
                ..Default::default()
            },
            CloudStreamFailure::MissingFinal,
            true,
        );
    }

    /// A key that cannot be resolved must look exactly like a provider that
    /// could not be reached: no frame was sent, so the run is free to fall back
    /// to the frozen local model or be held.
    #[cfg(feature = "cloud-realtime")]
    #[test]
    fn unresolvable_key_fails_before_connect_without_sending_audio() {
        let state = Arc::new(Mutex::new(FakeCloudTransportState::default()));
        let factory = Arc::new(FakeCloudTransportFactory {
            state: Arc::clone(&state),
        });

        let finalization = run_fake_cloud_with_key(factory, &[vec![0.25, -0.5]], || {
            Err(CloudStreamFailure::KeyUnavailable)
        });

        assert_eq!(
            finalization,
            CloudStreamFinalization::Failed {
                failure: CloudStreamFailure::KeyUnavailable,
                audio_sent: false,
            }
        );
        let state = state.lock().expect("fake cloud state");
        assert_eq!(state.connects, 0);
        assert!(state.frames.is_empty());
    }

    #[cfg(feature = "cloud-realtime")]
    #[test]
    fn key_resolution_errors_map_to_one_terminal_failure_each() {
        use crate::secrets::SttSecretVerificationError as KeyError;

        for (error, expected) in [
            (KeyError::NotConfigured, CloudStreamFailure::Authentication),
            (KeyError::Authentication, CloudStreamFailure::Authentication),
            (KeyError::Quota, CloudStreamFailure::Quota),
            (KeyError::Network, CloudStreamFailure::Network),
            (KeyError::Unavailable, CloudStreamFailure::KeyUnavailable),
            (KeyError::Locked, CloudStreamFailure::KeyUnavailable),
            (KeyError::Busy, CloudStreamFailure::KeyUnavailable),
            (KeyError::Backend, CloudStreamFailure::KeyUnavailable),
            (KeyError::Corrupt, CloudStreamFailure::KeyUnavailable),
            (KeyError::Invalid, CloudStreamFailure::KeyUnavailable),
            (
                KeyError::ConsentRequired,
                CloudStreamFailure::KeyUnavailable,
            ),
            (KeyError::Protocol, CloudStreamFailure::KeyUnavailable),
        ] {
            assert_eq!(
                CloudStreamFailure::from(error),
                expected,
                "key error {error:?}"
            );
        }
    }
}

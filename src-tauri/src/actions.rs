#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use crate::apple_intelligence;
use crate::audio_feedback::{play_feedback_sound, play_feedback_sound_blocking, SoundType};
use crate::audio_toolkit::{is_microphone_access_denied, is_no_input_device_error, VadPolicy};
use crate::delivery::{self, DeliveryOutcome, DeliveryReceipt};
use crate::managers::audio::{AudioRecordingManager, InputLevel, RecordingStop};
use crate::managers::history::{CaptureStatus, HistoryManager, NewRunReceipt};
use crate::managers::model::ModelManager;
#[cfg(feature = "cloud-realtime")]
use crate::managers::transcription::CloudStreamFinalization;
#[cfg(feature = "cloud-realtime")]
use crate::managers::transcription::StreamEngine;
use crate::managers::transcription::{BatchDecode, StreamWorkKind, TranscriptionManager};
use crate::modes::{AsrPlan, CloudReceiptStatus, RequestedEngine, RewriteOutcome, RunPlan};
use crate::prompt_renderer::RenderedPrompt;
use crate::secrets::{SecretAccount, SecretManager, SecretResolveError};
use crate::settings::{get_settings, OverlayStyle, APPLE_INTELLIGENCE_PROVIDER_ID};
use crate::shortcut;
use crate::tray::{set_tray_state, TrayIconState};
use crate::utils::{
    self, show_processing_overlay, show_recording_overlay, show_transcribing_overlay,
};
use crate::TranscriptionCoordinator;
use ferrous_opencc::{config::BuiltinConfig, OpenCC};
use log::{debug, error, warn};
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::Manager;
use tauri::{AppHandle, Emitter};

const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(25);

const TRANSCRIPTION_FAILURE_LOG_MESSAGE: &str = "Transcription failed; no text was delivered";
const TRANSCRIPTION_FAILURE_EVENT_MESSAGE: &str = "Transcription failed";

/// The one user-visible run-failure lane, emitted as `recording-error` by this
/// module and by the capture owner. `error_type` names the failure; no field
/// carries transcript, audio, sample counts, or provider text.
#[derive(Clone, serde::Serialize)]
pub(crate) struct RecordingErrorEvent {
    error_type: String,
    /// Sub-code for `cloud_unavailable`, so the frontend can pick exact copy
    /// without parsing prose.
    #[serde(skip_serializing_if = "Option::is_none")]
    cloud_kind: Option<&'static str>,
    /// Retained for the frontend's event shape; diagnostic events always leave
    /// it empty so arbitrary OS or provider text cannot cross the event boundary.
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

impl RecordingErrorEvent {
    pub(crate) fn typed(error_type: &str) -> Self {
        Self {
            error_type: error_type.to_string(),
            cloud_kind: None,
            detail: None,
        }
    }
}

/// `paste-error`: the words were transcribed and kept, but never left Sona.
/// The id says which history entry holds them, so a shell can open it; it is
/// `None` when history is off and nothing was kept.
#[derive(Clone, serde::Serialize)]
pub(crate) struct PasteErrorEvent {
    history_id: Option<i64>,
}

/// `rewrite-skipped`: the mode asked for a rewrite and the words went out as
/// spoken instead. `outcome` says why; the id names the history entry that
/// keeps the receipt, `None` when history is off.
#[derive(Clone, serde::Serialize)]
pub(crate) struct RewriteSkippedEvent {
    history_id: Option<i64>,
    outcome: RewriteOutcome,
}

impl RewriteSkippedEvent {
    /// Only an outcome the user did not choose is worth a notice: a mode
    /// without a rewrite and a rewrite that landed are both the plan working.
    fn for_outcome(history_id: Option<i64>, outcome: RewriteOutcome) -> Option<Self> {
        match outcome {
            RewriteOutcome::NotRequested | RewriteOutcome::Applied => None,
            RewriteOutcome::Unavailable
            | RewriteOutcome::NoCredential
            | RewriteOutcome::TooLong
            | RewriteOutcome::Failed => Some(Self {
                history_id,
                outcome,
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NoSpeechPersistence {
    Cancelled,
    SaveFailed,
    Saved,
}

impl NoSpeechPersistence {
    const fn recording_error_type(self) -> Option<&'static str> {
        match self {
            Self::Cancelled => None,
            Self::SaveFailed => Some("no_speech_save_failed"),
            Self::Saved => Some("no_speech_detected"),
        }
    }
}

/// Why a cloud-configured run could not start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloudRunError {
    #[cfg(feature = "cloud-realtime")]
    NativeKey,
    #[cfg(feature = "cloud-realtime")]
    FallbackModelUnavailable,
    /// Reachable only in a build compiled without `cloud-realtime`, where a
    /// cloud-configured mode has no transport at all.
    #[cfg(not(feature = "cloud-realtime"))]
    FeatureUnavailable,
}

impl CloudRunError {
    /// `(wire sub-code, log line)`. The sub-code is a contract the frontend
    /// maps to translated copy; the message is for the log and never reaches
    /// the UI.
    const fn describe(self) -> (&'static str, &'static str) {
        match self {
            #[cfg(feature = "cloud-realtime")]
            Self::NativeKey => (
                "native_key",
                "Cloud transcription needs a configured native provider key.",
            ),
            #[cfg(feature = "cloud-realtime")]
            Self::FallbackModelUnavailable => (
                "fallback_model_unavailable",
                "Cloud transcription needs its frozen local fallback model installed.",
            ),
            #[cfg(not(feature = "cloud-realtime"))]
            Self::FeatureUnavailable => (
                "feature_unavailable",
                "Cloud transcription is unavailable in this build.",
            ),
        }
    }
}

fn emit_cloud_run_error(app: &AppHandle, error: CloudRunError) {
    let (kind, message) = error.describe();
    warn!("{message}");
    let _ = app.emit(
        "recording-error",
        RecordingErrorEvent {
            error_type: "cloud_unavailable".to_string(),
            cloud_kind: Some(kind),
            detail: None,
        },
    );
}

/// Drop guard that notifies the [`TranscriptionCoordinator`] when the
/// transcription pipeline finishes — whether it completes normally or panics.
struct FinishGuard(AppHandle);
impl Drop for FinishGuard {
    fn drop(&mut self) {
        if let Some(c) = self.0.try_state::<TranscriptionCoordinator>() {
            c.notify_processing_finished();
        }
        // The pipeline just freed its large transient buffers (captured PCM,
        // WAV copy, engine scratch); hand the cached pages back to the OS so
        // they don't sit in malloc arenas until they get swapped out (#1792).
        crate::memory::trim_freed_memory();
    }
}

// Transcribe Action. One instance carries exactly one frozen run plan, so a
// settings edit between start and stop cannot change the run in flight.
struct TranscribeAction {
    run: RunPlan,
}

#[cfg(feature = "cloud-realtime")]
fn ensure_cloud_fallback_is_installed(
    run: &RunPlan,
    is_installed: impl FnOnce(&str) -> bool,
) -> Result<(), CloudRunError> {
    let Some(fallback) = run.local_asr() else {
        return Ok(());
    };

    if is_installed(&fallback.model_id) {
        Ok(())
    } else {
        Err(CloudRunError::FallbackModelUnavailable)
    }
}

/// The pure preflight decision: everything a cloud run must satisfy *before*
/// capture. Both lookups read state that is already in memory — resolving the
/// credential itself is explicitly not part of this, see [`cloud_key_source`].
#[cfg(feature = "cloud-realtime")]
fn check_cloud_preconditions(
    run: &RunPlan,
    is_installed: impl FnOnce(&str) -> bool,
    key_configured: impl FnOnce(crate::modes::CloudSttProvider) -> bool,
) -> Result<(), CloudRunError> {
    let Some(cloud) = run.cloud() else {
        return Ok(());
    };

    ensure_cloud_fallback_is_installed(run, is_installed)?;

    // The cached provider state answers the common misconfiguration (no key
    // ever saved) immediately. A key deleted out of band still passes here and
    // degrades on the worker into fallback or a held run, which is visible in
    // history — never a silent success.
    if key_configured(cloud.provider()) {
        Ok(())
    } else {
        Err(CloudRunError::NativeKey)
    }
}

/// Why this run must not open the microphone, when its local decode cannot
/// happen: `no_model_selected` when no model is selected or the selected one
/// is unknown, `model_not_downloaded` when its file is not on disk.
/// `is_downloaded` answers `None` for a model id Sona does not know. Cloud
/// runs answer for their own frozen fallback in the cloud preflight, and a
/// cloud run without a fallback still has a provider to transcribe with.
fn local_model_refusal(
    run: &RunPlan,
    is_downloaded: impl FnOnce(&str) -> Option<bool>,
) -> Option<&'static str> {
    if run.cloud().is_some() {
        return None;
    }
    let model_id = run.local_asr().map_or("", |local| local.model_id.as_str());
    if model_id.trim().is_empty() {
        return Some("no_model_selected");
    }
    match is_downloaded(model_id) {
        None => Some("no_model_selected"),
        Some(false) => Some("model_not_downloaded"),
        Some(true) => None,
    }
}

/// Code on the coordinator thread must not perform keychain, network, or other
/// unbounded-blocking I/O: it also has to service the next keypress, including
/// cancel. Reading the in-memory settings cache is the ceiling.
#[cfg(feature = "cloud-realtime")]
fn cloud_preflight(app: &AppHandle, run: &RunPlan) -> Result<(), CloudRunError> {
    check_cloud_preconditions(
        run,
        |model_id| {
            app.state::<Arc<ModelManager>>()
                .get_model_info(model_id)
                .is_some_and(|model| model.is_downloaded)
        },
        |provider| {
            get_settings(app)
                .cloud_stt_provider(provider)
                .is_some_and(|settings| settings.secret_state.configured)
        },
    )
}

/// Resolves this run's provider key on the cloud worker. Returned as a closure
/// so the keychain read — which can block on an OS prompt or a locked secret
/// service — never runs on the coordinator's serialization thread.
#[cfg(feature = "cloud-realtime")]
fn cloud_key_source(
    app: &AppHandle,
    provider: crate::modes::CloudSttProvider,
) -> crate::managers::transcription::CloudKeySource {
    let secrets = Arc::clone(&app.state::<Arc<SecretManager>>());
    Box::new(move || {
        tauri::async_runtime::block_on(crate::secrets::resolve_stt_secret(
            secrets.as_ref(),
            provider,
        ))
        .map_err(|error| {
            warn!("Cloud provider key could not be resolved: {error:?}");
            error.into()
        })
    })
}

#[cfg(not(feature = "cloud-realtime"))]
fn cloud_preflight(_app: &AppHandle, run: &RunPlan) -> Result<(), CloudRunError> {
    if run.cloud().is_some() {
        Err(CloudRunError::FeatureUnavailable)
    } else {
        Ok(())
    }
}

/// Begin a recording for an already-frozen run plan.
pub fn start_transcription(app: &AppHandle, binding_id: &str, shortcut_str: &str, run: &RunPlan) {
    TranscribeAction { run: run.clone() }.start(app, binding_id, shortcut_str);
}

/// Finish the recording started with this exact run plan.
pub fn stop_transcription(app: &AppHandle, binding_id: &str, shortcut_str: &str, run: RunPlan) {
    TranscribeAction { run }.stop(app, binding_id, shortcut_str);
}

/// Field name for structured output JSON schema
const TRANSCRIPTION_FIELD: &str = "transcription";

/// Strip invisible Unicode characters that some LLMs may insert
fn strip_invisible_chars(s: &str) -> String {
    s.replace(['\u{200B}', '\u{200C}', '\u{200D}', '\u{FEFF}'], "")
}

/// Strip a leading `<think>...</think>` block. Some endpoints can't disable
/// reasoning, and some local servers put the reasoning text into `content`
/// instead of a separate field — without this the user would get the model's
/// chain of thought pasted along with the cleaned transcription.
fn strip_think_block(s: &str) -> &str {
    if let Some(rest) = s.trim_start().strip_prefix("<think>") {
        if let Some(end) = rest.find("</think>") {
            return rest[end + "</think>".len()..].trim_start();
        }
    }
    s
}

/// What a rewrite may replace the dictation with: a non-blank answer. A blank
/// one is the provider declining, and typing nothing in place of the words
/// would lose every one of them.
fn accept_rewrite(content: &str) -> Result<String, RewriteOutcome> {
    let cleaned = strip_invisible_chars(strip_think_block(content));
    if cleaned.trim().is_empty() {
        warn!("Post-processing returned no text; delivering the raw transcript");
        return Err(RewriteOutcome::Failed);
    }
    Ok(cleaned)
}

/// The rewrite inside a structured answer.
///
/// The schema asks for one object with the transcription field. An endpoint
/// that ignores the schema answers in prose, and that prose is the rewrite.
/// One that answers with an object missing the field, or with JSON that does
/// not parse, answered the wrong question: its bytes are not the dictation
/// and must not be typed in its place.
fn structured_rewrite(content: &str) -> Result<String, StructuredRewriteError> {
    let content = strip_think_block(content);
    let body = strip_code_fence(content);
    if !body.trim_start().starts_with('{') {
        return Ok(body.to_string());
    }
    let json: serde_json::Value =
        serde_json::from_str(body).map_err(|_| StructuredRewriteError::Malformed)?;
    json.get(TRANSCRIPTION_FIELD)
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .ok_or(StructuredRewriteError::MissingField)
}

#[derive(Debug, PartialEq, Eq)]
enum StructuredRewriteError {
    Malformed,
    MissingField,
}

/// A fenced ```json block around an object is still that object.
fn strip_code_fence(content: &str) -> &str {
    let trimmed = content.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let rest = rest.find('\n').map_or("", |newline| &rest[newline + 1..]);
    rest.strip_suffix("```").unwrap_or(rest).trim()
}

/// Returns `true` when a transcription has no meaningful content to
/// post-process (empty or whitespace-only). Used to skip the post-processing
/// LLM call when nothing was actually transcribed, which would otherwise make
/// the model reply with an error message such as "you need to provide the
/// transcription".
fn is_blank_transcription(transcription: &str) -> bool {
    transcription.trim().is_empty()
}

/// Whether one capture produced usable speech.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureVerdict {
    Transcribed,
    NoSpeech,
}

/// Settle whether a capture had speech, given what VAD forwarded and whether
/// the model produced any text for it.
///
/// VAD is an optimizer, not a gatekeeper. When it forwards speech its answer is
/// already confirmed by the audio the engine received. When it forwards nothing
/// the capture layer hands the model the raw clip anyway (short captures only),
/// so an empty decode — not VAD's silence — is what makes no-speech a fact.
///
/// The second argument is what the *model* produced, not what survived Sona's
/// post-processing, because two post-processing stages empty a non-empty
/// transcript by design: `apply_spoken_edits`, where deleting the only clause
/// is the correct answer to "scratch that", and `remove_filler_words`, which is
/// on by default and empties a capture whose whole output is one hesitation
/// token. Either coinciding with `vad_forwarded_speech == false` would mislabel
/// a heard capture as no-speech, and the two correlate: a clip too quiet for VAD
/// is also the clip most likely to decode to a bare "Um.". Such a capture is
/// `Transcribed` with an empty delivery — the same outcome the filters already
/// produce when VAD did forward speech, so the two stay consistent.
fn capture_verdict(vad_forwarded_speech: bool, model_produced_text: bool) -> CaptureVerdict {
    if vad_forwarded_speech || model_produced_text {
        CaptureVerdict::Transcribed
    } else {
        CaptureVerdict::NoSpeech
    }
}

fn share_completed_pcm(samples: Vec<f32>) -> Arc<Vec<f32>> {
    Arc::new(samples)
}

/// Pick the transcript that will be delivered: the streamed text when the
/// stream produced one, otherwise a batch decode of the whole capture.
///
/// "Produced one" means the model emitted text, not that text survived
/// post-processing. A stream whose transcript was legitimately emptied — a
/// spoken "scratch that", or a filler-only capture — has already answered the
/// question, and re-decoding the whole clip would spend a full batch decode to
/// arrive at the same empty string.
///
/// Streamed text carries no realtime factor because no batch decode happened,
/// which is why the return type is the batch shape either way — the caller
/// records what was measured, and a stream measures nothing here.
fn select_final_transcription<F>(
    stream_result: anyhow::Result<Option<BatchDecode>>,
    samples: &[f32],
    batch: F,
) -> anyhow::Result<BatchDecode>
where
    F: FnOnce(&[f32]) -> anyhow::Result<BatchDecode>,
{
    match stream_result {
        Ok(Some(decode)) if decode.model_produced_text => Ok(decode),
        Ok(_) => batch(samples),
        Err(err) => Err(err),
    }
}

#[derive(Debug)]
enum FrozenTranscript {
    Final {
        text: String,
        engine_used: RequestedEngine,
        cloud_status: CloudReceiptStatus,
        /// The local batch decode's realtime factor, when one produced this
        /// text. A provider final and a streamed transcript both leave it None.
        realtime_factor: Option<f32>,
        /// Whether the engine produced any text before Sona's post-processing.
        /// `text` can be empty while this is true, which is a delivered-nothing
        /// run and not a silent capture.
        model_produced_text: bool,
    },
    HeldCloudUnavailable,
}

#[cfg(feature = "cloud-realtime")]
fn resolve_cloud_finalization<F>(
    run: &RunPlan,
    cloud: &crate::modes::CloudRunPlan,
    finalization: CloudStreamFinalization,
    samples: &[f32],
    decode_fallback: F,
) -> anyhow::Result<FrozenTranscript>
where
    F: FnOnce(&AsrPlan, &[f32]) -> anyhow::Result<BatchDecode>,
{
    match finalization {
        CloudStreamFinalization::Final(text) => {
            // The provider's own words decide whether it produced text; the
            // user's passes below may legitimately empty them.
            let model_produced_text = !text.trim().is_empty();
            Ok(FrozenTranscript::Final {
                text: crate::managers::transcription::finalize_cloud_text(text, run.asr(), cloud),
                engine_used: run.requested_engine(),
                cloud_status: CloudReceiptStatus::Final,
                realtime_factor: None,
                model_produced_text,
            })
        }
        CloudStreamFinalization::Failed { failure, .. } => {
            let Some(fallback) = run.local_asr() else {
                debug!("Cloud final unavailable without local fallback: {failure:?}");
                return Ok(FrozenTranscript::HeldCloudUnavailable);
            };

            let decode = decode_fallback(fallback, samples)?;
            Ok(FrozenTranscript::Final {
                text: decode.text,
                engine_used: RequestedEngine::Local,
                cloud_status: CloudReceiptStatus::Fallback,
                realtime_factor: decode.realtime_factor,
                model_produced_text: decode.model_produced_text,
            })
        }
    }
}

#[cfg(feature = "cloud-realtime")]
fn transcribe_frozen_run(
    manager: &TranscriptionManager,
    run: &RunPlan,
    samples: &[f32],
) -> anyhow::Result<FrozenTranscript> {
    if let Some(cloud) = run.cloud() {
        let finalization = manager.finalize_cloud_stream();
        if matches!(&finalization, CloudStreamFinalization::Failed { .. })
            && run.local_asr().is_some()
        {
            // Provider text was preview-only. Clear it before the single
            // full-PCM local decode selects the delivery transcript.
            manager.clear_stream_preview();
            manager.emit_stream_engine(StreamEngine::LocalFallback);
        }
        resolve_cloud_finalization(run, cloud, finalization, samples, |fallback, audio| {
            manager.transcribe_shared(fallback, audio)
        })
    } else {
        let decode =
            select_final_transcription(manager.finalize_stream(run.asr()), samples, |audio| {
                manager.transcribe_shared(run.asr(), audio)
            })?;
        Ok(FrozenTranscript::Final {
            text: decode.text,
            engine_used: RequestedEngine::Local,
            cloud_status: CloudReceiptStatus::NotRequested,
            realtime_factor: decode.realtime_factor,
            model_produced_text: decode.model_produced_text,
        })
    }
}

#[cfg(not(feature = "cloud-realtime"))]
fn transcribe_frozen_run(
    manager: &TranscriptionManager,
    run: &RunPlan,
    samples: &[f32],
) -> anyhow::Result<FrozenTranscript> {
    if run.cloud().is_some() {
        Ok(FrozenTranscript::HeldCloudUnavailable)
    } else {
        let decode =
            select_final_transcription(manager.finalize_stream(run.asr()), samples, |audio| {
                manager.transcribe_shared(run.asr(), audio)
            })?;
        Ok(FrozenTranscript::Final {
            text: decode.text,
            engine_used: RequestedEngine::Local,
            cloud_status: CloudReceiptStatus::NotRequested,
            realtime_factor: decode.realtime_factor,
            model_produced_text: decode.model_produced_text,
        })
    }
}

async fn complete_unless_cancelled<F, C>(operation: F, is_cancelled: C) -> Option<F::Output>
where
    F: Future,
    C: Fn() -> bool,
{
    tokio::pin!(operation);

    loop {
        if is_cancelled() {
            return None;
        }

        if let Ok(result) =
            tokio::time::timeout(CANCELLATION_POLL_INTERVAL, operation.as_mut()).await
        {
            return Some(result);
        }
    }
}

fn should_use_streaming_overlay(style: OverlayStyle, is_streaming: bool) -> bool {
    style == OverlayStyle::Live && is_streaming
}

fn provider_allows_unauthenticated_request(
    provider: &crate::settings::PostProcessProvider,
    endpoint: &crate::settings::PostProcessEndpoint,
) -> bool {
    provider.id == "custom" && !endpoint.is_remote()
}

/// Ask these providers to skip reasoning/thinking — post-processing rarely
/// benefits from it and it adds seconds of latency. llm_client picks the
/// field the endpoint understands and retries without it if rejected.
fn disables_reasoning(provider: &crate::settings::PostProcessProvider) -> bool {
    matches!(provider.id.as_str(), "custom" | "openrouter")
}

/// The one message a prose rewrite sends: the instructions, then the
/// envelope. A warm-up sends the instructions with no envelope, so both go
/// through here and cannot disagree on how the two are joined.
fn prose_rewrite_prompt(system_message: &str, user_message: &str) -> String {
    format!("{system_message}\n\n{user_message}")
}

/// The first rewrite after a local model unloads measured 7.3 s on
/// `gemma4:12b-mlx`: 4.0 s loading the model and 2.3 s reading the
/// instructions before 1.0 s of writing. Neither needs the words, so both can
/// happen while the user is still speaking. This asks the rewrite's own
/// endpoint for one token over the instructions the rewrite will open with;
/// the endpoint loads the model and keeps what it read, and that first
/// rewrite then measured 1.4 s. A recording that is cancelled or silent still
/// loads the model, which the server unloads again on its own idle timer.
///
/// Only a keyless endpoint on this Mac is warmed. Any other would be sent a
/// request, and possibly a credential, before there is anything to rewrite.
fn rewrite_warm_up(run: &RunPlan) -> Option<impl Future<Output = ()> + Send + 'static> {
    let llm = run.prompt().llm.as_ref()?;
    if !provider_allows_unauthenticated_request(&llm.provider, &llm.endpoint)
        || llm.model_id.trim().is_empty()
    {
        return None;
    }
    let provider = llm.provider.clone();
    let endpoint = llm.endpoint.clone();
    let model = llm.model_id.clone();
    let prompt = prose_rewrite_prompt(&crate::prompt_renderer::system_message(run), "");
    Some(async move {
        let started = Instant::now();
        match crate::llm_client::warm_up_chat_completion(
            &provider,
            &endpoint,
            &model,
            prompt,
            disables_reasoning(&provider),
        )
        .await
        {
            Ok(()) => debug!("Rewrite model warmed in {:?}", started.elapsed()),
            Err(failure) => debug!("Rewrite warm-up got no usable reply ({failure})"),
        }
    })
}

/// Runs the frozen rewrite provider over an already-rendered prompt. Voice
/// command mode renders a different prompt but must not fork the provider,
/// credential, structured-output, and fallback handling below.
///
/// The error is how the rewrite ended, never `Applied` or `NotRequested`:
/// the caller delivers the raw words and records the reason with them.
pub(crate) async fn post_process_transcription(
    app: &AppHandle,
    run: &RunPlan,
    rendered: &RenderedPrompt,
    transcription: &str,
) -> Result<String, RewriteOutcome> {
    if is_blank_transcription(transcription) {
        debug!("Post-processing skipped because the transcription is empty");
        return Err(RewriteOutcome::Failed);
    }

    // The run plan freezes the provider and model at recording start. The
    // credential is resolved immediately before provider I/O instead.
    let Some(llm) = run.prompt().llm.as_ref() else {
        debug!("Post-processing skipped because this run resolved no provider");
        return Err(RewriteOutcome::Unavailable);
    };
    let provider = llm.provider.clone();
    let endpoint = llm.endpoint.clone();
    if !provider.endpoint().is_ok_and(|current| current == endpoint) {
        warn!("Post-processing skipped because its frozen destination changed");
        return Err(RewriteOutcome::Unavailable);
    }
    let model = llm.model_id.clone();

    if model.trim().is_empty() {
        debug!("Post-processing skipped because no model is configured");
        return Err(RewriteOutcome::Unavailable);
    }

    debug!("Starting LLM post-processing");

    let disable_reasoning = disables_reasoning(&provider);

    if provider.supports_structured_output && provider.id == APPLE_INTELLIGENCE_PROVIDER_ID {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            // Which reason it is decides what the user would have to do about
            // it; the meetings and settings pages say so, and this line is the
            // record of a dictation that went out uncleaned.
            if let Some(reason) = apple_intelligence::apple_intelligence_blocker() {
                warn!("Post-processing skipped because {reason}");
                return Err(RewriteOutcome::Unavailable);
            }

            let token_limit = model.trim().parse::<i32>().unwrap_or(0);
            return match apple_intelligence::process_text_with_system_prompt(
                &rendered.system_message,
                &rendered.user_message,
                token_limit,
            ) {
                Ok(result) => accept_rewrite(&result),
                Err(error) => {
                    warn!("Apple Intelligence post-processing failed: {error}");
                    Err(RewriteOutcome::Failed)
                }
            };
        }

        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            debug!("Apple Intelligence provider selected on an unsupported platform");
            return Err(RewriteOutcome::Unavailable);
        }
    }

    let secret = {
        let account = match SecretAccount::llm(&provider.id) {
            Ok(account) => account,
            Err(_) => {
                warn!("Post-processing skipped because its credential account is invalid");
                return Err(RewriteOutcome::NoCredential);
            }
        };
        if provider_allows_unauthenticated_request(&provider, &endpoint) {
            None
        } else {
            let secrets = app.state::<Arc<SecretManager>>();
            match secrets.resolve(account).await {
                Ok(secret) => Some(secret),
                Err(SecretResolveError::NotFound) => {
                    warn!("Post-processing skipped because no credential is configured");
                    return Err(RewriteOutcome::NoCredential);
                }
                Err(SecretResolveError::Store(error)) => {
                    warn!(
                        "Post-processing skipped because credential access failed ({:?})",
                        error.kind
                    );
                    return Err(RewriteOutcome::NoCredential);
                }
            }
        }
    };

    let attempt = request_rewrite(
        &provider,
        &endpoint,
        secret.as_ref(),
        &model,
        rendered,
        disable_reasoning,
    )
    .await;
    if attempt.credential_proved {
        crate::settings::mark_post_process_secret_verified(app, &provider.id);
    }
    attempt.text
}

/// What the rewrite requests produced, held apart from the app-state write a
/// success implies. Splitting them leaves the request sequence drivable
/// without a running app, which is where the decision below is worth pinning.
struct RewriteAttempt {
    text: Result<String, RewriteOutcome>,
    /// A reply came back over the configured credential, which is what marks
    /// that credential verified. An answer the rewrite could not use still
    /// proves the credential.
    credential_proved: bool,
}

/// Ask the frozen endpoint for the rewrite: structured output first where the
/// provider supports it, prose otherwise or as the one retry.
async fn request_rewrite(
    provider: &crate::settings::PostProcessProvider,
    endpoint: &crate::settings::PostProcessEndpoint,
    secret: Option<&crate::secrets::SecretValue>,
    model: &str,
    rendered: &RenderedPrompt,
    disable_reasoning: bool,
) -> RewriteAttempt {
    use crate::llm_client::ChatCompletionFailure;

    let mut credential_proved = false;
    if provider.supports_structured_output {
        let json_schema = serde_json::json!({
            "type": "object",
            "properties": {
                (TRANSCRIPTION_FIELD): {
                    "type": "string",
                    "description": "The cleaned and processed transcription text"
                }
            },
            "required": [TRANSCRIPTION_FIELD],
            "additionalProperties": false
        });

        match crate::llm_client::send_chat_completion_with_schema(
            crate::llm_client::ChatCompletionInput {
                provider,
                endpoint,
                secret,
                model,
                user_content: rendered.user_message.clone(),
                system_prompt: Some(rendered.system_message.clone()),
                json_schema: Some(crate::llm_client::StructuredOutputSchema(json_schema)),
                disable_reasoning,
            },
        )
        .await
        {
            Ok(Some(content)) => {
                credential_proved = secret.is_some();
                match structured_rewrite(&content) {
                    Ok(text) => {
                        return RewriteAttempt {
                            text: accept_rewrite(&text),
                            credential_proved,
                        }
                    }
                    // The endpoint honoured the request and answered the
                    // wrong thing; asking again in prose is the one retry
                    // that can still produce the rewrite.
                    Err(error) => {
                        warn!("Structured post-processing answered {error:?}; retrying without a schema");
                    }
                }
            }
            Ok(None) => {
                warn!(
                    "Structured post-processing returned no content; delivering the raw transcript"
                );
                return RewriteAttempt {
                    text: Err(RewriteOutcome::Failed),
                    credential_proved,
                };
            }
            // An endpoint that answered is there to answer again, and the
            // prose request asks it something simpler.
            Err(ChatCompletionFailure::Answered(message)) => {
                warn!("Structured post-processing failed ({message}); retrying without a schema");
            }
            // Nothing came back, so a second request buys the same silence at
            // the same price: another `REQUEST_TIMEOUT` before words that were
            // transcribed long ago reach the user. Deliver them now and let
            // the skipped-rewrite notice say the rewrite failed.
            Err(ChatCompletionFailure::Unanswered(message)) => {
                warn!(
                    "Structured post-processing got no reply ({message}); delivering the raw transcript"
                );
                return RewriteAttempt {
                    text: Err(RewriteOutcome::Failed),
                    credential_proved,
                };
            }
        }
    }

    let processed_prompt = prose_rewrite_prompt(&rendered.system_message, &rendered.user_message);
    match crate::llm_client::send_chat_completion(
        provider,
        endpoint,
        secret,
        model,
        processed_prompt,
        disable_reasoning,
    )
    .await
    {
        Ok(Some(content)) => RewriteAttempt {
            text: accept_rewrite(&content),
            credential_proved: secret.is_some(),
        },
        Ok(None) => {
            warn!("Post-processing returned no content; delivering the raw transcript");
            RewriteAttempt {
                text: Err(RewriteOutcome::Failed),
                credential_proved,
            }
        }
        Err(failure) => {
            warn!("Post-processing failed ({failure}); delivering the raw transcript");
            RewriteAttempt {
                text: Err(RewriteOutcome::Failed),
                credential_proved,
            }
        }
    }
}

async fn maybe_convert_chinese_variant(
    effective_language: &str,
    transcription: &str,
) -> Option<String> {
    // Gate on the language the model actually transcribed in (the effective
    // language), not the persisted intent. A leftover zh-Hans/zh-Hant intent
    // from a previously selected model must not run OpenCC S2T/T2S over output a
    // non-Chinese model produced — that would silently rewrite any shared CJK
    // characters (e.g. Japanese kanji) in the result.
    let is_simplified = effective_language == "zh-Hans";
    let is_traditional = effective_language == "zh-Hant";

    if !is_simplified && !is_traditional {
        debug!("effective language is not Simplified or Traditional Chinese; skipping conversion");
        return None;
    }

    debug!(
        "Starting Chinese variant conversion using OpenCC for language: {}",
        effective_language
    );

    // Use OpenCC to convert based on selected language
    let config = if is_simplified {
        // Convert Traditional Chinese to Simplified Chinese
        BuiltinConfig::Tw2sp
    } else {
        // Convert Simplified Chinese to Traditional Chinese
        BuiltinConfig::S2tw
    };

    match OpenCC::from_config(config) {
        Ok(converter) => {
            let converted = converter.convert(transcription);
            debug!(
                "OpenCC translation completed. Input length: {}, Output length: {}",
                transcription.len(),
                converted.len()
            );
            Some(converted)
        }
        Err(e) => {
            error!("Failed to initialize OpenCC converter: {}. Falling back to original transcription.", e);
            None
        }
    }
}

pub(crate) struct ProcessedTranscription {
    pub final_text: String,
    pub post_processed_text: Option<String>,
    /// How the mode's rewrite ended, for the receipt and the shell's notice.
    pub rewrite: RewriteOutcome,
}

/// The measured facts about one capture that reached the engine. Every receipt
/// written on this path takes the whole struct, so a new receipt kind cannot
/// silently omit the measurement.
struct CompletedCapture {
    duration_ms: Option<u64>,
    has_audio: bool,
    level: InputLevel,
}

#[derive(Clone)]
struct PendingHistoryEntry {
    file_name: String,
    transcription: String,
    post_process_requested: bool,
    post_processed_text: Option<String>,
    run_receipt: crate::modes::ModeReceipt,
    context_receipt: crate::context::ContextReceipt,
    started_at_ms: u64,
    duration_ms: Option<u64>,
    word_count: Option<u64>,
    has_audio: bool,
    capture_status: Option<CaptureStatus>,
}

/// What the transcription route actually did for one run: which engine produced
/// the text, how the cloud attempt ended, and how fast the local batch decode
/// ran if one did. Grouped like [`CompletedCapture`] so a receipt cannot record
/// the route without the measurement that came with it.
struct DecodedRun {
    engine_used: RequestedEngine,
    cloud_status: CloudReceiptStatus,
    realtime_factor: Option<f32>,
}

impl PendingHistoryEntry {
    fn from_run(
        file_name: String,
        transcription: String,
        processed: &ProcessedTranscription,
        run: &RunPlan,
        decoded: DecodedRun,
        capture: CompletedCapture,
    ) -> Self {
        Self {
            file_name,
            transcription,
            post_process_requested: run.post_process_requested(),
            post_processed_text: processed.post_processed_text.clone(),
            run_receipt: run
                .mode_receipt_with_cloud_status(Some(decoded.engine_used), decoded.cloud_status)
                .with_input_level(capture.level.peak, capture.level.rms)
                .with_realtime_factor(decoded.realtime_factor)
                .with_rewrite(processed.rewrite),
            context_receipt: run.context().receipt().clone(),
            started_at_ms: run.run_started_at_ms,
            duration_ms: capture.duration_ms,
            word_count: word_count(&processed.final_text),
            has_audio: capture.has_audio,
            capture_status: Some(CaptureStatus::Complete),
        }
    }

    fn held_cloud_unavailable(file_name: String, run: &RunPlan, capture: CompletedCapture) -> Self {
        Self {
            file_name,
            transcription: String::new(),
            post_process_requested: false,
            post_processed_text: None,
            run_receipt: run
                .mode_receipt_with_cloud_status(None, CloudReceiptStatus::HeldCloudUnavailable)
                .with_input_level(capture.level.peak, capture.level.rms),
            context_receipt: run.context().receipt().clone(),
            started_at_ms: run.run_started_at_ms,
            duration_ms: capture.duration_ms,
            word_count: Some(0),
            has_audio: capture.has_audio,
            capture_status: Some(CaptureStatus::Complete),
        }
    }

    fn no_speech(
        file_name: String,
        run: &RunPlan,
        duration_ms: Option<u64>,
        has_audio: bool,
        level: InputLevel,
    ) -> Self {
        Self {
            file_name,
            transcription: String::new(),
            post_process_requested: false,
            post_processed_text: None,
            // VAD rejected the full capture, so neither a local model nor a
            // cloud session supplied text for this receipt. The measured level
            // is what separates a dead input from a quiet room afterwards.
            run_receipt: run
                .mode_receipt_with_cloud_status(None, CloudReceiptStatus::NotRequested)
                .with_input_level(level.peak, level.rms),
            context_receipt: run.context().receipt().clone(),
            started_at_ms: run.run_started_at_ms,
            duration_ms,
            word_count: Some(0),
            has_audio,
            capture_status: Some(CaptureStatus::NoSpeechDetected),
        }
    }

    /// Persist a capture prefix without a transcript. The original run is
    /// permanently marked truncated; only an explicit history retry may decode
    /// the WAV. The prefix is real microphone audio, so it carries its measured
    /// amplitude like any other capture: on a truncated row, whether the
    /// retained prefix was audible at all is what decides a retry.
    fn truncated_capture(file_name: String, run: &RunPlan, capture: CompletedCapture) -> Self {
        Self {
            file_name,
            transcription: String::new(),
            post_process_requested: false,
            post_processed_text: None,
            run_receipt: run
                .mode_receipt()
                .with_input_level(capture.level.peak, capture.level.rms),
            context_receipt: run.context().receipt().clone(),
            started_at_ms: run.run_started_at_ms,
            duration_ms: capture.duration_ms,
            word_count: Some(0),
            has_audio: capture.has_audio,
            capture_status: Some(CaptureStatus::Truncated),
        }
    }

    fn failed(
        file_name: String,
        post_process_requested: bool,
        run: &RunPlan,
        capture: CompletedCapture,
    ) -> Self {
        Self {
            file_name,
            transcription: String::new(),
            post_process_requested,
            post_processed_text: None,
            run_receipt: run
                .mode_receipt()
                .with_input_level(capture.level.peak, capture.level.rms),
            context_receipt: run.context().receipt().clone(),
            started_at_ms: run.run_started_at_ms,
            duration_ms: capture.duration_ms,
            word_count: Some(0),
            has_audio: capture.has_audio,
            capture_status: Some(CaptureStatus::Complete),
        }
    }

    /// `None` is a dictation with no row: saved history is off, or the write
    /// failed. Either way there is nothing for a delivery receipt to join.
    fn save(self, history: &HistoryManager) -> Option<i64> {
        let completed_at_ms = now_ms();
        match history.save_entry_with_receipt(
            self.file_name,
            self.transcription,
            self.post_process_requested,
            self.post_processed_text,
            Some(NewRunReceipt {
                run: self.run_receipt,
                context: self.context_receipt,
                started_at_ms: self.started_at_ms,
                completed_at_ms,
                duration_ms: self.duration_ms,
                word_count: self.word_count,
                source_kind: crate::managers::history::HistorySourceKind::Microphone,
                has_audio: self.has_audio,
                capture_status: self.capture_status,
            }),
        ) {
            Ok(entry) => entry.map(|entry| entry.id),
            Err(error) => {
                error!("Failed to save history entry with run receipt: {error}");
                None
            }
        }
    }
}

fn persist_delivery_attempt(
    history: &HistoryManager,
    history_id: Option<i64>,
    run_id: u64,
    delivery: DeliveryReceipt,
) {
    if let Some(history_id) = history_id {
        if let Err(error) = history.append_delivery_attempt(history_id, run_id, delivery) {
            error!("Failed to append delivery receipt: {error}");
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

fn duration_ms(sample_count: usize) -> Option<u64> {
    u64::try_from(sample_count)
        .ok()?
        .checked_mul(1_000)
        .map(|samples| samples / u64::from(crate::audio_toolkit::constants::WHISPER_SAMPLE_RATE))
}

fn word_count(text: &str) -> Option<u64> {
    u64::try_from(text.split_whitespace().count()).ok()
}

/// Save the contiguous prefix from a capture overrun. It never enters a
/// transcription path. The recorded receipt leaves an explicit retry as the
/// only way to decode the audio.
async fn persist_truncated_capture(
    app: &AppHandle,
    history: &HistoryManager,
    run: &RunPlan,
    prefix_samples: Vec<f32>,
    level: InputLevel,
) {
    let history_id = if prefix_samples.is_empty() {
        warn!("Capture overran before a usable audio prefix reached Sona");
        None
    } else {
        let completed_pcm = share_completed_pcm(prefix_samples);
        let sample_count = completed_pcm.len();
        let duration_ms = duration_ms(sample_count);
        let file_name = format!("sona-{}.wav", chrono::Utc::now().timestamp());
        let wav_path = history.recordings_dir().join(&file_name);
        let wav_path_for_verify = wav_path.clone();
        let samples_for_wav = Arc::clone(&completed_pcm);
        let wav_handle = tauri::async_runtime::spawn_blocking(move || {
            crate::audio_toolkit::save_wav_file(&wav_path, samples_for_wav.as_slice())
        });
        let wav_saved = match wav_handle.await {
            Ok(Ok(())) => {
                match crate::audio_toolkit::verify_wav_file(&wav_path_for_verify, sample_count) {
                    Ok(()) => true,
                    Err(error) => {
                        error!("Truncated-capture WAV verification failed: {error}");
                        false
                    }
                }
            }
            Ok(Err(error)) => {
                error!("Failed to save truncated-capture WAV: {error}");
                false
            }
            Err(error) => {
                error!("Truncated-capture WAV task panicked: {error}");
                false
            }
        };
        PendingHistoryEntry::truncated_capture(
            file_name,
            run,
            CompletedCapture {
                duration_ms,
                has_audio: wav_saved,
                level,
            },
        )
        .save(history)
    };

    persist_delivery_attempt(
        history,
        history_id,
        run.run_id,
        DeliveryReceipt::not_dispatched(),
    );
    let _ = app.emit(
        "recording-error",
        RecordingErrorEvent::typed("capture_overrun"),
    );
}

/// Persist VAD-rejected microphone audio without sending it to an ASR engine.
/// A silent recording can still be useful evidence when the user checks their
/// input device, so History retains the verified WAV and an explicit receipt.
fn remove_no_speech_wav(wav_path: &std::path::Path, reason: &str) {
    if let Err(error) = std::fs::remove_file(wav_path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            warn!("Could not remove {reason} no-speech WAV: {error}");
        }
    }
}

async fn rollback_cancelled_no_speech_history(
    history: &HistoryManager,
    history_id: i64,
) -> NoSpeechPersistence {
    match history.delete_entry(history_id).await {
        Ok(()) => NoSpeechPersistence::Cancelled,
        Err(error) => {
            error!("Could not roll back cancelled no-speech history entry: {error}");
            NoSpeechPersistence::SaveFailed
        }
    }
}

async fn persist_no_speech_capture(
    history: &HistoryManager,
    recording: &AudioRecordingManager,
    cancel_generation: u64,
    run: &RunPlan,
    samples: Vec<f32>,
    level: InputLevel,
) -> NoSpeechPersistence {
    let sample_count = samples.len();
    let duration_ms = duration_ms(sample_count);
    let file_name = format!("sona-{}.wav", chrono::Utc::now().timestamp());
    let wav_path = history.recordings_dir().join(&file_name);
    let wav_saved = if samples.is_empty() {
        false
    } else {
        let completed_pcm = share_completed_pcm(samples);
        let wav_path_for_write = wav_path.clone();
        let wav_path_for_verify = wav_path.clone();
        let samples_for_wav = Arc::clone(&completed_pcm);
        match tauri::async_runtime::spawn_blocking(move || {
            crate::audio_toolkit::save_wav_file(&wav_path_for_write, samples_for_wav.as_slice())
        })
        .await
        {
            Ok(Ok(())) => {
                match crate::audio_toolkit::verify_wav_file(&wav_path_for_verify, sample_count) {
                    Ok(()) => true,
                    Err(error) => {
                        error!("No-speech WAV verification failed: {error}");
                        false
                    }
                }
            }
            Ok(Err(error)) => {
                error!("Failed to save no-speech WAV: {error}");
                false
            }
            Err(error) => {
                error!("No-speech WAV task panicked: {error}");
                false
            }
        }
    };

    persist_no_speech_receipt(
        history,
        recording,
        cancel_generation,
        run,
        SavedNoSpeechCapture {
            file_name,
            wav_path,
            duration_ms,
            wav_saved,
            level,
        },
    )
    .await
}

/// One capture that reached a no-speech verdict, with its WAV already written
/// and its amplitude already measured.
struct SavedNoSpeechCapture {
    file_name: String,
    wav_path: std::path::PathBuf,
    duration_ms: Option<u64>,
    wav_saved: bool,
    level: InputLevel,
}

/// Write the no-speech receipt for a capture whose WAV is already on disk.
///
/// Two paths reach a no-speech verdict and both arrive here with a written WAV:
/// a capture too long for the model to arbitrate (written just above), and one
/// the model was handed and returned nothing for, whose WAV the transcribe path
/// already saved. Neither may write a second recording for the same audio.
async fn persist_no_speech_receipt(
    history: &HistoryManager,
    recording: &AudioRecordingManager,
    cancel_generation: u64,
    run: &RunPlan,
    capture: SavedNoSpeechCapture,
) -> NoSpeechPersistence {
    let SavedNoSpeechCapture {
        file_name,
        wav_path,
        duration_ms,
        wav_saved,
        level,
    } = capture;

    // A cancel can arrive while the blocking write or verification is pending.
    // Remove any partial file before reporting the terminal result.
    if recording.was_cancelled_since(cancel_generation) {
        remove_no_speech_wav(&wav_path, "cancelled");
        return NoSpeechPersistence::Cancelled;
    }
    if !wav_saved {
        remove_no_speech_wav(&wav_path, "unverified");
        return NoSpeechPersistence::SaveFailed;
    }

    let Some(history_id) =
        PendingHistoryEntry::no_speech(file_name, run, duration_ms, true, level).save(history)
    else {
        remove_no_speech_wav(&wav_path, "untracked");
        return if recording.was_cancelled_since(cancel_generation) {
            NoSpeechPersistence::Cancelled
        } else {
            NoSpeechPersistence::SaveFailed
        };
    };

    if recording.was_cancelled_since(cancel_generation) {
        return rollback_cancelled_no_speech_history(history, history_id).await;
    }

    persist_delivery_attempt(
        history,
        Some(history_id),
        run.run_id,
        DeliveryReceipt::not_dispatched(),
    );

    if recording.was_cancelled_since(cancel_generation) {
        return rollback_cancelled_no_speech_history(history, history_id).await;
    }

    NoSpeechPersistence::Saved
}

/// Resolve the frozen language intent against the model selected for this run.
fn resolve_effective_language(app: &AppHandle, asr: &AsrPlan) -> String {
    let model_manager = app.state::<Arc<ModelManager>>();
    match model_manager.get_model_info(&asr.model_id) {
        Some(info) => crate::managers::model::effective_language(
            &asr.language,
            &info.supported_languages,
            info.supports_language_detection,
        ),
        None => asr.language.clone(),
    }
}

pub(crate) async fn process_transcription_output(
    app: &AppHandle,
    transcription: &str,
    run: &RunPlan,
) -> ProcessedTranscription {
    let asr = run.asr();
    // Resolve the language from the frozen ASR plan rather than a later
    // settings write.
    let effective_language = resolve_effective_language(app, asr);

    // A command run's transcript is an instruction, not text to clean up: it is
    // never delivered, so no variant conversion or preset rewrite applies to it.
    if let Some(command) = run.command() {
        return crate::command_mode::rewrite_selection(
            app,
            run,
            command,
            transcription,
            &effective_language,
        )
        .await;
    }

    let mut final_text = transcription.to_string();
    let mut post_processed_text: Option<String> = None;

    if let Some(converted_text) =
        maybe_convert_chinese_variant(&effective_language, transcription).await
    {
        final_text = converted_text;
    }

    if run.post_process_requested() {
        let context = run.context();
        // A trailing "Sona, …" sentence is an instruction for the rewrite, not
        // words to type. It leaves the delivery either way: typing the request
        // back is never what was asked for, so a rewrite that cannot happen
        // still delivers the dictation with the cue removed.
        //
        // The instruction replaces this run's preset rewrite rather than adding
        // a second provider call: an explicit request outranks the mode's
        // standing one, and two rewrites would fight over the same text.
        let spoken = if run.prompt().spoken_instructions {
            crate::audio_toolkit::split_spoken_instruction(&final_text)
        } else {
            None
        };
        if let Some(spoken) = &spoken {
            final_text = spoken.text.clone();
            post_processed_text = Some(final_text.clone());
        }
        let rendered = match &spoken {
            Some(spoken) => crate::prompt_renderer::render_instruction(
                crate::prompt_renderer::InstructionRenderInput {
                    instruction: &spoken.instruction,
                    input: &final_text,
                    language: &effective_language,
                    target: context.target(),
                },
            ),
            None => crate::prompt_renderer::render(crate::prompt_renderer::PromptRenderInput {
                run,
                transcript: &final_text,
                language: &effective_language,
                target: context.target(),
                context: context.packet(),
            }),
        };
        debug!(
            "Prompt budget: {} of {} bytes (transcript truncated: {})",
            rendered.budget_receipt.user_bytes,
            rendered.budget_receipt.user_budget_bytes,
            rendered.budget_receipt.transcript_truncated
        );
        // A model cannot keep words it never received. When the dictation
        // does not fit the request, the rewrite would replace all of it with
        // a rewrite of part of it, so the raw words go out whole instead.
        let rewrite = if rendered.budget_receipt.transcript_truncated {
            warn!(
                "Post-processing skipped because the transcript exceeds the prompt budget; \
                 delivering the raw transcript"
            );
            RewriteOutcome::TooLong
        } else {
            match post_process_transcription(app, run, &rendered, &final_text).await {
                Ok(processed_text) => {
                    post_processed_text = Some(processed_text.clone());
                    final_text = processed_text;
                    RewriteOutcome::Applied
                }
                Err(outcome) => outcome,
            }
        };
        return ProcessedTranscription {
            final_text,
            post_processed_text,
            rewrite,
        };
    }
    if final_text != transcription {
        post_processed_text = Some(final_text.clone());
    }

    ProcessedTranscription {
        final_text,
        post_processed_text,
        rewrite: if run.prompt().rewrite_unavailable {
            RewriteOutcome::Unavailable
        } else {
            RewriteOutcome::NotRequested
        },
    }
}

impl TranscribeAction {
    fn start(&self, app: &AppHandle, binding_id: &str, _shortcut_str: &str) {
        let start_time = Instant::now();
        debug!("TranscribeAction::start called for binding: {}", binding_id);

        let tm = app.state::<Arc<TranscriptionManager>>();
        let rm = app.state::<Arc<AudioRecordingManager>>();
        if let Err(error) = cloud_preflight(app, &self.run) {
            emit_cloud_run_error(app, error);
            return;
        }
        if let Some(error_type) = local_model_refusal(&self.run, |model_id| {
            app.state::<Arc<ModelManager>>()
                .recheck_downloaded(model_id)
        }) {
            warn!("Refusing to record: the local transcription model cannot load ({error_type})");
            let _ = app.emit("recording-error", RecordingErrorEvent::typed(error_type));
            return;
        }
        let cloud_requested = self.run.cloud().is_some();

        let binding_id = binding_id.to_string();

        let plan_started = Instant::now();
        let ui_settings = get_settings(app);
        let is_always_on = ui_settings.always_on_microphone;
        let asr = self.run.asr();
        let selected_model_info = self.run.local_asr().and_then(|local_asr| {
            app.state::<Arc<ModelManager>>()
                .get_model_info(&local_asr.model_id)
        });
        let model_supports_streaming = !cloud_requested
            && selected_model_info
                .as_ref()
                .is_some_and(|model| model.supports_streaming);
        let vad_policy = if !asr.vad_enabled {
            VadPolicy::Disabled
        } else if cloud_requested || model_supports_streaming {
            VadPolicy::Streaming
        } else {
            VadPolicy::Offline
        };
        // The local engine is warmed now, not on the first VAD-forwarded speech
        // frame. Deferring it was correct while a VAD-silent capture skipped the
        // model entirely; it no longer does — a silent capture short enough to
        // arbitrate is handed to the model precisely because VAD's verdict is
        // not trusted on quiet speech. So VAD no longer decides *whether* the
        // load happens, only *when*, and waiting moved it to the worst moment:
        // serially inside stop->transcript, where it was measured at 221ms of a
        // 384ms transcription. Starting it here overlaps it with the recording.
        //
        // `initiate_model_load` is non-blocking, is a no-op when this plan's
        // model is already resident, and serializes on the engine's existing
        // load scope, so it neither adds a lock nor fights a pending
        // accelerator reload.
        if let Some(local_asr) = self.run.local_asr() {
            tm.initiate_model_load(local_asr);
        }
        // What still waits for speech is only the work that is wasted outright
        // on a silent capture: a remote session (credentials plus a network
        // round trip) and a live streaming worker.
        #[cfg(feature = "cloud-realtime")]
        if let Some(cloud) = self.run.cloud() {
            tm.arm_cloud_stream_on_first_speech(cloud, cloud_key_source(app, cloud.provider()));
        } else if model_supports_streaming {
            tm.arm_stream_on_first_speech(asr);
        }
        #[cfg(not(feature = "cloud-realtime"))]
        if model_supports_streaming {
            tm.arm_stream_on_first_speech(asr);
        }
        let plan_elapsed = plan_started.elapsed();

        // Sizing the overlay follows the same advertised capability. A model that
        // doesn't stream (or whose capability is not known yet) gets the compact
        // pill instead of an oversized transparent live window.
        let overlay_started = Instant::now();
        match ui_settings.overlay_style {
            OverlayStyle::Live if cloud_requested || model_supports_streaming => {
                utils::show_streaming_overlay(app)
            }
            OverlayStyle::Live | OverlayStyle::Minimal => show_recording_overlay(app),
            OverlayStyle::None => {} // show_overlay_state no-ops on None anyway
        }
        // The VAD session is warmed at startup, so nothing here loads a model
        // synchronously: `settings+stream_plan` covers the plan, the engine
        // warm hand-off, and any first-speech arming.
        debug!(
            "start-path pre-recording steps: settings+stream_plan={:?} overlay={:?}",
            plan_elapsed,
            overlay_started.elapsed()
        );
        debug!("Microphone mode - always_on: {}", is_always_on);
        // Opening the input stream is the slowest step on the start path
        // (~150 ms of cpal stream construction on CoreAudio), and it can also
        // fail outright on a denied permission or a missing device. Asserting
        // the recording tray state before it returns would claim a session that
        // may never exist. The overlay is deliberately not deferred with it: it
        // appears immediately in its arming state, which acknowledges the
        // shortcut without claiming the microphone is listening yet.

        let mut recording_error: Option<String> = None;
        let recording_start_time = Instant::now();
        match rm.try_start_recording(&binding_id, vad_policy) {
            Ok(readiness) => {
                debug!(
                    "Recording request accepted in {:?}; waiting for first microphone samples",
                    recording_start_time.elapsed()
                );
                // The recorder has accepted this session and its state machine
                // is already Recording, so the tray's Stop action is correct
                // from here on.
                set_tray_state(app, TrayIconState::Recording);
                if let Some(warm_up) = rewrite_warm_up(&self.run) {
                    tauri::async_runtime::spawn(warm_up);
                }
                let generation = readiness.generation();
                let app_clone = app.clone();
                let rm_clone = Arc::clone(&rm);
                std::thread::spawn(move || {
                    if !readiness.wait() {
                        debug!("Microphone readiness wait ended without receiving samples");

                        return;
                    }

                    // Development-only preview hook for evaluating the brief
                    // arming animation on hardware that normally starts too fast
                    // to make it visible.
                    #[cfg(debug_assertions)]
                    if let Ok(delay_ms) = std::env::var("SONA_DEBUG_MIC_READY_DELAY_MS")
                        .unwrap_or_default()
                        .parse::<u64>()
                    {
                        let delay_ms = delay_ms.min(10_000);
                        if delay_ms > 0 {
                            debug!("Delaying microphone-ready cue by {delay_ms}ms for UI preview");
                            std::thread::sleep(Duration::from_millis(delay_ms));
                        }
                    }

                    if !rm_clone.is_recording_readiness_current(generation) {
                        debug!("Microphone became ready for an inactive recording");
                        return;
                    }

                    debug!("Microphone is receiving samples; recording is ready");
                    utils::emit_recording_ready(&app_clone);

                    // The start chime is a readiness cue, so it must follow the
                    // first real input callback rather than Stream::play() or a
                    // fixed delay. The helper returns immediately when feedback
                    // is disabled; mute still follows the same readiness point.
                    if rm_clone.is_recording_readiness_current(generation) {
                        play_feedback_sound_blocking(&app_clone, SoundType::Start);
                    }
                    if rm_clone.is_recording_readiness_current(generation) {
                        rm_clone.apply_mute();
                    }
                });
            }
            Err(error) => {
                debug!("Failed to start recording");
                recording_error = Some(error);
            }
        }

        if recording_error.is_none() {
            // Dynamically register the cancel shortcut in a separate task to avoid deadlock
            shortcut::register_cancel_shortcut(app);
        } else {
            // Starting failed (for example due to blocked microphone permissions).
            // Revert UI state so we don't stay stuck in the recording overlay.
            tm.cancel_stream();
            utils::hide_recording_overlay(app);
            set_tray_state(app, TrayIconState::Idle);
            if let Some(err) = recording_error {
                let error_type = if is_microphone_access_denied(&err) {
                    "microphone_permission_denied"
                } else if is_no_input_device_error(&err) {
                    "no_input_device"
                } else {
                    "unknown"
                };
                let _ = app.emit("recording-error", RecordingErrorEvent::typed(error_type));
            }
        }

        debug!(
            "TranscribeAction::start completed in {:?}",
            start_time.elapsed()
        );
    }

    fn stop(&self, app: &AppHandle, binding_id: &str, _shortcut_str: &str) {
        // Prevent a slow microphone from emitting a ready event or start chime
        // after the user has already requested stop.
        app.state::<Arc<AudioRecordingManager>>()
            .invalidate_recording_readiness();

        // Unregister the cancel shortcut when transcription stops
        shortcut::unregister_cancel_shortcut(app);

        let stop_time = Instant::now();
        debug!("TranscribeAction::stop called for binding: {}", binding_id);

        let ah = app.clone();
        let rm = Arc::clone(&app.state::<Arc<AudioRecordingManager>>());
        let tm = Arc::clone(&app.state::<Arc<TranscriptionManager>>());
        let hm = Arc::clone(&app.state::<Arc<HistoryManager>>());

        set_tray_state(app, TrayIconState::Transcribing);
        // Stop should give immediate visual feedback. Live streaming can keep
        // the larger panel, but it still switches from listening to a working
        // spinner while the stream finalizes. Non-streaming paths use the
        // compact transcribing pill (None no-ops in show_*).
        let style = get_settings(app).overlay_style;
        // Capture this before finalizing the stream so every later working state
        // targets the same overlay that was shown for this transcription.
        let use_streaming_overlay =
            should_use_streaming_overlay(style, self.run.cloud().is_some() || tm.is_streaming());
        if use_streaming_overlay {
            tm.emit_stream_working(StreamWorkKind::Transcribing);
        } else {
            show_transcribing_overlay(app);
        }

        // Unmute before playing audio feedback so the stop sound is audible
        rm.remove_mute();

        // Play audio feedback for recording stop
        play_feedback_sound(app, SoundType::Stop);

        let binding_id = binding_id.to_string(); // Clone binding_id for the async task
        let post_process = self.run.post_process_requested();
        let run = self.run.clone();
        // Content-free receipts: identifiers, policy, and source decisions only.
        // This deliberately logs the context *policy* rather than the snapshot:
        // reading the snapshot here would complete the capture at stop, and the
        // application context has to be read immediately before the step that
        // consumes it. The full context receipt reaches the log through history.
        let context_plan = run.context_plan();
        debug!(
            "Run receipt {:?} context policy requested {:?} ceiling {:?} effective {:?}",
            run.mode_receipt(),
            context_plan.requested_policy(),
            context_plan.ceiling(),
            context_plan.effective_policy()
        );
        let cancel_generation = rm.cancel_generation();

        tauri::async_runtime::spawn(async move {
            let _guard = FinishGuard(ah.clone());
            debug!(
                "Starting async transcription task for binding: {}",
                binding_id
            );

            let stop_recording_time = Instant::now();
            match rm.stop_recording(&binding_id, cancel_generation) {
                Some(RecordingStop::NoSpeech { samples, level }) => {
                    if rm.was_cancelled_since(cancel_generation) {
                        debug!("No-speech recording was cancelled before persistence");
                        tm.cancel_stream();
                        utils::hide_recording_overlay(&ah);
                        set_tray_state(&ah, TrayIconState::Idle);
                        return;
                    }
                    tm.cancel_stream();
                    let persistence = persist_no_speech_capture(
                        &hm,
                        &rm,
                        cancel_generation,
                        &run,
                        samples,
                        level,
                    )
                    .await;
                    if let Some(error_type) = persistence.recording_error_type() {
                        let _ = ah.emit("recording-error", RecordingErrorEvent::typed(error_type));
                    }
                    utils::hide_recording_overlay(&ah);
                    set_tray_state(&ah, TrayIconState::Idle);
                }
                Some(RecordingStop::Complete {
                    samples,
                    vad_forwarded_speech,
                    level,
                }) => {
                    debug!(
                        "Recording stopped and samples retrieved in {:?}, sample count: {}, vad_forwarded_speech: {}",
                        stop_recording_time.elapsed(),
                        samples.len(),
                        vad_forwarded_speech
                    );

                    if rm.was_cancelled_since(cancel_generation) {
                        debug!("Transcription operation cancelled after recording stop");
                        tm.cancel_stream();
                        utils::hide_recording_overlay(&ah);
                        set_tray_state(&ah, TrayIconState::Idle);
                        return;
                    }

                    // Only a local decoder can arbitrate a VAD-silent capture:
                    // no speech frame was forwarded, so no cloud session was
                    // ever opened, and reporting a remote failure or a held
                    // cloud run for silence would be a false receipt. Without a
                    // local model there is no cheap arbiter, so VAD's answer
                    // stands exactly as it did before.
                    if !vad_forwarded_speech && run.cloud().is_some() && run.local_asr().is_none() {
                        debug!("VAD-silent capture has no local decoder to arbitrate it");
                        tm.cancel_stream();
                        let persistence = persist_no_speech_capture(
                            &hm,
                            &rm,
                            cancel_generation,
                            &run,
                            samples,
                            level,
                        )
                        .await;
                        if let Some(error_type) = persistence.recording_error_type() {
                            let _ =
                                ah.emit("recording-error", RecordingErrorEvent::typed(error_type));
                        }
                        utils::hide_recording_overlay(&ah);
                        set_tray_state(&ah, TrayIconState::Idle);
                        return;
                    }

                    if samples.is_empty() {
                        debug!("Recording produced no audio samples; skipping persistence");
                        // Tear down any streaming worker so its channel doesn't leak
                        // and block the next start_stream.
                        tm.cancel_stream();
                        utils::hide_recording_overlay(&ah);
                        set_tray_state(&ah, TrayIconState::Idle);
                    } else {
                        // Save WAV concurrently with transcription
                        let completed_pcm = share_completed_pcm(samples);
                        let sample_count = completed_pcm.len();
                        let duration_ms = duration_ms(sample_count);
                        let file_name = format!("sona-{}.wav", chrono::Utc::now().timestamp());
                        let wav_path = hm.recordings_dir().join(&file_name);
                        let wav_path_for_verify = wav_path.clone();
                        let samples_for_wav = Arc::clone(&completed_pcm);
                        let wav_handle = tauri::async_runtime::spawn_blocking(move || {
                            crate::audio_toolkit::save_wav_file(
                                &wav_path,
                                samples_for_wav.as_slice(),
                            )
                        });

                        // Transcribe concurrently with WAV save. If a live stream was
                        // running, finalize it and use its text (all audio was already
                        // fed to the stream); otherwise batch-transcribe the samples.
                        let transcription_time = Instant::now();
                        let transcription_result =
                            transcribe_frozen_run(&tm, &run, completed_pcm.as_slice());

                        // Await WAV save and verify
                        let wav_saved = match wav_handle.await {
                            Ok(Ok(())) => {
                                match crate::audio_toolkit::verify_wav_file(
                                    &wav_path_for_verify,
                                    sample_count,
                                ) {
                                    Ok(()) => true,
                                    Err(e) => {
                                        error!("WAV verification failed: {}", e);
                                        false
                                    }
                                }
                            }
                            Ok(Err(e)) => {
                                error!("Failed to save WAV file: {}", e);
                                false
                            }
                            Err(e) => {
                                error!("WAV save task panicked: {}", e);
                                false
                            }
                        };

                        if rm.was_cancelled_since(cancel_generation) {
                            debug!("Transcription operation cancelled before output handling");
                            utils::hide_recording_overlay(&ah);
                            set_tray_state(&ah, TrayIconState::Idle);
                            return;
                        }

                        match transcription_result {
                            Ok(FrozenTranscript::HeldCloudUnavailable) => {
                                // The complete PCM/WAV is retained, but no provider partial
                                // or local substitute exists to paste or auto-submit.
                                let history_id = PendingHistoryEntry::held_cloud_unavailable(
                                    file_name,
                                    &run,
                                    CompletedCapture {
                                        duration_ms,
                                        has_audio: wav_saved,
                                        level,
                                    },
                                )
                                .save(&hm);
                                persist_delivery_attempt(
                                    &hm,
                                    history_id,
                                    run.run_id,
                                    DeliveryReceipt::not_dispatched(),
                                );
                                // Same lane every other terminal run failure uses,
                                // so one listener covers them all. The recording
                                // and its receipt are already in history.
                                let _ = ah.emit(
                                    "recording-error",
                                    RecordingErrorEvent::typed("cloud_transcription_held"),
                                );
                                utils::hide_recording_overlay(&ah);
                                set_tray_state(&ah, TrayIconState::Idle);
                            }
                            Ok(FrozenTranscript::Final {
                                text: transcription,
                                engine_used,
                                cloud_status,
                                realtime_factor,
                                model_produced_text,
                            }) => {
                                debug!(
                                    "Transcription completed in {:?}",
                                    transcription_time.elapsed()
                                );

                                // VAD rejected every frame of this capture, so
                                // the model was handed the raw clip to settle
                                // it. Only now, with the model having produced
                                // nothing either, is "no speech" a fact rather
                                // than a guess. The WAV above is this capture's
                                // one recording; the receipt reuses it.
                                if capture_verdict(vad_forwarded_speech, model_produced_text)
                                    == CaptureVerdict::NoSpeech
                                {
                                    debug!("Model confirmed the VAD-silent capture had no speech");
                                    let persistence = persist_no_speech_receipt(
                                        &hm,
                                        &rm,
                                        cancel_generation,
                                        &run,
                                        SavedNoSpeechCapture {
                                            file_name,
                                            wav_path: wav_path_for_verify,
                                            duration_ms,
                                            wav_saved,
                                            level,
                                        },
                                    )
                                    .await;
                                    if let Some(error_type) = persistence.recording_error_type() {
                                        let _ = ah.emit(
                                            "recording-error",
                                            RecordingErrorEvent::typed(error_type),
                                        );
                                    }
                                    utils::hide_recording_overlay(&ah);
                                    set_tray_state(&ah, TrayIconState::Idle);
                                    return;
                                }
                                if post_process {
                                    if use_streaming_overlay {
                                        tm.emit_stream_working(StreamWorkKind::Polishing);
                                    } else {
                                        show_processing_overlay(&ah);
                                    }
                                }
                                let Some(processed) = complete_unless_cancelled(
                                    process_transcription_output(&ah, &transcription, &run),
                                    || rm.was_cancelled_since(cancel_generation),
                                )
                                .await
                                else {
                                    debug!(
                                        "Transcription operation cancelled during output handling"
                                    );
                                    utils::hide_recording_overlay(&ah);
                                    set_tray_state(&ah, TrayIconState::Idle);
                                    return;
                                };

                                if rm.was_cancelled_since(cancel_generation) {
                                    debug!("Transcription operation cancelled before paste");
                                    utils::hide_recording_overlay(&ah);
                                    set_tray_state(&ah, TrayIconState::Idle);
                                    return;
                                }

                                let history_entry = PendingHistoryEntry::from_run(
                                    file_name,
                                    transcription,
                                    &processed,
                                    &run,
                                    DecodedRun {
                                        engine_used,
                                        cloud_status,
                                        realtime_factor,
                                    },
                                    CompletedCapture {
                                        duration_ms,
                                        has_audio: wav_saved,
                                        level,
                                    },
                                );

                                if processed.final_text.is_empty() {
                                    let history_id = history_entry.save(&hm);
                                    persist_delivery_attempt(
                                        &hm,
                                        history_id,
                                        run.run_id,
                                        DeliveryReceipt::not_dispatched(),
                                    );
                                    utils::hide_recording_overlay(&ah);
                                    set_tray_state(&ah, TrayIconState::Idle);
                                } else {
                                    // Persist the text and content-free run receipt
                                    // before dispatch. The later delivery outcome is
                                    // an append-only child record.
                                    let history_id = history_entry.save(&hm);
                                    if let Some(event) = RewriteSkippedEvent::for_outcome(
                                        history_id,
                                        processed.rewrite,
                                    ) {
                                        let _ = ah.emit("rewrite-skipped", event);
                                    }
                                    let ah_clone = ah.clone();
                                    let hm_for_main = Arc::clone(&hm);
                                    let hm_for_fallback = Arc::clone(&hm);
                                    let history_for_main = history_id;
                                    let history_for_fallback = history_id;
                                    let run_id = run.run_id;
                                    let paste_time = Instant::now();
                                    let final_text = processed.final_text;
                                    let delivery_settings = run.delivery().clone();
                                    let rm_for_paste = Arc::clone(&rm);
                                    let dispatch = ah.run_on_main_thread(move || {
                                        if rm_for_paste.was_cancelled_since(cancel_generation) {
                                            debug!(
                                                "Transcription operation cancelled before delivery"
                                            );
                                            persist_delivery_attempt(
                                                &hm_for_main,
                                                history_for_main,
                                                run_id,
                                                DeliveryReceipt::not_dispatched(),
                                            );
                                            utils::hide_recording_overlay(&ah_clone);
                                            set_tray_state(&ah_clone, TrayIconState::Idle);
                                            return;
                                        }

                                        let receipt = delivery::deliver(
                                            &ah_clone,
                                            final_text,
                                            &delivery_settings,
                                        );
                                        if receipt.outcome
                                            == DeliveryOutcome::DefinitelyNotDispatched
                                        {
                                            let _ = ah_clone.emit(
                                                "paste-error",
                                                PasteErrorEvent {
                                                    history_id: history_for_main,
                                                },
                                            );
                                        }
                                        persist_delivery_attempt(
                                            &hm_for_main,
                                            history_for_main,
                                            run_id,
                                            receipt,
                                        );
                                        debug!(
                                            "Text delivery completed in {:?}",
                                            paste_time.elapsed()
                                        );
                                        utils::hide_recording_overlay(&ah_clone);
                                        set_tray_state(&ah_clone, TrayIconState::Idle);
                                    });
                                    if let Err(error) = dispatch {
                                        error!("Failed to schedule text delivery on the main thread: {error:?}");
                                        persist_delivery_attempt(
                                            &hm_for_fallback,
                                            history_for_fallback,
                                            run_id,
                                            DeliveryReceipt::not_dispatched(),
                                        );
                                        utils::hide_recording_overlay(&ah);
                                        set_tray_state(&ah, TrayIconState::Idle);
                                    }
                                }
                            }
                            Err(_) => {
                                if rm.was_cancelled_since(cancel_generation) {
                                    debug!(
                                    "Transcription operation cancelled after transcription error"
                                );
                                    utils::hide_recording_overlay(&ah);
                                    set_tray_state(&ah, TrayIconState::Idle);
                                    return;
                                }

                                error!("{TRANSCRIPTION_FAILURE_LOG_MESSAGE}");
                                let _ = ah.emit(
                                    "transcription-error",
                                    TRANSCRIPTION_FAILURE_EVENT_MESSAGE,
                                );
                                let history_id = PendingHistoryEntry::failed(
                                    file_name,
                                    post_process,
                                    &run,
                                    CompletedCapture {
                                        duration_ms,
                                        has_audio: wav_saved,
                                        level,
                                    },
                                )
                                .save(&hm);
                                persist_delivery_attempt(
                                    &hm,
                                    history_id,
                                    run.run_id,
                                    DeliveryReceipt::not_dispatched(),
                                );
                                utils::hide_recording_overlay(&ah);
                                set_tray_state(&ah, TrayIconState::Idle);
                            }
                        }
                    }
                }
                Some(RecordingStop::Overrun {
                    prefix_samples,
                    level,
                }) => {
                    if rm.was_cancelled_since(cancel_generation) {
                        debug!("Capture overrun was cancelled before prefix persistence");
                        tm.cancel_stream();
                        utils::hide_recording_overlay(&ah);
                        set_tray_state(&ah, TrayIconState::Idle);
                        return;
                    }

                    // Cloud partials are preview-only. A capture gap forbids a
                    // final transcript, so discard them before saving the
                    // clean prefix for an explicit history retry.
                    tm.cancel_stream();
                    persist_truncated_capture(&ah, &hm, &run, prefix_samples, level).await;
                    utils::hide_recording_overlay(&ah);
                    set_tray_state(&ah, TrayIconState::Idle);
                }
                None => {
                    debug!("No samples retrieved from recording stop");
                    // Tear down any streaming worker so its channel doesn't leak.
                    tm.cancel_stream();
                    utils::hide_recording_overlay(&ah);
                    set_tray_state(&ah, TrayIconState::Idle);
                }
            }
        });

        debug!(
            "TranscribeAction::stop completed in {:?}",
            stop_time.elapsed()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{
        capture_verdict, complete_unless_cancelled, is_blank_transcription,
        select_final_transcription, share_completed_pcm, should_use_streaming_overlay,
        strip_think_block, BatchDecode, CaptureVerdict, CloudRunError, CompletedCapture,
        InputLevel, PendingHistoryEntry, TRANSCRIPTION_FAILURE_EVENT_MESSAGE,
        TRANSCRIPTION_FAILURE_LOG_MESSAGE,
    };
    #[cfg(feature = "cloud-realtime")]
    use super::{ensure_cloud_fallback_is_installed, resolve_cloud_finalization, FrozenTranscript};
    use crate::settings::OverlayStyle;
    use std::future;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn blank_transcription_is_detected() {
        assert!(is_blank_transcription(""));
        assert!(is_blank_transcription("   "));
        assert!(is_blank_transcription("\t\n  \r\n"));
    }

    #[test]
    fn non_blank_transcription_is_kept() {
        assert!(!is_blank_transcription("hello"));
        assert!(!is_blank_transcription("  hello  "));
    }

    /// The whole point of the change this locks in: a capture VAD called silent
    /// is not silent until the model agrees. Only the bottom row may ever
    /// produce a no-speech receipt.
    #[test]
    fn a_vad_silent_capture_is_only_no_speech_once_the_model_returns_nothing() {
        for (vad_forwarded_speech, model_produced_text, expected) in [
            // VAD found nothing, but the model read the raw clip: real speech.
            (false, true, CaptureVerdict::Transcribed),
            // VAD forwarded speech: the engine already received real audio, so
            // an empty transcript is a failed decode, never a silent capture.
            (true, true, CaptureVerdict::Transcribed),
            (true, false, CaptureVerdict::Transcribed),
            // Nothing heard by either: the only no-speech receipt.
            (false, false, CaptureVerdict::NoSpeech),
        ] {
            assert_eq!(
                capture_verdict(vad_forwarded_speech, model_produced_text),
                expected,
                "vad_forwarded_speech={vad_forwarded_speech} model_produced_text={model_produced_text}"
            );
        }
    }

    /// A capture whose transcript post-processing legitimately emptied — a
    /// spoken "scratch that", or a bare "Um." with filler removal on — is a
    /// delivered-nothing run, not a silent microphone. It reads as
    /// `Transcribed` on both VAD paths, so the two stay consistent: the same
    /// audio cannot earn a different capture status because Silero happened to
    /// fire.
    #[test]
    fn a_transcript_emptied_by_post_processing_is_never_a_no_speech_receipt() {
        let emptied_by_post_processing = true;
        for vad_forwarded_speech in [false, true] {
            assert_eq!(
                capture_verdict(vad_forwarded_speech, emptied_by_post_processing),
                CaptureVerdict::Transcribed,
                "vad_forwarded_speech={vad_forwarded_speech}"
            );
        }
    }

    #[test]
    fn truncated_capture_has_no_transcript_or_post_processing_path() {
        let settings = crate::settings::get_default_settings();
        let run = crate::modes::RunPlan::for_intent(
            &settings,
            &crate::modes::TranscriptionIntent::ActiveMode,
        )
        .expect("default run");
        let entry = PendingHistoryEntry::truncated_capture(
            "prefix.wav".to_string(),
            &run,
            CompletedCapture {
                duration_ms: Some(320),
                has_audio: true,
                level: InputLevel {
                    peak: 0.0714,
                    rms: 0.0153,
                },
            },
        );

        assert!(entry.transcription.is_empty());
        assert!(!entry.post_process_requested);
        assert!(entry.post_processed_text.is_none());
        assert_eq!(entry.word_count, Some(0));
        assert!(entry.has_audio);
        assert_eq!(
            entry.capture_status,
            Some(crate::managers::history::CaptureStatus::Truncated)
        );
        // The prefix is real microphone audio, so it carries the measurement:
        // an absent amplitude on a persisted receipt means no live capture was
        // involved, never "a capture we happened not to measure".
        assert_eq!(entry.run_receipt.input_peak, Some(0.0714));
        assert_eq!(entry.run_receipt.input_rms, Some(0.0153));
    }

    #[test]
    fn no_speech_keeps_audio_but_records_no_engine_or_text() {
        let settings = crate::settings::get_default_settings();
        let run = crate::modes::RunPlan::for_intent(
            &settings,
            &crate::modes::TranscriptionIntent::ActiveMode,
        )
        .expect("default run");
        let entry = PendingHistoryEntry::no_speech(
            "silent.wav".to_string(),
            &run,
            Some(320),
            true,
            InputLevel {
                peak: 0.0119,
                rms: 0.0024,
            },
        );

        assert!(entry.transcription.is_empty());
        assert!(!entry.post_process_requested);
        assert!(entry.post_processed_text.is_none());
        assert_eq!(entry.word_count, Some(0));
        assert!(entry.has_audio);
        assert_eq!(
            entry.capture_status,
            Some(crate::managers::history::CaptureStatus::NoSpeechDetected)
        );
        assert_eq!(entry.run_receipt.engine_used, None);
        assert_eq!(
            entry.run_receipt.cloud_status,
            crate::modes::CloudReceiptStatus::NotRequested
        );
        // The measured amplitude is what separates a dead input stream from a
        // quiet room after the fact, so a no-speech receipt has to carry it.
        assert_eq!(entry.run_receipt.input_peak, Some(0.0119));
        assert_eq!(entry.run_receipt.input_rms, Some(0.0024));
    }

    #[test]
    fn completed_operation_returns_its_output() {
        let result = tauri::async_runtime::block_on(complete_unless_cancelled(
            future::ready("done"),
            || false,
        ));

        assert_eq!(result, Some("done"));
    }

    #[test]
    fn pending_operation_stops_after_cancellation() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancelled_for_thread = Arc::clone(&cancelled);
        let cancel_thread = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            cancelled_for_thread.store(true, Ordering::Release);
        });

        let result = tauri::async_runtime::block_on(complete_unless_cancelled(
            future::pending::<()>(),
            || cancelled.load(Ordering::Acquire),
        ));

        cancel_thread.join().unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn leading_think_block_is_stripped() {
        assert_eq!(
            strip_think_block("<think>pondering...</think>Cleaned text."),
            "Cleaned text."
        );
        assert_eq!(
            strip_think_block("  \n<think>multi\nline</think>\n  Cleaned text."),
            "Cleaned text."
        );
    }

    #[test]
    fn content_without_think_block_is_unchanged() {
        assert_eq!(strip_think_block("Cleaned text."), "Cleaned text.");
        assert_eq!(
            strip_think_block("Mentions <think> mid-sentence."),
            "Mentions <think> mid-sentence."
        );
        // Unclosed block: leave untouched rather than guess
        assert_eq!(
            strip_think_block("<think>never closed"),
            "<think>never closed"
        );
    }

    #[test]
    fn live_overlay_uses_streaming_states_only_for_streaming_models() {
        assert!(should_use_streaming_overlay(OverlayStyle::Live, true));
        assert!(!should_use_streaming_overlay(OverlayStyle::Live, false));
        assert!(!should_use_streaming_overlay(OverlayStyle::Minimal, true));
        assert!(!should_use_streaming_overlay(OverlayStyle::None, true));
    }
    #[test]
    fn preview_degradation_uses_the_complete_batch_pcm() {
        let completed = share_completed_pcm(vec![0.25, -0.5, 0.75]);
        let pcm_ptr = completed.as_slice().as_ptr();

        let result = select_final_transcription(Ok(None), completed.as_slice(), |audio| {
            assert_eq!(audio, [0.25, -0.5, 0.75]);
            assert_eq!(audio.as_ptr(), pcm_ptr);
            Ok(BatchDecode {
                text: "batch result".to_string(),
                realtime_factor: Some(13.8),
                model_produced_text: true,
            })
        });

        let decode = result.expect("batch transcription");
        assert_eq!(decode.text, "batch result");
        assert_eq!(decode.realtime_factor, Some(13.8));
    }

    #[test]
    fn a_streamed_transcript_claims_no_decode_throughput() {
        // The stream produced the text, so no batch decode was timed. The
        // receipt must not inherit a factor from anywhere else.
        let decode = select_final_transcription(
            Ok(Some(BatchDecode {
                text: "streamed text".to_string(),
                realtime_factor: None,
                model_produced_text: true,
            })),
            &[0.25, -0.5],
            |_| panic!("a usable stream final must not batch-decode"),
        )
        .expect("streamed transcription");

        assert_eq!(decode.text, "streamed text");
        assert_eq!(decode.realtime_factor, None);
    }

    /// A stream that decoded speech and then had it filtered away has already
    /// answered the question. Re-decoding the whole clip would spend a full
    /// batch decode to reach the same empty string, so the empty stream final
    /// stands.
    #[test]
    fn an_emptied_stream_final_is_not_re_decoded() {
        let decode = select_final_transcription(
            Ok(Some(BatchDecode {
                text: String::new(),
                realtime_factor: None,
                model_produced_text: true,
            })),
            &[0.25, -0.5],
            |_| panic!("a stream that produced text must not batch-decode again"),
        )
        .expect("streamed transcription");

        assert_eq!(decode.text, "");
        assert!(decode.model_produced_text);
    }

    /// A stream that produced nothing at all is a different case: the batch
    /// decode of the whole clip is the only remaining chance at the transcript.
    #[test]
    fn a_stream_that_produced_nothing_falls_back_to_the_batch_decode() {
        let decode = select_final_transcription(
            Ok(Some(BatchDecode {
                text: String::new(),
                realtime_factor: None,
                model_produced_text: false,
            })),
            &[0.25, -0.5],
            |_| {
                Ok(BatchDecode {
                    text: "batch recovered it".to_string(),
                    realtime_factor: Some(9.0),
                    model_produced_text: true,
                })
            },
        )
        .expect("batch transcription");

        assert_eq!(decode.text, "batch recovered it");
        assert_eq!(decode.realtime_factor, Some(9.0));
    }

    #[test]
    fn completed_pcm_is_shared_by_wav_and_batch_consumers() {
        let captured = vec![0.25, -0.5, 0.75];
        let captured_ptr = captured.as_ptr();
        let completed = share_completed_pcm(captured);
        let wav_pcm = Arc::clone(&completed);

        assert_eq!(completed.as_slice().as_ptr(), captured_ptr);
        assert!(Arc::ptr_eq(&completed, &wav_pcm));
        assert_eq!(wav_pcm.as_slice(), completed.as_slice());
    }

    #[cfg(feature = "cloud-realtime")]
    fn cloud_settings(local_fallback_enabled: bool) -> crate::settings::AppSettings {
        use crate::modes::{CloudSttProvider, RequestedEngine};

        let mut settings = crate::settings::get_default_settings();
        let mode = settings.modes.first_mut().expect("default mode");
        mode.asr.requested_engine = RequestedEngine::DeepgramNova3;
        mode.asr.local_fallback_enabled = local_fallback_enabled;
        mode.asr.local_fallback_model_id =
            local_fallback_enabled.then(|| "fallback-model".to_string());
        let provider = settings
            .cloud_stt_provider_mut(CloudSttProvider::DeepgramNova3)
            .expect("default cloud provider");
        provider.consent_version = crate::settings::CLOUD_STT_CONSENT_VERSION;
        provider.audio_transfer_consent = true;
        provider.privacy_consent = true;
        provider.local_fallback_consent = true;
        settings
    }

    #[cfg(feature = "cloud-realtime")]
    fn cloud_run(local_fallback_enabled: bool) -> crate::modes::RunPlan {
        use crate::modes::{RunPlan, TranscriptionIntent};

        RunPlan::for_intent(
            &cloud_settings(local_fallback_enabled),
            &TranscriptionIntent::ActiveMode,
        )
        .expect("valid cloud run")
    }

    /// A local run whose model cannot load can only fail after the microphone
    /// has opened and the user has already spoken, so it is refused before
    /// capture, and the refusal says whether a model must be chosen or
    /// downloaded.
    #[test]
    fn a_local_run_whose_model_cannot_load_is_refused_before_capture() {
        use super::local_model_refusal;
        use crate::modes::{RunPlan, TranscriptionIntent};

        let refusal = |model_id: &str, is_downloaded: Option<bool>| {
            let mut settings = crate::settings::get_default_settings();
            settings
                .modes
                .first_mut()
                .expect("default mode")
                .asr
                .model_id = model_id.to_string();
            let run = RunPlan::for_intent(&settings, &TranscriptionIntent::ActiveMode)
                .expect("local run");
            local_model_refusal(&run, |_| is_downloaded)
        };

        assert_eq!(
            [
                refusal("", Some(true)),
                refusal("retired-model", None),
                refusal("parakeet-tdt-0.6b-v3", Some(false)),
                refusal("parakeet-tdt-0.6b-v3", Some(true)),
            ],
            [
                Some("no_model_selected"),
                Some("no_model_selected"),
                Some("model_not_downloaded"),
                None,
            ]
        );
    }

    /// The user's text passes are settings, not engine features: a
    /// replacement rule applies to a provider final the same as to a local
    /// decode, and the final is delivered without a local decode.
    #[cfg(feature = "cloud-realtime")]
    #[test]
    fn cloud_final_gets_the_users_passes_without_local_decode() {
        use crate::managers::transcription::CloudStreamFinalization;
        use crate::modes::{CloudReceiptStatus, RequestedEngine, RunPlan, TranscriptionIntent};
        use crate::settings::ReplacementRule;

        let mut settings = cloud_settings(true);
        settings.replacements_enabled = true;
        settings.replacements_rules.push(ReplacementRule {
            spoken: "provider".to_string(),
            written: "Deepgram".to_string(),
            enabled: true,
        });
        let run = RunPlan::for_intent(&settings, &TranscriptionIntent::ActiveMode)
            .expect("valid cloud run");
        let cloud = run.cloud().expect("cloud plan");
        let result = resolve_cloud_finalization(
            &run,
            cloud,
            CloudStreamFinalization::Final("provider final".to_string()),
            &[0.25, -0.5],
            |_, _| panic!("provider final must not decode locally"),
        )
        .expect("cloud final");

        match result {
            FrozenTranscript::Final {
                text,
                engine_used,
                cloud_status,
                realtime_factor,
                model_produced_text,
            } => {
                assert!(model_produced_text);
                assert_eq!(text, "Deepgram final");
                assert_eq!(engine_used, RequestedEngine::DeepgramNova3);
                assert_eq!(cloud_status, CloudReceiptStatus::Final);
                // No local decode ran, so there is no throughput to claim.
                assert_eq!(realtime_factor, None);
            }
            FrozenTranscript::HeldCloudUnavailable => panic!("provider final was held"),
        }
    }

    #[cfg(feature = "cloud-realtime")]
    #[test]
    fn unavailable_key_decodes_full_captured_pcm_once_and_keeps_fallback_transcript() {
        use crate::managers::transcription::{CloudStreamFailure, CloudStreamFinalization};
        use crate::modes::{CloudReceiptStatus, RequestedEngine};
        use std::cell::Cell;

        let run = cloud_run(true);
        let completed = share_completed_pcm(vec![0.25, -0.5, 0.75, -1.0]);
        let pcm_ptr = completed.as_slice().as_ptr();
        let decode_calls = Cell::new(0);
        let result = resolve_cloud_finalization(
            &run,
            run.cloud().expect("cloud plan"),
            CloudStreamFinalization::Failed {
                failure: CloudStreamFailure::KeyUnavailable,
                audio_sent: false,
            },
            completed.as_slice(),
            |fallback, audio| {
                decode_calls.set(decode_calls.get() + 1);
                assert_eq!(fallback.model_id, "fallback-model");
                assert_eq!(audio, [0.25, -0.5, 0.75, -1.0]);
                assert_eq!(audio.as_ptr(), pcm_ptr);
                Ok(BatchDecode {
                    text: "local fallback transcript".to_string(),
                    realtime_factor: Some(12.5),
                    model_produced_text: true,
                })
            },
        )
        .expect("local fallback");

        assert_eq!(decode_calls.get(), 1);
        match result {
            FrozenTranscript::Final {
                text,
                engine_used,
                cloud_status,
                realtime_factor,
                model_produced_text,
            } => {
                assert!(model_produced_text);
                assert_eq!(text, "local fallback transcript");
                assert_eq!(engine_used, RequestedEngine::Local);
                assert_eq!(cloud_status, CloudReceiptStatus::Fallback);
                // The fallback decode is a real local decode, so its measured
                // throughput reaches the receipt.
                assert_eq!(realtime_factor, Some(12.5));
            }
            FrozenTranscript::HeldCloudUnavailable => panic!("fallback was held"),
        }
    }

    #[cfg(feature = "cloud-realtime")]
    #[test]
    fn unavailable_key_without_fallback_is_held_without_delivery_text() {
        use crate::managers::transcription::{CloudStreamFailure, CloudStreamFinalization};
        use std::cell::Cell;

        let run = cloud_run(false);
        let decode_calls = Cell::new(0);
        let result = resolve_cloud_finalization(
            &run,
            run.cloud().expect("cloud plan"),
            CloudStreamFinalization::Failed {
                failure: CloudStreamFailure::KeyUnavailable,
                audio_sent: false,
            },
            &[0.25, -0.5],
            |_, _| {
                decode_calls.set(decode_calls.get() + 1);
                Ok(BatchDecode {
                    text: "must not be delivered".to_string(),
                    realtime_factor: Some(9.0),
                    model_produced_text: true,
                })
            },
        )
        .expect("held cloud result");

        assert_eq!(decode_calls.get(), 0);
        assert!(matches!(result, FrozenTranscript::HeldCloudUnavailable));
    }

    #[cfg(feature = "cloud-realtime")]
    #[test]
    fn cloud_fallback_installation_gate_checks_only_the_frozen_fallback_plan() {
        use std::cell::Cell;

        let with_fallback = cloud_run(true);
        let checked = Cell::new(false);
        assert!(matches!(
            ensure_cloud_fallback_is_installed(&with_fallback, |model_id| {
                checked.set(true);
                model_id == "installed-model"
            }),
            Err(CloudRunError::FallbackModelUnavailable)
        ));
        assert!(checked.get());

        let without_fallback = cloud_run(false);
        assert!(ensure_cloud_fallback_is_installed(&without_fallback, |_| {
            panic!("a disabled fallback must not be queried")
        })
        .is_ok());
    }

    /// The coordinator's preflight rejects only what it can know without
    /// touching the credential store: a missing fallback model, or a provider
    /// whose key was never configured.
    #[cfg(feature = "cloud-realtime")]
    #[test]
    fn cloud_preflight_rejects_only_in_memory_preconditions() {
        use super::check_cloud_preconditions;

        let local = crate::modes::RunPlan::for_intent(
            &crate::settings::get_default_settings(),
            &crate::modes::TranscriptionIntent::ActiveMode,
        )
        .expect("local run");
        assert!(check_cloud_preconditions(
            &local,
            |_| panic!("a local run must not check a cloud fallback"),
            |_| panic!("a local run must not check a provider key"),
        )
        .is_ok());

        let cloud = cloud_run(true);
        assert_eq!(
            check_cloud_preconditions(&cloud, |_| false, |_| true),
            Err(CloudRunError::FallbackModelUnavailable)
        );
        assert_eq!(
            check_cloud_preconditions(&cloud, |_| true, |_| false),
            Err(CloudRunError::NativeKey)
        );
        assert_eq!(
            check_cloud_preconditions(&cloud, |_| true, |_| true),
            Ok(())
        );

        // Without a fallback there is no model to install, so the key state is
        // the only remaining precondition.
        let cloud_without_fallback = cloud_run(false);
        assert_eq!(
            check_cloud_preconditions(
                &cloud_without_fallback,
                |_| panic!("a disabled fallback must not be queried"),
                |_| false,
            ),
            Err(CloudRunError::NativeKey)
        );
    }

    /// Every terminal run failure the user can see rides one event with a
    /// typed code and no content. A regression here either silently drops a
    /// toast (unknown code) or leaks transcript/provider text into the UI.
    #[test]
    fn recording_error_payloads_are_typed_and_content_free() {
        use super::RecordingErrorEvent;

        let held = serde_json::to_value(RecordingErrorEvent::typed("cloud_transcription_held"))
            .expect("held payload");
        assert_eq!(
            held,
            serde_json::json!({ "error_type": "cloud_transcription_held" })
        );

        let overrun =
            serde_json::to_value(RecordingErrorEvent::typed("capture_overrun")).expect("overrun");
        assert_eq!(
            overrun,
            serde_json::json!({ "error_type": "capture_overrun" })
        );

        let no_speech = serde_json::to_value(RecordingErrorEvent::typed("no_speech_detected"))
            .expect("no-speech payload");
        assert_eq!(
            no_speech,
            serde_json::json!({ "error_type": "no_speech_detected" })
        );

        let no_model = serde_json::to_value(RecordingErrorEvent::typed("no_model_selected"))
            .expect("no-model payload");
        assert_eq!(
            no_model,
            serde_json::json!({ "error_type": "no_model_selected" })
        );

        let denied =
            serde_json::to_value(RecordingErrorEvent::typed("microphone_permission_denied"))
                .expect("denied payload");
        assert_eq!(
            denied,
            serde_json::json!({
                "error_type": "microphone_permission_denied",
            })
        );
    }

    #[test]
    fn no_speech_persistence_outcomes_emit_only_truthful_terminal_events() {
        use super::{NoSpeechPersistence, RecordingErrorEvent};

        assert_eq!(NoSpeechPersistence::Cancelled.recording_error_type(), None);
        for (outcome, expected) in [
            (NoSpeechPersistence::SaveFailed, "no_speech_save_failed"),
            (NoSpeechPersistence::Saved, "no_speech_detected"),
        ] {
            let payload = serde_json::to_value(RecordingErrorEvent::typed(
                outcome.recording_error_type().expect("terminal event"),
            ))
            .expect("no-speech payload");
            assert_eq!(payload, serde_json::json!({ "error_type": expected }));
        }
    }

    #[test]
    fn transcript_canary_is_absent_from_transcription_failure_diagnostics() {
        const CANARY: &str = "TRANSCRIPT-CANARY-4EE1";
        let event = serde_json::to_string(&TRANSCRIPTION_FAILURE_EVENT_MESSAGE)
            .expect("transcription failure event serialization");
        let report = serde_json::to_string(&serde_json::json!({
            "log": TRANSCRIPTION_FAILURE_LOG_MESSAGE,
            "event": event,
        }))
        .expect("diagnostic report serialization");

        for diagnostic in [
            TRANSCRIPTION_FAILURE_LOG_MESSAGE,
            TRANSCRIPTION_FAILURE_EVENT_MESSAGE,
            report.as_str(),
        ] {
            assert!(!diagnostic.contains(CANARY), "{diagnostic}");
        }
    }

    #[test]
    fn cloud_run_errors_expose_one_wire_kind_each() {
        use super::RecordingErrorEvent;

        let kinds = [
            #[cfg(feature = "cloud-realtime")]
            (CloudRunError::NativeKey, "native_key"),
            #[cfg(feature = "cloud-realtime")]
            (
                CloudRunError::FallbackModelUnavailable,
                "fallback_model_unavailable",
            ),
            #[cfg(not(feature = "cloud-realtime"))]
            (CloudRunError::FeatureUnavailable, "feature_unavailable"),
        ];

        for (error, expected_kind) in kinds {
            let (kind, message) = error.describe();
            assert_eq!(kind, expected_kind, "wire kind for {error:?}");
            assert!(!message.is_empty(), "log message for {error:?}");

            let payload = serde_json::to_value(RecordingErrorEvent {
                error_type: "cloud_unavailable".to_string(),
                cloud_kind: Some(kind),
                detail: None,
            })
            .expect("cloud payload");
            assert_eq!(
                payload,
                serde_json::json!({
                    "error_type": "cloud_unavailable",
                    "cloud_kind": expected_kind,
                })
            );
        }
    }

    /// What a counting endpoint does with each connection it accepts.
    enum FixtureReply {
        /// Plaintext only. Reached over `https://` the handshake fails, which
        /// reqwest reports as a connect failure: nothing answered. A refused
        /// connect is the same class, and an elapsed request timeout is too —
        /// but a refused port cannot count the attempts made against it, and
        /// the timeout costs twenty seconds a side.
        Plaintext,
        /// One complete HTTP response per connection, the last repeating.
        Http(Vec<String>),
    }

    /// Whether these bytes hold a whole HTTP request: the headers, then as
    /// much body as `Content-Length` promised.
    fn request_is_complete(request: &[u8]) -> bool {
        let Some(head) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
            return false;
        };
        let length: usize = String::from_utf8_lossy(&request[..head])
            .to_lowercase()
            .lines()
            .find_map(|line| {
                line.strip_prefix("content-length:")
                    .map(str::trim)
                    .and_then(|value| value.parse().ok())
            })
            .unwrap_or(0);
        request.len() >= head + 4 + length
    }

    /// The HTTP requests a counting endpoint read, in arrival order.
    type ReadRequests = Arc<std::sync::Mutex<Vec<Vec<u8>>>>;

    /// A loopback server that counts every connection it accepts. Each send
    /// builds its own client, so the count is the number of rewrite requests
    /// that reached the network.
    async fn counting_endpoint(
        reply: FixtureReply,
    ) -> (std::net::SocketAddr, Arc<AtomicUsize>, ReadRequests) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind counting endpoint");
        let address = listener.local_addr().expect("counting endpoint address");
        let connections = Arc::new(AtomicUsize::new(0));
        let served = Arc::clone(&connections);
        let requests = ReadRequests::default();
        let recorded = Arc::clone(&requests);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let index = served.fetch_add(1, Ordering::SeqCst);
                match &reply {
                    FixtureReply::Plaintext => {
                        let mut hello = [0_u8; 512];
                        let _ = stream.read(&mut hello).await;
                        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await;
                    }
                    FixtureReply::Http(responses) => {
                        let Some(response) = responses.get(index).or_else(|| responses.last())
                        else {
                            continue;
                        };
                        // Drain the request first: closing a socket that still
                        // holds unread bytes can cost the client the reply.
                        let mut request = Vec::new();
                        let mut chunk = [0_u8; 1024];
                        while !request_is_complete(&request) {
                            match stream.read(&mut chunk).await {
                                Ok(0) | Err(_) => break,
                                Ok(read) => request.extend_from_slice(&chunk[..read]),
                            }
                        }
                        recorded.lock().expect("read requests").push(request);
                        let _ = stream.write_all(response.as_bytes()).await;
                    }
                }
            }
        });
        (address, connections, requests)
    }

    fn structured_provider(base_url: &str) -> crate::settings::PostProcessProvider {
        crate::settings::PostProcessProvider {
            id: "custom".to_string(),
            label: "Custom".to_string(),
            base_url: base_url.to_string(),
            allow_base_url_edit: true,
            supports_structured_output: true,
        }
    }

    fn http_response(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// The rewrite asks a second time only when a second question can be
    /// answered. An endpoint that answered badly gets the prose retry, and
    /// that retry is how a provider which rejects the schema still rewrites.
    /// An endpoint that never answered gets nothing further: the same silence
    /// costs the same wait again, and a finished dictation is sitting behind
    /// it.
    ///
    /// Counting connections is the only way to see the difference. Both
    /// endpoints here refuse the structured request, and the delivered text
    /// cannot tell one request from two.
    #[tokio::test]
    async fn only_an_endpoint_that_answered_gets_a_second_rewrite_request() {
        use super::request_rewrite;
        use crate::modes::RewriteOutcome;

        let rendered = crate::prompt_renderer::render_instruction(
            crate::prompt_renderer::InstructionRenderInput {
                instruction: "make that a question",
                input: "The plan is ready by Friday.",
                language: "en",
                target: &crate::context::TargetMetadata::default(),
            },
        );

        let (silent, silent_requests, _) = counting_endpoint(FixtureReply::Plaintext).await;
        let provider = structured_provider(&format!("https://{silent}/v1"));
        let endpoint = provider.endpoint().expect("silent endpoint");
        let attempt = request_rewrite(&provider, &endpoint, None, "any", &rendered, false).await;

        assert_eq!(attempt.text, Err(RewriteOutcome::Failed));
        assert_eq!(
            silent_requests.load(Ordering::SeqCst),
            1,
            "an endpoint that never answered is asked once"
        );

        let rewritten = "Is the plan ready by Friday?";
        let completion =
            serde_json::json!({ "choices": [{ "message": { "content": rewritten } }] }).to_string();
        let (answering, answering_requests, _) = counting_endpoint(FixtureReply::Http(vec![
            http_response("500 Internal Server Error", "the schema is not supported"),
            http_response("200 OK", &completion),
        ]))
        .await;
        let provider = structured_provider(&format!("http://{answering}/v1"));
        let endpoint = provider.endpoint().expect("answering endpoint");
        let attempt = request_rewrite(&provider, &endpoint, None, "any", &rendered, false).await;

        assert_eq!(attempt.text, Ok(rewritten.to_string()));
        assert_eq!(
            answering_requests.load(Ordering::SeqCst),
            2,
            "an endpoint that answered is asked the simpler question"
        );
    }

    /// Settings whose active mode rewrites through the keyless custom
    /// provider at `base_url`, the way Aktan's local model is set up.
    fn custom_rewrite_settings(base_url: &str) -> crate::settings::AppSettings {
        let mut settings = crate::settings::get_default_settings();
        crate::modes::ensure_mode_settings(&mut settings);
        settings.post_process_provider_id = "custom".to_string();
        settings
            .post_process_models
            .insert("custom".to_string(), "gemma4:12b-mlx".to_string());
        settings
            .post_process_providers
            .iter_mut()
            .find(|provider| provider.id == "custom")
            .expect("the custom provider is always configured")
            .base_url = base_url.to_string();
        settings.modes[0].llm.enabled = true;
        settings.modes[0].llm.provider_id = None;
        settings
    }

    fn active_run(settings: &crate::settings::AppSettings) -> crate::modes::RunPlan {
        crate::modes::RunPlan::for_intent(settings, &crate::modes::TranscriptionIntent::ActiveMode)
            .expect("active mode resolves")
    }

    /// The body of a chat request a counting endpoint read. The prompt and the
    /// reply budget get fields of their own; everything else the request says
    /// lands in `rest`, so two bodies still compare in full.
    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct ChatRequestBody {
        messages: Vec<ChatRequestMessage>,
        max_tokens: Option<u64>,
        #[serde(flatten)]
        rest: serde_json::Map<String, serde_json::Value>,
    }

    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct ChatRequestMessage {
        content: String,
        #[serde(flatten)]
        rest: serde_json::Map<String, serde_json::Value>,
    }

    fn request_body(request: &[u8]) -> ChatRequestBody {
        let head = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("a complete HTTP request");
        serde_json::from_slice(&request[head + 4..]).expect("a chat request body")
    }

    /// Lifts the prompt out of a chat request, the last message's text, and
    /// drops the reply budget, the one field a warm-up means to ask
    /// differently. What is left is how the request asks.
    fn take_prompt(body: &mut ChatRequestBody) -> String {
        body.max_tokens = None;
        let last = body.messages.last_mut().expect("a last message");
        std::mem::take(&mut last.content)
    }

    /// A warm-up is only worth its request if the rewrite that follows finds
    /// its prompt already read. A local server keeps what it last read and
    /// reuses it only as far as the next request agrees token for token, so
    /// the warm-up must ask the same way, over everything the rewrite sends
    /// ahead of the words. One changed field or separator and the rewrite
    /// reads its instructions again: 2.1 s of the 3 s the warm-up exists to
    /// save on a 12B model.
    #[tokio::test]
    async fn a_warm_up_reads_everything_the_rewrite_sends_before_the_words() {
        use super::{disables_reasoning, request_rewrite, rewrite_warm_up};

        let completion =
            serde_json::json!({ "choices": [{ "message": { "content": "Done." } }] }).to_string();
        let (local, _, requests) = counting_endpoint(FixtureReply::Http(vec![http_response(
            "200 OK",
            &completion,
        )]))
        .await;
        let run = active_run(&custom_rewrite_settings(&format!("http://{local}/v1")));

        rewrite_warm_up(&run)
            .expect("a local rewrite is warmed")
            .await;
        let rendered = crate::prompt_renderer::render(crate::prompt_renderer::PromptRenderInput {
            run: &run,
            transcript: "Draft the update for Morgan.",
            language: "en",
            target: &crate::context::TargetMetadata::default(),
            context: &crate::context::ContextPacket::default(),
        });
        let llm = run.prompt().llm.as_ref().expect("the run rewrites");
        request_rewrite(
            &llm.provider,
            &llm.endpoint,
            None,
            &llm.model_id,
            &rendered,
            disables_reasoning(&llm.provider),
        )
        .await;

        let bodies: Vec<_> = requests
            .lock()
            .expect("read requests")
            .iter()
            .map(|request| request_body(request))
            .collect();
        let [mut warm_up, mut rewrite]: [ChatRequestBody; 2] =
            bodies.try_into().expect("one warm-up, then one rewrite");
        let warm_up_prompt = take_prompt(&mut warm_up);
        assert_eq!(
            take_prompt(&mut rewrite),
            format!("{warm_up_prompt}{}", rendered.user_message),
            "the rewrite's prompt is the warm-up's followed by the words"
        );
        assert_eq!(
            warm_up, rewrite,
            "the warm-up asks the way the rewrite does"
        );
    }

    /// Recording start runs before there is anything to rewrite, so a request
    /// then may go only where the rewrite itself will go, and only to an
    /// endpoint on this Mac that needs no key. Anything else would announce
    /// the dictation to a remote server, or spend a credential, for words
    /// that may never be spoken.
    #[test]
    fn only_a_rewrite_on_this_mac_is_warmed() {
        use super::rewrite_warm_up;

        let local = custom_rewrite_settings("http://127.0.0.1:11434/v1");
        assert!(rewrite_warm_up(&active_run(&local)).is_some());

        let mut cleanup_off = local.clone();
        cleanup_off.modes[0].llm.enabled = false;
        assert!(
            rewrite_warm_up(&active_run(&cleanup_off)).is_none(),
            "a mode that does not rewrite is not warmed"
        );

        let mut remote = custom_rewrite_settings("https://llm.example.test/v1");
        let custom = remote
            .post_process_provider("custom")
            .expect("the custom provider is always configured");
        let consent = crate::settings::PostProcessProviderConsent::for_endpoint(
            &custom.endpoint().expect("a remote endpoint"),
        );
        remote
            .post_process_provider_consents
            .insert("custom".to_string(), consent);
        let run = active_run(&remote);
        assert!(
            run.prompt().llm.is_some(),
            "the remote rewrite itself runs, so only the warm-up declines"
        );
        assert!(
            rewrite_warm_up(&run).is_none(),
            "a remote endpoint hears nothing before the words exist"
        );
    }
}

use crate::audio_toolkit::{
    is_microphone_access_denied, is_no_input_device_error, list_input_devices,
    vad::{
        self, SmoothedVad, VAD_OFFLINE_HANGOVER_FRAMES, VAD_ONSET_FRAMES, VAD_PREFILL_FRAMES,
        VAD_STREAMING_HANGOVER_FRAMES,
    },
    AudioRecorder, CaptureError, CaptureOverrun, RecordedAudio, VadPolicy,
};
use crate::helpers::clamshell;
use crate::managers::transcription::{StreamRouter, TranscriptionManager};
use crate::meeting::detection::input_device::SelfInputDeviceLease;
use crate::meeting::{
    capture::{MeetingCaptureSource, PacketSink},
    types::{
        MeetingCaptureError, SessionClockAnchor, SourceAvailability, SourceEpoch, SourceHealth,
        SourceKind, SourceProbe, SourceProbeDetail, SourceStartPlan, SourceStartReport,
        SourceStopReport,
    },
};
use crate::settings::{get_settings, update_settings, AppSettings};
use crate::utils;
use log::{debug, error, info, trace, warn};
use serde::Serialize;
use specta::Type;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager};
use tauri_specta::Event as _;

/// How long an on-demand stream stays open after a recording stops, when
/// `lazy_stream_close` is on. The stream stays *playing* for this long, which is
/// the whole point — a follow-up dictation inside the window skips the device
/// start entirely — and also the whole cost.
///
/// Measured on an M4 Pro (macOS 26.6, LG UltraFine display-audio input): a
/// playing cpal input stream holds `kAudioDevicePropertyDeviceIsRunningSomewhere`
/// at 1 and keeps the orange menu-bar microphone indicator lit, so this window
/// tells the user their microphone is live for 30 s after they stopped talking.
/// Pausing the stream instead does clear both (verified: property 0, zero orange
/// menu-bar pixels at 3 s, 8 s and 18 s into the pause) but forfeits the
/// benefit: `AudioOutputUnitStart` on a stopped device measured 130-143 ms,
/// against 155-169 ms to build a stream from scratch. The device-running bit is
/// simultaneously the privacy indicator and the warm/cold bit, so a window that
/// hides the indicator is a window that saves nothing but the ~27 ms of
/// AudioUnit construction. That is why this setting is an explicit opt-in
/// rather than the default, and why there is no third "warm but dark" mode.
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// The terminal result of one microphone capture. An overrun keeps only the
/// contiguous prefix before the gap; the action owner saves it for an explicit
/// retry and must never transcribe or deliver it automatically.
///
/// All three outcomes carry the capture's measured `level`, because the receipt
/// the action owner writes has to record what the microphone actually
/// delivered. Absent amplitudes on a persisted receipt therefore mean no live
/// capture was involved at all — an import, a reprocess, a retry off a stored
/// WAV — and never "a capture we happened not to measure".
pub enum RecordingStop {
    /// Audio to decode. `vad_forwarded_speech` is false when VAD rejected every
    /// frame and these are the raw samples handed to the model anyway, so the
    /// action owner knows an empty transcript is the no-speech receipt.
    Complete {
        samples: Vec<f32>,
        vad_forwarded_speech: bool,
        level: InputLevel,
    },
    /// VAD found no speech in a capture too long to re-decode. Its segmentation
    /// is the answer; the samples exist only for the history receipt.
    NoSpeech {
        samples: Vec<f32>,
        level: InputLevel,
    },
    Overrun {
        prefix_samples: Vec<f32>,
        level: InputLevel,
    },
}

/// TEN-VAD's operating point: the lowest threshold that flags nothing on a
/// silence set that includes a 20 s amplified-room-noise bed. Its documented
/// default of 0.50 looked clean on short negatives and fired twice on that bed.
const TEN_VAD_THRESHOLD: f32 = 0.55;

/// Silero's own operating point, unchanged. It is not interchangeable with
/// [`TEN_VAD_THRESHOLD`]: the two engines sit on different precision-recall
/// curves, and opening Silero at 0.55 would ship a strictly more conservative,
/// unmeasured detector on exactly the machines where TEN-VAD's weights failed.
const SILERO_VAD_THRESHOLD: f32 = 0.3;

/// Longest VAD-silent capture Sona still hands to the model before accepting
/// VAD's answer.
///
/// Silero misses quiet speech often enough that its verdict alone is not
/// trustworthy: a real 1.05 s utterance at peak 0.146 / rms 0.011 was rejected
/// as silence, yet decoded to "Test." in 76 ms (parakeet-tdt-0.6b Q8, Metal).
/// Checking costs one short decode, so up to this length the model arbitrates.
/// Past it a decode is no longer free and VAD's segmentation is the better
/// answer anyway, because a long capture gives it many chances to fire.
const VAD_SILENCE_ARBITRATION_MAX_SAMPLES: usize = WHISPER_SAMPLE_RATE * 15;

/// Whether a capture VAD judged silent is short enough for the model to
/// arbitrate. Empty audio has nothing to arbitrate.
fn model_arbitrates_vad_silence(sample_count: usize) -> bool {
    sample_count > 0 && sample_count <= VAD_SILENCE_ARBITRATION_MAX_SAMPLES
}

/// Measured level of one capture's audio, in normalized amplitude.
///
/// This is the evidence that separates the two ways a dictation comes back
/// empty — a dead input stream from a quiet but real utterance — so it is
/// reported for every capture rather than only for suspicious ones.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct InputLevel {
    pub peak: f32,
    pub rms: f32,
}

fn measure_input_level(samples: &[f32]) -> InputLevel {
    if samples.is_empty() {
        return InputLevel::default();
    }
    let mut peak = 0.0f32;
    // f64 accumulation: a 30 s capture is 480k terms, and f32 loses the quiet
    // tail of exactly the signals this measurement exists to characterize.
    let mut sum_squares = 0.0f64;
    for sample in samples {
        peak = peak.max(sample.abs());
        sum_squares += f64::from(*sample) * f64::from(*sample);
    }
    InputLevel {
        peak,
        rms: (sum_squares / samples.len() as f64).sqrt() as f32,
    }
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn set_mute(mute: bool) {
    // Expected behavior:
    // - Windows: works on most systems using standard audio drivers.
    // - Linux: works on many systems (PipeWire, PulseAudio, ALSA),
    //   but some distros may lack the tools used.
    // - macOS: works on most standard setups via AppleScript.
    // If unsupported, fails silently.

    #[cfg(target_os = "windows")]
    {
        // SAFETY: This block uses only COM interfaces returned by Windows calls and a null optional SetMute pointer.
        unsafe {
            use windows::Win32::{
                Media::Audio::{
                    eMultimedia, eRender, Endpoints::IAudioEndpointVolume, IMMDeviceEnumerator,
                    MMDeviceEnumerator,
                },
                System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED},
            };

            macro_rules! unwrap_or_return {
                ($expr:expr) => {
                    match $expr {
                        Ok(val) => val,
                        Err(_) => return,
                    }
                };
            }

            // Initialize the COM library for this thread.
            // If already initialized (e.g., by another library like Tauri), this does nothing.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

            let all_devices: IMMDeviceEnumerator =
                unwrap_or_return!(CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL));
            let default_device =
                unwrap_or_return!(all_devices.GetDefaultAudioEndpoint(eRender, eMultimedia));
            let volume_interface = unwrap_or_return!(
                default_device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
            );

            let _ = volume_interface.SetMute(mute, std::ptr::null());
        }
    }

    #[cfg(target_os = "linux")]
    {
        use std::process::Command;

        let mute_val = if mute { "1" } else { "0" };
        let amixer_state = if mute { "mute" } else { "unmute" };

        // Try multiple backends to increase compatibility
        // 1. PipeWire (wpctl)
        if Command::new("wpctl")
            .args(["set-mute", "@DEFAULT_AUDIO_SINK@", mute_val])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return;
        }

        // 2. PulseAudio (pactl)
        if Command::new("pactl")
            .args(["set-sink-mute", "@DEFAULT_SINK@", mute_val])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return;
        }

        // 3. ALSA (amixer)
        let _ = Command::new("amixer")
            .args(["set", "Master", amixer_state])
            .output();
    }

    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        let script = format!(
            "set volume output muted {}",
            if mute { "true" } else { "false" }
        );
        let _ = Command::new("osascript").args(["-e", &script]).output();
    }
}

/// Reads the current system output mute state, mirroring `set_mute`'s backends.
///
/// Returns `Some(true)`/`Some(false)` when the state could be determined, or
/// `None` when it couldn't (unsupported platform, missing CLI tools, or an
/// error). Callers treat `None` as "unknown" and fall back to unmuting on stop,
/// so we never strand the user's audio muted.
#[cfg(target_os = "windows")]
fn get_mute() -> Option<bool> {
    // SAFETY: This block uses only COM interfaces returned by Windows calls and a null optional SetMute pointer.
    unsafe {
        use windows::Win32::{
            Media::Audio::{
                eMultimedia, eRender, Endpoints::IAudioEndpointVolume, IMMDeviceEnumerator,
                MMDeviceEnumerator,
            },
            System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED},
        };

        // Matches set_mute: no-op if COM is already initialized on this thread.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        let all_devices: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let default_device = all_devices
            .GetDefaultAudioEndpoint(eRender, eMultimedia)
            .ok()?;
        let volume_interface = default_device
            .Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
            .ok()?;

        Some(volume_interface.GetMute().ok()?.as_bool())
    }
}

#[cfg(target_os = "linux")]
fn get_mute() -> Option<bool> {
    use std::process::Command;

    // 1. PipeWire (wpctl): prints "[MUTED]" in the volume line when muted.
    if let Ok(out) = Command::new("wpctl")
        .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
        .output()
    {
        if out.status.success() {
            return Some(String::from_utf8_lossy(&out.stdout).contains("[MUTED]"));
        }
    }

    // 2. PulseAudio (pactl): prints "Mute: yes" / "Mute: no".
    // Force LC_ALL=C so a localized system still emits the parseable English
    // "yes"/"no" instead of e.g. "ja"/"nein".
    if let Ok(out) = Command::new("pactl")
        .env("LC_ALL", "C")
        .args(["get-sink-mute", "@DEFAULT_SINK@"])
        .output()
    {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).to_lowercase();
            if s.contains("yes") {
                return Some(true);
            }
            if s.contains("no") {
                return Some(false);
            }
        }
    }

    // 3. ALSA (amixer): prints "[off]" for muted channels, "[on]" otherwise.
    // LC_ALL=C keeps the "[on]"/"[off]" tokens stable across locales.
    if let Ok(out) = Command::new("amixer")
        .env("LC_ALL", "C")
        .args(["get", "Master"])
        .output()
    {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            if s.contains("[off]") {
                return Some(true);
            }
            if s.contains("[on]") {
                return Some(false);
            }
        }
    }

    None
}

#[cfg(target_os = "macos")]
fn get_mute() -> Option<bool> {
    use std::process::Command;

    let out = Command::new("osascript")
        .args(["-e", "output muted of (get volume settings)"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    match String::from_utf8_lossy(&out.stdout).trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
fn get_mute() -> Option<bool> {
    None
}

/// Restores the system mute state after our forced mute, given the state
/// captured just before we muted. We only ever need to unmute — and only when
/// the system was NOT already muted beforehand. If the prior state was muted,
/// we leave it muted (the user's own state). If it's unknown (`None`), we
/// default to unmuting so audio is never left stranded muted by us.
fn restore_mute(prev_muted: Option<bool>) {
    if prev_muted != Some(true) {
        set_mute(false);
    }
}

const WHISPER_SAMPLE_RATE: usize = 16000;

/* ──────────────────────────────────────────────────────────────── */

#[derive(Clone, Debug)]
pub enum RecordingState {
    Idle,
    Recording { binding_id: String },
    Stopping,
}

/// `is_recording()` flipped, broadcast to every webview. A window that wants to
/// draw dictation's state reads the boolean once on mount and then follows this;
/// it is emitted from `set_state()`, the one place the boolean is written, so
/// the two can never disagree. Start, stop, cancel and every error path reach
/// idle through that function, which is what makes one event enough.
#[derive(Clone, Debug, Serialize, Type, tauri_specta::Event)]
pub struct DictationRecordingChangedEvent {
    pub recording: bool,
}

#[derive(Clone, Debug)]
pub enum MicrophoneMode {
    AlwaysOn,
    OnDemand,
}

/// Tracks our forced "mute while recording" so we can restore the user's audio
/// exactly as it was. `did_mute` is true while our mute is active; `prev_muted`
/// is the system mute state captured just before we muted, used to decide
/// whether to unmute on stop (so a system that was already muted stays muted).
#[derive(Debug, Default, Clone, Copy)]
struct MuteState {
    did_mute: bool,
    prev_muted: Option<bool>,
}

/// The persisted microphone preference currently in effect. Clamshell and
/// regular selections are kept distinct so losing a clamshell-only device does
/// not erase the user's normal microphone preference.
enum DesiredMicrophone {
    Default,
    Selected(String),
    Clamshell(String),
}

/// Result of resolving the persisted preference to a live cpal device.
/// `device: None` means cpal should open the system default. The unavailable
/// name is populated only when enumeration succeeded and confirmed that the
/// user's regular selected microphone is missing.
struct MicrophoneResolution {
    device: Option<cpal::Device>,
    unavailable_selected_microphone: Option<String>,
}

/* ──────────────────────────────────────────────────────────────── */

fn create_audio_recorder(
    ten_vad_path: &Path,
    silero_vad_path: &Path,
    app_handle: &tauri::AppHandle,
    selected_channel: Option<u16>,
    stream_router: Arc<StreamRouter>,
) -> Result<AudioRecorder, anyhow::Error> {
    // A single engine covers both the offline and streaming policies (never
    // active at once within a recording), so the recorder reconfigures its
    // hangover tail per session rather than keeping two ONNX sessions resident.
    let detector = vad::open_detector(
        ten_vad_path,
        TEN_VAD_THRESHOLD,
        silero_vad_path,
        SILERO_VAD_THRESHOLD,
    )
    .map_err(|e| anyhow::anyhow!("Failed to create voice activity detector: {}", e))?;
    let smoothed_vad = SmoothedVad::new(
        detector,
        VAD_PREFILL_FRAMES,
        VAD_OFFLINE_HANGOVER_FRAMES,
        VAD_ONSET_FRAMES,
    );

    // Recorder with VAD, a spectrum-level callback that forwards level updates to
    // the frontend, and an audio-frame callback that feeds live streaming via a
    // shared `StreamRouter` (captured directly, not via Tauri state — see its docs).
    let recorder = AudioRecorder::new()
        .map_err(|e| anyhow::anyhow!("Failed to create AudioRecorder: {}", e))?
        .with_vad(
            Box::new(smoothed_vad),
            VAD_OFFLINE_HANGOVER_FRAMES,
            VAD_STREAMING_HANGOVER_FRAMES,
        )
        .with_selected_channel(selected_channel)
        .with_level_callback({
            let app_handle = app_handle.clone();
            move |levels| {
                utils::emit_levels(&app_handle, &levels);
            }
        })
        .with_audio_callback({
            let router = stream_router;
            move |frame| {
                router.feed(frame);
            }
        });

    Ok(recorder)
}

/* ──────────────────────────────────────────────────────────────── */

/// One recording session's first-sample notification. Waiting on this never
/// blocks the shortcut coordinator: callers hand it to a dedicated worker.
pub struct RecordingReadiness {
    receiver: mpsc::Receiver<()>,
    generation: u64,
}

impl RecordingReadiness {
    pub fn wait(self) -> bool {
        self.receiver.recv().is_ok()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// Tracks exclusive microphone ownership across dictation, meetings, and recorder capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureOwner {
    Dictation,
    Meeting,
    Recorder,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CaptureLeaseToken {
    owner: CaptureOwner,
    generation: u64,
}

struct MicrophoneCaptureLease {
    token: Mutex<Option<CaptureLeaseToken>>,
    next_generation: AtomicU64,
}

impl MicrophoneCaptureLease {
    fn new() -> Self {
        Self {
            token: Mutex::new(None),
            next_generation: AtomicU64::new(0),
        }
    }

    fn try_acquire(&self, owner: CaptureOwner) -> Option<CaptureLeaseToken> {
        let mut held = lock_recover(&self.token);
        if held.is_some() {
            return None;
        }
        let token = CaptureLeaseToken {
            owner,
            generation: self.next_generation.fetch_add(1, Ordering::AcqRel) + 1,
        };
        *held = Some(token);
        Some(token)
    }

    fn owns(&self, token: CaptureLeaseToken) -> bool {
        *lock_recover(&self.token) == Some(token)
    }

    fn release(&self, token: CaptureLeaseToken) -> bool {
        let mut held = lock_recover(&self.token);
        if *held != Some(token) {
            return false;
        }
        *held = None;
        true
    }

    /// Releases whatever token this owner holds. Callers that keep no copy of the token use
    /// this; it is a no-op when someone else owns the microphone.
    fn release_owner(&self, owner: CaptureOwner) -> bool {
        let mut held = lock_recover(&self.token);
        match *held {
            Some(token) if token.owner == owner => {
                *held = None;
                true
            }
            _ => false,
        }
    }

    fn is_active(&self) -> bool {
        lock_recover(&self.token).is_some()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MeetingMicrophonePhase {
    Ready,
    Recording,
    Paused,
    Closed,
}

/// A single, explicitly acquired meeting adapter for the microphone device.
///
/// It owns no meeting lifecycle state. It translates the meeting capture
/// contract into the existing microphone recorder while the audio manager
/// retains device ownership.
pub struct MeetingMicrophoneSource {
    audio: Arc<AudioRecordingManager>,
    lease: CaptureLeaseToken,
    phase: MeetingMicrophonePhase,
    epoch: Option<SourceEpoch>,
}
/// Holds the shared microphone authority while AVFoundation owns recorder input.
///
/// It raises `SelfInputDeviceLease` for the same span. That lease is what meeting detection reads
/// to discount Sona's own microphone use, and AVFoundation raises the device-in-use property just
/// as cpal does, so without it the recorder's own capture looks like a third-party call and
/// prompts the user to take notes on themselves.
pub struct RecorderMicrophoneLease {
    audio: Arc<AudioRecordingManager>,
    token: CaptureLeaseToken,
}

impl Drop for RecorderMicrophoneLease {
    fn drop(&mut self) {
        self.audio.release_recorder_microphone(self.token);
    }
}

#[derive(Clone)]
pub struct AudioRecordingManager {
    /// Never assign through this directly — route every write through
    /// `set_state()`, which keeps `recording_active` in sync.
    state: Arc<Mutex<RecordingState>>,
    mode: Arc<Mutex<MicrophoneMode>>,
    app_handle: tauri::AppHandle,

    recorder: Arc<Mutex<Option<AudioRecorder>>>,
    is_open: Arc<Mutex<bool>>,
    is_recording: Arc<Mutex<bool>>,
    mute_state: Arc<Mutex<MuteState>>,
    close_generation: Arc<AtomicU64>,
    cancel_generation: Arc<AtomicU64>,
    stream_router: Arc<StreamRouter>,
    /// Lock-free mirror of "is the state in {Recording, Stopping}",
    /// maintained by `set_state()`. The hot-path `is_recording()` reads THIS
    /// instead of the std `state` mutex, so a UI poll can no longer deadlock
    /// the main/webview thread when a worker holds `state` across a slow
    /// CoreAudio open/close.
    recording_active: Arc<AtomicBool>,
    /// Invalidates asynchronous first-sample UI/chime work when a recording is
    /// stopped or cancelled. This prevents a slow device from producing a late
    /// "ready" indication for a session the user already ended.
    capture_generation: Arc<AtomicU64>,
    /// Resolution of a *named* microphone (selected or clamshell) to its cpal
    /// device, cached so on-demand recording starts skip the full device
    /// enumeration (~40-110ms). Keyed by the resolved name, so a settings
    /// change misses naturally; cleared when an open fails (device unplugged)
    /// so the retry re-enumerates. The system-default case is never cached —
    /// the recorder resolves the current default itself, cheaply.
    cached_device: Arc<Mutex<Option<(String, cpal::Device)>>>,
    capture_lease: Arc<MicrophoneCaptureLease>,
    /// What meeting detection reads to discount Sona's own microphone. Raised
    /// by `open_holding_lease` and dropped by `close_releasing_lease` alone;
    /// see their docs for why it brackets the device work rather than tracking
    /// `is_open`.
    self_lease: Arc<SelfInputDeviceLease>,
}

impl AudioRecordingManager {
    /* ---------- construction ------------------------------------------------ */

    pub fn new(
        app: &tauri::AppHandle,
        stream_router: Arc<StreamRouter>,
    ) -> Result<Self, anyhow::Error> {
        let settings = get_settings(app);
        let mode = if settings.always_on_microphone {
            MicrophoneMode::AlwaysOn
        } else {
            MicrophoneMode::OnDemand
        };

        let manager = Self {
            state: Arc::new(Mutex::new(RecordingState::Idle)),
            mode: Arc::new(Mutex::new(mode.clone())),
            app_handle: app.clone(),

            recorder: Arc::new(Mutex::new(None)),
            is_open: Arc::new(Mutex::new(false)),
            is_recording: Arc::new(Mutex::new(false)),
            mute_state: Arc::new(Mutex::new(MuteState::default())),
            close_generation: Arc::new(AtomicU64::new(0)),
            cancel_generation: Arc::new(AtomicU64::new(0)),
            stream_router,
            recording_active: Arc::new(AtomicBool::new(false)),
            capture_generation: Arc::new(AtomicU64::new(0)),
            cached_device: Arc::new(Mutex::new(None)),
            capture_lease: Arc::new(MicrophoneCaptureLease::new()),
            self_lease: Arc::new(SelfInputDeviceLease::default()),
        };

        // Always-on?  Open immediately.
        if matches!(mode, MicrophoneMode::AlwaysOn) {
            manager.start_microphone_stream()?;
        }

        Ok(manager)
    }

    /* ---------- helper methods --------------------------------------------- */

    /// The persisted microphone preference currently in effect. Only runs the
    /// clamshell probe (an `ioreg` subprocess, ~10-20ms) when a clamshell
    /// microphone is actually configured.
    fn desired_microphone(&self, settings: &AppSettings) -> DesiredMicrophone {
        if let Some(clamshell_microphone) = &settings.clamshell_microphone {
            let clamshell_started = Instant::now();
            let is_clamshell = clamshell::is_clamshell().unwrap_or(false);
            debug!(
                "device resolve: clamshell_check={:?} (clamshell={})",
                clamshell_started.elapsed(),
                is_clamshell
            );
            if is_clamshell {
                return DesiredMicrophone::Clamshell(clamshell_microphone.clone());
            }
        }
        match &settings.selected_microphone {
            Some(name) => DesiredMicrophone::Selected(name.clone()),
            None => DesiredMicrophone::Default,
        }
    }

    pub fn invalidate_device_cache(&self) {
        *lock_recover(&self.cached_device) = None;
    }

    /// The detection lease this manager raises for its own microphone stream.
    ///
    /// Handed out rather than reached for through the app handle, because the
    /// detection runtime is built after this manager and an always-on stream
    /// is already open by then. A second lease would mean detection watching
    /// a fact nobody writes.
    pub fn self_input_device_lease(&self) -> Arc<SelfInputDeviceLease> {
        Arc::clone(&self.self_lease)
    }

    fn resolve_microphone_device(&self, settings: &AppSettings) -> MicrophoneResolution {
        let desired = self.desired_microphone(settings);
        let (device_name, selected_microphone) = match desired {
            DesiredMicrophone::Default => {
                debug!("device resolve: no mic configured -> system default");
                return MicrophoneResolution {
                    device: None,
                    unavailable_selected_microphone: None,
                };
            }
            DesiredMicrophone::Selected(name) => (name.clone(), Some(name)),
            DesiredMicrophone::Clamshell(name) => (name, None),
        };

        // Cache hit: skip the full enumeration. A stale device (unplugged)
        // fails at open, where the caller invalidates and retries fresh.
        if let Some((cached_name, device)) = lock_recover(&self.cached_device).as_ref() {
            if *cached_name == device_name {
                debug!("device resolve: cache hit for '{}'", device_name);
                return MicrophoneResolution {
                    device: Some(device.clone()),
                    unavailable_selected_microphone: None,
                };
            }
        }

        // Only report a selected microphone as unavailable when enumeration
        // itself succeeded. A backend enumeration error may be transient and
        // must not erase the user's persisted preference.
        let enumerate_started = Instant::now();
        let (device, enumeration_succeeded) = match list_input_devices() {
            Ok(devices) => (
                devices
                    .into_iter()
                    .find(|d| d.name == device_name)
                    .map(|d| d.device),
                true,
            ),
            Err(e) => {
                debug!("Failed to list devices, using default: {}", e);
                (None, false)
            }
        };
        debug!(
            "device resolve: enumerate={:?} (found={})",
            enumerate_started.elapsed(),
            device.is_some()
        );
        if let Some(d) = &device {
            *lock_recover(&self.cached_device) = Some((device_name, d.clone()));
        }

        let unavailable_selected_microphone = if enumeration_succeeded && device.is_none() {
            selected_microphone
        } else {
            None
        };
        MicrophoneResolution {
            device,
            unavailable_selected_microphone,
        }
    }

    /// Keep persisted settings and the UI aligned with a successful runtime
    /// fallback. The update compares and clears one field under the settings
    /// lock, so recovery cannot clear a microphone selected concurrently while
    /// the stream was being rebuilt.
    fn persist_default_microphone_after_fallback(&self, unavailable_name: &str) {
        // A refusing store is logged at the settings seam. The emit below only
        // fires on a write that landed: without one there is no persisted
        // change for the UI to read back.
        let reset = update_settings(&self.app_handle, |settings| {
            if settings.selected_microphone.as_deref() != Some(unavailable_name) {
                return false;
            }
            settings.selected_microphone = None;
            true
        })
        .unwrap_or(false);
        if !reset {
            return;
        }
        let _ = self.app_handle.emit(
            "settings-changed",
            serde_json::json!({
                "setting": "selected_microphone",
                "value": "Default"
            }),
        );
    }

    fn schedule_lazy_close(&self) {
        let gen = self.close_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let app = self.app_handle.clone();
        std::thread::spawn(move || {
            std::thread::sleep(STREAM_IDLE_TIMEOUT);
            let rm = app.state::<Arc<AudioRecordingManager>>();
            // Hold state lock across the check AND close to serialize against
            // try_start_recording, preventing a race where the stream is closed
            // under an active recording.
            let state = lock_recover(&rm.state);
            if rm.close_generation.load(Ordering::SeqCst) == gen
                && matches!(*state, RecordingState::Idle)
                && !rm.capture_lease.is_active()
            {
                // stop_microphone_stream does not acquire the state lock,
                // so holding it here is safe (no deadlock).
                info!(
                    "Closing idle microphone stream after {:?}",
                    STREAM_IDLE_TIMEOUT
                );
                rm.stop_microphone_stream();
            }
        });
    }

    /// Reserve the microphone for a meeting source without opening a stream.
    pub fn try_acquire_meeting_microphone(
        self: &Arc<Self>,
    ) -> Result<MeetingMicrophoneSource, MeetingCaptureError> {
        let state = lock_recover(&self.state);
        if !matches!(*state, RecordingState::Idle) {
            return Err(MeetingCaptureError::InvalidState);
        }
        let Some(lease) = self.capture_lease.try_acquire(CaptureOwner::Meeting) else {
            return Err(MeetingCaptureError::Unavailable);
        };
        drop(state);
        Ok(MeetingMicrophoneSource {
            audio: Arc::clone(self),
            lease,
            phase: MeetingMicrophonePhase::Ready,
            epoch: None,
        })
    }

    /// Reserve the microphone while the native screen recorder owns AVFoundation input.
    pub fn try_acquire_recorder_microphone(self: &Arc<Self>) -> Option<RecorderMicrophoneLease> {
        let state = lock_recover(&self.state);
        if !matches!(*state, RecordingState::Idle) {
            return None;
        }
        let token = self.capture_lease.try_acquire(CaptureOwner::Recorder)?;
        self.self_lease.acquire();
        drop(state);
        Some(RecorderMicrophoneLease {
            audio: Arc::clone(self),
            token,
        })
    }

    pub fn capture_lease_is_active(&self) -> bool {
        self.capture_lease.is_active()
    }

    fn release_meeting_microphone(&self, lease: CaptureLeaseToken) {
        if !self.capture_lease.release(lease) {
            return;
        }
        if matches!(*lock_recover(&self.mode), MicrophoneMode::OnDemand) {
            if get_settings(&self.app_handle).lazy_stream_close {
                self.schedule_lazy_close();
            } else {
                self.stop_microphone_stream();
            }
        }
    }

    fn release_recorder_microphone(&self, token: CaptureLeaseToken) {
        let _ = self.capture_lease.release(token);
        self.self_lease.release();
    }

    /// Hands the microphone back when dictation goes idle. The lease itself remembers who holds
    /// it, so there is no second copy of the token to keep in step.
    fn release_dictation_microphone(&self) {
        let _ = self.capture_lease.release_owner(CaptureOwner::Dictation);
    }

    /* ---------- microphone life-cycle -------------------------------------- */

    /// Applies mute if mute_while_recording is enabled and stream is open.
    /// Snapshots the system's prior mute state first so `remove_mute` can
    /// restore it instead of unconditionally unmuting.
    pub fn apply_mute(&self) {
        let settings = get_settings(&self.app_handle);
        if !settings.mute_while_recording {
            return;
        }

        // Lock order: is_open before mute_state (matches stop_microphone_stream).
        let is_open = lock_recover(&self.is_open);
        let mut mute_guard = lock_recover(&self.mute_state);
        // Already muted this session — don't re-snapshot, or a duplicate/late
        // apply would overwrite prev_muted with our own forced-muted state and
        // strand audio muted on stop.
        if mute_guard.did_mute {
            return;
        }
        if *is_open {
            mute_guard.prev_muted = get_mute();
            set_mute(true);
            mute_guard.did_mute = true;
            debug!("Mute applied (prev_muted={:?})", mute_guard.prev_muted);
        }
    }

    /// Removes mute if it was applied, restoring the system's prior mute state
    /// (a system already muted before recording stays muted).
    pub fn remove_mute(&self) {
        let mut mute_guard = lock_recover(&self.mute_state);
        if mute_guard.did_mute {
            restore_mute(mute_guard.prev_muted);
            mute_guard.did_mute = false;
            debug!(
                "Mute removed (restored prev_muted={:?})",
                mute_guard.prev_muted
            );
        }
    }

    pub fn preload_vad(&self) -> Result<(), anyhow::Error> {
        let mut recorder_opt = lock_recover(&self.recorder);
        if recorder_opt.is_none() {
            let resolve = |name: &str| {
                self.app_handle
                    .path()
                    .resolve(name, tauri::path::BaseDirectory::Resource)
                    .map_err(|e| anyhow::anyhow!("Failed to resolve {name}: {e}"))
            };
            let ten_vad_path = resolve("resources/models/ten-vad.onnx")?;
            let silero_vad_path = resolve("resources/models/silero_vad_v4.onnx")?;
            let settings = get_settings(&self.app_handle);
            *recorder_opt = Some(create_audio_recorder(
                &ten_vad_path,
                &silero_vad_path,
                &self.app_handle,
                settings.selected_channel,
                Arc::clone(&self.stream_router),
            )?);
        }
        Ok(())
    }

    /// Pay the first recording's one-time costs at startup instead of on the
    /// keypress path.
    ///
    /// Three steps sit between a shortcut press and the first captured sample
    /// that are only expensive once: enumerating devices to resolve a named
    /// microphone, loading the Silero VAD session, and querying the device's
    /// supported stream configs. Measured on an M4 Pro from Sona's own
    /// `mic stream breakdown` lines, the first dictation of a session paid
    /// `vad_ensure=32-38ms` and `fetch_config=26-29ms` that every later one
    /// paid in microseconds — a ~60ms penalty falling entirely on the first
    /// dictation, which is exactly the press that feels slowest.
    ///
    /// None of this opens a stream or starts the device, so it does not raise
    /// the OS microphone indicator (see [`AudioRecorder::prewarm_config`] for
    /// how that was verified). Always-on mode has already opened a stream by
    /// the time this runs, so every step below is a cache hit there.
    pub fn prewarm(&self) {
        let vad_elapsed = {
            let _span = crate::launch_trace::span("vad_load");
            let started = Instant::now();
            if let Err(error) = self.preload_vad() {
                debug!("Microphone prewarm: VAD preload failed: {error}");
                return;
            }
            started.elapsed()
        };

        let _span = crate::launch_trace::span("mic_prewarm");
        let settings = get_settings(&self.app_handle);
        let resolve_started = Instant::now();
        let resolution = self.resolve_microphone_device(&settings);
        let resolve_elapsed = resolve_started.elapsed();

        let config_started = Instant::now();
        let config_result = lock_recover(&self.recorder)
            .as_ref()
            .map(|recorder| recorder.prewarm_config(resolution.device));
        match config_result {
            Some(Err(error)) => debug!("Microphone prewarm: config query failed: {error}"),
            Some(Ok(())) | None => {}
        }
        debug!(
            "mic prewarm: vad_load={:?} device_resolve={:?} fetch_config={:?}",
            vad_elapsed,
            resolve_elapsed,
            config_started.elapsed()
        );
    }

    /// Runs the device open with the detection lease already raised, dropping
    /// it again only if the open failed.
    ///
    /// The lease has to be up *before* CoreAudio starts the device, not after
    /// the open reports success. `AudioRecorder::open` calls through to cpal's
    /// `stream.play()`, which raises
    /// `kAudioDevicePropertyDeviceIsRunningSomewhere`; CoreAudio then fires
    /// the property listener on its own thread, and that listener wakes the
    /// detection loop immediately. Raising the lease afterwards loses that
    /// race every time — a cold open measures 155-169 ms (see
    /// [`STREAM_IDLE_TIMEOUT`]) while the woken tick reaches its decision in
    /// about 10 ms — so the loop reads a live microphone that nothing is
    /// holding and prompts about the dictation that is starting.
    ///
    /// A failed open drops the lease, which starts `SELF_MIC_COOLDOWN`: the
    /// device may have partially started before the failure, and the cooldown
    /// is what covers a property that is on with no stream behind it.
    ///
    /// A closure rather than two statements in the right order, because the
    /// ordering is the whole point and a comment asking the next reader to
    /// preserve it is not enforceable. The open cannot run outside the lease.
    fn open_holding_lease<T, E>(
        lease: &SelfInputDeviceLease,
        open: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        lease.acquire();
        let opened = open();
        if opened.is_err() {
            lease.release();
        }
        opened
    }

    /// Runs the device close and drops the detection lease after it returns.
    ///
    /// The mirror of [`Self::open_holding_lease`], and load-bearing for the
    /// same reason read backwards: the device property lags a teardown, so
    /// `SELF_MIC_COOLDOWN` has to be measured from the instant the close
    /// actually completed. Releasing first would start the cooldown early and
    /// leave a window where the property is still on, the stream is still
    /// tearing down, and nothing is holding the lease.
    fn close_releasing_lease(lease: &SelfInputDeviceLease, close: impl FnOnce()) {
        close();
        lease.release();
    }

    pub fn start_microphone_stream(&self) -> Result<(), anyhow::Error> {
        let mut open_flag = lock_recover(&self.is_open);
        if *open_flag {
            // `is_open` only records that we opened a stream at some point, not
            // that one is still running. If capture has since failed (mic
            // unplugged mid-session, USB dropout), rebuild it before the next
            // recording instead of handing the caller a stalled recorder.
            let needs_reopen = lock_recover(&self.recorder)
                .as_ref()
                .is_some_and(|rec| rec.needs_reopen());

            if !needs_reopen {
                // trace, not debug: with the aliveness check in
                // try_start_recording this now fires on every keypress in
                // always-on mode.
                //
                // The detection lease is already held here: an open stream is
                // what raises it, and only a close or a failed open drops it.
                trace!("Microphone stream already active");
                return Ok(());
            }

            warn!("Microphone stream is no longer running (device disconnected?); reopening");

            // Torn down inline rather than via stop_microphone_stream(), which
            // takes the `is_open` lock we are already holding.
            {
                let mut mute_guard = lock_recover(&self.mute_state);
                if mute_guard.did_mute {
                    restore_mute(mute_guard.prev_muted);
                    mute_guard.did_mute = false;
                }
            }
            if let Some(rec) = lock_recover(&self.recorder).as_mut() {
                let _ = rec.close();
            }
            *lock_recover(&self.is_recording) = false;
            // The flag goes down but the lease stays up. The device is Sona's
            // across the whole rebuild, so there is no instant in here where a
            // foreign process could be the explanation for a live microphone.
            // A rebuild that then fails to open releases it below.
            *open_flag = false;
            self.invalidate_device_cache();
            // Fall through to the same fresh resolution and fallback path used
            // when an on-demand stream opens after its device was unplugged.
        }

        let start_time = Instant::now();

        // Don't mute immediately - caller will handle muting after audio feedback.
        // The previous stream restored audio on close, so did_mute should already
        // be false here; if it somehow isn't, restore rather than just clearing the
        // flag, which would strand system audio muted.
        {
            let mut mute_guard = lock_recover(&self.mute_state);
            if mute_guard.did_mute {
                restore_mute(mute_guard.prev_muted);
                mute_guard.did_mute = false;
            }
        }

        // Get the selected device from settings, considering clamshell mode.
        // No pre-flight enumeration here: when nothing is configured the
        // recorder resolves the system default itself, and a machine with no
        // input devices at all fails inside open() with the same
        // "No input device found" error this used to check for.
        let settings = get_settings(&self.app_handle);
        let resolve_started = Instant::now();
        let mut resolution = self.resolve_microphone_device(&settings);
        let resolve_elapsed = resolve_started.elapsed();

        // Ensure VAD is loaded if it wasn't for whatever reason
        let vad_started = Instant::now();
        self.preload_vad()?;
        let vad_elapsed = vad_started.elapsed();

        let open_started = Instant::now();
        Self::open_holding_lease(&self.self_lease, || -> Result<(), anyhow::Error> {
            let mut recorder_opt = lock_recover(&self.recorder);
            let Some(rec) = recorder_opt.as_mut() else {
                return Ok(());
            };
            if let Err(first_err) = rec.open(resolution.device.clone()) {
                // A cached device or config may have gone stale (unplugged,
                // rate/format changed). Re-resolve from a fresh enumeration and
                // retry once before surfacing the error.
                warn!("Recorder open failed ({first_err}); re-resolving device and retrying once");
                self.invalidate_device_cache();
                resolution = self.resolve_microphone_device(&settings);
                rec.open(resolution.device.clone())
                    .map_err(|e| anyhow::anyhow!("Failed to open recorder: {}", e))?;
            }
            Ok(())
        })?;
        debug!(
            "mic stream breakdown: device_resolve={:?} vad_ensure={:?} open={:?}",
            resolve_elapsed,
            vad_elapsed,
            open_started.elapsed()
        );

        // The lease is already up; this only records that a stream now backs it.
        *open_flag = true;
        if let Some(unavailable_name) = resolution.unavailable_selected_microphone {
            // Do this only after the default stream opened successfully. A
            // failed fallback must not erase the user's microphone preference.
            self.persist_default_microphone_after_fallback(&unavailable_name);
        }
        // This timing covers through cpal's stream.play() returning — i.e. the
        // point cpal surfaces as "stream running." It does NOT guarantee the
        // host audio device is producing samples yet; the first input callback
        // fires asynchronously one buffer period later (hardware dependent,
        // typically ~10–200ms on macOS, longer on Bluetooth/USB).
        info!(
            "Microphone stream initialized in {:?}",
            start_time.elapsed()
        );
        Ok(())
    }

    pub fn stop_microphone_stream(&self) {
        if self.capture_lease.is_active() {
            return;
        }
        let mut open_flag = lock_recover(&self.is_open);
        if !*open_flag {
            return;
        }

        {
            let mut mute_guard = lock_recover(&self.mute_state);
            if mute_guard.did_mute {
                restore_mute(mute_guard.prev_muted);
            }
            mute_guard.did_mute = false;
        }

        Self::close_releasing_lease(&self.self_lease, || {
            if let Some(rec) = lock_recover(&self.recorder).as_mut() {
                // If still recording, stop first.
                if *lock_recover(&self.is_recording) {
                    let _ = rec.stop();
                    *lock_recover(&self.is_recording) = false;
                }
                let _ = rec.close();
            }
            *open_flag = false;
        });
        debug!("Microphone stream stopped");
    }

    /* ---------- mode switching --------------------------------------------- */

    pub fn update_mode(&self, new_mode: MicrophoneMode) -> Result<(), anyhow::Error> {
        let cur_mode = lock_recover(&self.mode).clone();

        match (cur_mode, &new_mode) {
            (MicrophoneMode::AlwaysOn, MicrophoneMode::OnDemand) => {
                if matches!(*lock_recover(&self.state), RecordingState::Idle) {
                    self.close_generation.fetch_add(1, Ordering::SeqCst);
                    self.stop_microphone_stream();
                }
            }
            (MicrophoneMode::OnDemand, MicrophoneMode::AlwaysOn) => {
                self.close_generation.fetch_add(1, Ordering::SeqCst);
                self.start_microphone_stream()?;
            }
            _ => {}
        }

        *lock_recover(&self.mode) = new_mode;
        Ok(())
    }

    /* ---------- recording --------------------------------------------------- */

    /// The one place `state` is written. Derives `recording_active` (the
    /// lock-free mirror read by `is_recording()`) from the new value itself,
    /// so the two can never drift: a new `RecordingState` variant only needs
    /// its active-set membership decided here, once. The same branch broadcasts
    /// `DictationRecordingChangedEvent`, so no window has to poll for the flip.
    ///
    /// It used to raise the detection lease as well. It no longer does, and
    /// must not: the recording state is not when Sona holds the input device,
    /// only when it is consuming what the device delivers. The pair around the
    /// device open and close owns that.
    ///
    /// It does hand the microphone back. Returning to `Idle` releases dictation's capture lease
    /// here rather than at each stop site, because every path to idle goes through this function
    /// — normal stop, cancel, and the error paths that reset state — and one of them forgetting
    /// would strand the lease and lock out meetings and the recorder for the life of the process.
    /// The ordering that matters: the state write lands first, then the release, so nothing can
    /// observe a free microphone while this manager still reports itself as recording.
    fn set_state(&self, guard: &mut RecordingState, new_state: RecordingState) {
        let release_dictation =
            matches!(new_state, RecordingState::Idle) && !matches!(*guard, RecordingState::Idle);
        *guard = new_state;
        let active = matches!(
            *guard,
            RecordingState::Recording { .. } | RecordingState::Stopping
        );
        if self.recording_active.swap(active, Ordering::SeqCst) != active {
            if let Some(manager) = self.app_handle.try_state::<Arc<TranscriptionManager>>() {
                manager.signal_idle_watcher();
            }
            if let Err(error) =
                (DictationRecordingChangedEvent { recording: active }).emit(&self.app_handle)
            {
                warn!("Failed to emit the dictation recording state: {error}");
            }
        }
        if release_dictation {
            self.release_dictation_microphone();
        }
    }

    pub fn try_start_recording(
        &self,
        binding_id: &str,
        vad_policy: VadPolicy,
    ) -> Result<RecordingReadiness, String> {
        let mut state = lock_recover(&self.state);
        if !matches!(*state, RecordingState::Idle) {
            return Err("Already recording".to_string());
        }
        let Some(token) = self.capture_lease.try_acquire(CaptureOwner::Dictation) else {
            return Err("Microphone is leased by an active capture".to_string());
        };

        self.close_generation.fetch_add(1, Ordering::SeqCst);
        if let Err(error) = self.start_microphone_stream() {
            let message = error.to_string();
            self.capture_lease.release(token);
            error!("Failed to open microphone stream: {message}");
            return Err(message);
        }

        let result = lock_recover(&self.recorder)
            .as_ref()
            .ok_or_else(|| "Recorder not available".to_string())
            .and_then(|recorder| {
                recorder
                    .start(vad_policy)
                    .map_err(|error| format!("Failed to start recorder: {error}"))
            });
        let receiver = match result {
            Ok(receiver) => receiver,
            Err(error) => {
                self.capture_lease.release(token);
                return Err(error);
            }
        };

        let generation = self.capture_generation.fetch_add(1, Ordering::AcqRel) + 1;
        *lock_recover(&self.is_recording) = true;
        self.set_state(
            &mut state,
            RecordingState::Recording {
                binding_id: binding_id.to_string(),
            },
        );
        debug!("Recording requested for binding {binding_id}");
        Ok(RecordingReadiness {
            receiver,
            generation,
        })
    }

    pub fn update_selected_device(&self) -> Result<(), anyhow::Error> {
        if self.capture_lease.is_active() {
            return Err(anyhow::anyhow!(
                "Cannot change the microphone while another capture owns it"
            ));
        }
        // Device settings changed; re-enumerate the device and restart capture.
        self.invalidate_device_cache();
        let was_open = *lock_recover(&self.is_open);
        if was_open {
            self.close_generation.fetch_add(1, Ordering::SeqCst);
            self.stop_microphone_stream();
            self.start_microphone_stream()?;
        }
        Ok(())
    }

    pub fn update_selected_channel(
        &self,
        selected_channel: Option<u16>,
    ) -> Result<(), anyhow::Error> {
        if self.capture_lease.is_active() {
            return Err(anyhow::anyhow!(
                "Cannot change the input channel while another capture owns it"
            ));
        }
        // Serialize against recording start/stop. Restarting an active capture
        // would discard its samples and leave the manager's recording state out
        // of sync with the new recorder.
        let state = lock_recover(&self.state);
        if !matches!(*state, RecordingState::Idle) {
            return Err(anyhow::anyhow!(
                "Cannot change the input channel while recording"
            ));
        }

        let previous_channel = get_settings(&self.app_handle).selected_channel;
        let was_open = *lock_recover(&self.is_open);
        if was_open {
            self.close_generation.fetch_add(1, Ordering::SeqCst);
            self.stop_microphone_stream();
        }
        if let Some(recorder) = lock_recover(&self.recorder).as_mut() {
            recorder.set_selected_channel(selected_channel);
        }
        if was_open {
            if let Err(error) = self.start_microphone_stream() {
                if let Some(recorder) = lock_recover(&self.recorder).as_mut() {
                    recorder.set_selected_channel(previous_channel);
                }
                return Err(error);
            }
        }
        drop(state);
        Ok(())
    }

    /// Invalidate pending first-sample UI and audio-feedback work immediately.
    /// Called at the beginning of stop, before the slower capture drain starts.
    pub fn invalidate_recording_readiness(&self) {
        self.capture_generation.fetch_add(1, Ordering::AcqRel);
    }

    pub fn is_recording_readiness_current(&self, generation: u64) -> bool {
        self.capture_generation.load(Ordering::Acquire) == generation
    }

    pub fn cancel_generation(&self) -> u64 {
        self.cancel_generation.load(Ordering::Acquire)
    }

    pub fn was_cancelled_since(&self, generation: u64) -> bool {
        self.cancel_generation.load(Ordering::Acquire) != generation
    }

    pub fn stop_recording(
        &self,
        binding_id: &str,
        cancel_generation: u64,
    ) -> Option<RecordingStop> {
        self.invalidate_recording_readiness();
        let mut state = lock_recover(&self.state);

        match *state {
            RecordingState::Recording {
                binding_id: ref active,
            } if active == binding_id => {
                self.set_state(&mut state, RecordingState::Stopping);
                drop(state);

                // Optionally keep recording for a bit longer to capture trailing audio.
                // This is only the explicit user setting; streaming VAD must not add
                // hidden post-release capture time.
                let settings = get_settings(&self.app_handle);
                let buffer_ms = settings.extra_recording_buffer_ms;
                if buffer_ms > 0 {
                    debug!(
                        "Extra recording buffer: sleeping {}ms before stopping",
                        buffer_ms
                    );
                    let started = Instant::now();
                    let buffer = Duration::from_millis(buffer_ms);
                    while started.elapsed() < buffer {
                        if self.was_cancelled_since(cancel_generation) {
                            debug!("Recording stop cancelled during extra buffer");
                            break;
                        }
                        let remaining = buffer.saturating_sub(started.elapsed());
                        std::thread::sleep(remaining.min(Duration::from_millis(25)));
                    }
                }

                let capture = if let Some(rec) = lock_recover(&self.recorder).as_ref() {
                    rec.stop()
                } else {
                    error!("Recorder not available");
                    Err(CaptureError::NotCapturing)
                };

                *lock_recover(&self.is_recording) = false;
                self.set_state(&mut lock_recover(&self.state), RecordingState::Idle);

                // In on-demand mode, close the mic (lazily if the setting is enabled)
                if matches!(*lock_recover(&self.mode), MicrophoneMode::OnDemand) {
                    if get_settings(&self.app_handle).lazy_stream_close {
                        self.schedule_lazy_close();
                    } else {
                        self.stop_microphone_stream();
                    }
                }

                if self.was_cancelled_since(cancel_generation) {
                    debug!("Recording stop cancelled; discarding captured samples");
                    return None;
                }

                let capture = match capture {
                    Ok(capture) => capture,
                    Err(CaptureError::Overrun {
                        overrun,
                        prefix_samples,
                    }) => {
                        self.log_capture_overrun(overrun);
                        // The prefix is real microphone audio, so it gets the
                        // same measurement the other two outcomes carry: on a
                        // truncated row, "was the retained prefix even audible"
                        // is exactly the question that decides a retry.
                        let level = measure_input_level(&prefix_samples);
                        return Some(RecordingStop::Overrun {
                            prefix_samples,
                            level,
                        });
                    }
                    Err(error) => {
                        error!("stop() failed: {error}");
                        return None;
                    }
                };

                let RecordedAudio {
                    samples,
                    vad_forwarded_speech,
                } = capture;
                let level = measure_input_level(&samples);
                debug!(
                    "capture level peak={:.4} rms={:.4} over {} samples; vad_forwarded_speech={}",
                    level.peak,
                    level.rms,
                    samples.len(),
                    vad_forwarded_speech
                );

                // VAD is an optimizer: it keeps silence off the engine, but only
                // the model may conclude the user did not speak. Its verdict
                // stands alone only where re-decoding is no longer cheap.
                if !vad_forwarded_speech && !model_arbitrates_vad_silence(samples.len()) {
                    return Some(RecordingStop::NoSpeech { samples, level });
                }

                // Pad a normal, very short recording for model input. An
                // overrun prefix stays exact and takes the branch above. The
                // level above describes the unpadded capture, which is the only
                // honest reading: padding is zeros the microphone never sent.
                let s_len = samples.len();
                let samples = if s_len < WHISPER_SAMPLE_RATE && s_len > 0 {
                    let mut padded = samples;
                    padded.resize(WHISPER_SAMPLE_RATE * 5 / 4, 0.0);
                    padded
                } else {
                    samples
                };
                Some(RecordingStop::Complete {
                    samples,
                    vad_forwarded_speech,
                    level,
                })
            }
            _ => None,
        }
    }

    /// Log capture diagnostics only. The action owner emits the content-free
    /// event after it has persisted the prefix and receipt.
    fn log_capture_overrun(&self, overrun: CaptureOverrun) {
        error!("Audio capture overran; retaining the contiguous prefix: {overrun}");
    }

    pub fn is_recording(&self) -> bool {
        // Lock-free: mirrors the `state` {Recording, Stopping} membership via
        // an atomic maintained by `set_state()`. Polled from the webview/main
        // thread, so it MUST NOT take the `state` mutex (a worker can hold it
        // across a slow CoreAudio open/close → main-thread deadlock / UI
        // freeze).
        self.recording_active.load(Ordering::SeqCst)
    }

    /// Cancel any ongoing recording without returning audio samples
    pub fn cancel_recording(&self) {
        self.invalidate_recording_readiness();
        self.cancel_generation.fetch_add(1, Ordering::AcqRel);
        let mut state = lock_recover(&self.state);

        match *state {
            RecordingState::Recording { .. } => {
                self.set_state(&mut state, RecordingState::Idle);
                drop(state);

                if let Some(rec) = lock_recover(&self.recorder).as_ref() {
                    let _ = rec.stop(); // Discard the result
                }

                *lock_recover(&self.is_recording) = false;

                // In on-demand mode, close the mic (lazily if the setting is enabled)
                if matches!(*lock_recover(&self.mode), MicrophoneMode::OnDemand) {
                    if get_settings(&self.app_handle).lazy_stream_close {
                        self.schedule_lazy_close();
                    } else {
                        self.stop_microphone_stream();
                    }
                }
            }
            RecordingState::Stopping => {
                debug!("Cancellation requested while recording is stopping");
            }
            RecordingState::Idle => {}
        }
    }
}

impl MeetingMicrophoneSource {
    fn ensure_lease(&self) -> Result<(), MeetingCaptureError> {
        if self.audio.capture_lease.owns(self.lease) {
            Ok(())
        } else {
            Err(MeetingCaptureError::InvalidState)
        }
    }

    fn finish(&mut self) {
        if self.phase == MeetingMicrophonePhase::Closed {
            return;
        }
        self.audio.release_meeting_microphone(self.lease);
        self.phase = MeetingMicrophonePhase::Closed;
    }

    fn start_error(error: anyhow::Error) -> MeetingCaptureError {
        let message = error.to_string();
        if is_microphone_access_denied(&message) {
            MeetingCaptureError::PermissionDenied
        } else if is_no_input_device_error(&message) {
            MeetingCaptureError::Unavailable
        } else {
            MeetingCaptureError::StreamFailure
        }
    }

    fn abort_inner(&mut self) -> Result<(), MeetingCaptureError> {
        let result = match self.phase {
            MeetingMicrophonePhase::Recording | MeetingMicrophonePhase::Paused => {
                lock_recover(&self.audio.recorder)
                    .as_ref()
                    .ok_or(MeetingCaptureError::Unavailable)
                    .and_then(AudioRecorder::abort_meeting_capture)
            }
            MeetingMicrophonePhase::Ready | MeetingMicrophonePhase::Closed => Ok(()),
        };
        self.finish();
        result
    }
}

impl MeetingCaptureSource for MeetingMicrophoneSource {
    fn probe(&self) -> SourceProbe {
        #[cfg(target_os = "macos")]
        {
            let availability = match list_input_devices() {
                Ok(devices) if devices.is_empty() => SourceAvailability::DeviceUnavailable,
                Ok(_) => SourceAvailability::Available,
                Err(_) => SourceAvailability::Unknown,
            };
            SourceProbe {
                source_kind: SourceKind::Microphone,
                availability,
                health: if availability == SourceAvailability::Available {
                    SourceHealth::NotStarted
                } else {
                    SourceHealth::Failed
                },
                detail: (availability != SourceAvailability::Available)
                    .then_some(SourceProbeDetail::Device),
                negotiated_format: None,
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            SourceProbe {
                source_kind: SourceKind::Microphone,
                availability: SourceAvailability::UnsupportedPlatform,
                health: SourceHealth::NotStarted,
                detail: Some(SourceProbeDetail::Platform),
                negotiated_format: None,
            }
        }
    }

    fn start(
        &mut self,
        plan: SourceStartPlan,
        anchor: SessionClockAnchor,
        sink: PacketSink,
    ) -> Result<SourceStartReport, MeetingCaptureError> {
        if self.phase != MeetingMicrophonePhase::Ready {
            return Err(MeetingCaptureError::InvalidState);
        }
        self.ensure_lease()?;
        if plan.source_kind != SourceKind::Microphone {
            return Err(MeetingCaptureError::InvalidFormat);
        }
        if let Err(error) = self.audio.start_microphone_stream() {
            self.finish();
            return Err(Self::start_error(error));
        }

        self.phase = MeetingMicrophonePhase::Recording;
        let result = lock_recover(&self.audio.recorder)
            .as_ref()
            .ok_or(MeetingCaptureError::Unavailable)
            .and_then(|recorder| recorder.start_meeting_capture(plan, anchor, sink));
        match result {
            Ok(report) => {
                self.epoch = Some(report.epoch);
                Ok(report)
            }
            Err(error) => {
                let _ = self.abort_inner();
                Err(error)
            }
        }
    }

    fn pause(&mut self) -> Result<(), MeetingCaptureError> {
        if self.phase != MeetingMicrophonePhase::Recording {
            return Err(MeetingCaptureError::InvalidState);
        }
        self.ensure_lease()?;
        let result = lock_recover(&self.audio.recorder)
            .as_ref()
            .ok_or(MeetingCaptureError::Unavailable)
            .and_then(AudioRecorder::pause_meeting_capture);
        match result {
            Ok(()) => {
                self.phase = MeetingMicrophonePhase::Paused;
                Ok(())
            }
            Err(error) => {
                let _ = self.abort_inner();
                Err(error)
            }
        }
    }

    fn resume(&mut self, epoch: SourceEpoch) -> Result<SourceStartReport, MeetingCaptureError> {
        if self.phase != MeetingMicrophonePhase::Paused {
            return Err(MeetingCaptureError::InvalidState);
        }
        self.ensure_lease()?;
        let current_epoch = self.epoch.ok_or(MeetingCaptureError::InvalidState)?;
        let resumed_epoch = if epoch.get() > current_epoch.get() {
            epoch
        } else {
            SourceEpoch::new(
                current_epoch
                    .get()
                    .checked_add(1)
                    .ok_or(MeetingCaptureError::InvalidState)?,
            )
        };
        let result = lock_recover(&self.audio.recorder)
            .as_ref()
            .ok_or(MeetingCaptureError::Unavailable)
            .and_then(|recorder| recorder.resume_meeting_capture(resumed_epoch));
        match result {
            Ok(report) => {
                self.epoch = Some(report.epoch);
                self.phase = MeetingMicrophonePhase::Recording;
                Ok(report)
            }
            Err(error) => {
                let _ = self.abort_inner();
                Err(error)
            }
        }
    }

    fn stop(&mut self) -> Result<SourceStopReport, MeetingCaptureError> {
        if !matches!(
            self.phase,
            MeetingMicrophonePhase::Recording | MeetingMicrophonePhase::Paused
        ) {
            return Err(MeetingCaptureError::InvalidState);
        }
        self.ensure_lease()?;
        let result = lock_recover(&self.audio.recorder)
            .as_ref()
            .ok_or(MeetingCaptureError::Unavailable)
            .and_then(AudioRecorder::stop_meeting_capture);
        if result.is_err() {
            let _ = lock_recover(&self.audio.recorder)
                .as_ref()
                .ok_or(MeetingCaptureError::Unavailable)
                .and_then(AudioRecorder::abort_meeting_capture);
        }
        self.finish();
        result
    }

    fn abort(&mut self) -> Result<(), MeetingCaptureError> {
        self.abort_inner()
    }
}

impl Drop for MeetingMicrophoneSource {
    fn drop(&mut self) {
        let _ = self.abort_inner();
    }
}

#[cfg(test)]
mod microphone_capture_lease_tests {
    use super::{CaptureOwner, MicrophoneCaptureLease};

    #[test]
    fn capture_lease_excludes_meeting_recorder_and_dictation() {
        let lease = MicrophoneCaptureLease::new();
        let meeting = lease
            .try_acquire(CaptureOwner::Meeting)
            .expect("meeting acquires the microphone");

        assert!(lease.owns(meeting));
        assert!(lease.try_acquire(CaptureOwner::Recorder).is_none());
        assert!(lease.try_acquire(CaptureOwner::Dictation).is_none());
        assert!(lease.release(meeting));

        let next_meeting = lease
            .try_acquire(CaptureOwner::Meeting)
            .expect("meeting reacquires after release");
        assert!(!lease.release(meeting));
        assert!(lease.owns(next_meeting));
        assert!(lease.release(next_meeting));

        let recorder = lease
            .try_acquire(CaptureOwner::Recorder)
            .expect("recorder acquires after meeting releases");
        assert!(lease.release(recorder));

        let dictation = lease
            .try_acquire(CaptureOwner::Dictation)
            .expect("dictation acquires after recorder releases");
        assert!(lease.release(dictation));
    }

    /// Dictation keeps no copy of its token: `set_state` hands the microphone back by owner when
    /// the manager returns to idle. If that lookup ever released someone else's lease, a meeting
    /// or a screen recording would lose the microphone under it mid-capture.
    #[test]
    fn release_by_owner_frees_only_that_owner() {
        let lease = MicrophoneCaptureLease::new();
        let meeting = lease
            .try_acquire(CaptureOwner::Meeting)
            .expect("meeting acquires the microphone");

        assert!(!lease.release_owner(CaptureOwner::Dictation));
        assert!(!lease.release_owner(CaptureOwner::Recorder));
        assert!(lease.owns(meeting));
        assert!(lease.release_owner(CaptureOwner::Meeting));
        assert!(!lease.is_active());
        assert!(!lease.release_owner(CaptureOwner::Meeting));
    }
}

#[cfg(test)]
mod stream_lease_tests {
    use super::AudioRecordingManager;
    use crate::meeting::detection::input_device::{SelfInputDeviceLease, SELF_MIC_COOLDOWN};
    use std::cell::Cell;

    /// The race measured on the real machine: with the lease raised after the
    /// open returned, a dictation start put a "microphone activity detected"
    /// panel on screen within two seconds.
    ///
    /// CoreAudio raises the device-in-use property inside the open and fires
    /// its listener on its own thread, which wakes the detection loop while
    /// the open is still running. The closure here stands in for that
    /// listener, and records what the woken tick would read. Moving the
    /// acquire back after the open fails this test.
    #[test]
    fn the_lease_is_already_held_when_the_device_edge_fires_inside_the_open() {
        let lease = SelfInputDeviceLease::default();
        let seen_by_the_woken_tick = Cell::new(false);

        let opened: Result<(), ()> = AudioRecordingManager::open_holding_lease(&lease, || {
            seen_by_the_woken_tick.set(lease.is_held());
            Ok(())
        });

        assert!(opened.is_ok());
        assert!(
            seen_by_the_woken_tick.get(),
            "a tick woken by the device edge mid-open must see Sona holding it"
        );
        assert!(
            lease.is_held(),
            "the open succeeded, so the stream keeps holding it"
        );
    }

    /// The device may have partially started before the failure, so the
    /// cooldown has to cover a property that is on with no stream behind it.
    #[test]
    fn a_failed_open_releases_the_lease_and_starts_the_cooldown() {
        let lease = SelfInputDeviceLease::default();

        let opened: Result<(), &str> =
            AudioRecordingManager::open_holding_lease(&lease, || Err("no input device found"));

        assert!(opened.is_err());
        assert!(!lease.is_held());
        assert!(lease.released_within(SELF_MIC_COOLDOWN));
    }

    /// The mirror of the open race. The property lags the teardown, so a tick
    /// woken during the close must still read the device as Sona's, and the
    /// cooldown must start from the instant the close completed. Releasing
    /// before the close fails this test.
    #[test]
    fn the_lease_is_still_held_while_the_close_runs_and_dropped_after_it() {
        let lease = SelfInputDeviceLease::default();
        lease.acquire();
        let seen_by_the_woken_tick = Cell::new(false);

        AudioRecordingManager::close_releasing_lease(&lease, || {
            seen_by_the_woken_tick.set(lease.is_held());
        });

        assert!(
            seen_by_the_woken_tick.get(),
            "a tick woken mid-teardown must still see Sona holding it"
        );
        assert!(!lease.is_held());
        assert!(lease.released_within(SELF_MIC_COOLDOWN));
    }

    /// The prewarmed and always-on paths call `start_microphone_stream`
    /// against a stream that is already running, and the idle watcher closes
    /// it exactly once. Repeated opens must not outlive that single close.
    #[test]
    fn a_prewarmed_stream_reopened_still_releases_on_its_one_lazy_close() {
        let lease = SelfInputDeviceLease::default();
        let open = || AudioRecordingManager::open_holding_lease(&lease, || Ok::<(), ()>(()));

        assert!(open().is_ok());
        assert!(open().is_ok());
        assert!(lease.is_held());

        AudioRecordingManager::close_releasing_lease(&lease, || {});
        assert!(!lease.is_held(), "the lazy close ends the episode");
    }
}

#[cfg(test)]
mod capture_verdict_tests {
    use super::{
        measure_input_level, model_arbitrates_vad_silence, VAD_SILENCE_ARBITRATION_MAX_SAMPLES,
        WHISPER_SAMPLE_RATE,
    };

    #[test]
    fn the_model_arbitrates_short_vad_silent_captures_and_not_long_ones() {
        // The two captures this policy was written against: ~1.1 s each.
        assert!(model_arbitrates_vad_silence(16_800));
        assert!(model_arbitrates_vad_silence(18_240));

        // Boundary: fifteen seconds is arbitrated, one sample past it is not.
        assert!(model_arbitrates_vad_silence(
            VAD_SILENCE_ARBITRATION_MAX_SAMPLES
        ));
        assert!(!model_arbitrates_vad_silence(
            VAD_SILENCE_ARBITRATION_MAX_SAMPLES + 1
        ));
        assert_eq!(
            VAD_SILENCE_ARBITRATION_MAX_SAMPLES,
            WHISPER_SAMPLE_RATE * 15
        );

        // Nothing to decode is nothing to arbitrate.
        assert!(!model_arbitrates_vad_silence(0));
    }

    #[test]
    fn input_level_separates_a_dead_capture_from_a_quiet_utterance() {
        assert_eq!(measure_input_level(&[]).peak, 0.0);
        assert_eq!(measure_input_level(&[]).rms, 0.0);

        // A full-scale square wave: peak and rms both reach 1.0.
        let full = measure_input_level(&[1.0, -1.0, 1.0, -1.0]);
        assert_eq!(full.peak, 1.0);
        assert_eq!(full.rms, 1.0);

        // Peak alone cannot tell these apart, which is why rms is reported
        // beside it: one transient in silence is not a spoken utterance.
        let transient = measure_input_level(&[0.0, 0.0, 0.0, 0.5]);
        let sustained = measure_input_level(&[0.5, -0.5, 0.5, -0.5]);
        assert_eq!(transient.peak, sustained.peak);
        assert!(transient.rms < sustained.rms);
    }
}

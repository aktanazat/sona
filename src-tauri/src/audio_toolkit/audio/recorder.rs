use std::{
    io::Error,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        mpsc, Arc, Mutex, MutexGuard,
    },
    time::{Duration, Instant},
};

use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    Device, InputCallbackInfo, Sample, SizedSample, StreamInstant,
};

use crate::audio_toolkit::{
    audio::{AudioVisualiser, FrameResampler},
    constants,
    vad::{self, VadFrame},
    VoiceActivityDetector,
};
use crate::meeting::{
    capture::PacketSink,
    types::{
        AudioFormat, CapturedPacket, MeetingCaptureError, PacketDiscontinuityFlags,
        PacketPushResult, SessionClockAnchor, SourceClockEpoch, SourceEpoch, SourceGap,
        SourceGapReason, SourceHealth, SourceKind, SourceStartPlan, SourceStartReport,
        SourceStopReport, TimestampBridge,
    },
};

/// The realtime lane carrying mono samples from the device callback to the
/// recorder's worker thread. Kept behind its producer/consumer halves so the
/// single-writer discipline that makes it lock-free cannot be broken from here.
mod capture_lane;

pub use capture_lane::{
    CaptureConsumer, CaptureDescriptor, CaptureOverrun, CaptureProducer, TimedCaptureOverrun,
    SOURCE_RESTARTED, TIMESTAMP_DISCONTINUITY, TIMESTAMP_MISSING,
};

/// How much native-rate mono audio the capture lane holds.
///
/// Two seconds absorbs any stall the consumer can plausibly hit — a VAD
/// inference spike or a scheduler hiccup, against 30 ms frames — and costs
/// 384 KB at 48 kHz. A deeper lane would only delay reporting a stall that has
/// already corrupted the recording.
const LANE_SECONDS: usize = 2;

/// A no-speech history entry keeps at most the first 30 seconds of 16 kHz mono audio.
const MAX_NO_SPEECH_HISTORY_SAMPLES: usize = 16_000 * 30;

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// The device-config cache shared by [`AudioRecorder::open`] and
/// [`AudioRecorder::prewarm_config`]. Keyed by device name so a system-default
/// change misses naturally.
type ConfigCache = Mutex<Option<(String, cpal::SupportedStreamConfig)>>;

/// The cached config for `device_name`, if this cache holds one for it.
///
/// An empty name never matches: cpal reports one for devices whose name query
/// fails, and treating those as a single cache key would hand one device's
/// rate and format to another.
fn cached_config_for(
    cache: &ConfigCache,
    device_name: &str,
) -> Option<cpal::SupportedStreamConfig> {
    lock_recover(cache)
        .as_ref()
        .filter(|(name, _)| !device_name.is_empty() && name == device_name)
        .map(|(_, config)| config.clone())
}

/// Remember the config this device reported, so the next open skips the HAL
/// property queries. Unnamed devices are not cached, for the reason above.
fn store_config(cache: &ConfigCache, device_name: String, config: cpal::SupportedStreamConfig) {
    if device_name.is_empty() {
        return;
    }
    *lock_recover(cache) = Some((device_name, config));
}

fn sample_rate_to_usize(sample_rate: u32) -> usize {
    match usize::try_from(sample_rate) {
        Ok(sample_rate) => sample_rate,
        Err(_) => unreachable!("desktop targets represent u32 audio sample rates"),
    }
}

fn retain_no_speech_samples(retained: &mut Vec<f32>, samples: &[f32]) {
    let remaining = MAX_NO_SPEECH_HISTORY_SAMPLES.saturating_sub(retained.len());
    let accepted = samples.len().min(remaining);
    retained.extend_from_slice(&samples[..accepted]);
}

/// How long the consumer sleeps when the capture lane is empty. This bounds both
/// the command latency on the keypress path and the delay on live streaming
/// frames, so it is far shorter than the 50 ms channel timeout it replaces; when
/// the lane has samples the loop never sleeps at all.
const LANE_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// How long the consumer sleeps while waiting for the device callback to
/// acknowledge a stop. The real wait is one buffer period (~10 ms built-in,
/// ~100 ms on Bluetooth), so this only bounds the rounding error on stop.
const STOP_ACK_POLL_INTERVAL: Duration = Duration::from_millis(1);

/// Give up waiting for the stop acknowledgement after this long: a device that
/// has stopped delivering callbacks entirely must not wedge `stop()`.
const STOP_ACK_TIMEOUT: Duration = Duration::from_secs(2);

/// The source-start acknowledgement waits for one timestamped descriptor after
/// the worker has armed the callback. The timeout is a hardware boundary with a
/// defined recovery path: the caller aborts the source before returning failure.
const START_ACK_TIMEOUT: Duration = Duration::from_secs(2);

const MEETING_CALLBACK_IDLE: u8 = 0;
const MEETING_CALLBACK_CAPTURING: u8 = 1;
const MEETING_CALLBACK_STOP_REQUESTED: u8 = 2;
const MEETING_CALLBACK_PAUSED: u8 = 3;

/// Atomics visible to the cpal callback while the recorder worker owns the
/// meeting packet sink and all lifecycle acknowledgements.
struct MeetingCallbackControl {
    mode: AtomicU8,
    source_epoch: AtomicU64,
    sequence: AtomicU64,
    format_epoch: AtomicU64,
    source_restarted: AtomicBool,
}

impl MeetingCallbackControl {
    fn new() -> Self {
        Self {
            mode: AtomicU8::new(MEETING_CALLBACK_IDLE),
            source_epoch: AtomicU64::new(0),
            sequence: AtomicU64::new(0),
            format_epoch: AtomicU64::new(0),
            source_restarted: AtomicBool::new(false),
        }
    }

    fn begin(&self, epoch: SourceEpoch) {
        self.source_epoch.store(epoch.get(), Ordering::Relaxed);
        self.sequence.store(0, Ordering::Relaxed);
        self.format_epoch.fetch_add(1, Ordering::Relaxed);
        self.source_restarted.store(false, Ordering::Relaxed);
        self.mode
            .store(MEETING_CALLBACK_CAPTURING, Ordering::Release);
    }

    fn resume(&self, epoch: SourceEpoch) {
        self.source_epoch.store(epoch.get(), Ordering::Relaxed);
        self.source_restarted.store(true, Ordering::Relaxed);
        self.mode
            .store(MEETING_CALLBACK_CAPTURING, Ordering::Release);
    }

    fn request_stop(&self) {
        self.mode
            .store(MEETING_CALLBACK_STOP_REQUESTED, Ordering::Release);
    }

    fn pause(&self) {
        self.mode.store(MEETING_CALLBACK_PAUSED, Ordering::Release);
    }

    fn idle(&self) {
        self.mode.store(MEETING_CALLBACK_IDLE, Ordering::Release);
    }
}

enum Cmd {
    /// Begin capturing. Carries the send timestamp so the consumer can log how
    /// long the command sat in the channel, plus a one-shot acknowledgement
    /// sent only after the first microphone samples are processed.
    Start(VadPolicy, Instant, mpsc::Sender<()>),
    Stop(mpsc::Sender<Result<RecordedAudio, CaptureError>>),
    StartMeeting {
        plan: SourceStartPlan,
        anchor: SessionClockAnchor,
        sink: PacketSink,
        reply: mpsc::Sender<Result<SourceStartReport, MeetingCaptureError>>,
    },
    PauseMeeting(mpsc::Sender<Result<(), MeetingCaptureError>>),
    ResumeMeeting {
        epoch: SourceEpoch,
        reply: mpsc::Sender<Result<SourceStartReport, MeetingCaptureError>>,
    },
    StopMeeting(mpsc::Sender<Result<SourceStopReport, MeetingCaptureError>>),
    AbortMeeting(mpsc::Sender<Result<(), MeetingCaptureError>>),
    Shutdown,
}

struct ActiveMeetingCapture {
    plan: SourceStartPlan,
    anchor: SessionClockAnchor,
    sink: PacketSink,
    start_reply: Option<mpsc::Sender<Result<SourceStartReport, MeetingCaptureError>>>,
    timestamp_bridge: Option<TimestampBridge>,
    format_epoch: Option<u64>,
    paused_at_offset_ns: Option<u64>,
    final_offset_ns: Option<u64>,
    observed_gaps: Vec<SourceGap>,
    untimestamped_prefix_ns: u64,
    overrun_reported: bool,
    paused: bool,
}

impl ActiveMeetingCapture {
    fn new(
        plan: SourceStartPlan,
        anchor: SessionClockAnchor,
        sink: PacketSink,
        start_reply: mpsc::Sender<Result<SourceStartReport, MeetingCaptureError>>,
    ) -> Self {
        Self {
            plan,
            anchor,
            sink,
            start_reply: Some(start_reply),
            timestamp_bridge: None,
            format_epoch: None,
            paused_at_offset_ns: None,
            final_offset_ns: None,
            observed_gaps: Vec::with_capacity(4),
            untimestamped_prefix_ns: 0,
            overrun_reported: false,
            paused: false,
        }
    }

    fn establish_timestamp_bridge(
        &mut self,
        native_timestamp: Option<(i64, u32)>,
        host_monotonic_anchor_ns: Option<u64>,
    ) -> Option<TimestampBridge> {
        if self.timestamp_bridge.is_none() {
            let (native_timestamp_value, native_timescale) = native_timestamp?;
            let host_monotonic_anchor_ns = host_monotonic_anchor_ns?;
            if native_timescale == 0 {
                return None;
            }
            let prefix_ticks = u128::from(self.untimestamped_prefix_ns)
                .checked_mul(u128::from(native_timescale))?
                .checked_div(1_000_000_000)?;
            let prefix_ticks = i64::try_from(prefix_ticks).ok()?;
            let native_anchor_value = native_timestamp_value.checked_sub(prefix_ticks)?;
            let host_monotonic_anchor_ns =
                host_monotonic_anchor_ns.checked_sub(self.untimestamped_prefix_ns)?;
            let session_offset_ns =
                host_monotonic_anchor_ns.checked_sub(self.anchor.host_monotonic_anchor_ns)?;
            self.timestamp_bridge = Some(TimestampBridge {
                native_anchor_value,
                native_timescale,
                host_monotonic_anchor_ns,
                session_offset_ns,
            });
        }
        self.timestamp_bridge
    }

    fn start_report(
        &self,
        format: AudioFormat,
        format_epoch: u64,
        timestamp_bridge: TimestampBridge,
    ) -> SourceStartReport {
        SourceStartReport {
            track_id: self.plan.track_id,
            source_kind: SourceKind::Microphone,
            format,
            epoch: self.plan.source_epoch,
            format_epoch,
            timestamp_bridge,
        }
    }

    fn packet_duration_ns(sample_rate_hz: u32, frame_count: u32) -> Option<u64> {
        u64::from(frame_count)
            .checked_mul(1_000_000_000)?
            .checked_div(u64::from(sample_rate_hz))
    }

    fn source_offset(&self, native_timestamp: Option<(i64, u32)>) -> Option<u64> {
        match native_timestamp {
            Some((value, timescale)) => self.timestamp_bridge?.map_native(value, timescale),
            None if self.timestamp_bridge.is_some() => self.final_offset_ns,
            None => None,
        }
    }

    fn source_end_offset(
        &self,
        native_timestamp: Option<(i64, u32)>,
        sample_rate_hz: u32,
        frame_count: u32,
    ) -> Option<u64> {
        self.source_offset(native_timestamp)?
            .checked_add(Self::packet_duration_ns(sample_rate_hz, frame_count)?)
    }

    fn report_gap(&mut self, gap: SourceGap) {
        self.sink.report_gap(gap.clone());
        self.observed_gaps.push(gap);
    }

    fn publish_clock_epoch(&mut self, epoch: SourceEpoch) -> Result<(), MeetingCaptureError> {
        let Some(timestamp_bridge) = self.timestamp_bridge else {
            return Err(MeetingCaptureError::InvalidState);
        };
        let Some(format_epoch) = self.format_epoch else {
            return Err(MeetingCaptureError::InvalidState);
        };
        if self.sink.report_clock_epoch(SourceClockEpoch {
            track_id: self.plan.track_id,
            epoch,
            format_epoch,
            bridge: timestamp_bridge,
        }) {
            return Ok(());
        }
        self.report_gap(SourceGap {
            track_id: self.plan.track_id,
            epoch,
            start_offset_ns: self.final_offset_ns,
            end_offset_ns: None,
            reason: SourceGapReason::TimestampDiscontinuity,
            dropped_frames: None,
        });
        Err(MeetingCaptureError::StreamFailure)
    }

    fn complete_start(
        &mut self,
        format: AudioFormat,
        format_epoch: u64,
        timestamp_bridge: TimestampBridge,
    ) {
        self.format_epoch = Some(format_epoch);
        if let Some(reply) = self.start_reply.take() {
            let _ = reply.send(Ok(self.start_report(
                format,
                format_epoch,
                timestamp_bridge,
            )));
        }
    }

    fn fail_start(&mut self, error: MeetingCaptureError) {
        if let Some(reply) = self.start_reply.take() {
            let _ = reply.send(Err(error));
        }
    }

    fn stop_report(&self) -> SourceStopReport {
        SourceStopReport {
            track_id: self.plan.track_id,
            final_offset_ns: self.final_offset_ns,
            health: SourceHealth::Healthy,
            observed_gaps: self.observed_gaps.clone(),
        }
    }
}

/// Why a recording could not be delivered.
#[derive(Debug)]
pub enum CaptureError {
    /// The recorder is not open, or its capture worker has already exited.
    NotCapturing,
    /// The realtime capture lane overran. `prefix_samples` is the contiguous
    /// 16 kHz prefix from before the gap. It may be saved and retried by the
    /// user, but must never be transcribed automatically.
    Overrun {
        overrun: CaptureOverrun,
        prefix_samples: Vec<f32>,
    },
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureError::NotCapturing => f.write_str("no capture worker is running"),
            CaptureError::Overrun { overrun, .. } => overrun.fmt(f),
        }
    }
}

impl std::error::Error for CaptureError {}

/// Audio retained from one complete capture.
///
/// The recorder reports what VAD observed, never a verdict on whether the user
/// spoke: VAD is an optimizer that keeps silence off the ASR engine, and it is
/// wrong often enough on quiet speech that only the model may conclude a
/// capture was silent. When `vad_forwarded_speech` is false, `samples` is the
/// raw unfiltered clip (bounded by [`MAX_NO_SPEECH_HISTORY_SAMPLES`]) precisely
/// so a caller can still decode it; otherwise it is the VAD-selected audio that
/// was also forwarded to any live stream.
#[derive(Debug)]
pub struct RecordedAudio {
    pub samples: Vec<f32>,
    pub vad_forwarded_speech: bool,
}

/// How 16 kHz mono frames should be filtered for one recording session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VadPolicy {
    /// Bypass VAD and forward every frame.
    Disabled,
    /// Current offline-tuned VAD profile.
    Offline,
    /// VAD profile with a longer post-speech tail for streaming-capable models.
    Streaming,
}

/// A single VAD engine plus the two hangover-tail lengths its smoothing wrapper
/// should use. The offline and streaming policies are never active
/// concurrently, so one detector is reconfigured per session (see `Cmd::Start`)
/// rather than kept as two resident engines.
#[derive(Clone)]
struct VadConfig {
    detector: Arc<Mutex<Box<dyn vad::VoiceActivityDetector>>>,
    offline_hangover_frames: usize,
    streaming_hangover_frames: usize,
}

impl VadConfig {
    /// Post-speech hangover tail (in 30 ms frames) for the given policy.
    /// `Disabled` never reaches the detector, so it maps to the offline value.
    fn hangover_for(&self, policy: VadPolicy) -> usize {
        match policy {
            VadPolicy::Streaming => self.streaming_hangover_frames,
            VadPolicy::Offline | VadPolicy::Disabled => self.offline_hangover_frames,
        }
    }
}

/// Callback invoked with each 16 kHz mono frame that passes the active capture
/// policy while recording. Used to feed a live streaming transcription as audio arrives.
pub type AudioFrameCallback = Arc<dyn Fn(&[f32]) + Send + Sync + 'static>;
struct StreamBuildOptions {
    producer: CaptureProducer,
    channels: usize,
    selected_channel: Option<usize>,
    stream_error: Arc<AtomicBool>,
    meeting_control: Arc<MeetingCallbackControl>,
    sample_rate: u32,
}

struct ConsumerInputs {
    in_sample_rate: u32,
    vad: Option<VadConfig>,
    lane: CaptureConsumer,
    cmd_rx: mpsc::Receiver<Cmd>,
    level_cb: Option<Arc<dyn Fn(Vec<f32>) + Send + Sync + 'static>>,
    audio_cb: Option<AudioFrameCallback>,
    stream_running_at: Instant,
    meeting_control: Arc<MeetingCallbackControl>,
}

pub struct AudioRecorder {
    device: Option<Device>,
    cmd_tx: Option<mpsc::Sender<Cmd>>,
    worker_handle: Option<std::thread::JoinHandle<()>>,
    vad: Option<VadConfig>,
    level_cb: Option<Arc<dyn Fn(Vec<f32>) + Send + Sync + 'static>>,
    audio_cb: Option<AudioFrameCallback>,
    /// Which input channel to use. None = average all (original behavior).
    selected_channel: Option<usize>,
    /// Preferred stream config cached per device name. The two HAL property
    /// queries in `get_preferred_config` cost ~26-44ms per open (worse on
    /// USB/Bluetooth), which lands on the keypress->capture path in on-demand
    /// mode. Filled by the first successful open, or ahead of it by
    /// [`prewarm_config`](Self::prewarm_config); cleared whenever an open fails
    /// so a stale rate/format self-heals on the caller's retry.
    config_cache: Arc<ConfigCache>,
    /// Set by cpal when the active input stream can no longer capture.
    stream_error: Arc<AtomicBool>,
}

impl AudioRecorder {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(AudioRecorder {
            device: None,
            cmd_tx: None,
            worker_handle: None,
            vad: None,
            level_cb: None,
            audio_cb: None,
            selected_channel: None,
            config_cache: Arc::new(Mutex::new(None)),
            stream_error: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Attach a single VAD engine, reconfigured per session for the offline vs
    /// streaming hangover tail. The two policies are mutually exclusive within a
    /// recording, so one engine covers both instead of two resident instances.
    pub fn with_vad(
        mut self,
        detector: Box<dyn VoiceActivityDetector>,
        offline_hangover_frames: usize,
        streaming_hangover_frames: usize,
    ) -> Self {
        self.vad = Some(VadConfig {
            detector: Arc::new(Mutex::new(detector)),
            offline_hangover_frames,
            streaming_hangover_frames,
        });
        self
    }

    pub fn with_level_callback<F>(mut self, cb: F) -> Self
    where
        F: Fn(Vec<f32>) + Send + Sync + 'static,
    {
        self.level_cb = Some(Arc::new(cb));
        self
    }

    /// Register a callback that receives real-time 16 kHz frames after the active
    /// VAD policy has been applied. Frames arrive in real time, in order, on the
    /// recorder's consumer thread — keep the callback cheap (e.g. forward to a
    /// channel) so it never stalls capture.
    pub fn with_audio_callback<F>(mut self, cb: F) -> Self
    where
        F: Fn(&[f32]) + Send + Sync + 'static,
    {
        self.audio_cb = Some(Arc::new(cb));
        self
    }

    pub fn with_selected_channel(mut self, channel: Option<u16>) -> Self {
        self.set_selected_channel(channel);
        self
    }

    pub fn set_selected_channel(&mut self, channel: Option<u16>) {
        self.selected_channel = channel.map(usize::from);
    }

    pub fn open(&mut self, device: Option<Device>) -> Result<(), Box<dyn std::error::Error>> {
        if self.worker_handle.is_some() {
            if !self.needs_reopen() {
                return Ok(()); // already open
            }
            log::warn!("Capture stream failed; rebuilding microphone stream");
            let _ = self.close();
        }

        self.stream_error.store(false, Ordering::Relaxed);

        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let (init_tx, init_rx) = mpsc::sync_channel::<Result<(), String>>(1);

        let host = crate::audio_toolkit::get_cpal_host();
        let device = match device {
            Some(dev) => dev,
            None => host
                .default_input_device()
                .ok_or_else(|| Error::new(std::io::ErrorKind::NotFound, "No input device found"))?,
        };

        let thread_device = device.clone();
        let vad = self.vad.clone();
        // Move the optional level callback into the worker thread
        let level_cb = self.level_cb.clone();
        // Move the optional real-time audio frame callback into the worker thread
        let audio_cb = self.audio_cb.clone();
        let selected_channel = self.selected_channel;
        let config_cache = Arc::clone(&self.config_cache);
        let stream_error = Arc::clone(&self.stream_error);

        let worker = std::thread::spawn(move || {
            let init_result = (|| -> Result<
                (
                    cpal::Stream,
                    u32,
                    CaptureConsumer,
                    Arc<MeetingCallbackControl>,
                ),
                String,
            > {
                let config_started = Instant::now();
                let device_name = thread_device.name().unwrap_or_default();
                let cached_config = cached_config_for(&config_cache, &device_name);
                let config_was_cached = cached_config.is_some();
                let config = match cached_config {
                    Some(cfg) => cfg,
                    None => AudioRecorder::get_preferred_config(&thread_device)
                        .map_err(|e| format!("Failed to fetch preferred config: {e}"))?,
                };
                let config_elapsed = config_started.elapsed();

                let sample_rate = config.sample_rate().0;
                let channels = usize::from(config.channels());

                log::info!(
                    "Using device: {:?}\nSample rate: {}\nChannels: {}\nFormat: {:?}",
                    thread_device.name(),
                    sample_rate,
                    channels,
                    config.sample_format()
                );

                if let Some(channel) = selected_channel {
                    if channel < channels {
                        log::info!("Using selected input channel: {}", channel + 1);
                    } else {
                        log::warn!(
                            "Selected input channel {} is out of range for a {}-channel device; averaging all channels instead",
                            channel + 1,
                            channels
                        );
                    }
                } else {
                    log::info!("Averaging all {} input channels", channels);
                }

                // Built here, before play(), so the device callback owns
                // preallocated storage and never has to touch the heap. The
                // descriptor sidecar remains dormant for dictation.
                let sample_capacity = usize::try_from(sample_rate)
                    .unwrap_or(usize::MAX)
                    .saturating_mul(LANE_SECONDS);
                let descriptor_capacity =
                    AudioRecorder::meeting_descriptor_capacity(&config, sample_capacity);
                let (producer, consumer) = capture_lane::timed_lane_with_descriptor_capacity(
                    sample_capacity,
                    descriptor_capacity,
                );
                let meeting_control = Arc::new(MeetingCallbackControl::new());

                let build_started = Instant::now();
                let stream_options = |producer| StreamBuildOptions {
                    producer,
                    channels,
                    selected_channel,
                    stream_error: Arc::clone(&stream_error),
                    meeting_control: Arc::clone(&meeting_control),
                    sample_rate,
                };
                let stream = match config.sample_format() {
                    cpal::SampleFormat::U8 => {
                        AudioRecorder::build_stream::<u8>(&thread_device, &config, stream_options(producer))
                    }
                    cpal::SampleFormat::I8 => {
                        AudioRecorder::build_stream::<i8>(&thread_device, &config, stream_options(producer))
                    }
                    cpal::SampleFormat::I16 => {
                        AudioRecorder::build_stream::<i16>(&thread_device, &config, stream_options(producer))
                    }
                    cpal::SampleFormat::I32 => {
                        AudioRecorder::build_stream::<i32>(&thread_device, &config, stream_options(producer))
                    }
                    cpal::SampleFormat::F32 => {
                        AudioRecorder::build_stream::<f32>(&thread_device, &config, stream_options(producer))
                    }
                    sample_format => {
                        return Err(format!("Unsupported sample format: {sample_format:?}"));
                    }
                }
                .map_err(|error| format!("Failed to build input stream: {error}"))?;
                let build_elapsed = build_started.elapsed();

                let play_started = Instant::now();
                stream
                    .play()
                    .map_err(|e| format!("Failed to start microphone stream: {e}"))?;
                log::debug!(
                    "mic worker init: fetch_config={:?} (cached={}) build_stream={:?} play={:?}",
                    config_elapsed,
                    config_was_cached,
                    build_elapsed,
                    play_started.elapsed()
                );

                // The device accepted this config; remember it so the next
                // open skips the HAL property queries entirely.
                if !config_was_cached {
                    store_config(&config_cache, device_name, config);
                }

                Ok((stream, sample_rate, consumer, meeting_control))
            })();

            match init_result {
                Ok((stream, sample_rate, consumer, meeting_control)) => {
                    let _ = init_tx.send(Ok(()));
                    // Timestamp for the play()-returned -> first-samples gap the
                    // init handshake can't see (hardware dependent).
                    let stream_running_at = Instant::now();
                    // Keep the stream alive while we process samples.
                    run_consumer(ConsumerInputs {
                        in_sample_rate: sample_rate,
                        vad,
                        lane: consumer,
                        cmd_rx,
                        level_cb,
                        audio_cb,
                        stream_running_at,
                        meeting_control,
                    });
                    drop(stream);
                }
                Err(error_message) => {
                    // A failed open may mean the cached config went stale
                    // (device re-plugged, rate/format changed in the OS).
                    // Drop it so the next attempt re-queries the device.
                    *lock_recover(&config_cache) = None;
                    log::error!("{error_message}");
                    let _ = init_tx.send(Err(error_message));
                }
            }
        });

        match init_rx.recv() {
            Ok(Ok(())) => {
                self.device = Some(device);
                self.cmd_tx = Some(cmd_tx);
                self.worker_handle = Some(worker);
                Ok(())
            }
            Ok(Err(error_message)) => {
                let _ = worker.join();
                let kind = if is_microphone_access_denied(&error_message) {
                    std::io::ErrorKind::PermissionDenied
                } else {
                    std::io::ErrorKind::Other
                };
                Err(Box::new(Error::new(kind, error_message)))
            }
            Err(recv_error) => {
                let _ = worker.join();
                Err(Box::new(Error::other(format!(
                    "Failed to initialize microphone worker: {recv_error}"
                ))))
            }
        }
    }

    /// Resolve and cache this device's preferred stream config without opening
    /// a stream, so the first `open()` skips the HAL property queries that
    /// otherwise land between the keypress and the first captured sample.
    ///
    /// Measured on an M4 Pro: `default_input_config` + `supported_input_configs`
    /// cost 44ms on the first call and ~26ms on later ones for an LG UltraFine
    /// display-audio input, and CoreAudio does not cache them for us — which is
    /// why this cache exists at all.
    ///
    /// These are property reads: no AudioUnit is constructed and the device is
    /// never started, so this does not raise the OS microphone indicator.
    /// Verified on macOS 26.6 by polling
    /// `kAudioDevicePropertyDeviceIsRunningSomewhere` (stayed 0) and by
    /// counting orange menu-bar pixels across the call (stayed 0).
    ///
    /// A config cached here is a claim about what the device reports, not that
    /// an open succeeded with it. `open()` already clears the cache and
    /// re-queries when a build fails, which is the same self-healing path a
    /// re-plugged device takes.
    pub fn prewarm_config(&self, device: Option<Device>) -> Result<(), Box<dyn std::error::Error>> {
        let device = match device {
            Some(device) => device,
            None => crate::audio_toolkit::get_cpal_host()
                .default_input_device()
                .ok_or_else(|| Error::new(std::io::ErrorKind::NotFound, "No input device found"))?,
        };
        let device_name = device.name().unwrap_or_default();
        if cached_config_for(&self.config_cache, &device_name).is_some() {
            return Ok(());
        }
        let started = Instant::now();
        let config = Self::get_preferred_config(&device)?;
        log::debug!(
            "prewarmed input config for {:?} in {:?}: {} Hz, {} ch, {:?}",
            device_name,
            started.elapsed(),
            config.sample_rate().0,
            config.channels(),
            config.sample_format()
        );
        store_config(&self.config_cache, device_name, config);
        Ok(())
    }

    /// Queue a recording start and return a one-shot receiver that resolves only
    /// after the first real microphone sample chunk has entered the capture path.
    /// `Stream::play()` returning is not sufficient: some Bluetooth and USB
    /// devices take much longer to begin delivering callbacks.
    pub fn start(
        &self,
        vad_policy: VadPolicy,
    ) -> Result<mpsc::Receiver<()>, Box<dyn std::error::Error>> {
        let tx = self
            .cmd_tx
            .as_ref()
            .ok_or_else(|| Error::other("Recorder is not open"))?;
        let (ready_tx, ready_rx) = mpsc::channel();
        tx.send(Cmd::Start(vad_policy, Instant::now(), ready_tx))?;
        Ok(ready_rx)
    }

    /// End the recording and return its 16 kHz mono samples.
    ///
    /// [`CaptureError::Overrun`] carries the clean prefix that arrived before
    /// the sticky capture gap. The caller may persist that prefix for an
    /// explicit retry, but must not decode or deliver it automatically.
    pub fn stop(&self) -> Result<RecordedAudio, CaptureError> {
        let tx = self.cmd_tx.as_ref().ok_or(CaptureError::NotCapturing)?;
        let (resp_tx, resp_rx) = mpsc::channel();
        tx.send(Cmd::Stop(resp_tx))
            .map_err(|_| CaptureError::NotCapturing)?;
        resp_rx
            .recv() // wait for the samples
            .map_err(|_| CaptureError::NotCapturing)?
    }

    /// Start timestamped microphone streaming for an already-authorized
    /// meeting source. The cpal callback does not receive the packet sink;
    /// the recorder worker drains the preallocated lane and forwards it.
    pub(crate) fn start_meeting_capture(
        &self,
        plan: SourceStartPlan,
        anchor: SessionClockAnchor,
        sink: PacketSink,
    ) -> Result<SourceStartReport, MeetingCaptureError> {
        let tx = self
            .cmd_tx
            .as_ref()
            .ok_or(MeetingCaptureError::Unavailable)?;
        let (reply_tx, reply_rx) = mpsc::channel();
        tx.send(Cmd::StartMeeting {
            plan,
            anchor,
            sink,
            reply: reply_tx,
        })
        .map_err(|_| MeetingCaptureError::StreamFailure)?;

        match reply_rx.recv_timeout(START_ACK_TIMEOUT) {
            Ok(reply) => reply,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = self.abort_meeting_capture();
                Err(MeetingCaptureError::StreamFailure)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(MeetingCaptureError::StreamFailure),
        }
    }

    pub(crate) fn pause_meeting_capture(&self) -> Result<(), MeetingCaptureError> {
        let tx = self
            .cmd_tx
            .as_ref()
            .ok_or(MeetingCaptureError::Unavailable)?;
        let (reply_tx, reply_rx) = mpsc::channel();
        tx.send(Cmd::PauseMeeting(reply_tx))
            .map_err(|_| MeetingCaptureError::StreamFailure)?;
        reply_rx
            .recv_timeout(STOP_ACK_TIMEOUT)
            .map_err(|_| MeetingCaptureError::StreamFailure)?
    }

    pub(crate) fn resume_meeting_capture(
        &self,
        epoch: SourceEpoch,
    ) -> Result<SourceStartReport, MeetingCaptureError> {
        let tx = self
            .cmd_tx
            .as_ref()
            .ok_or(MeetingCaptureError::Unavailable)?;
        let (reply_tx, reply_rx) = mpsc::channel();
        tx.send(Cmd::ResumeMeeting {
            epoch,
            reply: reply_tx,
        })
        .map_err(|_| MeetingCaptureError::StreamFailure)?;
        match reply_rx.recv_timeout(START_ACK_TIMEOUT) {
            Ok(reply) => reply,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = self.abort_meeting_capture();
                Err(MeetingCaptureError::StreamFailure)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(MeetingCaptureError::StreamFailure),
        }
    }

    pub(crate) fn stop_meeting_capture(&self) -> Result<SourceStopReport, MeetingCaptureError> {
        let tx = self
            .cmd_tx
            .as_ref()
            .ok_or(MeetingCaptureError::Unavailable)?;
        let (reply_tx, reply_rx) = mpsc::channel();
        tx.send(Cmd::StopMeeting(reply_tx))
            .map_err(|_| MeetingCaptureError::StreamFailure)?;
        reply_rx
            .recv_timeout(STOP_ACK_TIMEOUT)
            .map_err(|_| MeetingCaptureError::StreamFailure)?
    }

    pub(crate) fn abort_meeting_capture(&self) -> Result<(), MeetingCaptureError> {
        let tx = self
            .cmd_tx
            .as_ref()
            .ok_or(MeetingCaptureError::Unavailable)?;
        let (reply_tx, reply_rx) = mpsc::channel();
        tx.send(Cmd::AbortMeeting(reply_tx))
            .map_err(|_| MeetingCaptureError::StreamFailure)?;
        reply_rx
            .recv_timeout(STOP_ACK_TIMEOUT)
            .map_err(|_| MeetingCaptureError::StreamFailure)?
    }

    /// True when the active capture stream must be rebuilt.
    ///
    /// cpal may report a device disconnect asynchronously without closing its
    /// callback channel, so also honor the error callback's explicit flag.
    pub fn needs_reopen(&self) -> bool {
        self.stream_error.load(Ordering::Relaxed)
            || self
                .worker_handle
                .as_ref()
                .is_some_and(|handle| handle.is_finished())
    }

    pub fn close(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(tx) = self.cmd_tx.take() {
            let _ = tx.send(Cmd::Shutdown);
        }
        if let Some(h) = self.worker_handle.take() {
            let _ = h.join();
        }
        self.device = None;
        Ok(())
    }

    fn meeting_descriptor_capacity(
        config: &cpal::SupportedStreamConfig,
        sample_capacity: usize,
    ) -> usize {
        match config.buffer_size() {
            cpal::SupportedBufferSize::Range { min, .. } => {
                let minimum_frames = usize::try_from(*min).unwrap_or(sample_capacity).max(1);
                sample_capacity.saturating_add(minimum_frames.saturating_sub(1)) / minimum_frames
            }
            cpal::SupportedBufferSize::Unknown => sample_capacity,
        }
    }

    fn build_stream<T>(
        device: &cpal::Device,
        config: &cpal::SupportedStreamConfig,
        options: StreamBuildOptions,
    ) -> Result<cpal::Stream, cpal::BuildStreamError>
    where
        T: Sample + SizedSample + Send + 'static,
        f32: cpal::FromSample<T>,
    {
        let StreamBuildOptions {
            mut producer,
            channels,
            selected_channel,
            stream_error,
            meeting_control,
            sample_rate,
        } = options;
        // Resolve the effective channel to use. If the selected channel is
        // out of range for this device, fall back to averaging all channels.
        let use_channel: Option<usize> = match selected_channel {
            Some(ch) if ch < channels => Some(ch),
            Some(_) => None, // out of range, fall back to average
            None => None,    // user chose "average all"
        };
        let mut last_meeting_timestamp_ns = None;

        device.build_input_stream(
            &config.clone().into(),
            move |data: &[T], callback_info: &InputCallbackInfo| match meeting_control
                .mode
                .load(Ordering::Acquire)
            {
                MEETING_CALLBACK_CAPTURING => capture_into_timed_lane(
                    data,
                    callback_info,
                    TimedCaptureState {
                        channels,
                        use_channel,
                        sample_rate,
                        meeting_control: &meeting_control,
                        last_timestamp_value: &mut last_meeting_timestamp_ns,
                        producer: &mut producer,
                    },
                ),
                MEETING_CALLBACK_STOP_REQUESTED => {
                    producer.acknowledge_stop();
                }
                MEETING_CALLBACK_PAUSED => {}
                _ => capture_into_lane(data, channels, use_channel, &mut producer),
            },
            move |err| {
                log::error!("Stream error: {}", err);
                stream_error.store(true, Ordering::Relaxed);
            },
            None,
        )
    }

    pub fn preferred_input_channel_count(
        device: &cpal::Device,
    ) -> Result<u16, Box<dyn std::error::Error>> {
        Ok(Self::get_preferred_config(device)?.channels())
    }

    fn get_preferred_config(
        device: &cpal::Device,
    ) -> Result<cpal::SupportedStreamConfig, Box<dyn std::error::Error>> {
        // Use the device's native/default sample rate and let the FrameResampler
        // in run_consumer() downsample to 16kHz. This avoids forcing hardware into
        // a non-native rate which can cause issues on some devices (Bluetooth
        // codecs, certain ALSA drivers, etc.).
        let default_config = device.default_input_config()?;
        let target_rate = default_config.sample_rate();

        // Try to find the best sample format at the device's default rate
        let supported_configs = match device.supported_input_configs() {
            Ok(configs) => configs,
            Err(e) => {
                log::warn!("Could not enumerate input configs ({e}), using device default");
                return Ok(default_config);
            }
        };
        let mut best_config: Option<cpal::SupportedStreamConfigRange> = None;

        for config_range in supported_configs {
            if config_range.min_sample_rate() <= target_rate
                && config_range.max_sample_rate() >= target_rate
            {
                match best_config {
                    None => best_config = Some(config_range),
                    Some(ref current) => {
                        // Prioritize F32 > I16 > I32 > others
                        let score = |fmt: cpal::SampleFormat| match fmt {
                            cpal::SampleFormat::F32 => 4,
                            cpal::SampleFormat::I16 => 3,
                            cpal::SampleFormat::I32 => 2,
                            _ => 1,
                        };

                        if score(config_range.sample_format()) > score(current.sample_format()) {
                            best_config = Some(config_range);
                        }
                    }
                }
            }
        }

        if let Some(config) = best_config {
            return Ok(config.with_sample_rate(target_rate));
        }

        // Fall back to device default if no config matched (exotic/virtual devices)
        log::warn!(
            "No supported config matched device default rate {:?}, using default config",
            target_rate
        );
        Ok(default_config)
    }
}

/// The entire body of the cpal input callback: downmix one interleaved device
/// buffer to mono straight into the preallocated capture lane.
///
/// Realtime-safe by construction — no allocation, no locking, no logging, no
/// syscall — and it is the only thing the device callback does, so those are the
/// stream's properties too. Nothing is copied twice: the mixed samples land
/// directly in the lane's slots.
fn capture_into_lane<T>(
    data: &[T],
    channels: usize,
    use_channel: Option<usize>,
    producer: &mut CaptureProducer,
) where
    T: Sample,
    f32: cpal::FromSample<T>,
{
    let frames = data.len() / channels;
    if frames == 0 {
        return;
    }

    producer.commit(frames, |first, second| {
        if channels == 1 {
            for (slot, &sample) in first.iter_mut().chain(second.iter_mut()).zip(data) {
                *slot = sample.to_sample::<f32>();
            }
            return;
        }

        match use_channel {
            Some(channel) => {
                for (slot, frame) in first
                    .iter_mut()
                    .chain(second.iter_mut())
                    .zip(data.chunks_exact(channels))
                {
                    *slot = frame[channel].to_sample::<f32>();
                }
            }
            None => {
                let channel_count = u16::try_from(channels).unwrap_or(u16::MAX);
                for (slot, frame) in first
                    .iter_mut()
                    .chain(second.iter_mut())
                    .zip(data.chunks_exact(channels))
                {
                    *slot = frame
                        .iter()
                        .map(|&sample| sample.to_sample::<f32>())
                        .sum::<f32>()
                        / f32::from(channel_count);
                }
            }
        }
    });
}

/// CoreAudio exposes cpal input timestamps from `mach_absolute_time`, which is
/// the host monotonic clock. Other backends retain their native timestamp but
/// do not claim a host-clock bridge without an explicit platform adapter.
#[cfg(target_os = "macos")]
fn cpal_host_monotonic_anchor_ns(native_timestamp_value: Option<i64>) -> Option<u64> {
    native_timestamp_value.and_then(|value| u64::try_from(value).ok())
}

#[cfg(not(target_os = "macos"))]
fn cpal_host_monotonic_anchor_ns(_native_timestamp_value: Option<i64>) -> Option<u64> {
    None
}

struct TimedCaptureState<'a> {
    channels: usize,
    use_channel: Option<usize>,
    sample_rate: u32,
    meeting_control: &'a MeetingCallbackControl,
    last_timestamp_value: &'a mut Option<i64>,
    producer: &'a mut CaptureProducer,
}

/// Capture the same native-rate mono samples as dictation, paired with the
/// cpal capture timestamp that belongs to this callback.
///
/// This remains a callback-only copy into fixed storage: it does not call the
/// meeting packet sink, allocate, lock, log, resample, or dispatch UI work.
fn capture_into_timed_lane<T>(
    data: &[T],
    callback_info: &InputCallbackInfo,
    state: TimedCaptureState<'_>,
) where
    T: Sample,
    f32: cpal::FromSample<T>,
{
    let TimedCaptureState {
        channels,
        use_channel,
        sample_rate,
        meeting_control,
        last_timestamp_value,
        producer,
    } = state;
    let frames = data.len() / channels;
    if frames == 0 {
        return;
    }

    let native_timestamp_value = callback_info
        .timestamp()
        .capture
        .duration_since(&StreamInstant::new(0, 0))
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok());
    let native_timestamp_timescale = native_timestamp_value.map(|_| 1_000_000_000);
    let host_monotonic_anchor_ns = cpal_host_monotonic_anchor_ns(native_timestamp_value);
    let mut flags = 0;
    let source_restarted = meeting_control.source_restarted.load(Ordering::Acquire);
    if source_restarted {
        flags |= SOURCE_RESTARTED;
    }
    match native_timestamp_value {
        Some(timestamp) => {
            if last_timestamp_value.is_some_and(|previous| timestamp <= previous) {
                flags |= TIMESTAMP_DISCONTINUITY;
            }
            *last_timestamp_value = Some(timestamp);
        }
        None => flags |= TIMESTAMP_MISSING,
    }
    let descriptor = CaptureDescriptor {
        sequence: meeting_control.sequence.fetch_add(1, Ordering::Relaxed),
        source_epoch: meeting_control.source_epoch.load(Ordering::Relaxed),
        native_timestamp_value: native_timestamp_value.unwrap_or_default(),
        native_timestamp_timescale: native_timestamp_timescale.unwrap_or_default(),
        host_monotonic_anchor_ns,
        format_epoch: meeting_control.format_epoch.load(Ordering::Relaxed),
        frame_start: 0,
        frame_count: 0,
        sample_rate,
        channels: 1,
        sample_format: capture_lane::CaptureSampleFormat::F32,
        flags,
    };

    let accepted = producer.commit_timed(frames, descriptor, |first, second| {
        if channels == 1 {
            for (slot, &sample) in first.iter_mut().chain(second.iter_mut()).zip(data) {
                *slot = sample.to_sample::<f32>();
            }
            return;
        }

        match use_channel {
            Some(channel) => {
                for (slot, frame) in first
                    .iter_mut()
                    .chain(second.iter_mut())
                    .zip(data.chunks_exact(channels))
                {
                    *slot = frame[channel].to_sample::<f32>();
                }
            }
            None => {
                let channel_count = u16::try_from(channels).unwrap_or(u16::MAX);
                for (slot, frame) in first
                    .iter_mut()
                    .chain(second.iter_mut())
                    .zip(data.chunks_exact(channels))
                {
                    *slot = frame
                        .iter()
                        .map(|&sample| sample.to_sample::<f32>())
                        .sum::<f32>()
                        / f32::from(channel_count);
                }
            }
        }
    });
    if accepted && source_restarted {
        meeting_control
            .source_restarted
            .store(false, Ordering::Release);
    }
}

pub fn is_microphone_access_denied(error_message: &str) -> bool {
    let normalized = error_message.to_lowercase();
    normalized.contains("access is denied")
        || normalized.contains("permission denied")
        || normalized.contains("0x80070005")
}

pub fn is_no_input_device_error(error_message: &str) -> bool {
    let normalized = error_message.to_lowercase();
    normalized.contains("no input device found")
        || (normalized.contains("failed to fetch preferred config")
            && normalized.contains("coreaudio"))
}

fn descriptor_timestamp(descriptor: CaptureDescriptor) -> Option<(i64, u32)> {
    (descriptor.flags & TIMESTAMP_MISSING == 0 && descriptor.native_timestamp_timescale > 0)
        .then_some((
            descriptor.native_timestamp_value,
            descriptor.native_timestamp_timescale,
        ))
}

fn observe_meeting_packet(
    capture: &mut ActiveMeetingCapture,
    descriptor: CaptureDescriptor,
    samples: &[f32],
) {
    let native_timestamp = descriptor_timestamp(descriptor);
    if native_timestamp.is_none() && capture.timestamp_bridge.is_none() {
        let Some(duration_ns) = ActiveMeetingCapture::packet_duration_ns(
            descriptor.sample_rate,
            descriptor.frame_count,
        ) else {
            return;
        };
        let Some(prefix_ns) = capture.untimestamped_prefix_ns.checked_add(duration_ns) else {
            return;
        };
        capture.untimestamped_prefix_ns = prefix_ns;
    }
    let timestamp_bridge =
        capture.establish_timestamp_bridge(native_timestamp, descriptor.host_monotonic_anchor_ns);
    let packet = CapturedPacket {
        track_id: capture.plan.track_id,
        source_epoch: SourceEpoch::new(descriptor.source_epoch),
        format_epoch: descriptor.format_epoch,
        sequence: descriptor.sequence,
        native_timestamp_value: native_timestamp.map(|(value, _)| value),
        native_timestamp_timescale: native_timestamp.map(|(_, timescale)| timescale),
        host_monotonic_anchor_ns: descriptor.host_monotonic_anchor_ns,
        sample_rate_hz: descriptor.sample_rate,
        channels: descriptor.channels,
        frame_count: descriptor.frame_count,
        discontinuity_flags: PacketDiscontinuityFlags {
            timestamp_reset: descriptor.flags & TIMESTAMP_DISCONTINUITY != 0,
            route_changed: false,
            source_restarted: descriptor.flags & SOURCE_RESTARTED != 0,
        },
    };
    let start_offset_ns = capture.source_offset(native_timestamp);
    let end_offset_ns = capture.source_end_offset(
        native_timestamp,
        descriptor.sample_rate,
        descriptor.frame_count,
    );

    if descriptor.flags & TIMESTAMP_DISCONTINUITY != 0 {
        capture.report_gap(SourceGap {
            track_id: capture.plan.track_id,
            epoch: packet.source_epoch,
            start_offset_ns,
            end_offset_ns,
            reason: SourceGapReason::TimestampDiscontinuity,
            dropped_frames: None,
        });
    }
    if let (Some(pause_start), Some(pause_end)) = (capture.paused_at_offset_ns, start_offset_ns) {
        capture.paused_at_offset_ns = None;
        capture.report_gap(SourceGap {
            track_id: capture.plan.track_id,
            epoch: packet.source_epoch,
            start_offset_ns: Some(pause_start),
            end_offset_ns: Some(pause_end),
            reason: SourceGapReason::Paused,
            dropped_frames: None,
        });
    }

    if let PacketPushResult::Dropped { frames } = capture.sink.try_push_interleaved(packet, samples)
    {
        capture.observed_gaps.push(SourceGap {
            track_id: capture.plan.track_id,
            epoch: packet.source_epoch,
            start_offset_ns,
            end_offset_ns,
            reason: SourceGapReason::PacketDropped,
            dropped_frames: Some(u64::from(frames)),
        });
    }
    capture.final_offset_ns = end_offset_ns.or(capture.final_offset_ns);
    if let Some(timestamp_bridge) = timestamp_bridge {
        capture.complete_start(packet.format(), descriptor.format_epoch, timestamp_bridge);
    }
}

fn drain_meeting_lane(
    lane: &mut CaptureConsumer,
    capture: &mut ActiveMeetingCapture,
    wrapped_packet_scratch: &mut Vec<f32>,
) -> usize {
    lane.drain_timed(|descriptor, first, second| {
        if second.is_empty() {
            observe_meeting_packet(capture, descriptor, first);
            return;
        }

        wrapped_packet_scratch.clear();
        wrapped_packet_scratch.extend_from_slice(first);
        wrapped_packet_scratch.extend_from_slice(second);
        observe_meeting_packet(capture, descriptor, wrapped_packet_scratch);
    })
}

fn report_meeting_lane_overrun(capture: &mut ActiveMeetingCapture, overrun: TimedCaptureOverrun) {
    let descriptor = overrun.first_dropped;
    let native_timestamp = descriptor_timestamp(descriptor);
    capture.report_gap(SourceGap {
        track_id: capture.plan.track_id,
        epoch: SourceEpoch::new(descriptor.source_epoch),
        start_offset_ns: capture.source_offset(native_timestamp),
        end_offset_ns: capture.source_end_offset(
            native_timestamp,
            descriptor.sample_rate,
            descriptor.frame_count,
        ),
        reason: SourceGapReason::WriterPressure,
        dropped_frames: Some(u64::try_from(overrun.capture.lost_samples).unwrap_or(u64::MAX)),
    });
    capture.overrun_reported = true;
}

fn observe_meeting_lane_overrun(
    lane: &CaptureConsumer,
    capture: &mut ActiveMeetingCapture,
    sample_rate: u32,
) {
    if capture.overrun_reported {
        return;
    }
    if let Some(overrun) = lane.timed_overrun(sample_rate) {
        report_meeting_lane_overrun(capture, overrun);
        return;
    }
    if let Some(overrun) = lane.overrun(sample_rate) {
        capture.report_gap(SourceGap {
            track_id: capture.plan.track_id,
            epoch: capture.plan.source_epoch,
            start_offset_ns: capture.final_offset_ns,
            end_offset_ns: None,
            reason: SourceGapReason::InvalidFormat,
            dropped_frames: Some(u64::try_from(overrun.lost_samples).unwrap_or(u64::MAX)),
        });
        capture.overrun_reported = true;
    }
}

fn close_meeting_callback(
    lane: &mut CaptureConsumer,
    capture: &mut ActiveMeetingCapture,
    wrapped_packet_scratch: &mut Vec<f32>,
    meeting_control: &MeetingCallbackControl,
    sample_rate: u32,
) -> bool {
    // Close the lane before the callback mode, so the next callback is still
    // dispatched as a capture and commits the block it took before the stop.
    // The lane keeps that boundary block and acknowledges on the same call;
    // closing the mode first would drop up to one buffer period of tail audio.
    let acknowledgements_before = lane.stop_acks();
    lane.request_stop();
    meeting_control.request_stop();
    let deadline = Instant::now() + STOP_ACK_TIMEOUT;

    loop {
        let acknowledged = lane.stop_acks() != acknowledgements_before;
        drain_meeting_lane(lane, capture, wrapped_packet_scratch);
        observe_meeting_lane_overrun(lane, capture, sample_rate);
        if acknowledged {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(STOP_ACK_POLL_INTERVAL);
    }
}

fn report_unacknowledged_meeting_stop(capture: &mut ActiveMeetingCapture) {
    capture.report_gap(SourceGap {
        track_id: capture.plan.track_id,
        epoch: capture.plan.source_epoch,
        start_offset_ns: capture.final_offset_ns,
        end_offset_ns: None,
        reason: SourceGapReason::SourceStopped,
        dropped_frames: None,
    });
}

fn run_consumer(inputs: ConsumerInputs) {
    let ConsumerInputs {
        in_sample_rate,
        vad,
        mut lane,
        cmd_rx,
        level_cb,
        audio_cb,
        stream_running_at,
        meeting_control,
    } = inputs;
    let mut frame_resampler = FrameResampler::new(
        sample_rate_to_usize(in_sample_rate),
        sample_rate_to_usize(constants::WHISPER_SAMPLE_RATE),
        Duration::from_millis(30),
    );

    let mut processed_samples = Vec::<f32>::new();
    // Retain a bounded prefix only until VAD confirms speech. The cap prevents
    // an all-silent capture from growing without limit while keeping enough
    // audio to diagnose the source from History.
    let mut silence_until_speech: Option<Vec<f32>> = None;
    let mut recording = false;
    let mut vad_policy = VadPolicy::Offline;

    // ---------- latency instrumentation ---------------------------------- //
    // First arrival exposes the play()->samples-flowing gap; the first-captured
    // log confirms capture begins with the audio already in the lane when
    // Cmd::Start lands.
    let mut first_samples_logged = false;
    let mut awaiting_first_captured_chunk: Option<Instant> = None;
    let mut capture_ready_tx: Option<mpsc::Sender<()>> = None;

    // This buffer is reserved only while a meeting source is active. It bridges
    // a rare wrap in the existing SPSC ring because PacketSink accepts one
    // contiguous packet slice; the callback never uses or grows it.
    let mut wrapped_packet_scratch = Vec::new();
    let mut active_meeting: Option<ActiveMeetingCapture> = None;
    // ---------- spectrum visualisation setup ---------------------------- //
    const BUCKETS: usize = 16;
    // Scale the FFT window to the device sample rate so the analysis window
    // (~33 ms) and frequency resolution (~30 Hz/bin) stay roughly constant
    // across devices. A fixed 512-sample window collapses the low vocal
    // buckets onto a single bin at 48 kHz (e.g. built-in laptop mics), and
    // would stutter at ~4-8 updates/sec on an 8-16 kHz Bluetooth headset.
    // Targets: 48 kHz -> 2048, 16 kHz -> 512, 8 kHz -> 256.
    let target_window =
        sample_rate_to_usize(in_sample_rate / 30 + u32::from(in_sample_rate % 30 >= 15));
    let mut window_size = 256usize;
    for candidate in [512usize, 1024, 2048] {
        if candidate.abs_diff(target_window) < window_size.abs_diff(target_window) {
            window_size = candidate;
        }
    }
    let mut visualizer = AudioVisualiser::new(
        in_sample_rate,
        window_size,
        BUCKETS,
        400.0,  // vocal_min_hz
        4000.0, // vocal_max_hz
    );

    fn handle_frame(
        samples: &[f32],
        vad_policy: VadPolicy,
        vad: &Option<VadConfig>,
        audio_cb: &Option<AudioFrameCallback>,
        out_buf: &mut Vec<f32>,
        silence_until_speech: &mut Option<Vec<f32>>,
    ) {
        fn emit_frame(
            samples: &[f32],
            audio_cb: &Option<AudioFrameCallback>,
            out_buf: &mut Vec<f32>,
            silence_until_speech: &mut Option<Vec<f32>>,
        ) {
            *silence_until_speech = None;
            out_buf.extend_from_slice(samples);
            if let Some(cb) = audio_cb {
                cb(samples);
            }
        }

        if vad_policy == VadPolicy::Disabled {
            emit_frame(samples, audio_cb, out_buf, silence_until_speech);
            return;
        }

        if let Some(cfg) = vad {
            let mut det = lock_recover(&cfg.detector);
            match det.push_frame(samples).unwrap_or(VadFrame::Speech(samples)) {
                VadFrame::Speech(buf) => emit_frame(buf, audio_cb, out_buf, silence_until_speech),
                VadFrame::Noise => {
                    if let Some(silence) = silence_until_speech {
                        retain_no_speech_samples(silence, samples);
                    }
                }
            }
        } else {
            emit_frame(samples, audio_cb, out_buf, silence_until_speech);
        }
    }

    /// Feed one contiguous run of native-rate samples into the recording.
    fn absorb(
        chunk: &[f32],
        vad_policy: VadPolicy,
        vad: &Option<VadConfig>,
        audio_cb: &Option<AudioFrameCallback>,
        frame_resampler: &mut FrameResampler,
        out_buf: &mut Vec<f32>,
        silence_until_speech: &mut Option<Vec<f32>>,
    ) {
        frame_resampler.push(chunk, &mut |frame: &[f32]| {
            handle_frame(
                frame,
                vad_policy,
                vad,
                audio_cb,
                out_buf,
                silence_until_speech,
            )
        });
    }

    // Poll rather than block: the lane has no blocking primitive by design, and
    // commands must keep flowing even when a disconnected device stops producing
    // samples without closing its stream.
    loop {
        // Commands come first so a Start claims the samples already sitting in
        // the lane — the audio the device delivered between the keypress and
        // this iteration. Draining first would discard one buffer period
        // (~10 ms built-in, up to ~100 ms on Bluetooth) at every start.
        loop {
            let cmd = match cmd_rx.try_recv() {
                Ok(cmd) => cmd,
                Err(mpsc::TryRecvError::Empty) => break,
                // The recorder was dropped without close(); nothing can ever
                // command this worker again.
                Err(mpsc::TryRecvError::Disconnected) => return,
            };

            match cmd {
                Cmd::Start(policy, sent_at, ready_tx) => {
                    if active_meeting.is_some() {
                        drop(ready_tx);
                        continue;
                    }
                    // A poisoned lane here means the consumer stalled while
                    // idle, so the lane holds stale pre-keypress audio. Drop it
                    // rather than prepend it to the new recording.
                    if let Some(stale) = lane.overrun(in_sample_rate) {
                        log::warn!("Dropping stale capture backlog before recording: {stale}");
                        lane.clear_overrun();
                    }
                    lane.reset_high_water();

                    log::debug!(
                        "Cmd::Start processed {:?} after send; capture begins with the {} samples already in the lane",
                        sent_at.elapsed(),
                        lane.len()
                    );
                    awaiting_first_captured_chunk = Some(Instant::now());
                    capture_ready_tx = Some(ready_tx);
                    vad_policy = policy;
                    processed_samples.clear();
                    silence_until_speech = (policy != VadPolicy::Disabled).then(Vec::new);
                    recording = true;
                    visualizer.reset();
                    frame_resampler.reset();
                    // Reconfigure the single VAD engine for this session's policy
                    // and clear its smoothing + recurrent state before it sees
                    // any frames.
                    if vad_policy != VadPolicy::Disabled {
                        if let Some(cfg) = &vad {
                            let mut det = lock_recover(&cfg.detector);
                            det.set_hangover_frames(cfg.hangover_for(vad_policy));
                            det.reset();
                        }
                    }
                }
                Cmd::Stop(reply_tx) => {
                    if active_meeting.is_some() {
                        let _ = reply_tx.send(Err(CaptureError::NotCapturing));
                        continue;
                    }
                    recording = false;
                    // If Stop was queued before the first samples, dropping this
                    // sender prevents a stale ready UI event or start chime.
                    capture_ready_tx = None;
                    awaiting_first_captured_chunk = None;

                    // Close the lane, then wait for the device callback to
                    // acknowledge it. That acknowledgement is what guarantees
                    // every captured sample is already in the lane.
                    let acks_before = lane.stop_acks();
                    lane.request_stop();

                    let deadline = Instant::now() + STOP_ACK_TIMEOUT;
                    loop {
                        // Check before draining: observing the acknowledgement
                        // also publishes everything committed ahead of it, so
                        // the drain below cannot miss a sample.
                        let acknowledged = lane.stop_acks() != acks_before;
                        lane.drain(|chunk| {
                            absorb(
                                chunk,
                                vad_policy,
                                &vad,
                                &audio_cb,
                                &mut frame_resampler,
                                &mut processed_samples,
                                &mut silence_until_speech,
                            )
                        });
                        if acknowledged {
                            break;
                        }
                        if Instant::now() >= deadline {
                            log::warn!(
                                "Timed out waiting for the capture callback to acknowledge stop"
                            );
                            break;
                        }
                        std::thread::sleep(STOP_ACK_POLL_INTERVAL);
                    }

                    frame_resampler.finish(&mut |frame: &[f32]| {
                        handle_frame(
                            frame,
                            vad_policy,
                            &vad,
                            &audio_cb,
                            &mut processed_samples,
                            &mut silence_until_speech,
                        )
                    });

                    log::debug!(
                        "capture lane peak occupancy {} of {} samples",
                        lane.high_water(),
                        lane.capacity()
                    );

                    // A sticky overrun means the device produced audio this
                    // lane could not take. The drained samples are a contiguous
                    // prefix before that gap. Return them for WAV persistence,
                    // but make the gap explicit so no caller can auto-decode
                    // them into a plausible, incomplete transcript.
                    let reply = match lane.overrun(in_sample_rate) {
                        Some(overrun) => {
                            log::error!("{overrun}");
                            let prefix_samples = std::mem::take(&mut processed_samples);
                            silence_until_speech = None;
                            lane.clear_overrun();
                            Err(CaptureError::Overrun {
                                overrun,
                                prefix_samples,
                            })
                        }
                        None => {
                            // `silence_until_speech` is live only until VAD
                            // forwards its first speech frame, so a non-empty
                            // buffer here *is* the whole raw clip.
                            let raw_clip = silence_until_speech
                                .take()
                                .filter(|samples| !samples.is_empty());
                            let vad_forwarded_speech = raw_clip.is_none();
                            Ok(RecordedAudio {
                                samples: raw_clip
                                    .unwrap_or_else(|| std::mem::take(&mut processed_samples)),
                                vad_forwarded_speech,
                            })
                        }
                    };
                    let _ = reply_tx.send(reply);

                    // Resume the audio callback so the consumer loop can continue
                    // receiving samples (important for always-on microphone mode).
                    lane.resume();
                }
                Cmd::StartMeeting {
                    plan,
                    anchor,
                    sink,
                    reply,
                } => {
                    if recording || active_meeting.is_some() {
                        let _ = reply.send(Err(MeetingCaptureError::InvalidState));
                        continue;
                    }
                    if plan.source_kind != SourceKind::Microphone {
                        let _ = reply.send(Err(MeetingCaptureError::InvalidFormat));
                        continue;
                    }
                    let required_scratch = lane
                        .capacity()
                        .saturating_sub(wrapped_packet_scratch.capacity());
                    if wrapped_packet_scratch
                        .try_reserve_exact(required_scratch)
                        .is_err()
                    {
                        let _ = reply.send(Err(MeetingCaptureError::StreamFailure));
                        continue;
                    }

                    // Suppress the idle downmix before dropping any pre-start
                    // samples. The callback observes the release store before it
                    // can publish a timed descriptor for this capture.
                    meeting_control.pause();
                    lane.clear_overrun();
                    lane.resume();
                    lane.reset_high_water();
                    let epoch = plan.source_epoch;
                    active_meeting = Some(ActiveMeetingCapture::new(plan, anchor, sink, reply));
                    meeting_control.begin(epoch);
                }
                Cmd::PauseMeeting(reply) => {
                    let Some(capture) = active_meeting.as_mut() else {
                        let _ = reply.send(Err(MeetingCaptureError::InvalidState));
                        continue;
                    };
                    if capture.paused {
                        let _ = reply.send(Err(MeetingCaptureError::InvalidState));
                        continue;
                    }

                    let acknowledged = close_meeting_callback(
                        &mut lane,
                        capture,
                        &mut wrapped_packet_scratch,
                        &meeting_control,
                        in_sample_rate,
                    );
                    capture.paused = true;
                    capture.paused_at_offset_ns = capture.final_offset_ns;
                    if !acknowledged {
                        report_unacknowledged_meeting_stop(capture);
                        capture.fail_start(MeetingCaptureError::StreamFailure);
                    }
                    meeting_control.pause();
                    lane.clear_overrun();
                    lane.resume();
                    let _ = reply.send(if acknowledged {
                        Ok(())
                    } else {
                        Err(MeetingCaptureError::StreamFailure)
                    });
                }
                Cmd::ResumeMeeting { epoch, reply } => {
                    let Some(capture) = active_meeting.as_mut() else {
                        let _ = reply.send(Err(MeetingCaptureError::InvalidState));
                        continue;
                    };
                    if !capture.paused {
                        let _ = reply.send(Err(MeetingCaptureError::InvalidState));
                        continue;
                    }

                    capture.plan.source_epoch = epoch;
                    if let Err(error) = capture.publish_clock_epoch(epoch) {
                        let _ = reply.send(Err(error));
                        continue;
                    }
                    lane.clear_overrun();
                    lane.resume();
                    capture.paused = false;
                    capture.start_reply = Some(reply);
                    meeting_control.resume(epoch);
                }
                Cmd::StopMeeting(reply) => {
                    let Some(mut capture) = active_meeting.take() else {
                        let _ = reply.send(Err(MeetingCaptureError::InvalidState));
                        continue;
                    };
                    let acknowledged = close_meeting_callback(
                        &mut lane,
                        &mut capture,
                        &mut wrapped_packet_scratch,
                        &meeting_control,
                        in_sample_rate,
                    );
                    if !acknowledged {
                        report_unacknowledged_meeting_stop(&mut capture);
                        capture.fail_start(MeetingCaptureError::StreamFailure);
                    }
                    meeting_control.idle();
                    lane.clear_overrun();
                    lane.resume();
                    let _ = reply.send(if acknowledged {
                        Ok(capture.stop_report())
                    } else {
                        Err(MeetingCaptureError::StreamFailure)
                    });
                }
                Cmd::AbortMeeting(reply) => {
                    let result = if let Some(mut capture) = active_meeting.take() {
                        let acknowledged = close_meeting_callback(
                            &mut lane,
                            &mut capture,
                            &mut wrapped_packet_scratch,
                            &meeting_control,
                            in_sample_rate,
                        );
                        if !acknowledged {
                            report_unacknowledged_meeting_stop(&mut capture);
                        }
                        capture.fail_start(MeetingCaptureError::StreamFailure);
                        meeting_control.idle();
                        lane.clear_overrun();
                        lane.resume();
                        if acknowledged {
                            Ok(())
                        } else {
                            Err(MeetingCaptureError::StreamFailure)
                        }
                    } else {
                        Ok(())
                    };
                    let _ = reply.send(result);
                }
                Cmd::Shutdown => {
                    lane.request_stop();
                    return;
                }
            }
        }

        // Dictation keeps its existing VAD path. A meeting skips every
        // resampler, VAD, meter, event, and streaming-ASR callback here; its
        // worker forwards native-rate descriptor/sample pairs to PacketSink.
        let drained = if let Some(capture) = active_meeting.as_mut() {
            let overrun_was_reported = capture.overrun_reported;
            let drained = drain_meeting_lane(&mut lane, capture, &mut wrapped_packet_scratch);
            observe_meeting_lane_overrun(&lane, capture, in_sample_rate);
            if !overrun_was_reported && capture.overrun_reported {
                // The callback has already stopped accepting after the sticky
                // overrun. Request its acknowledgement so no later callback can
                // silently resume past the source gap.
                meeting_control.request_stop();
                lane.request_stop();
            }
            drained
        } else if recording {
            lane.drain(|chunk| {
                if let Some(buckets) = visualizer.feed(chunk) {
                    if let Some(cb) = &level_cb {
                        cb(buckets);
                    }
                }
                absorb(
                    chunk,
                    vad_policy,
                    &vad,
                    &audio_cb,
                    &mut frame_resampler,
                    &mut processed_samples,
                    &mut silence_until_speech,
                );
            })
        } else {
            lane.discard()
        };

        if drained == 0 {
            std::thread::sleep(LANE_POLL_INTERVAL);
            continue;
        }

        let drained_ms = f64::from(u32::try_from(drained).unwrap_or(u32::MAX)) * 1000.0
            / f64::from(in_sample_rate);
        if !first_samples_logged {
            first_samples_logged = true;
            log::debug!(
                "first audio arrived {:?} after stream start ({:.1}ms of audio)",
                stream_running_at.elapsed(),
                drained_ms
            );
        }

        if recording {
            if let Some(started) = awaiting_first_captured_chunk.take() {
                log::debug!(
                    "first captured audio ({:.1}ms) processed {:?} after Cmd::Start",
                    drained_ms,
                    started.elapsed()
                );
            }
            if let Some(ready_tx) = capture_ready_tx.take() {
                // Signal only after these samples have passed through the
                // visualizer and resampler. Silence still counts: readiness
                // means the host is delivering samples, not that VAD has
                // detected speech.
                let _ = ready_tx.send(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        cached_config_for, capture_into_lane, capture_into_timed_lane, capture_lane,
        is_microphone_access_denied, is_no_input_device_error, observe_meeting_packet,
        retain_no_speech_samples, run_consumer, store_config, ActiveMeetingCapture, AudioRecorder,
        CaptureError, CaptureProducer, Cmd, ConsumerInputs, MeetingCallbackControl, PacketSink,
        RecordedAudio, TimedCaptureState, VadConfig, VadPolicy, MAX_NO_SPEECH_HISTORY_SAMPLES,
    };
    use crate::audio_toolkit::vad::{VadFrame, VoiceActivityDetector};
    use crate::meeting::capture::PacketLaneReader;
    use crate::meeting::types::{
        MeetingCaptureError, MeetingSessionId, SessionClockAnchor, SourceEpoch, SourceKind,
        SourceStartPlan, SourceTrackId,
    };
    use cpal::{InputCallbackInfo, InputStreamTimestamp, StreamInstant};
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc, Arc, Mutex,
        },
        thread,
        time::{Duration, Instant},
    };

    /// Counts heap allocations made by the calling thread only, so the
    /// realtime-safety assertions below are not perturbed by the other tests
    /// `cargo test` runs in parallel.
    mod alloc_probe {
        use std::alloc::{GlobalAlloc, Layout, System};
        use std::cell::Cell;

        thread_local! {
            static COUNT: Cell<usize> = const { Cell::new(0) };
        }

        pub fn count() -> usize {
            COUNT.with(|count| count.get())
        }

        fn bump() {
            // `try_with`, not `with`: the allocator also runs while a thread's
            // locals are being destroyed, where `with` would panic.
            let _ = COUNT.try_with(|count| count.set(count.get() + 1));
        }

        pub struct Counting;

        // SAFETY: every method forwards to the system allocator unchanged. The
        // counter is a thread-local `Cell<usize>` with no destructor, so
        // touching it cannot allocate or recurse.
        unsafe impl GlobalAlloc for Counting {
            unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
                bump();
                System.alloc(layout)
            }

            unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
                bump();
                System.alloc_zeroed(layout)
            }

            unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
                bump();
                System.realloc(ptr, layout, new_size)
            }

            unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
                System.dealloc(ptr, layout)
            }
        }
    }

    #[global_allocator]
    static COUNTING_ALLOCATOR: alloc_probe::Counting = alloc_probe::Counting;

    const NATIVE_RATE: u32 = 48_000;

    fn native_rate_samples() -> usize {
        match usize::try_from(NATIVE_RATE) {
            Ok(rate) => rate,
            Err(_) => panic!("test sample rate does not fit usize"),
        }
    }

    #[test]
    fn no_speech_history_audio_is_bounded_to_its_first_samples() {
        let mut retained = Vec::new();
        let mut samples = vec![0.25; MAX_NO_SPEECH_HISTORY_SAMPLES + 512];
        samples[MAX_NO_SPEECH_HISTORY_SAMPLES] = 0.75;
        retain_no_speech_samples(&mut retained, &samples);
        retain_no_speech_samples(&mut retained, &[f32::INFINITY; 512]);

        assert_eq!(retained.len(), MAX_NO_SPEECH_HISTORY_SAMPLES);
        assert_eq!(retained.first(), Some(&0.25));
        assert_eq!(retained.last(), Some(&0.25));
    }
    struct SpeechAfter {
        speech_on_frame: usize,
        frames_seen: usize,
    }

    impl VoiceActivityDetector for SpeechAfter {
        fn push_frame<'a>(&'a mut self, frame: &'a [f32]) -> anyhow::Result<VadFrame<'a>> {
            self.frames_seen += 1;
            if self.frames_seen >= self.speech_on_frame {
                Ok(VadFrame::Speech(frame))
            } else {
                Ok(VadFrame::Noise)
            }
        }

        fn reset(&mut self) {
            self.frames_seen = 0;
        }
    }

    fn deterministic_vad(speech_on_frame: usize) -> VadConfig {
        VadConfig {
            detector: Arc::new(Mutex::new(Box::new(SpeechAfter {
                speech_on_frame,
                frames_seen: 0,
            }))),
            offline_hangover_frames: 0,
            streaming_hangover_frames: 0,
        }
    }

    /// Reports one utterance and then silence, and counts every frame it was
    /// shown. The count is the assertion: a meeting lane that consults the
    /// segmenter at all has taken on dictation's utterance-delimited lifetime,
    /// whatever it then does with the answer.
    ///
    /// `reset` clears nothing, because the count is about the whole run rather
    /// than about one session of it.
    struct CountingVad {
        frames_shown: Arc<AtomicUsize>,
        speech_frames: usize,
    }

    impl VoiceActivityDetector for CountingVad {
        fn push_frame<'a>(&'a mut self, frame: &'a [f32]) -> anyhow::Result<VadFrame<'a>> {
            let shown = self.frames_shown.fetch_add(1, Ordering::AcqRel) + 1;
            if shown <= self.speech_frames {
                Ok(VadFrame::Speech(frame))
            } else {
                Ok(VadFrame::Noise)
            }
        }

        fn reset(&mut self) {}
    }
    /// Reference downmix: the straight-to-`Vec` conversion the capture callback
    /// used before the lane existed. Equivalence against this is what proves the
    /// in-place write produces the same audio.
    fn reference_downmix(data: &[f32], channels: usize, selected: Option<usize>) -> Vec<f32> {
        if channels == 1 {
            return data.to_vec();
        }
        let use_channel = match selected {
            Some(ch) if ch < channels => Some(ch),
            _ => None,
        };
        data.chunks_exact(channels)
            .map(|frame| match use_channel {
                Some(ch) => frame[ch],
                None => {
                    let channels = u16::try_from(channels).unwrap_or(u16::MAX);
                    frame.iter().sum::<f32>() / f32::from(channels)
                }
            })
            .collect()
    }

    fn interleaved(frames: usize, channels: usize, offset: usize) -> Vec<f32> {
        (0..frames * channels)
            .map(|i| {
                let sample = u16::try_from((i + offset) % 977).unwrap_or_default();
                f32::from(sample) / 977.0 - 0.5
            })
            .collect()
    }

    fn callback_info(timestamp_ns: i64) -> InputCallbackInfo {
        let seconds = timestamp_ns / 1_000_000_000;
        let nanoseconds = u32::try_from(timestamp_ns % 1_000_000_000).unwrap_or_default();
        let timestamp = StreamInstant::new(seconds, nanoseconds);
        InputCallbackInfo::new(InputStreamTimestamp {
            callback: timestamp,
            capture: timestamp,
        })
    }

    fn start_with_policy(cmd_tx: &mpsc::Sender<Cmd>, policy: VadPolicy) -> mpsc::Receiver<()> {
        let (ready_tx, ready_rx) = mpsc::channel();
        cmd_tx
            .send(Cmd::Start(policy, Instant::now(), ready_tx))
            .expect("send start");
        ready_rx
    }

    fn start(cmd_tx: &mpsc::Sender<Cmd>) -> mpsc::Receiver<()> {
        start_with_policy(cmd_tx, VadPolicy::Disabled)
    }

    /// Drives the lane the way a device callback does until the stop reply
    /// arrives. The callback itself is what acknowledges a stop, so a test that
    /// blocked on the reply without producing would deadlock.
    fn stop_and_collect_recording(
        cmd_tx: &mpsc::Sender<Cmd>,
        producer: &mut CaptureProducer,
        buffer: &[f32],
    ) -> Result<RecordedAudio, CaptureError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        cmd_tx.send(Cmd::Stop(reply_tx)).expect("send stop");
        for _ in 0..200 {
            capture_into_lane(buffer, 1, None, producer);
            match reply_rx.recv_timeout(Duration::from_millis(20)) {
                Ok(reply) => return reply,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => panic!("consumer dropped the reply"),
            }
        }
        panic!("stop was never answered");
    }

    fn stop_and_collect(
        cmd_tx: &mpsc::Sender<Cmd>,
        producer: &mut CaptureProducer,
        buffer: &[f32],
    ) -> Result<Vec<f32>, CaptureError> {
        stop_and_collect_recording(cmd_tx, producer, buffer).map(|recording| recording.samples)
    }

    #[test]
    fn capture_callback_never_allocates() {
        // Two seconds at 48 kHz: exactly what open() builds.
        let (mut producer, mut consumer) =
            capture_lane::lane(native_rate_samples() * super::LANE_SECONDS);
        let stereo = interleaved(480, 2, 0);

        let before = alloc_probe::count();
        // 190 buffers of 480 frames is 91_200 samples: a realistic two seconds
        // of callbacks that still fits, so this is the accepting path.
        for _ in 0..190 {
            capture_into_lane(&stereo, 2, None, &mut producer);
        }
        assert_eq!(
            alloc_probe::count() - before,
            0,
            "the device callback allocated"
        );

        // The refusal path runs inside the same callback, so it has to be
        // allocation-free too.
        consumer.discard();
        let oversized = interleaved(native_rate_samples() * 3, 1, 0);
        let before = alloc_probe::count();
        capture_into_lane(&oversized, 1, None, &mut producer);
        for _ in 0..50 {
            capture_into_lane(&stereo, 2, None, &mut producer);
        }
        assert_eq!(
            alloc_probe::count() - before,
            0,
            "the callback allocated while refusing samples"
        );
        assert!(consumer.overrun(48_000).is_some());
    }

    #[test]
    fn timed_capture_callback_never_allocates() {
        let (mut producer, mut consumer) = capture_lane::timed_lane_with_descriptor_capacity(
            native_rate_samples() * super::LANE_SECONDS,
            native_rate_samples() * super::LANE_SECONDS,
        );
        let control = MeetingCallbackControl::new();
        control.begin(SourceEpoch::new(1));
        let callback_info = callback_info(1_000_000);
        let stereo = interleaved(480, 2, 0);

        let before = alloc_probe::count();
        for _ in 0..190 {
            capture_into_timed_lane(
                &stereo,
                &callback_info,
                TimedCaptureState {
                    channels: 2,
                    use_channel: None,
                    sample_rate: NATIVE_RATE,
                    meeting_control: &control,
                    last_timestamp_value: &mut None,
                    producer: &mut producer,
                },
            );
        }
        assert_eq!(
            alloc_probe::count() - before,
            0,
            "the timed device callback allocated"
        );

        consumer.discard_timed();
        let oversized = interleaved(native_rate_samples() * 3, 1, 0);
        let before = alloc_probe::count();
        capture_into_timed_lane(
            &oversized,
            &callback_info,
            TimedCaptureState {
                channels: 1,
                use_channel: None,
                sample_rate: NATIVE_RATE,
                meeting_control: &control,
                last_timestamp_value: &mut None,
                producer: &mut producer,
            },
        );
        for _ in 0..50 {
            capture_into_timed_lane(
                &stereo,
                &callback_info,
                TimedCaptureState {
                    channels: 2,
                    use_channel: None,
                    sample_rate: NATIVE_RATE,
                    meeting_control: &control,
                    last_timestamp_value: &mut None,
                    producer: &mut producer,
                },
            );
        }
        assert_eq!(
            alloc_probe::count() - before,
            0,
            "the timed callback allocated while refusing samples"
        );
        assert!(consumer.timed_overrun(NATIVE_RATE).is_some());
    }

    #[test]
    fn initial_untimestamped_packet_extends_the_real_capture_clock() {
        let track_id = SourceTrackId::new();
        let (sink, mut reader) = PacketSink::new(track_id, 1_024, 2);
        let (start_reply, start_result) = mpsc::channel();
        let session_anchor_ns = 5_000_000_000;
        let mut capture = ActiveMeetingCapture::new(
            SourceStartPlan {
                session_id: MeetingSessionId::new(),
                track_id,
                source_kind: SourceKind::Microphone,
                required: true,
                frozen_application_bundle_ids: Vec::new(),
                source_epoch: SourceEpoch::new(0),
            },
            SessionClockAnchor {
                host_monotonic_anchor_ns: session_anchor_ns,
                wall_start_utc_ms: 0,
                clock_policy_version: 1,
            },
            sink,
            start_reply,
        );
        let frame_count = 512;
        let packet_duration_ns = i64::from(frame_count) * 1_000_000_000 / i64::from(NATIVE_RATE);
        let samples = vec![0.25; usize::try_from(frame_count).unwrap()];
        let descriptor = |sequence, timestamp: Option<i64>| capture_lane::CaptureDescriptor {
            sequence,
            source_epoch: 0,
            native_timestamp_value: timestamp.unwrap_or_default(),
            native_timestamp_timescale: timestamp.map_or(0, |_| 1_000_000_000),
            host_monotonic_anchor_ns: timestamp
                .and_then(|value| u64::try_from(value).ok())
                .and_then(|value| session_anchor_ns.checked_add(value)),
            format_epoch: 1,
            frame_start: 0,
            frame_count,
            sample_rate: NATIVE_RATE,
            channels: 1,
            sample_format: capture_lane::CaptureSampleFormat::F32,
            flags: timestamp.map_or(capture_lane::TIMESTAMP_MISSING, |_| 0),
        };

        observe_meeting_packet(&mut capture, descriptor(0, None), &samples);
        assert!(matches!(
            start_result.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        observe_meeting_packet(
            &mut capture,
            descriptor(1, Some(packet_duration_ns)),
            &samples,
        );

        let report = start_result.recv().unwrap().unwrap();
        assert_eq!(report.timestamp_bridge.native_anchor_value, 0);
        assert_eq!(
            report.timestamp_bridge.host_monotonic_anchor_ns,
            session_anchor_ns
        );
        assert_eq!(report.timestamp_bridge.session_offset_ns, 0);
        assert!(reader.pop_gap().is_none());

        let mut stored_samples = Vec::new();
        let prefix = reader.pop_into(&mut stored_samples).unwrap().unwrap();
        assert_eq!(prefix.sequence, 0);
        assert_eq!(prefix.native_timestamp_value, None);
        let timestamped = reader.pop_into(&mut stored_samples).unwrap().unwrap();
        assert_eq!(timestamped.sequence, 1);
        assert_eq!(timestamped.native_timestamp_value, Some(packet_duration_ns));
    }

    #[test]
    fn lane_capture_matches_the_direct_downmix() {
        for channels in [1usize, 2, 4] {
            for selected in [None, Some(1usize), Some(9usize)] {
                let use_channel = match selected {
                    Some(ch) if ch < channels => Some(ch),
                    _ => None,
                };
                // Capacity deliberately not a multiple of the buffer size, so
                // later writes and reads straddle the end of the lane.
                let (mut producer, mut consumer) = capture_lane::lane(1_000);
                let mut expected = Vec::new();
                let mut captured = Vec::new();

                for round in 0..7 {
                    let data = interleaved(137, channels, round * 31);
                    expected.extend_from_slice(&reference_downmix(&data, channels, selected));
                    capture_into_lane(&data, channels, use_channel, &mut producer);
                    assert!(
                        consumer.overrun(NATIVE_RATE).is_none(),
                        "{channels}ch/{selected:?} overran a lane with room to spare"
                    );
                    consumer.drain(|chunk| captured.extend_from_slice(chunk));
                }

                assert_eq!(
                    captured, expected,
                    "lane capture diverged from the direct downmix for {channels}ch/{selected:?}"
                );
            }
        }
    }

    #[test]
    fn lane_capture_matches_the_direct_downmix_across_a_wrap() {
        // Read and write positions chosen so one drain returns two slices.
        let (mut producer, mut consumer) = capture_lane::lane(1_000);
        let warmup = interleaved(900, 1, 0);
        capture_into_lane(&warmup, 1, None, &mut producer);
        assert_eq!(consumer.discard(), 900);

        let wrapping = interleaved(300, 2, 5);
        capture_into_lane(&wrapping, 2, None, &mut producer);

        let mut slices = 0usize;
        let mut captured = Vec::new();
        consumer.drain(|chunk| {
            slices += 1;
            captured.extend_from_slice(chunk);
        });
        assert_eq!(slices, 2, "the drain did not straddle the end of the lane");
        assert_eq!(captured, reference_downmix(&wrapping, 2, None));
    }

    #[test]
    fn lane_overflow_poisons_the_lane_and_reports_the_loss() {
        let (mut producer, mut consumer) = capture_lane::lane(1_000);

        // Fill it exactly. High-water must see a full lane.
        let full = interleaved(1_000, 1, 0);
        capture_into_lane(&full, 1, None, &mut producer);
        assert!(consumer.overrun(16_000).is_none());
        assert_eq!(consumer.high_water(), 1_000);

        // The next buffer cannot fit.
        let refused = interleaved(400, 1, 0);
        capture_into_lane(&refused, 1, None, &mut producer);
        let overrun = consumer.overrun(16_000).expect("overrun recorded");
        assert_eq!(overrun.lost_samples, 400);
        assert_eq!(overrun.refused_buffers, 1);
        assert_eq!(overrun.capacity_samples, 1_000);
        assert_eq!(overrun.sample_rate, 16_000);

        // Sticky: even with the whole lane free again the producer refuses,
        // rather than resuming mid-recording on the far side of a gap.
        assert_eq!(consumer.discard(), 1_000);
        capture_into_lane(&refused, 1, None, &mut producer);
        let overrun = consumer.overrun(16_000).expect("still poisoned");
        assert_eq!(overrun.lost_samples, 800);
        assert_eq!(overrun.refused_buffers, 2);
        assert_eq!(consumer.len(), 0, "a poisoned lane accepted samples");

        // Clearing it discards the backlog and lets capture resume.
        consumer.clear_overrun();
        assert!(consumer.overrun(16_000).is_none());
        capture_into_lane(&refused, 1, None, &mut producer);
        assert!(consumer.overrun(16_000).is_none());
        assert_eq!(consumer.len(), 400);
    }

    #[test]
    fn high_water_tracks_the_peak_and_resets() {
        let (mut producer, consumer) = capture_lane::lane(1_000);
        capture_into_lane(&interleaved(300, 1, 0), 1, None, &mut producer);
        assert_eq!(consumer.high_water(), 300);
        capture_into_lane(&interleaved(120, 1, 0), 1, None, &mut producer);
        assert_eq!(consumer.high_water(), 420);
        consumer.reset_high_water();
        assert_eq!(consumer.high_water(), 0);
    }

    #[test]
    fn unopened_recorder_does_not_need_reopen() {
        // No worker has been spawned yet, so there is nothing to reap. Guards
        // against inverting the "no worker" case, which would make every first
        // open() take the rebuild path.
        let recorder = AudioRecorder::new().expect("recorder");
        assert!(!recorder.needs_reopen());
    }

    #[test]
    fn stream_error_requires_reopen() {
        let recorder = AudioRecorder::new().expect("recorder");
        recorder.stream_error.store(true, Ordering::Relaxed);
        assert!(recorder.needs_reopen());
    }

    fn test_config(sample_rate: u32) -> cpal::SupportedStreamConfig {
        cpal::SupportedStreamConfig::new(
            1,
            cpal::SampleRate(sample_rate),
            cpal::SupportedBufferSize::Range { min: 15, max: 4096 },
            cpal::SampleFormat::F32,
        )
    }

    /// The cache is keyed by device name, and the prewarm now writes it before
    /// any open has confirmed the config, so a wrong hit is no longer caught by
    /// a failed build on the same device.
    #[test]
    fn a_cached_config_is_reused_only_for_the_device_that_reported_it() {
        let cache = Mutex::new(None);
        store_config(&cache, "LG UltraFine".to_string(), test_config(48_000));

        assert_eq!(
            cached_config_for(&cache, "LG UltraFine").map(|c| c.sample_rate().0),
            Some(48_000)
        );
        assert!(cached_config_for(&cache, "MacBook Pro Microphone").is_none());
    }

    /// cpal reports an empty name for a device whose name query fails. Caching
    /// under it would make every such device share one rate and format, so an
    /// unnamed device must neither fill the cache nor read from it.
    #[test]
    fn an_unnamed_device_neither_fills_nor_reads_the_config_cache() {
        let cache = Mutex::new(None);
        store_config(&cache, String::new(), test_config(44_100));
        assert!(cached_config_for(&cache, "").is_none());

        store_config(&cache, "LG UltraFine".to_string(), test_config(48_000));
        assert!(
            cached_config_for(&cache, "").is_none(),
            "an unnamed device matched another device's cached config"
        );
    }

    /// A prewarmed config makes the next open cheaper; it must never be
    /// mistaken for an open stream. Readiness, the start chime, and the forced
    /// mute all hang off a capture session, and there is no session to start
    /// until `open()` has actually built one.
    #[test]
    fn a_prewarmed_config_does_not_make_the_recorder_capturable() {
        let recorder = AudioRecorder::new().expect("recorder");
        store_config(
            &recorder.config_cache,
            "LG UltraFine".to_string(),
            test_config(48_000),
        );

        assert!(
            recorder.start(VadPolicy::Offline).is_err(),
            "a prewarmed config was treated as an open capture stream"
        );
        assert!(matches!(recorder.stop(), Err(CaptureError::NotCapturing)));
        assert!(!recorder.needs_reopen());
    }

    #[test]
    fn shutdown_is_processed_without_audio_samples() {
        let (_producer, consumer) = capture_lane::lane(native_rate_samples());
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            run_consumer(ConsumerInputs {
                in_sample_rate: NATIVE_RATE,
                vad: None,
                lane: consumer,
                cmd_rx,
                level_cb: None,
                audio_cb: None,
                stream_running_at: Instant::now(),
                meeting_control: Arc::new(MeetingCallbackControl::new()),
            });
            let _ = done_tx.send(());
        });

        cmd_tx.send(Cmd::Shutdown).expect("send shutdown");
        let stopped = done_rx.recv_timeout(Duration::from_secs(1));

        worker.join().expect("join consumer");
        assert!(stopped.is_ok(), "shutdown waited for an audio sample");
    }

    #[test]
    fn consumer_returns_captured_audio_and_tears_down() {
        let (mut producer, consumer) =
            capture_lane::lane(native_rate_samples() * super::LANE_SECONDS);
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            run_consumer(ConsumerInputs {
                in_sample_rate: NATIVE_RATE,
                vad: None,
                lane: consumer,
                cmd_rx,
                level_cb: None,
                audio_cb: None,
                stream_running_at: Instant::now(),
                meeting_control: Arc::new(MeetingCallbackControl::new()),
            });
        });

        let ready = start(&cmd_tx);
        // 480 frames is 10 ms at 48 kHz, so 40 buffers is 400 ms of audio.
        let buffer = interleaved(480, 1, 0);
        for _ in 0..40 {
            capture_into_lane(&buffer, 1, None, &mut producer);
            thread::sleep(Duration::from_millis(2));
        }
        assert!(
            ready.recv_timeout(Duration::from_secs(2)).is_ok(),
            "readiness was never signalled"
        );

        let samples = stop_and_collect(&cmd_tx, &mut producer, &buffer).expect("clean capture");
        // 400 ms at 48 kHz resamples to ~6400 samples at 16 kHz.
        assert!(
            samples.len() > 4_000,
            "expected ~400ms of 16 kHz audio, got {} samples",
            samples.len()
        );

        // Teardown: the worker exits on Shutdown even with the producer still
        // pushing, and it leaves the lane closed.
        cmd_tx.send(Cmd::Shutdown).expect("send shutdown");
        worker.join().expect("join consumer");
        capture_into_lane(&buffer, 1, None, &mut producer);
    }

    /// A meeting's microphone lane ends when the meeting is stopped, and
    /// nothing about speech starting or stopping may end it.
    ///
    /// Dictation's lane is utterance-delimited by design — a closed segment is
    /// the end of that run — and the two lanes are the same cpal stream taken
    /// in turn. What keeps the two lifetimes apart is that the meeting arm of
    /// the consumer never reaches the segmenter at all, so there is no segment
    /// on it to close. Both halves of that are asserted here, because either
    /// one failing brings the defect back: a capture that ended when the room
    /// went quiet, with a transcript of three fragments and forty seconds of
    /// audio nobody kept.
    #[test]
    fn a_closed_speech_segment_cannot_end_a_meeting_capture() {
        let frames_shown = Arc::new(AtomicUsize::new(0));
        let (mut producer, consumer) = capture_lane::timed_lane_with_descriptor_capacity(
            native_rate_samples() * super::LANE_SECONDS,
            native_rate_samples() * super::LANE_SECONDS,
        );
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let meeting_control = Arc::new(MeetingCallbackControl::new());
        let callback_control = Arc::clone(&meeting_control);
        let vad = VadConfig {
            detector: Arc::new(Mutex::new(Box::new(CountingVad {
                frames_shown: Arc::clone(&frames_shown),
                speech_frames: 8,
            }))),
            offline_hangover_frames: 0,
            streaming_hangover_frames: 0,
        };
        let worker = thread::spawn(move || {
            run_consumer(ConsumerInputs {
                in_sample_rate: NATIVE_RATE,
                vad: Some(vad),
                lane: consumer,
                cmd_rx,
                level_cb: None,
                audio_cb: None,
                stream_running_at: Instant::now(),
                meeting_control,
            });
        });

        let track_id = SourceTrackId::new();
        let (sink, mut reader) = PacketSink::new(track_id, native_rate_samples(), 256);
        let (start_reply, start_result) = mpsc::channel();
        let plan = |epoch| SourceStartPlan {
            session_id: MeetingSessionId::new(),
            track_id,
            source_kind: SourceKind::Microphone,
            required: true,
            frozen_application_bundle_ids: Vec::new(),
            source_epoch: SourceEpoch::new(epoch),
        };
        let anchor = SessionClockAnchor {
            host_monotonic_anchor_ns: 0,
            wall_start_utc_ms: 0,
            clock_policy_version: 1,
        };
        cmd_tx
            .send(Cmd::StartMeeting {
                plan: plan(1),
                anchor,
                sink,
                reply: start_reply,
            })
            .expect("send start meeting");

        // Silence is buffers of zeros, not an absent callback: a quiet room
        // still delivers audio, which is the whole reason a segmenter is what
        // decides an utterance has ended. 480 frames is 10 ms at 48 kHz.
        let speech = interleaved(480, 1, 0);
        let silence = vec![0.0_f32; 480];
        let mut timestamp_ns = 1_000_000_i64;
        let mut last_timestamp = None;
        let mut pump = |producer: &mut CaptureProducer, buffer: &[f32], buffers: usize| {
            for _ in 0..buffers {
                timestamp_ns += 10_000_000;
                capture_into_timed_lane(
                    buffer,
                    &callback_info(timestamp_ns),
                    TimedCaptureState {
                        channels: 1,
                        use_channel: None,
                        sample_rate: NATIVE_RATE,
                        meeting_control: &callback_control,
                        last_timestamp_value: &mut last_timestamp,
                        producer,
                    },
                );
                thread::sleep(Duration::from_millis(1));
            }
        };
        let mut scratch = Vec::new();
        let mut drain = |reader: &mut PacketLaneReader| {
            let mut packets = 0;
            while let Ok(Some(_)) = reader.pop_into(&mut scratch) {
                packets += 1;
            }
            packets
        };

        pump(&mut producer, &speech, 8);
        assert!(
            start_result.recv_timeout(Duration::from_secs(2)).is_ok(),
            "the meeting source never acknowledged its start"
        );
        assert!(drain(&mut reader) > 0, "no audio reached the meeting lane");

        // Six hundred milliseconds of it, two orders of magnitude past any
        // hangover tail this recorder configures.
        pump(&mut producer, &silence, 60);
        let through_silence = drain(&mut reader);
        pump(&mut producer, &speech, 8);
        let after_silence = through_silence + drain(&mut reader);

        assert_eq!(
            frames_shown.load(Ordering::Acquire),
            0,
            "the meeting lane consulted the segmenter, so a closed segment can reach it"
        );
        assert!(
            after_silence > 0,
            "the meeting lane stopped carrying audio once the room went quiet"
        );
        assert!(
            reader.pop_gap().is_none() && !reader.take_gap_overflow(),
            "the silence was reported as a break in the capture"
        );

        // The capture is still the consumer's: a second start is refused only
        // while one is held, so this reads the state the silence would have
        // cleared without opening that state up for a test.
        let (second_reply, second_result) = mpsc::channel();
        let (second_sink, _second_reader) = PacketSink::new(SourceTrackId::new(), 16, 1);
        cmd_tx
            .send(Cmd::StartMeeting {
                plan: plan(2),
                anchor,
                sink: second_sink,
                reply: second_reply,
            })
            .expect("send second start meeting");
        assert!(
            matches!(
                second_result.recv_timeout(Duration::from_secs(2)),
                Ok(Err(MeetingCaptureError::InvalidState))
            ),
            "the capture was gone by the time the room spoke again"
        );

        cmd_tx.send(Cmd::Shutdown).expect("send shutdown");
        worker.join().expect("join consumer");
    }

    /// The overlay's listening state, the start chime, and the forced mute all
    /// hang off this one signal, so an early send would tell the user to speak
    /// into a stream that is not yet delivering audio — exactly the head-loss
    /// this gate exists to prevent.
    #[test]
    fn readiness_is_withheld_until_the_input_stream_delivers_its_first_buffer() {
        let (mut producer, consumer) =
            capture_lane::lane(native_rate_samples() * super::LANE_SECONDS);
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            run_consumer(ConsumerInputs {
                in_sample_rate: NATIVE_RATE,
                vad: None,
                lane: consumer,
                cmd_rx,
                level_cb: None,
                audio_cb: None,
                stream_running_at: Instant::now(),
                meeting_control: Arc::new(MeetingCallbackControl::new()),
            });
        });

        let ready = start(&cmd_tx);
        // Cmd::Start has been accepted and the consumer is polling, but the
        // device callback has not produced anything yet.
        assert_eq!(
            ready.recv_timeout(Duration::from_millis(200)),
            Err(mpsc::RecvTimeoutError::Timeout),
            "readiness was asserted before the input stream delivered a buffer"
        );

        capture_into_lane(&interleaved(480, 1, 0), 1, None, &mut producer);
        assert!(
            ready.recv_timeout(Duration::from_secs(2)).is_ok(),
            "readiness was not asserted once the first buffer arrived"
        );

        cmd_tx.send(Cmd::Shutdown).expect("send shutdown");
        worker.join().expect("join consumer");
    }
    #[test]
    fn vad_silence_buffer_does_not_drop_the_detected_speech_onset() {
        let (mut producer, consumer) = capture_lane::lane(16_000 * super::LANE_SECONDS);
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            run_consumer(ConsumerInputs {
                in_sample_rate: 16_000,
                vad: Some(deterministic_vad(2)),
                lane: consumer,
                cmd_rx,
                level_cb: None,
                audio_cb: None,
                stream_running_at: Instant::now(),
                meeting_control: Arc::new(MeetingCallbackControl::new()),
            });
        });

        let ready = start_with_policy(&cmd_tx, VadPolicy::Offline);
        let silence = vec![0.0; 480];
        capture_into_lane(&silence, 1, None, &mut producer);
        assert!(
            ready.recv_timeout(Duration::from_secs(1)).is_ok(),
            "consumer did not process the initial silent frame"
        );

        let onset = vec![0.75; 480];
        capture_into_lane(&onset, 1, None, &mut producer);
        let recording =
            stop_and_collect_recording(&cmd_tx, &mut producer, &onset).expect("clean VAD capture");

        assert!(recording.vad_forwarded_speech);
        assert!(
            recording
                .samples
                .first()
                .is_some_and(|sample| *sample > 0.5),
            "the first emitted sample must be from the detected speech frame"
        );

        cmd_tx.send(Cmd::Shutdown).expect("send shutdown");
        worker.join().expect("join consumer");
    }

    #[test]
    fn all_silence_returns_the_bounded_raw_clip_without_forwarding_engine_audio() {
        let (mut producer, consumer) = capture_lane::lane(16_000 * super::LANE_SECONDS);
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let forwarded = Arc::new(AtomicUsize::new(0));
        let forwarded_for_callback = Arc::clone(&forwarded);
        let worker = thread::spawn(move || {
            run_consumer(ConsumerInputs {
                in_sample_rate: 16_000,
                vad: Some(deterministic_vad(usize::MAX)),
                lane: consumer,
                cmd_rx,
                level_cb: None,
                audio_cb: Some(Arc::new(move |_| {
                    forwarded_for_callback.fetch_add(1, Ordering::AcqRel);
                })),
                stream_running_at: Instant::now(),
                meeting_control: Arc::new(MeetingCallbackControl::new()),
            });
        });

        let ready = start_with_policy(&cmd_tx, VadPolicy::Offline);
        let silence = vec![0.0; 480];
        capture_into_lane(&silence, 1, None, &mut producer);
        assert!(
            ready.recv_timeout(Duration::from_secs(1)).is_ok(),
            "consumer did not process the silent frame"
        );

        let recording = stop_and_collect_recording(&cmd_tx, &mut producer, &silence)
            .expect("clean silent capture");
        assert!(!recording.vad_forwarded_speech);
        assert!(!recording.samples.is_empty());
        assert!(recording.samples.len() <= MAX_NO_SPEECH_HISTORY_SAMPLES);
        assert_eq!(forwarded.load(Ordering::Acquire), 0);

        cmd_tx.send(Cmd::Shutdown).expect("send shutdown");
        worker.join().expect("join consumer");
    }
    #[test]
    fn an_overrun_recording_is_reported_and_the_next_one_starts_clean() {
        // A tenth of a second of lane, so one flood buffer cannot possibly fit.
        let (mut producer, consumer) = capture_lane::lane(native_rate_samples() / 10);
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            run_consumer(ConsumerInputs {
                in_sample_rate: NATIVE_RATE,
                vad: None,
                lane: consumer,
                cmd_rx,
                level_cb: None,
                audio_cb: None,
                stream_running_at: Instant::now(),
                meeting_control: Arc::new(MeetingCallbackControl::new()),
            });
        });

        let buffer = interleaved(480, 1, 0);
        let ready = start(&cmd_tx);
        for _ in 0..20 {
            capture_into_lane(&buffer, 1, None, &mut producer);
            thread::sleep(Duration::from_millis(2));
        }
        assert!(
            ready.recv_timeout(Duration::from_secs(2)).is_ok(),
            "the prefix capture never became ready"
        );
        // One buffer larger than the whole lane: refused outright, so the
        // outcome does not depend on how fast the consumer drains.
        let flood = interleaved(native_rate_samples(), 1, 0);
        capture_into_lane(&flood, 1, None, &mut producer);

        let CaptureError::Overrun {
            overrun,
            prefix_samples,
        } = stop_and_collect(&cmd_tx, &mut producer, &buffer)
            .expect_err("a lost buffer must report the contiguous prefix")
        else {
            panic!("expected capture overrun");
        };
        // At least the flood, and possibly the callbacks that land while the
        // stop is in flight: a poisoned lane keeps counting what it drops.
        assert!(
            overrun.lost_samples >= native_rate_samples(),
            "under-reported the loss: {overrun}"
        );
        assert!(overrun.refused_buffers >= 1);
        assert_eq!(overrun.capacity_samples, native_rate_samples() / 10);
        assert!(
            !prefix_samples.is_empty(),
            "the clean audio before the loss was not preserved"
        );

        // The failure must not persist: the next recording is delivered.
        let ready = start(&cmd_tx);
        for _ in 0..20 {
            capture_into_lane(&buffer, 1, None, &mut producer);
            thread::sleep(Duration::from_millis(2));
        }
        assert!(
            ready.recv_timeout(Duration::from_secs(2)).is_ok(),
            "the recording after an overrun never became ready"
        );
        let samples = stop_and_collect(&cmd_tx, &mut producer, &buffer).expect("recovered capture");
        assert!(
            !samples.is_empty(),
            "the recording after an overrun returned nothing"
        );

        cmd_tx.send(Cmd::Shutdown).expect("send shutdown");
        worker.join().expect("join consumer");
    }

    #[test]
    fn detects_access_is_denied() {
        assert!(is_microphone_access_denied("Access is denied"));
    }

    #[test]
    fn detects_permission_denied() {
        assert!(is_microphone_access_denied("permission denied"));
    }

    #[test]
    fn detects_windows_error_code() {
        assert!(is_microphone_access_denied("WASAPI error: 0x80070005"));
    }

    #[test]
    fn does_not_match_unrelated_errors() {
        assert!(!is_microphone_access_denied("device not found"));
    }

    #[test]
    fn detects_no_input_device() {
        assert!(is_no_input_device_error("No input device found"));
    }

    #[test]
    fn detects_coreaudio_config_error() {
        assert!(is_no_input_device_error(
            "Failed to fetch preferred config: A backend-specific error has occurred: An unknown error unknown to the coreaudio-rs API occurred"
        ));
    }

    #[test]
    fn does_not_match_other_errors_for_no_device() {
        assert!(!is_no_input_device_error("permission denied"));
        assert!(!is_no_input_device_error("device not found"));
    }
}

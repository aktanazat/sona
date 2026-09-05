use crate::managers::audio::{AudioRecordingManager, RecorderMicrophoneLease};
use crate::managers::history::HistoryManager;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tauri::AppHandle;
use tauri_specta::Event as _;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum RecorderPhase {
    Checking,
    Permission,
    Idle,
    SelectingSource,
    Previewing,
    Starting,
    Recording,
    Paused,
    Finalizing,
    Saved,
    Failed,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
pub struct RecorderDevice {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum RecorderAvailability {
    Supported,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum RecorderStartAvailability {
    Ready,
    CaptureBusy,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RecorderPreflight {
    pub availability: RecorderAvailability,
    pub start_availability: RecorderStartAvailability,
    pub camera_devices: Vec<RecorderDevice>,
    pub microphone_devices: Vec<RecorderDevice>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RecorderStartRequest {
    pub camera_enabled: bool,
    pub camera_device_id: Option<String>,
    pub microphone_enabled: bool,
    pub microphone_device_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum RecorderFailureCode {
    Unsupported,
    CaptureBusy,
    ScreenPermissionDenied,
    CameraPermissionDenied,
    MicrophonePermissionDenied,
    SourceSelectionCancelled,
    SourceUnavailable,
    CameraUnavailable,
    MicrophoneUnavailable,
    StreamFailed,
    TimestampDiscontinuity,
    WriterFailed,
    OutputFinalizeFailed,
    OutputCommitFailed,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RecorderSnapshot {
    pub phase: RecorderPhase,
    pub elapsed_ms: u64,
    pub screen_selected: bool,
    pub dropped_video_frames: u64,
    pub output_path: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub failure: Option<RecorderFailureCode>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum RecorderCommandError {
    InvalidState,
}

#[derive(Clone, Debug, Serialize, Type, tauri_specta::Event)]
pub struct RecorderStateChangedEvent {
    pub snapshot: RecorderSnapshot,
}

#[derive(Default)]
pub(crate) struct RecorderNativeStatus {
    failure: AtomicI32,
    dropped_video_frames: AtomicU64,
}

impl RecorderNativeStatus {
    pub(crate) fn record(&self, status: i32, dropped_video_frames: u64) {
        self.dropped_video_frames
            .fetch_max(dropped_video_frames, Ordering::Release);
        if status != 5 {
            let _ = self
                .failure
                .compare_exchange(0, status, Ordering::AcqRel, Ordering::Acquire);
        }
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn take_failure(&self) -> Option<i32> {
        let status = self.failure.swap(0, Ordering::AcqRel);
        (status != 0).then_some(status)
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn dropped_video_frames(&self) -> u64 {
        self.dropped_video_frames.load(Ordering::Acquire)
    }
}

#[cfg(target_os = "macos")]
type NativeRecorder = crate::recorder_macos::NativeRecorder;

#[cfg(not(target_os = "macos"))]
struct NativeRecorder;

struct RecorderState {
    phase: RecorderPhase,
    active_since: Option<Instant>,
    elapsed_before_active: Duration,
    screen_selected: bool,
    dropped_video_frames: u64,
    output_path: Option<PathBuf>,
    width: Option<u32>,
    height: Option<u32>,
    failure: Option<RecorderFailureCode>,
    partial_path: Option<PathBuf>,
    final_path: Option<PathBuf>,
    native: Option<NativeRecorder>,
    microphone_lease: Option<RecorderMicrophoneLease>,
}

impl Default for RecorderState {
    fn default() -> Self {
        Self {
            phase: RecorderPhase::Idle,
            active_since: None,
            elapsed_before_active: Duration::ZERO,
            screen_selected: false,
            dropped_video_frames: 0,
            output_path: None,
            width: None,
            height: None,
            failure: None,
            partial_path: None,
            final_path: None,
            native: None,
            microphone_lease: None,
        }
    }
}

pub struct ScreenRecorderManager {
    app: AppHandle,
    recordings_dir: PathBuf,
    audio: Arc<AudioRecordingManager>,
    state: Mutex<RecorderState>,
    heartbeat_stop: AtomicBool,
    heartbeat: Mutex<Option<JoinHandle<()>>>,
}

impl ScreenRecorderManager {
    pub fn new(
        app: &AppHandle,
        history: Arc<HistoryManager>,
        audio: Arc<AudioRecordingManager>,
    ) -> Arc<Self> {
        let manager = Arc::new(Self {
            app: app.clone(),
            recordings_dir: history.recordings_dir().to_path_buf(),
            audio,
            state: Mutex::new(RecorderState::default()),
            heartbeat_stop: AtomicBool::new(false),
            heartbeat: Mutex::new(None),
        });
        manager.start_heartbeat();
        manager
    }

    pub fn preflight(&self) -> RecorderPreflight {
        let checking = {
            let mut state = lock_recover(&self.state);
            if state.phase == RecorderPhase::Idle {
                state.phase = RecorderPhase::Checking;
                Some(self.snapshot_locked(&state))
            } else {
                None
            }
        };
        if let Some(snapshot) = checking {
            self.emit(snapshot);
        }

        let start_availability = if self.audio.capture_lease_is_active() {
            RecorderStartAvailability::CaptureBusy
        } else {
            RecorderStartAvailability::Ready
        };
        let preflight = match native_preflight() {
            Ok(native) => RecorderPreflight {
                availability: native.availability,
                start_availability,
                camera_devices: native.camera_devices,
                microphone_devices: native.microphone_devices,
            },
            Err(_) => RecorderPreflight {
                availability: RecorderAvailability::Unsupported,
                start_availability,
                camera_devices: Vec::new(),
                microphone_devices: Vec::new(),
            },
        };

        let idle = {
            let mut state = lock_recover(&self.state);
            if state.phase == RecorderPhase::Checking {
                state.phase = RecorderPhase::Idle;
                Some(self.snapshot_locked(&state))
            } else {
                None
            }
        };
        if let Some(snapshot) = idle {
            self.emit(snapshot);
        }
        preflight
    }

    pub fn preview_start(
        &self,
        request: RecorderStartRequest,
    ) -> Result<RecorderSnapshot, RecorderCommandError> {
        self.apply_native_failure();
        let selecting = {
            let mut state = lock_recover(&self.state);
            if !matches!(
                state.phase,
                RecorderPhase::Idle
                    | RecorderPhase::Saved
                    | RecorderPhase::Failed
                    | RecorderPhase::Permission
            ) {
                return Err(RecorderCommandError::InvalidState);
            }
            reset_for_preview(&mut state);
            state.phase = RecorderPhase::SelectingSource;
            self.snapshot_locked(&state)
        };
        self.emit(selecting);

        match native_preview_start(&request) {
            Ok(mut native) => {
                // The lease is taken here, not before the picker: the picker can sit open for
                // minutes, and holding the microphone across it would lock dictation and meetings
                // out of a device this recording may never use. Preflight already reports
                // CaptureBusy before the sheet appears.
                let microphone_lease = if request.microphone_enabled {
                    match self.audio.try_acquire_recorder_microphone() {
                        Some(lease) => Some(lease),
                        None => {
                            native_cancel(&mut native);
                            drop(native);
                            return Ok(self.fail(RecorderFailureCode::CaptureBusy));
                        }
                    }
                } else {
                    None
                };
                let snapshot = {
                    let mut state = lock_recover(&self.state);
                    // The picker blocked with the lock released. If a cancel moved the recorder
                    // out of SelectingSource meanwhile, honor it: entering Previewing here would
                    // leave a live capture nobody asked for.
                    if state.phase != RecorderPhase::SelectingSource {
                        drop(state);
                        native_cancel(&mut native);
                        drop(native);
                        drop(microphone_lease);
                        return Ok(self.snapshot());
                    }
                    state.native = Some(native);
                    state.microphone_lease = microphone_lease;
                    state.phase = RecorderPhase::Previewing;
                    state.screen_selected = true;
                    self.snapshot_locked(&state)
                };
                self.emit(snapshot.clone());
                Ok(snapshot)
            }
            Err(failure) => {
                if failure == RecorderFailureCode::SourceSelectionCancelled {
                    let snapshot = {
                        let mut state = lock_recover(&self.state);
                        reset_for_preview(&mut state);
                        state.phase = RecorderPhase::Idle;
                        state.failure = Some(failure);
                        self.snapshot_locked(&state)
                    };
                    self.emit(snapshot.clone());
                    Ok(snapshot)
                } else {
                    Ok(self.fail(failure))
                }
            }
        }
    }

    pub fn preview_stop(&self) -> Result<RecorderSnapshot, RecorderCommandError> {
        self.apply_native_failure();
        let (native, lease, partial, snapshot) = {
            let mut state = lock_recover(&self.state);
            if state.phase != RecorderPhase::Previewing {
                return Err(RecorderCommandError::InvalidState);
            }
            let native = state.native.take();
            let lease = state.microphone_lease.take();
            let partial = state.partial_path.take();
            reset_for_preview(&mut state);
            state.phase = RecorderPhase::Idle;
            let snapshot = self.snapshot_locked(&state);
            (native, lease, partial, snapshot)
        };
        drop(native);
        drop(lease);
        remove_partial(partial.as_deref());
        self.emit(snapshot.clone());
        Ok(snapshot)
    }

    pub fn start(&self) -> Result<RecorderSnapshot, RecorderCommandError> {
        self.apply_native_failure();
        if lock_recover(&self.state).phase != RecorderPhase::Previewing {
            return Err(RecorderCommandError::InvalidState);
        }
        let (partial_path, final_path) = match allocate_output_paths(&self.recordings_dir) {
            Ok(paths) => paths,
            Err(_) => return Ok(self.fail(RecorderFailureCode::WriterFailed)),
        };
        let starting = {
            let mut state = lock_recover(&self.state);
            if state.phase != RecorderPhase::Previewing {
                remove_partial(Some(&partial_path));
                return Err(RecorderCommandError::InvalidState);
            }
            state.phase = RecorderPhase::Starting;
            self.snapshot_locked(&state)
        };
        self.emit(starting);

        let start_result = {
            let mut state = lock_recover(&self.state);
            match state.native.as_mut() {
                Some(native) => native_start(native, &partial_path),
                None => Err(RecorderFailureCode::StreamFailed),
            }
        };
        if let Err(failure) = start_result {
            remove_partial(Some(&partial_path));
            return Ok(self.fail(failure));
        }

        let snapshot = {
            let mut state = lock_recover(&self.state);
            // native_start ran with the lock released, so a cancel could have landed in between.
            // Publishing Recording over that would resurrect a capture the user stopped.
            if state.phase != RecorderPhase::Starting {
                drop(state);
                remove_partial(Some(&partial_path));
                return Err(RecorderCommandError::InvalidState);
            }
            state.partial_path = Some(partial_path);
            state.final_path = Some(final_path);
            state.active_since = Some(Instant::now());
            state.elapsed_before_active = Duration::ZERO;
            state.phase = RecorderPhase::Recording;
            self.snapshot_locked(&state)
        };
        self.emit(snapshot.clone());
        Ok(snapshot)
    }

    pub fn pause(&self) -> Result<RecorderSnapshot, RecorderCommandError> {
        self.apply_native_failure();
        let result = {
            let mut state = lock_recover(&self.state);
            if state.phase != RecorderPhase::Recording {
                return Err(RecorderCommandError::InvalidState);
            }
            let native_result = state
                .native
                .as_mut()
                .map(native_pause)
                .unwrap_or(Err(RecorderFailureCode::StreamFailed));
            if let Err(failure) = native_result {
                Err(failure)
            } else {
                if let Some(active_since) = state.active_since.take() {
                    state.elapsed_before_active = state
                        .elapsed_before_active
                        .saturating_add(active_since.elapsed());
                }
                state.phase = RecorderPhase::Paused;
                Ok(self.snapshot_locked(&state))
            }
        };
        match result {
            Ok(snapshot) => {
                self.emit(snapshot.clone());
                Ok(snapshot)
            }
            Err(failure) => Ok(self.fail(failure)),
        }
    }

    pub fn resume(&self) -> Result<RecorderSnapshot, RecorderCommandError> {
        self.apply_native_failure();
        let result = {
            let mut state = lock_recover(&self.state);
            if state.phase != RecorderPhase::Paused {
                return Err(RecorderCommandError::InvalidState);
            }
            let native_result = state
                .native
                .as_mut()
                .map(native_resume)
                .unwrap_or(Err(RecorderFailureCode::StreamFailed));
            if let Err(failure) = native_result {
                Err(failure)
            } else {
                state.active_since = Some(Instant::now());
                state.phase = RecorderPhase::Recording;
                Ok(self.snapshot_locked(&state))
            }
        };
        match result {
            Ok(snapshot) => {
                self.emit(snapshot.clone());
                Ok(snapshot)
            }
            Err(failure) => Ok(self.fail(failure)),
        }
    }

    pub fn stop(&self) -> Result<RecorderSnapshot, RecorderCommandError> {
        self.apply_native_failure();
        let (mut native, partial_path, final_path, finalizing) = {
            let mut state = lock_recover(&self.state);
            if !matches!(
                state.phase,
                RecorderPhase::Recording | RecorderPhase::Paused
            ) {
                return Err(RecorderCommandError::InvalidState);
            }
            if let Some(active_since) = state.active_since.take() {
                state.elapsed_before_active = state
                    .elapsed_before_active
                    .saturating_add(active_since.elapsed());
            }
            state.phase = RecorderPhase::Finalizing;
            // Past this point the phase is committed, so a missing field cannot return through
            // `?`: that would leave the recorder in Finalizing with no native handle and no
            // command that accepts it. Report it as a failure instead, which lands on Failed.
            let taken = state
                .native
                .take()
                .zip(state.partial_path.clone().zip(state.final_path.clone()));
            let Some((native, (partial_path, final_path))) = taken else {
                drop(state);
                return Ok(self.fail(RecorderFailureCode::OutputFinalizeFailed));
            };
            let snapshot = self.snapshot_locked(&state);
            (native, partial_path, final_path, snapshot)
        };
        self.emit(finalizing);

        let report = native_stop(&mut native);
        let outcome = report.and_then(|report| {
            output_is_complete(&partial_path, report.width, report.height)
                .then_some(report)
                .ok_or(RecorderFailureCode::OutputFinalizeFailed)
        });
        let report = match outcome {
            Ok(report) => report,
            Err(failure) => {
                native_cancel(&mut native);
                return Ok(self.fail(failure));
            }
        };
        if fs::rename(&partial_path, &final_path).is_err() {
            native_cancel(&mut native);
            return Ok(self.fail(RecorderFailureCode::OutputCommitFailed));
        }
        native_cancel(&mut native);
        // Read the counter after `cancel_and_destroy`: past that point Swift is neither running
        // the status callback nor able to start one, so this load is the final total and includes
        // the drops raised since the last heartbeat and during finalization. The heartbeat refresh
        // cannot cover them, because `state.native` was taken before this transaction runs.
        let dropped_video_frames = native_dropped_video_frames(&native);

        let (lease, snapshot) = {
            let mut state = lock_recover(&self.state);
            let lease = state.microphone_lease.take();
            state.partial_path = None;
            state.final_path = None;
            state.output_path = Some(final_path);
            state.width = Some(report.width);
            state.height = Some(report.height);
            state.dropped_video_frames = dropped_video_frames;
            state.active_since = None;
            state.elapsed_before_active = Duration::from_millis(report.duration_ms);
            state.failure = None;
            state.phase = RecorderPhase::Saved;
            let snapshot = self.snapshot_locked(&state);
            (lease, snapshot)
        };
        drop(lease);
        self.emit(snapshot.clone());
        Ok(snapshot)
    }

    pub fn cancel(&self) -> Result<RecorderSnapshot, RecorderCommandError> {
        let (native, lease, partial, snapshot) = {
            let mut state = lock_recover(&self.state);
            if !cancel_reaches(state.phase) {
                return Err(RecorderCommandError::InvalidState);
            }
            let native = state.native.take();
            let lease = state.microphone_lease.take();
            let partial = state.partial_path.take();
            reset_for_preview(&mut state);
            state.phase = RecorderPhase::Idle;
            let snapshot = self.snapshot_locked(&state);
            (native, lease, partial, snapshot)
        };
        drop(native);
        drop(lease);
        remove_partial(partial.as_deref());
        self.emit(snapshot.clone());
        Ok(snapshot)
    }

    pub fn snapshot(&self) -> RecorderSnapshot {
        self.apply_native_failure();
        let mut state = lock_recover(&self.state);
        self.refresh_native_metrics(&mut state);
        self.snapshot_locked(&state)
    }

    fn start_heartbeat(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        let handle = thread::spawn(move || loop {
            thread::sleep(Duration::from_secs(1));
            let Some(manager) = weak.upgrade() else {
                return;
            };
            if manager.heartbeat_stop.load(Ordering::Acquire) {
                return;
            }
            manager.emit_heartbeat();
        });
        *lock_recover(&self.heartbeat) = Some(handle);
    }

    fn emit_heartbeat(&self) {
        self.apply_native_failure();
        let snapshot = {
            let mut state = lock_recover(&self.state);
            self.refresh_native_metrics(&mut state);
            (state.phase == RecorderPhase::Recording).then(|| self.snapshot_locked(&state))
        };
        if let Some(snapshot) = snapshot {
            self.emit(snapshot);
        }
    }

    fn apply_native_failure(&self) {
        let failure = {
            let state = lock_recover(&self.state);
            native_failure(state.native.as_ref())
        };
        if let Some(failure) = failure {
            let _ = self.fail(failure);
        }
    }

    fn fail(&self, failure: RecorderFailureCode) -> RecorderSnapshot {
        let (native, lease, partial, snapshot) = {
            let mut state = lock_recover(&self.state);
            let native = state.native.take();
            let lease = state.microphone_lease.take();
            let partial = state.partial_path.take();
            state.final_path = None;
            state.active_since = None;
            state.elapsed_before_active = Duration::ZERO;
            state.screen_selected = false;
            state.output_path = None;
            state.width = None;
            state.height = None;
            state.failure = Some(failure);
            state.phase = if is_permission_failure(failure) {
                RecorderPhase::Permission
            } else {
                RecorderPhase::Failed
            };
            let snapshot = self.snapshot_locked(&state);
            (native, lease, partial, snapshot)
        };
        drop(native);
        drop(lease);
        remove_partial(partial.as_deref());
        self.emit(snapshot.clone());
        snapshot
    }

    fn refresh_native_metrics(&self, state: &mut RecorderState) {
        if let Some(native) = state.native.as_ref() {
            state.dropped_video_frames = native_dropped_video_frames(native);
        }
    }

    fn snapshot_locked(&self, state: &RecorderState) -> RecorderSnapshot {
        RecorderSnapshot {
            phase: state.phase,
            elapsed_ms: duration_millis(
                state.elapsed_before_active
                    + state
                        .active_since
                        .map_or(Duration::ZERO, |started| started.elapsed()),
            ),
            screen_selected: state.screen_selected,
            dropped_video_frames: state.dropped_video_frames,
            output_path: state
                .output_path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            width: state.width,
            height: state.height,
            failure: state.failure,
        }
    }

    fn emit(&self, snapshot: RecorderSnapshot) {
        if let Err(error) = (RecorderStateChangedEvent { snapshot }).emit(&self.app) {
            log::warn!("Failed to emit recorder state: {error}");
        }
    }
}

impl Drop for ScreenRecorderManager {
    fn drop(&mut self) {
        // Signal, do not join. The heartbeat sleeps a second between ticks and holds only a weak
        // reference, so it exits on its own; joining would make dropping this manager wait out a
        // sleep on whichever thread happens to release the last Arc.
        self.heartbeat_stop.store(true, Ordering::Release);
        lock_recover(&self.heartbeat).take();
        let (native, lease, partial) = {
            let mut state = lock_recover(&self.state);
            (
                state.native.take(),
                state.microphone_lease.take(),
                state.partial_path.take(),
            )
        };
        drop(native);
        drop(lease);
        remove_partial(partial.as_deref());
    }
}

fn reset_for_preview(state: &mut RecorderState) {
    state.active_since = None;
    state.elapsed_before_active = Duration::ZERO;
    state.screen_selected = false;
    state.dropped_video_frames = 0;
    state.output_path = None;
    state.width = None;
    state.height = None;
    state.failure = None;
    state.partial_path = None;
    state.final_path = None;
}

/// Cancellation reaches every phase that owns work in flight, so no sequence of calls can wedge
/// the recorder in a phase that accepts nothing. The terminal phases have nothing to cancel:
/// Idle, Saved, Failed, Permission and Checking either hold no capture or resolve on their own.
fn cancel_reaches(phase: RecorderPhase) -> bool {
    matches!(
        phase,
        RecorderPhase::SelectingSource
            | RecorderPhase::Previewing
            | RecorderPhase::Starting
            | RecorderPhase::Recording
            | RecorderPhase::Paused
            | RecorderPhase::Finalizing
    )
}

fn is_permission_failure(failure: RecorderFailureCode) -> bool {
    matches!(
        failure,
        RecorderFailureCode::ScreenPermissionDenied
            | RecorderFailureCode::CameraPermissionDenied
            | RecorderFailureCode::MicrophonePermissionDenied
    )
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn output_is_complete(path: &Path, width: u32, height: u32) -> bool {
    width > 0
        && height > 0
        && fs::metadata(path)
            .map(|metadata| metadata.is_file() && metadata.len() > 0)
            .unwrap_or(false)
}

fn allocate_output_paths(recordings_dir: &Path) -> Result<(PathBuf, PathBuf), std::io::Error> {
    fs::create_dir_all(recordings_dir)?;
    for _ in 0..16 {
        let now = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        let id = Uuid::new_v4().to_string();
        let (partial_name, final_name) = output_names(&now, &id);
        let partial_path = recordings_dir.join(partial_name);
        let final_path = recordings_dir.join(final_name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial_path)
        {
            Ok(file) => {
                drop(file);
                fs::remove_file(&partial_path)?;
                return Ok((partial_path, final_path));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "unable to reserve a unique recorder output path",
    ))
}

fn output_names(timestamp: &str, id: &str) -> (String, String) {
    (
        format!(".sona-screen-{id}.partial.mp4"),
        format!("sona-screen-{timestamp}-{id}.mp4"),
    )
}

fn remove_partial(path: Option<&Path>) {
    if let Some(path) = path {
        let _ = fs::remove_file(path);
    }
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct NativePreflight {
    availability: RecorderAvailability,
    camera_devices: Vec<RecorderDevice>,
    microphone_devices: Vec<RecorderDevice>,
}

#[cfg(target_os = "macos")]
fn native_preflight() -> Result<NativePreflight, RecorderFailureCode> {
    let native = crate::recorder_macos::NativeRecorder::preflight().map_err(map_native_error)?;
    let availability = if native.availability == "supported" {
        RecorderAvailability::Supported
    } else {
        RecorderAvailability::Unsupported
    };
    Ok(NativePreflight {
        availability,
        camera_devices: native
            .camera_devices
            .into_iter()
            .map(|device| RecorderDevice {
                id: device.id,
                name: device.name,
            })
            .collect(),
        microphone_devices: native
            .microphone_devices
            .into_iter()
            .map(|device| RecorderDevice {
                id: device.id,
                name: device.name,
            })
            .collect(),
    })
}

#[cfg(not(target_os = "macos"))]
fn native_preflight() -> Result<NativePreflight, RecorderFailureCode> {
    Ok(NativePreflight {
        availability: RecorderAvailability::Unsupported,
        camera_devices: Vec::new(),
        microphone_devices: Vec::new(),
    })
}

#[cfg(target_os = "macos")]
fn native_preview_start(
    request: &RecorderStartRequest,
) -> Result<NativeRecorder, RecorderFailureCode> {
    crate::recorder_macos::NativeRecorder::preview_start(
        request.camera_device_id.as_deref(),
        request.microphone_device_id.as_deref(),
        request.camera_enabled,
        request.microphone_enabled,
    )
    .map_err(map_native_error)
}

#[cfg(not(target_os = "macos"))]
fn native_preview_start(_: &RecorderStartRequest) -> Result<NativeRecorder, RecorderFailureCode> {
    Err(RecorderFailureCode::Unsupported)
}

#[cfg(target_os = "macos")]
fn native_start(native: &mut NativeRecorder, path: &Path) -> Result<(), RecorderFailureCode> {
    native
        .start(&path.to_string_lossy())
        .map_err(map_native_error)
}

#[cfg(not(target_os = "macos"))]
fn native_start(_: &mut NativeRecorder, _: &Path) -> Result<(), RecorderFailureCode> {
    Err(RecorderFailureCode::Unsupported)
}

#[cfg(target_os = "macos")]
fn native_pause(native: &mut NativeRecorder) -> Result<(), RecorderFailureCode> {
    native.pause().map_err(map_native_error)
}

#[cfg(not(target_os = "macos"))]
fn native_pause(_: &mut NativeRecorder) -> Result<(), RecorderFailureCode> {
    Err(RecorderFailureCode::Unsupported)
}

#[cfg(target_os = "macos")]
fn native_resume(native: &mut NativeRecorder) -> Result<(), RecorderFailureCode> {
    native.resume().map_err(map_native_error)
}

#[cfg(not(target_os = "macos"))]
fn native_resume(_: &mut NativeRecorder) -> Result<(), RecorderFailureCode> {
    Err(RecorderFailureCode::Unsupported)
}

#[cfg(target_os = "macos")]
fn native_stop(
    native: &mut NativeRecorder,
) -> Result<crate::recorder_macos::NativeRecorderStopReport, RecorderFailureCode> {
    native.stop().map_err(map_native_error)
}

#[cfg(not(target_os = "macos"))]
fn native_stop(_: &mut NativeRecorder) -> Result<NativeStopReport, RecorderFailureCode> {
    Err(RecorderFailureCode::Unsupported)
}

#[cfg(target_os = "macos")]
fn native_cancel(native: &mut NativeRecorder) {
    native.cancel_and_destroy();
}

#[cfg(not(target_os = "macos"))]
fn native_cancel(_: &mut NativeRecorder) {}

#[cfg(target_os = "macos")]
fn native_failure(native: Option<&NativeRecorder>) -> Option<RecorderFailureCode> {
    native.and_then(|native| native.take_failure().map(map_native_error))
}

#[cfg(not(target_os = "macos"))]
fn native_failure(_: Option<&NativeRecorder>) -> Option<RecorderFailureCode> {
    None
}

#[cfg(target_os = "macos")]
fn native_dropped_video_frames(native: &NativeRecorder) -> u64 {
    native.dropped_video_frames()
}

#[cfg(not(target_os = "macos"))]
fn native_dropped_video_frames(_: &NativeRecorder) -> u64 {
    0
}

#[cfg(not(target_os = "macos"))]
struct NativeStopReport {
    width: u32,
    height: u32,
    duration_ms: u64,
}

#[cfg(target_os = "macos")]
fn map_native_error(error: crate::recorder_macos::NativeRecorderError) -> RecorderFailureCode {
    use crate::recorder_macos::NativeRecorderError;
    match error {
        NativeRecorderError::Unsupported => RecorderFailureCode::Unsupported,
        NativeRecorderError::ScreenPermissionDenied => RecorderFailureCode::ScreenPermissionDenied,
        NativeRecorderError::CameraPermissionDenied => RecorderFailureCode::CameraPermissionDenied,
        NativeRecorderError::MicrophonePermissionDenied => {
            RecorderFailureCode::MicrophonePermissionDenied
        }
        NativeRecorderError::SourceSelectionCancelled => {
            RecorderFailureCode::SourceSelectionCancelled
        }
        NativeRecorderError::SourceUnavailable => RecorderFailureCode::SourceUnavailable,
        NativeRecorderError::CameraUnavailable => RecorderFailureCode::CameraUnavailable,
        NativeRecorderError::MicrophoneUnavailable => RecorderFailureCode::MicrophoneUnavailable,
        NativeRecorderError::StreamFailed => RecorderFailureCode::StreamFailed,
        NativeRecorderError::TimestampDiscontinuity => RecorderFailureCode::TimestampDiscontinuity,
        NativeRecorderError::WriterFailed => RecorderFailureCode::WriterFailed,
        NativeRecorderError::OutputFinalizeFailed => RecorderFailureCode::OutputFinalizeFailed,
        NativeRecorderError::InvalidState => RecorderFailureCode::StreamFailed,
    }
}

#[cfg(test)]
mod tests {
    use super::{cancel_reaches, output_names, RecorderPhase};

    /// Cancellation is the only way out of a phase that owns work in flight. If any of these
    /// stops accepting it, that phase becomes a wedge: preview_start rejects it, stop rejects it,
    /// and the microphone lease stays held for the life of the process.
    #[test]
    fn cancel_reaches_every_phase_that_owns_capture() {
        for phase in [
            RecorderPhase::SelectingSource,
            RecorderPhase::Previewing,
            RecorderPhase::Starting,
            RecorderPhase::Recording,
            RecorderPhase::Paused,
            RecorderPhase::Finalizing,
        ] {
            assert!(cancel_reaches(phase), "{phase:?} owns capture");
        }
        for phase in [
            RecorderPhase::Idle,
            RecorderPhase::Checking,
            RecorderPhase::Saved,
            RecorderPhase::Failed,
            RecorderPhase::Permission,
        ] {
            assert!(!cancel_reaches(phase), "{phase:?} owns nothing to cancel");
        }
    }

    #[test]
    fn recorder_output_names_keep_partials_private_and_publish_mp4() {
        let (partial, published) = output_names("20260903T120000Z", "test-id");

        assert_eq!(partial, ".sona-screen-test-id.partial.mp4");
        assert_eq!(published, "sona-screen-20260903T120000Z-test-id.mp4");
    }
}

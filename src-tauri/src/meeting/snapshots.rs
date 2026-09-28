//! Pictures of a call's shared screen.
//!
//! While a meeting records, the person can take a picture of the meeting
//! app's window with one action, and can let Sona take one on its own when
//! that window changes a lot, such as a new slide. The automatic mode is off
//! until the person turns it on in Settings, can be turned off for one
//! meeting, keeps at most one picture every twenty seconds, and at most sixty
//! per meeting. Pictures are stored encrypted with the meeting and go where
//! the meeting goes: review, export, the trash. They are never sent to a
//! model: no engine that writes meeting notes reads images.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::RgbaImage;
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Emitter, Listener};
use uuid::Uuid;

use super::session::{MeetingSessionManager, MEETING_EVENT_SCHEMA_VERSION};
use super::store::{MeetingStore, StoreError};
use super::types::{MeetingEventPayload, MeetingPhase, MeetingSessionId};

/// Pictures Sona takes on its own in one meeting.
pub const AUTOMATIC_SNAPSHOT_LIMIT: u32 = 60;
/// Pictures of every kind one meeting keeps.
pub const SNAPSHOT_LIMIT: u32 = 200;
/// Width of a kept picture; a wider window is scaled down to it.
const FULL_WIDTH: u32 = 1920;
/// Width of the frame the automatic mode compares, which is never kept.
const WATCH_WIDTH: u32 = 320;
/// Width of the small copy the meeting screens show.
const THUMBNAIL_WIDTH: u32 = 480;
const WATCH_INTERVAL: Duration = Duration::from_secs(3);
const MAXIMUM_CAPTURE_TARGETS: usize = 64;
const MAXIMUM_BUNDLE_ID_BYTES: usize = 255;

const FINGERPRINT_SIDE: usize = 32;
/// Mean brightness change, 0 to 1, that makes a frame a new picture. A moving
/// cursor or a speaker tile stays well under it; a new slide is well over.
const CHANGE_THRESHOLD: f32 = 0.12;
/// A frame counts as settled when it moved less than this since the last
/// look, so a slide caught mid-transition or a playing video waits.
const SETTLED_THRESHOLD: f32 = 0.03;
const MINIMUM_INTERVAL_NS: u64 = 20_000_000_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, Type)]
#[serde(transparent)]
pub struct MeetingSnapshotId(Uuid);

impl MeetingSnapshotId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    pub fn uuid(self) -> Uuid {
        self.0
    }
}

impl Default for MeetingSnapshotId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingSnapshotTrigger {
    Manual,
    Automatic,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingSnapshotSummary {
    pub snapshot_id: MeetingSnapshotId,
    pub session_id: MeetingSessionId,
    /// Time into the recording, on the transcript's clock.
    pub offset_ns: u64,
    pub captured_at_utc_ms: i64,
    pub width: u32,
    pub height: u32,
    pub trigger: MeetingSnapshotTrigger,
    pub app_bundle_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingSnapshotSize {
    Thumbnail,
    Full,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingSnapshotImage {
    pub snapshot_id: MeetingSnapshotId,
    pub size: MeetingSnapshotSize,
    pub png_base64: String,
}

/// What the automatic mode is doing for one meeting right now.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingSnapshotWatch {
    /// The meeting is not recording.
    #[default]
    Idle,
    /// Looking at the meeting window for a change.
    Watching,
    /// No meeting window is on screen, so nothing is looked at.
    Paused,
    /// macOS has not given Sona Screen Recording.
    Denied,
    /// The meeting has all the automatic pictures it keeps.
    LimitReached,
    /// Turned off in Settings or for this meeting.
    Off,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingSnapshotStatus {
    pub session_id: MeetingSessionId,
    pub automatic_setting: bool,
    pub turned_off_for_meeting: bool,
    pub screen_recording_granted: bool,
    pub state: MeetingSnapshotWatch,
    pub count: u32,
    pub automatic_count: u32,
    pub automatic_limit: u32,
    pub limit: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingSnapshotError {
    ScreenRecordingDenied,
    NoMeetingWindow,
    NotRecording,
    LimitReached,
    NotFound,
    StorageUnavailable,
    Unsupported,
    CaptureFailed,
}

impl From<StoreError> for MeetingSnapshotError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::NotFound => Self::NotFound,
            StoreError::Conflict => Self::LimitReached,
            _ => Self::StorageUnavailable,
        }
    }
}

/// Sent whenever a meeting's pictures or its automatic mode change.
#[derive(Clone, Debug, Deserialize, Serialize, Type)]
#[serde(transparent)]
pub struct MeetingSnapshotChangedEvent(pub MeetingEventPayload);

impl tauri_specta::Event for MeetingSnapshotChangedEvent {
    const NAME: &'static str = "meeting:snapshot-changed";
}

/// A frame reduced to a 32 by 32 grid of brightness, 0 to 1, so two frames
/// compare in a thousand steps whatever their size.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Fingerprint([f32; FINGERPRINT_SIDE * FINGERPRINT_SIDE]);

impl Fingerprint {
    /// `pixels` are straight-alpha RGBA rows, `width * 4` bytes each. A
    /// transparent pixel, such as a window's rounded corner, reads as black.
    pub(crate) fn from_rgba(pixels: &[u8], width: u32, height: u32) -> Option<Self> {
        let width = usize::try_from(width).ok()?;
        let height = usize::try_from(height).ok()?;
        if width == 0 || height == 0 || pixels.len() < width.checked_mul(height)?.checked_mul(4)? {
            return None;
        }
        let mut sums = [0_f32; FINGERPRINT_SIDE * FINGERPRINT_SIDE];
        let mut counts = [0_u32; FINGERPRINT_SIDE * FINGERPRINT_SIDE];
        for y in 0..height {
            let row = y * FINGERPRINT_SIDE / height * FINGERPRINT_SIDE;
            for x in 0..width {
                let pixel = &pixels[(y * width + x) * 4..(y * width + x) * 4 + 4];
                let luma = 0.2126 * f32::from(pixel[0])
                    + 0.7152 * f32::from(pixel[1])
                    + 0.0722 * f32::from(pixel[2]);
                let cell = row + x * FINGERPRINT_SIDE / width;
                sums[cell] += luma * f32::from(pixel[3]) / (255.0 * 255.0);
                counts[cell] += 1;
            }
        }
        for (sum, count) in sums.iter_mut().zip(counts) {
            if count > 0 {
                // Captures are at most 1920 by 1920, so each cell has at most 3600 pixels.
                *sum /= f32::from(u16::try_from(count).ok()?);
            }
        }
        Some(Self(sums))
    }

    fn distance(&self, other: &Self) -> f32 {
        let total: f32 = self
            .0
            .iter()
            .zip(&other.0)
            .map(|(a, b)| (a - b).abs())
            .sum();
        // SAFETY: the fixed 32 by 32 fingerprint has 1024 cells, which fits in u16.
        let cell_count = u16::try_from(self.0.len()).expect("a 32 by 32 fingerprint fits in u16");
        total / f32::from(cell_count)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChangeVerdict {
    Keep,
    /// Too close to the last kept picture to be a new one.
    Unchanged,
    /// New, but still moving since the last look.
    Settling,
    /// New and settled, but the last picture is under twenty seconds old.
    TooSoon,
    LimitReached,
}

/// Decides, from frames looked at every few seconds, when the meeting window
/// shows something new enough to keep. Time comes in from the caller.
#[derive(Debug, Default)]
pub(crate) struct ChangeDetector {
    kept: Option<Fingerprint>,
    kept_at_ns: Option<u64>,
    previous: Option<Fingerprint>,
    automatic_count: u32,
}

impl ChangeDetector {
    pub(crate) fn consider(&mut self, frame: Fingerprint, now_ns: u64) -> ChangeVerdict {
        let previous = self.previous.replace(frame.clone());
        if self.at_limit() {
            return ChangeVerdict::LimitReached;
        }
        let changed = self
            .kept
            .as_ref()
            .is_none_or(|kept| kept.distance(&frame) >= CHANGE_THRESHOLD);
        if !changed {
            return ChangeVerdict::Unchanged;
        }
        let settled = previous
            .as_ref()
            .is_some_and(|previous| previous.distance(&frame) <= SETTLED_THRESHOLD);
        if !settled {
            return ChangeVerdict::Settling;
        }
        if self
            .kept_at_ns
            .is_some_and(|kept_at| now_ns.saturating_sub(kept_at) < MINIMUM_INTERVAL_NS)
        {
            return ChangeVerdict::TooSoon;
        }
        ChangeVerdict::Keep
    }

    /// Records a kept picture, taken on its own or by the person. Only the
    /// automatic ones count toward the meeting's automatic limit.
    pub(crate) fn kept(&mut self, frame: Fingerprint, now_ns: u64, automatic: bool) {
        self.kept = Some(frame);
        self.kept_at_ns = Some(now_ns);
        if automatic {
            self.automatic_count = self.automatic_count.saturating_add(1);
        }
    }

    fn at_limit(&self) -> bool {
        self.automatic_count >= AUTOMATIC_SNAPSHOT_LIMIT
    }
}

struct Frame {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
    bundle_id: String,
}

#[derive(Default)]
struct SessionWatch {
    turned_off: bool,
    worker_running: bool,
    state: MeetingSnapshotWatch,
    capture_bundle_id: Option<String>,
    detector: ChangeDetector,
}

fn is_recording(store: &MeetingStore, session_id: MeetingSessionId) -> bool {
    matches!(
        store.session_snapshot(session_id),
        Ok(snapshot) if matches!(snapshot.phase, MeetingPhase::CapturingRecording)
    )
}

fn thumbnail_size(width: u32, height: u32) -> (u32, u32) {
    if width <= THUMBNAIL_WIDTH {
        return (width, height);
    }
    let scaled = u64::from(height) * u64::from(THUMBNAIL_WIDTH) / u64::from(width);
    (THUMBNAIL_WIDTH, u32::try_from(scaled.max(1)).unwrap_or(1))
}

fn encode_png(image: &RgbaImage) -> Result<Vec<u8>, MeetingSnapshotError> {
    let mut bytes = Vec::new();
    image
        .write_with_encoder(PngEncoder::new_with_quality(
            &mut bytes,
            CompressionType::Fast,
            FilterType::Adaptive,
        ))
        .map_err(|_| MeetingSnapshotError::CaptureFailed)?;
    Ok(bytes)
}

/// `snapshot-02-00-14-05.png` for the second picture, taken 14 minutes and 5
/// seconds in. The number keeps two pictures from the same second apart.
pub(crate) fn export_image_name(index: usize, offset_ns: u64) -> String {
    let seconds = offset_ns / 1_000_000_000;
    format!(
        "snapshot-{:02}-{:02}-{:02}-{:02}.png",
        index + 1,
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// The folder an export's pictures go in: beside the exported file, named
/// after it, such as `Weekly sync snapshots` for `Weekly sync.md`.
pub(crate) fn export_folder_name(export_path: &Path) -> Option<String> {
    let stem = export_path.file_stem()?.to_str()?;
    Some(format!("{stem} snapshots"))
}

/// Writes every picture of an exported meeting, full size, into the folder
/// beside the exported file. Nothing is written for a meeting without any.
pub(crate) fn write_export_images(
    store: &MeetingStore,
    session_id: MeetingSessionId,
    snapshots: &[MeetingSnapshotSummary],
    export_path: &Path,
) -> Result<(), StoreError> {
    if snapshots.is_empty() {
        return Ok(());
    }
    let folder =
        export_path.with_file_name(export_folder_name(export_path).ok_or(StoreError::Invalid)?);
    std::fs::create_dir_all(&folder)?;
    for (index, snapshot) in snapshots.iter().enumerate() {
        let png = store.meeting_snapshot_png(
            session_id,
            snapshot.snapshot_id,
            MeetingSnapshotSize::Full,
        )?;
        std::fs::write(
            folder.join(export_image_name(index, snapshot.offset_ns)),
            png,
        )?;
    }
    Ok(())
}

/// Takes pictures for recording meetings and answers the snapshot commands.
pub struct MeetingSnapshotService {
    app: AppHandle,
    manager: Arc<MeetingSessionManager>,
    sessions: Mutex<HashMap<MeetingSessionId, SessionWatch>>,
}

impl MeetingSnapshotService {
    /// Starts watching for meetings that begin recording. A meeting gets one
    /// watcher thread while it records; the thread looks at the window only
    /// while the automatic mode is on.
    pub fn start(app: AppHandle, manager: Arc<MeetingSessionManager>) -> Arc<Self> {
        let service = Arc::new(Self {
            app: app.clone(),
            manager,
            sessions: Mutex::new(HashMap::new()),
        });
        let listener = Arc::clone(&service);
        app.listen("meeting:session-changed", move |event| {
            let Ok(payload) = serde_json::from_str::<MeetingEventPayload>(event.payload()) else {
                return;
            };
            let Some(session_id) = payload.session_id else {
                return;
            };
            let service = Arc::clone(&listener);
            tauri::async_runtime::spawn(async move { service.session_changed(session_id).await });
        });
        service
    }

    fn sessions(&self) -> MutexGuard<'_, HashMap<MeetingSessionId, SessionWatch>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn emit(&self, session_id: MeetingSessionId) {
        let payload = MeetingEventPayload {
            event_schema_version: MEETING_EVENT_SCHEMA_VERSION,
            session_id: Some(session_id),
            revision: 0,
        };
        // Emitting fails only when the payload cannot be serialized, which a
        // plain record never does; the next change sends a fresh one anyway.
        let _ = self.app.emit(
            <MeetingSnapshotChangedEvent as tauri_specta::Event>::NAME,
            payload,
        );
    }

    async fn store(&self) -> Result<Arc<MeetingStore>, MeetingSnapshotError> {
        self.manager
            .store()
            .await
            .map_err(|_| MeetingSnapshotError::StorageUnavailable)
    }

    async fn session_changed(self: Arc<Self>, session_id: MeetingSessionId) {
        let Ok(store) = self.store().await else {
            return;
        };
        if !is_recording(&store, session_id) {
            return;
        }
        let Ok(automatic_count) =
            store.count_meeting_snapshots(session_id, Some(MeetingSnapshotTrigger::Automatic))
        else {
            return;
        };
        {
            let mut sessions = self.sessions();
            let watch = sessions.entry(session_id).or_default();
            if watch.worker_running {
                return;
            }
            watch.worker_running = true;
            watch.detector.automatic_count = automatic_count;
        }
        let worker = Arc::clone(&self);
        let spawned = std::thread::Builder::new()
            .name("sona-meeting-snapshots".to_owned())
            .spawn(move || worker.watch(store, session_id));
        if spawned.is_err() {
            if let Some(watch) = self.sessions().get_mut(&session_id) {
                watch.worker_running = false;
            }
        }
        self.emit(session_id);
    }

    fn watch(self: Arc<Self>, store: Arc<MeetingStore>, session_id: MeetingSessionId) {
        let mut targets = self.capture_targets(&store, session_id);
        loop {
            std::thread::sleep(WATCH_INTERVAL);
            if !self.still_recording(&store, session_id) {
                return;
            }
            let state = self.look(&store, session_id, &mut targets);
            self.set_state(session_id, state);
        }
    }

    /// Whether the watcher should keep going. When the meeting stops, the
    /// watcher gives up its claim and then looks once more, so a meeting that
    /// resumed between the two looks is never left without one.
    fn still_recording(&self, store: &MeetingStore, session_id: MeetingSessionId) -> bool {
        if is_recording(store, session_id) {
            return true;
        }
        if let Some(watch) = self.sessions().get_mut(&session_id) {
            watch.worker_running = false;
            watch.state = MeetingSnapshotWatch::Idle;
        }
        self.emit(session_id);
        if !is_recording(store, session_id) {
            let paused = matches!(
                store.session_snapshot(session_id),
                Ok(snapshot) if snapshot.phase == MeetingPhase::CapturingPaused
            );
            if !paused {
                self.sessions().remove(&session_id);
            }
            return false;
        }
        let mut sessions = self.sessions();
        let watch = sessions.entry(session_id).or_default();
        if watch.worker_running {
            return false;
        }
        watch.worker_running = true;
        true
    }

    fn look(
        &self,
        store: &MeetingStore,
        session_id: MeetingSessionId,
        targets: &mut Vec<String>,
    ) -> MeetingSnapshotWatch {
        let enabled = crate::settings::get_settings(&self.app).meeting_screen_snapshots_enabled;
        let (turned_off, at_limit) = {
            let sessions = self.sessions();
            let watch = sessions.get(&session_id);
            (
                watch.is_some_and(|watch| watch.turned_off),
                watch.is_some_and(|watch| watch.detector.at_limit()),
            )
        };
        if !enabled || turned_off {
            return MeetingSnapshotWatch::Off;
        }
        if at_limit {
            return MeetingSnapshotWatch::LimitReached;
        }
        let frame = match platform::capture(targets, WATCH_WIDTH) {
            Ok(frame) => frame,
            Err(MeetingSnapshotError::ScreenRecordingDenied) => {
                return MeetingSnapshotWatch::Denied
            }
            Err(MeetingSnapshotError::Unsupported) => return MeetingSnapshotWatch::Off,
            Err(_) => {
                // The window went away; when it comes back, wait for it to
                // hold still before judging it.
                if let Some(watch) = self.sessions().get_mut(&session_id) {
                    watch.detector.previous = None;
                }
                return MeetingSnapshotWatch::Paused;
            }
        };
        // Once a meeting app is found, stay with it. Hiding its window must
        // pause capture, not photograph an unrelated browser instead.
        if targets.len() != 1 || targets.first() != Some(&frame.bundle_id) {
            targets.clear();
            targets.push(frame.bundle_id.clone());
            self.sessions()
                .entry(session_id)
                .or_default()
                .capture_bundle_id = Some(frame.bundle_id.clone());
        }
        let Some(fingerprint) = Fingerprint::from_rgba(&frame.pixels, frame.width, frame.height)
        else {
            return MeetingSnapshotWatch::Paused;
        };
        let now_ns = super::clock::host_monotonic_now_ns();
        let verdict = self
            .sessions()
            .entry(session_id)
            .or_default()
            .detector
            .consider(fingerprint.clone(), now_ns);
        match verdict {
            ChangeVerdict::Keep => match self.capture_and_store(
                store,
                session_id,
                MeetingSnapshotTrigger::Automatic,
                targets,
                Some(fingerprint),
            ) {
                Ok(_) => MeetingSnapshotWatch::Watching,
                Err(MeetingSnapshotError::LimitReached) => MeetingSnapshotWatch::LimitReached,
                Err(MeetingSnapshotError::ScreenRecordingDenied) => MeetingSnapshotWatch::Denied,
                Err(_) => MeetingSnapshotWatch::Paused,
            },
            ChangeVerdict::LimitReached => MeetingSnapshotWatch::LimitReached,
            ChangeVerdict::Unchanged | ChangeVerdict::Settling | ChangeVerdict::TooSoon => {
                MeetingSnapshotWatch::Watching
            }
        }
    }

    fn set_state(&self, session_id: MeetingSessionId, state: MeetingSnapshotWatch) {
        let changed = {
            let mut sessions = self.sessions();
            let watch = sessions.entry(session_id).or_default();
            let changed = watch.state != state;
            watch.state = state;
            changed
        };
        if changed {
            self.emit(session_id);
        }
    }

    /// Prefer the apps this meeting's audio comes from. Without an audio
    /// route, try the frontmost meeting app before the configured apps.
    fn capture_targets(&self, store: &MeetingStore, session_id: MeetingSessionId) -> Vec<String> {
        if let Some(bundle_id) = self
            .sessions()
            .get(&session_id)
            .and_then(|watch| watch.capture_bundle_id.clone())
        {
            return vec![bundle_id];
        }
        let planned = store
            .processing_plan(session_id)
            .map(|plan| plan.frozen_system_audio_application_bundle_ids)
            .unwrap_or_default();
        let configured = crate::settings::get_settings(&self.app).detection_meeting_apps;
        let mut targets: Vec<String> = Vec::new();
        let frontmost = crate::context::frontmost_application_identifier()
            .map(|identifier| identifier.to_lowercase())
            .filter(|identifier| {
                configured
                    .iter()
                    .any(|app| app.eq_ignore_ascii_case(identifier))
                    || super::detection::apps::is_browser_bundle_id(identifier)
            });
        let candidates = if planned.is_empty() {
            frontmost.into_iter().chain(configured).collect()
        } else {
            planned
        };
        for bundle_id in candidates {
            let bundle_id = bundle_id.trim().to_lowercase();
            if bundle_id.is_empty()
                || bundle_id.len() > MAXIMUM_BUNDLE_ID_BYTES
                || bundle_id.contains('\0')
                || targets.contains(&bundle_id)
            {
                continue;
            }
            targets.push(bundle_id);
            if targets.len() == MAXIMUM_CAPTURE_TARGETS {
                break;
            }
        }
        targets
    }

    fn capture_and_store(
        &self,
        store: &MeetingStore,
        session_id: MeetingSessionId,
        trigger: MeetingSnapshotTrigger,
        targets: &[String],
        fingerprint: Option<Fingerprint>,
    ) -> Result<MeetingSnapshotSummary, MeetingSnapshotError> {
        if !is_recording(store, session_id) {
            return Err(MeetingSnapshotError::NotRecording);
        }
        if trigger == MeetingSnapshotTrigger::Automatic
            && (!crate::settings::get_settings(&self.app).meeting_screen_snapshots_enabled
                || self
                    .sessions()
                    .get(&session_id)
                    .is_some_and(|watch| watch.turned_off))
        {
            return Err(MeetingSnapshotError::NotRecording);
        }
        let plan = store.processing_plan(session_id)?;
        let frame = platform::capture(targets, FULL_WIDTH)?;
        let captured_at_ns = super::clock::host_monotonic_now_ns();
        let captured_at_utc_ms = chrono::Utc::now().timestamp_millis();
        let offset_ns =
            captured_at_ns.saturating_sub(plan.session_clock_anchor.host_monotonic_anchor_ns);
        let (width, height) = (frame.width, frame.height);
        let image = RgbaImage::from_raw(width, height, frame.pixels)
            .ok_or(MeetingSnapshotError::CaptureFailed)?;
        let (thumbnail_width, thumbnail_height) = thumbnail_size(width, height);
        let thumbnail = image::imageops::thumbnail(&image, thumbnail_width, thumbnail_height);
        let png = encode_png(&image)?;
        let thumbnail_png = encode_png(&thumbnail)?;
        let summary = MeetingSnapshotSummary {
            snapshot_id: MeetingSnapshotId::new(),
            session_id,
            offset_ns,
            captured_at_utc_ms,
            width,
            height,
            trigger,
            app_bundle_id: Some(frame.bundle_id).filter(|bundle_id| !bundle_id.is_empty()),
        };
        if !is_recording(store, session_id) {
            return Err(MeetingSnapshotError::NotRecording);
        }
        if trigger == MeetingSnapshotTrigger::Automatic
            && (!crate::settings::get_settings(&self.app).meeting_screen_snapshots_enabled
                || self
                    .sessions()
                    .get(&session_id)
                    .is_some_and(|watch| watch.turned_off))
        {
            return Err(MeetingSnapshotError::NotRecording);
        }
        store.insert_meeting_snapshot(&summary, &png, &thumbnail_png)?;
        self.sessions()
            .entry(session_id)
            .or_default()
            .capture_bundle_id = summary.app_bundle_id.clone();
        let fingerprint =
            fingerprint.or_else(|| Fingerprint::from_rgba(image.as_raw(), width, height));
        if let Some(fingerprint) = fingerprint {
            self.sessions()
                .entry(session_id)
                .or_default()
                .detector
                .kept(
                    fingerprint,
                    super::clock::host_monotonic_now_ns(),
                    trigger == MeetingSnapshotTrigger::Automatic,
                );
        }
        self.emit(session_id);
        Ok(summary)
    }

    /// The person's own "Snapshot screen". Pressing it is the consent, so it
    /// works with the automatic mode off, but only while the meeting records.
    pub async fn take(
        self: &Arc<Self>,
        session_id: MeetingSessionId,
    ) -> Result<MeetingSnapshotSummary, MeetingSnapshotError> {
        let store = self.store().await?;
        if !is_recording(&store, session_id) {
            return Err(MeetingSnapshotError::NotRecording);
        }
        let service = Arc::clone(self);
        tauri::async_runtime::spawn_blocking(move || {
            let targets = service.capture_targets(&store, session_id);
            service.capture_and_store(
                &store,
                session_id,
                MeetingSnapshotTrigger::Manual,
                &targets,
                None,
            )
        })
        .await
        .map_err(|_| MeetingSnapshotError::CaptureFailed)?
    }

    pub async fn list(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<Vec<MeetingSnapshotSummary>, MeetingSnapshotError> {
        Ok(self.store().await?.meeting_snapshots(session_id)?)
    }

    pub async fn image(
        &self,
        session_id: MeetingSessionId,
        snapshot_id: MeetingSnapshotId,
        size: MeetingSnapshotSize,
    ) -> Result<MeetingSnapshotImage, MeetingSnapshotError> {
        let store = self.store().await?;
        let png = tauri::async_runtime::spawn_blocking(move || {
            store.meeting_snapshot_png(session_id, snapshot_id, size)
        })
        .await
        .map_err(|_| MeetingSnapshotError::StorageUnavailable)??;
        Ok(MeetingSnapshotImage {
            snapshot_id,
            size,
            png_base64: STANDARD.encode(png),
        })
    }

    pub async fn delete(
        &self,
        session_id: MeetingSessionId,
        snapshot_id: MeetingSnapshotId,
    ) -> Result<(), MeetingSnapshotError> {
        self.store()
            .await?
            .delete_meeting_snapshot(session_id, snapshot_id)?;
        self.emit(session_id);
        Ok(())
    }

    pub async fn status(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<MeetingSnapshotStatus, MeetingSnapshotError> {
        let store = self.store().await?;
        let count = store.count_meeting_snapshots(session_id, None)?;
        let automatic_count =
            store.count_meeting_snapshots(session_id, Some(MeetingSnapshotTrigger::Automatic))?;
        let automatic_setting =
            crate::settings::get_settings(&self.app).meeting_screen_snapshots_enabled;
        let sessions = self.sessions();
        let watch = sessions.get(&session_id);
        let turned_off_for_meeting = watch.is_some_and(|watch| watch.turned_off);
        let state = match watch {
            Some(watch) if watch.worker_running => {
                if !automatic_setting || turned_off_for_meeting {
                    MeetingSnapshotWatch::Off
                } else {
                    watch.state
                }
            }
            _ => MeetingSnapshotWatch::Idle,
        };
        Ok(MeetingSnapshotStatus {
            session_id,
            automatic_setting,
            turned_off_for_meeting,
            screen_recording_granted: platform::screen_recording_granted(),
            state,
            count,
            automatic_count,
            automatic_limit: AUTOMATIC_SNAPSHOT_LIMIT,
            limit: SNAPSHOT_LIMIT,
        })
    }

    /// Turns the automatic mode off or back on for one meeting. It does not
    /// outlive the app: after a restart the Settings switch decides again.
    pub async fn set_automatic(
        &self,
        session_id: MeetingSessionId,
        enabled: bool,
    ) -> Result<MeetingSnapshotStatus, MeetingSnapshotError> {
        self.sessions().entry(session_id).or_default().turned_off = !enabled;
        self.emit(session_id);
        self.status(session_id).await
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{Frame, MeetingSnapshotError};
    use std::ffi::{c_char, c_void, CStr, CString};

    unsafe extern "C" {
        fn sona_meeting_snapshot_probe() -> i32;
        fn sona_meeting_snapshot_capture(
            bundle_ids: *const c_void,
            bundle_id_count: usize,
            maximum_pixel_width: u32,
            out_pixels: *mut *mut u8,
            out_width: *mut u32,
            out_height: *mut u32,
            out_bundle_id: *mut c_char,
            out_bundle_id_capacity: usize,
        ) -> i32;
        fn sona_meeting_snapshot_free(pixels: *mut u8);
    }

    pub(super) fn screen_recording_granted() -> bool {
        // SAFETY: the probe takes nothing and only reads the process's grant.
        unsafe { sona_meeting_snapshot_probe() == 0 }
    }

    /// Looks at the first candidate app's largest window on screen, at most
    /// `maximum_width` pixels wide. Never asks for Screen Recording.
    pub(super) fn capture(
        targets: &[String],
        maximum_width: u32,
    ) -> Result<Frame, MeetingSnapshotError> {
        let owned: Vec<CString> = targets
            .iter()
            .filter_map(|target| CString::new(target.as_str()).ok())
            .collect();
        let pointers: Vec<*const c_char> = owned.iter().map(|target| target.as_ptr()).collect();
        let bundle_ids = if pointers.is_empty() {
            std::ptr::null()
        } else {
            pointers.as_ptr().cast::<c_void>()
        };
        let mut pixels: *mut u8 = std::ptr::null_mut();
        let mut width = 0_u32;
        let mut height = 0_u32;
        let mut bundle_id: [c_char; 256] = [0; 256];
        // The CStrings and pointer array stay alive through this call; output
        // pointers name distinct writable locals, including the 256-byte name buffer.
        // SAFETY: the bridge borrows inputs and bounds name writes by the supplied capacity.
        let result = unsafe {
            sona_meeting_snapshot_capture(
                bundle_ids,
                pointers.len(),
                maximum_width,
                &mut pixels,
                &mut width,
                &mut height,
                bundle_id.as_mut_ptr(),
                bundle_id.len(),
            )
        };
        if result != 0 {
            return Err(match result {
                2 => MeetingSnapshotError::Unsupported,
                3 => MeetingSnapshotError::ScreenRecordingDenied,
                4 => MeetingSnapshotError::NoMeetingWindow,
                _ => MeetingSnapshotError::CaptureFailed,
            });
        }
        if pixels.is_null() {
            return Err(MeetingSnapshotError::CaptureFailed);
        }
        let length = usize::try_from(width)
            .ok()
            .zip(usize::try_from(height).ok())
            .and_then(|(width, height)| width.checked_mul(height))
            .and_then(|count| count.checked_mul(4));
        // SAFETY: on success the bridge hands over a `width * height * 4`
        // byte buffer it allocated; it is copied once and freed by the bridge.
        let copied =
            length.map(|length| unsafe { std::slice::from_raw_parts(pixels, length) }.to_vec());
        // SAFETY: pixels is the bridge's live calloc allocation; no slice borrows it at this sole free.
        unsafe { sona_meeting_snapshot_free(pixels) };
        let pixels = copied.ok_or(MeetingSnapshotError::CaptureFailed)?;
        // SAFETY: the bridge always ends the name with a NUL inside the buffer.
        let bundle_id = unsafe { CStr::from_ptr(bundle_id.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        Ok(Frame {
            pixels,
            width,
            height,
            bundle_id,
        })
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::{Frame, MeetingSnapshotError};

    pub(super) fn screen_recording_granted() -> bool {
        false
    }

    pub(super) fn capture(
        _targets: &[String],
        _maximum_width: u32,
    ) -> Result<Frame, MeetingSnapshotError> {
        Err(MeetingSnapshotError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: u64 = 1_000_000_000;

    /// A 64 by 48 frame of one gray level, fully opaque.
    fn frame(level: u8) -> Fingerprint {
        let pixels: Vec<u8> = std::iter::repeat_n([level, level, level, 255], 64 * 48)
            .flatten()
            .collect();
        Fingerprint::from_rgba(&pixels, 64, 48).unwrap()
    }

    fn detector_that_kept(level: u8, at_ns: u64) -> ChangeDetector {
        let mut detector = ChangeDetector::default();
        detector.kept(frame(level), at_ns, false);
        detector
    }

    // Contract: a frame becomes a picture only when it differs from the last
    // kept one by the change threshold and has held still since the last look.
    #[test]
    fn a_frame_is_kept_once_it_changed_enough_and_holds_still() {
        let mut detector = detector_that_kept(100, 0);
        // 20 of 255 levels, about 0.08, is under the 0.12 threshold.
        assert_eq!(
            detector.consider(frame(120), 60 * SECOND),
            ChangeVerdict::Unchanged
        );
        // 40 of 255, about 0.16, is new, but it moved since the last look.
        assert_eq!(
            detector.consider(frame(140), 63 * SECOND),
            ChangeVerdict::Settling
        );
        assert_eq!(
            detector.consider(frame(140), 66 * SECOND),
            ChangeVerdict::Keep
        );
    }

    // Contract: no two automatic pictures are closer than twenty seconds.
    #[test]
    fn a_new_picture_within_twenty_seconds_of_the_last_waits() {
        let mut detector = detector_that_kept(0, 0);
        assert_eq!(
            detector.consider(frame(255), 10 * SECOND),
            ChangeVerdict::Settling
        );
        assert_eq!(
            detector.consider(frame(255), 13 * SECOND),
            ChangeVerdict::TooSoon
        );
        assert_eq!(
            detector.consider(frame(255), 20 * SECOND),
            ChangeVerdict::Keep
        );
    }

    // Contract: the automatic mode keeps at most the meeting's limit, however
    // much the screen changes afterwards.
    #[test]
    fn automatic_pictures_stop_at_the_meeting_limit() {
        let mut detector = ChangeDetector::default();
        for index in 0..AUTOMATIC_SNAPSHOT_LIMIT {
            detector.kept(frame(0), u64::from(index) * 30 * SECOND, true);
        }
        let later = u64::from(AUTOMATIC_SNAPSHOT_LIMIT) * 30 * SECOND;
        assert_eq!(
            detector.consider(frame(255), later),
            ChangeVerdict::LimitReached
        );
        assert_eq!(
            detector.consider(frame(255), later + 3 * SECOND),
            ChangeVerdict::LimitReached
        );
    }
}

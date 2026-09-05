use super::analytics::{
    merge_turns, MeetingActionItemState, MeetingAnalyticsSnapshot, MeetingCatchUp,
    MeetingNotesTemplate, MeetingUserNotes,
};
use super::capture::{MeetingCaptureSource, PacketLaneReadError, PacketLaneReader, PacketSink};
use super::clock::host_monotonic_now_ns;
use super::detection::machine::CalendarEventSummary;
use super::export;
use super::follow_up::{
    body_fits, follow_up_prompt, mailto_url, recipient_addresses, FollowUpEvidence,
    MeetingFollowUpDraft, MeetingFollowUpMail, MeetingFollowUpMailBody, MeetingFollowUpMailRequest,
    MeetingFollowUpSource, FOLLOW_UP_MAX_TOKENS,
};
use super::import_formats::{read_transcript_export, resolve_spans, ImportedSegment};
use super::keep_awake::MeetingKeepAwake;
use super::ledger;
use super::loop_types::{
    MeetingLoopAssignRequest, MeetingLoopMutationResult, MeetingLoopReopenRequest,
    MeetingLoopResolveRequest, MeetingLoopsResult,
};
use super::people_types::{PersonDetailResult, PersonId, PersonLinkConfidence};
use super::processing::{
    write_relationship_summary, LiveTranscript, LiveTranscriptWorker, MeetingProcessingService,
    MeetingTextGenerationError, ProcessingOrigin, ReplyShape,
};
use super::store::{
    InterruptedRecovery, MeetingStore, MeetingTrackWriter, RecoveredMeeting, SegmentEdit,
    StoreError, StoreMutation, StoreTransition, TrackCreation, TranscriptRevisionInput,
    TranscriptSegmentInput, STORE_SCHEMA_VERSION,
};
use super::suggestions::{
    MeetingSuggestion, MeetingSuggestionService, MeetingSuggestionSignal, MeetingSuggestionSink,
};
use super::types::*;
use crate::analytics::DashboardTrendRequest;
use crate::audio_toolkit::constants::WHISPER_SAMPLE_RATE;
use crate::managers::media_import::{
    decode_media_into, validate_media_path, AudioImportError, DecodeFailure, ValidatedMediaPath,
};
use crate::secrets::{SecretManager, SecretResolveError};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tauri_plugin_dialog::DialogExt;
use uuid::Uuid;

pub(super) const MEETING_EVENT_SCHEMA_VERSION: u32 = 1;
const DEFAULT_RECORD_MAX_PAYLOAD_BYTES: u32 = 4 * 1024 * 1024;
const DEFAULT_CHECKPOINT_INTERVAL_MS: u32 = 1_000;
const DEFAULT_SOURCE_SAMPLE_CAPACITY: u32 = 96_000;
const DEFAULT_SOURCE_DESCRIPTOR_CAPACITY: u32 = 128;
/// How much audio a source lane holds before its writer must have drained it.
/// The ring is a time budget, not a sample count: 96,000 f32 is two seconds of
/// 48 kHz mono and one second of stereo, so a fixed count silently gave the
/// stereo lane half the slack.
const SOURCE_LANE_TARGET_SECONDS: u32 = 4;
const RETENTION_SWEEP_INTERVAL: Duration = Duration::from_secs(15 * 60);
const RETENTION_OPERATION_NAMESPACE: Uuid =
    Uuid::from_u128(0x5192_4d08_51d9_4f31_a369_1f2d_a7de_c9d4);

/// The largest imported recording Sona will decode, in emitted 16 kHz mono
/// samples: twelve hours.
///
/// This is not a memory bound. The decode streams one resampled frame at a time
/// into the session's track file, so a two-hour import holds no more than a
/// two-minute one — about 30 ms of audio. What the ceiling bounds is the
/// meeting: its size on disk, and how long the transcript pass will run before
/// anybody sees notes. Twelve hours is past the longest meeting anyone records
/// and short of the point where an accidental video library becomes a meeting.
const MAX_IMPORT_RECORDING_SAMPLES: usize = 16_000 * 60 * 60 * 12;

/// The fixed format `media_import`'s Symphonia path resamples every source to,
/// and therefore the format an imported track records.
const IMPORT_AUDIO_FORMAT: AudioFormat = AudioFormat {
    sample_rate_hz: WHISPER_SAMPLE_RATE,
    channels: 1,
};

/// The engine credited with an imported transcript revision. No recognizer ran:
/// the text came from another product's export, and a reader auditing where a
/// segment's words came from needs that said rather than inferred.
const IMPORT_TRANSCRIPT_ENGINE_ID: &str = "import";

pub trait MeetingSourceProvider: Send + Sync {
    fn probe(&self, source_kind: SourceKind) -> SourceProbe;

    fn acquire(
        &self,
        source_kind: SourceKind,
    ) -> Result<Box<dyn MeetingCaptureSource>, MeetingCaptureError>;
}

pub(crate) struct NoCaptureSources;

impl MeetingSourceProvider for NoCaptureSources {
    fn probe(&self, source_kind: SourceKind) -> SourceProbe {
        SourceProbe {
            source_kind,
            availability: SourceAvailability::UnsupportedPlatform,
            health: SourceHealth::NotStarted,
            detail: Some(SourceProbeDetail::Platform),
            negotiated_format: None,
        }
    }

    fn acquire(
        &self,
        _source_kind: SourceKind,
    ) -> Result<Box<dyn MeetingCaptureSource>, MeetingCaptureError> {
        Err(MeetingCaptureError::Unavailable)
    }
}
/// The plan's capacity is the floor for a source that starts without
/// announcing a format; a source that announced one gets the same seconds of
/// slack whatever its rate and channel count.
fn lane_sample_capacity(floor: u32, format: Option<AudioFormat>) -> u64 {
    format
        .and_then(|format| {
            u64::from(format.sample_rate_hz)
                .checked_mul(u64::from(format.channels))?
                .checked_mul(u64::from(SOURCE_LANE_TARGET_SECONDS))
        })
        .unwrap_or(0)
        .max(u64::from(floor))
}

pub(crate) fn production_source_provider(
    audio: Arc<crate::managers::audio::AudioRecordingManager>,
) -> Arc<dyn MeetingSourceProvider> {
    Arc::new(BuiltinMeetingSources { audio })
}

struct BuiltinMeetingSources {
    audio: Arc<crate::managers::audio::AudioRecordingManager>,
}

impl MeetingSourceProvider for BuiltinMeetingSources {
    fn probe(&self, source_kind: SourceKind) -> SourceProbe {
        match source_kind {
            SourceKind::Microphone => match self.audio.try_acquire_meeting_microphone() {
                Ok(source) => source.probe(),
                Err(_) => SourceProbe {
                    source_kind,
                    availability: SourceAvailability::DeviceUnavailable,
                    health: SourceHealth::NotStarted,
                    detail: Some(SourceProbeDetail::Device),
                    negotiated_format: None,
                },
            },
            SourceKind::SystemAudio => {
                #[cfg(target_os = "macos")]
                {
                    crate::meeting_macos::MacosSystemAudioCapture::new().probe()
                }
                #[cfg(not(target_os = "macos"))]
                {
                    SourceProbe {
                        source_kind,
                        availability: SourceAvailability::UnsupportedPlatform,
                        health: SourceHealth::NotStarted,
                        detail: Some(SourceProbeDetail::Platform),
                        negotiated_format: None,
                    }
                }
            }
        }
    }

    fn acquire(
        &self,
        source_kind: SourceKind,
    ) -> Result<Box<dyn MeetingCaptureSource>, MeetingCaptureError> {
        match source_kind {
            SourceKind::Microphone => self
                .audio
                .try_acquire_meeting_microphone()
                .map(|source| Box::new(source) as Box<dyn MeetingCaptureSource>),
            SourceKind::SystemAudio => {
                #[cfg(target_os = "macos")]
                {
                    Ok(Box::new(
                        crate::meeting_macos::MacosSystemAudioCapture::new(),
                    ))
                }
                #[cfg(not(target_os = "macos"))]
                {
                    Err(MeetingCaptureError::Unavailable)
                }
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct MeetingPreflightCreateRequest {
    pub operation_id: MeetingOperationId,
    pub expected_revision: u64,
    pub title: String,
    pub origin: MeetingOrigin,
    pub suggestion_id: Option<MeetingSuggestionId>,
    #[serde(default)]
    pub calendar_event_key: Option<String>,
    pub requested_sources: Vec<SourceKind>,
    pub required_sources: Vec<SourceKind>,
    pub accepted_known_missing_sources: Vec<SourceKind>,
    pub degraded_start_policy: DegradedStartPolicy,
    pub destination: ProcessingDestination,
    pub remote_acknowledgement: Option<RemoteAcknowledgement>,
    pub microphone_device_uid: Option<String>,
    pub frozen_system_audio_application_bundle_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct MeetingPreflightRefreshRequest {
    pub operation_id: MeetingOperationId,
    pub session_id: MeetingSessionId,
    pub expected_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct MeetingConsentInput {
    pub policy_version: u32,
    pub microphone_acknowledged: bool,
    pub system_audio_acknowledged: bool,
    pub known_missing_sources_acknowledged: Vec<SourceKind>,
    pub degraded_start_policy: DegradedStartPolicy,
    pub destination: ProcessingDestination,
    pub remote_acknowledgement: Option<RemoteAcknowledgement>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct MeetingStartRequest {
    pub operation_id: MeetingOperationId,
    pub session_id: MeetingSessionId,
    pub expected_revision: u64,
    pub consent: MeetingConsentInput,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct MeetingConsentPanelStartRequest {
    pub prompt_id: String,
    pub operation_id: MeetingOperationId,
    pub consent: MeetingConsentInput,
    pub always_record_series: bool,
    /// Whether this recording posts one disclosure line into the meeting's own
    /// chat, and — for a recurring meeting — what its series should remember
    /// about that from now on.
    pub announce_in_chat: bool,
}

#[derive(Clone, Debug)]
pub struct MeetingDetectionStartContext {
    pub prompt_id: String,
    pub title: String,
    pub trigger_bundle_id: Option<String>,
    pub event_end_utc_ms: Option<i64>,
    pub calendar_event: Option<CalendarEventSummary>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct MeetingConsentPanelSessionState {
    pub snapshot: MeetingSessionSnapshot,
    pub standing_series_key: Option<String>,
    /// What this recording's disclosure is doing. The panel supplies the words
    /// for a `pending` one, because they come from the i18next catalog.
    pub disclosure: MeetingSessionDisclosure,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct MeetingMutationRequest {
    pub operation_id: MeetingOperationId,
    pub session_id: MeetingSessionId,
    pub expected_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct MeetingTitleSetRequest {
    pub operation_id: MeetingOperationId,
    pub session_id: MeetingSessionId,
    pub expected_revision: u64,
    pub title: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct MeetingSpeakerRenameRequest {
    pub operation_id: MeetingOperationId,
    pub session_id: MeetingSessionId,
    pub expected_revision: u64,
    pub speaker_id: SpeakerId,
    pub display_name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct MeetingSpeakerMergeRequest {
    pub operation_id: MeetingOperationId,
    pub session_id: MeetingSessionId,
    pub expected_revision: u64,
    pub source_speaker_id: SpeakerId,
    pub target_speaker_id: SpeakerId,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct MeetingSegmentEditRequest {
    pub operation_id: MeetingOperationId,
    pub session_id: MeetingSessionId,
    pub expected_revision: u64,
    pub segment_id: TranscriptSegmentId,
    pub replacement_text: String,
    pub removed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct MeetingNoteCreateRequest {
    pub operation_id: MeetingOperationId,
    pub session_id: MeetingSessionId,
    pub expected_revision: u64,
    pub start_offset_ns: Option<u64>,
    pub end_offset_ns: Option<u64>,
    pub body: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct MeetingNoteUpdateRequest {
    pub operation_id: MeetingOperationId,
    pub session_id: MeetingSessionId,
    pub expected_revision: u64,
    pub note_id: ManualNoteId,
    pub expected_note_revision: u64,
    pub start_offset_ns: Option<u64>,
    pub end_offset_ns: Option<u64>,
    pub body: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct MeetingNoteDeleteRequest {
    pub operation_id: MeetingOperationId,
    pub session_id: MeetingSessionId,
    pub expected_revision: u64,
    pub note_id: ManualNoteId,
    pub expected_note_revision: u64,
}

/// A save of the user's own notes layer. `expected_note_revision` guards the
/// notes row alone: this path never touches the session revision, because it
/// runs on an autosave timer while other edits may be in flight.
#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct MeetingUserNotesSaveRequest {
    pub session_id: MeetingSessionId,
    pub body: String,
    pub template: MeetingNotesTemplate,
    pub expected_note_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct MeetingActionItemDoneRequest {
    pub session_id: MeetingSessionId,
    pub artifact_id: MeetingArtifactId,
    pub action_index: u32,
    pub done: bool,
}

/// Save the notes layer and regenerate the meeting's notes from it in one
/// step, so the user cannot end up regenerating against a stale draft.
#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct MeetingReenhanceRequest {
    pub operation_id: MeetingOperationId,
    pub session_id: MeetingSessionId,
    pub expected_revision: u64,
    pub body: String,
    pub template: MeetingNotesTemplate,
    pub expected_note_revision: u64,
}

/// One recording to bring in as a meeting.
///
/// `title` and `recorded_at_utc_ms` are what the caller knows and the file may
/// not: a phone that recorded the audio knows when it did, and a picker on this
/// Mac knows only the file. Either falls back to the file itself — its name, and
/// the moment it was last written.
#[derive(Clone, Debug, Deserialize, Serialize, Type)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ImportRecordingRequest {
    pub path: PathBuf,
    pub title: Option<String>,
    pub recorded_at_utc_ms: Option<i64>,
    pub origin: RecordingOrigin,
}

/// Where an imported recording came from. Recorded on the track's descriptor,
/// so a meeting can say afterwards that its audio arrived from a paired phone
/// rather than from a file somebody dropped in.
#[derive(Clone, Debug, Deserialize, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RecordingOrigin {
    LocalFile,
    PairedDevice { device_id: String },
}

/// Which of the app's own stop controls a press came from.
///
/// [`MeetingStopCause::Operator`] covered both of them until this existed, and
/// the two are not the same event: the live meeting screen stops the capture
/// the operator is watching, while the consent panel stops one from a floating
/// window that may be the only surface open. A stop that loses its phase or
/// revision race still answers with a receipt the pressing surface renders as
/// done, so this is the field that says which button did nothing.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingStopSurface {
    /// The stop control on the live meeting screen in the main window.
    MeetingLive,
    /// The stop control in the consent panel.
    ConsentPanel,
}

impl MeetingStopSurface {
    const fn describe(self) -> &'static str {
        match self {
            Self::MeetingLive => "the live meeting screen",
            Self::ConsentPanel => "the consent panel",
        }
    }
}

/// Which surface asked for a capture to stop.
///
/// A stop is the one meeting command five unrelated surfaces issue, and until
/// this existed a committed one was unattributable: `stop` logged nothing and
/// filed every receipt under [`OperationActor::User`], including the auto-stops
/// detection issues on its own. That made the corpus's own audit trail wrong in
/// the one field a reader would trust, and it is why a capture that ended after
/// eleven seconds took a day to notice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeetingStopCause {
    /// A press on a stop control in one of the app's own windows, named by
    /// which window it was.
    Operator(MeetingStopSurface),
    /// The tray's Stop Meeting Notes item.
    Tray,
    /// A press on the auto-record card detection put on the panel. An operator
    /// press like [`Self::Operator`], reported apart from it because the card
    /// names one capture and can outlive it.
    RecordingCard,
    /// §5.5: detection ended the capture itself, on the trigger it names.
    Detection(super::detection::machine::StopTrigger),
}

impl MeetingStopCause {
    /// Who the receipt says did it. Only detection acts without a press.
    const fn actor(self) -> OperationActor {
        match self {
            Self::Operator(_) | Self::Tray | Self::RecordingCard => OperationActor::User,
            Self::Detection(_) => OperationActor::System,
        }
    }

    fn describe(self) -> String {
        match self {
            Self::Operator(surface) => surface.describe().to_string(),
            Self::Tray => "the tray".to_string(),
            Self::RecordingCard => "the recording card".to_string(),
            Self::Detection(trigger) => format!("detection on {}", trigger.as_str()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct MeetingMutationResult {
    pub receipt: OperationReceipt,
    pub snapshot: MeetingSessionSnapshot,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct MeetingRemovalResult {
    pub receipt: OperationReceipt,
    pub session_id: MeetingSessionId,
    pub removed: bool,
}

/// One lifecycle actor for all meetings. It serializes capture authority and
/// owns the global capture lease, while packet workers only persist bounded
/// source lanes and report health observations back through the store.
pub struct MeetingSessionManager {
    app: Option<AppHandle>,
    root: Option<PathBuf>,
    secrets: Arc<SecretManager>,
    suggestions: MeetingSuggestionService,
    sources: Mutex<Arc<dyn MeetingSourceProvider>>,
    store: Mutex<Option<Arc<MeetingStore>>>,
    recovery_complete: AtomicBool,
    /// UUID namespace for this launch's automatic recovery attempts. Drawn
    /// once per launch so an attempt's operation id is stable inside the
    /// launch — a second pass is deduplicated by the receipt the first one
    /// wrote — and different in the next one, which is what lets a meeting
    /// that failed today be tried again tomorrow.
    recovery_launch: Uuid,
    processing: Arc<MeetingProcessingService>,
    keep_awake: Mutex<MeetingKeepAwake>,
    actor: Mutex<ActorState>,
}

struct ActorState {
    active: Option<ActiveCapture>,
    tray_session_id: Option<MeetingSessionId>,
}

struct ActiveCapture {
    session_id: MeetingSessionId,
    sources: HashMap<SourceKind, ActiveSource>,
    /// The provisional transcript of this capture and the pass that fills it.
    /// It stops when this record is dropped, which is what every path that
    /// ends a capture — stop, discard, delete — already does.
    live: LiveTranscriptWorker,
}

struct ActiveSource {
    track_id: SourceTrackId,
    epoch: SourceEpoch,
    source: Box<dyn MeetingCaptureSource>,
    worker: TrackWorker,
}

struct TrackWorker {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<Result<(), StoreError>>>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RetentionSweepResult {
    pub due_sessions: usize,
    pub deleted_sessions: usize,
    pub failed_sessions: usize,
}

/// What one automatic recovery reprocess pass did. `skipped` counts meetings
/// deliberately left for the person, which is a normal outcome and not a
/// failure.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct RecoveryReprocessResult {
    pub attempted: usize,
    pub succeeded: usize,
    pub skipped: usize,
}

impl MeetingSessionManager {
    pub fn new(app: &AppHandle, secrets: Arc<SecretManager>) -> Self {
        let root = crate::portable::app_data_dir(app)
            .ok()
            .map(|directory| directory.join("meetings"));
        Self::with_parts(Some(app.clone()), root, secrets, Arc::new(NoCaptureSources))
    }

    pub fn with_parts(
        app: Option<AppHandle>,
        root: Option<PathBuf>,
        secrets: Arc<SecretManager>,
        source_provider: Arc<dyn MeetingSourceProvider>,
    ) -> Self {
        let processing = Arc::new(MeetingProcessingService::new(app.clone()));
        Self {
            app,
            root,
            secrets,
            suggestions: MeetingSuggestionService::new(Vec::new(), 120_000_000_000),
            sources: Mutex::new(source_provider),
            store: Mutex::new(None),
            recovery_complete: AtomicBool::new(false),
            recovery_launch: Uuid::new_v4(),
            processing,
            keep_awake: Mutex::new(MeetingKeepAwake::new()),
            actor: Mutex::new(ActorState {
                active: None,
                tray_session_id: None,
            }),
        }
    }

    pub fn set_source_provider(&self, source_provider: Arc<dyn MeetingSourceProvider>) {
        *self.sources_lock() = source_provider;
    }

    pub fn set_transcription_manager(
        &self,
        manager: Arc<crate::managers::transcription::TranscriptionManager>,
    ) {
        self.processing.set_transcription_manager(manager);
    }

    /// The generation service, for the sibling modules that write text through
    /// it — see [`super::prompts`]. Read-only: the slots inside it are set from
    /// here and from launch, and a second setter would be a second answer to
    /// which engine this app has.
    pub(super) fn processing(&self) -> &MeetingProcessingService {
        &self.processing
    }

    fn acquire_keep_awake(&self) {
        self.keep_awake
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .acquire();
    }

    fn release_keep_awake(&self) {
        self.keep_awake
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .release();
    }

    pub(crate) fn start_retention_sweeper(self: &Arc<Self>) {
        let manager = Arc::clone(self);
        thread::spawn(move || loop {
            thread::sleep(RETENTION_SWEEP_INTERVAL);
            if let Err(error) =
                tauri::async_runtime::block_on(manager.sweep_retention_at(utc_now_ms()))
            {
                log::warn!("Meeting retention sweep is unavailable: {error:?}");
            }
        });
    }

    pub async fn recover_at_startup(&self) -> Result<Vec<RecoveredMeeting>, MeetingCommandError> {
        self.recover_at_startup_at(utc_now_ms()).await
    }

    pub(crate) async fn recover_at_startup_at(
        &self,
        now_utc_ms: i64,
    ) -> Result<Vec<RecoveredMeeting>, MeetingCommandError> {
        let store = self.store().await?;
        let recovery = store.recover_interrupted().map_err(map_store_error)?;
        self.announce_recovery(&store, &recovery)?;
        if let Err(error) = self.sweep_retention_at(now_utc_ms).await {
            log::warn!("Meeting retention sweep is unavailable at startup: {error:?}");
        }
        self.recovery_complete.store(true, Ordering::Release);
        Ok(recovery.recovered)
    }

    /// One summary line per sweep, then one line per meeting the
    /// reconciliation touched, and one refreshed row per meeting in every open
    /// window. The summary is unconditional: a sweep that changed nothing used
    /// to log nothing at all, so a launch where the rows stayed wrong and a
    /// launch where the sweep never ran read identically. The per-meeting lines
    /// are the record of why a meeting is asking for attention, or gone, after
    /// a launch nobody watched.
    fn announce_recovery(
        &self,
        store: &MeetingStore,
        recovery: &InterruptedRecovery,
    ) -> Result<(), MeetingCommandError> {
        log::info!(
            "Meeting recovery sweep: {} interrupted, {} status healed, {} abandoned start gates discarded",
            recovery.recovered.len(),
            recovery.status_resolved.len(),
            recovery.discarded.len(),
        );
        for meeting in &recovery.recovered {
            log::info!(
                "Meeting recovery: {:?} was left in {:?} by an earlier launch and now needs review",
                meeting.session_id,
                meeting.prior_phase,
            );
        }
        for session_id in &recovery.status_resolved {
            log::info!(
                "Meeting recovery: {session_id:?} was still marked as processing by an earlier launch, now marked interrupted",
            );
        }
        for session_id in &recovery.discarded {
            log::info!(
                "Meeting recovery: {session_id:?} was a start gate an earlier launch left open and recorded nothing, discarded",
            );
            // The row is gone, so there is no snapshot to refresh — the
            // windows are told to drop it, exactly as a cancelled gate does.
            self.emit_removed(*session_id, 0);
        }
        for session_id in recovery
            .recovered
            .iter()
            .map(|meeting| meeting.session_id)
            .chain(recovery.status_resolved.iter().copied())
        {
            let snapshot = store
                .session_snapshot(session_id)
                .map_err(map_store_error)?;
            self.emit_session_changed(&snapshot);
        }
        Ok(())
    }

    /// The automatic reprocess pass, off the startup path. It owns a thread
    /// because every attempt waits for a whole engine run to finish, and
    /// startup must not.
    pub(crate) fn start_recovery_reprocess(self: &Arc<Self>, recovered: Vec<RecoveredMeeting>) {
        if recovered.is_empty() {
            return;
        }
        let manager = Arc::clone(self);
        thread::spawn(move || {
            let result = tauri::async_runtime::block_on(manager.reprocess_recovered(&recovered));
            log::info!(
                "Meeting recovery reprocess finished: {} attempted, {} finished, {} left for review",
                result.attempted,
                result.succeeded,
                result.skipped,
            );
        });
    }

    /// One automatic attempt per interrupted meeting per launch, in sequence.
    ///
    /// Each attempt is a whole transcription pass over one meeting, so they run
    /// one at a time: five at once at login would spend the same work while
    /// making the machine unusable. An attempt that fails leaves the meeting in
    /// recovery, where the next launch — or the person — can try it again.
    pub(crate) async fn reprocess_recovered(
        &self,
        recovered: &[RecoveredMeeting],
    ) -> RecoveryReprocessResult {
        let mut result = RecoveryReprocessResult::default();
        if recovered.is_empty() {
            return result;
        }
        let store = match self.store().await {
            Ok(store) => store,
            Err(error) => {
                log::warn!("Meeting recovery reprocess is unavailable: {error:?}");
                return result;
            }
        };
        for meeting in recovered {
            if let Some(reason) = self.reprocess_withheld(&store, meeting) {
                result.skipped += 1;
                log::info!(
                    "Meeting recovery left {:?} for review without an attempt: {reason}",
                    meeting.session_id,
                );
                continue;
            }
            let Ok(snapshot) = store.session_snapshot(meeting.session_id) else {
                result.skipped += 1;
                log::warn!(
                    "Meeting recovery could not read {:?} and made no attempt",
                    meeting.session_id,
                );
                continue;
            };
            let request = MeetingMutationRequest {
                operation_id: self.recovery_operation_id(meeting.session_id),
                session_id: meeting.session_id,
                expected_revision: snapshot.revision,
            };
            result.attempted += 1;
            match self
                .recovery_finalize_as(OperationActor::System, request)
                .await
            {
                Ok(outcome) if outcome.receipt.result == OperationResult::Committed => {
                    // In the app the run is a detached thread. Waiting for it
                    // is what makes the pass sequential.
                    self.processing.wait_for_job(meeting.session_id);
                    let status = store
                        .session_snapshot(meeting.session_id)
                        .map(|snapshot| snapshot.processing_status);
                    if status == Ok(ProcessingStatus::Succeeded) {
                        result.succeeded += 1;
                    }
                    log::info!(
                        "Meeting recovery reprocessed {:?}: {:?}",
                        meeting.session_id,
                        status,
                    );
                }
                Ok(outcome) => log::info!(
                    "Meeting recovery attempt on {:?} was refused: {:?}",
                    meeting.session_id,
                    outcome.receipt.reason_codes,
                ),
                Err(error) => log::warn!(
                    "Meeting recovery attempt on {:?} did not start: {error:?}",
                    meeting.session_id,
                ),
            }
        }
        result
    }

    /// Why this meeting is not something to reprocess unasked, if it is not.
    ///
    /// Only a meeting whose launch died at or after the stop is eligible: its
    /// audio is closed and only the transcript is missing, which is exactly
    /// what a reprocess rebuilds. Every other shape is a decision for the
    /// person who was in the room — a capture cut mid-recording, a meeting
    /// missing audio on disk, one bound for a remote destination, or one whose
    /// local model is not installed. Withholding costs the meeting nothing: it
    /// keeps its place in recovery with Retry beside it.
    fn reprocess_withheld(
        &self,
        store: &MeetingStore,
        meeting: &RecoveredMeeting,
    ) -> Option<&'static str> {
        if !matches!(
            meeting.prior_phase,
            MeetingPhase::Stopping | MeetingPhase::Processing
        ) {
            return Some("the recording was interrupted before it was stopped");
        }
        match store.has_missing_record_gap(meeting.session_id) {
            Ok(true) => return Some("some of its audio is no longer on disk"),
            Err(error) => {
                log::warn!("Meeting recovery could not read source gaps: {error:?}");
                return Some("its audio could not be checked");
            }
            Ok(false) => {}
        }
        match store.processing_plan(meeting.session_id) {
            Ok(plan) if matches!(plan.destination, ProcessingDestination::Local) => {}
            Ok(_) => return Some("it was set up to be processed remotely"),
            Err(error) => {
                log::warn!("Meeting recovery could not read the processing plan: {error:?}");
                return Some("its processing plan could not be read");
            }
        }
        if self.processing.local_processing_availability() != SourceAvailability::Available {
            return Some("the local model is not available on this launch");
        }
        None
    }

    /// An automatic attempt's operation id, namespaced to this launch. Stable
    /// inside the launch, so a second pass is deduplicated by the receipt the
    /// first one wrote; fresh in the next launch, so a meeting that failed
    /// today is tried again tomorrow.
    fn recovery_operation_id(&self, session_id: MeetingSessionId) -> MeetingOperationId {
        MeetingOperationId::from_uuid(Uuid::new_v5(
            &self.recovery_launch,
            session_id.uuid().as_bytes(),
        ))
    }

    /// The retention sweep: meetings past their keep-for, then deleted meetings
    /// past their undo.
    ///
    /// Both horizons are swept here because both are "this app forgetting
    /// something on a clock", and a second timer for the bin would be a second
    /// place a forgetting can fail to happen.
    pub(crate) async fn sweep_retention_at(
        &self,
        now_utc_ms: i64,
    ) -> Result<RetentionSweepResult, MeetingCommandError> {
        let store = self.store().await?;
        match store.purge_expired_trash(now_utc_ms) {
            Ok(0) => {}
            Ok(purged) => log::info!("Meeting retention sweep purged {purged} deleted meetings"),
            Err(error) => log::warn!("Meeting trash could not be purged: {error:?}"),
        }
        let due_sessions = store
            .due_retention_sessions(now_utc_ms)
            .map_err(map_store_error)?;
        let mut result = RetentionSweepResult {
            due_sessions: due_sessions.len(),
            deleted_sessions: 0,
            failed_sessions: 0,
        };
        for due in due_sessions {
            match self
                .delete_with_cause(
                    MeetingMutationRequest {
                        operation_id: retention_operation_id(due.session_id),
                        session_id: due.session_id,
                        expected_revision: due.revision,
                    },
                    DeletionCause::Retention,
                )
                .await
            {
                Ok(removal) if removal.removed => result.deleted_sessions += 1,
                Ok(_) => {}
                Err(error) => {
                    result.failed_sessions += 1;
                    log::warn!("Meeting retention deletion failed: {error:?}");
                }
            }
        }
        Ok(result)
    }

    /// The meetings a person deleted and could still get back.
    pub async fn trash_list(&self) -> Result<Vec<MeetingTrashEntry>, MeetingCommandError> {
        self.store()
            .await?
            .meeting_trash(utc_now_ms())
            .map_err(map_store_error)
    }

    /// Undo one deletion. The meeting comes back where it was, and the windows
    /// are told about it the same way any other change to a meeting is.
    pub async fn trash_restore(
        &self,
        job_id: MeetingDeletionJobId,
    ) -> Result<MeetingSessionSnapshot, MeetingCommandError> {
        let store = self.store().await?;
        let session_id = store
            .restore_trashed_meeting(job_id, utc_now_ms())
            .map_err(map_store_error)?;
        let snapshot = store
            .session_snapshot(session_id)
            .map_err(map_store_error)?;
        self.emit_session_changed(&snapshot);
        Ok(snapshot)
    }

    pub async fn tray_snapshot(
        &self,
    ) -> Result<Option<MeetingSessionSnapshot>, MeetingCommandError> {
        let session_id = {
            let actor = self.actor_lock();
            actor
                .active
                .as_ref()
                .map(|active| active.session_id)
                .or(actor.tray_session_id)
        };
        let Some(session_id) = session_id else {
            return Ok(None);
        };
        let store = self.store().await?;
        match store.session_snapshot(session_id) {
            Ok(snapshot) => Ok(Some(snapshot)),
            Err(StoreError::NotFound) => {
                let mut actor = self.actor_lock();
                if actor.tray_session_id == Some(session_id) {
                    actor.tray_session_id = None;
                }
                Ok(None)
            }
            Err(error) => Err(map_store_error(error)),
        }
    }

    pub fn suggestion_sink(self: &Arc<Self>) -> Arc<dyn MeetingSuggestionSink> {
        Arc::new(SessionSuggestionSink {
            manager: Arc::clone(self),
        })
    }

    pub fn suggestion_service(&self) -> MeetingSuggestionService {
        self.suggestions.clone()
    }

    pub fn suggestions_list(&self, now_ns: u64) -> Vec<MeetingSuggestion> {
        self.suggestions.list(now_ns)
    }

    pub async fn create_preflight(
        &self,
        request: MeetingPreflightCreateRequest,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        self.create_preflight_with_calendar(request, None).await
    }

    pub async fn create_preflight_with_calendar(
        &self,
        request: MeetingPreflightCreateRequest,
        calendar_event: Option<CalendarEventSummary>,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        match (&request.calendar_event_key, &calendar_event) {
            (None, None) => {}
            (Some(event_key), Some(event)) if event_key == &event.event_key => {}
            _ => return Err(MeetingCommandError::InvalidRequest),
        }
        if request.expected_revision != 0
            || request.title.trim().is_empty()
            || request.requested_sources.is_empty()
            || !request
                .required_sources
                .iter()
                .all(|source| request.requested_sources.contains(source))
        {
            return Err(MeetingCommandError::InvalidRequest);
        }
        if let Some(suggestion_id) = request.suggestion_id {
            self.suggestions
                .take_for_preflight(suggestion_id, host_monotonic_now_ns())
                .ok_or(MeetingCommandError::ConsentStale)?;
        }
        validate_destination(
            &request.destination,
            request.remote_acknowledgement.as_ref(),
        )?;
        let store = self.store().await?;
        if let Some(receipt) = store
            .operation_receipt(request.operation_id)
            .map_err(map_store_error)?
        {
            let session_id = receipt.session_id.ok_or(MeetingCommandError::NotFound)?;
            if let Some(event) = calendar_event.as_ref() {
                store
                    .remember_calendar_facts(session_id, event)
                    .map_err(map_store_error)?;
            }
            return self.result_for_receipt(store, receipt, session_id);
        }
        let session_id = MeetingSessionId::new();
        let preflight = self.build_preflight_snapshot(session_id, &request);
        let (retention_policy, _) = store.default_retention_policy().map_err(map_store_error)?;
        let receipt = store
            .create_preflight(
                StoreMutation {
                    operation_id: request.operation_id,
                    requested_at_utc_ms: utc_now_ms(),
                    session_id,
                    expected_revision: request.expected_revision,
                    command: MeetingCommandKind::PreflightCreate,
                },
                request.title,
                request.origin,
                preflight,
                retention_policy,
            )
            .map_err(map_store_error)?;
        if let Some(event) = calendar_event.as_ref() {
            store
                .remember_calendar_facts(session_id, event)
                .map_err(map_store_error)?;
        }
        let result = self.result_for_receipt(store, receipt, session_id)?;
        self.emit_session_changed(&result.snapshot);
        Ok(result)
    }

    pub async fn create_manual_preflight_from_tray(
        &self,
    ) -> Result<MeetingSessionSnapshot, MeetingCommandError> {
        let result = self
            .create_preflight(MeetingPreflightCreateRequest {
                operation_id: MeetingOperationId::new(),
                expected_revision: 0,
                title: MANUAL_DEFAULT_TITLE.to_string(),
                origin: MeetingOrigin::Manual,
                suggestion_id: None,
                calendar_event_key: None,
                requested_sources: SourceKind::ALL.to_vec(),
                required_sources: SourceKind::ALL.to_vec(),
                accepted_known_missing_sources: Vec::new(),
                degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
                destination: ProcessingDestination::Local,
                remote_acknowledgement: None,
                microphone_device_uid: None,
                frozen_system_audio_application_bundle_ids: Vec::new(),
            })
            .await?;
        let snapshot = result.snapshot;
        self.actor_lock().tray_session_id = Some(snapshot.session_id);
        Ok(snapshot)
    }

    pub async fn refresh_preflight(
        &self,
        request: MeetingPreflightRefreshRequest,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        let store = self.store().await?;
        if let Some(receipt) = store
            .operation_receipt(request.operation_id)
            .map_err(map_store_error)?
        {
            return self.result_for_receipt(store, receipt, request.session_id);
        }
        let current = store
            .preflight_snapshot(request.session_id)
            .map_err(map_store_error)?;
        let refreshed_request = MeetingPreflightCreateRequest {
            operation_id: request.operation_id,
            expected_revision: request.expected_revision.saturating_add(1),
            title: current.proposed_title.clone(),
            origin: current.origin,
            suggestion_id: None,
            calendar_event_key: None,
            requested_sources: current
                .sources
                .iter()
                .map(|source| source.source_kind)
                .collect(),
            required_sources: current
                .sources
                .iter()
                .filter(|source| source.required)
                .map(|source| source.source_kind)
                .collect(),
            accepted_known_missing_sources: current.accepted_known_missing_sources.clone(),
            degraded_start_policy: current.degraded_start_policy,
            destination: current.destination.clone(),
            remote_acknowledgement: None,
            microphone_device_uid: current.microphone_device_uid.clone(),
            frozen_system_audio_application_bundle_ids: current
                .frozen_system_audio_application_bundle_ids
                .clone(),
        };
        let refreshed = self.build_preflight_snapshot(request.session_id, &refreshed_request);
        let receipt = store
            .refresh_preflight(
                request.operation_id,
                utc_now_ms(),
                request.session_id,
                request.expected_revision,
                refreshed,
            )
            .map_err(map_store_error)?;
        let result = self.result_for_receipt(store, receipt, request.session_id)?;
        self.emit_session_changed(&result.snapshot);
        Ok(result)
    }

    pub async fn cancel_preflight(
        &self,
        request: MeetingMutationRequest,
    ) -> Result<OperationReceipt, MeetingCommandError> {
        let store = self.store().await?;
        let receipt = store
            .cancel_preflight(
                request.operation_id,
                utc_now_ms(),
                request.session_id,
                request.expected_revision,
            )
            .map_err(map_store_error)?;
        if receipt.result == OperationResult::Committed {
            let mut actor = self.actor_lock();
            if actor.tray_session_id == Some(request.session_id) {
                actor.tray_session_id = None;
            }
            drop(actor);
            self.emit_removed(request.session_id, request.expected_revision);
        }
        Ok(receipt)
    }

    pub async fn start_from_consent_panel(
        &self,
        context: &MeetingDetectionStartContext,
        request: MeetingConsentPanelStartRequest,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        request_system_audio_permission_once();
        let preflight = self
            .create_detection_preflight(context, request.operation_id, &request.consent)
            .await?;
        if request.always_record_series && context.calendar_event.is_none() {
            self.show_consent_gate(&preflight.snapshot);
            return Err(MeetingCommandError::InvalidRequest);
        }
        let start = self
            .start(MeetingStartRequest {
                operation_id: MeetingOperationId::new(),
                session_id: preflight.snapshot.session_id,
                expected_revision: preflight.snapshot.revision,
                consent: request.consent,
            })
            .await;
        let result = self.finish_detection_start(&preflight.snapshot, start)?;
        if result.snapshot.phase == MeetingPhase::CapturingRecording {
            self.arm_disclosure(
                result.snapshot.session_id,
                &result.snapshot.title,
                context.calendar_event.as_ref(),
                request.announce_in_chat,
                true,
            )
            .await;
        }
        Ok(result)
    }

    /// Note that a recording that just started owes the room a disclosure, and
    /// — from the panel only — remember the decision for the series.
    ///
    /// Nothing is posted here. The panel is the surface that owns the words, and
    /// it asks for the paste as soon as it sees a `pending` disclosure on the
    /// live meeting.
    ///
    /// A failure to remember or to arm is logged and dropped: the recording is
    /// already running, and a start that failed because a courtesy line could
    /// not be arranged would be the worst possible trade.
    async fn arm_disclosure(
        &self,
        session_id: MeetingSessionId,
        title: &str,
        calendar_event: Option<&CalendarEventSummary>,
        announce_in_chat: bool,
        remember_for_series: bool,
    ) {
        let Ok(store) = self.store().await else {
            return;
        };
        if remember_for_series {
            if let Some(event) = calendar_event {
                if let Err(error) = store.remember_series_announce(
                    &event.series_key,
                    announce_in_chat,
                    utc_now_ms(),
                ) {
                    log::warn!("A series could not remember its announce decision: {error:?}");
                }
            }
        }
        if !announce_in_chat {
            return;
        }
        if let Err(error) =
            store.request_session_disclosure(session_id, notetaker(calendar_event, title))
        {
            log::warn!("Meeting {session_id:?} could not arm its disclosure: {error:?}");
        }
    }

    pub(crate) async fn live_series_consent(
        &self,
        series_key: &str,
    ) -> Result<Option<super::store::StandingSeriesConsent>, MeetingCommandError> {
        self.store()
            .await?
            .live_series_consent(series_key)
            .map_err(map_store_error)
    }

    pub(crate) async fn grant_panel_series_consent(
        &self,
        context: &MeetingDetectionStartContext,
        consent: &MeetingConsentInput,
    ) -> Result<(), MeetingCommandError> {
        let event = context
            .calendar_event
            .as_ref()
            .ok_or(MeetingCommandError::InvalidRequest)?;
        let acknowledged_sources = acknowledged_sources(consent);
        self.store()
            .await?
            .grant_series_consent(
                &event.series_key,
                consent.policy_version,
                &acknowledged_sources,
                utc_now_ms(),
            )
            .map(|_| ())
            .map_err(map_store_error)
    }

    pub(crate) async fn start_from_standing_series(
        &self,
        context: &MeetingDetectionStartContext,
        standing: super::store::StandingSeriesConsent,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        let consent = MeetingConsentInput {
            policy_version: standing.policy_version,
            microphone_acknowledged: standing
                .acknowledged_sources
                .contains(&SourceKind::Microphone),
            system_audio_acknowledged: standing
                .acknowledged_sources
                .contains(&SourceKind::SystemAudio),
            known_missing_sources_acknowledged: Vec::new(),
            degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
            destination: ProcessingDestination::Local,
            remote_acknowledgement: None,
        };
        let preflight = self
            .create_detection_preflight(context, MeetingOperationId::new(), &consent)
            .await?;
        let provenance = MeetingConsentProvenance::StandingSeries {
            series_key: standing.series_key.clone(),
            granted_at_utc_ms: standing.granted_at_utc_ms,
        };
        let start = self
            .start_with_provenance(
                MeetingStartRequest {
                    operation_id: MeetingOperationId::new(),
                    session_id: preflight.snapshot.session_id,
                    expected_revision: preflight.snapshot.revision,
                    consent,
                },
                provenance,
            )
            .await;
        let result = self.finish_detection_start(&preflight.snapshot, start)?;
        // An occurrence nobody was asked about still announces itself when its
        // series decided to. That is the whole point of remembering the decision:
        // the meeting the operator is not sitting in front of is the one where a
        // silent recording would be least expected.
        if result.snapshot.phase == MeetingPhase::CapturingRecording {
            let announce = self.series_announces_in_chat(&standing.series_key).await;
            self.arm_disclosure(
                result.snapshot.session_id,
                &result.snapshot.title,
                context.calendar_event.as_ref(),
                announce,
                false,
            )
            .await;
        }
        Ok(result)
    }

    /// What one series remembers about announcing itself. False when it cannot
    /// be read: a recording that failed to learn the decision must not announce.
    pub async fn series_announces_in_chat(&self, series_key: &str) -> bool {
        match self.store().await {
            Ok(store) => store
                .series_preferences(series_key)
                .map(|preferences| preferences.announce_in_chat)
                .unwrap_or(false),
            Err(_) => false,
        }
    }

    /// Starts a call the operator granted standing consent to, citing that
    /// grant on the receipt.
    ///
    /// The sibling of `start_from_standing_series`, and deliberately not a
    /// second implementation of it: both build a `MeetingConsentInput` from a
    /// grant somebody already gave, both go through `create_detection_preflight`
    /// and `start_with_provenance`, and neither can start anything the consent
    /// screen would have refused. The one difference is where the grant lives.
    /// A series grant is a store row, so `start_with_plan_and_consent`
    /// revalidates it inside the start transaction; an app grant is a settings
    /// entry, which no SQL transaction can read, so `bundle_id` arrives here
    /// already checked against the live setting by the only caller that has it.
    /// The window that leaves open is one tick of the operator switching the
    /// grant off mid-call, and the recording card's own "Don't record this app
    /// automatically" is the recovery.
    pub(crate) async fn start_from_standing_app(
        &self,
        context: &MeetingDetectionStartContext,
        bundle_id: String,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        let consent = MeetingConsentInput {
            policy_version: MEETING_CONSENT_POLICY_VERSION,
            // Both sources, because the switch says both: the sentence under
            // the app list (`settingsV2.apps.autoRecordSources`) names the
            // microphone and the Mac's audio output, and switching "Record
            // automatically" on beneath it is the acknowledgement this row
            // records. A series grant stores what the operator ticked on the
            // consent screen; the app grant has no per-source choice to store.
            microphone_acknowledged: true,
            system_audio_acknowledged: true,
            known_missing_sources_acknowledged: Vec::new(),
            degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
            destination: ProcessingDestination::Local,
            remote_acknowledgement: None,
        };
        let preflight = self
            .create_detection_preflight(context, MeetingOperationId::new(), &consent)
            .await?;
        let start = self
            .start_with_provenance(
                MeetingStartRequest {
                    operation_id: MeetingOperationId::new(),
                    session_id: preflight.snapshot.session_id,
                    expected_revision: preflight.snapshot.revision,
                    consent,
                },
                MeetingConsentProvenance::StandingApp { bundle_id },
            )
            .await;
        self.finish_detection_start(&preflight.snapshot, start)
    }

    fn finish_detection_start(
        &self,
        preflight: &MeetingSessionSnapshot,
        start: Result<MeetingMutationResult, MeetingCommandError>,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        match start {
            Ok(result) if result.snapshot.phase == MeetingPhase::CapturingRecording => Ok(result),
            Ok(result) => {
                self.show_consent_gate(&result.snapshot);
                Ok(result)
            }
            Err(error) => {
                self.show_consent_gate(preflight);
                Err(error)
            }
        }
    }

    async fn create_detection_preflight(
        &self,
        context: &MeetingDetectionStartContext,
        operation_id: MeetingOperationId,
        consent: &MeetingConsentInput,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        let calendar_event_key = context
            .calendar_event
            .as_ref()
            .map(|event| event.event_key.clone());
        let result = self
            .create_preflight_with_calendar(
                MeetingPreflightCreateRequest {
                    operation_id,
                    expected_revision: 0,
                    title: context.title.clone(),
                    origin: MeetingOrigin::Suggestion,
                    suggestion_id: None,
                    calendar_event_key,
                    requested_sources: SourceKind::ALL.to_vec(),
                    required_sources: SourceKind::ALL.to_vec(),
                    accepted_known_missing_sources: consent
                        .known_missing_sources_acknowledged
                        .clone(),
                    degraded_start_policy: consent.degraded_start_policy,
                    destination: consent.destination.clone(),
                    remote_acknowledgement: consent.remote_acknowledgement.clone(),
                    microphone_device_uid: None,
                    frozen_system_audio_application_bundle_ids: context
                        .trigger_bundle_id
                        .iter()
                        .cloned()
                        .collect(),
                },
                context.calendar_event.clone(),
            )
            .await;
        if result.is_err() {
            if let Some(app) = self.app.as_ref() {
                crate::show_meeting_destination(app, MeetingNavigationDestination::Preflight, None);
            }
        }
        result
    }

    fn show_consent_gate(&self, snapshot: &MeetingSessionSnapshot) {
        if let Some(app) = self.app.as_ref() {
            crate::show_meeting_destination(
                app,
                MeetingNavigationDestination::Preflight,
                Some(snapshot),
            );
        }
    }

    pub async fn consent_panel_introduction_needed(&self) -> bool {
        self.store()
            .await
            .and_then(|store| {
                store
                    .consent_panel_introduction_needed()
                    .map_err(map_store_error)
            })
            .unwrap_or(false)
    }

    pub async fn mark_consent_panel_introduction_shown(&self) {
        if let Ok(store) = self.store().await {
            if let Err(error) = store.mark_consent_panel_introduction_shown(utc_now_ms()) {
                log::warn!("Meeting consent introduction state could not be saved: {error:?}");
            }
        }
    }

    pub async fn consent_panel_active_state(
        &self,
    ) -> Result<Option<MeetingConsentPanelSessionState>, MeetingCommandError> {
        let Some(snapshot) = self.tray_snapshot().await? else {
            return Ok(None);
        };
        if !matches!(
            snapshot.phase,
            MeetingPhase::CapturingRecording
                | MeetingPhase::CapturingPausing
                | MeetingPhase::CapturingPaused
                | MeetingPhase::CapturingResuming
        ) {
            return Ok(None);
        }
        let store = self.store().await?;
        let standing_series_key = store
            .latest_consent_for_session(snapshot.session_id)
            .map_err(map_store_error)?
            .and_then(|consent| match consent.provenance {
                MeetingConsentProvenance::StandingSeries { series_key, .. } => Some(series_key),
                MeetingConsentProvenance::StandingApp { .. }
                | MeetingConsentProvenance::PairedDevice { .. }
                | MeetingConsentProvenance::Direct => None,
            });
        let disclosure = store
            .session_disclosure(snapshot.session_id)
            .map_err(map_store_error)?;
        Ok(Some(MeetingConsentPanelSessionState {
            snapshot,
            standing_series_key,
            disclosure,
        }))
    }

    /// Post the recording disclosure into whatever the frontmost application has
    /// focused, once, and write down what happened.
    ///
    /// The line is the caller's because it is words a person reads. The refusal
    /// case is ordinary and expected: a target with no composer focused — a
    /// document, a browser, Sona's own panel — cannot accept an insertion, and
    /// the receipt says so rather than the app pressing ⌘V at it and hoping.
    pub async fn announce_disclosure(
        &self,
        session_id: MeetingSessionId,
        line: String,
    ) -> Result<MeetingSessionDisclosure, MeetingCommandError> {
        let store = self.store().await?;
        let held = store
            .session_disclosure(session_id)
            .map_err(map_store_error)?;
        match held {
            // Nobody asked for one, so nothing is pasted. Not an error: the
            // panel re-reads the live meeting on every change, and asking about
            // a meeting that is not announcing itself is a no-op.
            MeetingSessionDisclosure::NotAsked => Ok(MeetingSessionDisclosure::NotAsked),
            MeetingSessionDisclosure::Attempted { .. } => Ok(held),
            MeetingSessionDisclosure::Pending { .. } => {
                let receipt = crate::delivery::announce(&line);
                store
                    .record_session_disclosure(session_id, &receipt)
                    .map_err(map_store_error)
            }
        }
    }

    pub async fn forget_active_series(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<bool, MeetingCommandError> {
        let store = self.store().await?;
        let consent = store
            .latest_consent_for_session(session_id)
            .map_err(map_store_error)?
            .ok_or(MeetingCommandError::NotFound)?;
        let MeetingConsentProvenance::StandingSeries { series_key, .. } = consent.provenance else {
            return Err(MeetingCommandError::InvalidRequest);
        };
        store
            .revoke_series_consent(&series_key, utc_now_ms())
            .map_err(map_store_error)
    }

    pub async fn start(
        &self,
        request: MeetingStartRequest,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        self.start_with_provenance(request, MeetingConsentProvenance::Direct)
            .await
    }

    async fn start_with_provenance(
        &self,
        request: MeetingStartRequest,
        provenance: MeetingConsentProvenance,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        let store = self.store().await?;
        if let Some(receipt) = store
            .operation_receipt(request.operation_id)
            .map_err(map_store_error)?
        {
            return self.result_for_receipt(store, receipt, request.session_id);
        }
        let mut actor = self.actor_lock();
        if actor.active.is_some() {
            return Err(MeetingCommandError::CaptureLeaseBusy);
        }
        let preflight = store
            .preflight_snapshot(request.session_id)
            .map_err(map_store_error)?;
        validate_consent(&preflight, &request.consent)?;
        let attempt_number = store
            .next_plan_attempt(request.session_id)
            .map_err(map_store_error)?;
        let plan = self.build_plan(
            request.session_id,
            request.expected_revision,
            attempt_number,
            &preflight,
            &request.consent,
            &store,
        )?;
        let consent = MeetingConsent {
            consent_id: plan.consent_id,
            session_id: request.session_id,
            attempt_number,
            preflight_revision: request.expected_revision,
            policy_version: request.consent.policy_version,
            acknowledged_at_utc_ms: utc_now_ms(),
            provenance,
            microphone_acknowledged: request.consent.microphone_acknowledged,
            system_audio_acknowledged: request.consent.system_audio_acknowledged,
            known_missing_sources_acknowledged: request
                .consent
                .known_missing_sources_acknowledged
                .clone(),
            degraded_start_policy: request.consent.degraded_start_policy,
            destination: request.consent.destination.clone(),
            remote_acknowledgement: request.consent.remote_acknowledgement.clone(),
        };
        let receipt = store
            .start_with_plan_and_consent(
                request.operation_id,
                utc_now_ms(),
                &plan,
                &consent,
                request.expected_revision,
            )
            .map_err(map_store_error)?;
        if receipt.result != OperationResult::Committed {
            drop(actor);
            return self.result_for_receipt(store, receipt, request.session_id);
        }
        self.acquire_keep_awake();
        self.processing.set_capture_active(true);

        let mut active_sources = HashMap::new();
        let source_provider = Arc::clone(&*self.sources_lock());
        // Two devices cannot be handed the same instant, but they can be asked
        // in the same one. ScreenCaptureKit's start blocks on
        // SCShareableContent and startCapture for hundreds of milliseconds, so
        // starting the sources in sequence put that whole wait into the far end
        // of the meeting - measured at 196 to 1,566 ms behind the microphone.
        // Each source owns its own sink and worker and shares nothing with its
        // sibling; the store is only touched after the join.
        let mut prepared = Vec::new();
        for source_kind in &plan.requested_sources {
            let probe = source_provider.probe(*source_kind);
            if probe.availability != SourceAvailability::Available {
                continue;
            }
            let Ok(source) = source_provider.acquire(*source_kind) else {
                continue;
            };
            let track_id = SourceTrackId::new();
            let (sink, lane_reader) = PacketSink::new(
                track_id,
                usize::try_from(lane_sample_capacity(
                    plan.storage.source_lane_sample_capacity,
                    probe.negotiated_format,
                ))
                .map_err(|_| MeetingCommandError::InvalidRequest)?,
                usize::try_from(plan.storage.source_lane_descriptor_capacity)
                    .map_err(|_| MeetingCommandError::InvalidRequest)?,
            );
            prepared.push((*source_kind, track_id, source, sink, lane_reader));
        }
        let anchor = plan.session_clock_anchor;
        let started =
            thread::scope(|scope| {
                let attempts = prepared
                    .into_iter()
                    .map(|(source_kind, track_id, mut source, sink, lane_reader)| {
                        let source_plan = SourceStartPlan {
                            session_id: request.session_id,
                            track_id,
                            source_kind,
                            required: plan.required_sources.contains(&source_kind),
                            frozen_application_bundle_ids: plan
                                .frozen_system_audio_application_bundle_ids
                                .clone(),
                            source_epoch: SourceEpoch::new(0),
                        };
                        let handle =
                            scope.spawn(move || (source.start(source_plan, anchor, sink), source));
                        (source_kind, track_id, lane_reader, handle)
                    })
                    .collect::<Vec<_>>();
                attempts
                    .into_iter()
                    .filter_map(|(source_kind, track_id, lane_reader, handle)| {
                        let (report, source) = handle.join().ok()?;
                        let report = report.ok()?;
                        (report.track_id == track_id && report.source_kind == source_kind)
                            .then_some((source_kind, track_id, source, lane_reader, report))
                    })
                    .collect::<Vec<_>>()
            });
        for (source_kind, track_id, source, lane_reader, report) in started {
            store
                .create_track(TrackCreation {
                    session_id: request.session_id,
                    plan_id: plan.plan_id,
                    source_kind,
                    required: plan.required_sources.contains(&source_kind),
                    requested: true,
                    descriptor_json: "{}",
                    report,
                })
                .map_err(map_store_error)?;
            let writer = store
                .open_track_writer(request.session_id, track_id, plan.storage.clone())
                .map_err(map_store_error)?;
            let worker = TrackWorker::start(Arc::clone(&store), writer, lane_reader, report);
            active_sources.insert(
                source_kind,
                ActiveSource {
                    track_id,
                    epoch: report.epoch,
                    source,
                    worker,
                },
            );
        }

        let missing_required_source = plan
            .required_sources
            .iter()
            .any(|source_kind| !active_sources.contains_key(source_kind));
        if active_sources.is_empty()
            || (missing_required_source
                && plan.degraded_start_policy == DegradedStartPolicy::AbortIfRequiredSourceFails)
        {
            for source in active_sources.values_mut() {
                let _ = source.source.abort();
                let _ = source.worker.stop();
            }
            let snapshot = store
                .session_snapshot(request.session_id)
                .map_err(map_store_error)?;
            store
                .transition(StoreTransition {
                    operation_id: None,
                    actor: OperationActor::System,
                    command: MeetingCommandKind::Start,
                    requested_at_utc_ms: utc_now_ms(),
                    session_id: request.session_id,
                    expected_revision: snapshot.revision,
                    allowed_from: &[MeetingPhase::Starting],
                    next_phase: MeetingPhase::Preflight,
                    event_kind: "source_start_incomplete",
                    reason_codes: vec![MeetingReasonCode::SourceStartFailed],
                })
                .map_err(map_store_error)?;
            self.processing.set_capture_active(false);
            self.release_keep_awake();
            drop(actor);
            let snapshot = store
                .session_snapshot(request.session_id)
                .map_err(map_store_error)?;
            self.emit_session_changed(&snapshot);
            return Ok(MeetingMutationResult { receipt, snapshot });
        }

        store
            .open_capture_window(request.session_id, 0)
            .map_err(map_store_error)?;
        let starting_snapshot = store
            .session_snapshot(request.session_id)
            .map_err(map_store_error)?;
        store
            .transition(StoreTransition {
                operation_id: None,
                actor: OperationActor::System,
                command: MeetingCommandKind::Start,
                requested_at_utc_ms: utc_now_ms(),
                session_id: request.session_id,
                expected_revision: starting_snapshot.revision,
                allowed_from: &[MeetingPhase::Starting],
                next_phase: MeetingPhase::CapturingRecording,
                event_kind: "capture_started",
                reason_codes: Vec::new(),
            })
            .map_err(map_store_error)?;
        actor.active = Some(ActiveCapture {
            session_id: request.session_id,
            sources: active_sources,
            live: LiveTranscriptWorker::start(
                Arc::clone(&self.processing),
                Arc::clone(&store),
                request.session_id,
            ),
        });
        actor.tray_session_id = None;
        drop(actor);
        let snapshot = store
            .session_snapshot(request.session_id)
            .map_err(map_store_error)?;
        self.emit_session_changed(&snapshot);
        self.record_meeting_started(Arc::clone(&store), request.session_id);
        Ok(MeetingMutationResult { receipt, snapshot })
    }

    pub async fn pause(
        &self,
        request: MeetingMutationRequest,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        let store = self.store().await?;
        if let Some(receipt) = store
            .operation_receipt(request.operation_id)
            .map_err(map_store_error)?
        {
            return self.result_for_receipt(store, receipt, request.session_id);
        }
        let mut actor = self.actor_lock();
        let active = actor
            .active
            .as_mut()
            .filter(|active| active.session_id == request.session_id)
            .ok_or(MeetingCommandError::NotFound)?;
        let receipt = required_transition(
            &store,
            &request,
            MeetingCommandKind::Pause,
            &[MeetingPhase::CapturingRecording],
            MeetingPhase::CapturingPausing,
            "pause_requested",
        )?;
        if receipt.result != OperationResult::Committed {
            drop(actor);
            return self.result_for_receipt(store, receipt, request.session_id);
        }
        for source in active.sources.values_mut() {
            if source.source.pause().is_err() {
                store
                    .record_gap(&SourceGap {
                        track_id: source.track_id,
                        epoch: source.epoch,
                        start_offset_ns: None,
                        end_offset_ns: None,
                        reason: SourceGapReason::SourceStopped,
                        dropped_frames: None,
                    })
                    .map_err(map_store_error)?;
                store
                    .update_track_health(request.session_id, source.track_id, SourceHealth::Failed)
                    .map_err(map_store_error)?;
            }
        }
        let snapshot = store
            .session_snapshot(request.session_id)
            .map_err(map_store_error)?;
        let end = snapshot.elapsed_offset_ns.unwrap_or(0);
        store
            .close_open_capture_window(request.session_id, end, "paused")
            .map_err(map_store_error)?;
        let pausing = store
            .session_snapshot(request.session_id)
            .map_err(map_store_error)?;
        store
            .transition(StoreTransition {
                operation_id: None,
                actor: OperationActor::System,
                command: MeetingCommandKind::Pause,
                requested_at_utc_ms: utc_now_ms(),
                session_id: request.session_id,
                expected_revision: pausing.revision,
                allowed_from: &[MeetingPhase::CapturingPausing],
                next_phase: MeetingPhase::CapturingPaused,
                event_kind: "pause_confirmed",
                reason_codes: Vec::new(),
            })
            .map_err(map_store_error)?;
        self.release_keep_awake();
        drop(actor);
        let snapshot = store
            .session_snapshot(request.session_id)
            .map_err(map_store_error)?;
        self.emit_session_changed(&snapshot);
        Ok(MeetingMutationResult { receipt, snapshot })
    }

    pub async fn resume(
        &self,
        request: MeetingMutationRequest,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        let store = self.store().await?;
        if let Some(receipt) = store
            .operation_receipt(request.operation_id)
            .map_err(map_store_error)?
        {
            return self.result_for_receipt(store, receipt, request.session_id);
        }
        let mut actor = self.actor_lock();
        let active = actor
            .active
            .as_mut()
            .filter(|active| active.session_id == request.session_id)
            .ok_or(MeetingCommandError::NotFound)?;
        let receipt = required_transition(
            &store,
            &request,
            MeetingCommandKind::Resume,
            &[MeetingPhase::CapturingPaused],
            MeetingPhase::CapturingResuming,
            "resume_requested",
        )?;
        if receipt.result != OperationResult::Committed {
            drop(actor);
            return self.result_for_receipt(store, receipt, request.session_id);
        }
        self.acquire_keep_awake();
        for source in active.sources.values_mut() {
            let next_epoch = source
                .epoch
                .get()
                .checked_add(1)
                .map(SourceEpoch::new)
                .ok_or(MeetingCommandError::InvalidRequest)?;
            match source.source.resume(next_epoch) {
                Ok(report)
                    if report.track_id == source.track_id
                        && report.epoch.get() >= next_epoch.get() =>
                {
                    source.epoch = report.epoch;
                }
                Ok(_) | Err(_) => {
                    let _ = source.source.abort();
                    store
                        .record_gap(&SourceGap {
                            track_id: source.track_id,
                            epoch: next_epoch,
                            start_offset_ns: None,
                            end_offset_ns: None,
                            reason: SourceGapReason::SourceStopped,
                            dropped_frames: None,
                        })
                        .map_err(map_store_error)?;
                    store
                        .update_track_health(
                            request.session_id,
                            source.track_id,
                            SourceHealth::Failed,
                        )
                        .map_err(map_store_error)?;
                }
            }
        }
        let resuming = store
            .session_snapshot(request.session_id)
            .map_err(map_store_error)?;
        store
            .open_capture_window(request.session_id, resuming.elapsed_offset_ns.unwrap_or(0))
            .map_err(map_store_error)?;
        store
            .transition(StoreTransition {
                operation_id: None,
                actor: OperationActor::System,
                command: MeetingCommandKind::Resume,
                requested_at_utc_ms: utc_now_ms(),
                session_id: request.session_id,
                expected_revision: resuming.revision,
                allowed_from: &[MeetingPhase::CapturingResuming],
                next_phase: MeetingPhase::CapturingRecording,
                event_kind: "resume_confirmed",
                reason_codes: Vec::new(),
            })
            .map_err(map_store_error)?;
        drop(actor);
        let snapshot = store
            .session_snapshot(request.session_id)
            .map_err(map_store_error)?;
        self.emit_session_changed(&snapshot);
        Ok(MeetingMutationResult { receipt, snapshot })
    }

    /// `cause` names the surface that asked. It decides the receipt's actor and
    /// is logged at the commit, so a stop is attributable from the corpus and
    /// from the log without the caller having to remember to say so.
    pub async fn stop(
        &self,
        request: MeetingMutationRequest,
        cause: MeetingStopCause,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        let store = self.store().await?;
        if let Some(receipt) = store
            .operation_receipt(request.operation_id)
            .map_err(map_store_error)?
        {
            return self.result_for_receipt(store, receipt, request.session_id);
        }
        let mut actor = self.actor_lock();
        let receipt = required_transition_by(
            &store,
            cause.actor(),
            &request,
            MeetingCommandKind::Stop,
            &[
                MeetingPhase::Starting,
                MeetingPhase::CapturingRecording,
                MeetingPhase::CapturingPausing,
                MeetingPhase::CapturingPaused,
                MeetingPhase::CapturingResuming,
            ],
            MeetingPhase::Stopping,
            "stop_requested",
        )?;
        if receipt.result != OperationResult::Committed {
            drop(actor);
            // A stop that loses the phase or revision gate answers with a
            // receipt the pressing surface renders as done, so this line is
            // the only trace that a press did nothing - and, since the surface
            // rides on the cause, which press it was.
            log::warn!(
                "Meeting capture {} was not stopped by {}: {:?} {:?}",
                request.session_id.uuid(),
                cause.describe(),
                receipt.result,
                receipt.reason_codes,
            );
            return self.result_for_receipt(store, receipt, request.session_id);
        }
        // After the fence, so this names a stop that happened rather than one
        // that was asked for.
        log::info!(
            "Meeting capture {} stopped by {}",
            request.session_id.uuid(),
            cause.describe()
        );
        let mut active = actor
            .active
            .take()
            .filter(|active| active.session_id == request.session_id)
            .ok_or(MeetingCommandError::NotFound)?;
        for source in active.sources.values_mut() {
            // The packet lane is the persistence path. The stop report repeats
            // what the source observed and must not insert every gap a second time.
            let _ = source.source.stop();
            source.worker.stop().map_err(map_store_error)?;
        }
        // Every worker has drained and sealed, so whether a lane produced audio
        // is now a fact. Do it before the snapshot below reads the revision the
        // stop transition expects.
        store
            .seal_source_health(request.session_id)
            .map_err(map_store_error)?;
        let stopping = store
            .session_snapshot(request.session_id)
            .map_err(map_store_error)?;
        store
            .close_open_capture_window(
                request.session_id,
                stopping.elapsed_offset_ns.unwrap_or(0),
                "stopped",
            )
            .map_err(map_store_error)?;
        store
            .transition(StoreTransition {
                operation_id: None,
                actor: OperationActor::System,
                command: MeetingCommandKind::Stop,
                requested_at_utc_ms: utc_now_ms(),
                session_id: request.session_id,
                expected_revision: stopping.revision,
                allowed_from: &[MeetingPhase::Stopping],
                next_phase: MeetingPhase::Processing,
                event_kind: "capture_sealed",
                reason_codes: Vec::new(),
            })
            .map_err(map_store_error)?;
        self.processing.set_capture_active(false);
        self.release_keep_awake();
        actor.tray_session_id = Some(request.session_id);
        drop(actor);
        // The lock goes first: dropping this record ends the provisional pass,
        // and joining a pass that is mid-recognition must not hold every other
        // meeting command behind it.
        drop(active);
        self.processing.submit(
            Arc::clone(&store),
            request.session_id,
            ProcessingOrigin::Stop,
        );
        let snapshot = store
            .session_snapshot(request.session_id)
            .map_err(map_store_error)?;
        self.emit_session_changed(&snapshot);
        Ok(MeetingMutationResult { receipt, snapshot })
    }

    /// Turn a recording on disk into a meeting.
    ///
    /// The file is decoded through the one Symphonia path in the app into the
    /// fixed 16 kHz mono stream a live microphone produces, and written to a
    /// microphone-kind source track through the same `MeetingTrackWriter` the
    /// capture worker writes through. The session then walks the phases and
    /// writes the events a stopped recording does — `preflight_created`,
    /// `start_authorized`, `capture_started`, `stop_requested`,
    /// `capture_sealed` — and hands off to `MeetingProcessingService::submit`
    /// exactly as `stop` does, so transcript, diarization, notes, ledger,
    /// people, analytics and export run unchanged and cannot tell the
    /// difference.
    ///
    /// A failure anywhere after the session exists discards it. Half an import
    /// is not a meeting, and leaving one behind would put a session whose audio
    /// was never written into the recovery pool for every later launch to offer
    /// and fail to finish.
    pub async fn import_recording(
        &self,
        request: ImportRecordingRequest,
    ) -> Result<MeetingSessionSnapshot, MeetingCommandError> {
        let media = validate_media_path(&request.path).map_err(|error| {
            log::warn!("Meeting import refused {}: {error}", request.path.display());
            MeetingCommandError::ImportUnreadable
        })?;
        let store = self.store().await?;
        let session_id = self
            .open_imported_session(
                &store,
                import_title(request.title.as_deref(), &media.canonical_path),
                request
                    .recorded_at_utc_ms
                    .or_else(|| file_modified_utc_ms(&media.canonical_path))
                    .unwrap_or_else(utc_now_ms),
                &request.origin,
            )
            .await?;
        match self
            .write_imported_audio(Arc::clone(&store), session_id, media, &request.origin)
            .await
        {
            Ok(()) => {}
            Err(error) => return Err(self.abandon_import(session_id, error).await),
        }
        match self.seal_imported_capture(&store, session_id, None).await {
            Ok(snapshot) => {
                self.processing
                    .submit(store, session_id, ProcessingOrigin::Stop);
                self.emit_session_changed(&snapshot);
                Ok(snapshot)
            }
            Err(error) => Err(self.abandon_import(session_id, error).await),
        }
    }

    /// Turn another note-taker's transcript export into a meeting.
    ///
    /// There is no audio to write, so the session carries an empty
    /// microphone-kind track and one transcript revision stamped `import`, with
    /// the vendor's speaker names on its segments. Processing then runs from
    /// `ProcessingOrigin::ImportedTranscript`, which skips the two passes that
    /// read audio and runs the rest: analytics, notes, ledger, and — through
    /// the same review transition every meeting takes — people and automations.
    pub async fn import_transcript(
        &self,
        path: PathBuf,
    ) -> Result<MeetingSessionSnapshot, MeetingCommandError> {
        let parsed = read_transcript_export(&path).map_err(|error| {
            log::warn!(
                "Meeting transcript import refused {}: {}",
                path.display(),
                error.0
            );
            MeetingCommandError::ImportUnreadable
        })?;
        let store = self.store().await?;
        let spans = resolve_spans(&parsed.segments);
        let duration_ns = spans
            .last()
            .map_or(0, |(_, end)| end.saturating_mul(1_000_000));
        let session_id = self
            .open_imported_session(
                &store,
                parsed.title.clone(),
                parsed
                    .started_at_utc_ms
                    .or_else(|| file_modified_utc_ms(&path))
                    .unwrap_or_else(utc_now_ms),
                &RecordingOrigin::LocalFile,
            )
            .await?;
        let track_id =
            match self.create_import_track(&store, session_id, &RecordingOrigin::LocalFile) {
                Ok(track_id) => track_id,
                Err(error) => return Err(self.abandon_import(session_id, error).await),
            };
        let snapshot = match self
            .seal_imported_capture(&store, session_id, Some(duration_ns))
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => return Err(self.abandon_import(session_id, error).await),
        };
        // The transcript is written in the Processing phase, where a live
        // meeting's transcript is written too, so a reader arriving mid-import
        // sees a meeting being processed rather than one with a partial
        // transcript already published.
        if let Err(error) =
            write_imported_transcript(&store, session_id, track_id, &parsed.segments, &spans)
        {
            return Err(self.abandon_import(session_id, error).await);
        }
        self.processing
            .submit(store, session_id, ProcessingOrigin::ImportedTranscript);
        self.emit_session_changed(&snapshot);
        Ok(snapshot)
    }

    /// Create the session, authorize its plan, and stamp it with the moment the
    /// audio was recorded. Returns a session in `Starting`.
    ///
    /// The preflight is built rather than probed: an import has no device to
    /// ask, and recording a live microphone's current availability against a
    /// file that was captured last Tuesday would be a fact about the wrong
    /// thing. Destination is always local, because the bytes are already on
    /// this Mac and no remote consent rail was crossed to get them here.
    async fn open_imported_session(
        &self,
        store: &Arc<MeetingStore>,
        title: String,
        recorded_at_utc_ms: i64,
        origin: &RecordingOrigin,
    ) -> Result<MeetingSessionId, MeetingCommandError> {
        let session_id = MeetingSessionId::new();
        let (retention_policy, _) = store.default_retention_policy().map_err(map_store_error)?;
        let preflight = MeetingPreflightSnapshot {
            session_id,
            revision: 0,
            proposed_title: title.clone(),
            origin: MeetingOrigin::Import,
            sources: vec![MeetingSourceSnapshot {
                track_id: None,
                source_kind: SourceKind::Microphone,
                required: true,
                availability: SourceAvailability::Available,
                health: SourceHealth::Healthy,
                format: Some(IMPORT_AUDIO_FORMAT),
                last_durable_offset_ns: None,
                gap_count: 0,
            }],
            storage: StorageAvailability::Available,
            local_processing: self.processing.local_processing_availability(),
            destination: ProcessingDestination::Local,
            microphone_device_uid: None,
            frozen_system_audio_application_bundle_ids: Vec::new(),
            accepted_known_missing_sources: Vec::new(),
            degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
            required_acknowledgements: vec![SourceKind::Microphone],
            allowed_actions: vec![AllowedMeetingAction::Start],
        };
        store
            .create_preflight(
                StoreMutation {
                    operation_id: MeetingOperationId::new(),
                    requested_at_utc_ms: utc_now_ms(),
                    session_id,
                    expected_revision: 0,
                    command: MeetingCommandKind::PreflightCreate,
                },
                title,
                MeetingOrigin::Import,
                preflight.clone(),
                retention_policy,
            )
            .map_err(map_store_error)?;
        let consent = MeetingConsentInput {
            policy_version: MEETING_CONSENT_POLICY_VERSION,
            // No stream was opened and no room was listened to. For a file the
            // operator picked, choosing it is the acknowledgement; for a
            // recording pulled off a paired device, the acknowledgement was
            // given on that device, and the consent row says so.
            microphone_acknowledged: true,
            system_audio_acknowledged: false,
            known_missing_sources_acknowledged: Vec::new(),
            degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
            destination: ProcessingDestination::Local,
            remote_acknowledgement: None,
        };
        let provenance = match origin {
            RecordingOrigin::LocalFile => MeetingConsentProvenance::Direct,
            RecordingOrigin::PairedDevice { device_id } => MeetingConsentProvenance::PairedDevice {
                device_id: device_id.clone(),
            },
        };
        let attempt_number = store
            .next_plan_attempt(session_id)
            .map_err(map_store_error)?;
        let mut plan =
            self.build_plan(session_id, 0, attempt_number, &preflight, &consent, store)?;
        // The imported audio's own zero, so a stamp on a record maps back to
        // when the recording was made rather than when it was read off disk.
        plan.session_clock_anchor.wall_start_utc_ms = recorded_at_utc_ms;
        let receipt = store
            .start_with_plan_and_consent(
                MeetingOperationId::new(),
                recorded_at_utc_ms,
                &plan,
                &MeetingConsent {
                    consent_id: plan.consent_id,
                    session_id,
                    attempt_number,
                    preflight_revision: 0,
                    policy_version: consent.policy_version,
                    acknowledged_at_utc_ms: utc_now_ms(),
                    provenance,
                    microphone_acknowledged: true,
                    system_audio_acknowledged: false,
                    known_missing_sources_acknowledged: Vec::new(),
                    degraded_start_policy: consent.degraded_start_policy,
                    destination: consent.destination.clone(),
                    remote_acknowledgement: None,
                },
                0,
            )
            .map_err(map_store_error)?;
        if receipt.result != OperationResult::Committed {
            return Err(MeetingCommandError::InvalidTransition);
        }
        store
            .set_imported_start(session_id, recorded_at_utc_ms)
            .map_err(map_store_error)?;
        Ok(session_id)
    }

    /// The one microphone-kind track an import writes to. `origin` is kept on
    /// the track descriptor, the slot a live source uses for its own provenance.
    fn create_import_track(
        &self,
        store: &Arc<MeetingStore>,
        session_id: MeetingSessionId,
        origin: &RecordingOrigin,
    ) -> Result<SourceTrackId, MeetingCommandError> {
        let plan = store.processing_plan(session_id).map_err(map_store_error)?;
        let descriptor_json =
            serde_json::to_string(origin).map_err(|_| MeetingCommandError::InvalidRequest)?;
        let track_id = SourceTrackId::new();
        store
            .create_track(TrackCreation {
                session_id,
                plan_id: plan.plan_id,
                source_kind: SourceKind::Microphone,
                required: true,
                requested: true,
                descriptor_json: &descriptor_json,
                report: import_start_report(track_id),
            })
            .map_err(map_store_error)?;
        Ok(track_id)
    }

    /// Decode the file into the session's track, one resampled frame at a time.
    ///
    /// The decode runs on a blocking thread because it is CPU-bound for as long
    /// as the recording is.
    async fn write_imported_audio(
        &self,
        store: Arc<MeetingStore>,
        session_id: MeetingSessionId,
        media: ValidatedMediaPath,
        origin: &RecordingOrigin,
    ) -> Result<(), MeetingCommandError> {
        let track_id = self.create_import_track(&store, session_id, origin)?;
        let plan = store.processing_plan(session_id).map_err(map_store_error)?;
        let writer = store
            .open_track_writer(session_id, track_id, plan.storage.clone())
            .map_err(map_store_error)?;
        tauri::async_runtime::spawn_blocking(move || decode_into_track(writer, track_id, &media))
            .await
            .map_err(|_| MeetingCommandError::StorageUnavailable)?
    }

    /// Walk the phases a stopped recording walks and land in `Processing`.
    /// `duration_ns` overrides the capture window's end for an import with no
    /// audio behind it, where there are no durable records to measure.
    async fn seal_imported_capture(
        &self,
        store: &Arc<MeetingStore>,
        session_id: MeetingSessionId,
        duration_ns: Option<u64>,
    ) -> Result<MeetingSessionSnapshot, MeetingCommandError> {
        store
            .open_capture_window(session_id, 0)
            .map_err(map_store_error)?;
        for (allowed_from, next_phase, event_kind) in [
            (
                MeetingPhase::Starting,
                MeetingPhase::CapturingRecording,
                "capture_started",
            ),
            (
                MeetingPhase::CapturingRecording,
                MeetingPhase::Stopping,
                "stop_requested",
            ),
        ] {
            let snapshot = store
                .session_snapshot(session_id)
                .map_err(map_store_error)?;
            store
                .transition(StoreTransition {
                    operation_id: None,
                    actor: OperationActor::System,
                    command: MeetingCommandKind::Start,
                    requested_at_utc_ms: utc_now_ms(),
                    session_id,
                    expected_revision: snapshot.revision,
                    allowed_from: &[allowed_from],
                    next_phase,
                    event_kind,
                    reason_codes: Vec::new(),
                })
                .map_err(map_store_error)?;
        }
        let stopping = store
            .session_snapshot(session_id)
            .map_err(map_store_error)?;
        store
            .close_open_capture_window(
                session_id,
                duration_ns.unwrap_or_else(|| stopping.elapsed_offset_ns.unwrap_or(0)),
                "stopped",
            )
            .map_err(map_store_error)?;
        store
            .transition(StoreTransition {
                operation_id: None,
                actor: OperationActor::System,
                command: MeetingCommandKind::Stop,
                requested_at_utc_ms: utc_now_ms(),
                session_id,
                expected_revision: stopping.revision,
                allowed_from: &[MeetingPhase::Stopping],
                next_phase: MeetingPhase::Processing,
                event_kind: "capture_sealed",
                reason_codes: Vec::new(),
            })
            .map_err(map_store_error)?;
        store.session_snapshot(session_id).map_err(map_store_error)
    }

    /// Throw away a session whose import did not finish, and return the error
    /// that stopped it. A cleanup that itself fails is logged and not allowed to
    /// replace the operator's answer.
    async fn abandon_import(
        &self,
        session_id: MeetingSessionId,
        error: MeetingCommandError,
    ) -> MeetingCommandError {
        let Ok(store) = self.store().await else {
            return error;
        };
        let Ok(snapshot) = store.session_snapshot(session_id) else {
            return error;
        };
        if let Err(cleanup) = self
            .delete_with_cause(
                MeetingMutationRequest {
                    operation_id: MeetingOperationId::new(),
                    session_id,
                    expected_revision: snapshot.revision,
                },
                DeletionCause::Discard,
            )
            .await
        {
            log::warn!("A failed meeting import could not be discarded: {cleanup:?}");
        }
        error
    }

    pub async fn discard(
        &self,
        request: MeetingMutationRequest,
    ) -> Result<MeetingRemovalResult, MeetingCommandError> {
        self.delete_with_cause(request, DeletionCause::Discard)
            .await
    }

    pub async fn delete(
        &self,
        request: MeetingMutationRequest,
    ) -> Result<MeetingRemovalResult, MeetingCommandError> {
        self.delete_with_cause(request, DeletionCause::User).await
    }

    async fn delete_with_cause(
        &self,
        request: MeetingMutationRequest,
        cause: DeletionCause,
    ) -> Result<MeetingRemovalResult, MeetingCommandError> {
        let store = self.store().await?;
        let (receipt, job_id) = store
            .reserve_deletion(
                request.operation_id,
                utc_now_ms(),
                request.session_id,
                request.expected_revision,
                cause,
            )
            .map_err(map_store_error)?;
        if receipt.result == OperationResult::Rejected {
            return Ok(MeetingRemovalResult {
                receipt,
                session_id: request.session_id,
                removed: false,
            });
        }
        if !self.is_capture_active() {
            store
                .enqueue_cloud_tombstone_for_session(
                    request.session_id,
                    receipt.new_revision.unwrap_or(request.expected_revision),
                    format!("tombstone-{}", job_id.uuid()),
                    utc_now_ms(),
                )
                .map_err(map_store_error)?;
        }

        self.processing.cancel(request.session_id);
        self.processing.set_capture_active(false);
        self.release_keep_awake();

        let active = {
            let mut actor = self.actor_lock();
            if actor.tray_session_id == Some(request.session_id) {
                actor.tray_session_id = None;
            }
            if actor
                .active
                .as_ref()
                .is_some_and(|active| active.session_id == request.session_id)
            {
                actor.active.take()
            } else {
                None
            }
        };

        if let Some(mut active) = active {
            for source in active.sources.values_mut() {
                let _ = source.source.abort();
                source.worker.stop().map_err(map_store_error)?;
            }
        }
        let people_revision = store.finish_deletion(job_id).map_err(map_store_error)?;
        if let Some(people_revision) = people_revision {
            self.emit_artifact_changed(Some(request.session_id), people_revision);
        }
        self.emit_removed(
            request.session_id,
            receipt.new_revision.unwrap_or(request.expected_revision),
        );
        Ok(MeetingRemovalResult {
            receipt,
            session_id: request.session_id,
            removed: true,
        })
    }

    pub async fn recovery_list(&self) -> Result<Vec<MeetingHistorySummary>, MeetingCommandError> {
        let store = self.store().await?;
        Ok(store
            .list_sessions(None, 100, &MeetingListFilter::default())
            .map_err(map_store_error)?
            .entries
            .into_iter()
            .filter(|summary| summary.phase == MeetingPhase::RecoveryRequired)
            .collect())
    }

    pub async fn recovery_finalize(
        &self,
        request: MeetingMutationRequest,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        self.recovery_finalize_as(OperationActor::User, request)
            .await
    }

    /// The one owner of reprocessing an interrupted meeting, whether the
    /// person asked for it or the launch's automatic pass did. The phase fence
    /// is the whole duplicate-press guard: a second call while the first is
    /// still running is refused because the meeting is no longer in recovery.
    async fn recovery_finalize_as(
        &self,
        actor: OperationActor,
        request: MeetingMutationRequest,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        let store = self.store().await?;
        let receipt = required_transition_by(
            &store,
            actor,
            &request,
            MeetingCommandKind::RecoveryFinalize,
            &[MeetingPhase::RecoveryRequired],
            MeetingPhase::Processing,
            "recovery_finalized_partial",
        )?;
        if receipt.result == OperationResult::Committed {
            self.processing.set_capture_active(false);
            self.processing.submit(
                Arc::clone(&store),
                request.session_id,
                ProcessingOrigin::Recovery,
            );
        }
        let result = self.result_for_receipt(store, receipt, request.session_id)?;
        self.emit_session_changed(&result.snapshot);
        Ok(result)
    }

    pub async fn list(
        &self,
        cursor_utc_ms: Option<i64>,
        limit: usize,
        filter: MeetingListFilter,
    ) -> Result<PaginatedMeetings, MeetingCommandError> {
        self.store()
            .await?
            .list_sessions(cursor_utc_ms, limit, &filter)
            .map_err(map_store_error)
    }

    /// Dashboard trend reads are optional when encrypted meeting storage is
    /// unavailable. Returning the tagged projection keeps that state distinct
    /// from a real empty range.
    pub async fn trend_projection(&self, request: DashboardTrendRequest) -> MeetingTrendProjection {
        let range = request.range;
        let Ok(store) = self.store().await else {
            return MeetingTrendProjection::unavailable(range);
        };
        store
            .trend_projection(request)
            .unwrap_or_else(|_| MeetingTrendProjection::unavailable(range))
    }

    pub async fn get(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<MeetingReviewSnapshot, MeetingCommandError> {
        self.store()
            .await?
            .review_snapshot(session_id)
            .map_err(map_store_error)
    }

    pub async fn search(
        &self,
        request: MeetingSearchRequest,
    ) -> Result<MeetingSearchResult, MeetingCommandError> {
        if request.session_ids.is_empty() {
            return Err(MeetingCommandError::InvalidRequest);
        }
        let evidence = self
            .store()
            .await?
            .search_evidence(
                &request.session_ids,
                &request.query,
                request.limit.unwrap_or(20).clamp(1, 50),
            )
            .map_err(map_store_error)?;
        Ok(MeetingSearchResult {
            entries: evidence
                .into_iter()
                .map(|evidence| MeetingSearchHit {
                    session_id: evidence.citation.session_id,
                    kind: evidence.citation.kind,
                    entity_id: evidence.citation.entity_id,
                    start_offset_ns: evidence.citation.start_offset_ns,
                    end_offset_ns: evidence.citation.end_offset_ns,
                    excerpt: evidence.text,
                })
                .collect(),
        })
    }

    pub async fn title_set(
        &self,
        request: MeetingTitleSetRequest,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        let store = self.store().await?;
        let receipt = store
            .set_title(
                request.operation_id,
                utc_now_ms(),
                request.session_id,
                request.expected_revision,
                request.title,
            )
            .map_err(map_store_error)?;
        let result = self.result_for_receipt(store, receipt, request.session_id)?;
        self.emit_session_changed(&result.snapshot);
        Ok(result)
    }

    /// Every actionable ledger row in a meeting: the loops and commitments the
    /// review screen ticks off, with whatever has already been done to them.
    pub async fn loops_list(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<MeetingLoopsResult, MeetingCommandError> {
        self.store()
            .await?
            .meeting_loops(session_id)
            .map_err(map_store_error)
    }

    /// D26. Turn this meeting's record into a message the user can send.
    ///
    /// Nothing about the meeting changes, so this takes no expected revision:
    /// it reads the current artifact revision and the loops beside it, asks
    /// whichever engine the meeting resolves to for a message, and records the
    /// event. The operation id is the caller's, so a double press produces one
    /// receipt and one draft rather than two.
    ///
    /// No engine is not a failure. The evidence goes back either way and the
    /// sheet renders it as the draft, which is why this returns `Ok` with
    /// [`MeetingFollowUpSource::Structured`] rather than an error the button
    /// would have to hide behind. A meeting with no record at all is the one
    /// real absence, and that is `NotFound`.
    pub async fn follow_up_draft(
        &self,
        operation_id: MeetingOperationId,
        session_id: MeetingSessionId,
    ) -> Result<MeetingFollowUpDraft, MeetingCommandError> {
        let store = self.store().await?;
        let review = store.review_snapshot(session_id).map_err(map_store_error)?;
        let content = review
            .artifacts
            .iter()
            .filter(|artifact| artifact.state == MeetingArtifactState::Current)
            .find_map(|artifact| artifact.content.as_ref());
        let loops = store.meeting_loops(session_id).map_err(map_store_error)?;
        let evidence = FollowUpEvidence::gather(review.session.title.clone(), content, &loops.rows);
        if evidence.is_empty() {
            return Err(MeetingCommandError::NotFound);
        }

        // One engine per draft: the meeting's own choice, asked once. A second
        // attempt on the other engine after a failure would route text the
        // operator kept local onto a server, or the reverse.
        let generator = self
            .processing
            .text_generator_for_session(&store, session_id);
        let generated = generator.as_ref().and_then(|generator| {
            generator
                .generate(
                    &follow_up_prompt(),
                    &evidence.as_prompt_input(),
                    FOLLOW_UP_MAX_TOKENS,
                    ReplyShape::Prose,
                )
                .ok()
                .map(|message| message.trim().to_string())
                .filter(|message| !message.is_empty())
        });
        let engine = match (&generated, &generator) {
            (Some(_), Some(generator)) => generator.model_id(),
            _ => "structured-fallback",
        };
        let receipt = store
            .record_follow_up_draft(operation_id, session_id, engine)
            .map_err(map_store_error)?;

        Ok(MeetingFollowUpDraft {
            session_id,
            title: evidence.title,
            source: if generated.is_some() {
                MeetingFollowUpSource::Generated
            } else {
                MeetingFollowUpSource::Structured
            },
            message: generated,
            summary: evidence.summary,
            mine: evidence.mine,
            decisions: evidence.decisions,
            receipt,
        })
    }

    /// D26. The same draft, addressed and ready to send.
    ///
    /// The recipients are the meeting's calendar match, minus the operator's own
    /// entry: EventKit names participants and their addresses, the session
    /// remembered both when it started, and nothing else in this app knows who
    /// was in the room. The subject is the meeting's current title, which is the
    /// line every other surface calls this meeting.
    ///
    /// Nothing is opened here. The URL goes back to the caller, which opens it
    /// through the same opener plugin every other link in the app uses and
    /// writes the clipboard the same way its Copy button does — so this stays a
    /// pure read of the store, and the one thing it owns is the URL.
    pub async fn follow_up_mail(
        &self,
        request: MeetingFollowUpMailRequest,
    ) -> Result<MeetingFollowUpMail, MeetingCommandError> {
        let store = self.store().await?;
        let snapshot = store
            .session_snapshot(request.session_id)
            .map_err(map_store_error)?;
        let recipients = store
            .meeting_calendar_facts(request.session_id)
            .map_err(map_store_error)?
            .map(|event| recipient_addresses(&event.attendees))
            .unwrap_or_default();
        let fits = body_fits(&request.body);
        Ok(MeetingFollowUpMail {
            url: mailto_url(
                &recipients,
                &snapshot.title,
                if fits {
                    &request.body
                } else {
                    &request.over_bound_note
                },
            ),
            body: if fits {
                MeetingFollowUpMailBody::Draft
            } else {
                MeetingFollowUpMailBody::Clipboard
            },
        })
    }

    pub async fn loop_resolve(
        &self,
        request: MeetingLoopResolveRequest,
    ) -> Result<MeetingLoopMutationResult, MeetingCommandError> {
        let session_id = request.loop_id.session_id();
        let result = self
            .store()
            .await?
            .resolve_loop(request, utc_now_ms())
            .map_err(map_store_error)?;
        self.emit_artifact_changed(session_id, result.loops.revision);
        Ok(result)
    }

    pub async fn loop_reopen(
        &self,
        request: MeetingLoopReopenRequest,
    ) -> Result<MeetingLoopMutationResult, MeetingCommandError> {
        let session_id = request.loop_id.session_id();
        let result = self
            .store()
            .await?
            .reopen_loop(request, utc_now_ms())
            .map_err(map_store_error)?;
        self.emit_artifact_changed(session_id, result.loops.revision);
        Ok(result)
    }

    pub async fn loop_assign(
        &self,
        request: MeetingLoopAssignRequest,
    ) -> Result<MeetingLoopMutationResult, MeetingCommandError> {
        let session_id = request.loop_id.session_id();
        let result = self
            .store()
            .await?
            .assign_loop(request, utc_now_ms())
            .map_err(map_store_error)?;
        self.emit_artifact_changed(session_id, result.loops.revision);
        Ok(result)
    }

    /// Rewrites one person's relationship paragraph now, and hands back their
    /// page.
    ///
    /// The engine follows the person's most recent confirmed meeting, because
    /// D14 routes a meeting's text by that meeting's series and this paragraph
    /// is written out of those meetings' evidence. A person with no meeting has
    /// no engine to pick and nothing to summarize; a Mac with no engine at all
    /// writes nothing. Both return the page unchanged rather than an error: the
    /// button did what it could, and the paragraph already there is still true.
    pub async fn person_summary_regenerate(
        &self,
        person_id: PersonId,
    ) -> Result<PersonDetailResult, MeetingCommandError> {
        let store = self.store().await?;
        let detail = store.person_detail(person_id).map_err(map_store_error)?;
        let session_id = detail
            .detail
            .links
            .iter()
            .find(|link| link.confidence == PersonLinkConfidence::Confirmed)
            .map(|link| link.meeting.id);
        if let Some(session_id) = session_id {
            if let Some(generator) = self
                .processing
                .text_generator_for_session(&store, session_id)
            {
                write_relationship_summary(&store, person_id, generator.as_ref())
                    .map_err(map_store_error)?;
                return store.person_detail(person_id).map_err(map_store_error);
            }
        }
        Ok(detail)
    }

    pub async fn speaker_rename(
        &self,
        request: MeetingSpeakerRenameRequest,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        let store = self.store().await?;
        let display_name = request.display_name.clone();
        let operation_id = request.operation_id;
        let session_id = request.session_id;
        let receipt = store
            .rename_speaker(
                request.operation_id,
                utc_now_ms(),
                request.session_id,
                request.expected_revision,
                request.speaker_id,
                request.display_name,
            )
            .map_err(map_store_error)?;
        let result = self.result_for_receipt(Arc::clone(&store), receipt, request.session_id)?;
        self.emit_session_changed(&result.snapshot);
        if result.receipt.result == OperationResult::Committed {
            self.record_speaker_renamed(Arc::clone(&store), session_id, operation_id, display_name);
        }
        Ok(result)
    }

    pub async fn speaker_merge(
        &self,
        request: MeetingSpeakerMergeRequest,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        let store = self.store().await?;
        let receipt = store
            .merge_speaker(
                request.operation_id,
                utc_now_ms(),
                request.session_id,
                request.expected_revision,
                request.source_speaker_id,
                request.target_speaker_id,
            )
            .map_err(map_store_error)?;
        let result = self.result_for_receipt(store, receipt, request.session_id)?;
        self.emit_session_changed(&result.snapshot);
        Ok(result)
    }

    pub async fn segment_edit(
        &self,
        request: MeetingSegmentEditRequest,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        let store = self.store().await?;
        let receipt = store
            .edit_segment(SegmentEdit {
                mutation: StoreMutation {
                    operation_id: request.operation_id,
                    requested_at_utc_ms: utc_now_ms(),
                    session_id: request.session_id,
                    expected_revision: request.expected_revision,
                    command: MeetingCommandKind::SegmentEdit,
                },
                segment_id: request.segment_id,
                replacement_text: request.replacement_text,
                removed: request.removed,
            })
            .map_err(map_store_error)?;
        let result = self.result_for_receipt(store, receipt, request.session_id)?;
        self.emit_session_changed(&result.snapshot);
        Ok(result)
    }

    pub async fn note_create(
        &self,
        request: MeetingNoteCreateRequest,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        let store = self.store().await?;
        let now = utc_now_ms();
        let note = ManualNote {
            note_id: ManualNoteId::new(),
            session_id: request.session_id,
            start_offset_ns: request.start_offset_ns,
            end_offset_ns: request.end_offset_ns,
            body: request.body,
            revision: 0,
            created_at_utc_ms: now,
            updated_at_utc_ms: now,
        };
        let receipt = store
            .create_note(request.operation_id, now, &note, request.expected_revision)
            .map_err(map_store_error)?;
        let result = self.result_for_receipt(store, receipt, request.session_id)?;
        self.emit_session_changed(&result.snapshot);
        Ok(result)
    }

    pub async fn note_update(
        &self,
        request: MeetingNoteUpdateRequest,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        let store = self.store().await?;
        let note = ManualNote {
            note_id: request.note_id,
            session_id: request.session_id,
            start_offset_ns: request.start_offset_ns,
            end_offset_ns: request.end_offset_ns,
            body: request.body,
            revision: request
                .expected_note_revision
                .checked_add(1)
                .ok_or(MeetingCommandError::InvalidRequest)?,
            created_at_utc_ms: 0,
            updated_at_utc_ms: utc_now_ms(),
        };
        let receipt = store
            .update_note(
                request.operation_id,
                utc_now_ms(),
                &note,
                request.expected_revision,
                request.expected_note_revision,
            )
            .map_err(map_store_error)?;
        let result = self.result_for_receipt(store, receipt, request.session_id)?;
        self.emit_session_changed(&result.snapshot);
        Ok(result)
    }

    pub async fn note_delete(
        &self,
        request: MeetingNoteDeleteRequest,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        let store = self.store().await?;
        let receipt = store
            .delete_note(
                request.operation_id,
                utc_now_ms(),
                request.session_id,
                request.expected_revision,
                request.note_id,
                request.expected_note_revision,
            )
            .map_err(map_store_error)?;
        let result = self.result_for_receipt(store, receipt, request.session_id)?;
        self.emit_session_changed(&result.snapshot);
        Ok(result)
    }

    pub async fn artifacts_regenerate(
        &self,
        request: MeetingMutationRequest,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        let store = self.store().await?;
        if let Some(receipt) = store
            .operation_receipt(request.operation_id)
            .map_err(map_store_error)?
        {
            return self.result_for_receipt(store, receipt, request.session_id);
        }
        /* Off the runtime. A relayed generation blocks its calling thread for
         * up to `RELAY_JOIN_TIMEOUT`, and this is an async command, so waiting
         * here would hold a runtime worker for minutes on a machine that is
         * also transcribing. */
        let (artifact, engine) = {
            let processing = Arc::clone(&self.processing);
            let store = Arc::clone(&store);
            let session_id = request.session_id;
            let expected_revision = request.expected_revision;
            tauri::async_runtime::spawn_blocking(move || {
                processing.regenerate(&store, session_id, expected_revision)
            })
            .await
            .map_err(|_| map_processing_error(ProcessingFailure::EngineFailure))?
            .map_err(map_processing_error)?
        };
        let receipt = store
            .record_artifact_regeneration(
                request.operation_id,
                utc_now_ms(),
                request.session_id,
                request.expected_revision,
                artifact.artifact_id,
                engine,
            )
            .map_err(map_store_error)?;
        if receipt.result == OperationResult::Rejected {
            return Err(MeetingCommandError::StaleRevision);
        }
        let result = self.result_for_receipt(store, receipt, request.session_id)?;
        self.emit_session_changed(&result.snapshot);
        Ok(result)
    }
    pub async fn export(
        &self,
        request: MeetingExportRequest,
    ) -> Result<MeetingExportResult, MeetingCommandError> {
        let store = self.store().await?;
        if let Some(result) = store
            .export_result_for_operation(request.operation_id)
            .map_err(map_store_error)?
        {
            return Ok(result);
        }
        if store
            .operation_receipt(request.operation_id)
            .map_err(map_store_error)?
            .is_some()
        {
            return Err(MeetingCommandError::InvalidRequest);
        }

        let review = store
            .review_snapshot(request.session_id)
            .map_err(map_store_error)?;
        if review.session.revision != request.expected_revision {
            return Err(MeetingCommandError::StaleRevision);
        }
        if !review.can_export {
            return Err(MeetingCommandError::InvalidTransition);
        }
        let contents = export::render(request.format, &review)
            .map_err(|_| MeetingCommandError::ExportFailed)?;
        let app = self.app.clone().ok_or(MeetingCommandError::ExportFailed)?;
        let (filter_name, extension, file_name) = match request.format {
            MeetingExportFormat::Json => ("JSON", "json", "meeting.json"),
            MeetingExportFormat::Markdown => ("Markdown", "md", "meeting.md"),
        };
        let selected = tauri::async_runtime::spawn_blocking(move || {
            app.dialog()
                .file()
                .add_filter(filter_name, &[extension])
                .set_file_name(file_name)
                .blocking_save_file()
                .map(|path| path.into_path().map_err(|_| ()))
        })
        .await
        .map_err(|_| MeetingCommandError::ExportFailed)?;
        let path = selected
            .ok_or(MeetingCommandError::ExportCancelled)?
            .map_err(|_| MeetingCommandError::ExportFailed)?;
        tauri::async_runtime::spawn_blocking(move || export::write_atomic(&path, &contents))
            .await
            .map_err(|_| MeetingCommandError::ExportFailed)?
            .map_err(|_| MeetingCommandError::ExportFailed)?;
        store
            .record_export(
                request.operation_id,
                utc_now_ms(),
                request.session_id,
                request.expected_revision,
                request.format,
            )
            .map_err(map_store_error)
    }

    /// Write this meeting's ledger as one self-contained HTML page.
    ///
    /// The page is a view, not a record. Its inferred half — threads, states,
    /// receipts — comes from the current artifact revision; its measured half
    /// — turns, seconds, word counts, talk share — comes from analytics, which
    /// owns those numbers, and the two are joined only here. Nothing is
    /// mutated, so this takes no operation id and no expected revision: run it
    /// again after a transcript edit and the counts on the page move while the
    /// model's reading of the conversation stays where it was.
    pub async fn produce_ledger_html(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<String, MeetingCommandError> {
        let store = self.store().await?;
        let review = store.review_snapshot(session_id).map_err(map_store_error)?;
        if !review.can_export {
            return Err(MeetingCommandError::InvalidTransition);
        }
        let (template_id, ledger) = review
            .artifacts
            .iter()
            .filter(|artifact| artifact.state == MeetingArtifactState::Current)
            .find_map(|artifact| {
                let ledger = artifact.content.as_ref()?.ledger.as_ref()?;
                Some((artifact.template_id.clone(), ledger.clone()))
            })
            .ok_or(MeetingCommandError::NotFound)?;

        let analytics = self
            .processing
            .refresh_analytics(&store, session_id, review.session.revision)
            .map_err(map_processing_error)?;
        let segments = store
            .analytics_segments(session_id)
            .map_err(map_store_error)?;
        let turns = merge_turns(&segments);
        let segment_speakers: HashMap<TranscriptSegmentId, SpeakerId> = segments
            .iter()
            .map(|segment| (segment.segment_id, segment.speaker_id))
            .collect();
        let speaker_names: HashMap<SpeakerId, String> = review
            .speakers
            .iter()
            .map(|speaker| (speaker.speaker_id, speaker.display_name.clone()))
            .collect();
        // The page's time axis has to contain every turn drawn on it, so the
        // end is whichever is later: the elapsed capture window, or the last
        // word transcribed.
        let duration_ns = review.session.elapsed_offset_ns.unwrap_or(0).max(
            segments
                .iter()
                .map(|segment| segment.end_offset_ns)
                .max()
                .unwrap_or(0),
        );
        let template =
            MeetingNotesTemplate::from_artifact_template_id(&template_id).unwrap_or_default();
        let page = ledger::build_page(ledger::LedgerPageInput {
            title: &review.session.title,
            kind: template.label(),
            // Upstream reads a date only out of the transcript's own content,
            // never a filename or an mtime. Sona recorded this meeting, so its
            // own capture clock is that content.
            date: review.session.started_at_utc_ms.and_then(local_iso_date),
            duration_ns,
            ledger: &ledger,
            talk: &analytics.talk,
            turns: &turns,
            speaker_names: &speaker_names,
            segment_speakers: &segment_speakers,
        });
        let contents = ledger::render_html(&page)
            .map_err(|_| MeetingCommandError::ExportFailed)?
            .into_bytes();

        let app = self.app.clone().ok_or(MeetingCommandError::ExportFailed)?;
        let selected = tauri::async_runtime::spawn_blocking(move || {
            app.dialog()
                .file()
                .add_filter("HTML", &["html"])
                .set_file_name("meeting-ledger.html")
                .blocking_save_file()
                .map(|path| path.into_path().map_err(|_| ()))
        })
        .await
        .map_err(|_| MeetingCommandError::ExportFailed)?;
        let path = selected
            .ok_or(MeetingCommandError::ExportCancelled)?
            .map_err(|_| MeetingCommandError::ExportFailed)?;
        let written = path.display().to_string();
        tauri::async_runtime::spawn_blocking(move || export::write_atomic(&path, &contents))
            .await
            .map_err(|_| MeetingCommandError::ExportFailed)?
            .map_err(|_| MeetingCommandError::ExportFailed)?;
        Ok(written)
    }

    pub async fn question_forget(
        &self,
        request: MeetingMutationRequest,
        question_id: MeetingQuestionId,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        let store = self.store().await?;
        let receipt = store
            .forget_question(
                request.operation_id,
                utc_now_ms(),
                request.session_id,
                request.expected_revision,
                question_id,
            )
            .map_err(map_store_error)?;
        let result = self.result_for_receipt(store, receipt, request.session_id)?;
        self.emit_session_changed(&result.snapshot);
        Ok(result)
    }

    /// Conversation metrics, tracker hits, action-item ticks and the user's
    /// own notes for one meeting, derived fresh from the current transcript.
    /// The stored copy is refreshed as a side effect so a later read of a
    /// deleted-transcript meeting still has something to show.
    pub async fn analytics_get(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<MeetingAnalyticsSnapshot, MeetingCommandError> {
        let store = self.store().await?;
        let snapshot = store
            .session_snapshot(session_id)
            .map_err(map_store_error)?;
        let analytics = self
            .processing
            .refresh_analytics(&store, session_id, snapshot.revision)
            .map_err(map_processing_error)?;
        Ok(MeetingAnalyticsSnapshot {
            session_id,
            input_revision: snapshot.revision,
            computed_at_utc_ms: store
                .conversation_metrics_computed_at(session_id)
                .map_err(map_store_error)?
                .unwrap_or_default(),
            analytics,
            action_items: store
                .action_item_states(session_id)
                .map_err(map_store_error)?,
            notes: store
                .user_notes(session_id, self.default_notes_template())
                .map_err(map_store_error)?,
        })
    }

    pub async fn user_notes_get(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<MeetingUserNotes, MeetingCommandError> {
        self.store()
            .await?
            .user_notes(session_id, self.default_notes_template())
            .map_err(map_store_error)
    }

    /// Save the user's notes and chosen template. This is autosaved while a
    /// person types, so it carries its own note revision instead of the
    /// session revision every audited mutation uses.
    pub async fn user_notes_save(
        &self,
        request: MeetingUserNotesSaveRequest,
    ) -> Result<MeetingUserNotes, MeetingCommandError> {
        self.store()
            .await?
            .save_user_notes(
                request.session_id,
                &request.body,
                request.template,
                request.expected_note_revision,
            )
            .map_err(map_store_error)
    }

    pub async fn action_item_done_set(
        &self,
        request: MeetingActionItemDoneRequest,
    ) -> Result<Vec<MeetingActionItemState>, MeetingCommandError> {
        let store = self.store().await?;
        store
            .set_action_item_done(
                request.session_id,
                request.artifact_id,
                request.action_index,
                request.done,
            )
            .map_err(map_store_error)?;
        store
            .action_item_states(request.session_id)
            .map_err(map_store_error)
    }

    /// Regenerate the notes for a meeting with the user's notes and template
    /// included. This is `artifacts_regenerate` with the notes layer already
    /// saved, so it shares that path rather than duplicating it.
    pub async fn artifacts_reenhance(
        &self,
        request: MeetingReenhanceRequest,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        let store = self.store().await?;
        store
            .save_user_notes(
                request.session_id,
                &request.body,
                request.template,
                request.expected_note_revision,
            )
            .map_err(map_store_error)?;
        drop(store);
        self.artifacts_regenerate(MeetingMutationRequest {
            operation_id: request.operation_id,
            session_id: request.session_id,
            expected_revision: request.expected_revision,
        })
        .await
    }

    pub async fn catch_up(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<MeetingCatchUp, MeetingCommandError> {
        let store = self.store().await?;
        let live = self.live_transcript(session_id);
        self.processing
            .catch_up(&store, session_id, live.as_deref())
            .map_err(map_processing_error)
    }

    /// The provisional transcript of the capture that is running now, when the
    /// running capture is this meeting's.
    ///
    /// Cloned out from under the actor lock, because reading it means
    /// recognizing audio and generating text and no other meeting command may
    /// be made to wait for either. A meeting that has stopped has no live
    /// transcript here, which is what sends its readers to the stored
    /// revision.
    fn live_transcript(&self, session_id: MeetingSessionId) -> Option<Arc<LiveTranscript>> {
        self.actor_lock()
            .active
            .as_ref()
            .filter(|active| active.session_id == session_id)
            .map(|active| active.live.transcript())
    }

    fn default_notes_template(&self) -> MeetingNotesTemplate {
        self.app
            .as_ref()
            .map(|app| crate::settings::get_settings(app).meeting_notes_template)
            .unwrap_or_default()
    }

    pub async fn retention_get(&self) -> Result<MeetingRetentionSnapshot, MeetingCommandError> {
        let (policy, revision) = self
            .store()
            .await?
            .default_retention_policy()
            .map_err(map_store_error)?;
        Ok(MeetingRetentionSnapshot { policy, revision })
    }

    pub async fn retention_set(
        &self,
        request: MeetingRetentionSetRequest,
    ) -> Result<MeetingRetentionMutationResult, MeetingCommandError> {
        if matches!(
            request.policy,
            MeetingRetentionPolicy::DeleteAfterDays { days: 0 }
        ) {
            return Err(MeetingCommandError::InvalidRequest);
        }
        let store = self.store().await?;
        let (receipt, _) = store
            .set_default_retention_policy(
                request.operation_id,
                utc_now_ms(),
                request.expected_revision,
                &request.policy,
            )
            .map_err(map_store_error)?;
        let (policy, revision) = store.default_retention_policy().map_err(map_store_error)?;
        Ok(MeetingRetentionMutationResult {
            receipt,
            snapshot: MeetingRetentionSnapshot { policy, revision },
        })
    }

    pub async fn remote_cancel(
        &self,
        request: MeetingMutationRequest,
    ) -> Result<(), MeetingCommandError> {
        let snapshot = self
            .store()
            .await?
            .session_snapshot(request.session_id)
            .map_err(map_store_error)?;
        if snapshot.revision != request.expected_revision {
            return Err(MeetingCommandError::StaleRevision);
        }
        if !snapshot
            .allowed_actions
            .contains(&AllowedMeetingAction::CancelRemote)
        {
            return Err(MeetingCommandError::NotFound);
        }
        Err(MeetingCommandError::RemoteUnavailable)
    }

    /// Returns the startup-recovered cached store without opening another SQLCipher connection.
    pub(crate) async fn cloud_store(&self) -> Result<Arc<MeetingStore>, MeetingCommandError> {
        if !self.recovery_complete.load(Ordering::Acquire) {
            return Err(MeetingCommandError::StorageUnavailable);
        }
        self.store_lock()
            .clone()
            .ok_or(MeetingCommandError::StorageUnavailable)
    }
    pub(crate) async fn import_cloud_bundle(
        &self,
        bundle: super::cloud_bundle::CloudMeetingBundleV1,
    ) -> Result<MeetingSessionSnapshot, MeetingCommandError> {
        let store = self.cloud_store().await?;
        let session_id = bundle.import_into_store(&store).map_err(map_store_error)?;
        let snapshot = store
            .session_snapshot(session_id)
            .map_err(map_store_error)?;
        self.emit_session_changed(&snapshot);
        Ok(snapshot)
    }

    /// Reports whether the lifecycle actor currently owns a live capture lease.
    pub(crate) fn is_capture_active(&self) -> bool {
        self.actor_lock().active.is_some()
    }

    /// Mount the encrypted meeting store, or hand back the mount that already
    /// exists.
    ///
    /// The key is resolved before the state lock is taken, so nothing waits on
    /// the credential store while holding it. The database is then opened
    /// *under* that lock: startup recovery and Capture's first count read are
    /// dispatched in the same instant, and two callers opening the file at once
    /// run its migrations concurrently against one database, which fails one of
    /// them with `StorageUnavailable` and makes Capture report that meeting
    /// storage is gone on a healthy install.
    ///
    /// Crate-visible rather than module-visible because the query plane spans
    /// this store and dictation history and so cannot live under `meeting/`;
    /// it reads through this mount rather than opening a second connection to
    /// a database whose key, retention sweep and deletion cascade this store
    /// owns.
    pub(crate) async fn store(&self) -> Result<Arc<MeetingStore>, MeetingCommandError> {
        if let Some(store) = self.store_lock().clone() {
            return Ok(store);
        }
        let root = self
            .root
            .clone()
            .ok_or(MeetingCommandError::StorageUnavailable)?;
        let key = self
            .secrets
            .meeting_storage_key()
            .await
            .map_err(map_secret_error)?;
        let mut cached = self.store_lock();
        if let Some(store) = cached.clone() {
            return Ok(store);
        }
        let opened = MeetingStore::open(root, key).map_err(map_store_error)?;
        let store = cached.insert(opened).clone();
        drop(cached);
        super::workflow_engine::resume_pending_workflow_events(
            Arc::clone(&store),
            self.app.clone(),
        );
        Ok(store)
    }

    fn build_preflight_snapshot(
        &self,
        session_id: MeetingSessionId,
        request: &MeetingPreflightCreateRequest,
    ) -> MeetingPreflightSnapshot {
        let provider = Arc::clone(&*self.sources_lock());
        let sources = request
            .requested_sources
            .iter()
            .map(|source_kind| {
                let probe = provider.probe(*source_kind);
                MeetingSourceSnapshot {
                    track_id: None,
                    source_kind: *source_kind,
                    required: request.required_sources.contains(source_kind),
                    availability: probe.availability,
                    health: probe.health,
                    format: probe.negotiated_format,
                    last_durable_offset_ns: None,
                    gap_count: 0,
                }
            })
            .collect();
        MeetingPreflightSnapshot {
            session_id,
            revision: request.expected_revision,
            proposed_title: request.title.clone(),
            origin: request.origin,
            sources,
            storage: StorageAvailability::Available,
            local_processing: self.processing.local_processing_availability(),
            destination: request.destination.clone(),
            microphone_device_uid: request.microphone_device_uid.clone(),
            frozen_system_audio_application_bundle_ids: request
                .frozen_system_audio_application_bundle_ids
                .clone(),
            accepted_known_missing_sources: request.accepted_known_missing_sources.clone(),
            degraded_start_policy: request.degraded_start_policy,
            required_acknowledgements: request.required_sources.clone(),
            allowed_actions: vec![
                AllowedMeetingAction::RefreshPreflight,
                AllowedMeetingAction::CancelPreflight,
                AllowedMeetingAction::Start,
            ],
        }
    }

    fn build_plan(
        &self,
        session_id: MeetingSessionId,
        preflight_revision: u64,
        attempt_number: u32,
        preflight: &MeetingPreflightSnapshot,
        consent: &MeetingConsentInput,
        store: &MeetingStore,
    ) -> Result<MeetingRunPlan, MeetingCommandError> {
        let retention_policy = store
            .session_retention_policy(session_id)
            .map_err(map_store_error)?;
        let asr_model_id = self.processing.current_asr_model_id();
        let language = self
            .app
            .as_ref()
            .map(|app| crate::settings::get_settings(app).selected_language)
            .unwrap_or_else(|| "und".to_string());
        Ok(MeetingRunPlan {
            plan_id: MeetingPlanId::new(),
            session_id,
            consent_id: ConsentId::new(),
            attempt_number,
            schema_version: STORE_SCHEMA_VERSION,
            app_build: env!("CARGO_PKG_VERSION").to_string(),
            preflight_revision,
            requested_sources: preflight
                .sources
                .iter()
                .map(|source| source.source_kind)
                .collect(),
            required_sources: preflight
                .sources
                .iter()
                .filter(|source| source.required)
                .map(|source| source.source_kind)
                .collect(),
            accepted_known_missing_sources: consent.known_missing_sources_acknowledged.clone(),
            degraded_start_policy: consent.degraded_start_policy,
            microphone_device_uid: preflight.microphone_device_uid.clone(),
            frozen_system_audio_application_bundle_ids: preflight
                .frozen_system_audio_application_bundle_ids
                .clone(),
            session_clock_anchor: SessionClockAnchor {
                host_monotonic_anchor_ns: host_monotonic_now_ns(),
                wall_start_utc_ms: utc_now_ms(),
                clock_policy_version: 1,
            },
            storage: MeetingStoragePlan {
                format_version: 1,
                record_max_payload_bytes: DEFAULT_RECORD_MAX_PAYLOAD_BYTES,
                checkpoint_interval_ms: DEFAULT_CHECKPOINT_INTERVAL_MS,
                source_lane_sample_capacity: DEFAULT_SOURCE_SAMPLE_CAPACITY,
                source_lane_descriptor_capacity: DEFAULT_SOURCE_DESCRIPTOR_CAPACITY,
            },
            language,
            asr_model_id: asr_model_id.clone(),
            asr_model_version: asr_model_id,
            diarization_model_id: Some(crate::meeting::diarization::model_manifest().id.clone()),
            diarization_model_version: Some(
                crate::meeting::diarization::model_manifest()
                    .revision
                    .clone(),
            ),
            destination: consent.destination.clone(),
            remote_acknowledgement: consent.remote_acknowledgement.clone(),
            retention_policy,
        })
    }

    fn result_for_receipt(
        &self,
        store: Arc<MeetingStore>,
        receipt: OperationReceipt,
        session_id: MeetingSessionId,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        let snapshot = store
            .session_snapshot(session_id)
            .map_err(map_store_error)?;
        Ok(MeetingMutationResult { receipt, snapshot })
    }

    fn actor_lock(&self) -> std::sync::MutexGuard<'_, ActorState> {
        self.actor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn sources_lock(&self) -> std::sync::MutexGuard<'_, Arc<dyn MeetingSourceProvider>> {
        self.sources
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn store_lock(&self) -> std::sync::MutexGuard<'_, Option<Arc<MeetingStore>>> {
        self.store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(super) fn app_handle(&self) -> Option<&AppHandle> {
        self.app.as_ref()
    }

    pub(super) fn emit_artifact_changed(
        &self,
        session_id: Option<MeetingSessionId>,
        revision: u64,
    ) {
        if let Some(app) = &self.app {
            let _ = app.emit(
                "meeting:artifact-changed",
                MeetingEventPayload {
                    event_schema_version: MEETING_EVENT_SCHEMA_VERSION,
                    session_id,
                    revision,
                },
            );
        }
    }

    fn emit_session_changed(&self, snapshot: &MeetingSessionSnapshot) {
        if let Some(app) = &self.app {
            let _ = app.emit(
                "meeting:session-changed",
                MeetingEventPayload {
                    event_schema_version: MEETING_EVENT_SCHEMA_VERSION,
                    session_id: Some(snapshot.session_id),
                    revision: snapshot.revision,
                },
            );
        }
    }

    fn emit_removed(&self, session_id: MeetingSessionId, revision: u64) {
        if let Some(app) = &self.app {
            let _ = app.emit(
                "meeting:removed",
                MeetingEventPayload {
                    event_schema_version: MEETING_EVENT_SCHEMA_VERSION,
                    session_id: Some(session_id),
                    revision,
                },
            );
        }
    }
}

struct SessionSuggestionSink {
    manager: Arc<MeetingSessionManager>,
}

impl MeetingSuggestionSink for SessionSuggestionSink {
    fn submit(&self, signal: MeetingSuggestionSignal) {
        self.manager.suggestions.submit(signal);
        if let Some(app) = &self.manager.app {
            let _ = app.emit(
                "meeting:suggestion-changed",
                MeetingEventPayload {
                    event_schema_version: MEETING_EVENT_SCHEMA_VERSION,
                    session_id: None,
                    revision: 0,
                },
            );
        }
    }
}

impl TrackWorker {
    fn start(
        store: Arc<MeetingStore>,
        writer: MeetingTrackWriter,
        reader: PacketLaneReader,
        report: SourceStartReport,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let handle =
            thread::spawn(move || run_track_worker(store, writer, reader, report, worker_stop));
        Self {
            stop,
            handle: Some(handle),
        }
    }

    fn stop(&mut self) -> Result<(), StoreError> {
        self.stop.store(true, Ordering::Release);
        let Some(handle) = self.handle.take() else {
            return Ok(());
        };
        handle.join().map_err(|_| StoreError::Unavailable)??;
        Ok(())
    }
}

fn run_track_worker(
    store: Arc<MeetingStore>,
    mut writer: MeetingTrackWriter,
    mut reader: PacketLaneReader,
    report: SourceStartReport,
    stop: Arc<AtomicBool>,
) -> Result<(), StoreError> {
    let mut bridges = HashMap::new();
    bridges.insert(
        (report.epoch.get(), report.format_epoch),
        report.timestamp_bridge,
    );
    let mut samples = Vec::new();
    loop {
        let mut progressed = false;
        while let Some(epoch) = reader.pop_clock_epoch() {
            store.record_clock_epoch(epoch)?;
            bridges.insert((epoch.epoch.get(), epoch.format_epoch), epoch.bridge);
            progressed = true;
        }
        while let Some(gap) = reader.pop_gap() {
            store.record_gap(&gap)?;
            progressed = true;
        }
        if reader.take_gap_overflow() {
            store.record_gap(&SourceGap {
                track_id: report.track_id,
                epoch: report.epoch,
                start_offset_ns: None,
                end_offset_ns: None,
                reason: SourceGapReason::WriterPressure,
                dropped_frames: None,
            })?;
            progressed = true;
        }
        match reader.pop_into(&mut samples) {
            Ok(Some(packet)) => {
                let bridge = bridges
                    .get(&(packet.source_epoch.get(), packet.format_epoch))
                    .copied();
                if let Some(bridge) = bridge {
                    let _ = writer.accept_with_bridge(packet, &samples, bridge)?;
                } else {
                    store.record_gap(&SourceGap {
                        track_id: packet.track_id,
                        epoch: packet.source_epoch,
                        start_offset_ns: None,
                        end_offset_ns: None,
                        reason: SourceGapReason::TimestampDiscontinuity,
                        dropped_frames: Some(u64::from(packet.frame_count)),
                    })?;
                }
                progressed = true;
            }
            Ok(None) => {}
            Err(PacketLaneReadError::DescriptorWithoutSamples) => {
                store.record_gap(&SourceGap {
                    track_id: report.track_id,
                    epoch: report.epoch,
                    start_offset_ns: None,
                    end_offset_ns: None,
                    reason: SourceGapReason::PacketDropped,
                    dropped_frames: None,
                })?;
                progressed = true;
            }
        }
        if stop.load(Ordering::Acquire) && !progressed {
            return writer.seal();
        }
        if !progressed {
            thread::sleep(Duration::from_millis(5));
        }
    }
}

fn required_transition(
    store: &MeetingStore,
    request: &MeetingMutationRequest,
    command: MeetingCommandKind,
    allowed_from: &[MeetingPhase],
    next_phase: MeetingPhase,
    event_kind: &str,
) -> Result<OperationReceipt, MeetingCommandError> {
    required_transition_by(
        store,
        OperationActor::User,
        request,
        command,
        allowed_from,
        next_phase,
        event_kind,
    )
}

/// The same transition, attributed. Recovery is the one command the app also
/// issues on its own, and the receipt has to say which of the two did it.
#[allow(clippy::too_many_arguments)]
fn required_transition_by(
    store: &MeetingStore,
    actor: OperationActor,
    request: &MeetingMutationRequest,
    command: MeetingCommandKind,
    allowed_from: &[MeetingPhase],
    next_phase: MeetingPhase,
    event_kind: &str,
) -> Result<OperationReceipt, MeetingCommandError> {
    store
        .transition(StoreTransition {
            operation_id: Some(request.operation_id),
            actor,
            command,
            requested_at_utc_ms: utc_now_ms(),
            session_id: request.session_id,
            expected_revision: request.expected_revision,
            allowed_from,
            next_phase,
            event_kind,
            reason_codes: Vec::new(),
        })
        .map_err(map_store_error)?
        .ok_or(MeetingCommandError::InvalidRequest)
}

fn validate_destination(
    destination: &ProcessingDestination,
    acknowledgement: Option<&RemoteAcknowledgement>,
) -> Result<(), MeetingCommandError> {
    match destination {
        ProcessingDestination::Local => Ok(()),
        ProcessingDestination::Remote { destination_id } => acknowledgement
            .filter(|acknowledgement| acknowledgement.destination_id == *destination_id)
            .map(|_| ())
            .ok_or(MeetingCommandError::ConsentRequired),
    }
}

fn acknowledged_sources(consent: &MeetingConsentInput) -> Vec<SourceKind> {
    let mut sources = Vec::with_capacity(SourceKind::ALL.len());
    if consent.microphone_acknowledged {
        sources.push(SourceKind::Microphone);
    }
    if consent.system_audio_acknowledged {
        sources.push(SourceKind::SystemAudio);
    }
    sources
}

/// Who the room is told the notes are for.
///
/// The calendar account's own attendee entry is the only place this app learns
/// its operator's name: there is no `Person` for the user, and a speaker label
/// is whatever the diarizer called a voice. A meeting whose calendar names
/// nobody falls back to the meeting's own title, so the one disclosure sentence
/// always has something true to interpolate.
fn notetaker<'a>(calendar_event: Option<&'a CalendarEventSummary>, title: &'a str) -> &'a str {
    calendar_event
        .and_then(|event| {
            event
                .attendees
                .iter()
                .find(|attendee| attendee.is_self)
                .map(|attendee| attendee.name.trim())
        })
        .filter(|name| !name.is_empty())
        .unwrap_or(title)
}

#[cfg(all(target_os = "macos", not(test)))]
fn request_system_audio_permission_once() {
    static REQUESTED: AtomicBool = AtomicBool::new(false);
    if REQUESTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        // Screen Recording is the system-audio capture grant on macOS. The
        // result is deliberately not treated as authorization: the preflight
        // probe below remains the source of truth and opens the existing gate
        // when the operator declines.
        let _ = objc2_core_graphics::CGRequestScreenCaptureAccess();
    }
}

#[cfg(any(not(target_os = "macos"), test))]
fn request_system_audio_permission_once() {}

fn validate_consent(
    preflight: &MeetingPreflightSnapshot,
    consent: &MeetingConsentInput,
) -> Result<(), MeetingCommandError> {
    validate_destination(
        &consent.destination,
        consent.remote_acknowledgement.as_ref(),
    )?;
    if preflight.destination != consent.destination {
        return Err(MeetingCommandError::ConsentStale);
    }
    for source in &preflight.required_acknowledgements {
        match source {
            SourceKind::Microphone if !consent.microphone_acknowledged => {
                return Err(MeetingCommandError::ConsentRequired)
            }
            SourceKind::SystemAudio if !consent.system_audio_acknowledged => {
                return Err(MeetingCommandError::ConsentRequired)
            }
            _ => {}
        }
    }
    let unavailable_required = preflight
        .sources
        .iter()
        .filter(|source| source.required && source.availability != SourceAvailability::Available)
        .map(|source| source.source_kind)
        .collect::<Vec<_>>();
    if !unavailable_required.is_empty()
        && (consent.degraded_start_policy != DegradedStartPolicy::ContinueAndMarkPartial
            || !unavailable_required
                .iter()
                .all(|source| consent.known_missing_sources_acknowledged.contains(source)))
    {
        return Err(MeetingCommandError::SourceUnavailable);
    }
    Ok(())
}

fn map_secret_error(error: SecretResolveError) -> MeetingCommandError {
    match error {
        SecretResolveError::NotFound | SecretResolveError::Store(_) => {
            MeetingCommandError::StorageUnavailable
        }
    }
}

fn map_store_error(error: StoreError) -> MeetingCommandError {
    match error {
        StoreError::NotFound | StoreError::PersonNotFound | StoreError::SpeakerNotFound => {
            MeetingCommandError::NotFound
        }
        StoreError::ConsentStale => MeetingCommandError::ConsentStale,
        StoreError::ExplicitConsentRequired => MeetingCommandError::ConsentRequired,
        StoreError::Conflict => MeetingCommandError::InvalidTransition,
        StoreError::StaleRevision => MeetingCommandError::StaleRevision,
        StoreError::Invalid => MeetingCommandError::InvalidRequest,
        StoreError::LocalModelUnavailable => MeetingCommandError::LocalModelUnavailable,
        StoreError::LocalEvidenceUnavailable => MeetingCommandError::LocalEvidenceUnavailable,
        StoreError::InsufficientEnrollmentEvidence => {
            MeetingCommandError::InsufficientEnrollmentEvidence
        }
        StoreError::ProfileModelIncompatible => MeetingCommandError::ProfileModelIncompatible,
        StoreError::ProfileMergeResolutionRequired => {
            MeetingCommandError::ProfileMergeResolutionRequired
        }
        StoreError::EncryptionUnavailable
        | StoreError::StorageUnavailable
        | StoreError::Unavailable
        | StoreError::VoiceInvariant
        | StoreError::Io
        | StoreError::Corrupt => MeetingCommandError::StorageUnavailable,
    }
}

fn map_processing_error(error: ProcessingFailure) -> MeetingCommandError {
    match error {
        ProcessingFailure::LocalModelUnavailable => MeetingCommandError::LocalModelUnavailable,
        ProcessingFailure::RemoteUnavailable => MeetingCommandError::RemoteUnavailable,
        ProcessingFailure::Cancelled => MeetingCommandError::StaleRevision,
        // `Interrupted` is written by startup recovery, never returned by a
        // live operation. If one ever surfaces it, the meeting does need
        // recovery, which is the error that says so.
        ProcessingFailure::Interrupted => MeetingCommandError::RecoveryRequired,
        ProcessingFailure::EngineFailure => MeetingCommandError::EngineFailure,
    }
}

fn utc_now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// A capture timestamp as the `YYYY-MM-DD` an exported page parses by
/// splitting on `-`. Local, because the date a person means by "that meeting"
/// is the one their own clock showed.
fn local_iso_date(utc_ms: i64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(utc_ms).map(|utc| {
        utc.with_timezone(&chrono::Local)
            .format("%Y-%m-%d")
            .to_string()
    })
}
fn retention_operation_id(session_id: MeetingSessionId) -> MeetingOperationId {
    MeetingOperationId::from_uuid(Uuid::new_v5(
        &RETENTION_OPERATION_NAMESPACE,
        session_id.uuid().as_bytes(),
    ))
}

/// The title an import files under: what the caller asked for, else the file's
/// own name, else the same placeholder a meeting started from the tray carries
/// until its notes name it.
fn import_title(requested: Option<&str>, path: &Path) -> String {
    requested
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_string)
        .or_else(|| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().trim().to_string())
                .filter(|stem| !stem.is_empty())
        })
        .unwrap_or_else(|| MANUAL_DEFAULT_TITLE.to_string())
}

/// When the file was last written, as the recording's own timestamp.
fn file_modified_utc_ms(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(chrono::DateTime::<chrono::Utc>::from(modified).timestamp_millis())
}

/// The imported timeline's zero.
///
/// Native timestamps on an imported packet are sample indices at
/// `WHISPER_SAMPLE_RATE`, anchored at the file's first sample, so a record's
/// session offset is derived from the audio itself. Nothing about when the
/// decode ran enters the timeline — the same rule the live sources follow.
const fn import_timestamp_bridge() -> TimestampBridge {
    TimestampBridge {
        native_anchor_value: 0,
        native_timescale: WHISPER_SAMPLE_RATE,
        host_monotonic_anchor_ns: 0,
        session_offset_ns: 0,
    }
}

/// What a live source reports when it starts, for a source that is a file.
fn import_start_report(track_id: SourceTrackId) -> SourceStartReport {
    SourceStartReport {
        track_id,
        source_kind: SourceKind::Microphone,
        format: IMPORT_AUDIO_FORMAT,
        epoch: SourceEpoch::new(0),
        format_epoch: 0,
        timestamp_bridge: import_timestamp_bridge(),
    }
}

/// Write one decoded recording to its track. Runs on a blocking thread.
///
/// The decoder hands over one resampled frame at a time and each becomes one
/// record, so a live capture callback and an imported frame reach disk through
/// the same `accept_with_bridge` and produce the same records. The real-time
/// packet lane is skipped deliberately: it exists so an audio callback can never
/// block on the writer, and a file has no callback to protect.
fn decode_into_track(
    mut writer: MeetingTrackWriter,
    track_id: SourceTrackId,
    media: &ValidatedMediaPath,
) -> Result<(), MeetingCommandError> {
    let bridge = import_timestamp_bridge();
    let mut sequence = 0_u64;
    let mut frame_index = 0_u64;
    // A write that fails stops the decode. The decoder's own error type is the
    // only thing its callback can return, so the store's answer is parked here
    // and is the one reported.
    let mut store_error: Option<StoreError> = None;
    let decoded = decode_media_into(
        &media.canonical_path,
        &media.extension,
        &AtomicBool::new(false),
        MAX_IMPORT_RECORDING_SAMPLES,
        |frame| {
            let packet = CapturedPacket {
                track_id,
                source_epoch: SourceEpoch::new(0),
                format_epoch: 0,
                sequence,
                native_timestamp_value: i64::try_from(frame_index).ok(),
                native_timestamp_timescale: Some(WHISPER_SAMPLE_RATE),
                host_monotonic_anchor_ns: Some(0),
                sample_rate_hz: IMPORT_AUDIO_FORMAT.sample_rate_hz,
                channels: IMPORT_AUDIO_FORMAT.channels,
                frame_count: u32::try_from(frame.len()).map_err(|_| AudioImportError::decode())?,
                discontinuity_flags: PacketDiscontinuityFlags::default(),
            };
            match writer.accept_with_bridge(packet, frame, bridge) {
                Ok(_) => {
                    sequence = sequence.saturating_add(1);
                    frame_index = frame_index.saturating_add(u64::from(packet.frame_count));
                    Ok(())
                }
                Err(error) => {
                    store_error = Some(error);
                    Err(AudioImportError::decode())
                }
            }
        },
    );
    if let Some(error) = store_error {
        return Err(map_store_error(error));
    }
    match decoded {
        Ok(_) => writer.seal().map_err(map_store_error),
        Err(DecodeFailure::Cancelled) => Err(MeetingCommandError::ImportUnreadable),
        Err(DecodeFailure::Failed(error)) => {
            log::warn!(
                "Meeting import could not decode {}: {error}",
                media.file_name
            );
            Err(MeetingCommandError::ImportUnreadable)
        }
    }
}

/// Write an imported transcript as this session's transcript revision, with the
/// vendor's speaker names on the segments rather than as a diarization overlay:
/// on an imported transcript the names are the attribution, not a guess over it.
fn write_imported_transcript(
    store: &Arc<MeetingStore>,
    session_id: MeetingSessionId,
    track_id: SourceTrackId,
    segments: &[ImportedSegment],
    spans: &[(u64, u64)],
) -> Result<(), MeetingCommandError> {
    let revision_id = store
        .begin_transcript_revision(TranscriptRevisionInput {
            session_id,
            engine_id: IMPORT_TRANSCRIPT_ENGINE_ID,
            model_version: None,
            destination: &ProcessingDestination::Local,
            source_set: &[SourceKind::Microphone],
            language: "und",
        })
        .map_err(map_store_error)?;
    let inputs = segments
        .iter()
        .zip(spans)
        .map(|(segment, (start_ms, end_ms))| TranscriptSegmentInput {
            track_id,
            source_kind: SourceKind::Microphone,
            start_offset_ns: start_ms.saturating_mul(1_000_000),
            end_offset_ns: end_ms.saturating_mul(1_000_000),
            text: segment.text.clone(),
            confidence_milli: None,
            speaker: segment.speaker.clone(),
        })
        .collect::<Vec<_>>();
    store
        .append_transcript_segments(session_id, revision_id, &inputs)
        .map_err(map_store_error)?;
    store
        .complete_transcript_revision(session_id, revision_id)
        .map_err(map_store_error)
}

/// Crate-visible so the cloud-sync tests can drive a pulled recording through
/// this module's import, the way `meeting::store` shares its workflow
/// fixtures.
#[cfg(test)]
pub(crate) mod tests {
    use super::super::analytics::MeetingCatchUpState;
    use super::*;
    use crate::secrets::{MemorySecretBackend, SecretManager};
    use tempfile::TempDir;

    struct FakeSource {
        kind: SourceKind,
        starts: Arc<std::sync::atomic::AtomicUsize>,
        aborts: Arc<std::sync::atomic::AtomicUsize>,
        /// Where `start` leaves the lane it was handed, so a test can push
        /// audio down the real ingest path instead of pretending it did.
        lane: Arc<Mutex<Option<PacketSink>>>,
    }

    impl MeetingCaptureSource for FakeSource {
        fn probe(&self) -> SourceProbe {
            SourceProbe {
                source_kind: self.kind,
                availability: SourceAvailability::Available,
                health: SourceHealth::Healthy,
                detail: None,
                negotiated_format: Some(AudioFormat {
                    sample_rate_hz: 48_000,
                    channels: 1,
                }),
            }
        }

        fn start(
            &mut self,
            plan: SourceStartPlan,
            _anchor: SessionClockAnchor,
            sink: PacketSink,
        ) -> Result<SourceStartReport, MeetingCaptureError> {
            self.starts.fetch_add(1, Ordering::AcqRel);
            *self
                .lane
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(sink);
            Ok(SourceStartReport {
                track_id: plan.track_id,
                source_kind: plan.source_kind,
                format: AudioFormat {
                    sample_rate_hz: 48_000,
                    channels: 1,
                },
                epoch: SourceEpoch::new(0),
                format_epoch: 0,
                timestamp_bridge: TimestampBridge {
                    native_anchor_value: 0,
                    native_timescale: 1_000_000_000,
                    host_monotonic_anchor_ns: 0,
                    session_offset_ns: 0,
                },
            })
        }

        fn pause(&mut self) -> Result<(), MeetingCaptureError> {
            Ok(())
        }

        fn resume(
            &mut self,
            _epoch: SourceEpoch,
        ) -> Result<SourceStartReport, MeetingCaptureError> {
            Err(MeetingCaptureError::InvalidState)
        }

        fn stop(&mut self) -> Result<SourceStopReport, MeetingCaptureError> {
            Ok(SourceStopReport {
                track_id: SourceTrackId::new(),
                final_offset_ns: Some(0),
                health: SourceHealth::Healthy,
                observed_gaps: Vec::new(),
            })
        }

        fn abort(&mut self) -> Result<(), MeetingCaptureError> {
            self.aborts.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
    }

    struct FakeSources {
        starts: Arc<std::sync::atomic::AtomicUsize>,
        aborts: Arc<std::sync::atomic::AtomicUsize>,
        unavailable: Option<SourceKind>,
        lane: Arc<Mutex<Option<PacketSink>>>,
    }

    impl MeetingSourceProvider for FakeSources {
        fn probe(&self, source_kind: SourceKind) -> SourceProbe {
            if self.unavailable == Some(source_kind) {
                return SourceProbe {
                    source_kind,
                    availability: SourceAvailability::PermissionDenied,
                    health: SourceHealth::NotStarted,
                    detail: Some(SourceProbeDetail::Permission),
                    negotiated_format: None,
                };
            }
            SourceProbe {
                source_kind,
                availability: SourceAvailability::Available,
                health: SourceHealth::Healthy,
                detail: None,
                negotiated_format: Some(AudioFormat {
                    sample_rate_hz: 48_000,
                    channels: 1,
                }),
            }
        }

        fn acquire(
            &self,
            source_kind: SourceKind,
        ) -> Result<Box<dyn MeetingCaptureSource>, MeetingCaptureError> {
            if self.unavailable == Some(source_kind) {
                return Err(MeetingCaptureError::Unavailable);
            }
            Ok(Box::new(FakeSource {
                kind: source_kind,
                starts: Arc::clone(&self.starts),
                aborts: Arc::clone(&self.aborts),
                lane: Arc::clone(&self.lane),
            }))
        }
    }

    fn manager_with(sources: Arc<dyn MeetingSourceProvider>) -> (TempDir, MeetingSessionManager) {
        let directory = TempDir::new().unwrap();
        let secrets = Arc::new(SecretManager::with_backend(Arc::new(
            MemorySecretBackend::new(),
        )));
        let manager = MeetingSessionManager::with_parts(
            None,
            Some(directory.path().join("meetings")),
            secrets,
            sources,
        );
        (directory, manager)
    }

    fn manager() -> (
        TempDir,
        MeetingSessionManager,
        Arc<std::sync::atomic::AtomicUsize>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let aborts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (directory, manager) = manager_with(Arc::new(FakeSources {
            starts: Arc::clone(&starts),
            aborts: Arc::clone(&aborts),
            unavailable: None,
            lane: Arc::new(Mutex::new(None)),
        }));
        (directory, manager, starts, aborts)
    }
    fn review_ready_session(manager: &MeetingSessionManager) -> MeetingSessionSnapshot {
        tauri::async_runtime::block_on(async {
            let session_id = MeetingSessionId::new();
            let store = manager.store().await.unwrap();
            let request = MeetingPreflightCreateRequest {
                operation_id: MeetingOperationId::new(),
                expected_revision: 0,
                title: "Retention test".to_string(),
                origin: MeetingOrigin::Manual,
                suggestion_id: None,
                calendar_event_key: None,
                requested_sources: SourceKind::ALL.to_vec(),
                required_sources: SourceKind::ALL.to_vec(),
                accepted_known_missing_sources: Vec::new(),
                degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
                destination: ProcessingDestination::Local,
                remote_acknowledgement: None,
                microphone_device_uid: None,
                frozen_system_audio_application_bundle_ids: Vec::new(),
            };
            let preflight = manager.build_preflight_snapshot(session_id, &request);
            store
                .create_preflight(
                    StoreMutation {
                        operation_id: request.operation_id,
                        requested_at_utc_ms: 0,
                        session_id,
                        expected_revision: request.expected_revision,
                        command: MeetingCommandKind::PreflightCreate,
                    },
                    request.title,
                    request.origin,
                    preflight,
                    MeetingRetentionPolicy::DeleteAfterDays { days: 1 },
                )
                .unwrap();
            store
                .transition(StoreTransition {
                    operation_id: None,
                    actor: OperationActor::System,
                    command: MeetingCommandKind::Start,
                    requested_at_utc_ms: 0,
                    session_id,
                    expected_revision: 0,
                    allowed_from: &[MeetingPhase::Preflight],
                    next_phase: MeetingPhase::Starting,
                    event_kind: "test_start",
                    reason_codes: Vec::new(),
                })
                .unwrap();
            store
                .transition(StoreTransition {
                    operation_id: None,
                    actor: OperationActor::System,
                    command: MeetingCommandKind::Stop,
                    requested_at_utc_ms: 0,
                    session_id,
                    expected_revision: 1,
                    allowed_from: &[MeetingPhase::Starting],
                    next_phase: MeetingPhase::ReviewReady,
                    event_kind: "test_review_ready",
                    reason_codes: Vec::new(),
                })
                .unwrap();
            store.session_snapshot(session_id).unwrap()
        })
    }

    #[test]
    fn engine_processing_failures_keep_their_wire_error() {
        assert_eq!(
            serde_json::to_value(map_processing_error(ProcessingFailure::EngineFailure))
                .expect("command error serializes"),
            serde_json::json!("engine_failure"),
        );
    }

    #[test]
    fn meeting_trend_is_typed_unavailable_without_meeting_storage() {
        let secrets = Arc::new(SecretManager::with_backend(Arc::new(
            MemorySecretBackend::new(),
        )));
        let manager =
            MeetingSessionManager::with_parts(None, None, secrets, Arc::new(NoCaptureSources));
        let request = DashboardTrendRequest {
            range: crate::analytics::DashboardTrendRange::Days7,
        };

        let projection = tauri::async_runtime::block_on(manager.trend_projection(request));
        assert_eq!(
            projection,
            MeetingTrendProjection::Unavailable {
                range: crate::analytics::DashboardTrendRange::Days7
            }
        );
    }

    /// The provisioned credential-store key and an existing meetings root, the
    /// shape of every launch after the first.
    fn mounted_manager() -> (
        TempDir,
        Arc<MeetingSessionManager>,
        Arc<MemorySecretBackend>,
    ) {
        let directory = TempDir::new().unwrap();
        let backend = Arc::new(MemorySecretBackend::new());
        backend.insert("meeting_storage/database-key-v1", &"11".repeat(32));
        let secrets = Arc::new(SecretManager::with_backend(backend.clone()));
        let manager = Arc::new(MeetingSessionManager::with_parts(
            None,
            Some(directory.path().join("meetings")),
            secrets,
            Arc::new(NoCaptureSources),
        ));
        (directory, manager, backend)
    }

    /// Capture asks for meeting counts as the window paints, and it must not
    /// have to wait for a visit to the meetings surface. Startup recovery owns
    /// the mount; the count read reuses it rather than resolving the
    /// credential-store key again, so one launch is one credential-store read.
    #[test]
    fn startup_recovery_mounts_meeting_storage_for_capture_counts() {
        let (_directory, manager, backend) = mounted_manager();
        let request = DashboardTrendRequest {
            range: crate::analytics::DashboardTrendRange::Days30,
        };
        assert_eq!(backend.operation_count(), 0);

        let recovered = tauri::async_runtime::block_on(manager.recover_at_startup_at(0))
            .expect("startup recovery mounts meeting storage");
        assert!(recovered.is_empty());
        assert_eq!(
            backend.operation_count(),
            1,
            "startup mount reads the one provisioned key entry once"
        );

        let projection = tauri::async_runtime::block_on(manager.trend_projection(request));
        assert!(
            matches!(projection, MeetingTrendProjection::Available { .. }),
            "counts must be available without visiting the meetings surface, got {projection:?}"
        );
        assert_eq!(
            backend.operation_count(),
            1,
            "the count read reuses the startup mount instead of resolving another key"
        );
    }

    /// Startup recovery and Capture's first count read are dispatched in the
    /// same instant, so both reach an unmounted store. Opening the SQLCipher
    /// database twice would run its migrations concurrently against one file;
    /// every caller has to come back with the single mounted store.
    #[test]
    fn racing_first_callers_share_one_meeting_store_mount() {
        let (_directory, manager, _backend) = mounted_manager();
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let manager = Arc::clone(&manager);
                thread::spawn(move || {
                    tauri::async_runtime::block_on(manager.store())
                        .map(|store| Arc::as_ptr(&store).addr())
                })
            })
            .collect();

        let mounts = threads
            .into_iter()
            .map(|handle| handle.join().expect("mount thread"))
            .collect::<Result<Vec<_>, _>>()
            .expect("every racing caller mounts meeting storage");
        let first = mounts[0];
        assert!(
            mounts.iter().all(|mount| *mount == first),
            "racing callers opened more than one store: {mounts:?}"
        );
    }

    fn retention_deadline(snapshot: &MeetingSessionSnapshot) -> i64 {
        snapshot
            .retention_deadline_utc_ms
            .expect("review-ready test session has a deadline")
    }

    #[test]
    fn retention_sweep_deletes_due_review_ready_sessions() {
        let (_directory, manager, _starts, _aborts) = manager();
        let snapshot = review_ready_session(&manager);
        let result = tauri::async_runtime::block_on(
            manager.sweep_retention_at(retention_deadline(&snapshot)),
        )
        .unwrap();

        assert_eq!(
            result,
            RetentionSweepResult {
                due_sessions: 1,
                deleted_sessions: 1,
                failed_sessions: 0,
            }
        );
        assert!(matches!(
            tauri::async_runtime::block_on(manager.get(snapshot.session_id)),
            Err(MeetingCommandError::NotFound)
        ));
    }
    #[test]
    fn retention_sweep_does_not_take_another_session_capture() {
        let (_directory, manager, _starts, aborts) = manager();
        let due = review_ready_session(&manager);
        let preflight = tauri::async_runtime::block_on(manager.create_preflight(
            MeetingPreflightCreateRequest {
                operation_id: MeetingOperationId::new(),
                expected_revision: 0,
                title: "Active capture".to_string(),
                origin: MeetingOrigin::Manual,
                suggestion_id: None,
                calendar_event_key: None,
                requested_sources: SourceKind::ALL.to_vec(),
                required_sources: SourceKind::ALL.to_vec(),
                accepted_known_missing_sources: Vec::new(),
                degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
                destination: ProcessingDestination::Local,
                remote_acknowledgement: None,
                microphone_device_uid: None,
                frozen_system_audio_application_bundle_ids: Vec::new(),
            },
        ))
        .unwrap();
        let active = tauri::async_runtime::block_on(manager.start(MeetingStartRequest {
            operation_id: MeetingOperationId::new(),
            session_id: preflight.snapshot.session_id,
            expected_revision: preflight.snapshot.revision,
            consent: MeetingConsentInput {
                policy_version: 1,
                microphone_acknowledged: true,
                system_audio_acknowledged: true,
                known_missing_sources_acknowledged: Vec::new(),
                degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
                destination: ProcessingDestination::Local,
                remote_acknowledgement: None,
            },
        }))
        .unwrap();

        tauri::async_runtime::block_on(manager.sweep_retention_at(retention_deadline(&due)))
            .unwrap();
        assert_eq!(aborts.load(Ordering::Acquire), 0);
        assert_eq!(
            tauri::async_runtime::block_on(manager.stop(
                MeetingMutationRequest {
                    operation_id: MeetingOperationId::new(),
                    session_id: active.snapshot.session_id,
                    expected_revision: active.snapshot.revision,
                },
                MeetingStopCause::Operator(MeetingStopSurface::MeetingLive),
            ))
            .unwrap()
            .snapshot
            .phase,
            MeetingPhase::ReviewReady
        );
    }

    #[test]
    fn retention_sweep_leaves_not_due_review_ready_sessions() {
        let (_directory, manager, _starts, _aborts) = manager();
        let snapshot = review_ready_session(&manager);
        let result = tauri::async_runtime::block_on(
            manager.sweep_retention_at(retention_deadline(&snapshot).saturating_sub(1)),
        )
        .unwrap();

        assert_eq!(
            result,
            RetentionSweepResult {
                due_sessions: 0,
                deleted_sessions: 0,
                failed_sessions: 0,
            }
        );
        assert_eq!(
            tauri::async_runtime::block_on(manager.get(snapshot.session_id))
                .unwrap()
                .session
                .phase,
            MeetingPhase::ReviewReady
        );
    }

    #[test]
    fn retention_sweep_uses_an_idempotent_manager_deletion_operation() {
        let (_directory, manager, _starts, _aborts) = manager();
        let snapshot = review_ready_session(&manager);
        let operation_id = retention_operation_id(snapshot.session_id);
        tauri::async_runtime::block_on(manager.sweep_retention_at(retention_deadline(&snapshot)))
            .unwrap();
        let receipt = tauri::async_runtime::block_on(manager.store())
            .unwrap()
            .operation_receipt(operation_id)
            .unwrap()
            .expect("retention operation receipt");

        let duplicate = tauri::async_runtime::block_on(manager.delete_with_cause(
            MeetingMutationRequest {
                operation_id,
                session_id: snapshot.session_id,
                expected_revision: snapshot.revision,
            },
            DeletionCause::Retention,
        ))
        .unwrap();
        assert_eq!(duplicate.receipt, receipt);
        assert!(duplicate.removed);
        assert_eq!(
            tauri::async_runtime::block_on(
                manager.sweep_retention_at(retention_deadline(&snapshot)),
            )
            .unwrap(),
            RetentionSweepResult {
                due_sessions: 0,
                deleted_sessions: 0,
                failed_sessions: 0,
            }
        );
    }

    #[test]
    fn startup_retention_sweep_uses_the_injected_clock_after_restart() {
        let directory = TempDir::new().unwrap();
        let secrets = Arc::new(SecretManager::with_backend(Arc::new(
            MemorySecretBackend::new(),
        )));
        let root = directory.path().join("meetings");
        let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let aborts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let manager = MeetingSessionManager::with_parts(
            None,
            Some(root.clone()),
            Arc::clone(&secrets),
            Arc::new(FakeSources {
                starts: Arc::clone(&starts),
                aborts: Arc::clone(&aborts),
                unavailable: None,
                lane: Arc::new(Mutex::new(None)),
            }),
        );
        let snapshot = review_ready_session(&manager);
        drop(manager);

        let restarted = MeetingSessionManager::with_parts(
            None,
            Some(root),
            secrets,
            Arc::new(FakeSources {
                starts,
                aborts,
                unavailable: None,
                lane: Arc::new(Mutex::new(None)),
            }),
        );
        assert!(tauri::async_runtime::block_on(
            restarted.recover_at_startup_at(retention_deadline(&snapshot)),
        )
        .unwrap()
        .is_empty());
        assert!(matches!(
            tauri::async_runtime::block_on(restarted.get(snapshot.session_id)),
            Err(MeetingCommandError::NotFound)
        ));
    }

    #[test]
    fn failed_panel_start_does_not_create_standing_series_consent() {
        let directory = TempDir::new().unwrap();
        let secrets = Arc::new(SecretManager::with_backend(Arc::new(
            MemorySecretBackend::new(),
        )));
        let manager = MeetingSessionManager::with_parts(
            None,
            Some(directory.path().join("meetings")),
            secrets,
            Arc::new(FakeSources {
                starts: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                aborts: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                unavailable: Some(SourceKind::SystemAudio),
                lane: Arc::new(Mutex::new(None)),
            }),
        );
        let event = CalendarEventSummary {
            event_key: "failed-panel-event".to_string(),
            series_key: "failed-panel-series".to_string(),
            title: "Weekly review".to_string(),
            attendee_count: 2,
            start_utc_ms: 1_000,
            end_utc_ms: 2_000,
            attendees: Vec::new(),
            notes: None,
            calendar_name: None,
            url: None,
        };
        let context = MeetingDetectionStartContext {
            prompt_id: "failed-panel-prompt".to_string(),
            title: event.title.clone(),
            trigger_bundle_id: None,
            event_end_utc_ms: Some(event.end_utc_ms),
            calendar_event: Some(event),
        };
        let result = tauri::async_runtime::block_on(manager.start_from_consent_panel(
            &context,
            MeetingConsentPanelStartRequest {
                prompt_id: context.prompt_id.clone(),
                operation_id: MeetingOperationId::new(),
                consent: MeetingConsentInput {
                    policy_version: 1,
                    microphone_acknowledged: true,
                    system_audio_acknowledged: true,
                    known_missing_sources_acknowledged: vec![SourceKind::SystemAudio],
                    degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
                    destination: ProcessingDestination::Local,
                    remote_acknowledgement: None,
                },
                always_record_series: true,
                announce_in_chat: false,
            },
        ));

        assert!(result.is_err());
        assert!(
            tauri::async_runtime::block_on(manager.live_series_consent("failed-panel-series"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn acknowledged_partial_retry_starts_with_the_available_source() {
        let directory = TempDir::new().unwrap();
        let secrets = Arc::new(SecretManager::with_backend(Arc::new(
            MemorySecretBackend::new(),
        )));
        let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let aborts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let manager = MeetingSessionManager::with_parts(
            None,
            Some(directory.path().join("meetings")),
            secrets,
            Arc::new(FakeSources {
                starts: Arc::clone(&starts),
                aborts,
                unavailable: Some(SourceKind::SystemAudio),
                lane: Arc::new(Mutex::new(None)),
            }),
        );
        let preflight = tauri::async_runtime::block_on(manager.create_preflight(
            MeetingPreflightCreateRequest {
                operation_id: MeetingOperationId::new(),
                expected_revision: 0,
                title: "Acknowledged partial capture".to_string(),
                origin: MeetingOrigin::Manual,
                suggestion_id: None,
                calendar_event_key: None,
                requested_sources: SourceKind::ALL.to_vec(),
                required_sources: SourceKind::ALL.to_vec(),
                accepted_known_missing_sources: Vec::new(),
                degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
                destination: ProcessingDestination::Local,
                remote_acknowledgement: None,
                microphone_device_uid: None,
                frozen_system_audio_application_bundle_ids: Vec::new(),
            },
        ))
        .unwrap();

        let started = tauri::async_runtime::block_on(manager.start(MeetingStartRequest {
            operation_id: MeetingOperationId::new(),
            session_id: preflight.snapshot.session_id,
            expected_revision: preflight.snapshot.revision,
            consent: MeetingConsentInput {
                policy_version: 1,
                microphone_acknowledged: true,
                system_audio_acknowledged: true,
                known_missing_sources_acknowledged: vec![SourceKind::SystemAudio],
                degraded_start_policy: DegradedStartPolicy::ContinueAndMarkPartial,
                destination: ProcessingDestination::Local,
                remote_acknowledgement: None,
            },
        }))
        .unwrap();

        assert_eq!(started.snapshot.phase, MeetingPhase::CapturingRecording);
        assert_eq!(starts.load(Ordering::Acquire), 1);
        let plan = tauri::async_runtime::block_on(async {
            manager
                .store()
                .await
                .unwrap()
                .processing_plan(started.snapshot.session_id)
                .unwrap()
        });
        assert_eq!(
            plan.accepted_known_missing_sources,
            vec![SourceKind::SystemAudio]
        );
        assert_eq!(
            plan.degraded_start_policy,
            DegradedStartPolicy::ContinueAndMarkPartial
        );

        let discarded = tauri::async_runtime::block_on(manager.discard(MeetingMutationRequest {
            operation_id: MeetingOperationId::new(),
            session_id: started.snapshot.session_id,
            expected_revision: started.snapshot.revision,
        }))
        .unwrap();
        assert!(discarded.removed);
    }

    #[test]
    fn suggestion_has_no_capture_authority_until_start() {
        let (_directory, manager, starts, _aborts) = manager();
        manager
            .suggestion_service()
            .submit(MeetingSuggestionSignal {
                provider: super::super::suggestions::MeetingProvider::Zoom,
                app_bundle_id: "us.zoom.xos".to_string(),
                observed_at_ns: 1,
                evidence_flags: Default::default(),
            });
        assert_eq!(starts.load(Ordering::Acquire), 0);
        let preflight = tauri::async_runtime::block_on(manager.create_preflight(
            MeetingPreflightCreateRequest {
                operation_id: MeetingOperationId::new(),
                expected_revision: 0,
                title: "Design sync".to_string(),
                origin: MeetingOrigin::Manual,
                suggestion_id: None,
                calendar_event_key: None,
                requested_sources: SourceKind::ALL.to_vec(),
                required_sources: SourceKind::ALL.to_vec(),
                accepted_known_missing_sources: Vec::new(),
                degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
                destination: ProcessingDestination::Local,
                remote_acknowledgement: None,
                microphone_device_uid: None,
                frozen_system_audio_application_bundle_ids: Vec::new(),
            },
        ))
        .unwrap();
        assert_eq!(starts.load(Ordering::Acquire), 0);
        let _ = tauri::async_runtime::block_on(manager.start(MeetingStartRequest {
            operation_id: MeetingOperationId::new(),
            session_id: preflight.snapshot.session_id,
            expected_revision: preflight.snapshot.revision,
            consent: MeetingConsentInput {
                policy_version: 1,
                microphone_acknowledged: true,
                system_audio_acknowledged: true,
                known_missing_sources_acknowledged: Vec::new(),
                degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
                destination: ProcessingDestination::Local,
                remote_acknowledgement: None,
            },
        }))
        .unwrap();
        assert_eq!(starts.load(Ordering::Acquire), 2);
    }
    #[test]
    fn stale_discard_does_not_abort_an_active_capture() {
        let (_directory, manager, _starts, aborts) = manager();
        let preflight = tauri::async_runtime::block_on(manager.create_preflight(
            MeetingPreflightCreateRequest {
                operation_id: MeetingOperationId::new(),
                expected_revision: 0,
                title: "Design sync".to_string(),
                origin: MeetingOrigin::Manual,
                suggestion_id: None,
                calendar_event_key: None,
                requested_sources: SourceKind::ALL.to_vec(),
                required_sources: SourceKind::ALL.to_vec(),
                accepted_known_missing_sources: Vec::new(),
                degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
                destination: ProcessingDestination::Local,
                remote_acknowledgement: None,
                microphone_device_uid: None,
                frozen_system_audio_application_bundle_ids: Vec::new(),
            },
        ))
        .unwrap();
        let started = tauri::async_runtime::block_on(manager.start(MeetingStartRequest {
            operation_id: MeetingOperationId::new(),
            session_id: preflight.snapshot.session_id,
            expected_revision: preflight.snapshot.revision,
            consent: MeetingConsentInput {
                policy_version: 1,
                microphone_acknowledged: true,
                system_audio_acknowledged: true,
                known_missing_sources_acknowledged: Vec::new(),
                degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
                destination: ProcessingDestination::Local,
                remote_acknowledgement: None,
            },
        }))
        .unwrap();

        let stale = tauri::async_runtime::block_on(manager.discard(MeetingMutationRequest {
            operation_id: MeetingOperationId::new(),
            session_id: preflight.snapshot.session_id,
            expected_revision: preflight.snapshot.revision,
        }))
        .unwrap();
        assert!(!stale.removed);
        assert_eq!(aborts.load(Ordering::Acquire), 0);

        let removed = tauri::async_runtime::block_on(manager.discard(MeetingMutationRequest {
            operation_id: MeetingOperationId::new(),
            session_id: preflight.snapshot.session_id,
            expected_revision: started.snapshot.revision,
        }))
        .unwrap();
        assert!(removed.removed);
        assert_eq!(aborts.load(Ordering::Acquire), 2);
    }

    #[test]
    fn countdown_preflight_persists_the_exact_calendar_event() {
        let (_directory, manager, _, _) = manager();
        let event = CalendarEventSummary {
            event_key: "calendar-event-1".to_string(),
            series_key: "calendar-series-1".to_string(),
            title: "Roadmap review".to_string(),
            attendee_count: 2,
            start_utc_ms: 1_000,
            end_utc_ms: 2_000,
            attendees: Vec::new(),
            notes: Some("Review Q4".to_string()),
            calendar_name: Some("Work".to_string()),
            url: Some("https://meet.example/roadmap".to_string()),
        };
        let request = MeetingPreflightCreateRequest {
            operation_id: MeetingOperationId::new(),
            expected_revision: 0,
            title: event.title.clone(),
            origin: MeetingOrigin::Manual,
            suggestion_id: None,
            calendar_event_key: Some(event.event_key.clone()),
            requested_sources: SourceKind::ALL.to_vec(),
            required_sources: SourceKind::ALL.to_vec(),
            accepted_known_missing_sources: Vec::new(),
            degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
            destination: ProcessingDestination::Local,
            remote_acknowledgement: None,
            microphone_device_uid: None,
            frozen_system_audio_application_bundle_ids: Vec::new(),
        };
        let result = tauri::async_runtime::block_on(
            manager.create_preflight_with_calendar(request, Some(event.clone())),
        )
        .unwrap();
        let store = tauri::async_runtime::block_on(manager.store()).unwrap();
        assert_eq!(
            store
                .meeting_calendar_facts(result.snapshot.session_id)
                .unwrap(),
            Some(event)
        );
    }

    #[test]
    fn agent_hook_acknowledges_only_durable_event_insertions() {
        let (_directory, manager, _, _) = manager();
        assert!(tauri::async_runtime::block_on(
            manager.record_agent_hook_event("request-1".to_string(), "permission".to_string())
        ));

        let secrets = Arc::new(SecretManager::with_backend(Arc::new(
            MemorySecretBackend::new(),
        )));
        let unavailable =
            MeetingSessionManager::with_parts(None, None, secrets, Arc::new(NoCaptureSources));
        assert!(!tauri::async_runtime::block_on(
            unavailable.record_agent_hook_event("request-2".to_string(), "permission".to_string(),)
        ));
    }

    /* --------------------------------- automatic recovery reprocessing pass */

    /// An engine that answers everything asked of it. Zero-track meetings then
    /// run the whole pipeline to a successful finish, which is what these tests
    /// need: the subject is the orchestration, not the audio.
    struct ReadyEngine;

    impl super::super::processing::MeetingTranscriptEngine for ReadyEngine {
        fn selected_model_id(&self) -> Option<String> {
            Some("fake-asr".to_string())
        }

        fn plan_for(&self, _run_plan: &MeetingRunPlan) -> Option<crate::modes::AsrPlan> {
            Some(crate::modes::AsrPlan::from_settings(
                &crate::settings::AppSettings::default(),
            ))
        }

        fn engine_id(&self) -> &'static str {
            "fake-asr"
        }

        fn transcribe(
            &self,
            _plan: &crate::modes::AsrPlan,
            _samples: &[f32],
        ) -> Result<String, ProcessingFailure> {
            Ok(String::new())
        }

        fn is_busy(&self) -> bool {
            false
        }
    }

    /// The cold-start race, as a double: the availability probe passes because
    /// a model is selected, and the meeting's own plan is then refused. This is
    /// the failure the eligibility gate cannot see coming, so it is the one
    /// that has to land somewhere safe.
    struct ProbeOnlyEngine;

    impl super::super::processing::MeetingTranscriptEngine for ProbeOnlyEngine {
        fn selected_model_id(&self) -> Option<String> {
            Some("fake-asr".to_string())
        }

        fn plan_for(&self, run_plan: &MeetingRunPlan) -> Option<crate::modes::AsrPlan> {
            // The probe asks with no sources on the plan; a real meeting has
            // the sources it recorded.
            run_plan.requested_sources.is_empty().then(|| {
                crate::modes::AsrPlan::from_settings(&crate::settings::AppSettings::default())
            })
        }

        fn engine_id(&self) -> &'static str {
            "fake-asr"
        }

        fn transcribe(
            &self,
            _plan: &crate::modes::AsrPlan,
            _samples: &[f32],
        ) -> Result<String, ProcessingFailure> {
            Err(ProcessingFailure::EngineFailure)
        }

        fn is_busy(&self) -> bool {
            false
        }
    }

    /// An engine that dies where nothing catches it but `run`.
    struct PanickingEngine;

    impl super::super::processing::MeetingTranscriptEngine for PanickingEngine {
        fn selected_model_id(&self) -> Option<String> {
            Some("fake-asr".to_string())
        }

        fn plan_for(&self, run_plan: &MeetingRunPlan) -> Option<crate::modes::AsrPlan> {
            assert!(
                run_plan.requested_sources.is_empty(),
                "the engine panics on a real meeting's plan"
            );
            Some(crate::modes::AsrPlan::from_settings(
                &crate::settings::AppSettings::default(),
            ))
        }

        fn engine_id(&self) -> &'static str {
            "fake-asr"
        }

        fn transcribe(
            &self,
            _plan: &crate::modes::AsrPlan,
            _samples: &[f32],
        ) -> Result<String, ProcessingFailure> {
            Ok(String::new())
        }

        fn is_busy(&self) -> bool {
            false
        }
    }

    /// A meeting a launch left behind in `phase`, with the plan and consent a
    /// real one carries — recovery reads both — and no tracks, so nothing about
    /// its audio is in question. Retention is a day, so the retention deadline
    /// is observable when processing stamps one.
    fn interrupted_session(
        manager: &MeetingSessionManager,
        phase: MeetingPhase,
        destination: ProcessingDestination,
    ) -> MeetingSessionId {
        tauri::async_runtime::block_on(async {
            let session_id = MeetingSessionId::new();
            let store = manager.store().await.unwrap();
            let request = MeetingPreflightCreateRequest {
                operation_id: MeetingOperationId::new(),
                expected_revision: 0,
                title: "Interrupted sync".to_string(),
                origin: MeetingOrigin::Manual,
                suggestion_id: None,
                calendar_event_key: None,
                requested_sources: vec![SourceKind::Microphone],
                required_sources: vec![SourceKind::Microphone],
                accepted_known_missing_sources: Vec::new(),
                degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
                destination: destination.clone(),
                remote_acknowledgement: None,
                microphone_device_uid: None,
                frozen_system_audio_application_bundle_ids: Vec::new(),
            };
            let preflight = manager.build_preflight_snapshot(session_id, &request);
            store
                .create_preflight(
                    StoreMutation {
                        operation_id: request.operation_id,
                        requested_at_utc_ms: 0,
                        session_id,
                        expected_revision: 0,
                        command: MeetingCommandKind::PreflightCreate,
                    },
                    request.title.clone(),
                    request.origin,
                    preflight,
                    MeetingRetentionPolicy::DeleteAfterDays { days: 1 },
                )
                .unwrap();
            let consent_id = ConsentId::new();
            let plan = MeetingRunPlan {
                plan_id: MeetingPlanId::new(),
                session_id,
                consent_id,
                attempt_number: 1,
                schema_version: 1,
                app_build: "test".to_string(),
                preflight_revision: 0,
                requested_sources: vec![SourceKind::Microphone],
                required_sources: vec![SourceKind::Microphone],
                accepted_known_missing_sources: Vec::new(),
                degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
                microphone_device_uid: None,
                frozen_system_audio_application_bundle_ids: Vec::new(),
                session_clock_anchor: SessionClockAnchor {
                    host_monotonic_anchor_ns: 0,
                    wall_start_utc_ms: 0,
                    clock_policy_version: 1,
                },
                storage: MeetingStoragePlan {
                    format_version: 1,
                    record_max_payload_bytes: 4_096,
                    checkpoint_interval_ms: 1,
                    source_lane_sample_capacity: 1_024,
                    source_lane_descriptor_capacity: 4,
                },
                language: "en".to_string(),
                asr_model_id: Some("fake-asr".to_string()),
                asr_model_version: Some("fake-asr".to_string()),
                diarization_model_id: None,
                diarization_model_version: None,
                destination: destination.clone(),
                remote_acknowledgement: None,
                retention_policy: MeetingRetentionPolicy::DeleteAfterDays { days: 1 },
            };
            let consent = MeetingConsent {
                consent_id,
                session_id,
                attempt_number: 1,
                preflight_revision: 0,
                policy_version: 1,
                acknowledged_at_utc_ms: 0,
                provenance: MeetingConsentProvenance::Direct,
                microphone_acknowledged: true,
                system_audio_acknowledged: false,
                known_missing_sources_acknowledged: Vec::new(),
                degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
                destination,
                remote_acknowledgement: None,
            };
            store
                .start_with_plan_and_consent(MeetingOperationId::new(), 0, &plan, &consent, 0)
                .unwrap();
            if phase != MeetingPhase::Starting {
                let revision = store.session_snapshot(session_id).unwrap().revision;
                store
                    .transition(StoreTransition {
                        operation_id: None,
                        actor: OperationActor::System,
                        command: MeetingCommandKind::Stop,
                        requested_at_utc_ms: 0,
                        session_id,
                        expected_revision: revision,
                        allowed_from: &[MeetingPhase::Starting],
                        next_phase: phase,
                        event_kind: "test_phase",
                        reason_codes: Vec::new(),
                    })
                    .unwrap();
            }
            session_id
        })
    }

    /// Matrix 6: the whole point of the pass. A meeting interrupted after its
    /// stop is transcribed without being asked, through the same command a
    /// person's Retry uses, and the receipt says the app did it.
    #[test]
    fn automatic_reprocessing_finishes_a_meeting_interrupted_after_the_stop() {
        let (_directory, manager, _backend) = mounted_manager();
        manager
            .processing
            .set_transcript_engine(Arc::new(ReadyEngine));
        let session_id = interrupted_session(
            &manager,
            MeetingPhase::Processing,
            ProcessingDestination::Local,
        );

        let recovered = tauri::async_runtime::block_on(manager.recover_at_startup_at(0)).unwrap();
        let result = tauri::async_runtime::block_on(manager.reprocess_recovered(&recovered));

        assert_eq!(
            result,
            RecoveryReprocessResult {
                attempted: 1,
                succeeded: 1,
                skipped: 0,
            }
        );
        let store = tauri::async_runtime::block_on(manager.store()).unwrap();
        let snapshot = store.session_snapshot(session_id).unwrap();
        assert_eq!(snapshot.phase, MeetingPhase::ReviewReady);
        assert_eq!(snapshot.processing_status, ProcessingStatus::Succeeded);
        assert!(
            snapshot.retention_deadline_utc_ms.is_some(),
            "a meeting that reached review through a successful pass starts its retention clock"
        );
        let receipt = store
            .operation_receipt(manager.recovery_operation_id(session_id))
            .unwrap()
            .expect("an automatic attempt leaves a receipt to read afterwards");
        assert_eq!(receipt.actor, OperationActor::System);
        assert_eq!(receipt.command, MeetingCommandKind::RecoveryFinalize);
        assert_eq!(receipt.result, OperationResult::Committed);
        assert_eq!(receipt.from_phase, Some(MeetingPhase::RecoveryRequired));
        assert_eq!(receipt.to_phase, Some(MeetingPhase::Processing));
    }

    /// Matrix 7: no model installed on this launch. The attempt is not spent,
    /// so the next launch — with the model downloaded — still has one.
    #[test]
    fn automatic_reprocessing_withholds_the_attempt_without_a_local_model() {
        let (_directory, manager, _backend) = mounted_manager();
        let session_id = interrupted_session(
            &manager,
            MeetingPhase::Processing,
            ProcessingDestination::Local,
        );

        let recovered = tauri::async_runtime::block_on(manager.recover_at_startup_at(0)).unwrap();
        let before = tauri::async_runtime::block_on(manager.store())
            .unwrap()
            .session_snapshot(session_id)
            .unwrap();
        let result = tauri::async_runtime::block_on(manager.reprocess_recovered(&recovered));

        assert_eq!(result.attempted, 0);
        assert_eq!(result.skipped, 1);
        let store = tauri::async_runtime::block_on(manager.store()).unwrap();
        assert_eq!(store.session_snapshot(session_id).unwrap(), before);
        assert!(store
            .operation_receipt(manager.recovery_operation_id(session_id))
            .unwrap()
            .is_none());
    }

    /// Matrix 8, the hazard this design exists to avoid: a failed automatic
    /// attempt must not walk the meeting into review. Review stamps a retention
    /// deadline, and a meeting nobody has read would then be deleted on a timer
    /// with nothing left to retry it with.
    #[test]
    fn a_failed_automatic_attempt_returns_the_meeting_to_recovery() {
        let (_directory, manager, _backend) = mounted_manager();
        manager
            .processing
            .set_transcript_engine(Arc::new(ProbeOnlyEngine));
        let session_id = interrupted_session(
            &manager,
            MeetingPhase::Processing,
            ProcessingDestination::Local,
        );

        let recovered = tauri::async_runtime::block_on(manager.recover_at_startup_at(0)).unwrap();
        let result = tauri::async_runtime::block_on(manager.reprocess_recovered(&recovered));

        assert_eq!(result.attempted, 1);
        assert_eq!(result.succeeded, 0);
        let store = tauri::async_runtime::block_on(manager.store()).unwrap();
        let snapshot = store.session_snapshot(session_id).unwrap();
        assert_eq!(snapshot.phase, MeetingPhase::RecoveryRequired);
        assert_eq!(
            snapshot.processing_status,
            ProcessingStatus::Failed {
                reason: ProcessingFailure::LocalModelUnavailable
            }
        );
        assert_eq!(
            snapshot.retention_deadline_utc_ms, None,
            "audio nobody has read must not be on a deletion timer"
        );
        let month_later = utc_now_ms() + 31 * 24 * 60 * 60 * 1_000;
        assert!(store
            .due_retention_sessions(month_later)
            .unwrap()
            .is_empty());
    }

    /// Matrix 13: a pipeline that panics still leaves the outcome written down.
    /// Before, the thread died with it and the meeting read as processing until
    /// the next launch.
    #[test]
    fn a_panicking_pipeline_still_records_a_terminal_status() {
        let (_directory, manager, _backend) = mounted_manager();
        manager
            .processing
            .set_transcript_engine(Arc::new(PanickingEngine));
        let session_id = interrupted_session(
            &manager,
            MeetingPhase::Processing,
            ProcessingDestination::Local,
        );

        let recovered = tauri::async_runtime::block_on(manager.recover_at_startup_at(0)).unwrap();
        tauri::async_runtime::block_on(manager.reprocess_recovered(&recovered));

        let snapshot = tauri::async_runtime::block_on(manager.store())
            .unwrap()
            .session_snapshot(session_id)
            .unwrap();
        assert_eq!(
            snapshot.processing_status,
            ProcessingStatus::Failed {
                reason: ProcessingFailure::EngineFailure
            }
        );
        assert_eq!(snapshot.phase, MeetingPhase::RecoveryRequired);
    }

    /// Matrix 9: a meeting the person set up to be processed elsewhere is not
    /// something to quietly process here.
    #[test]
    fn automatic_reprocessing_withholds_a_remote_meeting() {
        let (_directory, manager, _backend) = mounted_manager();
        manager
            .processing
            .set_transcript_engine(Arc::new(ReadyEngine));
        let session_id = interrupted_session(
            &manager,
            MeetingPhase::Processing,
            ProcessingDestination::Remote {
                destination_id: "remote-1".to_string(),
            },
        );

        let recovered = tauri::async_runtime::block_on(manager.recover_at_startup_at(0)).unwrap();
        let result = tauri::async_runtime::block_on(manager.reprocess_recovered(&recovered));

        assert_eq!(result.attempted, 0);
        assert_eq!(result.skipped, 1);
        assert_eq!(
            tauri::async_runtime::block_on(manager.store())
                .unwrap()
                .session_snapshot(session_id)
                .unwrap()
                .phase,
            MeetingPhase::RecoveryRequired
        );
    }

    /// Matrix 12: a meeting that lost audio on disk. Rebuilding a transcript
    /// from the tracks that survived would quietly replace the meeting with a
    /// fraction of itself, so the pass leaves it, and finalizing what was
    /// captured stays on offer to the person.
    #[test]
    fn automatic_reprocessing_withholds_a_meeting_missing_audio_on_disk() {
        let (_directory, manager, _backend) = mounted_manager();
        manager
            .processing
            .set_transcript_engine(Arc::new(ReadyEngine));
        let session_id = interrupted_session(
            &manager,
            MeetingPhase::Processing,
            ProcessingDestination::Local,
        );
        let store = tauri::async_runtime::block_on(manager.store()).unwrap();
        // A track the interrupted launch registered and never wrote a byte for.
        store
            .create_track(TrackCreation {
                session_id,
                plan_id: store.processing_plan(session_id).unwrap().plan_id,
                source_kind: SourceKind::Microphone,
                required: true,
                requested: true,
                descriptor_json: "{}",
                report: SourceStartReport {
                    track_id: SourceTrackId::new(),
                    source_kind: SourceKind::Microphone,
                    format: AudioFormat {
                        sample_rate_hz: 48_000,
                        channels: 1,
                    },
                    epoch: SourceEpoch::new(0),
                    format_epoch: 1,
                    timestamp_bridge: TimestampBridge {
                        native_anchor_value: 0,
                        native_timescale: 1_000_000_000,
                        host_monotonic_anchor_ns: 0,
                        session_offset_ns: 0,
                    },
                },
            })
            .unwrap();

        let recovered = tauri::async_runtime::block_on(manager.recover_at_startup_at(0)).unwrap();
        let result = tauri::async_runtime::block_on(manager.reprocess_recovered(&recovered));

        assert!(store.has_missing_record_gap(session_id).unwrap());
        assert_eq!(result.attempted, 0);
        assert_eq!(result.skipped, 1);
        let snapshot = store.session_snapshot(session_id).unwrap();
        assert_eq!(snapshot.phase, MeetingPhase::RecoveryRequired);
        assert!(snapshot
            .allowed_actions
            .contains(&AllowedMeetingAction::FinalizePartial));
    }

    /// Matrix 2 and the pass's side of it: a recording cut mid-capture is the
    /// person's call, not the app's. It keeps its place in recovery, where
    /// finalizing what was captured is still offered.
    #[test]
    fn automatic_reprocessing_withholds_a_capture_interrupted_mid_recording() {
        let (_directory, manager, _backend) = mounted_manager();
        manager
            .processing
            .set_transcript_engine(Arc::new(ReadyEngine));
        let session_id = interrupted_session(
            &manager,
            MeetingPhase::CapturingRecording,
            ProcessingDestination::Local,
        );

        let recovered = tauri::async_runtime::block_on(manager.recover_at_startup_at(0)).unwrap();
        let result = tauri::async_runtime::block_on(manager.reprocess_recovered(&recovered));

        assert_eq!(result.attempted, 0);
        assert_eq!(result.skipped, 1);
        let snapshot = tauri::async_runtime::block_on(manager.store())
            .unwrap()
            .session_snapshot(session_id)
            .unwrap();
        assert_eq!(snapshot.phase, MeetingPhase::RecoveryRequired);
        assert!(snapshot
            .allowed_actions
            .contains(&AllowedMeetingAction::FinalizePartial));
    }

    /// Matrix 10: Retry pressed on a meeting a job already has is refused by
    /// the phase, not by anything the interface remembers. A list row can be
    /// seconds out of date; the fence cannot.
    #[test]
    fn retry_is_refused_while_the_meeting_is_already_processing() {
        let (_directory, manager, _backend) = mounted_manager();
        // A meeting with a live job on it: phase Processing, nothing swept.
        let session_id = interrupted_session(
            &manager,
            MeetingPhase::Processing,
            ProcessingDestination::Local,
        );
        let store = tauri::async_runtime::block_on(manager.store()).unwrap();
        let current = store.session_snapshot(session_id).unwrap();

        let refused =
            tauri::async_runtime::block_on(manager.recovery_finalize(MeetingMutationRequest {
                operation_id: MeetingOperationId::new(),
                session_id,
                expected_revision: current.revision,
            }))
            .unwrap();

        assert_eq!(refused.receipt.result, OperationResult::Rejected);
        assert!(
            refused
                .receipt
                .reason_codes
                .contains(&MeetingReasonCode::InvalidTransition),
            "the refusal comes from the phase, with the revision up to date"
        );
        assert_eq!(
            store.session_snapshot(session_id).unwrap().phase,
            MeetingPhase::Processing,
            "the meeting stays with the job that already has it, and no second job starts"
        );
    }

    /// Matrix 11: one automatic attempt per launch, and a fresh one next
    /// launch. The same meeting seen twice in one launch is deduplicated by the
    /// receipt the first attempt wrote; a new launch draws a new namespace and
    /// may try again.
    #[test]
    fn automatic_attempts_are_one_per_launch_and_renewed_by_the_next() {
        let (_directory, manager, _backend) = mounted_manager();
        manager
            .processing
            .set_transcript_engine(Arc::new(ProbeOnlyEngine));
        let session_id = interrupted_session(
            &manager,
            MeetingPhase::Processing,
            ProcessingDestination::Local,
        );
        let recovered = tauri::async_runtime::block_on(manager.recover_at_startup_at(0)).unwrap();

        let first = tauri::async_runtime::block_on(manager.reprocess_recovered(&recovered));
        let repeat = tauri::async_runtime::block_on(manager.reprocess_recovered(&recovered));

        assert_eq!(first.attempted, 1);
        assert_eq!(
            repeat.attempted, 1,
            "the pass still counts the meeting, and the receipt is what stops the work"
        );
        let store = tauri::async_runtime::block_on(manager.store()).unwrap();
        let receipt = store
            .operation_receipt(manager.recovery_operation_id(session_id))
            .unwrap()
            .expect("the first attempt's receipt");
        assert_eq!(receipt.result, OperationResult::Committed);
        let events = tauri::async_runtime::block_on(manager.store())
            .unwrap()
            .session_snapshot(session_id)
            .unwrap();
        assert_eq!(events.phase, MeetingPhase::RecoveryRequired);

        // A different launch of the same app, reading the same store.
        let next_launch = Arc::new(MeetingSessionManager::with_parts(
            None,
            manager.root.clone(),
            Arc::clone(&manager.secrets),
            Arc::new(NoCaptureSources),
        ));
        assert_ne!(
            next_launch.recovery_operation_id(session_id),
            manager.recovery_operation_id(session_id),
            "a per-launch namespace is what keeps yesterday's receipt from silencing today's attempt"
        );
    }

    /// An engine that returns the same words for every chunk, so a segment's
    /// presence and its offsets are what a test reads.
    struct TranscribingEngine;

    impl super::super::processing::MeetingTranscriptEngine for TranscribingEngine {
        fn selected_model_id(&self) -> Option<String> {
            Some("fake-asr".to_string())
        }

        fn plan_for(&self, _run_plan: &MeetingRunPlan) -> Option<crate::modes::AsrPlan> {
            Some(crate::modes::AsrPlan::from_settings(
                &crate::settings::AppSettings::default(),
            ))
        }

        fn engine_id(&self) -> &'static str {
            "fake-asr"
        }

        fn transcribe(
            &self,
            _plan: &crate::modes::AsrPlan,
            _samples: &[f32],
        ) -> Result<String, ProcessingFailure> {
            Ok("imported words".to_string())
        }

        fn is_busy(&self) -> bool {
            false
        }
    }

    /// Everything is speech, so the chunker cuts on length alone and the fixture
    /// does not have to be shaped like a voice.
    struct AlwaysVoice;

    impl super::super::processing::MeetingVad for AlwaysVoice {
        fn is_voice(&mut self, _frame: &[f32]) -> Result<bool, ProcessingFailure> {
            Ok(true)
        }
    }

    struct AlwaysVoiceFactory;

    impl super::super::processing::MeetingVadFactory for AlwaysVoiceFactory {
        fn open(
            &self,
            _source_kind: SourceKind,
        ) -> Result<Box<dyn super::super::processing::MeetingVad>, ProcessingFailure> {
            Ok(Box::new(AlwaysVoice))
        }
    }

    /// A complete 16 kHz mono WAV. The decoder verifies a WAV's declared frame
    /// count against what it decodes, so the fixture has to be whole. The
    /// count is a `u16`, which an `f32` holds exactly, so the tone's phase is.
    fn write_mono_wav(path: &Path, samples: u16) {
        let specification = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(path, specification).expect("create wav");
        for index in 0..samples {
            let phase = f32::from(index) / 16_000.0 * std::f32::consts::TAU * 220.0;
            writer
                .write_sample(phase.sin() * 0.4)
                .expect("write sample");
        }
        writer.finalize().expect("finalize wav");
    }

    /// A manager whose processing runs without a model: the transcript engine
    /// returns the same words for every chunk and everything counts as speech.
    pub(crate) fn importing_manager() -> (TempDir, Arc<MeetingSessionManager>) {
        let (directory, manager, _backend) = mounted_manager();
        manager
            .processing
            .set_transcript_engine(Arc::new(TranscribingEngine));
        manager
            .processing
            .set_vad_factory(Arc::new(AlwaysVoiceFactory));
        (directory, manager)
    }

    fn retained_meetings(manager: &MeetingSessionManager) -> Vec<MeetingHistorySummary> {
        tauri::async_runtime::block_on(manager.store())
            .unwrap()
            .list_sessions(None, 100, &MeetingListFilter::default())
            .unwrap()
            .entries
    }

    /// The whole point of the import: what lands is a meeting, not a special
    /// kind of one. It reaches the phase a stopped recording reaches, through
    /// the same processing job, with a transcript over a microphone track whose
    /// records were written by the capture writer.
    #[test]
    fn an_imported_recording_reaches_review_with_a_transcript() {
        let (files, manager) = importing_manager();
        let path = files.path().join("Team sync.wav");
        write_mono_wav(&path, 16_000);

        let snapshot =
            tauri::async_runtime::block_on(manager.import_recording(ImportRecordingRequest {
                path,
                title: None,
                recorded_at_utc_ms: Some(1_700_000_000_000),
                origin: RecordingOrigin::LocalFile,
            }))
            .expect("the recording imports");

        // Handed back mid-pipeline, exactly as `stop` hands a meeting back.
        assert_eq!(snapshot.phase, MeetingPhase::Processing);
        assert_eq!(snapshot.title, "Team sync");
        assert_eq!(snapshot.started_at_utc_ms, Some(1_700_000_000_000));

        assert_eq!(
            tauri::async_runtime::block_on(manager.store())
                .expect("meeting store mounts")
                .meeting_origin(snapshot.session_id)
                .expect("imported meeting preserves its origin"),
            MeetingOrigin::Import,
        );

        let review = tauri::async_runtime::block_on(manager.get(snapshot.session_id))
            .expect("the imported meeting is readable");
        assert_eq!(review.session.phase, MeetingPhase::ReviewReady);
        assert_eq!(review.tracks.len(), 1);
        assert_eq!(review.tracks[0].source_kind, SourceKind::Microphone);
        assert_eq!(
            review.tracks[0].format,
            Some(AudioFormat {
                sample_rate_hz: 16_000,
                channels: 1
            })
        );
        assert!(
            review.tracks[0].durable_record_count > 0,
            "the decode was written through the capture writer"
        );
        assert!(
            review
                .transcript
                .iter()
                .all(|segment| segment.base.text == "imported words"),
            "every segment came from the engine, over decoded audio"
        );
        assert!(!review.transcript.is_empty());
        assert!(
            review.gaps.is_empty(),
            "a file has no dropped packets to report"
        );
    }

    /// A transcript-only import has no audio, so the two passes that read audio
    /// are skipped and the meeting still arrives at review — with the vendor's
    /// speaker names as the attribution on its segments.
    #[test]
    fn an_imported_transcript_reaches_review_with_its_speakers() {
        let (files, manager) = importing_manager();
        let path = files.path().join("otter_export.txt");
        std::fs::write(&path, include_str!("fixtures/otter_export.txt")).unwrap();

        let snapshot = tauri::async_runtime::block_on(manager.import_transcript(path))
            .expect("the transcript imports");
        assert_eq!(snapshot.title, "Weekly product sync");

        let review = tauri::async_runtime::block_on(manager.get(snapshot.session_id))
            .expect("the imported meeting is readable");
        assert_eq!(review.session.phase, MeetingPhase::ReviewReady);
        assert_eq!(review.tracks.len(), 1);
        assert_eq!(
            review.tracks[0].durable_record_count, 0,
            "a transcript import writes no audio"
        );
        assert_eq!(
            review
                .transcript
                .iter()
                .map(|segment| segment.base.text.as_str())
                .collect::<Vec<_>>(),
            vec![
                "Let's start with the pricing page. The new tiers are live behind a flag.",
                "I can flip the flag on Thursday once the copy review is done.",
                "Thursday works. I'll write the changelog entry today.",
            ]
        );
        let mut speakers = review
            .speakers
            .iter()
            .map(|speaker| speaker.display_name.as_str())
            .collect::<Vec<_>>();
        speakers.sort_unstable();
        assert_eq!(speakers, vec!["Priya Raman", "Tom Alvarez"]);
        // The same speaker in two turns is one speaker, and the review screen's
        // rename and merge act on it exactly as they do on a diarized one.
        assert_eq!(
            review.transcript[0].assigned_speaker_id,
            review.transcript[2].assigned_speaker_id
        );
        assert_ne!(
            review.transcript[0].assigned_speaker_id,
            review.transcript[1].assigned_speaker_id
        );
    }

    /// Refusal is the whole contract for bad input: a typed error, and nothing
    /// left in the store. A session that kept its phase would be offered by
    /// every later launch's recovery pass and fail there forever.
    #[test]
    fn unreadable_input_is_refused_and_leaves_no_meeting_behind() {
        let (files, manager) = importing_manager();

        let prose = files.path().join("notes.txt");
        std::fs::write(&prose, "Some notes I typed after the call.\n").unwrap();
        assert_eq!(
            tauri::async_runtime::block_on(manager.import_transcript(prose)),
            Err(MeetingCommandError::ImportUnreadable)
        );

        let unsupported = files.path().join("deck.key");
        std::fs::write(&unsupported, b"not audio").unwrap();
        assert_eq!(
            tauri::async_runtime::block_on(manager.import_recording(ImportRecordingRequest {
                path: unsupported,
                title: None,
                recorded_at_utc_ms: None,
                origin: RecordingOrigin::LocalFile,
            })),
            Err(MeetingCommandError::ImportUnreadable)
        );

        // A supported extension over bytes that are not audio: the session is
        // already open by the time the decode gives up, so this is the arm that
        // has to clean up after itself.
        let corrupt = files.path().join("call.wav");
        std::fs::write(&corrupt, b"RIFF____WAVEfmt not really a wav at all").unwrap();
        assert_eq!(
            tauri::async_runtime::block_on(manager.import_recording(ImportRecordingRequest {
                path: corrupt,
                title: None,
                recorded_at_utc_ms: None,
                origin: RecordingOrigin::LocalFile,
            })),
            Err(MeetingCommandError::ImportUnreadable)
        );

        assert!(retained_meetings(&manager).is_empty());
    }

    /// The recording's own moment, not the moment somebody got round to
    /// importing it: a meeting is filed under when it happened.
    #[test]
    fn an_import_without_a_stated_time_is_filed_under_the_files_own() {
        let (files, manager) = importing_manager();
        let path = files.path().join("call.wav");
        write_mono_wav(&path, 8_000);
        let modified = file_modified_utc_ms(&path).expect("the fixture has an mtime");

        let snapshot =
            tauri::async_runtime::block_on(manager.import_recording(ImportRecordingRequest {
                path,
                title: Some("  Board call  ".to_string()),
                recorded_at_utc_ms: None,
                origin: RecordingOrigin::PairedDevice {
                    device_id: "phone-1".to_string(),
                },
            }))
            .expect("the recording imports");

        assert_eq!(snapshot.title, "Board call");
        assert_eq!(snapshot.started_at_utc_ms, Some(modified));
    }

    /// The consent ledger answers "who agreed to this". For a recording the
    /// sync thread pulled off a paired device nobody on this Mac chose
    /// anything, so its row must name the device rather than claim a direct
    /// acknowledgement here. A file the operator picked is still Direct.
    #[test]
    fn a_pulled_recordings_consent_names_the_device_that_recorded_it() {
        let (files, manager) = importing_manager();
        let store = tauri::async_runtime::block_on(manager.store()).expect("the store mounts");
        let pulled = files.path().join("pulled.wav");
        write_mono_wav(&pulled, 8_000);
        let picked = files.path().join("picked.wav");
        write_mono_wav(&picked, 8_000);

        let snapshot =
            tauri::async_runtime::block_on(manager.import_recording(ImportRecordingRequest {
                path: pulled,
                title: None,
                recorded_at_utc_ms: None,
                origin: RecordingOrigin::PairedDevice {
                    device_id: "phone-1".to_string(),
                },
            }))
            .expect("the pulled recording imports");
        let consent = store
            .latest_consent_for_session(snapshot.session_id)
            .expect("consents are readable")
            .expect("the import wrote a consent row");
        assert_eq!(
            consent.provenance,
            MeetingConsentProvenance::PairedDevice {
                device_id: "phone-1".to_string()
            }
        );

        let snapshot =
            tauri::async_runtime::block_on(manager.import_recording(ImportRecordingRequest {
                path: picked,
                title: None,
                recorded_at_utc_ms: None,
                origin: RecordingOrigin::LocalFile,
            }))
            .expect("the picked recording imports");
        let consent = store
            .latest_consent_for_session(snapshot.session_id)
            .expect("consents are readable")
            .expect("the import wrote a consent row");
        assert_eq!(consent.provenance, MeetingConsentProvenance::Direct);
    }

    /* ------------------------------------- the recap while capture is running */

    /// An engine that recognizes every chunk it is handed as one fixed line,
    /// and that can be told the shared engine is busy with somebody's
    /// dictation. The call count is how a test sees a pass that should not
    /// have run at all.
    struct LineEngine {
        line: &'static str,
        busy: Arc<AtomicBool>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl super::super::processing::MeetingTranscriptEngine for LineEngine {
        fn selected_model_id(&self) -> Option<String> {
            Some("fake-asr".to_string())
        }

        fn plan_for(&self, _run_plan: &MeetingRunPlan) -> Option<crate::modes::AsrPlan> {
            Some(crate::modes::AsrPlan::from_settings(
                &crate::settings::AppSettings::default(),
            ))
        }

        fn engine_id(&self) -> &'static str {
            "fake-asr"
        }

        fn transcribe(
            &self,
            _plan: &crate::modes::AsrPlan,
            _samples: &[f32],
        ) -> Result<String, ProcessingFailure> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            Ok(self.line.to_string())
        }

        fn is_busy(&self) -> bool {
            self.busy.load(Ordering::Acquire)
        }
    }

    /// Voice activity by loudness. These tests have no app handle and so no
    /// bundled detector, and what they need from one is only that a frame of
    /// pushed audio counts as speech.
    struct LoudVad;

    impl super::super::processing::MeetingVad for LoudVad {
        fn is_voice(&mut self, frame: &[f32]) -> Result<bool, ProcessingFailure> {
            Ok(frame.iter().any(|sample| sample.abs() > 0.01))
        }
    }

    struct LoudVadFactory;

    impl super::super::processing::MeetingVadFactory for LoudVadFactory {
        fn open(
            &self,
            _source_kind: SourceKind,
        ) -> Result<Box<dyn super::super::processing::MeetingVad>, ProcessingFailure> {
            Ok(Box::new(LoudVad))
        }
    }

    /// A text engine that answers with one fixed document, for tests about
    /// which transcript a recap read rather than about what a model wrote.
    struct FixedGenerator {
        available: bool,
        output: String,
    }

    impl super::super::processing::MeetingTextGenerator for FixedGenerator {
        fn is_available(&self) -> bool {
            self.available
        }

        fn model_id(&self) -> &'static str {
            "fixed"
        }

        fn model_version(&self) -> &'static str {
            "fixed-v1"
        }

        fn max_input_bytes(&self) -> usize {
            usize::MAX
        }

        fn generate(
            &self,
            _system_prompt: &str,
            _evidence: &str,
            _max_tokens: i32,
            _shape: ReplyShape,
        ) -> Result<String, super::super::processing::MeetingTextGenerationError> {
            Ok(self.output.clone())
        }
    }

    const RECAP_BULLET: &str = "Pricing stayed open.";
    const RECAP_OUTPUT: &str = r#"{"bullets":["Pricing stayed open."]}"#;

    /// One microphone-only capture, running, with the doubles a recap needs.
    /// Microphone-only because one source is one lane to push audio into, and
    /// that is the whole difference this harness cares about.
    fn capturing_meeting(
        engine: Arc<dyn super::super::processing::MeetingTranscriptEngine>,
        generator: Arc<dyn super::super::processing::MeetingTextGenerator>,
    ) -> (
        TempDir,
        MeetingSessionManager,
        MeetingSessionId,
        Arc<Mutex<Option<PacketSink>>>,
    ) {
        let directory = TempDir::new().unwrap();
        let secrets = Arc::new(SecretManager::with_backend(Arc::new(
            MemorySecretBackend::new(),
        )));
        let lane = Arc::new(Mutex::new(None));
        let manager = MeetingSessionManager::with_parts(
            None,
            Some(directory.path().join("meetings")),
            secrets,
            Arc::new(FakeSources {
                starts: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                aborts: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                unavailable: Some(SourceKind::SystemAudio),
                lane: Arc::clone(&lane),
            }),
        );
        manager.processing.set_transcript_engine(engine);
        manager.processing.set_vad_factory(Arc::new(LoudVadFactory));
        manager.processing.set_text_generators(
            generator,
            Arc::new(FixedGenerator {
                available: false,
                output: String::new(),
            }),
        );
        let preflight = tauri::async_runtime::block_on(manager.create_preflight(
            MeetingPreflightCreateRequest {
                operation_id: MeetingOperationId::new(),
                expected_revision: 0,
                title: "Live recap".to_string(),
                origin: MeetingOrigin::Manual,
                suggestion_id: None,
                calendar_event_key: None,
                requested_sources: vec![SourceKind::Microphone],
                required_sources: vec![SourceKind::Microphone],
                accepted_known_missing_sources: Vec::new(),
                degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
                destination: ProcessingDestination::Local,
                remote_acknowledgement: None,
                microphone_device_uid: None,
                frozen_system_audio_application_bundle_ids: Vec::new(),
            },
        ))
        .unwrap();
        let started = tauri::async_runtime::block_on(manager.start(MeetingStartRequest {
            operation_id: MeetingOperationId::new(),
            session_id: preflight.snapshot.session_id,
            expected_revision: preflight.snapshot.revision,
            consent: MeetingConsentInput {
                policy_version: 1,
                microphone_acknowledged: true,
                system_audio_acknowledged: true,
                known_missing_sources_acknowledged: Vec::new(),
                degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
                destination: ProcessingDestination::Local,
                remote_acknowledgement: None,
            },
        }))
        .unwrap();
        assert_eq!(started.snapshot.phase, MeetingPhase::CapturingRecording);
        (directory, manager, started.snapshot.session_id, lane)
    }

    /// Push a second and a half of loud audio into the capture lane and wait
    /// for the ingest worker to commit it.
    ///
    /// The wait is the point: a provisional pass reads records that have
    /// landed on disk, never the ring, so a test that reads before the commit
    /// is testing a race rather than the feature.
    fn capture_audio(
        lane: &Arc<Mutex<Option<PacketSink>>>,
        manager: &MeetingSessionManager,
        session_id: MeetingSessionId,
    ) {
        const FRAMES: u32 = 4_800;
        const PACKETS: u64 = 15;
        let store = tauri::async_runtime::block_on(manager.store()).unwrap();
        let track_id = store.session_snapshot(session_id).unwrap().sources[0]
            .track_id
            .expect("a started microphone track");
        let samples = vec![0.4_f32; FRAMES as usize];
        {
            let mut guard = lane.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let lane = guard.as_mut().expect("the source was handed a lane");
            for sequence in 0..PACKETS {
                let offset_ns = i64::try_from(sequence).unwrap() * 100_000_000;
                assert_eq!(
                    lane.try_push_interleaved(
                        CapturedPacket {
                            track_id,
                            source_epoch: SourceEpoch::new(0),
                            format_epoch: 0,
                            sequence,
                            native_timestamp_value: Some(offset_ns),
                            native_timestamp_timescale: Some(1_000_000_000),
                            host_monotonic_anchor_ns: Some(u64::try_from(offset_ns).unwrap()),
                            sample_rate_hz: 48_000,
                            channels: 1,
                            frame_count: FRAMES,
                            discontinuity_flags: PacketDiscontinuityFlags::default(),
                        },
                        &samples,
                    ),
                    PacketPushResult::Accepted
                );
            }
        }
        for _ in 0..500 {
            if store
                .session_snapshot(session_id)
                .unwrap()
                .sources
                .iter()
                .any(|source| source.last_durable_offset_ns.is_some())
            {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("the ingest worker never committed a durable record");
    }

    fn line_engine(
        busy: &Arc<AtomicBool>,
        calls: &Arc<std::sync::atomic::AtomicUsize>,
    ) -> Arc<LineEngine> {
        Arc::new(LineEngine {
            line: "We left pricing open.",
            busy: Arc::clone(busy),
            calls: Arc::clone(calls),
        })
    }

    /// The recap a person presses for during the meeting, which used to be the
    /// app saying it would have one later. It reads words recognized during
    /// capture and says so.
    #[test]
    fn catch_up_during_capture_reads_the_provisional_transcript() {
        let busy = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (_directory, manager, session_id, lane) = capturing_meeting(
            line_engine(&busy, &calls),
            Arc::new(FixedGenerator {
                available: true,
                output: RECAP_OUTPUT.to_string(),
            }),
        );
        capture_audio(&lane, &manager, session_id);

        let recap = tauri::async_runtime::block_on(manager.catch_up(session_id)).unwrap();

        assert_eq!(recap.state, MeetingCatchUpState::Ready);
        assert_eq!(recap.bullets, vec![RECAP_BULLET.to_string()]);
        assert!(
            recap.provisional,
            "a recap of a running capture is provisional"
        );
        assert_eq!(recap.segment_count, 1);
        assert!(
            recap.through_offset_ns.is_some_and(|offset| offset > 0),
            "a provisional recap says how far into the meeting it read"
        );
        assert!(
            calls.load(Ordering::Acquire) >= 1,
            "the press recognizes the audio captured since the last pass"
        );
    }

    /// After the stop the stored revision is the meeting, and the provisional
    /// reading is gone. Nothing recognizes audio for a recap any more: the
    /// transcript it reads was written by the post-stop pass.
    #[test]
    fn catch_up_after_the_stop_reads_the_stored_transcript() {
        let busy = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (_directory, manager, session_id, lane) = capturing_meeting(
            line_engine(&busy, &calls),
            Arc::new(FixedGenerator {
                available: true,
                output: RECAP_OUTPUT.to_string(),
            }),
        );
        capture_audio(&lane, &manager, session_id);
        let revision = tauri::async_runtime::block_on(async {
            manager
                .store()
                .await
                .unwrap()
                .session_snapshot(session_id)
                .unwrap()
                .revision
        });
        tauri::async_runtime::block_on(manager.stop(
            MeetingMutationRequest {
                operation_id: MeetingOperationId::new(),
                session_id,
                expected_revision: revision,
            },
            MeetingStopCause::Operator(MeetingStopSurface::MeetingLive),
        ))
        .unwrap();
        let after_processing = calls.load(Ordering::Acquire);

        let recap = tauri::async_runtime::block_on(manager.catch_up(session_id)).unwrap();

        assert_eq!(recap.state, MeetingCatchUpState::Ready);
        assert_eq!(recap.bullets, vec![RECAP_BULLET.to_string()]);
        assert!(
            !recap.provisional,
            "a stopped meeting is recapped from its stored transcript"
        );
        assert_eq!(
            calls.load(Ordering::Acquire),
            after_processing,
            "a recap after the stop recognizes nothing: the transcript is already written"
        );
    }

    /// Who ended a capture has to survive in the receipt, because the receipt
    /// is the corpus's own answer to that question and the only one a reader
    /// gets days later.
    ///
    /// `stop` used to file every caller under [`OperationActor::User`]. Four
    /// unrelated surfaces issue this command — a press in the app, the tray,
    /// the auto-record card, and detection acting on a [`StopTrigger`] — and
    /// the receipt claimed a person for all of them, including the one nobody
    /// touched. That is not a missing record, it is a false one, and it is why
    /// a capture that ended by itself after eleven seconds went a day without
    /// an explanation.
    #[test]
    fn a_stop_receipt_says_whether_the_app_or_a_person_ended_the_capture() {
        let actor_for = |cause: MeetingStopCause| {
            let busy = Arc::new(AtomicBool::new(false));
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let (_directory, manager, session_id, _lane) = capturing_meeting(
                line_engine(&busy, &calls),
                Arc::new(FixedGenerator {
                    available: false,
                    output: String::new(),
                }),
            );
            let revision = tauri::async_runtime::block_on(async {
                manager
                    .store()
                    .await
                    .unwrap()
                    .session_snapshot(session_id)
                    .unwrap()
                    .revision
            });
            tauri::async_runtime::block_on(manager.stop(
                MeetingMutationRequest {
                    operation_id: MeetingOperationId::new(),
                    session_id,
                    expected_revision: revision,
                },
                cause,
            ))
            .unwrap()
            .receipt
            .actor
        };

        assert_eq!(
            actor_for(MeetingStopCause::Operator(MeetingStopSurface::MeetingLive)),
            OperationActor::User,
            "a press is still a person"
        );
        assert_eq!(
            actor_for(MeetingStopCause::Detection(
                crate::meeting::detection::machine::StopTrigger::SleepBoundary
            )),
            OperationActor::System,
            "an auto-stop nobody asked for must not be recorded as the user's"
        );
    }

    /// The two stop buttons in the app's own windows are different presses,
    /// and a stop that loses its phase or revision race renders as done on
    /// whichever one was pressed. Until the surface rode on the request, the
    /// log said `Operator` for both and said nothing at all for the press
    /// that changed nothing, so the one question a reader has afterwards -
    /// which button did nothing - had no answer anywhere.
    #[test]
    fn a_stop_names_the_surface_that_pressed_it() {
        assert_eq!(
            MeetingStopCause::Operator(MeetingStopSurface::MeetingLive).describe(),
            "the live meeting screen"
        );
        assert_eq!(
            MeetingStopCause::Operator(MeetingStopSurface::ConsentPanel).describe(),
            "the consent panel"
        );
        assert_eq!(MeetingStopCause::Tray.describe(), "the tray");
        assert_eq!(
            MeetingStopCause::RecordingCard.describe(),
            "the recording card"
        );
        assert_eq!(
            MeetingStopCause::Detection(
                crate::meeting::detection::machine::StopTrigger::SleepBoundary
            )
            .describe(),
            "detection on sleep_boundary"
        );
    }

    /// A press that arrives against a revision the meeting has already moved
    /// past stops nothing, and the pressing surface still renders done. The
    /// receipt is the only place that disagreement is visible, so it has to
    /// carry the refusal and the reason rather than an empty commit.
    #[test]
    fn a_stop_that_loses_the_revision_gate_is_refused_rather_than_committed() {
        let busy = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (_directory, manager, session_id, _lane) = capturing_meeting(
            line_engine(&busy, &calls),
            Arc::new(FixedGenerator {
                available: false,
                output: String::new(),
            }),
        );
        let revision = tauri::async_runtime::block_on(async {
            manager
                .store()
                .await
                .unwrap()
                .session_snapshot(session_id)
                .unwrap()
                .revision
        });

        let refused = tauri::async_runtime::block_on(manager.stop(
            MeetingMutationRequest {
                operation_id: MeetingOperationId::new(),
                session_id,
                expected_revision: revision + 1,
            },
            MeetingStopCause::Operator(MeetingStopSurface::ConsentPanel),
        ))
        .unwrap();

        assert_eq!(refused.receipt.result, OperationResult::Rejected);
        assert!(
            refused
                .receipt
                .reason_codes
                .contains(&MeetingReasonCode::StaleRevision),
            "the refusal has to name the gate that lost: {:?}",
            refused.receipt.reason_codes
        );
        assert_eq!(refused.snapshot.phase, MeetingPhase::CapturingRecording);
    }

    /// A Mac with no engine to write text says so, mid-meeting as anywhere
    /// else. The provisional transcript exists — the words were recognized —
    /// and there is still no recap, which is the honest answer.
    #[test]
    fn catch_up_during_capture_reports_a_missing_text_engine() {
        let busy = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (_directory, manager, session_id, lane) = capturing_meeting(
            line_engine(&busy, &calls),
            Arc::new(FixedGenerator {
                available: false,
                output: RECAP_OUTPUT.to_string(),
            }),
        );
        capture_audio(&lane, &manager, session_id);

        let recap = tauri::async_runtime::block_on(manager.catch_up(session_id)).unwrap();

        assert_eq!(recap.state, MeetingCatchUpState::ModelUnavailable);
        assert!(recap.bullets.is_empty(), "no engine writes no bullets");
        assert!(recap.provisional);
    }

    /// The dictation somebody is waiting on comes first. While the shared
    /// engine is busy no pass runs, and the recap is the one a meeting with
    /// nothing recognized yet gets — not a partial one, and not a queue behind
    /// the words a person is watching for.
    #[test]
    fn a_busy_engine_runs_no_provisional_pass() {
        let busy = Arc::new(AtomicBool::new(true));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (_directory, manager, session_id, lane) = capturing_meeting(
            line_engine(&busy, &calls),
            Arc::new(FixedGenerator {
                available: true,
                output: RECAP_OUTPUT.to_string(),
            }),
        );
        capture_audio(&lane, &manager, session_id);

        let while_busy = tauri::async_runtime::block_on(manager.catch_up(session_id)).unwrap();

        assert_eq!(while_busy.state, MeetingCatchUpState::NoTranscriptYet);
        assert_eq!(
            calls.load(Ordering::Acquire),
            0,
            "a busy engine is not asked to recognize a meeting's audio"
        );

        busy.store(false, Ordering::Release);
        let once_free = tauri::async_runtime::block_on(manager.catch_up(session_id)).unwrap();

        assert_eq!(once_free.state, MeetingCatchUpState::Ready);
        assert_eq!(
            calls.load(Ordering::Acquire),
            1,
            "the audio the skipped pass left behind is read by the next one"
        );
    }

    /// A meeting point two lanes can only clear together. A lane records that
    /// it arrived and then waits a bounded moment for its sibling; asked one
    /// after the other, the first waits out the whole window alone and says so.
    #[derive(Default)]
    struct Rendezvous {
        arrived: Mutex<usize>,
        sibling: std::sync::Condvar,
        alone: std::sync::atomic::AtomicUsize,
    }

    impl Rendezvous {
        fn arrive(&self) {
            let mut arrived = self
                .arrived
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *arrived += 1;
            if *arrived >= 2 {
                self.sibling.notify_all();
                return;
            }
            let (_guard, wait) = self
                .sibling
                .wait_timeout_while(arrived, Duration::from_millis(750), |arrived| *arrived < 2)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if wait.timed_out() {
                self.alone.fetch_add(1, Ordering::AcqRel);
            }
        }
    }

    struct RendezvousSource {
        kind: SourceKind,
        rendezvous: Arc<Rendezvous>,
    }

    impl MeetingCaptureSource for RendezvousSource {
        fn probe(&self) -> SourceProbe {
            SourceProbe {
                source_kind: self.kind,
                availability: SourceAvailability::Available,
                health: SourceHealth::Healthy,
                detail: None,
                negotiated_format: Some(AudioFormat {
                    sample_rate_hz: 48_000,
                    channels: 1,
                }),
            }
        }

        fn start(
            &mut self,
            plan: SourceStartPlan,
            _anchor: SessionClockAnchor,
            _sink: PacketSink,
        ) -> Result<SourceStartReport, MeetingCaptureError> {
            self.rendezvous.arrive();
            Ok(SourceStartReport {
                track_id: plan.track_id,
                source_kind: plan.source_kind,
                format: AudioFormat {
                    sample_rate_hz: 48_000,
                    channels: 1,
                },
                epoch: SourceEpoch::new(0),
                format_epoch: 0,
                timestamp_bridge: TimestampBridge {
                    native_anchor_value: 0,
                    native_timescale: 1_000_000_000,
                    host_monotonic_anchor_ns: 0,
                    session_offset_ns: 0,
                },
            })
        }

        fn pause(&mut self) -> Result<(), MeetingCaptureError> {
            Ok(())
        }

        fn resume(
            &mut self,
            _epoch: SourceEpoch,
        ) -> Result<SourceStartReport, MeetingCaptureError> {
            Err(MeetingCaptureError::InvalidState)
        }

        fn stop(&mut self) -> Result<SourceStopReport, MeetingCaptureError> {
            Ok(SourceStopReport {
                track_id: SourceTrackId::new(),
                final_offset_ns: Some(0),
                health: SourceHealth::Healthy,
                observed_gaps: Vec::new(),
            })
        }

        fn abort(&mut self) -> Result<(), MeetingCaptureError> {
            Ok(())
        }
    }

    struct RendezvousSources {
        rendezvous: Arc<Rendezvous>,
    }

    impl MeetingSourceProvider for RendezvousSources {
        fn probe(&self, source_kind: SourceKind) -> SourceProbe {
            RendezvousSource {
                kind: source_kind,
                rendezvous: Arc::clone(&self.rendezvous),
            }
            .probe()
        }

        fn acquire(
            &self,
            source_kind: SourceKind,
        ) -> Result<Box<dyn MeetingCaptureSource>, MeetingCaptureError> {
            Ok(Box::new(RendezvousSource {
                kind: source_kind,
                rendezvous: Arc::clone(&self.rendezvous),
            }))
        }
    }

    /// ScreenCaptureKit's start blocks on `SCShareableContent` and
    /// `startCapture`, so asking the sources one after the other put that whole
    /// wait into the far end of the meeting: the owner's captures show system
    /// audio starting 196 to 1,566 ms after the microphone.
    #[test]
    fn both_lanes_are_asked_to_start_at_once() {
        let rendezvous = Arc::new(Rendezvous::default());
        let (_directory, manager) = manager_with(Arc::new(RendezvousSources {
            rendezvous: Arc::clone(&rendezvous),
        }));
        let preflight = tauri::async_runtime::block_on(manager.create_preflight(
            MeetingPreflightCreateRequest {
                operation_id: MeetingOperationId::new(),
                expected_revision: 0,
                title: "Design sync".to_string(),
                origin: MeetingOrigin::Manual,
                suggestion_id: None,
                calendar_event_key: None,
                requested_sources: SourceKind::ALL.to_vec(),
                required_sources: SourceKind::ALL.to_vec(),
                accepted_known_missing_sources: Vec::new(),
                degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
                destination: ProcessingDestination::Local,
                remote_acknowledgement: None,
                microphone_device_uid: None,
                frozen_system_audio_application_bundle_ids: Vec::new(),
            },
        ))
        .unwrap();

        let started = tauri::async_runtime::block_on(manager.start(MeetingStartRequest {
            operation_id: MeetingOperationId::new(),
            session_id: preflight.snapshot.session_id,
            expected_revision: preflight.snapshot.revision,
            consent: MeetingConsentInput {
                policy_version: 1,
                microphone_acknowledged: true,
                system_audio_acknowledged: true,
                known_missing_sources_acknowledged: Vec::new(),
                degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
                destination: ProcessingDestination::Local,
                remote_acknowledgement: None,
            },
        }))
        .unwrap();

        assert_eq!(
            rendezvous.alone.load(Ordering::Acquire),
            0,
            "a lane asked to start after its sibling waits alone for it"
        );
        assert_eq!(
            started
                .snapshot
                .sources
                .iter()
                .filter(|source| source.track_id.is_some())
                .count(),
            SourceKind::ALL.len(),
            "both lanes still have a track after starting together"
        );
    }

    /// The ring is how much audio a lane holds before its writer must have
    /// drained it, and 96,000 samples is two seconds of one channel but one of
    /// two, so the fixed count silently halved the stereo lane's slack.
    #[test]
    fn a_lane_holds_the_same_seconds_of_audio_in_any_format() {
        for (channels, samples_per_second) in [(1_u16, 48_000_u64), (2, 96_000)] {
            assert_eq!(
                lane_sample_capacity(
                    96_000,
                    Some(AudioFormat {
                        sample_rate_hz: 48_000,
                        channels,
                    })
                ),
                samples_per_second * u64::from(SOURCE_LANE_TARGET_SECONDS)
            );
        }
    }

    #[test]
    fn a_lane_that_announced_no_format_keeps_the_plan_capacity() {
        assert_eq!(lane_sample_capacity(96_000, None), 96_000);
        assert_eq!(
            lane_sample_capacity(
                96_000,
                Some(AudioFormat {
                    sample_rate_hz: 8_000,
                    channels: 1,
                })
            ),
            96_000,
            "the plan's count is a floor, not a target"
        );
    }
}

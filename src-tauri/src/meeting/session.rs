use super::analytics::{
    merge_turns, MeetingActionItemState, MeetingAnalyticsSnapshot, MeetingCatchUp, MeetingLiveHelp,
    MeetingLiveHelpKind, MeetingNotesTemplate, MeetingProvisionalTranscript, MeetingUserNotes,
};
use super::call_silence::{CaptureAudibility, TrackMeter};
use super::capture::{
    packet_start_offset_ns, DroppedRun, MeetingCaptureSource, PacketLaneReadError,
    PacketLaneReader, PacketSink,
};
use super::clock::host_monotonic_now_ns;
use super::detection::calendar::CalendarAtStart;
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
use super::local_generator::{MeetingLocalEngineStatus, MeetingTextEngineChoice};
use super::loop_types::{
    MeetingLoopAssignRequest, MeetingLoopMutationResult, MeetingLoopReopenRequest,
    MeetingLoopResolveRequest, MeetingLoopsResult,
};
use super::people_types::{
    PersonId, PersonLinkConfidence, PersonSummaryOutcome, PersonSummaryRegenerateResult,
};
use super::processing::{
    write_relationship_summary, CaptureGate, FollowOns, LiveTranscript, LiveTranscriptWorker,
    MeetingProcessingService, MeetingTextGenerationError, ProcessingOrigin, ReplyShape, RunFailure,
    LIVE_HELP_QUESTION_MAX_CHARS, LIVE_RECALL_WINDOW_NS,
};
use super::store::voice_identity::names_user;
use super::store::{
    CalendarEventSource, InterruptedRecovery, MeetingStore, MeetingTrackWriter, RecoveredMeeting,
    SegmentEdit, SpeakerRename, StoreError, StoreMutation, StoreTransition, TrackCreation,
    TranscriptRevisionInput, TranscriptSegmentInput, STORE_SCHEMA_VERSION,
};
use super::suggestions::{
    MeetingSuggestion, MeetingSuggestionService, MeetingSuggestionSignal, MeetingSuggestionSink,
};
use super::template_types::{custom_template_id_from_artifact, NotesTemplateChoice};
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
/// stereo lane half the slack. The writer shares the meeting database and the
/// disk with the rest of the app, and a busy Mac held it up for longer than
/// the lane could hold: a half-hour call lost eight seconds of audio that way.
/// A minute is 11 MB of 48 kHz mono and 23 MB of stereo.
const SOURCE_LANE_TARGET_SECONDS: u32 = 60;
/// The smallest packet a lane plans its descriptors for. Each packet takes
/// one, so 128 descriptors held 1.3 seconds of 480-frame microphone packets
/// whatever the samples could hold.
const SMALLEST_PLANNED_PACKET_FRAMES: u64 = 128;
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

/// Enough descriptors for the lane's seconds at the smallest planned packet,
/// with the plan's count as the floor.
fn lane_descriptor_capacity(floor: u32, format: Option<AudioFormat>) -> u64 {
    format
        .and_then(|format| {
            u64::from(format.sample_rate_hz).checked_mul(u64::from(SOURCE_LANE_TARGET_SECONDS))
        })
        .map_or(0, |frames| frames / SMALLEST_PLANNED_PACKET_FRAMES)
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
    /// The stored result of this meeting's one recording notice.
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

/// Several lines out of the transcript, or back in, as one edit: the tail of
/// a recording the person cut, or put back.
#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct MeetingSegmentsRemovedRequest {
    pub operation_id: MeetingOperationId,
    pub session_id: MeetingSessionId,
    pub expected_revision: u64,
    pub segment_ids: Vec<TranscriptSegmentId>,
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
    #[serde(default)]
    pub custom_template_id: Option<MeetingTemplateId>,
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
    #[serde(default)]
    pub custom_template_id: Option<MeetingTemplateId>,
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
    /// The confirmation Sona asks for when it quits during a capture.
    Quit,
}

impl MeetingStopSurface {
    const fn describe(self) -> &'static str {
        match self {
            Self::MeetingLive => "the live meeting screen",
            Self::ConsentPanel => "the consent panel",
            Self::Quit => "quitting Sona",
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
    /// What the mount's recovery sweep moved into recovery, held until
    /// startup takes it for the automatic reprocess pass.
    recovered_at_mount: Mutex<Vec<RecoveredMeeting>>,
    /// Whether mounting the store sweeps what an earlier launch left. Only the
    /// app that records may: a command-line read runs beside a live meeting,
    /// and its sweep would take that meeting for an interrupted one.
    recovers_on_mount: bool,
    /// UUID namespace for this launch's automatic recovery attempts. Drawn
    /// once per launch so an attempt's operation id is stable inside the
    /// launch — a second pass is deduplicated by the receipt the first one
    /// wrote — and different in the next one, which is what lets a meeting
    /// that failed today be tried again tomorrow.
    recovery_launch: Uuid,
    processing: Arc<MeetingProcessingService>,
    capture_active: Arc<AtomicBool>,
    actor: Mutex<ActorState>,
}

struct ActorState {
    active: Option<ActiveCapture>,
    tray_session_id: Option<MeetingSessionId>,
}

struct CaptureLease {
    active: Arc<AtomicBool>,
}

impl CaptureLease {
    fn acquire(active: &Arc<AtomicBool>) -> Result<Self, MeetingCommandError> {
        active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| MeetingCommandError::CaptureLeaseBusy)?;
        Ok(Self {
            active: Arc::clone(active),
        })
    }
}

impl Drop for CaptureLease {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
    }
}

/// Everything a capture holds beyond its lanes: the Mac kept awake,
/// processing parked, and the lease. It belongs to the capture and goes with
/// it, so no other meeting's command can let go of any part of it.
struct CaptureHold {
    keep_awake: MeetingKeepAwake,
    // Before the lease, so the next capture's park cannot be lifted by this one.
    _processing: CaptureGate,
    _lease: CaptureLease,
}

impl CaptureHold {
    fn new(lease: CaptureLease, processing: &MeetingProcessingService) -> Self {
        let mut keep_awake = MeetingKeepAwake::new();
        keep_awake.acquire();
        Self {
            keep_awake,
            _processing: processing.park_for_capture(),
            _lease: lease,
        }
    }
}

struct ActiveCapture {
    session_id: MeetingSessionId,
    sources: HashMap<SourceKind, ActiveSource>,
    /// The provisional transcript of this capture and the pass that fills it.
    /// It stops when this record is dropped, which is what every path that
    /// ends a capture — stop, discard, delete — already does.
    live: LiveTranscriptWorker,
    call_names: Option<super::call_speakers::CallNameSampler>,
    /// The level of each track, for the warning that the call went silent.
    audibility: Arc<CaptureAudibility>,
    // Released last so idle cannot precede source and live-worker teardown.
    hold: CaptureHold,
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
            recovered_at_mount: Mutex::new(Vec::new()),
            recovers_on_mount: true,
            recovery_launch: Uuid::new_v4(),
            processing,
            capture_active: Arc::new(AtomicBool::new(false)),
            actor: Mutex::new(ActorState {
                active: None,
                tray_session_id: None,
            }),
        }
    }

    /// A manager for a process that only reads, beside an app that may be
    /// recording: its mount leaves every meeting where it is.
    pub fn reading_only(mut self) -> Self {
        self.recovers_on_mount = false;
        self
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
    pub(crate) fn processing(&self) -> &MeetingProcessingService {
        &self.processing
    }

    pub(crate) fn meeting_local_engine_status(&self) -> MeetingLocalEngineStatus {
        self.processing.meeting_local_engine_status()
    }
    pub(crate) fn text_engine_for_next_meeting(&self) -> MeetingTextEngineChoice {
        self.processing.text_engine_for_next_meeting()
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

    /// The recovery sweep runs inside the mount, where it sees only what an
    /// earlier launch left. This mounts the store if nothing has yet, sweeps
    /// retention, and hands the meetings the sweep recovered to the automatic
    /// reprocess pass, once.
    pub(crate) async fn recover_at_startup_at(
        &self,
        now_utc_ms: i64,
    ) -> Result<Vec<RecoveredMeeting>, MeetingCommandError> {
        self.store().await?;
        if let Err(error) = self.sweep_retention_at(now_utc_ms).await {
            log::warn!("Meeting retention sweep is unavailable at startup: {error:?}");
        }
        Ok(std::mem::take(&mut *self.recovered_at_mount_lock()))
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
            "Meeting recovery sweep: {} interrupted, {} finished, {} status healed, {} abandoned start gates discarded, {} unfinished imports discarded",
            recovery.recovered.len(),
            recovery.finished.len(),
            recovery.status_resolved.len(),
            recovery.discarded.len(),
            recovery.abandoned_imports.len(),
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
        for session_id in &recovery.finished {
            log::info!(
                "Meeting recovery: {session_id:?} had its notes written by an earlier launch that ended before review, now in review",
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
        for session_id in &recovery.abandoned_imports {
            log::info!(
                "Meeting recovery: {session_id:?} was an import an earlier launch left partway through its file, discarded",
            );
            self.emit_removed(*session_id, 0);
        }
        for session_id in recovery
            .recovered
            .iter()
            .map(|meeting| meeting.session_id)
            .chain(recovery.status_resolved.iter().copied())
            .chain(recovery.finished.iter().copied())
        {
            let snapshot = store
                .session_snapshot(session_id)
                .map_err(map_store_error)?;
            self.emit_session_changed(&snapshot);
        }
        Ok(())
    }

    /// The steps that follow a run's notes, for each meeting the sweep moved
    /// on to review because an earlier launch wrote its notes and ended
    /// before the move. A meeting the automatic reprocess would have run
    /// again gets every step that run would have fired; one it would have
    /// withheld gets only those that stay inside Sona.
    fn finish_saved_runs(&self, store: &Arc<MeetingStore>, finished: &[MeetingSessionId]) {
        for &session_id in finished {
            let meeting = RecoveredMeeting {
                session_id,
                prior_phase: MeetingPhase::Processing,
            };
            let follow_ons = if self.reprocess_withheld(store, &meeting).is_none() {
                FollowOns::All
            } else {
                FollowOns::InsideSona
            };
            match store.session_snapshot(session_id) {
                Ok(snapshot) => {
                    self.processing
                        .notes_ready(Arc::clone(store), &snapshot, follow_ons);
                }
                Err(error) => log::warn!(
                    "Meeting recovery could not read {session_id:?} to finish it: {error:?}"
                ),
            }
        }
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
        let due_transcripts = store
            .due_transcript_retention_sessions(now_utc_ms)
            .map_err(map_store_error)?;
        let mut purged = 0;
        let mut failed = 0;
        for session_id in due_transcripts {
            let before = store
                .transcript_correction_source_snapshot(session_id)
                .inspect_err(|error| log::warn!("Correction source snapshot failed: {error:?}"))
                .ok();
            match store.purge_transcript_at(session_id, now_utc_ms) {
                Ok(Some(change)) => {
                    purged += 1;
                    if let Some(revision) = change.people_revision {
                        self.emit_artifact_changed(Some(session_id), revision);
                    }
                }
                Ok(None) => continue,
                Err(_) => failed += 1,
            }
            if let Some(before) = before {
                match store.transcript_correction_projection_changes(before) {
                    Ok(changed) => self.emit_transcript_correction_changes(changed),
                    Err(error) => log::warn!("Correction source publication failed: {error:?}"),
                }
            }
            // The transaction may have committed before removing audio failed.
            // Readers must see the deletion even while its bytes await cleanup.
            if let Ok(snapshot) = store.session_snapshot(session_id) {
                self.emit_session_changed(&snapshot);
            }
        }
        if purged != 0 || failed != 0 {
            log::info!("Meeting transcript retention: {purged} purged, {failed} awaiting cleanup");
        }
        let due_audio = store
            .due_audio_retention_sessions(now_utc_ms)
            .map_err(map_store_error)?;
        let mut audio_purged = 0;
        let mut audio_failed = 0;
        for session_id in due_audio {
            match store.purge_audio_at(session_id, now_utc_ms) {
                Ok(true) => audio_purged += 1,
                Ok(false) => continue,
                Err(_) => audio_failed += 1,
            }
            if let Ok(snapshot) = store.session_snapshot(session_id) {
                self.emit_session_changed(&snapshot);
            }
        }
        if audio_purged != 0 || audio_failed != 0 {
            log::info!(
                "Meeting audio retention: {audio_purged} purged, {audio_failed} awaiting cleanup"
            );
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

    /// A start made by hand that names no event. During the one meeting
    /// under way (which the caller reads from the calendar) it records that
    /// meeting under its event: the preflight carries it, and the "Local
    /// notes" default takes its title. The event is remembered as found under
    /// way, so it starts no automatic send. Without one, the events near the
    /// start wait on the recording as maybes, which change nothing until the
    /// operator takes one.
    pub async fn create_manual_preflight(
        &self,
        mut request: MeetingPreflightCreateRequest,
        calendar: CalendarAtStart,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        let CalendarAtStart { under_way, near } = calendar;
        if let Some(event) = under_way.as_ref() {
            request.calendar_event_key = Some(event.event_key.clone());
            let title = event.title.trim();
            if request.title == MANUAL_DEFAULT_TITLE && !title.is_empty() {
                request.title = title.to_string();
            }
        }
        let result = self
            .create_preflight_for_event(request, under_way, CalendarEventSource::FoundUnderWay)
            .await?;
        if !near.is_empty() {
            let store = self.store().await?;
            if let Err(error) =
                store.suggest_start_events(result.snapshot.session_id, &near, utc_now_ms())
            {
                log::warn!("A start by hand could not offer the events near it: {error:?}");
            }
        }
        Ok(result)
    }

    pub async fn create_preflight_with_calendar(
        &self,
        request: MeetingPreflightCreateRequest,
        calendar_event: Option<CalendarEventSummary>,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        self.create_preflight_for_event(request, calendar_event, CalendarEventSource::Chosen)
            .await
    }

    async fn create_preflight_for_event(
        &self,
        request: MeetingPreflightCreateRequest,
        calendar_event: Option<CalendarEventSummary>,
        source: CalendarEventSource,
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
                    .remember_calendar_event(session_id, event, source)
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
                    actor: OperationActor::User,
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
                .remember_calendar_event(session_id, event, source)
                .map_err(map_store_error)?;
        }
        let result = self.result_for_receipt(store, receipt, session_id)?;
        self.emit_session_changed(&result.snapshot);
        Ok(result)
    }

    pub async fn create_manual_preflight_from_tray(
        &self,
        calendar: CalendarAtStart,
    ) -> Result<MeetingSessionSnapshot, MeetingCommandError> {
        let result = self
            .create_manual_preflight(
                MeetingPreflightCreateRequest {
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
                },
                calendar,
            )
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
            .start_with_provenance(
                MeetingStartRequest {
                    operation_id: MeetingOperationId::new(),
                    session_id: preflight.snapshot.session_id,
                    expected_revision: preflight.snapshot.revision,
                    consent: request.consent,
                },
                MeetingConsentProvenance::Direct,
            )
            .await;
        let result = self.finish_detection_start(&preflight.snapshot, start)?;
        if result.snapshot.phase == MeetingPhase::CapturingRecording {
            self.arm_disclosure(
                result.snapshot.session_id,
                context.calendar_event.as_ref(),
                composer_app_for(context.trigger_bundle_id.as_deref()),
                request.announce_in_chat,
                true,
            )
            .await;
        }
        Ok(result)
    }

    /// Freeze the notice at start and try once. Failure to post never stops
    /// recording, but remains visible with the exact text to copy.
    async fn arm_disclosure(
        &self,
        session_id: MeetingSessionId,
        calendar_event: Option<&CalendarEventSummary>,
        composer_app: Option<String>,
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
        let settings = self.app.as_ref().map(crate::settings::get_settings);
        let enabled = announce_in_chat
            && settings
                .as_ref()
                .is_some_and(|s| s.meeting_disclosure_enabled);
        let line = settings
            .map(|s| s.meeting_disclosure_message)
            .unwrap_or_else(crate::settings::default_meeting_disclosure_message);
        if let Err(error) =
            store.request_session_disclosure(session_id, &line, composer_app.as_deref(), enabled)
        {
            log::warn!("Meeting {session_id:?} could not arm its disclosure: {error:?}");
            return;
        }
        if let Err(error) = self.announce_disclosure(session_id, line).await {
            log::warn!("Meeting {session_id:?} could not record its disclosure outcome: {error:?}");
        }
        if let Ok(snapshot) = store.session_snapshot(session_id) {
            self.emit_session_changed(&snapshot);
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
                context.calendar_event.as_ref(),
                composer_app_for(context.trigger_bundle_id.as_deref()),
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
        let result = self.finish_detection_start(&preflight.snapshot, start)?;
        if result.snapshot.phase == MeetingPhase::CapturingRecording {
            self.arm_disclosure(
                result.snapshot.session_id,
                context.calendar_event.as_ref(),
                composer_app_for(context.trigger_bundle_id.as_deref()),
                true,
                false,
            )
            .await;
        }
        Ok(result)
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
                    frozen_system_audio_application_bundle_ids:
                        super::detection::apps::system_audio_route(
                            context.trigger_bundle_id.as_deref(),
                        ),
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

    /// Only the notice frozen by this meeting's consent can be attempted.
    /// The store commits a claim before delivery; a second caller cannot send.
    pub async fn announce_disclosure(
        &self,
        session_id: MeetingSessionId,
        line: String,
    ) -> Result<MeetingSessionDisclosure, MeetingCommandError> {
        let store = self.store().await?;
        let enabled = self
            .app
            .as_ref()
            .is_some_and(|app| crate::settings::get_settings(app).meeting_disclosure_enabled);
        let actor = self.actor_lock();
        let recording = actor
            .active
            .as_ref()
            .is_some_and(|active| active.session_id == session_id)
            && store
                .session_snapshot(session_id)
                .map_err(map_store_error)?
                .phase
                == MeetingPhase::CapturingRecording;
        store.attempt_session_disclosure(session_id, |frozen, composer_app| {
            if !recording || !enabled {
                return crate::delivery::CallChatResult::not_posted(
                    "Automatic posting is off or this meeting is no longer recording.",
                );
            }
            if frozen != line {
                return crate::delivery::CallChatResult::not_posted(
                    "The notice changed after recording started. Copy the saved notice to send it yourself.",
                );
            }
            crate::delivery::announce(frozen, composer_app)
        }).map_err(map_store_error)
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
        let composer_app = composer_app_for(None);
        let result = self
            .start_with_provenance(request, MeetingConsentProvenance::Direct)
            .await?;
        if result.snapshot.phase == MeetingPhase::CapturingRecording {
            let store = self.store().await?;
            let event = store
                .meeting_calendar_facts(result.snapshot.session_id)
                .map_err(map_store_error)?;
            self.arm_disclosure(
                result.snapshot.session_id,
                event.as_ref(),
                composer_app,
                true,
                false,
            )
            .await;
        }
        Ok(result)
    }

    async fn start_with_provenance(
        &self,
        request: MeetingStartRequest,
        provenance: MeetingConsentProvenance,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        let _activity = crate::planning::quiet::foreground_activity();
        let store = self.store().await?;
        if let Some(receipt) = store
            .operation_receipt(request.operation_id)
            .map_err(map_store_error)?
        {
            return self.result_for_receipt(store, receipt, request.session_id);
        }
        let mut actor = self.actor_lock();
        let capture_lease = CaptureLease::acquire(&self.capture_active)?;
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
        let hold = CaptureHold::new(capture_lease, &self.processing);
        let audibility = CaptureAudibility::new();
        let mut sources = HashMap::new();
        let began =
            self.begin_capture(&store, request.session_id, &plan, &audibility, &mut sources);
        if let Ok(true) = began {
            actor.active = Some(ActiveCapture {
                session_id: request.session_id,
                sources,
                call_names: None,
                audibility,
                live: LiveTranscriptWorker::start(
                    Arc::clone(&self.processing),
                    Arc::clone(&store),
                    request.session_id,
                ),
                hold,
            });
            actor.tray_session_id = None;
            drop(actor);
            let snapshot = store
                .session_snapshot(request.session_id)
                .map_err(map_store_error)?;
            self.emit_session_changed(&snapshot);
            self.record_meeting_started(Arc::clone(&store), request.session_id);
            return Ok(MeetingMutationResult { receipt, snapshot });
        }
        for source in sources.values_mut() {
            let _ = source.source.abort();
            let _ = source.worker.stop();
        }
        let (event_kind, reason_codes) = match &began {
            Ok(_) => (
                "source_start_incomplete",
                vec![MeetingReasonCode::SourceStartFailed],
            ),
            Err(_) => {
                if let Err(error) =
                    store.close_open_capture_window(request.session_id, 0, "start_failed")
                {
                    log::warn!(
                        "Meeting capture {} kept the capture window its failed start opened: {error:?}",
                        request.session_id.uuid()
                    );
                }
                ("start_failed", Vec::new())
            }
        };
        let restored = system_transition(
            &store,
            request.session_id,
            MeetingCommandKind::Start,
            MeetingPhase::Starting,
            MeetingPhase::Preflight,
            event_kind,
            reason_codes,
        );
        drop(hold);
        drop(actor);
        if let Err(error) = began {
            if let Err(restore) = restored {
                log::warn!(
                    "Meeting capture {} failed to start and stayed at starting: {restore:?}",
                    request.session_id.uuid()
                );
            }
            self.emit_current(&store, request.session_id);
            return Err(error);
        }
        restored.map_err(map_store_error)?;
        let snapshot = store
            .session_snapshot(request.session_id)
            .map_err(map_store_error)?;
        self.emit_session_changed(&snapshot);
        Ok(MeetingMutationResult { receipt, snapshot })
    }

    /// Open the lanes of a start that has committed `starting`, write down
    /// what opened, and begin recording. `Ok(true)` is a recording with every
    /// lane in `sources`; `Ok(false)` is a start the degraded-start policy
    /// refuses. Whatever the answer, every lane that opened is in `sources` or
    /// already aborted.
    fn begin_capture(
        &self,
        store: &Arc<MeetingStore>,
        session_id: MeetingSessionId,
        plan: &MeetingRunPlan,
        audibility: &Arc<CaptureAudibility>,
        sources: &mut HashMap<SourceKind, ActiveSource>,
    ) -> Result<bool, MeetingCommandError> {
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
                usize::try_from(lane_descriptor_capacity(
                    plan.storage.source_lane_descriptor_capacity,
                    probe.negotiated_format,
                ))
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
                            session_id,
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
        let mut started = started.into_iter();
        while let Some((source_kind, track_id, mut source, lane_reader, report)) = started.next() {
            let writer = store
                .create_track(TrackCreation {
                    session_id,
                    plan_id: plan.plan_id,
                    source_kind,
                    required: plan.required_sources.contains(&source_kind),
                    requested: true,
                    descriptor_json: "{}",
                    report,
                })
                .and_then(|()| store.open_track_writer(session_id, track_id, plan.storage.clone()));
            let writer = match writer {
                Ok(writer) => writer,
                Err(error) => {
                    let _ = source.abort();
                    for (_, _, mut unregistered, _, _) in started {
                        let _ = unregistered.abort();
                    }
                    return Err(map_store_error(error));
                }
            };
            let worker = TrackWorker::start(
                Arc::clone(store),
                writer,
                lane_reader,
                report,
                audibility.meter(source_kind),
            );
            sources.insert(
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
            .any(|source_kind| !sources.contains_key(source_kind));
        if sources.is_empty()
            || (missing_required_source
                && plan.degraded_start_policy == DegradedStartPolicy::AbortIfRequiredSourceFails)
        {
            return Ok(false);
        }
        store
            .open_capture_window(session_id, 0)
            .map_err(map_store_error)?;
        system_transition(
            store,
            session_id,
            MeetingCommandKind::Start,
            MeetingPhase::Starting,
            MeetingPhase::CapturingRecording,
            "capture_started",
            Vec::new(),
        )
        .map_err(map_store_error)?;
        Ok(true)
    }

    pub async fn call_name_targets(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<super::call_speakers::CallNameTargets, MeetingCommandError> {
        let store = self.store().await?;
        let plan = store.processing_plan(session_id).map_err(map_store_error)?;
        Ok(super::call_speakers::targets(&plan))
    }

    pub async fn call_name_status(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<super::call_speakers::CallNameStatus, MeetingCommandError> {
        self.store()
            .await?
            .call_name_status(session_id)
            .map_err(map_store_error)
    }

    pub async fn call_names_set(
        &self,
        session_id: MeetingSessionId,
        target_id: Option<String>,
        automatically_use: bool,
    ) -> Result<super::call_speakers::CallNameStatus, MeetingCommandError> {
        let store = self.store().await?;
        let mut actor = self.actor_lock();
        let active = actor
            .active
            .as_mut()
            .filter(|active| active.session_id == session_id)
            .ok_or(MeetingCommandError::NotFound)?;
        let previous = store
            .call_name_status(session_id)
            .map_err(map_store_error)?;
        // Changing automatic use does not interrupt the selected AX identity.
        if previous.enabled
            && active.call_names.is_some()
            && target_id.is_some()
            && previous.target.as_ref().map(|target| &target.id) == target_id.as_ref()
        {
            store
                .set_call_names_automatic(session_id, automatically_use)
                .map_err(map_store_error)?;
        } else if let Some(target_id) = target_id {
            let plan = store.processing_plan(session_id).map_err(map_store_error)?;
            let target = super::call_speakers::selected_target(&plan, &target_id)
                .ok_or(MeetingCommandError::InvalidRequest)?;
            drop(active.call_names.take());
            active.call_names = Some(
                super::call_speakers::CallNameSampler::start(
                    Arc::clone(&store),
                    plan,
                    target,
                    automatically_use,
                )
                .map_err(map_store_error)?,
            );
        } else {
            drop(active.call_names.take());
            store
                .finish_call_names(
                    session_id,
                    "off",
                    "Call-name reading is off. Earlier observed names are kept.",
                )
                .map_err(map_store_error)?;
            store
                .set_call_names_automatic(session_id, automatically_use)
                .map_err(map_store_error)?;
        }
        drop(actor);
        store.call_name_status(session_id).map_err(map_store_error)
    }

    pub async fn call_name_dismiss(
        &self,
        session_id: MeetingSessionId,
        speaker_id: SpeakerId,
    ) -> Result<super::call_speakers::CallNameStatus, MeetingCommandError> {
        let store = self.store().await?;
        store
            .dismiss_call_name(session_id, speaker_id)
            .map_err(map_store_error)?;
        store.call_name_status(session_id).map_err(map_store_error)
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
        // The pause is written down before any device hears of it, so a write
        // that fails leaves every lane recording under the phase that says so.
        if let Err(error) = confirm_pause(&store, request.session_id) {
            if let Err(restore) = undo_pause(&store, request.session_id) {
                log::warn!(
                    "Meeting capture {} could not pause and stayed at pausing: {restore:?}",
                    request.session_id.uuid()
                );
            }
            drop(actor);
            self.emit_current(&store, request.session_id);
            return Err(map_store_error(error));
        }
        for source in active.sources.values_mut() {
            if source.source.pause().is_err() {
                note_failed_lane(&store, request.session_id, source.track_id, source.epoch);
            }
        }
        active.hold.keep_awake.release();
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
        active.hold.keep_awake.acquire();
        let mut resumed = Vec::new();
        for source in active.sources.values_mut() {
            let next_epoch = source.epoch.get().checked_add(1).map(SourceEpoch::new);
            let report = next_epoch.and_then(|epoch| source.source.resume(epoch).ok());
            match (next_epoch, report) {
                (Some(epoch), Some(report))
                    if report.track_id == source.track_id && report.epoch.get() >= epoch.get() =>
                {
                    source.epoch = report.epoch;
                    resumed.push(source.track_id);
                }
                _ => {
                    let _ = source.source.abort();
                    note_failed_lane(
                        &store,
                        request.session_id,
                        source.track_id,
                        next_epoch.unwrap_or(source.epoch),
                    );
                }
            }
        }
        if let Err(error) = confirm_resume(&store, request.session_id, !resumed.is_empty()) {
            for source in active
                .sources
                .values_mut()
                .filter(|source| resumed.contains(&source.track_id))
            {
                if source.source.pause().is_err() {
                    let _ = source.source.abort();
                    note_failed_lane(&store, request.session_id, source.track_id, source.epoch);
                }
            }
            if let Err(restore) = undo_resume(&store, request.session_id) {
                log::warn!(
                    "Meeting capture {} could not resume and stayed at resuming: {restore:?}",
                    request.session_id.uuid()
                );
            }
            active.hold.keep_awake.release();
            drop(actor);
            self.emit_current(&store, request.session_id);
            return Err(map_store_error(error));
        }
        if resumed.is_empty() {
            active.hold.keep_awake.release();
        }
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
            .take_if(|active| active.session_id == request.session_id)
            .ok_or(MeetingCommandError::NotFound)?;
        drop(active.call_names.take());
        for source in active.sources.values_mut() {
            // The packet lane is the persistence path. The stop report repeats
            // what the source observed and must not insert every gap a second time.
            let _ = source.source.stop();
            // A writer that died lost only its lane's unsaved tail. What it
            // saved is on disk, and the rest of the meeting still has to stop.
            if let Err(error) = source.worker.stop() {
                log::warn!(
                    "Meeting capture {} stopped a lane whose writer had died: {error:?}",
                    request.session_id.uuid()
                );
                if let Err(error) = store.update_track_health(
                    request.session_id,
                    source.track_id,
                    SourceHealth::Failed,
                ) {
                    log::warn!(
                        "Meeting capture {} could not mark that lane failed: {error:?}",
                        request.session_id.uuid()
                    );
                }
            }
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
        self.import_recording_with(request, None, None).await
    }

    /// Install what the phone knew about a recording before any processing
    /// can read the meeting: its own notes, and the calendar event it was
    /// made during. The event is remembered the way a Mac preflight remembers
    /// one, and followed by the start a live meeting records, so its
    /// attendees are linked to people the way a Mac recording's are.
    pub(crate) async fn import_recording_with(
        &self,
        request: ImportRecordingRequest,
        notes: Option<&str>,
        calendar_event: Option<&CalendarEventSummary>,
    ) -> Result<MeetingSessionSnapshot, MeetingCommandError> {
        let _activity = crate::planning::quiet::foreground_activity();
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
        if let Some(notes) = notes {
            let saved = store
                .notes_template_fallback(session_id, self.default_notes_template())
                .and_then(|choice| {
                    store.save_user_notes(
                        session_id,
                        notes,
                        choice.template,
                        choice.custom_template_id,
                        0,
                    )
                });
            if let Err(error) = saved {
                return Err(self
                    .abandon_import(session_id, map_store_error(error))
                    .await);
            }
        }
        if let Some(event) = calendar_event {
            if let Err(error) = store.remember_calendar_facts(session_id, event) {
                return Err(self
                    .abandon_import(session_id, map_store_error(error))
                    .await);
            }
            self.record_meeting_started(Arc::clone(&store), session_id);
        }
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
        let _activity = crate::planning::quiet::foreground_activity();
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
                    actor: OperationActor::User,
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
            .seal_imported_capture_window(
                session_id,
                duration_ns.unwrap_or_else(|| stopping.elapsed_offset_ns.unwrap_or(0)),
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
        let before = store
            .transcript_correction_source_snapshot(request.session_id)
            .inspect_err(|error| log::warn!("Correction source snapshot failed: {error:?}"))
            .ok();
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
        store
            .enqueue_cloud_tombstone_for_session(
                request.session_id,
                receipt.new_revision.unwrap_or(request.expected_revision),
                format!("tombstone-{}", job_id.uuid()),
                utc_now_ms(),
            )
            .map_err(map_store_error)?;

        self.processing.cancel(request.session_id);

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
            drop(active.call_names.take());
            for source in active.sources.values_mut() {
                let _ = source.source.abort();
                let _ = source.worker.stop();
            }
        }
        let people_revision = store.finish_deletion(job_id).map_err(map_store_error)?;
        if let Some(before) = before {
            match store.transcript_correction_projection_changes(before) {
                Ok(changed) => self.emit_transcript_correction_changes(changed),
                Err(error) => log::warn!("Correction source publication failed: {error:?}"),
            }
        }
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

    /// Every meeting still waiting for recovery, newest first. The list page
    /// is a display limit and recovery rows share the `Failed` state with
    /// failed processing, so the pages are read to the end: a meeting parked
    /// behind a hundred newer ones needs recovering as much as the latest.
    pub async fn recovery_list(&self) -> Result<Vec<MeetingHistorySummary>, MeetingCommandError> {
        let store = self.store().await?;
        let filter = MeetingListFilter {
            status: MeetingStatusFilter::Failed,
            ..MeetingListFilter::default()
        };
        let mut cursor = None;
        let mut entries = Vec::new();
        loop {
            let page = store
                .list_sessions(cursor, 100, &filter)
                .map_err(map_store_error)?;
            cursor = page.entries.last().map(|summary| summary.created_at_utc_ms);
            entries.extend(
                page.entries
                    .into_iter()
                    .filter(|summary| summary.phase == MeetingPhase::RecoveryRequired),
            );
            if !page.has_more || cursor.is_none() {
                break;
            }
        }
        Ok(entries)
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
        store
            .require_retained_transcript(request.session_id)
            .map_err(map_store_error)?;
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
    /// sheet renders it as the draft, which is why that case returns `Ok` with
    /// [`MeetingFollowUpSource::Structured`] rather than an error the button
    /// would have to hide behind. A meeting with no record at all is the one
    /// real absence, and that is `NotFound`.
    ///
    /// An engine that was asked and did not answer is a third thing, and it is
    /// an error. Until this said so, `.ok()` swallowed it and the sheet showed
    /// the structured draft as if that had been the plan, so a relay that was
    /// down was indistinguishable from a machine with no engine configured.
    ///
    /// The generation runs off the runtime, for the same reason
    /// `artifacts_regenerate` does it: the trait is synchronous, the engine
    /// may be the relay, and a runtime worker blocked on the answer is a
    /// worker the relay's own turn needs in order to run. That is why this
    /// button sat at `Drafting...` forever with no request ever leaving the
    /// process.
    pub async fn follow_up_draft(
        &self,
        operation_id: MeetingOperationId,
        session_id: MeetingSessionId,
    ) -> Result<MeetingFollowUpDraft, MeetingCommandError> {
        let store = self.store().await?;
        let review = store.review_snapshot(session_id).map_err(map_store_error)?;
        let content_in = |state: MeetingArtifactState| {
            review
                .artifacts
                .iter()
                .filter(|artifact| artifact.state == state)
                .find_map(|artifact| artifact.content.as_ref())
        };
        let content = content_in(MeetingArtifactState::Current)
            .or_else(|| content_in(MeetingArtifactState::OutOfDate));
        let loops = store.meeting_loops(session_id).map_err(map_store_error)?;
        let evidence = FollowUpEvidence::gather(review.session.title.clone(), content, &loops.rows);
        if evidence.is_empty() {
            return Err(MeetingCommandError::NotFound);
        }

        let preferences = self
            .app
            .as_ref()
            .map(|app| crate::settings::get_settings(app).meeting_prep)
            .unwrap_or_default();
        let calendar = store
            .meeting_calendar_facts(session_id)
            .map_err(map_store_error)?;
        let addresses = calendar
            .as_ref()
            .map(|event| recipient_addresses(&event.attendees))
            .unwrap_or_default();
        let mut mail =
            super::mail_context::read(preferences.email_context_enabled, &addresses, utc_now_ms())
                .await;
        let mut mail_context_status = mail.status_text().to_string();
        // Email never reaches the relay, even if remote meeting notes are on.
        // Without a local model the email is left out, never the draft.
        let local = if mail.threads.is_empty() {
            None
        } else {
            self.processing.local_text_generator()
        };
        if local.is_none() && !mail.threads.is_empty() {
            mail.threads.clear();
            mail_context_status = super::prep::EMAIL_NEEDS_LOCAL_MODEL.into();
        }
        let generator = local.or_else(|| {
            self.processing
                .text_generator_for_session(&store, session_id)
        });
        let generated = match generator {
            Some(generator) => {
                let engine = generator.model_id();
                let prompt = follow_up_prompt(&preferences.about_me);
                let budget = super::processing::evidence_budget(
                    generator.as_ref(),
                    &prompt,
                    FOLLOW_UP_MAX_TOKENS,
                );
                let record = evidence.as_prompt_input();
                let mut input = String::new();
                let had_email = !mail.threads.is_empty();
                loop {
                    input.clear();
                    input.push_str(&record);
                    input.push_str("\n\nCALENDAR AND RECENT EMAIL (context only):\n");
                    input.push_str(
                        &serde_json::json!({"calendar": calendar, "email": mail.threads})
                            .to_string(),
                    );
                    if input.len() <= budget || mail.threads.is_empty() {
                        break;
                    }
                    mail.threads.pop();
                }
                if had_email && mail.threads.is_empty() {
                    mail_context_status =
                        "Email was left out because the local model's context was full.".into();
                }
                if input.len() > budget {
                    return Err(MeetingCommandError::EngineFailure);
                }
                let message = tauri::async_runtime::spawn_blocking(move || {
                    generator.generate(&prompt, &input, FOLLOW_UP_MAX_TOKENS, ReplyShape::Prose)
                })
                .await
                .map_err(|_| MeetingCommandError::EngineFailure)?
                .map_err(map_generation_error)?;
                let message = super::follow_up::validate_follow_up(&message)
                    .map_err(|_| MeetingCommandError::EngineFailure)?;
                Some((engine, message))
            }
            None => None,
        };
        let (engine, message) = match generated {
            Some((engine, message)) => (engine, Some(message)),
            None => ("structured-fallback", None),
        };
        let receipt = store
            .record_follow_up_draft(operation_id, session_id, engine)
            .map_err(map_store_error)?;

        Ok(MeetingFollowUpDraft {
            session_id,
            title: evidence.title,
            source: if message.is_some() {
                MeetingFollowUpSource::Generated
            } else {
                MeetingFollowUpSource::Structured
            },
            message,
            summary: evidence.summary,
            mine: evidence.mine,
            decisions: evidence.decisions,
            receipt,
            mail_context_status,
        })
    }

    /// Prepares one calendar occurrence without creating a recording session.
    /// A single queue prevents an overnight pass and an on-demand press from
    /// loading two local models at once.
    pub(crate) async fn prepare_brief(
        &self,
        event: CalendarEventSummary,
        refresh: bool,
    ) -> Result<super::prep::MeetingBrief, String> {
        let _turn = super::prep::brief_gate().lock().await;
        let store = self
            .store()
            .await
            .map_err(|_| "The meeting store is unavailable.")?;
        let preferences = self
            .app
            .as_ref()
            .map(|app| crate::settings::get_settings(app).meeting_prep)
            .unwrap_or_default();
        let sources = super::prep::gather_sources(&store, &event)
            .map_err(|_| "Past meeting context could not be read.")?;
        // Email may reach only a local model. Everything else follows the
        // remote choice of every meeting the brief draws on.
        let local = if preferences.email_context_enabled {
            self.processing.local_text_generator()
        } else {
            None
        };
        let general =
            self.processing
                .text_generator_for_context(&store, &event.series_key, &sources);
        let mut key = super::prep::cache_key(&event, &preferences, &sources)?;
        for generator in [&local, &general].into_iter().flatten() {
            key.push_str(generator.model_id());
            key.push_str(&generator.model_version());
        }
        let now = utc_now_ms();
        if !refresh {
            if let Some(brief) = store
                .cached_brief(&event.event_key, &key, now)
                .map_err(|_| "The saved brief could not be read.")?
            {
                return Ok(brief);
            }
        }
        let web = super::prep_web::research(
            self.app.as_ref(),
            self.secrets.as_ref(),
            &store,
            &event.attendees,
        )
        .await;
        let addresses = recipient_addresses(&event.attendees);
        let mut mail =
            super::mail_context::read(preferences.email_context_enabled, &addresses, now).await;
        let email_left_out = !mail.threads.is_empty() && local.is_none();
        if email_left_out {
            mail.threads.clear();
        }
        let generator = if mail.threads.is_empty() {
            general
        } else {
            local
        };
        let end = event.end_utc_ms;
        let requested_preferences = preferences.clone();
        let mut brief = tauri::async_runtime::spawn_blocking(move || {
            super::prep::generate_brief(event, preferences, sources, mail, web, generator, now)
        })
        .await
        .map_err(|_| "The brief could not be prepared.")??;
        if email_left_out {
            brief.status.push(' ');
            brief.status.push_str(super::prep::EMAIL_NEEDS_LOCAL_MODEL);
        }
        if self.app.as_ref().is_some_and(|app| {
            crate::settings::get_settings(app).meeting_prep != requested_preferences
        }) {
            return Err("Your preferences changed. Open the brief again to use them.".into());
        }
        store
            .keep_brief(&key, end, &brief)
            .map_err(|_| "The brief could not be saved.")?;
        Ok(brief)
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
        self.loop_resolve_as(OperationActor::User, request).await
    }

    /// The same resolution from an outside caller: `sona --loop-resolve`, and
    /// the MCP tool that shells out to it. Split the way
    /// [`Self::recovery_finalize`] is, and for the same reason - a receipt
    /// that files an outside write under the person who never pressed
    /// anything is wrong in the one field a reader would trust.
    pub(crate) async fn loop_resolve_as(
        &self,
        actor: OperationActor,
        request: MeetingLoopResolveRequest,
    ) -> Result<MeetingLoopMutationResult, MeetingCommandError> {
        let session_id = request.loop_id.session_id();
        let result = self
            .store()
            .await?
            .resolve_loop(request, actor, utc_now_ms())
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
    /// page with what the attempt did.
    ///
    /// The engine follows the person's most recent confirmed meeting, because
    /// D14 routes a meeting's text by that meeting's series and this paragraph
    /// is written out of those meetings' evidence. A person with no meeting has
    /// no engine to pick and nothing to summarize; a Mac with no engine at all
    /// writes nothing. Neither is an error — the paragraph already there is
    /// still true — but each is named in the outcome, because a button that
    /// returned the old paragraph as a success taught readers nothing.
    pub async fn person_summary_regenerate(
        &self,
        person_id: PersonId,
    ) -> Result<PersonSummaryRegenerateResult, MeetingCommandError> {
        let store = self.store().await?;
        let detail = store.person_detail(person_id).map_err(map_store_error)?;
        let session_id = detail
            .detail
            .links
            .iter()
            .find(|link| link.confidence == PersonLinkConfidence::Confirmed)
            .map(|link| link.meeting.id);
        let Some(session_id) = session_id else {
            return Ok(PersonSummaryRegenerateResult {
                outcome: PersonSummaryOutcome::NoEvidence,
                page: detail,
            });
        };
        let Some(generator) = self
            .processing
            .text_generator_for_session(&store, session_id)
        else {
            return Ok(PersonSummaryRegenerateResult {
                outcome: PersonSummaryOutcome::EngineUnavailable,
                page: detail,
            });
        };
        let outcome = write_relationship_summary(&store, person_id, generator.as_ref())
            .map_err(map_store_error)?;
        let page = match outcome {
            PersonSummaryOutcome::Written => {
                store.person_detail(person_id).map_err(map_store_error)?
            }
            PersonSummaryOutcome::NoEvidence
            | PersonSummaryOutcome::EngineUnavailable
            | PersonSummaryOutcome::Failed => detail,
        };
        Ok(PersonSummaryRegenerateResult { outcome, page })
    }

    pub async fn speaker_rename(
        &self,
        request: MeetingSpeakerRenameRequest,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        let store = self.store().await?;
        let before = store
            .transcript_correction_identity_snapshot(request.session_id, &[request.speaker_id])
            .map_err(map_store_error)?;
        let user_name = self.user_name();
        let display_name = request.display_name.clone();
        let operation_id = request.operation_id;
        let session_id = request.session_id;
        let receipt = store
            .rename_speaker(SpeakerRename {
                operation_id: request.operation_id,
                requested_at_utc_ms: utc_now_ms(),
                session_id: request.session_id,
                expected_revision: request.expected_revision,
                speaker_id: request.speaker_id,
                display_name: request.display_name,
                user_name: user_name.as_deref(),
            })
            .map_err(map_store_error)?;
        self.emit_transcript_correction_changes(
            store
                .transcript_correction_projection_changes(before)
                .map_err(map_store_error)?
                .into_iter()
                .filter(|session| session.session_id != request.session_id),
        );
        let result = self.result_for_receipt(Arc::clone(&store), receipt, request.session_id)?;
        self.emit_session_changed(&result.snapshot);
        // Person linking reads a renamed speaker's name without About me, so it
        // would make a person of the user.
        if result.receipt.result == OperationResult::Committed
            && !names_user(&display_name, user_name.as_deref())
        {
            self.record_speaker_renamed(Arc::clone(&store), session_id, operation_id, display_name);
        }
        Ok(result)
    }

    pub async fn speaker_merge(
        &self,
        request: MeetingSpeakerMergeRequest,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        let store = self.store().await?;
        let before = store
            .transcript_correction_identity_snapshot(
                request.session_id,
                &[request.source_speaker_id, request.target_speaker_id],
            )
            .map_err(map_store_error)?;
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
        self.emit_transcript_correction_changes(
            store
                .transcript_correction_projection_changes(before)
                .map_err(map_store_error)?
                .into_iter()
                .filter(|session| session.session_id != request.session_id),
        );
        let result = self.result_for_receipt(store, receipt, request.session_id)?;
        self.emit_session_changed(&result.snapshot);
        Ok(result)
    }

    pub async fn segment_edit(
        &self,
        request: MeetingSegmentEditRequest,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        let store = self.store().await?;
        let before = store
            .transcript_correction_source_snapshot(request.session_id)
            .map_err(map_store_error)?;
        let receipt = store
            .edit_segment(SegmentEdit {
                mutation: StoreMutation {
                    operation_id: request.operation_id,
                    actor: OperationActor::User,
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
        self.emit_transcript_correction_changes(
            store
                .transcript_correction_projection_changes(before)
                .map_err(map_store_error)?
                .into_iter()
                .filter(|session| session.session_id != request.session_id),
        );
        let result = self.result_for_receipt(store, receipt, request.session_id)?;
        self.emit_session_changed(&result.snapshot);
        Ok(result)
    }

    pub async fn segments_set_removed(
        &self,
        request: MeetingSegmentsRemovedRequest,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        let store = self.store().await?;
        let before = store
            .transcript_correction_source_snapshot(request.session_id)
            .map_err(map_store_error)?;
        let receipt = store
            .set_segments_removed(
                StoreMutation {
                    operation_id: request.operation_id,
                    actor: OperationActor::User,
                    requested_at_utc_ms: utc_now_ms(),
                    session_id: request.session_id,
                    expected_revision: request.expected_revision,
                    command: MeetingCommandKind::SegmentEdit,
                },
                &request.segment_ids,
                request.removed,
            )
            .map_err(map_store_error)?;
        self.emit_transcript_correction_changes(
            store
                .transcript_correction_projection_changes(before)
                .map_err(map_store_error)?
                .into_iter()
                .filter(|session| session.session_id != request.session_id),
        );
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
        /* A stale press never reaches an engine. The receipt below refuses it
         * too, but only after the rewrite has been paid for. */
        let current = store
            .session_snapshot(request.session_id)
            .map_err(map_store_error)?;
        if current.transcript_purged_at_utc_ms.is_some() {
            return Err(MeetingCommandError::TranscriptDeleted);
        }
        if current.revision != request.expected_revision {
            return Err(MeetingCommandError::StaleRevision);
        }
        let rewrite = self
            .processing
            .begin_rewrite(request.session_id)
            .ok_or(MeetingCommandError::NotesInProgress)?;
        /* Off the runtime. A relayed generation blocks its calling thread for
         * up to `RELAY_JOIN_TIMEOUT`, and this is an async command, so waiting
         * here would hold a runtime worker for minutes on a machine that is
         * also transcribing. The claim on the notes comes back with them and
         * is held until their receipt is written, so no second press starts
         * between the two. */
        let (rewrite, generated) = {
            let store = Arc::clone(&store);
            let expected_revision = request.expected_revision;
            let operation_id = request.operation_id;
            tauri::async_runtime::spawn_blocking(move || {
                let generated = rewrite.regenerate(&store, expected_revision, operation_id);
                (Some(rewrite), generated)
            })
            .await
            .unwrap_or_else(|_| (None, Err(RunFailure::engine(EngineFailureCause::Panicked))))
        };
        let (artifact, engine, relationship_refresh) = match generated {
            Ok(generated) => generated,
            Err(failure) => {
                /* The row describes this press. Until it did, a rewrite that
                 * failed left the header saying whatever the pipeline had
                 * said, which for a meeting whose notes were written once and
                 * refused the second time was "notes ready". */
                store
                    .set_processing_status(request.session_id, failure.status())
                    .map_err(map_store_error)?;
                if let Ok(snapshot) = store.session_snapshot(request.session_id) {
                    self.emit_session_changed(&snapshot);
                }
                return Err(map_run_failure(failure));
            }
        };
        let regeneration = store
            .record_artifact_regeneration(
                request.operation_id,
                utc_now_ms(),
                request.expected_revision,
                &artifact,
                engine,
            )
            .map_err(map_store_error)?;
        drop(rewrite);
        if regeneration.receipt.result == OperationResult::Rejected {
            return Err(MeetingCommandError::StaleRevision);
        }
        let result =
            self.result_for_receipt(Arc::clone(&store), regeneration.receipt, request.session_id)?;
        self.emit_session_changed(&result.snapshot);
        if regeneration.first_notes {
            self.processing.notes_ready(
                Arc::clone(&store),
                &result.snapshot,
                FollowOns::InsideSona,
            );
        }
        if let Some(refresh) = relationship_refresh {
            self.processing
                .start_relationship_refresh(store, request.session_id, refresh);
        }
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
        let user_notes = store
            .user_notes(request.session_id, self.default_notes_template())
            .map_err(map_store_error)?;
        let contents = export::render(
            request.format,
            &export::ExportDocument {
                review: &review,
                user_notes: &user_notes.body,
            },
        )
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
        let images_store = Arc::clone(&store);
        let images_session_id = request.session_id;
        let images = review.snapshots;
        let images_path = path.clone();
        tauri::async_runtime::spawn_blocking(move || export::write_atomic(&path, &contents))
            .await
            .map_err(|_| MeetingCommandError::ExportFailed)?
            .map_err(|_| MeetingCommandError::ExportFailed)?;
        tauri::async_runtime::spawn_blocking(move || {
            super::snapshots::write_export_images(
                &images_store,
                images_session_id,
                &images,
                &images_path,
            )
        })
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
    /// receipts — comes from the meeting's readable ledger, the one its loops
    /// are read off; its measured half — turns, seconds, word counts, talk
    /// share — comes from analytics, which owns those numbers, and the two are
    /// joined only here. Nothing is mutated, so this takes no operation id and
    /// no expected revision: run it again after a transcript edit and the
    /// counts on the page move while the model's reading of the conversation
    /// stays where it was.
    pub async fn produce_ledger_html(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<String, MeetingCommandError> {
        let store = self.store().await?;
        let review = store.review_snapshot(session_id).map_err(map_store_error)?;
        if !review.can_export {
            return Err(MeetingCommandError::InvalidTransition);
        }
        let (template_id, ledger) = store
            .readable_ledger(session_id)
            .map_err(map_store_error)?
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
        let kind = if let Some(custom_id) = custom_template_id_from_artifact(&template_id) {
            store
                .custom_template(custom_id)
                .map_err(map_store_error)?
                .map(|template| template.name)
                .unwrap_or_else(|| "Custom template".to_string())
        } else {
            MeetingNotesTemplate::from_artifact_template_id(&template_id)
                .unwrap_or_default()
                .label()
                .to_string()
        };
        let page = ledger::build_page(ledger::LedgerPageInput {
            title: &review.session.title,
            kind: &kind,
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
                .user_notes(
                    session_id,
                    store
                        .notes_template_fallback(session_id, self.default_notes_template())
                        .map_err(map_store_error)?,
                )
                .map_err(map_store_error)?,
        })
    }

    pub async fn user_notes_get(
        &self,
        session_id: MeetingSessionId,
    ) -> Result<MeetingUserNotes, MeetingCommandError> {
        let store = self.store().await?;
        let fallback = store
            .notes_template_fallback(session_id, self.default_notes_template())
            .map_err(map_store_error)?;
        store
            .user_notes(session_id, fallback)
            .map_err(map_store_error)
    }

    /// Save the user's notes and chosen template. This is autosaved while a
    /// person types, so it carries its own note revision instead of the
    /// session revision every audited mutation uses.
    pub async fn user_notes_save(
        &self,
        request: MeetingUserNotesSaveRequest,
    ) -> Result<MeetingUserNotes, MeetingCommandError> {
        let store = self.store().await?;
        let notes = store
            .save_user_notes(
                request.session_id,
                &request.body,
                request.template,
                request.custom_template_id,
                request.expected_note_revision,
            )
            .map_err(map_store_error)?;
        self.emit_session_changed(
            &store
                .session_snapshot(request.session_id)
                .map_err(map_store_error)?,
        );
        Ok(notes)
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
            .require_retained_transcript(request.session_id)
            .map_err(map_store_error)?;
        store
            .save_user_notes(
                request.session_id,
                &request.body,
                request.template,
                request.custom_template_id,
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

    /// Live help for a meeting while it records. A question goes with `Ask`
    /// and only with `Ask`: trimmed, not empty, and at most
    /// `LIVE_HELP_QUESTION_MAX_CHARS` characters. Anything else is a request
    /// the live screen never sends.
    pub async fn live_help(
        &self,
        session_id: MeetingSessionId,
        kind: MeetingLiveHelpKind,
        question: Option<String>,
    ) -> Result<MeetingLiveHelp, MeetingCommandError> {
        let question = match (kind, question.as_deref().map(str::trim)) {
            (MeetingLiveHelpKind::Ask, Some(question))
                if !question.is_empty()
                    && question.chars().count() <= LIVE_HELP_QUESTION_MAX_CHARS =>
            {
                Some(question)
            }
            (MeetingLiveHelpKind::Ask, _) | (_, Some(_)) => {
                return Err(MeetingCommandError::InvalidRequest)
            }
            (_, None) => None,
        };
        let store = self.store().await?;
        let live = self.live_transcript(session_id);
        self.processing
            .live_help(&store, session_id, live.as_deref(), kind, question)
            .map_err(map_processing_error)
    }

    /// The words the running capture has recognized so far, for the live
    /// screen. Empty for any meeting that is not the one capturing now: the
    /// stored revision is that meeting's transcript, and `meeting_get` reads
    /// it. This never touches the store or the engine — it is a lock and a
    /// copy, so the screen can ask on every transcript event.
    pub fn provisional_transcript(
        &self,
        session_id: MeetingSessionId,
    ) -> MeetingProvisionalTranscript {
        self.live_transcript(session_id)
            .map(|live| live.snapshot(session_id))
            .unwrap_or(MeetingProvisionalTranscript {
                session_id,
                segments: Vec::new(),
            })
    }

    /// The past lines the running capture's last minute of words were
    /// already said in, for the live screen's recall cards. A meeting that
    /// is not capturing has no words to recall from, so no matches; a store
    /// that cannot be read is `Unavailable`. The read runs off the async
    /// workers and asks no model anything.
    pub async fn live_recall(&self, session_id: MeetingSessionId) -> MeetingLiveRecall {
        let Some(live) = self.live_transcript(session_id) else {
            return MeetingLiveRecall::Available {
                matches: Vec::new(),
            };
        };
        let recent = live.recent_lines(LIVE_RECALL_WINDOW_NS);
        let Ok(store) = self.store().await else {
            return MeetingLiveRecall::Unavailable;
        };
        let user_name = self.user_name();
        match tauri::async_runtime::spawn_blocking(move || {
            store.live_recall(
                session_id,
                &recent,
                &mut live.recall_memory(),
                user_name.as_deref(),
            )
        })
        .await
        {
            Ok(Ok(matches)) => MeetingLiveRecall::Available { matches },
            Ok(Err(_)) | Err(_) => MeetingLiveRecall::Unavailable,
        }
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

    /// How long the call's sound has been gone while the person talks,
    /// when the running capture is this meeting's and that is the case.
    pub fn call_silent_ns(&self, session_id: MeetingSessionId) -> Option<u64> {
        self.actor_lock()
            .active
            .as_ref()
            .filter(|active| active.session_id == session_id)
            .and_then(|active| active.audibility.call_silent_ns())
    }

    fn default_notes_template(&self) -> NotesTemplateChoice {
        self.app
            .as_ref()
            .map(|app| {
                let settings = crate::settings::get_settings(app);
                NotesTemplateChoice {
                    template: settings.meeting_notes_template,
                    custom_template_id: settings.meeting_notes_custom_template_id,
                }
            })
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

    pub async fn transcript_retention_get(
        &self,
    ) -> Result<MeetingPartRetentionSnapshot, MeetingCommandError> {
        self.store()
            .await?
            .transcript_retention_policy()
            .map_err(map_store_error)
    }

    pub async fn transcript_retention_set(
        &self,
        request: MeetingRetentionSetRequest,
    ) -> Result<MeetingPartRetentionMutationResult, MeetingCommandError> {
        let store = self.store().await?;
        let receipt = store
            .set_transcript_retention_policy(
                request.operation_id,
                utc_now_ms(),
                request.expected_revision,
                &request.policy,
            )
            .map_err(map_store_error)?;
        let snapshot = store
            .transcript_retention_policy()
            .map_err(map_store_error)?;
        Ok(MeetingPartRetentionMutationResult { receipt, snapshot })
    }

    pub async fn audio_retention_get(
        &self,
    ) -> Result<MeetingPartRetentionSnapshot, MeetingCommandError> {
        self.store()
            .await?
            .audio_retention_policy()
            .map_err(map_store_error)
    }

    pub async fn audio_retention_set(
        &self,
        request: MeetingRetentionSetRequest,
    ) -> Result<MeetingPartRetentionMutationResult, MeetingCommandError> {
        let store = self.store().await?;
        let receipt = store
            .set_audio_retention_policy(
                request.operation_id,
                utc_now_ms(),
                request.expected_revision,
                &request.policy,
            )
            .map_err(map_store_error)?;
        let snapshot = store.audio_retention_policy().map_err(map_store_error)?;
        Ok(MeetingPartRetentionMutationResult { receipt, snapshot })
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

    /// Returns the cached store without opening another SQLCipher connection.
    /// The mount sweeps recovery before it caches anything, so a store this
    /// returns is one recovery has already seen.
    pub(crate) async fn cloud_store(&self) -> Result<Arc<MeetingStore>, MeetingCommandError> {
        self.store_lock()
            .clone()
            .ok_or(MeetingCommandError::StorageUnavailable)
    }
    pub(crate) async fn import_cloud_bundle(
        &self,
        bundle: super::cloud_bundle::CloudMeetingBundleV1,
    ) -> Result<MeetingSessionSnapshot, MeetingCommandError> {
        let _activity = crate::planning::quiet::foreground_activity();
        let store = self.cloud_store().await?;
        let session_id = bundle.import_into_store(&store).map_err(map_store_error)?;
        let snapshot = store
            .session_snapshot(session_id)
            .map_err(map_store_error)?;
        self.emit_session_changed(&snapshot);
        Ok(snapshot)
    }

    /// Reports capture ownership through startup, pause and teardown without locking the actor.
    pub(crate) fn is_capture_active(&self) -> bool {
        self.capture_active.load(Ordering::Acquire)
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
    /// The recovery sweep runs under the same lock, before the store is
    /// cached, so a mounted store is always a swept one: whatever is still
    /// mid-flight in it was left by an earlier launch, and no command of this
    /// launch reaches a meeting the sweep has not seen. A mount that fails
    /// caches nothing, so the next call tries it again.
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
        let recovery = if self.recovers_on_mount {
            opened.recover_interrupted().map_err(map_store_error)?
        } else {
            InterruptedRecovery::default()
        };
        // A lane's health moves from the track worker's thread, which has no
        // handle to the shell. The store tells this listener after each
        // verdict commits, and the live screen re-reads the session — which
        // is also how it learns the revision a later Stop has to carry.
        if let Some(app) = self.app.clone() {
            opened.watch_health(move |change| {
                let _ = app.emit(
                    "meeting:source-health-changed",
                    MeetingEventPayload {
                        event_schema_version: MEETING_EVENT_SCHEMA_VERSION,
                        session_id: Some(change.session_id),
                        revision: change.revision,
                    },
                );
            });
        }
        *self.recovered_at_mount_lock() = recovery.recovered.clone();
        let store = cached.insert(opened).clone();
        drop(cached);
        if self.recovers_on_mount {
            if let Err(error) = self.announce_recovery(&store, &recovery) {
                log::warn!("Meeting recovery could not refresh the open windows: {error:?}");
            }
        }
        super::workflow_engine::resume_pending_workflow_events(
            Arc::clone(&store),
            self.app.clone(),
        );
        self.finish_saved_runs(&store, &recovery.finished);
        Ok(store)
    }

    /// The user's name from About me.
    pub(crate) fn user_name(&self) -> Option<String> {
        self.app.as_ref().and_then(super::prep::user_name)
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
            .map(|app| crate::settings::get_settings(app).meeting_transcription_language)
            .unwrap_or_else(|| "auto".to_string());
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

    fn recovered_at_mount_lock(&self) -> std::sync::MutexGuard<'_, Vec<RecoveredMeeting>> {
        self.recovered_at_mount
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

    pub(super) fn emit_session_changed(&self, snapshot: &MeetingSessionSnapshot) {
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

    /// A command that failed after moving the phase still tells the windows,
    /// so the next press carries the revision the store has now.
    fn emit_current(&self, store: &MeetingStore, session_id: MeetingSessionId) {
        if let Ok(snapshot) = store.session_snapshot(session_id) {
            self.emit_session_changed(&snapshot);
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
        meter: TrackMeter,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            run_track_worker(store, writer, reader, report, meter, worker_stop)
        });
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
    mut meter: TrackMeter,
    stop: Arc<AtomicBool>,
) -> Result<(), StoreError> {
    super::capture::run_at_capture_priority();
    let mut bridges = HashMap::new();
    bridges.insert(
        (report.epoch.get(), report.format_epoch),
        report.timestamp_bridge,
    );
    let mut samples = Vec::new();
    loop {
        let mut progressed = record_clock_epochs(&store, &mut reader, &mut bridges)?;
        while let Some(gap) = reader.pop_gap() {
            store.record_gap(&gap)?;
            progressed = true;
        }
        while let Some(run) = reader.pop_dropped() {
            // An epoch reaches its ring before any packet that names it, so a
            // second look finds the bridge for a run that ended after the first.
            record_clock_epochs(&store, &mut reader, &mut bridges)?;
            store.record_gap(&dropped_gap(run, &bridges))?;
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
                    meter.feed(&samples, packet.sample_rate_hz, packet.channels);
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
            Err(PacketLaneReadError::DescriptorWithoutSamples(packet)) => {
                store.record_gap(&dropped_gap(DroppedRun::new(packet), &bridges))?;
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

/// Records the clock epochs the lane announced and keeps their bridges for
/// what comes after them. True when there was one.
fn record_clock_epochs(
    store: &MeetingStore,
    reader: &mut PacketLaneReader,
    bridges: &mut HashMap<(u64, u64), TimestampBridge>,
) -> Result<bool, StoreError> {
    let mut recorded = false;
    while let Some(epoch) = reader.pop_clock_epoch() {
        store.record_clock_epoch(epoch)?;
        bridges.insert((epoch.epoch.get(), epoch.format_epoch), epoch.bridge);
        recorded = true;
    }
    Ok(recorded)
}

/// Where a run the lane refused sits on the session clock: from its first
/// packet's start to its last packet's end, each read through the bridge its
/// epoch announced. Without that bridge the run keeps its size but no time.
fn dropped_gap(run: DroppedRun, bridges: &HashMap<(u64, u64), TimestampBridge>) -> SourceGap {
    let start_of = |packet: CapturedPacket| {
        let bridge = bridges.get(&(packet.source_epoch.get(), packet.format_epoch))?;
        packet_start_offset_ns(*bridge, packet)
    };
    SourceGap {
        track_id: run.first.track_id,
        epoch: run.first.source_epoch,
        start_offset_ns: start_of(run.first),
        end_offset_ns: start_of(run.last)
            .and_then(|start| start.checked_add(run.last.duration_ns()?)),
        reason: SourceGapReason::PacketDropped,
        dropped_frames: Some(run.frames),
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

/// A phase change the session makes on its own once a command's transition
/// has committed: no operation id and no receipt, fenced on the phase.
fn system_transition(
    store: &MeetingStore,
    session_id: MeetingSessionId,
    command: MeetingCommandKind,
    from: MeetingPhase,
    to: MeetingPhase,
    event_kind: &str,
    reason_codes: Vec<MeetingReasonCode>,
) -> Result<(), StoreError> {
    let current = store.session_snapshot(session_id)?;
    store.transition(StoreTransition {
        operation_id: None,
        actor: OperationActor::System,
        command,
        requested_at_utc_ms: utc_now_ms(),
        session_id,
        expected_revision: current.revision,
        allowed_from: &[from],
        next_phase: to,
        event_kind,
        reason_codes,
    })?;
    Ok(())
}

/// Close the capture window over what reached the disk and commit the pause.
fn confirm_pause(store: &MeetingStore, session_id: MeetingSessionId) -> Result<(), StoreError> {
    let pausing = store.session_snapshot(session_id)?;
    store.close_open_capture_window(
        session_id,
        pausing.elapsed_offset_ns.unwrap_or(0),
        "paused",
    )?;
    system_transition(
        store,
        session_id,
        MeetingCommandKind::Pause,
        MeetingPhase::CapturingPausing,
        MeetingPhase::CapturingPaused,
        "pause_confirmed",
        Vec::new(),
    )
}

/// Put a pause that could not be written back to recording, with the capture
/// window it may have closed open again.
fn undo_pause(store: &MeetingStore, session_id: MeetingSessionId) -> Result<(), StoreError> {
    let pausing = store.session_snapshot(session_id)?;
    if pausing.open_capture_window_started_at_ns.is_none() {
        store.open_capture_window(session_id, pausing.elapsed_offset_ns.unwrap_or(0))?;
    }
    system_transition(
        store,
        session_id,
        MeetingCommandKind::Pause,
        MeetingPhase::CapturingPausing,
        MeetingPhase::CapturingRecording,
        "pause_failed",
        Vec::new(),
    )
}

/// Open a capture window over the lanes that came back and commit where the
/// resume landed. A window opens only over audio that is arriving, so with no
/// lane back the meeting stays paused under a committed receipt; a failed
/// lane does not come back, so what it offers then is Stop.
fn confirm_resume(
    store: &MeetingStore,
    session_id: MeetingSessionId,
    any_resumed: bool,
) -> Result<(), StoreError> {
    let (next_phase, event_kind, reason_codes) = if any_resumed {
        let resuming = store.session_snapshot(session_id)?;
        store.open_capture_window(session_id, resuming.elapsed_offset_ns.unwrap_or(0))?;
        (
            MeetingPhase::CapturingRecording,
            "resume_confirmed",
            Vec::new(),
        )
    } else {
        (
            MeetingPhase::CapturingPaused,
            "resume_failed",
            vec![MeetingReasonCode::SourceStartFailed],
        )
    };
    system_transition(
        store,
        session_id,
        MeetingCommandKind::Resume,
        MeetingPhase::CapturingResuming,
        next_phase,
        event_kind,
        reason_codes,
    )
}

/// Put a resume that could not be written back to paused, closing the capture
/// window it may have opened.
fn undo_resume(store: &MeetingStore, session_id: MeetingSessionId) -> Result<(), StoreError> {
    let resuming = store.session_snapshot(session_id)?;
    if resuming.open_capture_window_started_at_ns.is_some() {
        store.close_open_capture_window(
            session_id,
            resuming.elapsed_offset_ns.unwrap_or(0),
            "resume_failed",
        )?;
    }
    system_transition(
        store,
        session_id,
        MeetingCommandKind::Resume,
        MeetingPhase::CapturingResuming,
        MeetingPhase::CapturingPaused,
        "resume_failed",
        Vec::new(),
    )
}

/// Write down a lane whose device would not pause or resume: a gap from here
/// and failed health. Best effort, because the phase has already moved and an
/// error here would only hide that from the caller.
fn note_failed_lane(
    store: &MeetingStore,
    session_id: MeetingSessionId,
    track_id: SourceTrackId,
    epoch: SourceEpoch,
) {
    let noted = store
        .record_gap(&SourceGap {
            track_id,
            epoch,
            start_offset_ns: None,
            end_offset_ns: None,
            reason: SourceGapReason::SourceStopped,
            dropped_frames: None,
        })
        .and_then(|()| store.update_track_health(session_id, track_id, SourceHealth::Failed));
    if let Err(error) = noted {
        log::warn!(
            "Meeting capture {} could not record a failed lane: {error:?}",
            session_id.uuid()
        );
    }
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

/// Which application's chat box a disclosure belongs in.
///
/// An offer raised by an application names it. A calendar offer names none,
/// so the meeting application in front when the offer was accepted stands in:
/// the consent panel does not take focus, so that is the application the
/// person was looking at. Anything else in front — an editor, a terminal,
/// nothing — means no composer is known, and the attempt is refused later
/// rather than aimed at whatever happens to be focused.
fn composer_app_for(trigger_bundle_id: Option<&str>) -> Option<String> {
    if let Some(bundle_id) = trigger_bundle_id {
        return Some(bundle_id.to_string());
    }
    crate::context::frontmost_application_identifier().filter(|bundle_id| {
        super::detection::apps::is_browser_bundle_id(bundle_id)
            || super::detection::apps::DEFAULT_MEETING_APP_BUNDLE_IDS
                .iter()
                .any(|candidate| bundle_id.eq_ignore_ascii_case(candidate))
    })
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

pub(crate) fn map_store_error(error: StoreError) -> MeetingCommandError {
    match error {
        StoreError::NotFound | StoreError::PersonNotFound | StoreError::SpeakerNotFound => {
            MeetingCommandError::NotFound
        }
        StoreError::ConsentStale => MeetingCommandError::ConsentStale,
        StoreError::TranscriptDeleted => MeetingCommandError::TranscriptDeleted,
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
        StoreError::TranscriptCorrectionConflict => {
            MeetingCommandError::TranscriptCorrectionConflict
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
        ProcessingFailure::TranscriptDeleted => MeetingCommandError::TranscriptDeleted,
        // `Interrupted` is written by startup recovery, never returned by a
        // live operation. If one ever surfaces it, the meeting does need
        // recovery, which is the error that says so.
        ProcessingFailure::Interrupted => MeetingCommandError::RecoveryRequired,
        ProcessingFailure::EngineFailure => MeetingCommandError::EngineFailure,
    }
}

/// A run's failure as the command's answer, keeping the engine causes whose
/// remedy is not "try again": a meeting the safety filter stopped, one the
/// model declined to write about, and one in a language it does not support.
fn map_run_failure(failure: RunFailure) -> MeetingCommandError {
    match failure.cause() {
        Some(EngineFailureCause::SafetyBlocked) => MeetingCommandError::SafetyBlocked,
        Some(EngineFailureCause::ModelDeclined) => MeetingCommandError::ModelDeclined,
        Some(EngineFailureCause::UnsupportedLanguage) => MeetingCommandError::UnsupportedLanguage,
        _ => map_processing_error(failure.reason()),
    }
}

/// A generation the operator asked for and did not get.
///
/// Every outcome leaves the press with no engine text, and every one is the
/// command's answer rather than something to paper over: `Unreachable` is an
/// engine that was chosen and then went away before the call, `Failed` is one
/// that answered with nothing usable, `TimedOut` is one its time limit cut
/// short, and `ReplyNotStructured` is one that answered in the wrong form. The
/// only caller asks for [`ReplyShape::Prose`], which no reply can be the wrong
/// form of, so the shape refusal is here to keep this total rather than
/// because a draft can reach it - and one that somehow did arrive is the same
/// thing to the sheet as nothing usable.
const fn map_generation_error(error: MeetingTextGenerationError) -> MeetingCommandError {
    match error {
        MeetingTextGenerationError::Unreachable => MeetingCommandError::RemoteUnavailable,
        MeetingTextGenerationError::SafetyBlocked => MeetingCommandError::SafetyBlocked,
        MeetingTextGenerationError::Declined => MeetingCommandError::ModelDeclined,
        MeetingTextGenerationError::UnsupportedLanguage => MeetingCommandError::UnsupportedLanguage,
        MeetingTextGenerationError::Failed
        | MeetingTextGenerationError::ReplyNotStructured
        | MeetingTextGenerationError::TimedOut
        | MeetingTextGenerationError::ContextExceeded => MeetingCommandError::EngineFailure,
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

    #[test]
    fn capture_status_reads_ownership_while_the_lifecycle_actor_is_locked() {
        let (_directory, manager) = manager_with(Arc::new(NoCaptureSources));
        let manager = Arc::new(manager);
        let actor = manager.actor_lock();
        let lease = CaptureLease::acquire(&manager.capture_active).expect("startup owns capture");
        let (send, receive) = std::sync::mpsc::channel();
        let reader = Arc::clone(&manager);
        let worker = thread::spawn(move || send.send(reader.is_capture_active()));
        let status = receive.recv_timeout(Duration::from_secs(1));
        drop(actor);
        worker
            .join()
            .expect("capture reader completed")
            .expect("report capture");
        assert_eq!(
            status,
            Ok(true),
            "a planner callback cannot wait on the actor"
        );
        assert!(matches!(
            CaptureLease::acquire(&manager.capture_active),
            Err(MeetingCommandError::CaptureLeaseBusy)
        ));
        drop(lease);
        assert!(!manager.is_capture_active());
        assert!(CaptureLease::acquire(&manager.capture_active).is_ok());
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
                        actor: OperationActor::User,
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
        let preflight = tauri::async_runtime::block_on(manager.create_preflight_with_calendar(
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
            None,
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
    fn transcript_retention_refuses_reprocessing_without_changing_saved_notes() {
        let (_directory, manager, _starts, _aborts) = manager();
        let snapshot = review_ready_session(&manager);
        let session_id = snapshot.session_id;
        let store = tauri::async_runtime::block_on(manager.store()).unwrap();
        let saved = store
            .save_user_notes(
                session_id,
                "Keep my own notes",
                MeetingNotesTemplate::General,
                None,
                0,
            )
            .unwrap();
        let now = snapshot.retention_deadline_utc_ms.unwrap() + 86_400_000;
        store
            .set_transcript_retention_policy(
                MeetingOperationId::new(),
                now - 7 * 86_400_000,
                0,
                &MeetingRetentionPolicy::DeleteAfterDays { days: 1 },
            )
            .unwrap();
        assert!(store
            .purge_transcript_at(session_id, now)
            .unwrap()
            .is_some());
        let current = store.session_snapshot(session_id).unwrap();
        let request = || MeetingMutationRequest {
            operation_id: MeetingOperationId::new(),
            session_id,
            expected_revision: current.revision,
        };
        assert!(matches!(
            tauri::async_runtime::block_on(manager.artifacts_regenerate(request())),
            Err(MeetingCommandError::TranscriptDeleted)
        ));
        assert!(matches!(
            tauri::async_runtime::block_on(manager.recovery_finalize(request())),
            Err(MeetingCommandError::TranscriptDeleted)
        ));
        assert!(matches!(
            tauri::async_runtime::block_on(manager.artifacts_reenhance(MeetingReenhanceRequest {
                operation_id: MeetingOperationId::new(),
                session_id,
                expected_revision: current.revision,
                body: "Do not overwrite on a refused rewrite".to_owned(),
                template: MeetingNotesTemplate::General,
                custom_template_id: None,
                expected_note_revision: saved.revision,
            })),
            Err(MeetingCommandError::TranscriptDeleted)
        ));
        assert_eq!(
            store
                .user_notes(session_id, MeetingNotesTemplate::General.into())
                .unwrap(),
            saved
        );
        assert_eq!(
            store.session_snapshot(session_id).unwrap().revision,
            current.revision
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
            location: None,
            url: None,
            place: None,
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
        let preflight = tauri::async_runtime::block_on(manager.create_preflight_with_calendar(
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
            None,
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
        let preflight = tauri::async_runtime::block_on(manager.create_preflight_with_calendar(
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
            None,
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
        let preflight = tauri::async_runtime::block_on(manager.create_preflight_with_calendar(
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
            None,
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
            location: None,
            url: Some("https://meet.example/roadmap".to_string()),
            place: None,
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
        assert!(!store
            .calendar_event_found_under_way(result.snapshot.session_id)
            .unwrap());
    }

    #[test]
    fn a_manual_start_during_a_meeting_records_it_under_that_event() {
        let (_directory, manager, _, _) = manager();
        let event = CalendarEventSummary {
            event_key: "calendar-series-1#1000".to_string(),
            series_key: "calendar-series-1".to_string(),
            title: "Roadmap review".to_string(),
            attendee_count: 2,
            start_utc_ms: 1_000,
            end_utc_ms: 2_000,
            attendees: Vec::new(),
            notes: Some("Review Q4".to_string()),
            calendar_name: Some("Work".to_string()),
            location: None,
            url: None,
            place: None,
        };
        let untitled = CalendarEventSummary {
            title: "  ".to_string(),
            ..event.clone()
        };
        let record_pressed = || MeetingPreflightCreateRequest {
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
        };
        let start = |under_way| {
            tauri::async_runtime::block_on(manager.create_manual_preflight(
                record_pressed(),
                CalendarAtStart {
                    under_way,
                    near: Vec::new(),
                },
            ))
            .unwrap()
            .snapshot
        };
        let joined = start(Some(event.clone()));
        let joined_untitled = start(Some(untitled.clone()));
        let alone = start(None);
        let store = tauri::async_runtime::block_on(manager.store()).unwrap();

        assert_eq!(joined.title, "Roadmap review");
        assert_eq!(
            store.meeting_calendar_facts(joined.session_id).unwrap(),
            Some(event)
        );
        assert!(
            store
                .calendar_event_found_under_way(joined.session_id)
                .unwrap(),
            "nobody chose the event a start by hand found, so nothing sends for it"
        );
        assert_eq!(joined_untitled.title, MANUAL_DEFAULT_TITLE);
        assert_eq!(
            store
                .meeting_calendar_facts(joined_untitled.session_id)
                .unwrap(),
            Some(untitled)
        );
        assert_eq!(alone.title, MANUAL_DEFAULT_TITLE);
        assert_eq!(
            store.meeting_calendar_facts(alone.session_id).unwrap(),
            None
        );
    }

    /* A start by hand between meetings offers the events around it, the one
     * under way first, and keeps its own title and no event until the
     * operator takes one. */
    #[test]
    fn a_manual_start_between_meetings_offers_the_events_around_it() {
        use crate::meeting::detection::calendar::CalendarOccurrence;
        use crate::meeting::evening::{FilingReason, FilingTarget};

        let (_directory, manager, _, _) = manager();
        let now = utc_now_ms();
        let occurrence =
            |key: &str, title: &str, start_utc_ms: i64, end_utc_ms: i64| CalendarOccurrence {
                summary: CalendarEventSummary {
                    event_key: key.to_string(),
                    series_key: key.to_string(),
                    title: title.to_string(),
                    attendee_count: 1,
                    start_utc_ms,
                    end_utc_ms,
                    attendees: Vec::new(),
                    notes: None,
                    calendar_name: Some("Work".to_string()),
                    location: None,
                    url: None,
                    place: None,
                },
                is_recurring: false,
            };
        let review = occurrence("review#1", "Design review", now + 600_000, now + 2_400_000);
        let focus = occurrence("focus#1", "Focus time", now - 1_200_000, now + 2_400_000);
        let request = MeetingPreflightCreateRequest {
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
        };

        let started = tauri::async_runtime::block_on(manager.create_manual_preflight(
            request,
            CalendarAtStart {
                under_way: None,
                near: vec![review.clone(), focus.clone()],
            },
        ))
        .unwrap()
        .snapshot;
        let store = tauri::async_runtime::block_on(manager.store()).unwrap();
        let offered = store
            .context_suggestions(started.session_id, now)
            .unwrap()
            .into_iter()
            .map(|suggestion| (suggestion.target, suggestion.reasons))
            .collect::<Vec<_>>();

        let target = |occurrence: &CalendarOccurrence| FilingTarget::Event {
            event_key: occurrence.summary.event_key.clone(),
            title: occurrence.summary.title.clone(),
            start_utc_ms: occurrence.summary.start_utc_ms,
            end_utc_ms: occurrence.summary.end_utc_ms,
        };
        assert_eq!(
            offered,
            vec![
                (
                    target(&focus),
                    vec![FilingReason::OnCalendar { overlaps: true }]
                ),
                (
                    target(&review),
                    vec![FilingReason::OnCalendar { overlaps: false }]
                ),
            ]
        );
        assert_eq!(started.title, MANUAL_DEFAULT_TITLE);
        assert_eq!(
            store.meeting_calendar_facts(started.session_id).unwrap(),
            None
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
                        actor: OperationActor::User,
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

    /// The launch before this one, over the same store. What it leaves behind
    /// is what this launch's first mount finds and sweeps.
    fn earlier_launch(manager: &MeetingSessionManager) -> MeetingSessionManager {
        MeetingSessionManager::with_parts(
            None,
            manager.root.clone(),
            Arc::clone(&manager.secrets),
            Arc::new(NoCaptureSources),
        )
    }

    /// A command-line read mounts the store while the app records. Its mount
    /// must not take the live meeting for one an earlier launch left behind.
    #[test]
    fn a_reading_process_leaves_a_live_meeting_recording() {
        let (_directory, manager, _backend) = mounted_manager();
        let session_id = interrupted_session(
            &manager,
            MeetingPhase::CapturingRecording,
            ProcessingDestination::Local,
        );

        let reader = earlier_launch(&manager).reading_only();
        let read = tauri::async_runtime::block_on(reader.store())
            .unwrap()
            .session_snapshot(session_id)
            .unwrap();

        assert_eq!(read.phase, MeetingPhase::CapturingRecording);
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
            &earlier_launch(&manager),
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
            &earlier_launch(&manager),
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

    /// An earlier launch wrote a meeting's notes and ended before it moved
    /// the meeting to review. This launch has no local model, so the run that
    /// would have sent the meeting's automations is withheld: the meeting
    /// goes to review with the notes it has, and gets only what stays inside
    /// Sona.
    #[test]
    fn recovery_finishes_saved_notes_without_what_a_withheld_run_sends() {
        let (_directory, manager, _backend) = mounted_manager();
        manager
            .processing
            .set_text_generators(Arc::new(NotesAndAnswers), no_engine());
        let earlier = earlier_launch(&manager);
        let session_id = interrupted_session(
            &earlier,
            MeetingPhase::Processing,
            ProcessingDestination::Local,
        );
        let store = tauri::async_runtime::block_on(earlier.store()).unwrap();
        let (folder_prompt, series_prompt) = folder_and_series_prompts(&store, session_id);
        save_notes(&store, session_id);
        drop(store);
        drop(earlier);

        let recovered = tauri::async_runtime::block_on(manager.recover_at_startup_at(0)).unwrap();

        assert!(
            recovered.is_empty(),
            "a meeting whose notes are written is not run again"
        );
        let store = tauri::async_runtime::block_on(manager.store()).unwrap();
        let snapshot = store.session_snapshot(session_id).unwrap();
        assert_eq!(snapshot.phase, MeetingPhase::ReviewReady);
        assert_eq!(snapshot.processing_status, ProcessingStatus::Succeeded);
        assert_eq!(
            prompts_run(&store, session_id),
            vec![folder_prompt],
            "the series prompt {series_prompt:?} waits for a run"
        );
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
            &earlier_launch(&manager),
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
                reason: ProcessingFailure::LocalModelUnavailable,
                cause: None,
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

    /// Matrix 13: a pipeline that panics still leaves the outcome written down,
    /// and says a panic is what happened. Before, the thread died with it and
    /// the meeting read as processing until the next launch.
    #[test]
    fn a_panicking_pipeline_still_records_a_terminal_status() {
        let (_directory, manager, _backend) = mounted_manager();
        manager
            .processing
            .set_transcript_engine(Arc::new(PanickingEngine));
        let session_id = interrupted_session(
            &earlier_launch(&manager),
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
                reason: ProcessingFailure::EngineFailure,
                cause: Some(crate::meeting::types::EngineFailureCause::Panicked),
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
            &earlier_launch(&manager),
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
        let earlier = earlier_launch(&manager);
        let session_id = interrupted_session(
            &earlier,
            MeetingPhase::Processing,
            ProcessingDestination::Local,
        );
        let store = tauri::async_runtime::block_on(earlier.store()).unwrap();
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
        drop(store);
        drop(earlier);

        let recovered = tauri::async_runtime::block_on(manager.recover_at_startup_at(0)).unwrap();
        let result = tauri::async_runtime::block_on(manager.reprocess_recovered(&recovered));
        let store = tauri::async_runtime::block_on(manager.store()).unwrap();

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
            &earlier_launch(&manager),
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
            &earlier_launch(&manager),
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
        manager
            .processing
            .set_text_generators(no_engine(), no_engine());
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
        let store = tauri::async_runtime::block_on(manager.store()).unwrap();
        let bundle = store
            .export_cloud_meeting_bundle(snapshot.session_id)
            .unwrap();
        let recorded_duration_ms = i64::try_from(
            review
                .session
                .elapsed_offset_ns
                .expect("imported audio has a measured duration")
                / 1_000_000,
        )
        .expect("the short fixture duration fits in milliseconds");
        assert_eq!(
            bundle.session.ended_at_utc_ms,
            Some(1_700_000_000_000 + recorded_duration_ms),
            "retention starts at the original recording end, not the import time",
        );
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

    /// A run says which of its five stages it is in, in order and never
    /// back, and ends by saying it is over. The first run on a Mac has
    /// measured nothing here, so it shows no time left; what it measured is
    /// what times the next one, and its notes carry what the run cost.
    #[test]
    fn a_run_reports_each_stage_in_order_and_leaves_a_receipt() {
        let (files, manager) = importing_manager();
        manager.processing.set_text_generators(
            Arc::new(super::super::processing::tests::EchoingGenerator),
            Arc::new(FixedGenerator {
                available: false,
                output: String::new(),
            }),
        );
        let import = |name: &str| {
            let path = files.path().join(name);
            write_mono_wav(&path, 16_000);
            tauri::async_runtime::block_on(manager.import_recording(ImportRecordingRequest {
                path,
                title: None,
                recorded_at_utc_ms: Some(1_700_000_000_000),
                origin: RecordingOrigin::LocalFile,
            }))
            .expect("the recording imports")
            .session_id
        };

        let first = import("first.wav");
        let published = manager.processing.published_progress();
        let (end, updates) = published.split_last().expect("the run reported");
        assert_eq!(end.session_id, first);
        assert_eq!(end.progress, None, "the run ends by saying it is over");
        let updates: Vec<_> = updates
            .iter()
            .map(|payload| {
                assert_eq!(payload.session_id, first);
                payload.progress.clone().expect("only the last update ends")
            })
            .collect();
        let stages: Vec<_> = updates.iter().map(|update| update.stage).collect();
        assert!(
            stages.windows(2).all(|pair| pair[0] <= pair[1]),
            "a run never steps back a stage: {stages:?}"
        );
        let mut reported = stages.clone();
        reported.dedup();
        assert_eq!(reported, ProcessingStage::ALL, "every stage is reported");
        assert!(
            updates.iter().all(|update| update.eta_ms.is_none()),
            "nothing has been measured on this Mac yet"
        );

        let review = tauri::async_runtime::block_on(manager.get(first))
            .expect("the imported meeting is readable");
        let receipt = review
            .artifacts
            .iter()
            .find(|artifact| artifact.state == MeetingArtifactState::Current)
            .expect("the run wrote notes")
            .receipt
            .expect("the notes carry the run's receipt");
        let listed = retained_meetings(&manager)
            .into_iter()
            .find(|meeting| meeting.session_id == first)
            .expect("the meeting is listed");
        assert_eq!(
            listed.recorded_duration_ms,
            i64::try_from(receipt.audio_ns / 1_000_000).ok(),
            "the receipt counts the recording the way the list does"
        );
        assert_eq!(receipt.route, ProcessingRoute::Local);

        let second = import("second.wav");
        assert!(
            manager
                .processing
                .published_progress()
                .iter()
                .filter(|payload| payload.session_id == second)
                .any(|payload| payload
                    .progress
                    .as_ref()
                    .is_some_and(|update| update.eta_ms.is_some())),
            "the first run's stages time the second"
        );
    }

    /// An engine that is not there, so a run writes no notes.
    fn no_engine() -> Arc<FixedGenerator> {
        Arc::new(FixedGenerator {
            available: false,
            output: String::new(),
        })
    }

    /// An engine that writes what `EchoingGenerator` writes and answers every
    /// saved prompt with one line, so a test can count the prompts that ran.
    struct NotesAndAnswers;

    impl super::super::processing::MeetingTextGenerator for NotesAndAnswers {
        fn is_available(&self) -> bool {
            true
        }

        fn model_id(&self) -> &'static str {
            "notes-and-answers"
        }

        fn model_version(&self) -> std::borrow::Cow<'static, str> {
            std::borrow::Cow::Borrowed("notes-and-answers-v1")
        }

        fn max_input_bytes(&self) -> usize {
            usize::MAX
        }

        fn generate(
            &self,
            system_prompt: &str,
            evidence: &str,
            max_tokens: i32,
            shape: ReplyShape,
        ) -> Result<String, MeetingTextGenerationError> {
            match shape {
                ReplyShape::Prose => Ok("- Ship on Friday".to_string()),
                _ => super::super::processing::MeetingTextGenerator::generate(
                    &super::super::processing::tests::EchoingGenerator,
                    system_prompt,
                    evidence,
                    max_tokens,
                    shape,
                ),
            }
        }
    }

    /// A one-second recording imported under `title` with no engine to
    /// write notes: the meeting reaches review with a transcript and nothing
    /// else.
    fn import_without_notes(
        files: &TempDir,
        manager: &MeetingSessionManager,
        title: Option<&str>,
    ) -> MeetingSessionId {
        manager
            .processing
            .set_text_generators(no_engine(), no_engine());
        let path = files.path().join("Pricing sync.wav");
        write_mono_wav(&path, 16_000);
        tauri::async_runtime::block_on(manager.import_recording(ImportRecordingRequest {
            path,
            title: title.map(str::to_string),
            recorded_at_utc_ms: Some(1_700_000_000_000),
            origin: RecordingOrigin::LocalFile,
        }))
        .expect("the recording imports")
        .session_id
    }

    /// "Try again" on the meeting as it is now.
    fn rewrite(
        manager: &MeetingSessionManager,
        session_id: MeetingSessionId,
    ) -> Result<MeetingMutationResult, MeetingCommandError> {
        let expected_revision = tauri::async_runtime::block_on(manager.store())
            .unwrap()
            .session_snapshot(session_id)
            .unwrap()
            .revision;
        tauri::async_runtime::block_on(manager.artifacts_regenerate(MeetingMutationRequest {
            operation_id: MeetingOperationId::new(),
            session_id,
            expected_revision,
        }))
    }

    /// A folder that runs one saved prompt on its meetings' notes, with the
    /// meeting filed in it, and a series that runs another, with the meeting
    /// in the series. The folder's prompt comes first.
    fn folder_and_series_prompts(
        store: &MeetingStore,
        session_id: MeetingSessionId,
    ) -> (SavedPromptId, SavedPromptId) {
        use crate::meeting::automation_types::{
            MeetingAutomationKind, MeetingSeriesAutomationSetRequest,
        };
        use crate::meeting::folder_types::{
            MeetingFolderCreateRequest, MeetingFolderDefaultsSetRequest,
            MeetingFolderMembershipRequest,
        };
        use crate::meeting::prompt_types::{PromptOutput, PromptTarget, SavedPromptSaveRequest};
        const SERIES_KEY: &str = "weekly-pricing";
        let save = |name: &str| {
            let saved = store
                .save_saved_prompt(
                    &SavedPromptSaveRequest {
                        operation_id: MeetingOperationId::new(),
                        prompt_id: None,
                        name: name.to_string(),
                        body: "List the decisions.".to_string(),
                        output: PromptOutput::Text,
                        target: PromptTarget::Meeting,
                        expected_revision: store.saved_prompts().unwrap().revision,
                    },
                    1,
                )
                .unwrap();
            saved
                .prompts
                .prompts
                .iter()
                .find(|prompt| prompt.name == name)
                .expect("the prompt is saved")
                .prompt_id
        };
        let folder_prompt = save("Folder decisions");
        let series_prompt = save("Series decisions");
        store
            .remember_calendar_facts(
                session_id,
                &CalendarEventSummary {
                    event_key: format!("{SERIES_KEY}#1"),
                    series_key: SERIES_KEY.to_string(),
                    title: "Weekly pricing".to_string(),
                    attendee_count: 2,
                    start_utc_ms: 1,
                    end_utc_ms: 1_800_001,
                    attendees: Vec::new(),
                    notes: None,
                    calendar_name: None,
                    location: None,
                    url: None,
                    place: None,
                },
            )
            .unwrap();
        let automation = store
            .set_series_automation(
                &MeetingSeriesAutomationSetRequest {
                    operation_id: MeetingOperationId::new(),
                    series_key: SERIES_KEY.to_string(),
                    kind: MeetingAutomationKind::RunPrompt,
                    enabled: true,
                    target: Some(series_prompt.uuid().to_string()),
                    expected_revision: store.series_automations(SERIES_KEY).unwrap().revision,
                },
                1,
            )
            .unwrap();
        assert_eq!(automation.receipt.result, OperationResult::Committed);
        let created = store
            .create_meeting_folder(
                &MeetingFolderCreateRequest {
                    operation_id: MeetingOperationId::new(),
                    name: "Clients".to_string(),
                    expected_revision: store.meeting_folder_list().unwrap().revision,
                },
                1,
            )
            .unwrap();
        let folder_id = created
            .folders
            .folders
            .iter()
            .find(|folder| folder.name == "Clients")
            .expect("the folder is made")
            .folder_id;
        let defaults = store
            .set_meeting_folder_defaults(
                &MeetingFolderDefaultsSetRequest {
                    operation_id: MeetingOperationId::new(),
                    folder_id,
                    template: None,
                    prompt_ids: vec![folder_prompt],
                    expected_revision: created.folders.revision,
                },
                1,
            )
            .unwrap();
        assert_eq!(defaults.receipt.result, OperationResult::Committed);
        let filed = store
            .add_meeting_to_folder(
                &MeetingFolderMembershipRequest {
                    operation_id: MeetingOperationId::new(),
                    folder_id,
                    session_id,
                    expected_revision: defaults.folders.revision,
                },
                1,
            )
            .unwrap();
        assert_eq!(filed.receipt.result, OperationResult::Committed);
        (folder_prompt, series_prompt)
    }

    /// The saved prompts that have run on the meeting.
    fn prompts_run(store: &MeetingStore, session_id: MeetingSessionId) -> Vec<SavedPromptId> {
        store
            .prompt_runs(&crate::meeting::prompt_types::PromptTargetRef::Meeting { session_id })
            .unwrap()
            .into_iter()
            .map(|run| run.prompt_id)
            .collect()
    }

    /// A meeting still under the placeholder title whose first notes come
    /// from "Try again": the press keeps the notes and the title read off
    /// them, instead of failing as stale over the title it wrote itself.
    #[test]
    fn a_rewrite_that_names_the_meeting_keeps_its_notes() {
        let (files, manager) = importing_manager();
        let session_id = import_without_notes(&files, &manager, Some(MANUAL_DEFAULT_TITLE));
        manager
            .processing
            .set_text_generators(Arc::new(NotesAndAnswers), no_engine());

        let result = rewrite(&manager, session_id).expect("the rewrite is recorded");

        assert_eq!(result.receipt.result, OperationResult::Committed);
        assert_eq!(
            result.snapshot.processing_status,
            ProcessingStatus::Succeeded
        );
        assert_ne!(result.snapshot.title, MANUAL_DEFAULT_TITLE);
    }

    /// A second "Try again" while the first is still writing is refused,
    /// rather than writing a second set of notes over the first, and only
    /// one engine pass runs.
    #[test]
    fn a_second_rewrite_is_refused_while_the_first_writes() {
        /// Holds its first answer until the test lets it go, and otherwise
        /// writes what `EchoingGenerator` writes.
        struct HeldGenerator {
            entered: Mutex<std::sync::mpsc::Sender<()>>,
            release: Mutex<std::sync::mpsc::Receiver<()>>,
            held: AtomicBool,
            notes_asked: std::sync::atomic::AtomicUsize,
        }

        impl super::super::processing::MeetingTextGenerator for HeldGenerator {
            fn is_available(&self) -> bool {
                true
            }

            fn model_id(&self) -> &'static str {
                "held"
            }

            fn model_version(&self) -> std::borrow::Cow<'static, str> {
                std::borrow::Cow::Borrowed("held-v1")
            }

            fn max_input_bytes(&self) -> usize {
                usize::MAX
            }

            fn generate(
                &self,
                system_prompt: &str,
                evidence: &str,
                max_tokens: i32,
                shape: ReplyShape,
            ) -> Result<String, MeetingTextGenerationError> {
                if shape == ReplyShape::MeetingNotes {
                    self.notes_asked.fetch_add(1, Ordering::SeqCst);
                }
                if !self.held.swap(true, Ordering::SeqCst) {
                    self.entered
                        .lock()
                        .unwrap()
                        .send(())
                        .expect("the test waits");
                    self.release
                        .lock()
                        .unwrap()
                        .recv()
                        .expect("the test lets it go");
                }
                super::super::processing::MeetingTextGenerator::generate(
                    &super::super::processing::tests::EchoingGenerator,
                    system_prompt,
                    evidence,
                    max_tokens,
                    shape,
                )
            }
        }

        let (files, manager) = importing_manager();
        let session_id = import_without_notes(&files, &manager, None);
        let (entered, first_entered) = std::sync::mpsc::channel();
        let (release, held) = std::sync::mpsc::channel();
        let generator = Arc::new(HeldGenerator {
            entered: Mutex::new(entered),
            release: Mutex::new(held),
            held: AtomicBool::new(false),
            notes_asked: std::sync::atomic::AtomicUsize::new(0),
        });
        manager
            .processing
            .set_text_generators(generator.clone(), no_engine());
        let first = {
            let manager = Arc::clone(&manager);
            thread::spawn(move || rewrite(&manager, session_id))
        };
        first_entered
            .recv()
            .expect("the first press reached the engine");

        let second = rewrite(&manager, session_id);
        release.send(()).expect("the first press is waiting");
        let first = first.join().expect("the first press returns");

        assert_eq!(second.err(), Some(MeetingCommandError::NotesInProgress));
        assert_eq!(
            first.expect("the first press writes").receipt.result,
            OperationResult::Committed
        );
        assert_eq!(generator.notes_asked.load(Ordering::SeqCst), 1);
    }

    /// A meeting whose first notes come from "Try again" gets the folder
    /// prompts it is filed for, as a run's notes would, and nothing that can
    /// leave the Mac: its series automations stay with a run.
    #[test]
    fn first_notes_from_a_rewrite_run_only_the_folder_prompts() {
        let (files, manager) = importing_manager();
        let session_id = import_without_notes(&files, &manager, None);
        let store = tauri::async_runtime::block_on(manager.store()).unwrap();
        let (folder_prompt, series_prompt) = folder_and_series_prompts(&store, session_id);
        manager
            .processing
            .set_text_generators(Arc::new(NotesAndAnswers), no_engine());

        let result = rewrite(&manager, session_id).expect("the rewrite writes the notes");

        assert_eq!(
            result.snapshot.processing_status,
            ProcessingStatus::Succeeded
        );
        assert_eq!(
            prompts_run(&store, session_id),
            vec![folder_prompt],
            "the series prompt {series_prompt:?} is not asked"
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

        fn model_version(&self) -> std::borrow::Cow<'static, str> {
            std::borrow::Cow::Borrowed("fixed-v1")
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

    /// The engine a meeting resolved to, refusing the way a real one does.
    struct RefusingGenerator {
        error: MeetingTextGenerationError,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl super::super::processing::MeetingTextGenerator for RefusingGenerator {
        fn is_available(&self) -> bool {
            true
        }

        fn model_id(&self) -> &'static str {
            "refusing"
        }

        fn model_version(&self) -> std::borrow::Cow<'static, str> {
            std::borrow::Cow::Borrowed("refusing-v1")
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
        ) -> Result<String, MeetingTextGenerationError> {
            self.calls.fetch_add(1, Ordering::Release);
            Err(self.error)
        }
    }

    /// An engine that enters the async runtime before answering, which is
    /// what a relayed generation does: its transport is a submit-and-poll job
    /// on the same runtime the command is running on. Asked from a runtime
    /// worker this cannot answer at all, so it is the double that tells the
    /// two arrangements apart.
    struct RuntimeEnteringGenerator;

    impl super::super::processing::MeetingTextGenerator for RuntimeEnteringGenerator {
        fn is_available(&self) -> bool {
            true
        }

        fn model_id(&self) -> &'static str {
            "runtime-entering"
        }

        fn model_version(&self) -> std::borrow::Cow<'static, str> {
            std::borrow::Cow::Borrowed("runtime-entering-v1")
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
        ) -> Result<String, MeetingTextGenerationError> {
            tauri::async_runtime::block_on(async {});
            Ok(FOLLOW_UP_MESSAGE.to_string())
        }
    }

    const FOLLOW_UP_MESSAGE: &str = "Thanks all - pricing is still open.";

    /// The notes `meeting_with_artifact` files: one recap line and nothing
    /// else.
    fn recap_artifacts() -> crate::meeting::types::GeneratedMeetingArtifacts {
        crate::meeting::types::GeneratedMeetingArtifacts {
            summary: crate::meeting::types::CitedArtifactText {
                text: RECAP_BULLET.to_string(),
                citations: Vec::new(),
            },
            summary_trace: Vec::new(),
            outline: Vec::new(),
            decisions: Vec::new(),
            action_items: Vec::new(),
            key_questions: Vec::new(),
            risks: Vec::new(),
            follow_up_draft: crate::meeting::types::CitedArtifactText {
                text: String::new(),
                citations: Vec::new(),
            },
            ledger: None,
            ledger_failure: None,
            plain: false,
            plain_because: None,
            left_out: Vec::new(),
        }
    }

    /// Notes for the transcript the meeting has now, written as a run writes
    /// them, with the meeting left where it was.
    fn save_notes(store: &MeetingStore, session_id: MeetingSessionId) {
        let transcript_revision_id = store
            .begin_transcript_revision(TranscriptRevisionInput {
                session_id,
                engine_id: "test",
                model_version: None,
                destination: &ProcessingDestination::Local,
                source_set: &[SourceKind::Microphone],
                language: "en",
            })
            .unwrap();
        store
            .complete_transcript_revision(session_id, transcript_revision_id)
            .unwrap();
        store
            .store_artifact_revision(crate::meeting::store::ArtifactRevisionInput {
                session_id,
                transcript_revision_id,
                input_revision: store.session_snapshot(session_id).unwrap().revision,
                template_id: "test",
                template_version: 1,
                generation_key: "test-saved-notes",
                state: MeetingArtifactState::Current,
                content: Some(&recap_artifacts()),
                generated_at_utc_ms: 1,
            })
            .unwrap();
    }

    /// A review-ready meeting with a current artifact, which is the least a
    /// follow-up can be drafted from: with no record at all the draft is
    /// `NotFound` and no engine is ever asked.
    fn meeting_with_artifact(manager: &MeetingSessionManager) -> MeetingSessionId {
        let session_id = review_ready_session(manager).session_id;
        tauri::async_runtime::block_on(async {
            let store = manager.store().await.unwrap();
            let content = recap_artifacts();
            // A `Current` artifact generated from a revision that is not the
            // session's own is filed `OutOfDate` instead, which is the whole
            // point of the field: the record moved on after the pass ran. The
            // draft reads current artifacts only, so this fixture has to sit on
            // the revision the meeting actually has.
            let revision = store.session_snapshot(session_id).unwrap().revision;
            let transcript_revision_id = store
                .begin_transcript_revision(crate::meeting::store::TranscriptRevisionInput {
                    session_id,
                    engine_id: "test",
                    model_version: None,
                    destination: &ProcessingDestination::Local,
                    source_set: &[SourceKind::Microphone],
                    language: "en",
                })
                .unwrap();
            store
                .store_artifact_revision(crate::meeting::store::ArtifactRevisionInput {
                    session_id,
                    transcript_revision_id,
                    input_revision: revision,
                    template_id: "test",
                    template_version: 1,
                    generation_key: "test-follow-up",
                    state: MeetingArtifactState::Current,
                    content: Some(&content),
                    generated_at_utc_ms: 1,
                })
                .unwrap();
        });
        session_id
    }

    /// The press that hung forever. The draft ran the engine on the runtime
    /// and chained its answer into `.ok()`, so every refusal became the
    /// structured draft: the sheet showed the record back with no message and
    /// nothing anywhere said the engine had refused. A refusal is the
    /// command's answer, and each one has a different thing to say to the
    /// operator - a server that was not there is not a model that failed.
    #[test]
    fn a_refused_follow_up_draft_answers_with_the_refusal() {
        for (error, expected) in [
            (
                MeetingTextGenerationError::Unreachable,
                MeetingCommandError::RemoteUnavailable,
            ),
            (
                MeetingTextGenerationError::Failed,
                MeetingCommandError::EngineFailure,
            ),
            (
                MeetingTextGenerationError::ReplyNotStructured,
                MeetingCommandError::EngineFailure,
            ),
            (
                MeetingTextGenerationError::TimedOut,
                MeetingCommandError::EngineFailure,
            ),
        ] {
            let (_directory, manager, _starts, _aborts) = manager();
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            manager.processing.set_text_generators(
                Arc::new(RefusingGenerator {
                    error,
                    calls: Arc::clone(&calls),
                }),
                Arc::new(FixedGenerator {
                    available: false,
                    output: String::new(),
                }),
            );
            let session_id = meeting_with_artifact(&manager);

            let refusal = tauri::async_runtime::block_on(
                manager.follow_up_draft(MeetingOperationId::new(), session_id),
            )
            .expect_err("a refused generation is not a draft");

            assert_eq!(refusal, expected, "wire error for {error:?}");
            assert_eq!(
                calls.load(Ordering::Acquire),
                1,
                "the meeting's own engine is asked once and not failed over"
            );
        }
    }

    /// A model may decide no email is appropriate. The UI must not turn that
    /// valid answer into a failure or silently substitute a structured draft.
    #[test]
    fn a_follow_up_draft_with_no_email_needed_is_empty_and_successful() {
        let (_directory, manager, _starts, _aborts) = manager();
        manager.processing.set_text_generators(
            Arc::new(FixedGenerator {
                available: true,
                output: "   \n".to_string(),
            }),
            Arc::new(FixedGenerator {
                available: false,
                output: String::new(),
            }),
        );
        let session_id = meeting_with_artifact(&manager);

        let draft = tauri::async_runtime::block_on(
            manager.follow_up_draft(MeetingOperationId::new(), session_id),
        )
        .unwrap();
        assert_eq!(draft.source, MeetingFollowUpSource::Generated);
        assert_eq!(draft.message.as_deref(), Some(""));
    }

    /// A relayed engine's transport runs on the same async runtime the
    /// command does, so the generation has to leave the runtime's own workers
    /// alone. Asked inline from the command's worker it cannot answer at all;
    /// that is the hang the sheet showed as "Drafting..." forever.
    #[test]
    fn a_follow_up_draft_runs_its_engine_off_the_async_runtime() {
        let (_directory, manager, _starts, _aborts) = manager();
        manager.processing.set_text_generators(
            Arc::new(RuntimeEnteringGenerator),
            Arc::new(FixedGenerator {
                available: false,
                output: String::new(),
            }),
        );
        let session_id = meeting_with_artifact(&manager);

        let draft = tauri::async_runtime::block_on(
            manager.follow_up_draft(MeetingOperationId::new(), session_id),
        )
        .unwrap();

        assert_eq!(draft.source, MeetingFollowUpSource::Generated);
        assert_eq!(draft.message.as_deref(), Some(FOLLOW_UP_MESSAGE));
    }

    /// No engine at all is not a failure: the record is the draft, and the
    /// sheet says so. This is the one case the old `.ok()` reported honestly,
    /// and it stays reachable now that refusals do not land here.
    #[test]
    fn a_follow_up_draft_with_no_engine_hands_back_the_record() {
        let (_directory, manager, _starts, _aborts) = manager();
        manager.processing.set_text_generators(
            Arc::new(FixedGenerator {
                available: false,
                output: String::new(),
            }),
            Arc::new(FixedGenerator {
                available: false,
                output: String::new(),
            }),
        );
        let session_id = meeting_with_artifact(&manager);

        let draft = tauri::async_runtime::block_on(
            manager.follow_up_draft(MeetingOperationId::new(), session_id),
        )
        .unwrap();

        assert_eq!(draft.source, MeetingFollowUpSource::Structured);
        assert!(draft.message.is_none());
        assert_eq!(draft.summary, RECAP_BULLET);
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
        let preflight = tauri::async_runtime::block_on(manager.create_preflight_with_calendar(
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
            None,
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
        let preflight = tauri::async_runtime::block_on(manager.create_preflight_with_calendar(
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
            None,
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
                    sample_rate_hz: 1_000,
                    channels: 1,
                })
            ),
            96_000,
            "the plan's count is a floor, not a target"
        );
    }

    /// A 10 ms microphone packet `sequence` packets into the meeting, stamped
    /// in nanoseconds with when it was heard, as the microphone stamps it.
    fn microphone_packet(track_id: SourceTrackId, sequence: u64) -> CapturedPacket {
        let offset_ns = sequence * 10_000_000;
        CapturedPacket {
            track_id,
            source_epoch: SourceEpoch::new(0),
            format_epoch: 0,
            sequence,
            native_timestamp_value: Some(i64::try_from(offset_ns).unwrap()),
            native_timestamp_timescale: Some(1_000_000_000),
            host_monotonic_anchor_ns: Some(offset_ns),
            sample_rate_hz: 48_000,
            channels: 1,
            frame_count: 480,
            discontinuity_flags: PacketDiscontinuityFlags::default(),
        }
    }

    /// Offers the lane microphone packets `sequences` and returns the ones it
    /// refused.
    fn deliver(
        lane: &Mutex<Option<PacketSink>>,
        track_id: SourceTrackId,
        sequences: std::ops::Range<u64>,
    ) -> Vec<u64> {
        let samples = [0.4_f32; 480];
        let mut guard = lane.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let lane = guard.as_mut().expect("the source was handed a lane");
        sequences
            .filter(|&sequence| {
                lane.try_push_interleaved(microphone_packet(track_id, sequence), &samples)
                    != PacketPushResult::Accepted
            })
            .collect()
    }

    /// Offers the lane microphone packets `sequences` while something else
    /// holds the meeting database, as a long write elsewhere in the app does,
    /// then lets the database go. Returns the ones the lane refused.
    fn deliver_while_the_database_is_held(
        store: &Arc<MeetingStore>,
        lane: &Mutex<Option<PacketSink>>,
        track_id: SourceTrackId,
        sequences: std::ops::Range<u64>,
    ) -> Vec<u64> {
        let (held, holding) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let holder = thread::spawn({
            let store = Arc::clone(store);
            move || {
                store.with_planning_snapshot(|_| {
                    held.send(()).unwrap();
                    released.recv().unwrap();
                    Ok(())
                })
            }
        });
        holding.recv().unwrap();
        let refused = deliver(lane, track_id, sequences);
        release.send(()).unwrap();
        holder.join().unwrap().unwrap();
        refused
    }

    fn stop_capturing(manager: &MeetingSessionManager, session_id: MeetingSessionId) {
        let store = tauri::async_runtime::block_on(manager.store()).unwrap();
        tauri::async_runtime::block_on(manager.stop(
            MeetingMutationRequest {
                operation_id: MeetingOperationId::new(),
                session_id,
                expected_revision: store.session_snapshot(session_id).unwrap().revision,
            },
            MeetingStopCause::Operator(MeetingStopSurface::MeetingLive),
        ))
        .unwrap();
    }

    /// A writer held up for ten seconds, by the database or a busy Mac, loses
    /// none of what the device delivered meanwhile: the lane holds it and the
    /// writer catches up. A lane of four seconds and 128 packets dropped all
    /// but the first 1.3 seconds of it.
    #[test]
    fn a_writer_held_up_for_ten_seconds_loses_no_audio() {
        const PACKETS: u64 = 1_000;
        let (_directory, manager, session_id, lane) =
            capturing_meeting(Arc::new(ReadyEngine), no_engine());
        let store = tauri::async_runtime::block_on(manager.store()).unwrap();
        let track_id = store.session_snapshot(session_id).unwrap().sources[0]
            .track_id
            .expect("a started microphone track");
        let refused = deliver_while_the_database_is_held(&store, &lane, track_id, 0..PACKETS);
        stop_capturing(&manager, session_id);
        let review = tauri::async_runtime::block_on(manager.get(session_id)).unwrap();

        assert_eq!(
            refused.len(),
            0,
            "the lane refused audio while its writer waited"
        );
        assert!(
            review.gaps.iter().all(|gap| !matches!(
                gap.reason,
                SourceGapReason::PacketDropped | SourceGapReason::WriterPressure
            )),
            "audio was dropped: {:?}",
            review.gaps
        );
        assert_eq!(review.tracks[0].durable_record_count, PACKETS);
    }

    /// A meeting whose writer was held up for longer than its lane holds: 63
    /// seconds of microphone offered behind a held database, then more until
    /// the lane takes ten packets again, then stopped. Returns the review,
    /// the packets the lane refused, and where the last packet heard ends.
    fn a_meeting_that_lost_audio_behind_a_held_writer() -> (MeetingReviewSnapshot, Vec<u64>, u64) {
        let (_directory, manager, session_id, lane) =
            capturing_meeting(Arc::new(ReadyEngine), no_engine());
        let store = tauri::async_runtime::block_on(manager.store()).unwrap();
        let track_id = store.session_snapshot(session_id).unwrap().sources[0]
            .track_id
            .expect("a started microphone track");
        let mut refused = deliver_while_the_database_is_held(&store, &lane, track_id, 0..6_300);
        assert!(!refused.is_empty(), "the lane never filled");
        let (mut next, mut taken) = (6_300, 0);
        while taken < 10 {
            assert!(next < 1_000_000, "the lane never took audio again");
            match deliver(&lane, track_id, next..next + 1).as_slice() {
                [] => taken += 1,
                _ => refused.push(next),
            }
            next += 1;
        }
        stop_capturing(&manager, session_id);
        let review = tauri::async_runtime::block_on(manager.get(session_id)).unwrap();
        (review, refused, next * 10_000_000)
    }

    /// A writer held up for longer than its lane holds loses what arrives
    /// after the lane fills, and the review says where: from the first packet
    /// the lane refused to the end of the last. It used to say "time unknown".
    #[test]
    fn a_loss_behind_a_held_writer_says_where_it_was() {
        let (review, refused, _) = a_meeting_that_lost_audio_behind_a_held_writer();
        let dropped = review
            .gaps
            .iter()
            .filter(|gap| gap.reason == SourceGapReason::PacketDropped)
            .map(|gap| (gap.start_offset_ns, gap.end_offset_ns, gap.dropped_frames))
            .collect::<Vec<_>>();

        assert_eq!(
            dropped,
            [(
                refused.first().map(|first| first * 10_000_000),
                refused.last().map(|last| (last + 1) * 10_000_000),
                Some(u64::try_from(refused.len()).unwrap() * 480),
            )]
        );
    }

    /// The audio heard after a loss stays where it was heard. Laid end to end
    /// instead, it slid back over the hole, and the microphone drifted from
    /// the system audio by every second either one lost.
    #[test]
    fn audio_after_a_loss_stays_where_it_was_heard() {
        let (review, _, heard_until_ns) = a_meeting_that_lost_audio_behind_a_held_writer();

        assert_eq!(review.tracks[0].last_offset_ns, Some(heard_until_ns));
    }

    /// What a recording holds (the Mac kept awake, processing parked behind
    /// it, its lanes and their writers) is the recording's, and goes when it
    /// goes. Each test takes a path where another meeting's command, or a
    /// failure partway through the recording's own, used to take one away
    /// early or leave one behind.
    mod capture_ownership {
        use super::*;
        use crate::meeting::store::{CloudHead, CloudOutboxKind};
        use std::sync::atomic::AtomicUsize;
        use std::sync::mpsc;
        use tauri::async_runtime::block_on;

        /// Long enough for a run nothing parks to finish several times over:
        /// a second of imported audio through the test engine takes a small
        /// part of it.
        const UNPARKED_RUN: Duration = Duration::from_secs(5);

        fn available(source_kind: SourceKind) -> SourceProbe {
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

        /// A device that starts silent, reports the format epoch it was given,
        /// and resumes or refuses to as it was told.
        struct ScriptedSource {
            kind: SourceKind,
            format_epoch: u64,
            resumes: bool,
            aborts: Arc<AtomicUsize>,
            track_id: Option<SourceTrackId>,
        }

        impl ScriptedSource {
            fn report(&self, track_id: SourceTrackId, epoch: SourceEpoch) -> SourceStartReport {
                SourceStartReport {
                    track_id,
                    source_kind: self.kind,
                    format: AudioFormat {
                        sample_rate_hz: 48_000,
                        channels: 1,
                    },
                    epoch,
                    format_epoch: self.format_epoch,
                    timestamp_bridge: TimestampBridge {
                        native_anchor_value: 0,
                        native_timescale: 1_000_000_000,
                        host_monotonic_anchor_ns: 0,
                        session_offset_ns: 0,
                    },
                }
            }
        }

        impl MeetingCaptureSource for ScriptedSource {
            fn probe(&self) -> SourceProbe {
                available(self.kind)
            }

            fn start(
                &mut self,
                plan: SourceStartPlan,
                _anchor: SessionClockAnchor,
                _sink: PacketSink,
            ) -> Result<SourceStartReport, MeetingCaptureError> {
                self.track_id = Some(plan.track_id);
                Ok(self.report(plan.track_id, plan.source_epoch))
            }

            fn pause(&mut self) -> Result<(), MeetingCaptureError> {
                Ok(())
            }

            fn resume(
                &mut self,
                epoch: SourceEpoch,
            ) -> Result<SourceStartReport, MeetingCaptureError> {
                match self.track_id {
                    Some(track_id) if self.resumes => Ok(self.report(track_id, epoch)),
                    _ => Err(MeetingCaptureError::InvalidState),
                }
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

        struct ScriptedSources {
            format_epoch: u64,
            resumes: bool,
            aborts: Arc<AtomicUsize>,
        }

        impl MeetingSourceProvider for ScriptedSources {
            fn probe(&self, source_kind: SourceKind) -> SourceProbe {
                available(source_kind)
            }

            fn acquire(
                &self,
                source_kind: SourceKind,
            ) -> Result<Box<dyn MeetingCaptureSource>, MeetingCaptureError> {
                Ok(Box::new(ScriptedSource {
                    kind: source_kind,
                    format_epoch: self.format_epoch,
                    resumes: self.resumes,
                    aborts: Arc::clone(&self.aborts),
                    track_id: None,
                }))
            }
        }

        fn scripted(
            format_epoch: u64,
            resumes: bool,
        ) -> (TempDir, MeetingSessionManager, Arc<AtomicUsize>) {
            let aborts = Arc::new(AtomicUsize::new(0));
            let (directory, manager) = manager_with(Arc::new(ScriptedSources {
                format_epoch,
                resumes,
                aborts: Arc::clone(&aborts),
            }));
            (directory, manager, aborts)
        }

        /// FakeSources whose last opened lane stays reachable, so a test can
        /// feed it what the device would.
        fn recording_with_a_lane() -> (
            TempDir,
            MeetingSessionManager,
            MeetingSessionId,
            Arc<Mutex<Option<PacketSink>>>,
        ) {
            let lane = Arc::new(Mutex::new(None));
            let (directory, manager) = manager_with(Arc::new(FakeSources {
                starts: Arc::new(AtomicUsize::new(0)),
                aborts: Arc::new(AtomicUsize::new(0)),
                unavailable: None,
                lane: Arc::clone(&lane),
            }));
            let live = recording(&manager);
            (directory, manager, live, lane)
        }

        fn open_gate(manager: &MeetingSessionManager) -> MeetingSessionSnapshot {
            block_on(manager.create_preflight_with_calendar(
                MeetingPreflightCreateRequest {
                    operation_id: MeetingOperationId::new(),
                    expected_revision: 0,
                    title: "Design review".to_string(),
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
                None,
            ))
            .unwrap()
            .snapshot
        }

        fn start_request(gate: &MeetingSessionSnapshot) -> MeetingStartRequest {
            MeetingStartRequest {
                operation_id: MeetingOperationId::new(),
                session_id: gate.session_id,
                expected_revision: gate.revision,
                consent: MeetingConsentInput {
                    policy_version: 1,
                    microphone_acknowledged: true,
                    system_audio_acknowledged: true,
                    known_missing_sources_acknowledged: Vec::new(),
                    degraded_start_policy: DegradedStartPolicy::AbortIfRequiredSourceFails,
                    destination: ProcessingDestination::Local,
                    remote_acknowledgement: None,
                },
            }
        }

        fn recording(manager: &MeetingSessionManager) -> MeetingSessionId {
            let started = block_on(manager.start(start_request(&open_gate(manager))))
                .unwrap()
                .snapshot;
            assert_eq!(started.phase, MeetingPhase::CapturingRecording);
            assert!(keeps_awake(manager), "a recording keeps the Mac awake");
            started.session_id
        }

        fn current(
            manager: &MeetingSessionManager,
            session_id: MeetingSessionId,
        ) -> MeetingSessionSnapshot {
            block_on(manager.store())
                .unwrap()
                .session_snapshot(session_id)
                .unwrap()
        }

        fn request(
            manager: &MeetingSessionManager,
            session_id: MeetingSessionId,
        ) -> MeetingMutationRequest {
            MeetingMutationRequest {
                operation_id: MeetingOperationId::new(),
                session_id,
                expected_revision: current(manager, session_id).revision,
            }
        }

        fn stop(
            manager: &MeetingSessionManager,
            session_id: MeetingSessionId,
        ) -> Result<MeetingMutationResult, MeetingCommandError> {
            block_on(manager.stop(
                request(manager, session_id),
                MeetingStopCause::Operator(MeetingStopSurface::MeetingLive),
            ))
        }

        fn keeps_awake(manager: &MeetingSessionManager) -> bool {
            manager
                .actor_lock()
                .active
                .as_ref()
                .is_some_and(|capture| capture.hold.keep_awake.is_held())
        }

        /// A second connection to the meeting database, as another process
        /// would hold one, for making the store's next write fail.
        fn beside_the_store(manager: &MeetingSessionManager, sql: &str) {
            let store = block_on(manager.store()).unwrap();
            let key = block_on(manager.secrets.meeting_storage_key()).unwrap();
            let connection = rusqlite::Connection::open(store.database_path()).unwrap();
            connection
                .execute_batch(&format!(
                    "PRAGMA key = \"x'{}'\";",
                    hex::encode(key.as_bytes())
                ))
                .unwrap();
            connection.execute_batch(sql).unwrap();
        }

        /// A gap whose offset the store cannot write, so the lane's writer
        /// fails on its next pass and its thread ends, as any write it cannot
        /// make ends it.
        fn kill_writer(lane: &Mutex<Option<PacketSink>>) {
            lane.lock()
                .unwrap()
                .as_mut()
                .expect("a lane was opened")
                .report_gap(SourceGap {
                    track_id: SourceTrackId::new(),
                    epoch: SourceEpoch::new(0),
                    start_offset_ns: None,
                    end_offset_ns: Some(u64::MAX),
                    reason: SourceGapReason::SourceStopped,
                    dropped_frames: None,
                });
        }

        fn transcribes_imports(manager: &MeetingSessionManager) {
            manager
                .processing
                .set_transcript_engine(Arc::new(TranscribingEngine));
            manager
                .processing
                .set_vad_factory(Arc::new(AlwaysVoiceFactory));
        }

        fn import_request(files: &TempDir) -> ImportRecordingRequest {
            let path = files.path().join("Earlier call.wav");
            write_mono_wav(&path, 16_000);
            ImportRecordingRequest {
                path,
                title: None,
                recorded_at_utc_ms: Some(1_700_000_000_000),
                origin: RecordingOrigin::LocalFile,
            }
        }

        /// Runs `work` beside the recording, then stops the recording.
        /// Returns whether `work` finished while the recording still ran, the
        /// stop, and what `work` returned once it could.
        fn beside_the_recording<T: Send>(
            manager: &MeetingSessionManager,
            live: MeetingSessionId,
            work: impl FnOnce() -> T + Send,
        ) -> (bool, Result<MeetingMutationResult, MeetingCommandError>, T) {
            thread::scope(|scope| {
                let (send, receive) = mpsc::channel();
                scope.spawn(move || send.send(work()));
                let early = receive.recv_timeout(UNPARKED_RUN);
                let finished_early = early.is_ok();
                let stopped = stop(manager, live);
                let finished = early
                    .or_else(|_| receive.recv())
                    .expect("the work finishes once the recording stops");
                (finished_early, stopped, finished)
            })
        }

        /// Deleting a finished meeting, or discarding one waiting in
        /// recovery, while another records leaves the recording's hold on the
        /// Mac. Both used to let go of it, so the display could sleep partway
        /// through the call.
        #[test]
        fn removing_another_meeting_keeps_the_recording_awake() {
            let (_directory, manager, _starts, _aborts) = manager();
            let finished = review_ready_session(&manager);
            let recovered = interrupted_session(
                &manager,
                MeetingPhase::RecoveryRequired,
                ProcessingDestination::Local,
            );
            let live = recording(&manager);

            let deleted = block_on(manager.delete(request(&manager, finished.session_id))).unwrap();
            let awake_after_delete = keeps_awake(&manager);
            let discarded = block_on(manager.discard(request(&manager, recovered))).unwrap();
            let awake_after_discard = keeps_awake(&manager);
            stop(&manager, live).unwrap();

            assert!(deleted.removed && discarded.removed);
            assert!(
                awake_after_delete,
                "deleting another meeting let the Mac sleep mid-recording"
            );
            assert!(
                awake_after_discard,
                "discarding another meeting let the Mac sleep mid-recording"
            );
            assert!(!keeps_awake(&manager), "the stop lets the Mac sleep");
        }

        /// The same removal leaves processing parked behind the recording. A
        /// run beside a live microphone competes with it for the machine, and
        /// discarding another meeting used to lift the gate for every run.
        #[test]
        fn removing_another_meeting_keeps_processing_parked_behind_the_recording() {
            let (files, manager, _starts, _aborts) = manager();
            transcribes_imports(&manager);
            let recovered = interrupted_session(
                &manager,
                MeetingPhase::RecoveryRequired,
                ProcessingDestination::Local,
            );
            let live = recording(&manager);
            block_on(manager.discard(request(&manager, recovered))).unwrap();
            let import = import_request(&files);

            let (ran_beside_the_recording, stopped, imported) =
                beside_the_recording(&manager, live, || {
                    block_on(manager.import_recording(import))
                });

            assert!(
                !ran_beside_the_recording,
                "discarding another meeting let processing run beside the recording"
            );
            stopped.unwrap();
            imported.expect("the import lands once the recording stops");
        }

        /// Finishing a meeting from recovery while another records starts its
        /// run behind the recording. It used to lift the gate for every run.
        #[test]
        fn finishing_a_recovered_meeting_waits_for_the_recording() {
            let (_directory, manager, _starts, _aborts) = manager();
            manager
                .processing
                .set_transcript_engine(Arc::new(ReadyEngine));
            let recovered = interrupted_session(
                &manager,
                MeetingPhase::RecoveryRequired,
                ProcessingDestination::Local,
            );
            let live = recording(&manager);
            let finish = request(&manager, recovered);

            let (ran_beside_the_recording, stopped, finished) =
                beside_the_recording(&manager, live, || {
                    block_on(manager.recovery_finalize(finish))
                });

            assert!(
                !ran_beside_the_recording,
                "finishing a recovered meeting let its run start beside the recording"
            );
            stopped.unwrap();
            finished.unwrap();
            assert_eq!(
                current(&manager, recovered).phase,
                MeetingPhase::ReviewReady
            );
        }

        /// Notes wait for the recording as transcription does. A transcript
        /// import has no audio to transcribe, so nothing held its run and its
        /// notes were written beside the live recording. Now the run says it
        /// is waiting, and writes the notes once the recording stops.
        #[test]
        fn notes_wait_for_the_recording_and_are_written_after_it() {
            let (files, manager, _starts, _aborts) = manager();
            manager
                .processing
                .set_text_generators(Arc::new(NotesAndAnswers), no_engine());
            let path = files.path().join("otter_export.txt");
            std::fs::write(&path, include_str!("fixtures/otter_export.txt")).unwrap();
            let live = recording(&manager);

            let (ran_beside_the_recording, stopped, imported) =
                beside_the_recording(&manager, live, || block_on(manager.import_transcript(path)));

            assert!(
                !ran_beside_the_recording,
                "the notes were written beside the recording"
            );
            stopped.unwrap();
            let imported = imported.expect("the transcript imports once the recording stops");
            let review = block_on(manager.get(imported.session_id)).unwrap();
            assert!(
                review
                    .artifacts
                    .iter()
                    .any(|artifact| artifact.state == MeetingArtifactState::Current),
                "the notes are written once the recording stops"
            );
            assert!(
                manager
                    .processing
                    .published_progress()
                    .iter()
                    .filter(|payload| payload.session_id == imported.session_id)
                    .filter_map(|payload| payload.progress.as_ref())
                    .any(|progress| progress.waiting_for_capture
                        && progress.stage == ProcessingStage::Notes),
                "the run never said it was waiting for the recording"
            );
        }

        /// Notes for the meeting the person just left hold their next model
        /// request for one that starts while they are being written. They
        /// used to check for a recording only before their first request, so
        /// a local model kept working beside the whole recording.
        #[test]
        fn notes_under_way_pause_for_a_recording_that_starts() {
            struct FirstRequestWaits {
                calls: AtomicUsize,
                started: Mutex<Option<mpsc::Sender<()>>>,
                resume: Mutex<mpsc::Receiver<()>>,
            }

            impl crate::meeting::processing::MeetingTextGenerator for FirstRequestWaits {
                fn is_available(&self) -> bool {
                    true
                }

                fn model_id(&self) -> &'static str {
                    "first-request-waits"
                }

                fn model_version(&self) -> std::borrow::Cow<'static, str> {
                    std::borrow::Cow::Borrowed("first-request-waits-v1")
                }

                fn max_input_bytes(&self) -> usize {
                    usize::MAX
                }

                fn generate(
                    &self,
                    system_prompt: &str,
                    evidence: &str,
                    max_tokens: i32,
                    shape: ReplyShape,
                ) -> Result<String, MeetingTextGenerationError> {
                    self.calls.fetch_add(1, Ordering::SeqCst);
                    let first = self.started.lock().unwrap().take();
                    if let Some(started) = first {
                        started.send(()).unwrap();
                        self.resume.lock().unwrap().recv().unwrap();
                    }
                    crate::meeting::processing::MeetingTextGenerator::generate(
                        &NotesAndAnswers,
                        system_prompt,
                        evidence,
                        max_tokens,
                        shape,
                    )
                }
            }

            let (files, manager, _starts, _aborts) = manager();
            let (started, first_request) = mpsc::channel();
            let (resume, resumed) = mpsc::channel();
            let generator = Arc::new(FirstRequestWaits {
                calls: AtomicUsize::new(0),
                started: Mutex::new(Some(started)),
                resume: Mutex::new(resumed),
            });
            manager
                .processing
                .set_text_generators(Arc::clone(&generator) as _, no_engine());
            let path = files.path().join("otter_export.txt");
            std::fs::write(&path, include_str!("fixtures/otter_export.txt")).unwrap();

            let (during, imported) = thread::scope(|scope| {
                let import = scope.spawn(|| block_on(manager.import_transcript(path)));
                first_request.recv().expect("notes reach the model");
                let recording = manager.processing.park_for_capture();
                resume.send(()).unwrap();
                thread::sleep(UNPARKED_RUN);
                let during = generator.calls.load(Ordering::SeqCst);
                drop(recording);
                (during, import.join().unwrap())
            });

            imported.expect("the transcript imports once the recording stops");
            assert_eq!(
                during, 1,
                "notes asked the model again beside the recording"
            );
            assert!(
                generator.calls.load(Ordering::SeqCst) > 1,
                "notes go on once the recording stops"
            );
        }

        /// A synced meeting deleted while another records still queues the
        /// removal of its cloud copy. The delete used to skip it whenever any
        /// capture ran, and nothing queues it later, so the copy stayed in the
        /// cloud and on every other device.
        #[test]
        fn deleting_a_synced_meeting_during_a_recording_still_removes_its_cloud_copy() {
            let (_directory, manager, _starts, _aborts) = manager();
            let finished = review_ready_session(&manager);
            let store = block_on(manager.store()).unwrap();
            store
                .upsert_cloud_head(&CloudHead {
                    object_id: "meeting-deleted-during-capture".to_string(),
                    source_session_id: Some(finished.session_id),
                    remote_revision_id: Some("revision-1".to_string()),
                    tombstone: false,
                    acknowledged_revision_id: Some("revision-1".to_string()),
                    change_sequence: 1,
                })
                .unwrap();
            let live = recording(&manager);

            let deleted = block_on(manager.delete(request(&manager, finished.session_id))).unwrap();
            let outbox = store
                .cloud_outboxes_for_session(finished.session_id)
                .unwrap();
            stop(&manager, live).unwrap();

            assert!(deleted.removed);
            assert!(
                outbox
                    .iter()
                    .any(|row| row.kind == CloudOutboxKind::Tombstone),
                "the deletion never reaches the cloud"
            );
        }

        /// A recording whose lane writer died partway still stops: what
        /// reached the disk is sealed and handed to processing, and the Mac
        /// may sleep again. The stop used to return at the dead writer,
        /// leaving the meeting at stopping with no notes until the next
        /// launch.
        #[test]
        fn a_recording_whose_writer_died_still_stops() {
            let (_directory, manager, live, lane) = recording_with_a_lane();
            kill_writer(&lane);

            let stopped = stop(&manager, live);

            let phase = stopped.expect("the stop goes through").snapshot.phase;
            assert!(
                matches!(phase, MeetingPhase::Processing | MeetingPhase::ReviewReady),
                "stopped at {phase:?}"
            );
            assert!(!keeps_awake(&manager));
            assert!(!manager.is_capture_active());
        }

        /// The same dead writer does not keep a discard from finishing. It
        /// used to leave the meeting at deleting, listed and never removed
        /// until the next launch.
        #[test]
        fn a_recording_whose_writer_died_can_still_be_discarded() {
            let (_directory, manager, live, lane) = recording_with_a_lane();
            kill_writer(&lane);

            let discarded = block_on(manager.discard(request(&manager, live)));

            assert!(discarded.expect("the discard goes through").removed);
            assert!(matches!(
                block_on(manager.store()).unwrap().session_snapshot(live),
                Err(StoreError::NotFound)
            ));
            assert!(!manager.is_capture_active());
        }

        /// A start whose lanes opened but whose tracks could not be written
        /// goes back to its gate with nothing left running: every lane closed,
        /// the Mac free to sleep, processing no longer parked. It used to stay
        /// at starting, which no command leaves, with all of them held.
        #[test]
        fn a_start_that_fails_after_its_lanes_open_leaves_nothing_running() {
            let (files, manager, aborts) = scripted(u64::MAX, false);
            transcribes_imports(&manager);
            let gate = open_gate(&manager);

            let started = block_on(manager.start(start_request(&gate)));

            let after = current(&manager, gate.session_id);
            assert!(started.is_err());
            assert_eq!(
                after.phase,
                MeetingPhase::Preflight,
                "the gate is offered again"
            );
            assert_eq!(aborts.load(Ordering::Acquire), SourceKind::ALL.len());
            assert!(!keeps_awake(&manager));
            assert!(!manager.is_capture_active());
            let manager = Arc::new(manager);
            let importer = Arc::clone(&manager);
            let import = import_request(&files);
            let (send, receive) = mpsc::channel();
            thread::spawn(move || send.send(block_on(importer.import_recording(import))));
            assert!(
                matches!(receive.recv_timeout(Duration::from_secs(30)), Ok(Ok(_))),
                "processing stayed parked behind a capture that never began"
            );
        }

        /// A pause whose bookkeeping cannot be written leaves the meeting
        /// recording, where Pause and Stop both work. It used to stay at
        /// pausing, which only Stop leaves, still holding the Mac awake.
        #[test]
        fn a_pause_that_cannot_be_written_leaves_the_meeting_recording() {
            let (_directory, manager, _aborts) = scripted(0, true);
            let live = recording(&manager);
            beside_the_store(
                &manager,
                "CREATE TRIGGER refuse_close BEFORE UPDATE ON meeting_capture_windows
                 BEGIN SELECT RAISE(ABORT, 'refused'); END;",
            );

            let refused = block_on(manager.pause(request(&manager, live)));
            let after = current(&manager, live);
            let awake = keeps_awake(&manager);
            beside_the_store(&manager, "DROP TRIGGER refuse_close;");
            let paused = block_on(manager.pause(request(&manager, live)));
            let stopped = stop(&manager, live);

            assert!(refused.is_err());
            assert_eq!(after.phase, MeetingPhase::CapturingRecording);
            assert!(awake, "a recording that did not pause keeps the Mac awake");
            assert_eq!(
                paused.unwrap().snapshot.phase,
                MeetingPhase::CapturingPaused
            );
            stopped.unwrap();
        }

        /// A resume whose bookkeeping cannot be written leaves the meeting
        /// paused, with its lanes paused again and the Mac free to sleep. It
        /// used to stay at resuming, which no Resume leaves, with the lanes
        /// running outside any capture window.
        #[test]
        fn a_resume_that_cannot_be_written_leaves_the_meeting_paused() {
            let (_directory, manager, _aborts) = scripted(0, true);
            let live = recording(&manager);
            let paused = block_on(manager.pause(request(&manager, live))).unwrap();
            assert_eq!(paused.snapshot.phase, MeetingPhase::CapturingPaused);
            beside_the_store(
                &manager,
                "CREATE TRIGGER refuse_open BEFORE INSERT ON meeting_capture_windows
                 BEGIN SELECT RAISE(ABORT, 'refused'); END;",
            );

            let refused = block_on(manager.resume(request(&manager, live)));
            let after = current(&manager, live);
            let awake = keeps_awake(&manager);
            beside_the_store(&manager, "DROP TRIGGER refuse_open;");
            let resumed = block_on(manager.resume(request(&manager, live)));
            let stopped = stop(&manager, live);

            assert!(refused.is_err());
            assert_eq!(after.phase, MeetingPhase::CapturingPaused);
            assert!(!awake, "a paused meeting lets the Mac sleep");
            assert_eq!(
                resumed.unwrap().snapshot.phase,
                MeetingPhase::CapturingRecording
            );
            stopped.unwrap();
        }

        /// The session aborts a lane whose resume fails, and an aborted device
        /// cannot resume. Once no lane can, Resume leaves the meeting's
        /// actions instead of staying on offer as a press that does nothing,
        /// and Stop keeps what was recorded.
        #[test]
        fn a_meeting_no_lane_can_resume_offers_stop_and_not_resume() {
            let (_directory, manager, _starts, _aborts) = manager();
            let live = recording(&manager);
            block_on(manager.pause(request(&manager, live))).unwrap();

            let failed = block_on(manager.resume(request(&manager, live)))
                .unwrap()
                .snapshot;
            let stopped = stop(&manager, live);

            assert_eq!(failed.phase, MeetingPhase::CapturingPaused);
            assert!(
                !failed
                    .allowed_actions
                    .contains(&AllowedMeetingAction::Resume),
                "Resume is offered with no lane left that can resume"
            );
            assert!(failed.allowed_actions.contains(&AllowedMeetingAction::Stop));
            stopped.unwrap();
        }

        /// Startup recovery is for what an earlier launch left behind. Run
        /// after this launch opened a gate and started a recording, it used
        /// to delete the gate and move the live recording into recovery while
        /// its lanes kept writing.
        #[test]
        fn recovery_after_this_launch_began_leaves_its_meetings_alone() {
            let (_directory, manager, _starts, _aborts) = manager();
            let gate = open_gate(&manager);
            let live = recording(&manager);

            let recovered = block_on(manager.recover_at_startup_at(0)).unwrap();
            let gate_after = block_on(manager.store())
                .unwrap()
                .session_snapshot(gate.session_id);
            let live_after = current(&manager, live);
            let stopped = stop(&manager, live);

            assert!(recovered.is_empty());
            assert_eq!(
                gate_after.expect("the gate is still there").phase,
                MeetingPhase::Preflight
            );
            assert_eq!(live_after.phase, MeetingPhase::CapturingRecording);
            stopped.unwrap();
        }

        /// A launch whose startup recovery could not open the store recovers
        /// at the first mount that can. The later mount used to skip it:
        /// meetings an earlier launch left read as processing all launch, and
        /// cloud work stayed refused.
        #[test]
        fn a_mount_after_a_failed_startup_recovery_still_recovers() {
            let (_directory, manager, _backend) = mounted_manager();
            let interrupted = interrupted_session(
                &earlier_launch(&manager),
                MeetingPhase::Processing,
                ProcessingDestination::Local,
            );
            manager.secrets.set_migration_pending(true);
            let refused = block_on(manager.recover_at_startup());
            manager.secrets.set_migration_pending(false);

            let store = block_on(manager.store()).unwrap();

            assert!(refused.is_err(), "no key while a migration is pending");
            assert_eq!(
                store.session_snapshot(interrupted).unwrap().phase,
                MeetingPhase::RecoveryRequired
            );
            assert!(block_on(manager.cloud_store()).is_ok());
        }
    }
}

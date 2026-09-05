use super::ledger::MeetingLedger;
use crate::analytics::DashboardTrendRange;
use serde::{Deserialize, Serialize};
use specta::Type;
use uuid::Uuid;

macro_rules! meeting_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, Type)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub const fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }

            pub const fn uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

meeting_id!(MeetingSessionId);
meeting_id!(MeetingPlanId);
meeting_id!(ConsentId);
meeting_id!(SourceTrackId);
meeting_id!(TranscriptRevisionId);
meeting_id!(TranscriptSegmentId);
meeting_id!(SpeakerId);
meeting_id!(ManualNoteId);
meeting_id!(MeetingSuggestionId);
meeting_id!(MeetingOperationId);
meeting_id!(MeetingDeletionJobId);
meeting_id!(MeetingExportReceiptId);
meeting_id!(MeetingQuestionId);
meeting_id!(MeetingArtifactId);
meeting_id!(MeetingDiarizationGenerationId);
meeting_id!(SavedPromptId);
meeting_id!(PromptRunId);

/// The title a recording with no calendar event and no recognised app gets
/// before anything has been read out of it. One constant because two places
/// mint it and a third has to recognise it: a derived title may only replace
/// this exact string, never a title a person typed.
pub const MANUAL_DEFAULT_TITLE: &str = "Local notes";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, Type)]
#[serde(transparent)]
pub struct SourceEpoch(pub u64);

impl SourceEpoch {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingPhase {
    Preflight,
    Starting,
    CapturingRecording,
    CapturingPausing,
    CapturingPaused,
    CapturingResuming,
    Stopping,
    Processing,
    ReviewReady,
    RecoveryRequired,
    Deleting,
}

impl MeetingPhase {
    pub const fn capture_mode(self) -> Option<CaptureMode> {
        match self {
            Self::CapturingRecording => Some(CaptureMode::Recording),
            Self::CapturingPausing => Some(CaptureMode::Pausing),
            Self::CapturingPaused => Some(CaptureMode::Paused),
            Self::CapturingResuming => Some(CaptureMode::Resuming),
            _ => None,
        }
    }

    pub const fn retains_capture_lease(self) -> bool {
        matches!(
            self,
            Self::Starting
                | Self::CapturingRecording
                | Self::CapturingPausing
                | Self::CapturingPaused
                | Self::CapturingResuming
                | Self::Stopping
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    Recording,
    Pausing,
    Paused,
    Resuming,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Microphone,
    SystemAudio,
}

impl SourceKind {
    pub const ALL: [Self; 2] = [Self::Microphone, Self::SystemAudio];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Microphone => "microphone",
            Self::SystemAudio => "system_audio",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SourceAvailability {
    Available,
    PermissionRequired,
    PermissionDenied,
    DeviceUnavailable,
    UnsupportedPlatform,
    StorageUnavailable,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SourceHealth {
    NotStarted,
    Starting,
    Healthy,
    Degraded,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SourceProbeDetail {
    Permission,
    Device,
    Platform,
    Stream,
    Route,
    Storage,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SourceGapReason {
    SourceUnavailable,
    SourceStartFailed,
    PermissionLost,
    Paused,
    PacketDropped,
    WriterPressure,
    StorageFailure,
    TimestampMissing,
    TimestampDiscontinuity,
    InvalidFormat,
    SourceStopped,
    CorruptRecord,
    MissingRecord,
    RecoveryTail,
    SystemSleep,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingFailure {
    LocalModelUnavailable,
    RemoteUnavailable,
    EngineFailure,
    Cancelled,
    /// The launch that was recording or processing this meeting ended before
    /// the work finished. Written by startup recovery, never by a run: it is
    /// the terminal status of an attempt nobody is making any more, which is
    /// what keeps an abandoned meeting out of the Processing filter.
    Interrupted,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProcessingStatus {
    Pending,
    Running,
    Succeeded,
    Failed { reason: ProcessingFailure },
    Cancelled,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum DiarizationStatus {
    NotRequested,
    ModelUnavailable,
    Downloading,
    Running,
    Succeeded,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingDiarizationSnapshot {
    pub status: DiarizationStatus,
    pub model_id: String,
    pub model_version: String,
    pub generation_id: Option<MeetingDiarizationGenerationId>,
    pub assigned_segment_count: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CaptureCompleteness {
    NotStarted,
    Complete,
    Partial,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum DegradedStartPolicy {
    AbortIfRequiredSourceFails,
    ContinueAndMarkPartial,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProcessingDestination {
    Local,
    Remote { destination_id: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct RemoteAcknowledgement {
    pub destination_id: String,
    pub policy_version: u32,
    pub acknowledged_at_utc_ms: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum DeletionCause {
    Discard,
    User,
    Retention,
    Recovery,
}

/// One deleted meeting that can still be brought back.
///
/// Not a stored shape: `expires_at_utc_ms` is derived from the deletion instant
/// and the app's trash horizon, so the list and the sweep cannot disagree about
/// when an undo runs out.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MeetingTrashEntry {
    /// The deletion job, which is the handle a restore is asked for by. There is
    /// no session id here on purpose: the session row is gone, and a restore is
    /// an undo of one deletion rather than an operation on a meeting.
    pub job_id: MeetingDeletionJobId,
    pub title: String,
    pub deleted_at_utc_ms: i64,
    pub expires_at_utc_ms: i64,
}

/// What one recording's disclosure — the line the consent panel offers to post
/// in the meeting's own chat — is doing.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MeetingSessionDisclosure {
    /// Nobody asked this meeting to announce itself, which is the default.
    NotAsked,
    /// Asked for and not posted yet. `notetaker` is the name the room is told
    /// the notes are for: the calendar account's own attendee entry, which is
    /// the only place this app learns its operator's name.
    ///
    /// ponytail: falls back to the meeting's title when the calendar names
    /// nobody, so the one sentence always has something to interpolate. The
    /// upgrade path is an account name in settings, not a second phrasing.
    Pending { notetaker: String },
    /// Posted, or refused. Delivery's own receipt says which: a target that
    /// cannot accept an insertion is `definitely_not_dispatched`, and that is
    /// the case the live surface mentions.
    Attempted {
        receipt: crate::delivery::DeliveryReceipt,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingOrigin {
    Manual,
    Suggestion,
    Cli,
    /// A recording or transcript brought in from a file rather than captured.
    Import,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum StorageAvailability {
    Available,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct MeetingRetentionSetRequest {
    pub operation_id: MeetingOperationId,
    pub expected_revision: u64,
    pub policy: MeetingRetentionPolicy,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingCommandError {
    ConsentRequired,
    ConsentStale,
    InvalidTransition,
    StaleRevision,
    CaptureLeaseBusy,
    NoSourceStarted,
    SourceUnavailable,
    StorageUnavailable,
    RecoveryRequired,
    DeletionInProgress,
    NotFound,
    InvalidRequest,
    ExportCancelled,
    ExportFailed,
    LocalModelUnavailable,
    LocalEvidenceUnavailable,
    InsufficientEnrollmentEvidence,
    ProfileModelIncompatible,
    ProfileMergeResolutionRequired,
    RemoteUnavailable,
    EngineFailure,
    /// The file offered for import is not a recording or transcript export
    /// Sona can read. Every refusal reaching the operator says this, because
    /// they all have the same answer: choose a different file.
    ImportUnreadable,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingCaptureError {
    Unavailable,
    PermissionDenied,
    InvalidFormat,
    StreamFailure,
    InvalidState,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PacketPushResult {
    Accepted,
    Dropped { frames: u32 },
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PacketDiscontinuityFlags {
    pub timestamp_reset: bool,
    pub route_changed: bool,
    pub source_restarted: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct AudioFormat {
    pub sample_rate_hz: u32,
    pub channels: u16,
}

impl AudioFormat {
    pub fn checked_frame_samples(self, frame_count: u32) -> Option<usize> {
        usize::try_from(frame_count)
            .ok()?
            .checked_mul(usize::from(self.channels))
    }

    pub fn bytes_per_second(self) -> Option<u64> {
        u64::from(self.sample_rate_hz)
            .checked_mul(u64::from(self.channels))
            .and_then(|samples| {
                samples.checked_mul(u64::try_from(std::mem::size_of::<f32>()).ok()?)
            })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct SessionClockAnchor {
    pub host_monotonic_anchor_ns: u64,
    pub wall_start_utc_ms: i64,
    pub clock_policy_version: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct TimestampBridge {
    /// Native timestamp units are deliberately retained with their source
    /// timescale. Arrival time never participates in timeline reconstruction.
    pub native_anchor_value: i64,
    pub native_timescale: u32,
    pub host_monotonic_anchor_ns: u64,
    pub session_offset_ns: u64,
}

impl TimestampBridge {
    pub fn map_native(self, native_timestamp_value: i64, native_timescale: u32) -> Option<u64> {
        if native_timescale == 0 || native_timescale != self.native_timescale {
            return None;
        }
        let delta = native_timestamp_value.checked_sub(self.native_anchor_value)?;
        let delta_ns = u64::try_from(delta)
            .ok()?
            .checked_mul(1_000_000_000)?
            .checked_div(u64::from(native_timescale))?;
        self.session_offset_ns.checked_add(delta_ns)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct SourceClockEpoch {
    pub track_id: SourceTrackId,
    pub epoch: SourceEpoch,
    pub format_epoch: u64,
    pub bridge: TimestampBridge,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct CapturedPacket {
    pub track_id: SourceTrackId,
    pub source_epoch: SourceEpoch,
    pub format_epoch: u64,
    pub sequence: u64,
    pub native_timestamp_value: Option<i64>,
    pub native_timestamp_timescale: Option<u32>,
    /// A platform host-clock sample from the callback, never callback arrival time.
    pub host_monotonic_anchor_ns: Option<u64>,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub frame_count: u32,
    pub discontinuity_flags: PacketDiscontinuityFlags,
}

impl CapturedPacket {
    pub fn format(self) -> AudioFormat {
        AudioFormat {
            sample_rate_hz: self.sample_rate_hz,
            channels: self.channels,
        }
    }

    pub fn native_timestamp(self) -> Option<(i64, u32)> {
        Some((
            self.native_timestamp_value?,
            self.native_timestamp_timescale
                .filter(|timescale| *timescale > 0)?,
        ))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct SourceProbe {
    pub source_kind: SourceKind,
    pub availability: SourceAvailability,
    pub health: SourceHealth,
    pub detail: Option<SourceProbeDetail>,
    pub negotiated_format: Option<AudioFormat>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct SourceStartPlan {
    pub session_id: MeetingSessionId,
    pub track_id: SourceTrackId,
    pub source_kind: SourceKind,
    pub required: bool,
    pub frozen_application_bundle_ids: Vec<String>,
    pub source_epoch: SourceEpoch,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct SourceStartReport {
    pub track_id: SourceTrackId,
    pub source_kind: SourceKind,
    pub format: AudioFormat,
    pub epoch: SourceEpoch,
    pub format_epoch: u64,
    pub timestamp_bridge: TimestampBridge,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct SourceGap {
    pub track_id: SourceTrackId,
    pub epoch: SourceEpoch,
    pub start_offset_ns: Option<u64>,
    pub end_offset_ns: Option<u64>,
    pub reason: SourceGapReason,
    pub dropped_frames: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct SourceStopReport {
    pub track_id: SourceTrackId,
    pub final_offset_ns: Option<u64>,
    pub health: SourceHealth,
    /// Source-local summary for diagnostics. Each gap is already published
    /// through the packet sink, which is the sole persistence path.
    pub observed_gaps: Vec<SourceGap>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingStoragePlan {
    pub format_version: u32,
    pub record_max_payload_bytes: u32,
    pub checkpoint_interval_ms: u32,
    pub source_lane_sample_capacity: u32,
    pub source_lane_descriptor_capacity: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingRunPlan {
    pub plan_id: MeetingPlanId,
    pub session_id: MeetingSessionId,
    pub consent_id: ConsentId,
    pub attempt_number: u32,
    pub schema_version: u32,
    pub app_build: String,
    pub preflight_revision: u64,
    pub requested_sources: Vec<SourceKind>,
    pub required_sources: Vec<SourceKind>,
    pub accepted_known_missing_sources: Vec<SourceKind>,
    pub degraded_start_policy: DegradedStartPolicy,
    pub microphone_device_uid: Option<String>,
    pub frozen_system_audio_application_bundle_ids: Vec<String>,
    pub session_clock_anchor: SessionClockAnchor,
    pub storage: MeetingStoragePlan,
    pub language: String,
    pub asr_model_id: Option<String>,
    pub asr_model_version: Option<String>,
    pub diarization_model_id: Option<String>,
    pub diarization_model_version: Option<String>,
    pub destination: ProcessingDestination,
    pub remote_acknowledgement: Option<RemoteAcknowledgement>,
    pub retention_policy: MeetingRetentionPolicy,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MeetingConsentProvenance {
    #[default]
    Direct,
    StandingSeries {
        series_key: String,
        granted_at_utc_ms: i64,
    },
    /// A standing per-application grant: the operator switched "Record calls
    /// automatically" on for this bundle ID. Unlike the series grant, which is
    /// a row this store owns and revalidates inside the start transaction, this
    /// one lives in application settings — so the receipt records which grant
    /// was cited and the session layer re-reads the setting immediately before
    /// it starts. Detection still forges nothing.
    StandingApp { bundle_id: String },
    /// A recording a paired device made and this Mac pulled from the vault.
    /// Nobody on this Mac acknowledged anything: the device's operator did,
    /// when they recorded and uploaded it, so the row names the device.
    PairedDevice { device_id: String },
}

/// The consent policy every acknowledgement on this machine is stamped with.
///
/// Every consent row a frontend surface produces echoes the number the
/// frontend sent (`MEETING_CONSENT_POLICY_VERSION` in MeetingStartGate.tsx),
/// because that acknowledgement is a click on a surface the frontend drew.
/// Two rows have no frontend in the loop — an import, where choosing the
/// file is the acknowledgement, and a standing per-application grant,
/// acknowledged in settings and spent later — so the backend needs the same
/// number in its own hand. This is its only Rust copy; it and the frontend's
/// must move together.
pub const MEETING_CONSENT_POLICY_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingConsent {
    pub consent_id: ConsentId,
    pub session_id: MeetingSessionId,
    pub attempt_number: u32,
    pub preflight_revision: u64,
    pub policy_version: u32,
    pub acknowledged_at_utc_ms: i64,
    /// How this attempt was authorized. Old direct-consent rows deserialize as
    /// `Direct`; standing grants are cited by identity and revalidated inside
    /// the same transaction that writes this immutable receipt.
    #[serde(default)]
    pub provenance: MeetingConsentProvenance,
    pub microphone_acknowledged: bool,
    pub system_audio_acknowledged: bool,
    pub known_missing_sources_acknowledged: Vec<SourceKind>,
    pub degraded_start_policy: DegradedStartPolicy,
    pub destination: ProcessingDestination,
    pub remote_acknowledgement: Option<RemoteAcknowledgement>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MeetingRetentionPolicy {
    Forever,
    DeleteAfterDays { days: u32 },
}

impl MeetingRetentionPolicy {
    pub fn delete_after_utc_ms(&self, from_utc_ms: i64) -> Option<i64> {
        match self {
            Self::Forever => None,
            Self::DeleteAfterDays { days } => i64::from(*days)
                .checked_mul(86_400_000)
                .and_then(|duration| from_utc_ms.checked_add(duration)),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingRetentionSnapshot {
    pub policy: MeetingRetentionPolicy,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingReasonCode {
    ConsentMissing,
    ConsentStale,
    StaleRevision,
    CaptureLeaseBusy,
    SourceUnavailable,
    SourceStartFailed,
    SourceGap,
    StorageUnavailable,
    StorageFailure,
    LocalModelUnavailable,
    RecoveryRequired,
    Deleted,
    InvalidTransition,
    DuplicateOperation,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum OperationResult {
    Committed,
    Rejected,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingCommandKind {
    PreflightCreate,
    PreflightRefresh,
    PreflightCancel,
    Start,
    Pause,
    Resume,
    Stop,
    Discard,
    RecoveryFinalize,
    TitleSet,
    SpeakerRename,
    SpeakerMerge,
    SpeakerIdentify,
    SegmentEdit,
    NoteCreate,
    NoteUpdate,
    NoteDelete,
    ArtifactsRegenerate,
    QuestionAsk,
    QuestionForget,
    Export,
    Delete,
    RetentionSet,
    RemoteCancel,
    LoopResolve,
    LoopReopen,
    LoopAssign,
    LoopCarry,
    SeriesTemplateSet,
    SeriesDigestSet,
    SeriesAlwaysRecordSet,
    FollowUpDraft,
    SeriesAutomationSet,
    SeriesRemoteOptOutSet,
    SavedPromptSave,
    SavedPromptDelete,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct OperationReceipt {
    pub schema_version: u32,
    pub operation_id: MeetingOperationId,
    pub session_id: Option<MeetingSessionId>,
    pub actor: OperationActor,
    pub command: MeetingCommandKind,
    pub expected_revision: u64,
    pub from_phase: Option<MeetingPhase>,
    pub to_phase: Option<MeetingPhase>,
    pub requested_at_utc_ms: i64,
    pub committed_at_utc_ms: Option<i64>,
    pub result: OperationResult,
    pub reason_codes: Vec<MeetingReasonCode>,
    pub new_revision: Option<u64>,
    pub effect_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingRetentionMutationResult {
    pub receipt: OperationReceipt,
    pub snapshot: MeetingRetentionSnapshot,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum OperationActor {
    User,
    System,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AllowedMeetingAction {
    RefreshPreflight,
    CancelPreflight,
    Start,
    Pause,
    Resume,
    Stop,
    Discard,
    FinalizePartial,
    Edit,
    Regenerate,
    AskQuestion,
    Export,
    Delete,
    CancelRemote,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingSourceSnapshot {
    pub track_id: Option<SourceTrackId>,
    pub source_kind: SourceKind,
    pub required: bool,
    pub availability: SourceAvailability,
    pub health: SourceHealth,
    pub format: Option<AudioFormat>,
    pub last_durable_offset_ns: Option<u64>,
    pub gap_count: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingPreflightSnapshot {
    pub session_id: MeetingSessionId,
    pub revision: u64,
    pub proposed_title: String,
    pub origin: MeetingOrigin,
    pub sources: Vec<MeetingSourceSnapshot>,
    pub storage: StorageAvailability,
    pub local_processing: SourceAvailability,
    pub destination: ProcessingDestination,
    pub microphone_device_uid: Option<String>,
    pub frozen_system_audio_application_bundle_ids: Vec<String>,
    pub accepted_known_missing_sources: Vec<SourceKind>,
    pub degraded_start_policy: DegradedStartPolicy,
    pub required_acknowledgements: Vec<SourceKind>,
    pub allowed_actions: Vec<AllowedMeetingAction>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingSessionSnapshot {
    pub session_id: MeetingSessionId,
    pub phase: MeetingPhase,
    pub revision: u64,
    pub title: String,
    pub started_at_utc_ms: Option<i64>,
    pub elapsed_offset_ns: Option<u64>,
    pub sources: Vec<MeetingSourceSnapshot>,
    pub open_capture_window_started_at_ns: Option<u64>,
    pub capture_completeness: CaptureCompleteness,
    pub storage: StorageAvailability,
    pub processing_status: ProcessingStatus,
    /// The model-installation readiness captured by the current preflight.
    /// Post-capture processing has its own lifecycle in `processing_status`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preflight_local_processing: Option<SourceAvailability>,
    pub retention_deadline_utc_ms: Option<i64>,
    pub allowed_actions: Vec<AllowedMeetingAction>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingTrackSnapshot {
    pub track_id: SourceTrackId,
    pub source_kind: SourceKind,
    pub format: Option<AudioFormat>,
    pub first_offset_ns: Option<u64>,
    pub last_offset_ns: Option<u64>,
    pub durable_record_count: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingSpeaker {
    pub speaker_id: SpeakerId,
    pub session_id: MeetingSessionId,
    pub source_kind: SourceKind,
    pub display_name: String,
    pub revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct TranscriptSegment {
    pub segment_id: TranscriptSegmentId,
    pub transcript_revision_id: TranscriptRevisionId,
    pub track_id: SourceTrackId,
    pub ordinal: u64,
    pub start_offset_ns: u64,
    pub end_offset_ns: u64,
    pub speaker_id: SpeakerId,
    pub text: String,
    pub confidence_milli: Option<u16>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SpeakerAssignmentKind {
    LocalSpeaker,
    SystemSpeaker,
    Unknown,
    Overlap,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct EffectiveTranscriptSegment {
    pub base: TranscriptSegment,
    pub replacement_text: Option<String>,
    pub removed: bool,
    pub edit_revision: Option<u64>,
    pub assigned_speaker_id: SpeakerId,
    pub speaker_assignment: SpeakerAssignmentKind,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct ManualNote {
    pub note_id: ManualNoteId,
    pub session_id: MeetingSessionId,
    pub start_offset_ns: Option<u64>,
    pub end_offset_ns: Option<u64>,
    pub body: String,
    pub revision: u64,
    pub created_at_utc_ms: i64,
    pub updated_at_utc_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingReviewSnapshot {
    pub session: MeetingSessionSnapshot,
    pub tracks: Vec<MeetingTrackSnapshot>,
    pub gaps: Vec<SourceGap>,
    pub speakers: Vec<MeetingSpeaker>,
    pub transcript: Vec<EffectiveTranscriptSegment>,
    pub notes: Vec<ManualNote>,
    pub artifacts: Vec<MeetingArtifactRevision>,
    pub questions: Vec<MeetingAnswer>,
    pub diarization: MeetingDiarizationSnapshot,
    pub can_export: bool,
    pub remote_cancellation_pending: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum HistoryItemKind {
    Meeting,
}

/// Which of the three real sources line two of a meetings-list row came from.
/// The row states this on itself, so a reader can tell the news a model wrote
/// from a count the store measured, and never has to guess which one they are
/// looking at.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MeetingHistoryHeadline {
    /// No generated prose and no transcript: there is nothing true to say yet.
    #[default]
    None,
    /// `MeetingLedger::headline` from the current artifact revision.
    Ledger { text: String },
    /// First sentence of the current revision's generated notes summary.
    Summary { text: String },
    /// A transcript exists and prose does not, so the row reports how much was
    /// said instead of nothing.
    Words { words: u32 },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingHistorySummary {
    pub kind: HistoryItemKind,
    pub session_id: MeetingSessionId,
    pub title: String,
    pub phase: MeetingPhase,
    pub created_at_utc_ms: i64,
    pub capture_completeness: CaptureCompleteness,
    pub processing_status: ProcessingStatus,
    /// Capture time this meeting actually recorded, pauses excluded, summed
    /// over its closed capture windows. `None` until one window has closed,
    /// which is the state a session abandoned mid-capture is really in.
    #[serde(default)]
    pub recorded_duration_ms: Option<i64>,
    /// Sources that really opened a track, in `SourceKind::ALL` order. Empty
    /// before the first track exists.
    #[serde(default)]
    pub sources: Vec<SourceKind>,
    /// Diarized speaker labels, merged-away speakers excluded, in the order
    /// the store assigned them.
    #[serde(default)]
    pub speaker_labels: Vec<String>,
    #[serde(default)]
    pub headline: MeetingHistoryHeadline,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct PaginatedMeetings {
    pub entries: Vec<MeetingHistorySummary>,
    pub has_more: bool,
}

/// Which retained meetings one list page should contain. Every field defaults
/// to "no constraint", so a caller that sends no filter still gets the whole
/// list and an older caller keeps working unchanged.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingListFilter {
    #[serde(default)]
    pub status: MeetingStatusFilter,
    #[serde(default)]
    pub window: MeetingTimeWindow,
    /// Case-insensitive substring of the title. Blank means no constraint.
    #[serde(default)]
    pub title_query: String,
}

/// The four states a person actually sorts a meeting list by. Each maps onto
/// the stored phase and processing status rather than onto a label, so the
/// filter and the row's own status chip can never disagree.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingStatusFilter {
    #[default]
    Any,
    /// Processing succeeded and the meeting is waiting to be read.
    Ready,
    /// Capture has stopped and the artifacts are not finished.
    Processing,
    /// Processing failed or was cancelled, or capture needs recovery.
    Failed,
}

/// A window of local calendar days, today included. `Any` is unbounded.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingTimeWindow {
    #[default]
    Any,
    Today,
    /// serde's `snake_case` writes `last7_days` while specta splits the digit
    /// off and the UI sends `last_7_days`; the alias keeps reading whatever a
    /// caller on the older spelling sends.
    #[serde(rename = "last_7_days", alias = "last7_days")]
    Last7Days,
    #[serde(rename = "last_30_days", alias = "last30_days")]
    Last30Days,
}

impl MeetingTimeWindow {
    /// How many local calendar days the window spans, today included, or
    /// `None` when it is unbounded.
    pub const fn days(self) -> Option<u32> {
        match self {
            Self::Any => None,
            Self::Today => Some(1),
            Self::Last7Days => Some(7),
            Self::Last30Days => Some(30),
        }
    }
}

/// Content-free aggregate for either the selected meeting trend range or all
/// retained meeting sessions.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingTrendTotals {
    pub meetings: u64,
    pub verified_captured_duration_ms: u64,
    pub transcript_segments: u64,
    pub generated_action_items: u64,
}

/// One local-calendar day in the meeting trend. Every requested date is
/// present, including dates with no retained meeting sessions.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingTrendPoint {
    pub local_date: String,
    pub meetings: u64,
    pub verified_captured_duration_ms: u64,
    pub transcript_segments: u64,
    pub generated_action_items: u64,
}

/// A bounded meeting projection. Storage failures have no zero-valued data
/// projection, so callers can distinguish unavailable storage from an empty
/// range.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MeetingTrendProjection {
    Available {
        range: DashboardTrendRange,
        range_start_local_date: String,
        range_end_local_date: String,
        all_time: MeetingTrendTotals,
        range_total: MeetingTrendTotals,
        points: Vec<MeetingTrendPoint>,
    },
    Unavailable {
        range: DashboardTrendRange,
    },
}

impl MeetingTrendProjection {
    pub(crate) const fn unavailable(range: DashboardTrendRange) -> Self {
        Self::Unavailable { range }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingArtifactKind {
    Notes,
    Actions,
    Decisions,
    Topics,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingArtifactState {
    Current,
    OutOfDate,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingArtifact {
    pub artifact_id: MeetingArtifactId,
    pub session_id: MeetingSessionId,
    pub kind: MeetingArtifactKind,
    pub transcript_revision_id: Option<TranscriptRevisionId>,
    pub state: MeetingArtifactState,
    pub created_at_utc_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct ArtifactCitation {
    pub segment_id: TranscriptSegmentId,
    pub start_offset_ns: u64,
    pub end_offset_ns: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct CitedArtifactText {
    pub text: String,
    pub citations: Vec<ArtifactCitation>,
}

/// Where one line of the summary came from: the transcript segment a reader
/// lands on when they press that line. `line` is the zero-based index of the
/// line inside `CitedArtifactText::text` split on newlines, which holds
/// because the text and this map are written together in one generation and
/// an artifact revision is immutable afterwards. Sparse on purpose: a line
/// with no entry renders as plain text, which is what every revision
/// generated before line provenance existed has.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct SummaryLineTrace {
    pub line: u32,
    pub anchor: ArtifactCitation,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingOutlineTopic {
    pub title: CitedArtifactText,
    pub detail: Option<CitedArtifactText>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingActionItem {
    pub text: CitedArtifactText,
    pub owner_text: Option<String>,
    pub due_text: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct GeneratedMeetingArtifacts {
    pub summary: CitedArtifactText,
    /// The summary's line-to-segment map, in line order. Defaulted rather than
    /// required, so a revision generated before this existed still reads back
    /// and simply offers no jumps.
    #[serde(default)]
    pub summary_trace: Vec<SummaryLineTrace>,
    pub outline: Vec<MeetingOutlineTopic>,
    pub decisions: Vec<CitedArtifactText>,
    pub action_items: Vec<MeetingActionItem>,
    pub key_questions: Vec<CitedArtifactText>,
    pub risks: Vec<CitedArtifactText>,
    pub follow_up_draft: CitedArtifactText,
    /// The where-did-we-land ledger for this meeting: threads, where each one
    /// landed, and the receipt each state was read from. Defaulted rather than
    /// required, so a revision generated before ledgers existed still reads
    /// back; a `TEMPLATE_VERSION` bump is what retires those.
    #[serde(default)]
    pub ledger: Option<MeetingLedger>,
}

impl GeneratedMeetingArtifacts {
    /// The one line that stands for this meeting: the ledger's reading across
    /// rows when there is one, the summary otherwise. Every surface that shows
    /// a meeting in one line reads this, including the derived title, so a
    /// meeting is never headlined two different ways.
    pub fn headline(&self) -> Option<&str> {
        self.ledger
            .as_ref()
            .map(|ledger| ledger.headline.trim())
            .filter(|headline| !headline.is_empty())
            .or_else(|| {
                let summary = self.summary.text.trim();
                (!summary.is_empty()).then_some(summary)
            })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingArtifactRevision {
    pub artifact_id: MeetingArtifactId,
    pub session_id: MeetingSessionId,
    pub transcript_revision_id: TranscriptRevisionId,
    pub input_revision: u64,
    pub template_id: String,
    pub template_version: u32,
    pub generation_key: String,
    pub state: MeetingArtifactState,
    pub generated_at_utc_ms: i64,
    pub content: Option<GeneratedMeetingArtifacts>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CitationKind {
    Transcript,
    ManualNote,
    Title,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingCitation {
    pub kind: CitationKind,
    pub session_id: MeetingSessionId,
    pub entity_id: String,
    pub start_offset_ns: Option<u64>,
    #[serde(default)]
    pub end_offset_ns: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MeetingQuestionScope {
    #[default]
    ThisMeeting,
    ExplicitSeries {
        session_ids: Vec<MeetingSessionId>,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingAnswerState {
    Supported,
    InsufficientEvidence,
    Unavailable,
    OutOfDate,
    Forgotten,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingAnswer {
    pub question_id: MeetingQuestionId,
    pub session_id: MeetingSessionId,
    pub scope: MeetingQuestionScope,
    pub question: Option<String>,
    pub state: MeetingAnswerState,
    pub answer: Option<String>,
    pub citations: Vec<MeetingCitation>,
    pub input_revision: u64,
    pub revision: u64,
    pub created_at_utc_ms: i64,
    /// How far into the meeting the evidence behind this answer reached, for a
    /// provisional answer. `None` once the transcript has landed: a finished
    /// meeting's answer was read from all of it.
    pub through_offset_ns: Option<u64>,
    /// Read from the provisional transcript of a capture that was still
    /// running. A provisional answer is never saved as history — the segments
    /// it cites belong to an in-memory reading that no revision keeps — so it
    /// is returned to the asker and nowhere else.
    pub provisional: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingSearchHit {
    pub session_id: MeetingSessionId,
    pub kind: CitationKind,
    pub entity_id: String,
    pub start_offset_ns: Option<u64>,
    pub end_offset_ns: Option<u64>,
    pub excerpt: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingSearchResult {
    pub entries: Vec<MeetingSearchHit>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct MeetingSearchRequest {
    pub query: String,
    pub session_ids: Vec<MeetingSessionId>,
    pub limit: Option<usize>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingExportFormat {
    Json,
    Markdown,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct MeetingExportRequest {
    pub operation_id: MeetingOperationId,
    pub session_id: MeetingSessionId,
    pub expected_revision: u64,
    pub format: MeetingExportFormat,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingExportReceipt {
    pub export_receipt_id: MeetingExportReceiptId,
    pub session_id: MeetingSessionId,
    pub format: MeetingExportFormat,
    pub snapshot_revision: u64,
    pub capture_completeness: CaptureCompleteness,
    pub transcript_revision_id: Option<TranscriptRevisionId>,
    pub created_at_utc_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingExportResult {
    pub receipt: OperationReceipt,
    pub export_receipt: MeetingExportReceipt,
}

macro_rules! meeting_event {
    ($event_type:ident, $event_name:literal) => {
        #[derive(Clone, Debug, Deserialize, Serialize, Type)]
        #[serde(transparent)]
        pub struct $event_type(pub MeetingEventPayload);

        impl tauri_specta::Event for $event_type {
            const NAME: &'static str = $event_name;
        }
    };
}

meeting_event!(MeetingSuggestionChangedEvent, "meeting:suggestion-changed");
meeting_event!(MeetingSessionChangedEvent, "meeting:session-changed");
meeting_event!(
    MeetingSourceHealthChangedEvent,
    "meeting:source-health-changed"
);
meeting_event!(MeetingTranscriptChangedEvent, "meeting:transcript-changed");
meeting_event!(MeetingNoteChangedEvent, "meeting:note-changed");
meeting_event!(MeetingArtifactChangedEvent, "meeting:artifact-changed");
meeting_event!(MeetingRemoteJobChangedEvent, "meeting:remote-job-changed");
meeting_event!(MeetingRemovedEvent, "meeting:removed");

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingEventPayload {
    pub event_schema_version: u32,
    pub session_id: Option<MeetingSessionId>,
    pub revision: u64,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingNavigationDestination {
    List,
    Preflight,
    Session,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingNavigationPayload {
    pub event_schema_version: u32,
    pub destination: MeetingNavigationDestination,
    pub session_id: Option<MeetingSessionId>,
    pub revision: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
#[serde(transparent)]
pub struct MeetingNavigationRequestedEvent(pub MeetingNavigationPayload);

impl tauri_specta::Event for MeetingNavigationRequestedEvent {
    const NAME: &'static str = "meeting:navigation-requested";
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bindings and the Meetings time filter both say `last_7_days`,
    /// while serde's own `snake_case` writes `last7_days`. The window has to
    /// answer to the UI spelling, keep reading the older one, and keep its
    /// day counts, which the store's cutoff query reads.
    #[test]
    fn time_window_wire_values_match_the_ui_and_still_read_the_legacy_spelling() {
        for (window, ui_spelling, legacy_spelling, days) in [
            (MeetingTimeWindow::Last7Days, "last_7_days", "last7_days", 7),
            (
                MeetingTimeWindow::Last30Days,
                "last_30_days",
                "last30_days",
                30,
            ),
        ] {
            assert_eq!(
                serde_json::to_value(window).expect("time window serializes"),
                serde_json::json!(ui_spelling)
            );
            for spelling in [ui_spelling, legacy_spelling] {
                assert_eq!(
                    serde_json::from_value::<MeetingTimeWindow>(serde_json::json!(spelling))
                        .unwrap_or_else(|error| panic!("window reads {spelling}: {error}")),
                    window
                );
            }
            assert_eq!(window.days(), Some(days));
        }
    }
}

import Foundation

/// The meeting wire types, mirrored from `src/bindings.ts` (which is generated
/// from `src-tauri/src/meeting/types.rs` and its siblings). Field names are the
/// Rust names with snake_case turned into camelCase by `Core.decoder`; enum
/// values are the wire strings verbatim.
///
/// Names here carry a prefix from the meetings slice — `Meeting`, `Meetings`,
/// `Ledger`, `FollowUp`, `CatchUp`, `Loop`, `Speaker`, `Segment`, `Transcript`,
/// `Artifact` — so a wire type whose Rust name has no such prefix is renamed
/// rather than dropped: `SourceKind` is `MeetingSourceKind`,
/// `EffectiveTranscriptSegment` is `TranscriptEffectiveSegment`,
/// `CitedArtifactText` is `ArtifactCitedText`, `OperationReceipt` is
/// `MeetingOperationReceipt`, `AllowedMeetingAction` is `MeetingAllowedAction`.

// MARK: - Identifiers

typealias MeetingSessionId = String
typealias MeetingOperationId = String
typealias MeetingArtifactId = String
typealias MeetingQuestionId = String
typealias MeetingLoopId = String
typealias MeetingDeletionJobId = String
typealias MeetingExportReceiptId = String
typealias MeetingDiarizationGenerationId = String
/// `ManualNoteId`.
typealias MeetingNoteId = String
/// `PersonId`, as the people-context rows carry it.
typealias MeetingPersonId = String
/// `SourceTrackId`.
typealias MeetingSourceTrackId = String
typealias SpeakerId = String
typealias TranscriptSegmentId = String
typealias TranscriptRevisionId = String

// MARK: - Events

extension CoreEvent {
    static let meetingSessionChanged = "meeting:session-changed"
    static let meetingTranscriptChanged = "meeting:transcript-changed"
    static let meetingNoteChanged = "meeting:note-changed"
    static let meetingArtifactChanged = "meeting:artifact-changed"
    static let meetingRemoteJobChanged = "meeting:remote-job-changed"
    static let meetingRemoved = "meeting:removed"
    static let meetingNavigationRequested = "meeting:navigation-requested"
    static let meetingSourceHealthChanged = "meeting:source-health-changed"
}

/// The payload every `meeting:*` event but navigation carries.
struct MeetingEventPayload: Decodable {
    let eventSchemaVersion: Int
    let sessionId: MeetingSessionId?
    let revision: Int
}

enum MeetingNavigationDestination: String, Decodable {
    case list
    case preflight
    case session
}

struct MeetingNavigationPayload: Decodable {
    let eventSchemaVersion: Int
    let destination: MeetingNavigationDestination
    let sessionId: MeetingSessionId?
    let revision: Int
}

// MARK: - Session state

enum MeetingPhase: String, Decodable {
    case preflight
    case starting
    case capturingRecording = "capturing_recording"
    case capturingPausing = "capturing_pausing"
    case capturingPaused = "capturing_paused"
    case capturingResuming = "capturing_resuming"
    case stopping
    case processing
    case reviewReady = "review_ready"
    case recoveryRequired = "recovery_required"
    case deleting

    /// `isActiveMeetingPhase` in meetingUtils.ts: capture is running or moving.
    var isActive: Bool {
        switch self {
        case .capturingRecording, .capturingPausing, .capturingPaused, .capturingResuming, .starting, .stopping:
            true
        default:
            false
        }
    }

    /// The words the phase row shows, from `meetings.phases.*`.
    var label: String {
        switch self {
        case .preflight: "Getting ready"
        case .starting: "Starting"
        case .capturingRecording: "Recording"
        case .capturingPausing: "Pausing"
        case .capturingPaused: "Paused"
        case .capturingResuming: "Resuming"
        case .stopping: "Stopping"
        case .processing: "Processing"
        case .reviewReady: "Ready to read"
        case .recoveryRequired: "Needs recovery"
        case .deleting: "Deleting"
        }
    }
}

/// `SourceKind`.
enum MeetingSourceKind: String, Decodable, CaseIterable, Hashable {
    case microphone
    case systemAudio = "system_audio"

    /// `meetings.sources.*`.
    var label: String {
        switch self {
        case .microphone: "Microphone"
        case .systemAudio: "System audio"
        }
    }
}

/// `SourceAvailability`.
enum MeetingSourceAvailability: String, Decodable {
    case available
    case permissionRequired = "permission_required"
    case permissionDenied = "permission_denied"
    case deviceUnavailable = "device_unavailable"
    case unsupportedPlatform = "unsupported_platform"
    case storageUnavailable = "storage_unavailable"
    case unknown

    /// `meetings.availability.*`.
    var label: String {
        switch self {
        case .available: "Available"
        case .permissionRequired: "Needs permission"
        case .permissionDenied: "Permission denied"
        case .deviceUnavailable: "Device unavailable"
        case .unsupportedPlatform: "Not supported here"
        case .storageUnavailable: "Storage unavailable"
        case .unknown: "Unknown"
        }
    }
}

/// `SourceHealth`.
enum MeetingSourceHealth: String, Decodable {
    case notStarted = "not_started"
    case starting
    case healthy
    case degraded
    case failed
}

/// `CaptureCompleteness`.
enum MeetingCaptureCompleteness: String, Decodable {
    case notStarted = "not_started"
    case complete
    case partial

    /// `meetings.completeness.*`.
    var label: String {
        switch self {
        case .notStarted: "Nothing recorded"
        case .complete: "Recorded in full"
        case .partial: "Recorded with gaps"
        }
    }
}

/// `StorageAvailability`.
enum MeetingStorageAvailability: String, Decodable {
    case available
    case unavailable
}

/// `ProcessingFailure`.
enum MeetingProcessingFailure: String, Decodable {
    case localModelUnavailable = "local_model_unavailable"
    case remoteUnavailable = "remote_unavailable"
    case engineFailure = "engine_failure"
    case cancelled
    case interrupted
}

/// `EngineFailureCause`: what the run could not do, when the failure was the
/// engine's.
enum MeetingEngineFailureCause: String, Decodable {
    case storage
    case transcription
    case voiceDetection = "voice_detection"
    case evidencePack = "evidence_pack"
    case modelRefused = "model_refused"
    case replyNotStructured = "reply_not_structured"
    case replyRejected = "reply_rejected"
    case panicked

    var label: String {
        switch self {
        case .storage: "the meeting's records could not be read or written"
        case .transcription: "the speech engine refused some of the audio"
        case .voiceDetection: "speech detection refused part of a track"
        case .evidencePack: "the transcript would not fit the model's prompt"
        case .modelRefused: "the model returned nothing usable"
        case .replyNotStructured: "the model's reply was not the shape asked for"
        case .replyRejected: "the model cited a moment that is not in the transcript"
        case .panicked: "the notes pipeline crashed"
        }
    }
}

/// `ProcessingStatus`: `{"kind": "failed", "reason": ..., "cause": ...}` and
/// its four data-free siblings.
enum MeetingProcessingStatus: Decodable, Equatable {
    case pending
    case running
    case succeeded
    case failed(reason: MeetingProcessingFailure, cause: MeetingEngineFailureCause?)
    case cancelled

    private enum Key: String, CodingKey { case kind, reason, cause }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let kind = try container.decode(String.self, forKey: .kind)
        switch kind {
        case "pending": self = .pending
        case "running": self = .running
        case "succeeded": self = .succeeded
        case "cancelled": self = .cancelled
        case "failed":
            self = .failed(
                reason: try container.decode(MeetingProcessingFailure.self, forKey: .reason),
                cause: try container.decodeIfPresent(MeetingEngineFailureCause.self, forKey: .cause)
            )
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: container, debugDescription: "unknown processing status \(kind)")
        }
    }

    /// `meetings.processing.*`, with the failure reason folded in.
    var label: String {
        switch self {
        case .pending: "Waiting to be processed"
        case .running: "Writing the notes"
        case .succeeded: "Notes ready"
        case .cancelled: "Processing cancelled"
        case let .failed(reason, cause):
            switch reason {
            case .localModelUnavailable: "No local model was available"
            case .remoteUnavailable: "The remote engine was unavailable"
            case .engineFailure: "Processing failed: \(cause?.label ?? "the engine failed")"
            case .cancelled: "Processing was cancelled"
            case .interrupted: "Sona closed before processing finished"
            }
        }
    }

    var isFailed: Bool { if case .failed = self { true } else { false } }
    var isPending: Bool {
        switch self {
        case .pending, .running: true
        default: false
        }
    }
}

/// `AllowedMeetingAction`: what the core will accept on this session now.
enum MeetingAllowedAction: String, Decodable {
    case refreshPreflight = "refresh_preflight"
    case cancelPreflight = "cancel_preflight"
    case start
    case pause
    case resume
    case stop
    case discard
    case finalizePartial = "finalize_partial"
    case edit
    case regenerate
    case export
    case delete
    case cancelRemote = "cancel_remote"
}

/// `AudioFormat`.
struct MeetingAudioFormat: Decodable {
    let sampleRateHz: Int
    let channels: Int
}

/// `SourceSnapshot`.
struct MeetingSourceSnapshot: Decodable {
    let trackId: MeetingSourceTrackId?
    let sourceKind: MeetingSourceKind
    let required: Bool
    let availability: MeetingSourceAvailability
    let health: MeetingSourceHealth
    let format: MeetingAudioFormat?
    let lastDurableOffsetNs: Int64?
    let gapCount: Int
}

struct MeetingSessionSnapshot: Decodable {
    let sessionId: MeetingSessionId
    let phase: MeetingPhase
    let revision: Int
    let title: String
    let startedAtUtcMs: Int64?
    let elapsedOffsetNs: Int64?
    let sources: [MeetingSourceSnapshot]
    let openCaptureWindowStartedAtNs: Int64?
    let captureCompleteness: MeetingCaptureCompleteness
    let storage: MeetingStorageAvailability
    let processingStatus: MeetingProcessingStatus
    let preflightLocalProcessing: MeetingSourceAvailability?
    let retentionDeadlineUtcMs: Int64?
    let allowedActions: [MeetingAllowedAction]

    func allows(_ action: MeetingAllowedAction) -> Bool { allowedActions.contains(action) }
}

struct MeetingTrackSnapshot: Decodable {
    let trackId: MeetingSourceTrackId
    let sourceKind: MeetingSourceKind
    let format: MeetingAudioFormat?
    let firstOffsetNs: Int64?
    let lastOffsetNs: Int64?
    let durableRecordCount: Int
}

/// `SourceGapReason`.
enum MeetingSourceGapReason: String, Decodable {
    case sourceUnavailable = "source_unavailable"
    case sourceStartFailed = "source_start_failed"
    case permissionLost = "permission_lost"
    case paused
    case packetDropped = "packet_dropped"
    case writerPressure = "writer_pressure"
    case storageFailure = "storage_failure"
    case timestampMissing = "timestamp_missing"
    case timestampDiscontinuity = "timestamp_discontinuity"
    case invalidFormat = "invalid_format"
    case sourceStopped = "source_stopped"
    case corruptRecord = "corrupt_record"
    case missingRecord = "missing_record"
    case recoveryTail = "recovery_tail"
    case systemSleep = "system_sleep"

    /// `meetings.gapReasons.*`, in the words the gap timeline uses.
    var label: String {
        switch self {
        case .sourceUnavailable: "source unavailable"
        case .sourceStartFailed: "source failed to start"
        case .permissionLost: "permission lost"
        case .paused: "paused"
        case .packetDropped: "audio dropped"
        case .writerPressure: "the writer fell behind"
        case .storageFailure: "storage failed"
        case .timestampMissing: "timestamps missing"
        case .timestampDiscontinuity: "timestamps jumped"
        case .invalidFormat: "unreadable audio format"
        case .sourceStopped: "source stopped"
        case .corruptRecord: "a record was corrupt"
        case .missingRecord: "a record was missing"
        case .recoveryTail: "recovered tail"
        case .systemSleep: "the Mac slept"
        }
    }
}

/// `SourceGap`.
struct MeetingSourceGap: Decodable {
    let trackId: MeetingSourceTrackId
    let epoch: Int
    let startOffsetNs: Int64?
    let endOffsetNs: Int64?
    let reason: MeetingSourceGapReason
    let droppedFrames: Int64?
}

// MARK: - Receipts and errors

enum MeetingOperationActor: String, Decodable {
    case user
    case system
    case external
}

enum MeetingOperationResult: String, Decodable {
    case committed
    case rejected
    case failed
}

enum MeetingReasonCode: String, Decodable {
    case consentMissing = "consent_missing"
    case consentStale = "consent_stale"
    case staleRevision = "stale_revision"
    case captureLeaseBusy = "capture_lease_busy"
    case sourceUnavailable = "source_unavailable"
    case sourceStartFailed = "source_start_failed"
    case sourceGap = "source_gap"
    case storageUnavailable = "storage_unavailable"
    case storageFailure = "storage_failure"
    case localModelUnavailable = "local_model_unavailable"
    case recoveryRequired = "recovery_required"
    case deleted
    case invalidTransition = "invalid_transition"
    case duplicateOperation = "duplicate_operation"

    /// `meetings.reasons.*`.
    var label: String {
        switch self {
        case .consentMissing: "consent was never given"
        case .consentStale: "the consent on file is out of date"
        case .staleRevision: "the meeting changed while you were reading it"
        case .captureLeaseBusy: "another recording holds the microphone"
        case .sourceUnavailable: "a capture source was unavailable"
        case .sourceStartFailed: "a capture source failed to start"
        case .sourceGap: "audio went missing mid-recording"
        case .storageUnavailable: "storage was unavailable"
        case .storageFailure: "storage failed"
        case .localModelUnavailable: "no local model was available"
        case .recoveryRequired: "this meeting needs recovery first"
        case .deleted: "the meeting was deleted"
        case .invalidTransition: "the meeting is not in a state for that"
        case .duplicateOperation: "that had already been done"
        }
    }
}

enum MeetingCommandKind: String, Decodable {
    case preflightCreate = "preflight_create"
    case preflightRefresh = "preflight_refresh"
    case preflightCancel = "preflight_cancel"
    case start
    case pause
    case resume
    case stop
    case discard
    case recoveryFinalize = "recovery_finalize"
    case titleSet = "title_set"
    case speakerRename = "speaker_rename"
    case speakerMerge = "speaker_merge"
    case speakerIdentify = "speaker_identify"
    case segmentEdit = "segment_edit"
    case noteCreate = "note_create"
    case noteUpdate = "note_update"
    case noteDelete = "note_delete"
    case artifactsRegenerate = "artifacts_regenerate"
    case questionAsk = "question_ask"
    case questionForget = "question_forget"
    case export
    case delete
    case retentionSet = "retention_set"
    case remoteCancel = "remote_cancel"
    case loopResolve = "loop_resolve"
    case loopReopen = "loop_reopen"
    case loopAssign = "loop_assign"
    case loopCarry = "loop_carry"
    case seriesTemplateSet = "series_template_set"
    case seriesDigestSet = "series_digest_set"
    case seriesAlwaysRecordSet = "series_always_record_set"
    case followUpDraft = "follow_up_draft"
    case seriesAutomationSet = "series_automation_set"
    case seriesRemoteOptOutSet = "series_remote_opt_out_set"
    case savedPromptSave = "saved_prompt_save"
    case savedPromptDelete = "saved_prompt_delete"
}

/// `OperationReceipt`: what the core did with one command, and why.
struct MeetingOperationReceipt: Decodable {
    let schemaVersion: Int
    let operationId: MeetingOperationId
    let sessionId: MeetingSessionId?
    let actor: MeetingOperationActor
    let command: MeetingCommandKind
    let expectedRevision: Int
    let fromPhase: MeetingPhase?
    let toPhase: MeetingPhase?
    let requestedAtUtcMs: Int64
    let committedAtUtcMs: Int64?
    let result: MeetingOperationResult
    let reasonCodes: [MeetingReasonCode]
    let newRevision: Int?
    let effectIds: [String]

    /// The line the review screen shows when a command did not commit:
    /// "Rejected: the meeting changed while you were reading it".
    var refusal: String? {
        guard result != .committed else { return nil }
        let reasons = reasonCodes.map(\.label).joined(separator: ", ")
        let verb = result == .rejected ? "Rejected" : "Failed"
        return reasons.isEmpty ? "\(verb): the core gave no reason" : "\(verb): \(reasons)"
    }
}

/// `MeetingCommandError`: every way a meeting command refuses, in the words
/// `meetingUtils.ts` maps each one to.
enum MeetingCommandError: String, Decodable {
    case consentRequired = "consent_required"
    case consentStale = "consent_stale"
    case invalidTransition = "invalid_transition"
    case staleRevision = "stale_revision"
    case captureLeaseBusy = "capture_lease_busy"
    case noSourceStarted = "no_source_started"
    case sourceUnavailable = "source_unavailable"
    case storageUnavailable = "storage_unavailable"
    case recoveryRequired = "recovery_required"
    case deletionInProgress = "deletion_in_progress"
    case notFound = "not_found"
    case invalidRequest = "invalid_request"
    case exportCancelled = "export_cancelled"
    case exportFailed = "export_failed"
    case localModelUnavailable = "local_model_unavailable"
    case localEvidenceUnavailable = "local_evidence_unavailable"
    case insufficientEnrollmentEvidence = "insufficient_enrollment_evidence"
    case profileModelIncompatible = "profile_model_incompatible"
    case profileMergeResolutionRequired = "profile_merge_resolution_required"
    case remoteUnavailable = "remote_unavailable"
    case engineFailure = "engine_failure"
    case importUnreadable = "import_unreadable"

    var label: String {
        switch self {
        case .consentRequired: "Recording needs your consent first."
        case .consentStale: "The consent on file is out of date. Start again to give it fresh."
        case .invalidTransition: "The meeting is not in a state for that."
        case .staleRevision: "This meeting changed while you were reading it. Reloading it now."
        case .captureLeaseBusy: "Another recording is already using the microphone."
        case .noSourceStarted: "No capture source started, so there was nothing to record."
        case .sourceUnavailable: "A capture source was unavailable."
        case .storageUnavailable: "Sona cannot reach its meeting storage."
        case .recoveryRequired: "This meeting needs recovery before it can be read."
        case .deletionInProgress: "This meeting is being deleted."
        case .notFound: "That meeting is no longer here."
        case .invalidRequest: "The core refused that request."
        case .exportCancelled: "The export was cancelled."
        case .exportFailed: "The export failed."
        case .localModelUnavailable: "No local model is installed, so Sona cannot write this."
        case .localEvidenceUnavailable: "Sona could not remember that voice."
        case .insufficientEnrollmentEvidence: "Sona could not remember that voice."
        case .profileModelIncompatible: "Sona could not remember that voice."
        case .profileMergeResolutionRequired: "Choose which voice profile to keep first."
        case .remoteUnavailable: "The remote engine was unavailable."
        case .engineFailure: "The engine failed while writing this."
        case .importUnreadable: "Sona cannot read that file. Choose a different one."
        }
    }
}

struct MeetingMutationResult: Decodable {
    let receipt: MeetingOperationReceipt
    let snapshot: MeetingSessionSnapshot
}

struct MeetingRemovalResult: Decodable {
    let receipt: MeetingOperationReceipt
    let sessionId: MeetingSessionId
    let removed: Bool
}

// MARK: - Transcript, speakers, notes

struct TranscriptSegment: Decodable {
    let segmentId: TranscriptSegmentId
    let transcriptRevisionId: TranscriptRevisionId
    let trackId: MeetingSourceTrackId
    let ordinal: Int
    let startOffsetNs: Int64
    let endOffsetNs: Int64
    let speakerId: SpeakerId
    let text: String
    let confidenceMilli: Int?
}

enum SpeakerAssignmentKind: String, Decodable {
    case localSpeaker = "local_speaker"
    case systemSpeaker = "system_speaker"
    case unknown
    case overlap
}

/// `EffectiveTranscriptSegment`: the stored segment plus the edit and speaker
/// assignment laid over it.
struct TranscriptEffectiveSegment: Decodable, Identifiable {
    let base: TranscriptSegment
    let replacementText: String?
    let removed: Bool
    let editRevision: Int?
    let assignedSpeakerId: SpeakerId
    let speakerAssignment: SpeakerAssignmentKind

    var id: TranscriptSegmentId { base.segmentId }
    /// The words as they now read: the edit when there is one.
    var text: String { replacementText ?? base.text }
    var edited: Bool { replacementText != nil }
}

struct MeetingSpeaker: Decodable, Identifiable {
    let speakerId: SpeakerId
    let sessionId: MeetingSessionId
    let sourceKind: MeetingSourceKind
    let displayName: String
    let revision: Int

    var id: SpeakerId { speakerId }
}

/// `ManualNote`: a note a person typed against a moment of the meeting.
struct MeetingManualNote: Decodable, Identifiable {
    let noteId: MeetingNoteId
    let sessionId: MeetingSessionId
    let startOffsetNs: Int64?
    let endOffsetNs: Int64?
    let body: String
    let revision: Int
    let createdAtUtcMs: Int64
    let updatedAtUtcMs: Int64

    var id: MeetingNoteId { noteId }
}

enum MeetingDiarizationStatus: String, Decodable {
    case notRequested = "not_requested"
    case modelUnavailable = "model_unavailable"
    case downloading
    case running
    case succeeded
    case failed

    var label: String {
        switch self {
        case .notRequested: "Speakers were not separated"
        case .modelUnavailable: "The speaker model is not installed"
        case .downloading: "Downloading the speaker model"
        case .running: "Separating speakers"
        case .succeeded: "Speakers separated"
        case .failed: "Separating speakers failed"
        }
    }
}

struct MeetingDiarizationSnapshot: Decodable {
    let status: MeetingDiarizationStatus
    let modelId: String
    let modelVersion: String
    let generationId: MeetingDiarizationGenerationId?
    let assignedSegmentCount: Int
}

// MARK: - Artifacts

struct ArtifactCitation: Decodable, Hashable {
    let segmentId: TranscriptSegmentId
    let startOffsetNs: Int64
    let endOffsetNs: Int64
}

/// `CitedArtifactText`: a line a model wrote, with the transcript it came from.
struct ArtifactCitedText: Decodable {
    let text: String
    let citations: [ArtifactCitation]
}

/// `SummaryLineTrace`: which transcript moment one summary line came from.
struct ArtifactSummaryLineTrace: Decodable {
    let line: Int
    let anchor: ArtifactCitation
}

struct MeetingOutlineTopic: Decodable {
    let title: ArtifactCitedText
    let detail: ArtifactCitedText?
}

struct MeetingActionItem: Decodable {
    let text: ArtifactCitedText
    let ownerText: String?
    let dueText: String?
}

enum LedgerFirmness: String, Decodable {
    case firm
    case soft

    var label: String { self == .firm ? "firm" : "soft" }
}

/// `LedgerThreadState`: the nine states a reader can check a quote against.
enum LedgerThreadState: String, Decodable {
    case decided
    case agreed
    case action
    case closed
    case open
    case partial
    case ambiguous
    case unanswered
    case dropped

    /// `LEDGER_OUTCOME` in meetingLedger.ts: nine states rolled into the three
    /// a glance needs.
    var outcome: LedgerOutcome {
        switch self {
        case .decided, .agreed, .action, .closed: .landed
        case .open, .partial, .ambiguous: .open
        case .unanswered, .dropped: .dropped
        }
    }

    var label: String { rawValue }
}

/// The frontend's rollup of `LedgerThreadState`.
enum LedgerOutcome: String, CaseIterable {
    case landed
    case open
    case dropped

    var label: String {
        switch self {
        case .landed: "landed"
        case .open: "open"
        case .dropped: "dropped"
        }
    }
}

/// The quote a ledger state was read from.
struct LedgerReceipt: Decodable {
    let quote: String
    let speaker: String?
    let tMs: Int64
    let citations: [ArtifactCitation]
}

struct LedgerThread: Decodable {
    let topic: String
    let state: LedgerThreadState
    let substantive: Bool
    let receipt: LedgerReceipt
    let owner: String?
}

struct LedgerOpenLoop: Decodable {
    let question: String
    let instead: String
    let atMs: Int64
    let citations: [ArtifactCitation]
}

struct LedgerCommitment: Decodable {
    let who: String
    let what: String
    let firmness: LedgerFirmness
    let receipt: LedgerReceipt
}

struct LedgerStance: Decodable {
    let from: String
    let to: String?
    let what: String
    let note: String?
    let atMs: Int64
    let citations: [ArtifactCitation]
}

/// `LedgerReceiptState`: whether every receipt was found in the transcript.
enum LedgerReceiptState: Decodable {
    case verified
    case degraded(droppedThreads: Int, droppedCommitments: Int)

    private enum Key: String, CodingKey { case status, droppedThreads, droppedCommitments }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let status = try container.decode(String.self, forKey: .status)
        switch status {
        case "verified": self = .verified
        case "degraded":
            self = .degraded(
                droppedThreads: try container.decode(Int.self, forKey: .droppedThreads),
                droppedCommitments: try container.decode(Int.self, forKey: .droppedCommitments)
            )
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .status, in: container, debugDescription: "unknown receipt state \(status)")
        }
    }
}

struct MeetingLedger: Decodable {
    let headline: String
    let threads: [LedgerThread]
    let openLoops: [LedgerOpenLoop]
    let commitments: [LedgerCommitment]
    let stances: [LedgerStance]
    let caveats: [String]
    let receipts: LedgerReceiptState
}

/// `GeneratedMeetingArtifacts`: everything one generation of the notes wrote.
struct MeetingGeneratedArtifacts: Decodable {
    let summary: ArtifactCitedText
    let summaryTrace: [ArtifactSummaryLineTrace]?
    let outline: [MeetingOutlineTopic]
    let decisions: [ArtifactCitedText]
    let actionItems: [MeetingActionItem]
    let keyQuestions: [ArtifactCitedText]
    let risks: [ArtifactCitedText]
    let followUpDraft: ArtifactCitedText
    let ledger: MeetingLedger?
}

enum MeetingArtifactState: String, Decodable {
    case current
    case outOfDate = "out_of_date"
    case failed
}

struct MeetingArtifactRevision: Decodable, Identifiable {
    let artifactId: MeetingArtifactId
    let sessionId: MeetingSessionId
    let transcriptRevisionId: TranscriptRevisionId
    let inputRevision: Int
    let templateId: String
    let templateVersion: Int
    let generationKey: String
    let state: MeetingArtifactState
    let generatedAtUtcMs: Int64
    let content: MeetingGeneratedArtifacts?

    var id: MeetingArtifactId { artifactId }
}

/// `CitationKind`.
enum MeetingCitationKind: String, Decodable {
    case transcript
    case manualNote = "manual_note"
    case title
}

struct MeetingCitation: Decodable {
    let kind: MeetingCitationKind
    let sessionId: MeetingSessionId
    let entityId: String
    let startOffsetNs: Int64?
    let endOffsetNs: Int64?
}

enum MeetingQuestionScope: Decodable {
    case thisMeeting
    case explicitSeries(sessionIds: [MeetingSessionId])

    private enum Key: String, CodingKey { case kind, sessionIds }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let kind = try container.decode(String.self, forKey: .kind)
        switch kind {
        case "this_meeting": self = .thisMeeting
        case "explicit_series":
            self = .explicitSeries(sessionIds: try container.decode([MeetingSessionId].self, forKey: .sessionIds))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: container, debugDescription: "unknown question scope \(kind)")
        }
    }
}

enum MeetingAnswerState: String, Decodable {
    case supported
    case insufficientEvidence = "insufficient_evidence"
    case unavailable
    case outOfDate = "out_of_date"
    case forgotten

    var label: String {
        switch self {
        case .supported: "Answered from the transcript"
        case .insufficientEvidence: "The transcript does not say"
        case .unavailable: "No model was available to answer"
        case .outOfDate: "Asked of an older transcript"
        case .forgotten: "Forgotten"
        }
    }
}

/// One question asked of a meeting, and what the evidence supported.
struct MeetingAnswer: Decodable, Identifiable {
    let questionId: MeetingQuestionId
    let sessionId: MeetingSessionId
    let scope: MeetingQuestionScope
    let question: String?
    let state: MeetingAnswerState
    let answer: String?
    let citations: [MeetingCitation]
    let inputRevision: Int
    let revision: Int
    let createdAtUtcMs: Int64
    let throughOffsetNs: Int64?
    let provisional: Bool

    var id: MeetingQuestionId { questionId }
}

// MARK: - The review read

struct MeetingReviewSnapshot: Decodable {
    let session: MeetingSessionSnapshot
    let tracks: [MeetingTrackSnapshot]
    let gaps: [MeetingSourceGap]
    let speakers: [MeetingSpeaker]
    let transcript: [TranscriptEffectiveSegment]
    let notes: [MeetingManualNote]
    let artifacts: [MeetingArtifactRevision]
    let questions: [MeetingAnswer]
    let diarization: MeetingDiarizationSnapshot
    let canExport: Bool
    let remoteCancellationPending: Bool

    /// `currentLedger` in meetingLedger.ts: the newest current revision's
    /// ledger. Revisions arrive newest first and one generated before ledgers
    /// existed carries none.
    var currentLedger: MeetingLedger? {
        artifacts.first { $0.state == .current && $0.content?.ledger != nil }?.content?.ledger
    }

    /// The newest current revision, which is what the notes panes read.
    var currentArtifact: MeetingArtifactRevision? {
        artifacts.first { $0.state == .current }
    }

    /// The speaker names by id, for the transcript and the talk-time rows.
    var speakerNames: [SpeakerId: String] {
        Dictionary(speakers.map { ($0.speakerId, $0.displayName) }, uniquingKeysWith: { first, _ in first })
    }
}

// MARK: - The list

enum MeetingHistoryItemKind: String, Decodable {
    case meeting
}

/// `MeetingHistoryHeadline`: which of the three real sources line two of a
/// list row came from.
enum MeetingHistoryHeadline: Decodable {
    case none
    case ledger(String)
    case summary(String)
    case words(Int)

    private enum Key: String, CodingKey { case kind, text, words }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let kind = try container.decode(String.self, forKey: .kind)
        switch kind {
        case "none": self = .none
        case "ledger": self = .ledger(try container.decode(String.self, forKey: .text))
        case "summary": self = .summary(try container.decode(String.self, forKey: .text))
        case "words": self = .words(try container.decode(Int.self, forKey: .words))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: container, debugDescription: "unknown headline kind \(kind)")
        }
    }

    /// The line a row prints, and nothing when there is nothing true to say.
    var text: String? {
        switch self {
        case .none: nil
        case let .ledger(text): text
        case let .summary(text): text
        case let .words(words): "\(words) words said"
        }
    }
}

struct MeetingHistorySummary: Decodable, Identifiable {
    let kind: MeetingHistoryItemKind
    let sessionId: MeetingSessionId
    let title: String
    let phase: MeetingPhase
    let createdAtUtcMs: Int64
    let captureCompleteness: MeetingCaptureCompleteness
    let processingStatus: MeetingProcessingStatus
    let recordedDurationMs: Int64?
    let sources: [MeetingSourceKind]?
    let speakerLabels: [String]?
    let headline: MeetingHistoryHeadline?

    var id: MeetingSessionId { sessionId }
    var date: Date { Date(timeIntervalSince1970: TimeInterval(createdAtUtcMs) / 1000) }
    var recordedDuration: TimeInterval? {
        recordedDurationMs.map { TimeInterval($0) / 1000 }
    }
}

/// `PaginatedMeetings`.
struct MeetingsPage: Decodable {
    let entries: [MeetingHistorySummary]
    let hasMore: Bool
}

enum MeetingStatusFilter: String, Decodable, CaseIterable {
    case any
    case ready
    case processing
    case failed

    var label: String {
        switch self {
        case .any: "Any state"
        case .ready: "Ready"
        case .processing: "Processing"
        case .failed: "Failed"
        }
    }
}

enum MeetingTimeWindow: String, Decodable, CaseIterable {
    case any
    case today
    case last7Days = "last_7_days"
    case last30Days = "last_30_days"

    var label: String {
        switch self {
        case .any: "Any time"
        case .today: "Today"
        case .last7Days: "Last 7 days"
        case .last30Days: "Last 30 days"
        }
    }
}

/// `DashboardTrendRange`.
enum MeetingTrendRange: String, Decodable, CaseIterable {
    case days7 = "days_7"
    case days30 = "days_30"
    case days180 = "days_180"

    var label: String {
        switch self {
        case .days7: "7 days"
        case .days30: "30 days"
        case .days180: "180 days"
        }
    }
}

struct MeetingTrendTotals: Decodable {
    let meetings: Int
    let verifiedCapturedDurationMs: Int64
    let transcriptSegments: Int
    let generatedActionItems: Int
}

struct MeetingTrendPoint: Decodable, Identifiable {
    let localDate: String
    let meetings: Int
    let verifiedCapturedDurationMs: Int64
    let transcriptSegments: Int
    let generatedActionItems: Int

    var id: String { localDate }
}

/// `MeetingTrendProjection`: a storage failure has no zero-valued range, so
/// unavailable is its own answer rather than an empty chart.
enum MeetingTrendProjection: Decodable {
    case available(
        range: MeetingTrendRange,
        rangeStartLocalDate: String,
        rangeEndLocalDate: String,
        allTime: MeetingTrendTotals,
        rangeTotal: MeetingTrendTotals,
        points: [MeetingTrendPoint]
    )
    case unavailable(range: MeetingTrendRange)

    private enum Key: String, CodingKey {
        case status, range, rangeStartLocalDate, rangeEndLocalDate, allTime, rangeTotal, points
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let status = try container.decode(String.self, forKey: .status)
        let range = try container.decode(MeetingTrendRange.self, forKey: .range)
        switch status {
        case "available":
            self = .available(
                range: range,
                rangeStartLocalDate: try container.decode(String.self, forKey: .rangeStartLocalDate),
                rangeEndLocalDate: try container.decode(String.self, forKey: .rangeEndLocalDate),
                allTime: try container.decode(MeetingTrendTotals.self, forKey: .allTime),
                rangeTotal: try container.decode(MeetingTrendTotals.self, forKey: .rangeTotal),
                points: try container.decode([MeetingTrendPoint].self, forKey: .points)
            )
        case "unavailable":
            self = .unavailable(range: range)
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .status, in: container, debugDescription: "unknown trend status \(status)")
        }
    }
}

struct MeetingSearchHit: Decodable, Identifiable {
    let sessionId: MeetingSessionId
    let kind: MeetingCitationKind
    let entityId: String
    let startOffsetNs: Int64?
    let endOffsetNs: Int64?
    let excerpt: String

    var id: String { "\(sessionId):\(entityId)" }
}

struct MeetingSearchResult: Decodable {
    let entries: [MeetingSearchHit]
}

/// The meetings a person deleted and can still get back.
struct MeetingTrashEntry: Decodable, Identifiable {
    let jobId: MeetingDeletionJobId
    let title: String
    let deletedAtUtcMs: Int64
    let expiresAtUtcMs: Int64

    var id: MeetingDeletionJobId { jobId }
    var deletedAt: Date { Date(timeIntervalSince1970: TimeInterval(deletedAtUtcMs) / 1000) }
    var expiresAt: Date { Date(timeIntervalSince1970: TimeInterval(expiresAtUtcMs) / 1000) }
}

// MARK: - Export

enum MeetingExportFormat: String, Decodable, CaseIterable {
    case json
    case markdown

    var fileExtension: String { self == .json ? "json" : "md" }
    var label: String { self == .json ? "JSON" : "Markdown" }
}

struct MeetingExportReceipt: Decodable {
    let exportReceiptId: MeetingExportReceiptId
    let sessionId: MeetingSessionId
    let format: MeetingExportFormat
    let snapshotRevision: Int
    let captureCompleteness: MeetingCaptureCompleteness
    let transcriptRevisionId: TranscriptRevisionId?
    let createdAtUtcMs: Int64
}

struct MeetingExportResult: Decodable {
    let receipt: MeetingOperationReceipt
    let exportReceipt: MeetingExportReceipt
}

// MARK: - Analytics and the user's own notes

enum MeetingNotesTemplate: String, Decodable, CaseIterable {
    case general
    case oneOnOne = "one_on_one"
    case interview
    case salesCall = "sales_call"
    case standup

    var label: String {
        switch self {
        case .general: "General"
        case .oneOnOne: "One on one"
        case .interview: "Interview"
        case .salesCall: "Sales call"
        case .standup: "Standup"
        }
    }
}

struct SpeakerTalkShare: Decodable, Identifiable {
    let speakerId: SpeakerId
    let speakingNs: Int64
    let sharePermille: Int
    let turnCount: Int
    let longestMonologueNs: Int64

    var id: SpeakerId { speakerId }
}

struct MeetingTalkMetrics: Decodable {
    let segmentCount: Int
    let turnCount: Int
    let interactionCount: Int
    let totalSpeakingNs: Int64
    let speakers: [SpeakerTalkShare]
    let longestMonologueNs: Int64
    let longestMonologueSpeakerId: SpeakerId?
    let medianSwitchGapMs: Int64?
}

/// `TrackerResult`: how often one keyword tracker hit.
struct MeetingTrackerResult: Decodable, Identifiable {
    let name: String
    let hitCount: Int
    let segmentIds: [TranscriptSegmentId]

    var id: String { name }
}

struct MeetingAnalytics: Decodable {
    let talk: MeetingTalkMetrics
    let trackers: [MeetingTrackerResult]
}

struct MeetingActionItemState: Decodable {
    let artifactId: MeetingArtifactId
    let actionIndex: Int
    let done: Bool

    /// `actionItemKey`: unique across regenerated revisions.
    var key: String { "\(artifactId):\(actionIndex)" }
}

struct MeetingUserNotes: Decodable {
    let sessionId: MeetingSessionId
    let body: String
    let template: MeetingNotesTemplate
    let revision: Int
    let updatedAtUtcMs: Int64
}

struct MeetingAnalyticsSnapshot: Decodable {
    let sessionId: MeetingSessionId
    let inputRevision: Int
    let computedAtUtcMs: Int64
    let analytics: MeetingAnalytics
    let actionItems: [MeetingActionItemState]
    let notes: MeetingUserNotes
}

// MARK: - Catch up

enum MeetingCatchUpState: String, Decodable {
    case ready
    case noTranscriptYet = "no_transcript_yet"
    case modelUnavailable = "model_unavailable"
    case failed

    var label: String {
        switch self {
        case .ready: "Caught up"
        case .noTranscriptYet: "Nothing has been said yet."
        case .modelUnavailable: "No local model is installed, so Sona cannot recap this."
        case .failed: "The recap failed."
        }
    }
}

struct MeetingCatchUp: Decodable {
    let state: MeetingCatchUpState
    let bullets: [String]
    let throughOffsetNs: Int64?
    let segmentCount: Int
    let provisional: Bool
}

// MARK: - Follow up

enum MeetingFollowUpSource: String, Decodable {
    case generated
    case structured

    /// What the sheet says about who wrote the draft.
    var label: String {
        switch self {
        case .generated: "Written by the model from this meeting."
        case .structured: "No model was available, so this is the record verbatim."
        }
    }
}

enum MeetingFollowUpMailBody: String, Decodable {
    case draft
    case clipboard
}

struct MeetingFollowUpDraft: Decodable {
    let sessionId: MeetingSessionId
    let title: String
    let source: MeetingFollowUpSource
    let message: String?
    let summary: String
    let mine: [String]
    let decisions: [String]
    let receipt: MeetingOperationReceipt

    /// The words the compose window carries: the model's message, or the
    /// record in `followUpDraftText`'s order — what was said, what I owe,
    /// then what was decided — with a blank line between sections.
    var body: String {
        if let message, !message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            return message
        }
        var sections: [String] = []
        if !summary.isEmpty {
            sections.append(summary)
        }
        if !mine.isEmpty {
            sections.append((["What I owe"] + mine.map { "- \($0)" }).joined(separator: "\n"))
        }
        if !decisions.isEmpty {
            sections.append((["What we decided"] + decisions.map { "- \($0)" }).joined(separator: "\n"))
        }
        return sections.joined(separator: "\n\n")
    }
}

struct MeetingFollowUpMail: Decodable {
    let url: String
    let body: MeetingFollowUpMailBody
}

// MARK: - Loops

enum MeetingLoopKind: String, Decodable {
    case loop
    case commitment

    var label: String { self == .loop ? "Open loop" : "Commitment" }
}

enum MeetingLoopDirection: String, Decodable, CaseIterable {
    case mine
    case waitingOn = "waiting_on"
    case unattributed

    var label: String {
        switch self {
        case .mine: "I owe"
        case .waitingOn: "Waiting on them"
        case .unattributed: "Nobody's yet"
        }
    }
}

enum MeetingLoopStatus: String, Decodable {
    case open
    case done
    case dropped
    case carried

    var label: String {
        switch self {
        case .open: "Open"
        case .done: "Done"
        case .dropped: "Dropped"
        case .carried: "Carried forward"
        }
    }
}

enum MeetingLoopResolution: String, Decodable {
    case done
    case dropped
}

/// One actionable ledger row: the words from the artifact, the state from the
/// store.
struct MeetingLoopRow: Decodable, Identifiable {
    let loopId: MeetingLoopId
    let sessionId: MeetingSessionId
    let kind: MeetingLoopKind
    let text: String
    let ownerText: String?
    let ownerPersonId: MeetingPersonId?
    let ownerDisplayName: String?
    let direction: MeetingLoopDirection
    let status: MeetingLoopStatus
    let resolvedAtUtcMs: Int64?
    let resolvingOperationId: String?
    let resolvedBy: MeetingOperationActor?
    let carriedIntoLoopId: MeetingLoopId?
    let carriedSinceAtUtcMs: Int64?
    let atMs: Int64
    let revision: Int
    let instead: String?
    let firmness: LedgerFirmness?
    let quote: String?
    let speaker: String?
    let citations: [ArtifactCitation]

    var id: MeetingLoopId { loopId }
    /// The name to print for the owner: the person a user picked, else the
    /// name the ledger read out of the transcript.
    var owner: String? { ownerDisplayName ?? ownerText }
}

struct MeetingLoopsResult: Decodable {
    let schemaVersion: Int
    let revision: Int
    let rows: [MeetingLoopRow]
}

struct MeetingLoopMutationResult: Decodable {
    let receipt: MeetingOperationReceipt
    let loops: MeetingLoopsResult
}

// MARK: - People context

/// `PersonLinkSource`.
enum MeetingPersonLinkSource: String, Decodable {
    case calendar
    case speaker
    case title
    case manual

    var label: String {
        switch self {
        case .calendar: "from the calendar"
        case .speaker: "from a speaker label"
        case .title: "from the title"
        case .manual: "linked by hand"
        }
    }
}

/// `PersonBriefingLastMeeting`.
struct MeetingPersonLastMeeting: Decodable {
    let id: MeetingSessionId
    let title: String
    let atUtcMs: Int64
    let headline: String?

    var date: Date { Date(timeIntervalSince1970: TimeInterval(atUtcMs) / 1000) }
}

/// `PersonOpenLoop`, as the review screen's people strip reads it.
struct MeetingPersonOpenLoop: Decodable {
    let loopId: MeetingLoopId
    let meetingId: MeetingSessionId
    let title: String
    let atUtcMs: Int64
    let text: String
    let ownerPersonId: MeetingPersonId?
    let status: MeetingLoopStatus
    let direction: MeetingLoopDirection
    let waitingOnStale: Bool
    let carriedSinceAtUtcMs: Int64?
    let carriedIntoMeetingId: MeetingSessionId?
}

struct MeetingPersonContextRow: Decodable, Identifiable {
    let personId: MeetingPersonId
    let displayName: String
    let evidenceSource: MeetingPersonLinkSource
    let meetingsTogether: Int
    let lastPriorMeeting: MeetingPersonLastMeeting?
    let topOpenLoop: MeetingPersonOpenLoop?

    var id: MeetingPersonId { personId }
}

struct MeetingPeopleContextResult: Decodable {
    let schemaVersion: Int
    let revision: Int
    let rows: [MeetingPersonContextRow]
}

// MARK: - Start-flow vocabulary

/// `MeetingOrigin`: how a session came to exist.
enum MeetingOrigin: String, Decodable {
    case manual
    case suggestion
    case cli
    case importedFile = "import"
}

enum MeetingProvider: String, Decodable {
    case zoom
    case googleMeet = "google_meet"
    case microsoftTeams = "microsoft_teams"
    case webex
    case slackHuddle = "slack_huddle"
    case faceTime = "face_time"
    case configuredApp = "configured_app"

    /// `meetings.providers.*`.
    var label: String {
        switch self {
        case .zoom: "Zoom"
        case .googleMeet: "Google Meet"
        case .microsoftTeams: "Microsoft Teams"
        case .webex: "Webex"
        case .slackHuddle: "Slack huddle"
        case .faceTime: "FaceTime"
        case .configuredApp: "A configured app"
        }
    }
}

/// `DegradedStartPolicy`.
enum MeetingDegradedStartPolicy: String, Decodable {
    case abortIfRequiredSourceFails = "abort_if_required_source_fails"
    case continueAndMarkPartial = "continue_and_mark_partial"
}

/// `ProcessingDestination`: where the words get turned into notes.
enum MeetingProcessingDestination: Decodable {
    case local
    case remote(destinationId: String)

    private enum Key: String, CodingKey { case kind, destinationId }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let kind = try container.decode(String.self, forKey: .kind)
        switch kind {
        case "local": self = .local
        case "remote": self = .remote(destinationId: try container.decode(String.self, forKey: .destinationId))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: container, debugDescription: "unknown destination \(kind)")
        }
    }
}

// MARK: - Request bodies

/// Every meeting command's params, in the exact wire keys. The command
/// argument names are camelCase (`sessionId`, `cursorUtcMs`, `request`) and the
/// request structs inside them keep serde's snake_case, so both spellings live
/// here rather than being guessed at each call site.
enum MeetingRequest {
    /// A fresh operation id. Every mutation carries one, and a repeat of the
    /// same id is the core's duplicate-operation answer rather than a second
    /// write.
    static func operationId() -> MeetingOperationId { UUID().uuidString.lowercased() }

    /// `MeetingMutationRequest`, which nine commands take unchanged.
    static func mutation(_ sessionId: MeetingSessionId, revision: Int) -> [String: JSONValue] {
        [
            "operation_id": .string(operationId()),
            "session_id": .string(sessionId),
            "expected_revision": .number(Double(revision)),
        ]
    }

    /// The `{"request": {...}}` envelope the dispatcher takes.
    static func wrap(_ request: [String: JSONValue]) -> [String: JSONValue] {
        ["request": .object(request)]
    }

    static func session(_ sessionId: MeetingSessionId) -> [String: JSONValue] {
        ["sessionId": .string(sessionId)]
    }

    static func list(cursorUtcMs: Int64?, limit: Int, titleQuery: String,
                     status: MeetingStatusFilter, window: MeetingTimeWindow) -> [String: JSONValue] {
        [
            "cursorUtcMs": cursorUtcMs.map { JSONValue.number(Double($0)) } ?? .null,
            "limit": .number(Double(limit)),
            "filter": .object([
                "status": .string(status.rawValue),
                "window": .string(window.rawValue),
                "title_query": .string(titleQuery),
            ]),
        ]
    }

    static func trend(_ range: MeetingTrendRange) -> [String: JSONValue] {
        wrap(["range": .string(range.rawValue)])
    }

    static func search(query: String, sessionIds: [MeetingSessionId], limit: Int) -> [String: JSONValue] {
        wrap([
            "query": .string(query),
            "session_ids": .array(sessionIds.map { .string($0) }),
            "limit": .number(Double(limit)),
        ])
    }

    static func titleSet(_ sessionId: MeetingSessionId, revision: Int, title: String) -> [String: JSONValue] {
        var request = mutation(sessionId, revision: revision)
        request["title"] = .string(title)
        return wrap(request)
    }

    static func speakerRename(_ sessionId: MeetingSessionId, revision: Int,
                              speakerId: SpeakerId, displayName: String) -> [String: JSONValue] {
        var request = mutation(sessionId, revision: revision)
        request["speaker_id"] = .string(speakerId)
        request["display_name"] = .string(displayName)
        return wrap(request)
    }

    static func speakerMerge(_ sessionId: MeetingSessionId, revision: Int,
                             source: SpeakerId, target: SpeakerId) -> [String: JSONValue] {
        var request = mutation(sessionId, revision: revision)
        request["source_speaker_id"] = .string(source)
        request["target_speaker_id"] = .string(target)
        return wrap(request)
    }

    static func segmentEdit(_ sessionId: MeetingSessionId, revision: Int, segmentId: TranscriptSegmentId,
                            replacementText: String, removed: Bool) -> [String: JSONValue] {
        var request = mutation(sessionId, revision: revision)
        request["segment_id"] = .string(segmentId)
        request["replacement_text"] = .string(replacementText)
        request["removed"] = .bool(removed)
        return wrap(request)
    }

    static func noteCreate(_ sessionId: MeetingSessionId, revision: Int,
                           startOffsetNs: Int64?, body: String) -> [String: JSONValue] {
        var request = mutation(sessionId, revision: revision)
        request["start_offset_ns"] = startOffsetNs.map { JSONValue.number(Double($0)) } ?? .null
        request["end_offset_ns"] = .null
        request["body"] = .string(body)
        return wrap(request)
    }

    static func noteUpdate(_ sessionId: MeetingSessionId, revision: Int, note: MeetingManualNote,
                           body: String) -> [String: JSONValue] {
        var request = mutation(sessionId, revision: revision)
        request["note_id"] = .string(note.noteId)
        request["expected_note_revision"] = .number(Double(note.revision))
        request["start_offset_ns"] = note.startOffsetNs.map { JSONValue.number(Double($0)) } ?? .null
        request["end_offset_ns"] = note.endOffsetNs.map { JSONValue.number(Double($0)) } ?? .null
        request["body"] = .string(body)
        return wrap(request)
    }

    static func noteDelete(_ sessionId: MeetingSessionId, revision: Int,
                           note: MeetingManualNote) -> [String: JSONValue] {
        var request = mutation(sessionId, revision: revision)
        request["note_id"] = .string(note.noteId)
        request["expected_note_revision"] = .number(Double(note.revision))
        return wrap(request)
    }

    static func questionForget(_ sessionId: MeetingSessionId, revision: Int,
                               questionId: MeetingQuestionId) -> [String: JSONValue] {
        [
            "request": .object(mutation(sessionId, revision: revision)),
            "questionId": .string(questionId),
        ]
    }

    static func export(_ sessionId: MeetingSessionId, revision: Int,
                       format: MeetingExportFormat) -> [String: JSONValue] {
        var request = mutation(sessionId, revision: revision)
        request["format"] = .string(format.rawValue)
        return wrap(request)
    }

    static func actionItemDone(_ sessionId: MeetingSessionId, artifactId: MeetingArtifactId,
                               actionIndex: Int, done: Bool) -> [String: JSONValue] {
        wrap([
            "session_id": .string(sessionId),
            "artifact_id": .string(artifactId),
            "action_index": .number(Double(actionIndex)),
            "done": .bool(done),
        ])
    }

    static func userNotesSave(_ sessionId: MeetingSessionId, body: String, template: MeetingNotesTemplate,
                              expectedNoteRevision: Int) -> [String: JSONValue] {
        wrap([
            "session_id": .string(sessionId),
            "body": .string(body),
            "template": .string(template.rawValue),
            "expected_note_revision": .number(Double(expectedNoteRevision)),
        ])
    }

    static func reenhance(_ sessionId: MeetingSessionId, revision: Int, body: String,
                          template: MeetingNotesTemplate, expectedNoteRevision: Int) -> [String: JSONValue] {
        var request = mutation(sessionId, revision: revision)
        request["body"] = .string(body)
        request["template"] = .string(template.rawValue)
        request["expected_note_revision"] = .number(Double(expectedNoteRevision))
        return wrap(request)
    }

    static func followUpDraft(_ sessionId: MeetingSessionId) -> [String: JSONValue] {
        ["operationId": .string(operationId()), "sessionId": .string(sessionId)]
    }

    static func followUpMail(_ sessionId: MeetingSessionId, body: String,
                             overBoundNote: String) -> [String: JSONValue] {
        wrap([
            "session_id": .string(sessionId),
            "body": .string(body),
            "over_bound_note": .string(overBoundNote),
        ])
    }

    static func loopResolve(_ loopId: MeetingLoopId, revision: Int,
                            resolution: MeetingLoopResolution) -> [String: JSONValue] {
        wrap([
            "operation_id": .string(operationId()),
            "loop_id": .string(loopId),
            "expected_revision": .number(Double(revision)),
            "resolution": .string(resolution.rawValue),
        ])
    }

    static func loopReopen(_ loopId: MeetingLoopId, revision: Int) -> [String: JSONValue] {
        wrap([
            "operation_id": .string(operationId()),
            "loop_id": .string(loopId),
            "expected_revision": .number(Double(revision)),
        ])
    }

    static func loopAssign(_ loopId: MeetingLoopId, revision: Int,
                           ownerPersonId: MeetingPersonId?) -> [String: JSONValue] {
        wrap([
            "operation_id": .string(operationId()),
            "loop_id": .string(loopId),
            "expected_revision": .number(Double(revision)),
            "owner_person_id": ownerPersonId.map { JSONValue.string($0) } ?? .null,
        ])
    }

    static func trashRestore(_ jobId: MeetingDeletionJobId) -> [String: JSONValue] {
        ["jobId": .string(jobId)]
    }
}

// MARK: - Reading the wire

extension Int64 {
    /// A nanosecond offset into a meeting as a clock reading:
    /// `formatMeetingOffset` in meetingUtils.ts.
    var meetingOffsetClock: String { (TimeInterval(self) / 1_000_000_000).clock }

    /// A millisecond offset into a meeting, the same way.
    var meetingMsClock: String { (TimeInterval(self) / 1000).clock }

    var meetingDate: Date { Date(timeIntervalSince1970: TimeInterval(self) / 1000) }

    /// `formatTalkDuration`: seconds under a minute, m:ss above it.
    var meetingTalkDuration: String {
        let seconds = Int((Double(self) / 1_000_000_000).rounded())
        return seconds < 60 ? "\(seconds)s" : (TimeInterval(seconds)).clock
    }
}

extension Int {
    /// `formatTalkShare`: per-mille to the whole percent a strip needs.
    var meetingTalkShare: String { "\(Int((Double(self) / 10).rounded()))%" }
}

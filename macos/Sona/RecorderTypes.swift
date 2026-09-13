import Foundation

/// The screen recorder's wire shapes and its state machine, ported from
/// `src-tauri/src/recorder.rs` and `src/components/recorder/recorderMachine.ts`.
/// Every Rust type here carries `serde(rename_all = "camelCase")`, so the enum
/// raw values below are the exact wire strings.

// MARK: - The wire

enum RecorderPhase: String, Decodable, Equatable {
    case checking
    case permission
    case idle
    case selectingSource
    case previewing
    case starting
    case recording
    case paused
    case finalizing
    case saved
    case failed
}

struct RecorderDevice: Decodable, Identifiable, Hashable {
    let id: String
    let name: String
}

enum RecorderAvailability: String, Decodable {
    case supported
    case unsupported
}

enum RecorderStartAvailability: String, Decodable {
    case ready
    case captureBusy
}

struct RecorderPreflight: Decodable {
    let availability: RecorderAvailability
    let startAvailability: RecorderStartAvailability
    let cameraDevices: [RecorderDevice]
    let microphoneDevices: [RecorderDevice]
}

/// The `request` parameter of `recorder_preview_start`. The optional device ids
/// are written as explicit nulls, the way the React dialog sent them.
struct RecorderStartRequest: Encodable {
    let cameraEnabled: Bool
    let cameraDeviceId: String?
    let microphoneEnabled: Bool
    let microphoneDeviceId: String?

    private enum Key: String, CodingKey {
        case cameraEnabled, cameraDeviceId, microphoneEnabled, microphoneDeviceId
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        try container.encode(cameraEnabled, forKey: .cameraEnabled)
        try container.encode(cameraDeviceId, forKey: .cameraDeviceId)
        try container.encode(microphoneEnabled, forKey: .microphoneEnabled)
        try container.encode(microphoneDeviceId, forKey: .microphoneDeviceId)
    }
}

enum RecorderFailureCode: String, Decodable, Equatable {
    case unsupported
    case captureBusy
    case screenPermissionDenied
    case cameraPermissionDenied
    case microphonePermissionDenied
    case sourceSelectionCancelled
    case sourceUnavailable
    case cameraUnavailable
    case microphoneUnavailable
    case streamFailed
    case timestampDiscontinuity
    case writerFailed
    case outputFinalizeFailed
    case outputCommitFailed
}

struct RecorderSnapshot: Decodable, Equatable {
    var phase: RecorderPhase
    var elapsedMs: UInt64
    var screenSelected: Bool
    var droppedVideoFrames: UInt64
    var outputPath: String?
    var width: Int?
    var height: Int?
    var failure: RecorderFailureCode?
}

/// The error type of every recorder command but `recorder_preflight`.
enum RecorderCommandError: String, Decodable {
    case invalidState
}

/// The payload of `recorder-state-changed-event`.
struct RecorderStateChangedEvent: Decodable {
    let snapshot: RecorderSnapshot
}

// MARK: - Phases

extension RecorderPhase {
    /// `phaseCapabilities[...].closable`: one answer per phase, stated
    /// exhaustively so a new phase fails the build instead of quietly becoming
    /// dismissible in the middle of a capture.
    var closable: Bool {
        switch self {
        case .checking, .permission, .idle, .previewing, .saved, .failed: true
        case .selectingSource, .starting, .recording, .paused, .finalizing: false
        }
    }

    /// `phaseCapabilities[...].capture`: the phase owns a live native stream.
    var capture: Bool {
        switch self {
        case .previewing, .starting, .recording, .paused, .finalizing, .saved: true
        case .checking, .permission, .idle, .selectingSource, .failed: false
        }
    }

    /// The `recorder.phase.*` copy.
    var label: String {
        switch self {
        case .checking: "Checking"
        case .permission: "Permission needed"
        case .idle: "Set up"
        case .selectingSource: "Selecting source"
        case .previewing: "Preview ready"
        case .starting: "Starting recording"
        case .recording: "Recording"
        case .paused: "Paused"
        case .finalizing: "Saving recording"
        case .saved: "Saved"
        case .failed: "Needs attention"
        }
    }
}

// MARK: - Failures

/// What the dialog offers after a failure: `recorderFailureRecovery`.
enum RecorderRecovery {
    case done
    case permission
    case choose
    case retry
}

extension RecorderFailureCode {
    /// The `recorder.error.*` copy, in plain English.
    var message: String {
        switch self {
        case .unsupported: "Screen recording requires macOS 14 or later."
        case .captureBusy: "Another Sona capture is using the microphone."
        case .screenPermissionDenied: "Screen recording access was denied."
        case .cameraPermissionDenied: "Camera access was denied."
        case .microphonePermissionDenied: "Microphone access was denied."
        case .sourceSelectionCancelled: "Source selection was cancelled."
        case .sourceUnavailable: "The selected screen is no longer available."
        case .cameraUnavailable: "The selected camera is no longer available."
        case .microphoneUnavailable: "The selected microphone is no longer available."
        case .streamFailed: "The recording stream stopped unexpectedly."
        case .timestampDiscontinuity: "The recording timeline became invalid."
        case .writerFailed: "The recording could not be written."
        case .outputFinalizeFailed: "The recording could not be finalized."
        case .outputCommitFailed: "The recording could not be saved."
        }
    }

    var recovery: RecorderRecovery {
        switch self {
        case .unsupported: .done
        case .captureBusy, .cameraUnavailable, .microphoneUnavailable, .streamFailed,
             .timestampDiscontinuity, .writerFailed, .outputFinalizeFailed, .outputCommitFailed: .retry
        case .screenPermissionDenied, .cameraPermissionDenied, .microphonePermissionDenied: .permission
        case .sourceSelectionCancelled, .sourceUnavailable: .choose
        }
    }
}

/// The three accesses a recording needs. Which one is missing decides the
/// notice, the grant button and the System Settings pane.
enum RecorderPermission: String {
    case screen
    case camera
    case microphone

    /// `permissionForFailure`: the backend routes every permission code to the
    /// Permission phase, so the denied access arrives as the snapshot's
    /// failure rather than as a field of its own.
    init?(failure: RecorderFailureCode?) {
        switch failure {
        case .screenPermissionDenied: self = .screen
        case .cameraPermissionDenied: self = .camera
        case .microphonePermissionDenied: self = .microphone
        default: return nil
        }
    }

    /// The `recorder.permission.*` copy.
    var request: String {
        switch self {
        case .screen: "Allow screen recording to choose a source."
        case .camera: "Allow camera access to include your camera."
        case .microphone: "Allow microphone access to include your microphone."
        }
    }
}

// MARK: - Snapshot helpers

extension RecorderSnapshot {
    /// `emptySnapshot()`.
    static let empty = RecorderSnapshot(
        phase: .checking,
        elapsedMs: 0,
        screenSelected: false,
        droppedVideoFrames: 0,
        outputPath: nil,
        width: nil,
        height: nil,
        failure: nil)

    /// `failureSnapshot`: the phase becomes failed with the code attached, and
    /// every other fact of the capture survives.
    func failing(_ failure: RecorderFailureCode) -> RecorderSnapshot {
        var next = self
        next.phase = .failed
        next.failure = failure
        return next
    }

    /// `idleSnapshot`: back to setup, with the clock and the chosen screen gone.
    var idled: RecorderSnapshot {
        var next = self
        next.phase = .idle
        next.failure = nil
        next.elapsedMs = 0
        next.screenSelected = false
        return next
    }

    /// `recorderHasCapture`: a chosen screen is capture the phase alone does
    /// not report, because the picker has already handed the app a source.
    var hasCapture: Bool { screenSelected || phase.capture }

    /// The file name of the saved recording.
    var outputName: String? {
        outputPath.map { URL(fileURLWithPath: $0).lastPathComponent }
    }

    /// "1920 × 1080", when the core measured the output.
    var dimensions: String? {
        guard let width, let height else { return nil }
        return "\(width) × \(height)"
    }

    /// `recorderCommandErrorFallback`: a command that failed says nothing new
    /// when a native snapshot has already named the failure, so only an
    /// unexplained refusal invents `streamFailed`.
    static func commandFailureFallback(latest: RecorderSnapshot?) -> RecorderFailureCode? {
        latest?.failure != nil ? nil : .streamFailed
    }
}

// MARK: - The machine

/// `RecorderUiState`: what the dialog knows between two events.
struct RecorderUiState {
    var snapshot: RecorderSnapshot = .empty
    var preflight: RecorderPreflight?
    var permission: RecorderPermission?
    var permissionRequested = false
    var revealFailed = false
}

/// `RecorderAction`.
enum RecorderAction {
    case checking
    case preflight(RecorderPreflight)
    case permission(RecorderPermission, requested: Bool)
    case snapshot(RecorderSnapshot)
    case failure(RecorderFailureCode)
    case phase(RecorderPhase)
    case revealFailed
    case clearFailure
    case reset
}

extension RecorderUiState {
    /// `recorderReducer`, transition for transition.
    func applying(_ action: RecorderAction) -> RecorderUiState {
        var next = self
        switch action {
        case .checking:
            next.snapshot.phase = .checking
            next.snapshot.failure = nil
            next.permission = nil
            next.permissionRequested = false
            next.revealFailed = false
        case let .preflight(preflight):
            next.preflight = preflight
            if preflight.availability == .unsupported {
                next.snapshot = snapshot.failing(.unsupported)
            } else if preflight.startAvailability == .captureBusy {
                next.snapshot = snapshot.failing(.captureBusy)
            } else {
                next.snapshot = snapshot.idled
            }
            next.permission = nil
            next.permissionRequested = false
            next.revealFailed = false
        case let .permission(required, requested):
            next.snapshot.phase = .permission
            next.snapshot.failure = nil
            next.permission = required
            next.permissionRequested = requested
        case let .snapshot(native):
            /* A cancelled native picker is not a failure the user has to clear:
             * it returns the sheet to setup with the input choices intact. */
            if native.failure == .sourceSelectionCancelled {
                next.snapshot = native.idled
                next.permission = nil
            } else {
                next.snapshot = native
                next.permission = RecorderPermission(failure: native.failure)
            }
            next.permissionRequested = false
            next.revealFailed = false
        case let .failure(failure):
            next.snapshot = snapshot.failing(failure)
            next.permission = nil
            next.permissionRequested = false
        case let .phase(phase):
            next.snapshot.phase = phase
            next.snapshot.failure = nil
        case .revealFailed:
            next.revealFailed = true
        case .clearFailure:
            next.snapshot = snapshot.idled
            next.permission = nil
            next.permissionRequested = false
            next.revealFailed = false
        case .reset:
            next = RecorderUiState()
        }
        return next
    }
}

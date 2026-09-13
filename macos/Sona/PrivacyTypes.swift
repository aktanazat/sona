import Foundation

/// The wire shapes behind Privacy, file import, documents and the query
/// plane. Field names mirror the Rust structs with snake_case turned into
/// camelCase by `Core.decoder`; enum values are the wire strings verbatim.

/// The events this slice listens to, beyond the ones `CoreTypes` declares.
extension CoreEvent {
    /// Carries nothing useful: on it the store re-reads `get_app_settings`.
    static let privacySettingsChanged = "settings-changed"
    static let privacyCloudSyncChanged = "cloud-sync:changed"
    static let upstreamImportProgressEvent = "upstream-import-progress-event"
    static let audioImportUpdateEvent = "audio-import-update-event"
    static let audioImportRoutedEvent = "audio-import-routed-event"
    static let queryLinkRequested = "query:link-requested"
}

// MARK: - Context capture

/// How much of the app you are dictating into a mode may read. The ceiling is
/// the global maximum; a mode can ask for less, never more.
enum ContextPolicy: String, Codable, CaseIterable, Hashable {
    case none
    case target
    case targetAndSelection = "target_and_selection"
    case full

    var label: String {
        switch self {
        case .none: "Off"
        case .target: "App"
        case .targetAndSelection: "Selection"
        case .full: "Full"
        }
    }

    /// What the selected level actually reads, as the ceiling row says it.
    var sentence: String {
        switch self {
        case .none: "Reads nothing from your other apps."
        case .target: "Reads the frontmost app identity."
        case .targetAndSelection: "Reads the frontmost app identity and selected text when available."
        case .full: "May also read a permitted browser URL and recently changed clipboard text when available."
        }
    }
}

/// Whether the OS lets Sona read other apps at all.
enum ContextAccessibilityAccess: String, Decodable {
    case granted
    case denied
    case unsupported

    var word: String {
        switch self {
        case .granted: "Granted"
        case .denied: "Denied"
        case .unsupported: "Unsupported"
        }
    }
}

/// The outcome of reading one context source on the last capture.
enum ContextSourceStatus: String, Decodable {
    case notRequested = "not_requested"
    case captured
    case empty
    case unsupported
    case permissionDenied = "permission_denied"
    case disabled
    case disabledByCeiling = "disabled_by_ceiling"
    case secureField = "secure_field"
    case stale
    case failed

    var word: String {
        switch self {
        case .notRequested: "Not requested"
        case .captured: "Available"
        case .empty: "Empty"
        case .unsupported: "Unsupported"
        case .permissionDenied: "Permission denied"
        case .disabled: "Disabled"
        case .disabledByCeiling: "Blocked by ceiling"
        case .secureField: "Secure field"
        case .stale: "Stale"
        case .failed: "Unavailable"
        }
    }

    /// A source that is off for a reason the reader chose reads quietly; one
    /// the system refused reads live.
    var isRefusal: Bool {
        self == .permissionDenied || self == .failed
    }
}

/// Which context sources this build can reach, right now.
struct ContextDiagnostics: Decodable {
    let accessibility: ContextAccessibilityAccess
    let targetIdentity: ContextSourceStatus
    let focusedField: ContextSourceStatus
    let selectedText: ContextSourceStatus
    let browserUrl: ContextSourceStatus
    let clipboard: ContextSourceStatus
    let urlCaptureEnabled: Bool

    /// The rows the diagnostics card draws, in the order the old table used.
    var sources: [(label: String, status: ContextSourceStatus)] {
        [
            ("Frontmost app", targetIdentity),
            ("Focused field", focusedField),
            ("Selected text", selectedText),
            ("Browser URL", browserUrl),
            ("Recent clipboard", clipboard),
        ]
    }
}

// MARK: - Settings this slice reads

/// Only the fields Privacy shows. `get_app_settings` returns far more; a
/// decoder that ignores the rest is what keeps this slice off every other
/// screen's contract.
struct PrivacySettings: Decodable {
    var contextPolicyCeiling: ContextPolicy?
    var contextUrlCaptureEnabled: Bool?
    var externalQueryEnabled: Bool?
    var externalMutationsEnabled: Bool?
    var postProcessProviderId: String?
    var postProcessProviders: [PrivacyPostProcessProvider]?
    var modes: [PrivacyMode]?
    var cloudSttProviders: [PrivacyCloudSttProvider]?
}

struct PrivacyPostProcessProvider: Decodable {
    let id: String
    let label: String
    let baseUrl: String
}

struct PrivacyMode: Decodable {
    let llm: PrivacyModeLlm
}

struct PrivacyModeLlm: Decodable {
    let enabled: Bool
    /// The provider this mode overrides to, or absent to inherit the global one.
    let providerId: String?
}

/// One cloud transcription provider's persisted, content-free permissions.
struct PrivacyCloudSttProvider: Decodable {
    let provider: String
    let consentVersion: Int?
    let audioTransferConsent: Bool?
    let privacyConsent: Bool?
    let localFallbackConsent: Bool?
}

// MARK: - Egress routes

/// The backend checks this version before audio may leave the device; it
/// mirrors `settings::CLOUD_STT_CONSENT_VERSION`.
enum EgressConsent {
    static let cloudSttVersion = 1

    /// The two cloud transcription providers, with the stable account ids the
    /// secret store uses and the names the rows print.
    static let cloudSttProviders: [(provider: String, account: String, label: String)] = [
        ("deepgram_nova_3", "deepgram_nova3", "Deepgram Nova-3"),
        ("eleven_labs_scribe_v2", "elevenlabs_scribe_v2", "ElevenLabs Scribe v2"),
    ]

    /// A provider is a live route only with a key AND a current, complete grant.
    static func isCurrent(_ state: PrivacyCloudSttProvider?) -> Bool {
        guard let state else { return false }
        return state.consentVersion == cloudSttVersion
            && state.audioTransferConsent == true
            && state.privacyConsent == true
            && state.localFallbackConsent == true
    }
}

/// What the credential store says about one provider's key.
struct EgressSecretState: Decodable {
    let configured: Bool
}

/// One route off this Mac, as the chip reads it.
enum EgressRoute: Equatable {
    /// Still being read. A guess here would be the one guess this page cannot make.
    case checking
    /// Nothing configured: the work stays here.
    case thisMac
    /// The providers that can receive it.
    case providers([String])
    /// The state could not be read; the row offers a retry instead of a fact.
    case failed

    var fact: String {
        switch self {
        case .checking: "…"
        case .thisMac: "This Mac"
        case .providers(let names): names.joined(separator: ", ")
        case .failed: "Unavailable"
        }
    }
}

/// Availability of cloud sync, read from the backend rather than from settings.
struct PrivacyCloudSyncStatus: Decodable {
    let configured: Bool
    let endpoint: String?
    let error: String?
    let reason: String

    /// One state word for the chip. Why a route is unavailable is a sentence,
    /// and sentences belong in the notice below.
    var fact: String {
        if error != nil { return "Unavailable" }
        if !configured { return "Not configured" }
        return endpoint ?? "Configured"
    }

    /// The English of one `CloudSyncErrorKind`.
    var sentence: String? {
        switch error {
        case nil: nil
        case "portable_unavailable": "Cloud sync is unavailable in portable mode."
        case "secret_unavailable": "The system credential store is unavailable."
        case "setup_required": "Set up cloud sync before using this action."
        case "auth_required": "Cloud sign-in is required."
        case "quota": "Cloud storage quota has been reached."
        case "integrity_failure": "The cloud operation could not be verified."
        case "conflict": "A meeting has a sync conflict."
        case "unsupported_protocol": "This device does not support the version of cloud sync in use."
        case "transient": "Cloud sync is temporarily unavailable."
        case let other?: other
        }
    }
}

// MARK: - History storage

/// How dictation history is stored at rest. The key is fetched off the startup
/// path, so this begins life "unlocking" and settles on `history-storage-changed`.
struct StorageStatus: Decodable {
    let encrypted: Bool
    /// Milliseconds since 1970, when the database was encrypted.
    let migratedAt: Double?
    let reason: String?

    var isUnlocking: Bool { reason == "unlocking" }
    var isReadable: Bool { encrypted && reason == nil }

    var word: String {
        if isReadable { return "Encrypted at rest" }
        if isUnlocking { return "Unlocking" }
        return encrypted ? "Locked" : "Not encrypted"
    }

    var migratedDate: Date? {
        migratedAt.map { Date(timeIntervalSince1970: $0 / 1000) }
    }

    /// Only the reasons a reader can act on: "unlocking" is already the word above.
    var sentence: String? {
        guard !isReadable, !isUnlocking, let reason else { return nil }
        switch reason {
        case "key_unavailable":
            return "The system credential store returned no usable key, so history is stored unencrypted."
        case "encryption_unavailable":
            return "This version of Sona cannot open an encrypted database, so history is stored unencrypted."
        case "migration_failed":
            return "Encrypting the existing database failed. The unencrypted database is intact and still in use."
        case "key_rejected":
            return "The stored key does not open the encrypted database, so history cannot be read."
        default:
            return reason
        }
    }
}

// MARK: - Upstream import

enum UpstreamImportAppState: String, Decodable {
    case closed
    case running
    case unverifiable
}

enum UpstreamImportPhase: String, Decodable {
    case settings
    case history
    case recordings

    var label: String {
        switch self {
        case .settings: "Settings"
        case .history: "History"
        case .recordings: "Recordings"
        }
    }
}

/// What the previous app's data directory holds, and what of it already came over.
struct UpstreamImportStatus: Decodable {
    let available: Bool
    let appState: UpstreamImportAppState
    let settingsAvailable: Bool
    let historyEntries: Int
    let recordingFiles: Int
    let recordingBytes: Double
    let settingsImported: Bool
    let settingsBackupAvailable: Bool
    let settingsBackupSavedAtMs: Double?
    let historyImported: Int
    let recordingsImported: Int

    var backupDate: Date? {
        settingsBackupSavedAtMs.map { Date(timeIntervalSince1970: $0 / 1000) }
    }
}

/// What the person ticked. Sent as-is: the Rust field names are already flat.
struct UpstreamImportSelection: Encodable, Equatable {
    var settings = false
    var history = false
    var recordings = false
}

struct UpstreamImportResult: Decodable {
    let settingsImported: Bool
    let historyImported: Int
    let historyExisting: Int
    let recordingsCopied: Int
    let recordingsExisting: Int

    var sentence: String {
        let head = settingsImported ? "Settings imported." : "Settings were already imported."
        return "\(head) Imported \(historyImported) history entries; \(historyExisting) already existed. "
            + "Copied \(recordingsCopied) recordings; \(recordingsExisting) already existed."
    }
}

struct UpstreamImportProgress: Decodable {
    let phase: UpstreamImportPhase
    let completed: Int
    let total: Int

    var sentence: String { "\(phase.label): \(completed) of \(total)" }
}

/// The command's own error value, as `CoreError.remote(as:)` decodes it.
enum UpstreamImportFailure: String, Decodable {
    case sourceUnavailable = "source_unavailable"
    case upstreamRunning = "upstream_running"
    case appStateUnverifiable = "app_state_unverifiable"
    case invalidSelection = "invalid_selection"
    case settingsUnreadable = "settings_unreadable"
    case settingsBackupWriteFailed = "settings_backup_write_failed"
    case settingsBackupUnreadable = "settings_backup_unreadable"
    case settingsNotImported = "settings_not_imported"
    case secretStoreUnavailable = "secret_store_unavailable"
    case secretConflict = "secret_conflict"
    case historyUnreadable = "history_unreadable"
    case recordingCopyFailed = "recording_copy_failed"
    case receiptWriteFailed = "receipt_write_failed"
    case internalFailure = "internal"

    var sentence: String {
        switch self {
        case .sourceUnavailable: "The legacy app's data folder is unavailable."
        case .upstreamRunning: "Close the legacy app before importing."
        case .appStateUnverifiable: "Sona cannot verify that the legacy app is closed on this platform."
        case .invalidSelection: "Choose settings or history to import."
        case .settingsUnreadable: "The legacy settings file could not be read."
        case .settingsBackupWriteFailed: "Sona could not save a backup of the settings it is about to replace."
        case .settingsBackupUnreadable: "The settings backup could not be read, so the import cannot be undone."
        case .settingsNotImported: "No imported settings to revert."
        case .secretStoreUnavailable: "The system credential store is unavailable, so settings with provider keys were not imported."
        case .secretConflict: "A provider key already exists in the system credential store. Resolve it before importing settings."
        case .historyUnreadable: "The legacy history database could not be read."
        case .recordingCopyFailed: "A recording could not be copied and verified."
        case .receiptWriteFailed: "Sona couldn't save the import details."
        case .internalFailure: "The import did not finish."
        }
    }
}

// MARK: - Identity adoption

/// Why the one-time adoption of the previous identity ended the way it did.
enum IdentityAdoptionMode: String, Decodable {
    case portable
    case nothingToAdopt = "nothing_to_adopt"
    case skippedNonvirgin = "skipped_nonvirgin"
    case freshStart = "fresh_start"
    case completed

    var word: String {
        switch self {
        case .portable: "Portable install"
        case .nothingToAdopt: "Nothing to adopt"
        case .skippedNonvirgin: "Skipped"
        case .freshStart: "Fresh start"
        case .completed: "Adopted"
        }
    }

    var sentence: String {
        switch self {
        case .portable: "This copy runs from its own folder, so no identity was adopted."
        case .nothingToAdopt: "No previous installation was found to adopt."
        case .skippedNonvirgin: "This installation already had data of its own, so nothing was moved."
        case .freshStart: "Sona started fresh; the previous data folder was left where it is."
        case .completed: "The previous installation's data folder was moved into this one."
        }
    }
}

enum IdentityAdoptionAction: String, Decodable {
    case renamed
    case copied
    case skipped
    case failed

    var word: String {
        switch self {
        case .renamed: "Moved"
        case .copied: "Copied"
        case .skipped: "Skipped"
        case .failed: "Failed"
        }
    }
}

struct IdentityAdoptionEntry: Decodable, Identifiable {
    let path: String
    let action: IdentityAdoptionAction
    let bytes: Double
    let sha256: String?

    var id: String { path }
    var size: String { Model.bytes(UInt64(max(bytes, 0))) }
}

enum IdentityCredentialStatus: String, Decodable {
    case moved
    case notFound = "not_found"
    case needsReentry = "needs_reentry"

    var word: String {
        switch self {
        case .moved: "Moved"
        case .notFound: "Not found"
        case .needsReentry: "Re-enter"
        }
    }
}

struct IdentityCredentialReceipt: Decodable, Identifiable {
    let account: String
    let status: IdentityCredentialStatus

    var id: String { account }
}

/// What the adoption did, written once and read from disk afterwards.
struct IdentityAdoptionReceipt: Decodable {
    let mode: IdentityAdoptionMode
    let sourceIdentity: String?
    let entries: [IdentityAdoptionEntry]
    let credentials: [IdentityCredentialReceipt]
    let completedAtMs: Double
    let appVersion: String

    var completed: Date { Date(timeIntervalSince1970: completedAtMs / 1000) }
    /// Only a completed adoption can be put back.
    var canRevert: Bool { mode == .completed }
}

enum IdentityAdoptionFailure: String, Decodable {
    case unavailable
    case legacyRunning = "legacy_running"
    case copyFailed = "copy_failed"
    case destinationConflict = "destination_conflict"
    case invalidData = "invalid_data"
    case secretMigrationFailed = "secret_migration_failed"
    case rollbackUnavailable = "rollback_unavailable"
    case rollbackFailed = "rollback_failed"

    var sentence: String {
        switch self {
        case .unavailable: "Sona could not reach the data folders this would move."
        case .legacyRunning: "Close the legacy app before putting its data back."
        case .copyFailed: "A file could not be copied."
        case .destinationConflict: "A file already exists where this would write."
        case .invalidData: "The adoption record could not be read."
        case .secretMigrationFailed: "A provider key could not be moved back to the legacy credential store."
        case .rollbackUnavailable: "There is no completed adoption to undo."
        case .rollbackFailed: "Putting the data back did not finish. Nothing else was changed."
        }
    }
}

// MARK: - Audio file import

enum AudioImportStatus: String, Decodable {
    case queued
    case decoding
    case transcribing
    case done
    case cancelled
    case failed

    /// A job is cancellable only while it is in one of these states.
    var isRunning: Bool {
        self == .queued || self == .decoding || self == .transcribing
    }

    var word: String {
        switch self {
        case .queued: "Queued"
        case .decoding: "Reading the audio"
        case .transcribing: "Transcribing"
        case .done: "Saved to history"
        case .cancelled: "Cancelled"
        case .failed: "Failed"
        }
    }
}

enum AudioImportFailureCode: String, Decodable {
    case invalidFile = "invalid_file"
    case unsupportedFormat = "unsupported_format"
    case noAudio = "no_audio"
    case decode
    case durationLimit = "duration_limit"
    case transcription
    case history
    case meetingImport = "meeting_import"

    var sentence: String {
        switch self {
        case .invalidFile: "The selected file cannot be read."
        case .unsupportedFormat: "This audio format is not supported."
        case .noAudio: "This media file has no audio track."
        case .decode: "The audio file could not be decoded."
        case .durationLimit: "Imported audio is limited to 30 minutes."
        case .transcription: "The audio could not be transcribed."
        case .history: "The transcript could not be saved to history."
        case .meetingImport: "This recording could not be saved as a meeting."
        }
    }
}

/// Where one import landed. Tagged by `kind`, so it is decoded by hand.
enum AudioImportResult: Decodable {
    case done(historyId: Int64)
    /// Long enough to be a recording of something rather than a dictation, so
    /// it became a meeting and no history row exists.
    case meeting(sessionId: String)
    case cancelled
    case failed(code: AudioImportFailureCode, message: String)

    private enum Key: String, CodingKey {
        case kind, historyId, sessionId, code, message
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let kind = try container.decode(String.self, forKey: .kind)
        switch kind {
        case "done":
            self = .done(historyId: try container.decode(Int64.self, forKey: .historyId))
        case "meeting":
            self = .meeting(sessionId: try container.decode(String.self, forKey: .sessionId))
        case "cancelled":
            self = .cancelled
        case "failed":
            self = .failed(
                code: try container.decode(AudioImportFailureCode.self, forKey: .code),
                message: try container.decode(String.self, forKey: .message))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: container, debugDescription: "unknown audio import result \(kind)")
        }
    }

    var failure: AudioImportFailureCode? {
        if case .failed(let code, _) = self { return code }
        return nil
    }

    var isCancelled: Bool {
        if case .cancelled = self { return true }
        return false
    }

    var isMeeting: Bool {
        if case .meeting = self { return true }
        return false
    }

    /// The `sona://` address of what this import became, when it has one.
    var link: String? {
        switch self {
        case .done(let historyId): "sona://dictation/\(historyId)"
        case .meeting(let sessionId): "sona://meeting/\(sessionId)"
        case .cancelled, .failed: nil
        }
    }
}

/// The complete public state of one import. Source paths stay in the core;
/// only the original file name crosses the socket.
struct AudioImportJob: Decodable, Identifiable {
    let id: Int64
    let fileName: String
    let status: AudioImportStatus
    let decodedSamples: Double
    let cancelRequested: Bool
    let result: AudioImportResult?

    var canCancel: Bool { !cancelRequested && status.isRunning }

    /// `done` is two sentences, not one: a dictation landed in History, and a
    /// recording the OS opened landed in Library. The result says which.
    var word: String {
        if cancelRequested { return "Cancelling" }
        if result?.isMeeting == true { return "Saved as a meeting" }
        return status.word
    }

    var sentence: String? { result?.failure?.sentence }
}

struct AudioImportUpdate: Decodable {
    let job: AudioImportJob
}

enum AudioImportDestination: String, Decodable {
    case meeting
    case dictation
}

/// Where one file the operating system handed to Sona ended up. Emitted only
/// for that route, because it is the only one where nobody was asked.
struct AudioImportRouted: Decodable {
    let fileName: String
    let destination: AudioImportDestination
    let link: String

    var sentence: String {
        switch destination {
        case .meeting: "\(fileName) saved as a meeting"
        case .dictation: "\(fileName) saved to history"
        }
    }
}

// MARK: - The import dialog's own list

/// What a chosen file is doing, in the order it can happen.
enum ImportRowState: Equatable {
    case ready
    case queued
    case running
    case done
    case cancelled
    case failed

    var word: String {
        switch self {
        case .ready: "Ready to import"
        case .queued: "Queued"
        case .running: "Working on it"
        case .done: "Imported"
        case .cancelled: "Cancelled"
        case .failed: "Failed"
        }
    }
}

/// One file the dialog is holding. One path, one row: the path is its identity.
struct ImportRow: Identifiable, Equatable {
    let path: String
    let name: String
    var state: ImportRowState = .ready
    /// The core job this row follows, once a command returned one.
    var jobId: Int64?
    /// The sentence explaining a failure, or nil.
    var failure: String?

    var id: String { path }
}

/// The file types the core's media importer accepts.
enum ImportMedia {
    static let extensions = ["wav", "mp3", "m4a", "aac", "flac", "ogg", "mov", "mp4", "m4v"]

    /// The last path segment.
    static func name(of path: String) -> String {
        (path as NSString).lastPathComponent
    }

    /// Lower-cased extension without the dot. A leading dot is a hidden file,
    /// not an extension.
    static func fileExtension(of path: String) -> String {
        let name = name(of: path)
        guard let dot = name.lastIndex(of: "."), dot != name.startIndex else { return "" }
        return String(name[name.index(after: dot)...]).lowercased()
    }

    /// Append the paths this list does not hold and the importer accepts.
    static func add(_ paths: [String], to rows: [ImportRow]) -> [ImportRow] {
        var known = Set(rows.map(\.path))
        var appended = rows
        for path in paths {
            guard !known.contains(path), extensions.contains(fileExtension(of: path)) else { continue }
            known.insert(path)
            appended.append(ImportRow(path: path, name: name(of: path)))
        }
        return appended
    }

    /// What one core job says about the row following it. A result outranks a
    /// status: a failed job still reports the stage it failed in.
    static func rowState(for job: AudioImportJob) -> (state: ImportRowState, failure: String?) {
        if let code = job.result?.failure {
            return (.failed, code.sentence)
        }
        if job.result?.isCancelled == true || job.status == .cancelled {
            return (.cancelled, nil)
        }
        switch job.status {
        case .done: return (.done, nil)
        case .failed: return (.failed, nil)
        case .queued: return (.queued, nil)
        case .decoding, .transcribing, .cancelled: return (.running, nil)
        }
    }
}

// MARK: - Documents

struct DocumentSummary: Decodable {
    let id: String
    let title: String
    let sourceName: String
    let mediaType: String
    let createdAtUtcMs: Double

    var created: Date { Date(timeIntervalSince1970: createdAtUtcMs / 1000) }
}

/// One imported document: what it is, and what it says.
struct DocumentEntry: Decodable, Identifiable {
    let summary: DocumentSummary
    let content: String

    var id: String { summary.id }
}

struct DocumentListResult: Decodable {
    let schemaVersion: Int
    let revision: Int
    let entries: [DocumentEntry]
}

struct DocumentMutationResult: Decodable {
    let schemaVersion: Int
    let revision: Int
    let document: DocumentEntry?
    let removed: Bool
}

/// The file types `doc_ingest` reads.
enum DocumentMedia {
    static let extensions = ["txt", "md", "markdown"]
}

// MARK: - The query plane

enum QueryScope: String, Codable, CaseIterable, Hashable {
    case all
    case meetings
    case dictations
    case people
    case loops

    var label: String {
        switch self {
        case .all: "Everything"
        case .meetings: "Meetings"
        case .dictations: "Dictations"
        case .people: "People"
        case .loops: "Loops"
        }
    }
}

enum QueryRowKind: String, Decodable {
    case meeting
    case dictation
    case person
    case series
    case loop
    case receipt

    var word: String {
        switch self {
        case .meeting: "Meeting"
        case .dictation: "Dictation"
        case .person: "Person"
        case .series: "Series"
        case .loop: "Loop"
        case .receipt: "Receipt"
        }
    }
}

/// One answer. `link` is always a `sona://` URL this app parses.
struct QueryRow: Decodable, Identifiable {
    let kind: QueryRowKind
    let id: String
    let title: String
    /// Why this row is in front of you: the text that matched.
    let snippet: String
    let whenUtcMs: Double
    let link: String

    var when: Date { Date(timeIntervalSince1970: whenUtcMs / 1000) }
}

/// Where the next page starts. Decoded camelCase, sent back snake_case: the
/// core names these fields the way Rust does.
struct QueryCursor: Codable, Equatable {
    let whenUtcMs: Double
    let kind: QueryRowKind
    let id: String
    let dictationId: Int64?

    private enum ReadKey: String, CodingKey {
        case whenUtcMs, kind, id, dictationId
    }

    private enum WriteKey: String, CodingKey {
        case whenUtcMs = "when_utc_ms"
        case kind
        case id
        case dictationId = "dictation_id"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: ReadKey.self)
        whenUtcMs = try container.decode(Double.self, forKey: .whenUtcMs)
        kind = try container.decode(QueryRowKind.self, forKey: .kind)
        id = try container.decode(String.self, forKey: .id)
        dictationId = try container.decodeIfPresent(Int64.self, forKey: .dictationId)
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: WriteKey.self)
        try container.encode(whenUtcMs, forKey: .whenUtcMs)
        try container.encode(kind.rawValue, forKey: .kind)
        try container.encode(id, forKey: .id)
        try container.encode(dictationId, forKey: .dictationId)
    }
}

/// Why a page reads the way it does, when something other than the corpus
/// decided it.
enum QueryPageReason: String, Decodable {
    case noRows = "no_rows"
    case noSearchableTokens = "no_searchable_tokens"
    case semanticUnavailable = "semantic_unavailable"
    case filteredOut = "filtered_out"
    case awaitingContinuity = "awaiting_continuity"
    case peopleIndexEmpty = "people_index_empty"

    var sentence: String {
        switch self {
        case .noRows: "Nothing in the corpus matches that."
        case .noSearchableTokens: "That question named no word to look for."
        case .semanticUnavailable: "Only literal word matching ran: the meaning model is not on this Mac yet."
        case .filteredOut: "Rows of that kind exist, and every one was excluded."
        case .awaitingContinuity: "Some rows are not reportable yet: a meeting's continuity pass has not succeeded."
        case .peopleIndexEmpty: "Nobody has been named in this corpus yet."
        }
    }
}

struct QuerySearchPage: Decodable {
    let schemaVersion: Int
    let entries: [QueryRow]
    let nextCursor: QueryCursor?
    let reason: QueryPageReason?
}

enum QueryEventResult: String, Decodable {
    case committed
    case rejected
    case ok
    case failed
    case skipped
}

enum QueryEventSource: String, Decodable {
    case operationReceipt = "operation_receipt"
    case workflowRun = "workflow_run"
}

/// One line of "what happened since I last looked".
struct QueryEvent: Decodable, Identifiable {
    let id: String
    let source: QueryEventSource
    let action: String
    let result: QueryEventResult
    let detail: String
    let outcomeSummary: String?
    let whenUtcMs: Double
    /// The `sona://` address of the noun this touched, when it touched one.
    let link: String?

    var when: Date { Date(timeIntervalSince1970: whenUtcMs / 1000) }
}

struct QueryEventsPage: Decodable {
    let schemaVersion: Int
    let entries: [QueryEvent]
    let nextCursor: String?
}

/// One question's evidence bundle, ready to ride as a turn's context pack.
struct QueryPack: Decodable {
    let schemaVersion: Int
    let pack: String
    /// Exactly the rows quoted in `pack`, in the order they are quoted.
    let sources: [QueryRow]
}

/// A `sona://` noun the shell has no navigation of its own for.
enum QueryLinkTarget: Decodable, Equatable {
    case person(id: String)
    case organization(slug: String)
    case dictation(historyId: Int64)
    /// The search surface, with the question the link carried.
    case search(query: String)

    private enum Key: String, CodingKey {
        case kind, personId, slug, historyId, query
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let kind = try container.decode(String.self, forKey: .kind)
        switch kind {
        case "person":
            self = .person(id: try container.decode(String.self, forKey: .personId))
        case "organization":
            self = .organization(slug: try container.decode(String.self, forKey: .slug))
        case "dictation":
            self = .dictation(historyId: try container.decode(Int64.self, forKey: .historyId))
        case "search":
            self = .search(query: try container.decode(String.self, forKey: .query))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: container, debugDescription: "unknown query link target \(kind)")
        }
    }
}

struct QueryLinkRequest: Decodable {
    let eventSchemaVersion: Int
    let target: QueryLinkTarget
}

enum QueryFailure: String, Decodable {
    case unavailable
    case invalidRequest = "invalid_request"
    case unknownCursor = "unknown_cursor"
    case failed

    var sentence: String {
        switch self {
        case .unavailable: "The corpus cannot be opened right now."
        case .invalidRequest: "That is not a question this plane will answer."
        case .unknownCursor: "That page is gone. Search again from the top."
        case .failed: "The search did not finish."
        }
    }
}

import Foundation

/// The shapes the Library reads out of the core: the log itself, the run
/// receipts under a row, the activity trend, and the two retention settings.
/// Field names mirror the Rust structs with snake_case turned into camelCase
/// by `Core.decoder`; enum values are the wire spellings, unconverted.

// MARK: - The log

/// Why a search returned a row. `text` means the row's own words matched;
/// `semantic` means they did not and its meaning did. Absent outside a search.
enum HistoryMatchKind: String, Decodable {
    case text
    case semantic
}

/// One dictation as the Library reads it: the `Transcription` the rest of the
/// app already knows, plus the fields only this page needs. The shared
/// `HistoryEntry` mirror in CoreTypes.swift leaves those three out, so the row
/// decodes the entry through it and then reads the remainder from the same
/// object.
struct HistoryRow: Identifiable, Decodable {
    /// The entry as every other screen states it.
    var item: Transcription
    /// The words a mode wrote, trimmed. Empty when the run wrote none.
    let processedText: String
    /// The words as heard, trimmed.
    let spokenText: String
    /// Whether the run asked for post-processing at all. An empty processed
    /// text means something different when it did.
    let postProcessRequested: Bool
    /// The entry this one was reprocessed or retried from.
    let parentId: Int64?
    let matchKind: HistoryMatchKind?

    var id: Int64 { item.id }

    private enum Key: String, CodingKey {
        case postProcessRequested, parentId, matchKind
    }

    init(from decoder: Decoder) throws {
        let entry = try HistoryEntry(from: decoder)
        let values = try decoder.container(keyedBy: Key.self)
        item = Transcription(entry)
        processedText = (entry.postProcessedText ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        spokenText = entry.transcriptionText.trimmingCharacters(in: .whitespacesAndNewlines)
        postProcessRequested = try values.decodeIfPresent(Bool.self, forKey: .postProcessRequested) ?? false
        parentId = try values.decodeIfPresent(Int64.self, forKey: .parentId)
        matchKind = try values.decodeIfPresent(HistoryMatchKind.self, forKey: .matchKind)
    }

    /// The words this row reads in, under the view the page is set to. A
    /// processed view with nothing processed falls back to what was heard.
    func text(_ view: LibraryTextView) -> String {
        view == .processed && !processedText.isEmpty ? processedText : spokenText
    }

    /// True when the page is showing processed text, the run asked for it, and
    /// none came back: the row says so rather than passing the raw words off
    /// as the mode's work.
    func processedMissing(_ view: LibraryTextView) -> Bool {
        view == .processed && processedText.isEmpty && postProcessRequested
    }

    /// A recording the run left no words on at all.
    var silent: Bool { processedText.isEmpty && spokenText.isEmpty }
}

/// One page of the log.
struct HistoryPage: Decodable {
    let entries: [HistoryRow]
    let hasMore: Bool
}

/// The same frame `HistoryUpdate` reads, decoded a second time for the fields
/// the shared mirror leaves out. Only `added` and `updated` carry an entry.
struct HistoryRowUpdate: Decodable {
    let entry: HistoryRow?
}

/// What the feed is doing. `paging` and `pagingError` belong to the next page
/// only: the rows already read stay on screen through both.
enum LibraryPhase {
    case loading
    case ready
    case paging
    case pagingError
    case error
}

/// Which field of a row the log is read in.
enum LibraryTextView {
    case processed
    case raw
}

/// One local day of the log, with its empty recordings kept apart: a day's
/// worth of runs that produced no words is one line to open, not one full row
/// each.
struct HistoryDay: Identifiable {
    let id: Date
    let heading: String
    let spoken: [HistoryRow]
    let silent: [HistoryRow]
}

// MARK: - Run receipts

/// One immutable record of a run against a recording: content-free provenance.
struct HistoryRunReceipt: Identifiable, Decodable {
    let id: Int64
    let runId: Int64
    let completedAtMs: Int64
    let mode: ReceiptMode
    let context: ReceiptContext
    let durationMs: Int64?
    let wordCount: Int?
    let sourceKind: ReceiptSourceKind?
    let hasAudio: Bool
    let captureStatus: ReceiptCaptureStatus?
    let deliveryAttempts: [ReceiptDeliveryAttempt]
}

/// The half of the receipt that names the run's settings. Only the fields the
/// inspector prints are decoded; the rest of the wire struct is ignored.
struct ReceiptMode: Decodable {
    let settingsRevision: Int
    let modeId: String
    let contextPolicy: ReceiptContextPolicy
    let promptPreset: ReceiptPromptPreset
    let providerId: String?
    let modelId: String?
    let engineRequested: ReceiptEngine?
    let engineUsed: ReceiptEngine?
    let cloudStatus: ReceiptCloudStatus?
    /// Peak and average amplitude of the capture, normalized to full scale.
    let inputPeak: Double?
    let inputRms: Double?
    /// Audio seconds per decode second for the local batch decode.
    let realtimeFactor: Double?
}

struct ReceiptContext: Decodable {
    let sources: ReceiptContextSources
}

/// Which context sources took part in a run, and why the others did not.
struct ReceiptContextSources: Decodable {
    let target: ReceiptSourceStatus
    let focusedField: ReceiptSourceStatus
    let selectedText: ReceiptSourceStatus
    let browserUrl: ReceiptSourceStatus
    let clipboard: ReceiptSourceStatus

    /// The five, in the order the inspector prints them.
    var listed: [(String, String)] {
        [
            ("App", target.label),
            ("Focused field", focusedField.label),
            ("Selected text", selectedText.label),
            ("Browser URL", browserUrl.label),
            ("Recent clipboard", clipboard.label),
        ]
    }
}

enum ReceiptSourceStatus: String, Decodable {
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

    var label: String {
        switch self {
        case .notRequested: "Not requested"
        case .captured: "Captured"
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
}

enum ReceiptCaptureStatus: String, Decodable {
    case complete
    case truncated
    case noSpeechDetected = "no_speech_detected"

    var label: String {
        switch self {
        case .complete: "Complete"
        case .truncated: "Truncated"
        case .noSpeechDetected: "No speech detected"
        }
    }
}

enum ReceiptSourceKind: String, Decodable {
    case microphone
    case file

    var label: String {
        switch self {
        case .microphone: "Microphone"
        case .file: "File import"
        }
    }
}

enum ReceiptContextPolicy: String, Decodable {
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
}

enum ReceiptPromptPreset: String, Decodable {
    case minimalistCleanup = "minimalist_cleanup"
    case applicationContext = "application_context"
    case email
    case meeting
    case notes
    case generic

    var label: String {
        switch self {
        case .minimalistCleanup: "Minimal cleanup"
        case .applicationContext: "Application context"
        case .email: "Email"
        case .meeting: "Meeting"
        case .notes: "Notes"
        case .generic: "General"
        }
    }
}

enum ReceiptEngine: String, Decodable {
    case local
    case deepgram = "deepgram_nova_3"
    case elevenLabs = "eleven_labs_scribe_v2"

    var label: String {
        switch self {
        case .local: "Local"
        case .deepgram: "Deepgram Nova 3"
        case .elevenLabs: "ElevenLabs Scribe v2"
        }
    }
}

enum ReceiptCloudStatus: String, Decodable {
    case notRequested = "not_requested"
    case final
    case fallback
    case heldCloudUnavailable = "held_cloud_unavailable"
}

/// One delivery observation. A second observation is another attempt; no
/// outcome is ever overwritten.
struct ReceiptDeliveryAttempt: Identifiable, Decodable {
    let id: Int64
    let delivery: ReceiptDelivery
}

struct ReceiptDelivery: Decodable {
    let method: ReceiptDeliveryMethod
    let outcome: ReceiptDeliveryOutcome
}

enum ReceiptDeliveryMethod: String, Decodable {
    case none
    case accessibilityInsertion = "accessibility_insertion"
    case clipboardPaste = "clipboard_paste"
    case directTyping = "direct_typing"
    case externalScript = "external_script"

    var label: String {
        switch self {
        case .none: "No delivery"
        case .accessibilityInsertion: "Accessibility insertion"
        case .clipboardPaste: "Clipboard paste"
        case .directTyping: "Direct typing"
        case .externalScript: "External script"
        }
    }
}

enum ReceiptDeliveryOutcome: String, Decodable {
    case delivered
    case definitelyNotDispatched = "definitely_not_dispatched"
    case dispatchedButUnconfirmed = "dispatched_but_unconfirmed"
    case dispatchedUnderSecureInput = "dispatched_under_secure_input"

    var label: String {
        switch self {
        case .delivered: "Delivered"
        case .definitelyNotDispatched: "Not sent"
        case .dispatchedButUnconfirmed: "Sent, not confirmed"
        case .dispatchedUnderSecureInput: "Sent while Secure Input was on"
        }
    }
}

/// What the page knows about one row's receipts. Three ways to have none, and
/// they are not the same thing, so the inspector says which.
enum ReceiptLoad {
    case loading
    case failed
    case ready([HistoryRunReceipt])
}

// MARK: - The activity trend

/// The only calendar windows the trend command offers.
enum TrendRange: String, CaseIterable, Codable {
    case week = "days_7"
    case month = "days_30"
    case halfYear = "days_180"

    var label: String {
        switch self {
        case .week: "7 days"
        case .month: "30 days"
        case .halfYear: "180 days"
        }
    }
}

/// One local-calendar day. Every day in the range is present, including the
/// days with nothing on them.
struct TrendPoint: Identifiable, Decodable {
    let localDate: String
    let recordings: Int
    let durationMs: Int
    let words: Int

    var id: String { localDate }
}

struct TrendTotals: Decodable {
    let recordings: Int
    let durationMs: Int
    let words: Int
}

/// A bounded projection over the retained log. `activeDays` and
/// `currentStreakDays` are counted inside the range.
struct TrendProjection: Decodable {
    let range: TrendRange
    let rangeTotal: TrendTotals
    let activeDays: Int
    let currentStreakDays: Int
    let points: [TrendPoint]
}

// MARK: - Audio, storage, retention

/// One bounded fragment of a stored recording. The core identifies the media
/// through the history row and never hands out a path.
struct HistoryAudioChunk: Decodable {
    let bytes: [UInt8]
    let eof: Bool
}

/// Whether the log is encrypted at rest, and why not when it is not.
struct HistoryStorageState: Decodable {
    let encrypted: Bool
    let migratedAt: Int64?
    let reason: String?

    /// The one line the storage row states.
    var line: String {
        if encrypted {
            return "Encrypted at rest"
        }
        switch reason {
        case "encryption_unavailable":
            return "Not encrypted: this build cannot open an encrypted database."
        case "key_rejected":
            return "The stored key does not open the encrypted database, so history cannot be read."
        case "key_unavailable":
            return "Not encrypted: the system credential store returned no usable key."
        case "migration_failed":
            return "Not encrypted: encrypting the existing database failed, and the plain one is still in use."
        case let other?:
            return "Not encrypted: \(other)"
        case nil:
            return "Not encrypted"
        }
    }
}

/// How long a recording survives after its words were written.
enum LibraryRetention: String, CaseIterable, Decodable {
    case never
    case preserveLimit = "preserve_limit"
    case days3 = "days_3"
    case weeks2 = "weeks_2"
    case months3 = "months_3"

    var label: String {
        switch self {
        case .never: "Do not retain recordings"
        case .preserveLimit: "Keep recordings with history"
        case .days3: "3 days"
        case .weeks2: "2 weeks"
        case .months3: "3 months"
        }
    }
}

/// The two fields of the core's settings this page writes.
struct LibrarySettings: Decodable {
    let historyLimit: Int?
    let recordingRetentionPeriod: LibraryRetention?
}

/// One mode the Process again dialog can run a recording through.
struct ReprocessMode: Identifiable, Decodable {
    let id: String
    let name: String
}

struct ReprocessModeList: Decodable {
    let modes: [ReprocessMode]
    let activeModeId: String
}

// MARK: - Params

struct HistoryPageParams: Encodable {
    let cursor: Int64?
    let limit: Int
}

struct HistorySearchParams: Encodable {
    let query: String
    let cursor: Int64?
    let limit: Int
}

struct HistoryIdParam: Encodable {
    let id: Int64
}

struct HistoryEntryParam: Encodable {
    let historyId: Int64
}

struct HistoryAudioParams: Encodable {
    let historyId: Int64
    let offset: Int
}

struct ReprocessParams: Encodable {
    let id: Int64
    let modeId: String
}

struct TrendRequest: Encodable {
    let range: TrendRange
}

struct TrendParams: Encodable {
    let request: TrendRequest
}

struct LibraryLimitParam: Encodable {
    let limit: Int
}

struct LibraryRetentionParam: Encodable {
    let period: String
}

// MARK: - What the shell could not do

enum LibraryFault: LocalizedError {
    /// A chunk read stopped short of the end of the recording.
    case audioTruncated
    /// The row has no stored recording: retention swept it, or none was kept.
    case audioMissing
    /// The bytes are there but no decoder on this Mac reads them.
    case audioUnplayable(String)

    var errorDescription: String? {
        switch self {
        case .audioTruncated: "The recording ended before Sona had read all of it."
        case .audioMissing: "No recording is stored for this dictation."
        case let .audioUnplayable(reason): "That recording could not be played: \(reason)"
        }
    }
}

// MARK: - Measurements

/// The page's one duration renderer: "0s", "15s", "3m 12s", "1h 4m". Explicit
/// units, no zero padding, so a receipt's length and a player's position are
/// the same string produced the same way.
enum LibraryClock {
    static func short(_ totalSeconds: Double) -> String {
        let seconds = max(0, Int(totalSeconds.rounded()))
        if seconds < 60 {
            return "\(seconds)s"
        }
        let minutes = seconds / 60
        if minutes < 60 {
            let rest = seconds % 60
            return rest == 0 ? "\(minutes)m" : "\(minutes)m \(rest)s"
        }
        let hours = minutes / 60
        let restMinutes = minutes % 60
        return restMinutes == 0 ? "\(hours)h" : "\(hours)h \(restMinutes)m"
    }

    static func milliseconds(_ value: Int64) -> String {
        short(Double(value) / 1000)
    }
}

/// The counted nouns this page states, singular when there is one of them.
enum LibraryCount {
    static func recordings(_ count: Int) -> String {
        "\(count.formatted()) \(count == 1 ? "recording" : "recordings")"
    }

    static func words(_ count: Int) -> String {
        "\(count.formatted()) \(count == 1 ? "word" : "words")"
    }

    static func days(_ count: Int) -> String {
        "\(count) \(count == 1 ? "day" : "days")"
    }
}

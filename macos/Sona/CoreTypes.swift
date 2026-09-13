import Foundation

/// The shapes the core sends. Field names mirror the Rust structs in
/// `src-tauri/src` with snake_case turned into camelCase by the decoder.

/// Event names as the core emits them. The first four are tauri-specta names,
/// which carry the type suffix; the rest are emitted by hand in the managers.
enum CoreEvent {
    static let activity = "dictation-activity"
    static let historyUpdate = "history-update-payload"
    static let historyStorage = "history-storage-changed"
    static let streamText = "stream-text-event"
    static let streamPhase = "stream-phase-event"
    static let handyKeys = "handy-keys-event"
    static let modelStateChanged = "model-state-changed"
    static let modelsUpdated = "models-updated"
    static let downloadProgress = "model-download-progress"
    static let downloadComplete = "model-download-complete"
    static let downloadFailed = "model-download-failed"
    static let downloadCancelled = "model-download-cancelled"
    static let modelDeleted = "model-deleted"
}

struct HistoryEntry: Decodable {
    let id: Int64
    /// Seconds since 1970.
    let timestamp: Int64
    let saved: Bool
    let title: String
    let transcriptionText: String
    let postProcessedText: String?
}

struct HistoryStats: Decodable {
    let entries: UInt64
    let totalDurationMs: UInt64
    let totalWords: UInt64
}

struct ModelInfo: Decodable {
    let id: String
    let name: String
    let description: String
    let sizeMb: UInt64
    let isDownloaded: Bool
    let isDownloading: Bool
    let partialSize: UInt64
}

struct BindingChange: Decodable {
    let success: Bool
    let error: String?
}

struct HandyKeysEvent: Decodable {
    let modifiers: [String]
    let key: String?
    let isKeyDown: Bool
    let hotkeyString: String
}

struct DictationActivity: Decodable {
    /// "idle", "recording", or "transcribing".
    let state: String
}

struct StreamText: Decodable {
    let committed: String
    let tentative: String
}

struct StreamPhase: Decodable {
    /// "listening" or "working".
    let phase: String
    /// "transcribing" or "polishing", only while working.
    let kind: String?
}

/// `{"action": "added", "entry": {...}}` and the three siblings.
enum HistoryUpdate: Decodable {
    case added(HistoryEntry)
    case updated(HistoryEntry)
    case deleted(Int64)
    case toggled(Int64)

    private enum Key: String, CodingKey { case action, entry, id }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let action = try container.decode(String.self, forKey: .action)
        switch action {
        case "added": self = .added(try container.decode(HistoryEntry.self, forKey: .entry))
        case "updated": self = .updated(try container.decode(HistoryEntry.self, forKey: .entry))
        case "deleted": self = .deleted(try container.decode(Int64.self, forKey: .id))
        case "toggled": self = .toggled(try container.decode(Int64.self, forKey: .id))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .action, in: container, debugDescription: "unknown history action \(action)")
        }
    }
}

struct DownloadProgress: Decodable {
    let modelId: String
    let downloaded: UInt64
    let total: UInt64
}

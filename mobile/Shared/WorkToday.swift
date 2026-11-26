import Foundation

/// What the paired Mac says needs its user today: work due, tasks waiting in Triage,
/// unread inbox rows and agents waiting on an answer. Field names are the wire names
/// `src-tauri/src/cloud_sync/work_today.rs` writes.
struct WorkToday: Codable, Equatable {
    static let sourceFormat = "sona-work-today-v1"
    static let kind = "work_today"

    struct Item: Codable, Equatable, Identifiable {
        var id: String
        var number: Int64
        var title: String
        var due_at_ms: Int64?
        var project: String?
        var revision: Int64

        var dueAt: Date? { due_at_ms.map { Date(timeIntervalSince1970: Double($0) / 1000) } }
    }

    struct Notice: Codable, Equatable, Identifiable {
        var id: String
        var notice_ids: [String]
        var task_id: String?
        var task_number: Int64?
        var title: String
        var line: String
        var needs_you: Bool
        var at_ms: Int64

        var at: Date { Date(timeIntervalSince1970: Double(at_ms) / 1000) }
    }

    struct Question: Codable, Equatable, Identifiable {
        struct Option: Codable, Equatable, Hashable {
            var label: String
            var value: String
        }

        var session_id: String
        var task_id: String
        var task_number: Int64
        var task_title: String
        var agent: String
        var question: String
        var options: [Option]
        var asked_at_ms: Int64

        var id: String { session_id }
    }

    struct Handled: Codable, Equatable {
        enum Outcome: String, Codable { case applied, refused }
        var action_id: String
        var outcome: Outcome
        var at_utc_ms: Int64
    }

    var format_version: Int
    var kind: String
    var source_device_id: String
    var produced_at_utc_ms: Int64
    var due: [Item]
    var triage: [Item]
    var inbox: [Notice]
    var questions: [Question]
    var handled: [Handled]

    var producedAt: Date { Date(timeIntervalSince1970: Double(produced_at_utc_ms) / 1000) }
}

/// A decision about the Mac's work, sealed into the vault for the Mac that listed it.
/// The Mac refuses unknown fields and writes nil as null, so every key is written.
struct WorkActionManifest: Codable, Equatable {
    enum Action: String, Codable { case done, accept, decline, read, archive, answer }

    var acted_at_utc_ms: Int64
    var action: Action
    var asked_at_ms: Int64?
    var body: String?
    var device_id: String
    var expected_revision: Int64?
    var format_version: Int
    var kind: String
    var notice_ids: [String]
    var session_id: String?
    var target_device_id: String
    var task_id: String?
    var value: String?

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(acted_at_utc_ms, forKey: .acted_at_utc_ms)
        try container.encode(action, forKey: .action)
        try container.encode(asked_at_ms, forKey: .asked_at_ms)
        try container.encode(body, forKey: .body)
        try container.encode(device_id, forKey: .device_id)
        try container.encode(expected_revision, forKey: .expected_revision)
        try container.encode(format_version, forKey: .format_version)
        try container.encode(kind, forKey: .kind)
        try container.encode(notice_ids, forKey: .notice_ids)
        try container.encode(session_id, forKey: .session_id)
        try container.encode(target_device_id, forKey: .target_device_id)
        try container.encode(task_id, forKey: .task_id)
        try container.encode(value, forKey: .value)
    }
}

enum WorkActionObject {
    static let kind = "work_action"
    /// The `source_format` bound into an action's HKDF info and AES-GCM AAD.
    static let sourceFormat = "sona-work-action-v1"
    /// What the Mac takes in an answer's text.
    static let maxTextBytes = 8 * 1024

    static func encodeManifest(_ manifest: WorkActionManifest) throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        return try encoder.encode(manifest)
    }
}

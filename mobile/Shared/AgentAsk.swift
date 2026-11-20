import Foundation

/// A question for Sona's agent, sealed into the vault for the paired Mac to answer.
///
/// Field names are the wire names `src-tauri/src/cloud_sync` parses; the phone has no
/// relay key, so the Mac runs the turn and seals an `AgentReplyManifest` back.
struct AgentAskManifest: Codable, Equatable {
    struct Turn: Codable, Equatable {
        enum Role: String, Codable { case user, assistant }
        var message: String
        var role: Role
    }

    var asked_at_utc_ms: Int64
    var conversation_id: String
    var device_id: String
    var format_version: Int
    var kind: String
    var question: String
    var recent_turns: [Turn]
}

/// The Mac's answer to one ask: first `working` when it takes the question, then
/// `answered` or `failed`.
struct AgentReplyManifest: Codable, Equatable {
    enum State: String, Codable { case working, answered, failed }

    var answer: String?
    var ask_object_id: String
    var device_id: String
    var failure: String?
    var format_version: Int
    var kind: String
    var state: State
    var updated_at_utc_ms: Int64

    /* The Mac refuses unknown fields and writes nil as null, so the phone writes every key. */
    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(answer, forKey: .answer)
        try container.encode(ask_object_id, forKey: .ask_object_id)
        try container.encode(device_id, forKey: .device_id)
        try container.encode(failure, forKey: .failure)
        try container.encode(format_version, forKey: .format_version)
        try container.encode(kind, forKey: .kind)
        try container.encode(state, forKey: .state)
        try container.encode(updated_at_utc_ms, forKey: .updated_at_utc_ms)
    }
}

enum AgentAskObject {
    static let kind = "agent_ask"
    static let replyKind = "agent_reply"
    /// The `source_format` bound into an ask's HKDF info and AES-GCM AAD.
    static let sourceFormat = "sona-agent-ask-v1"
    static let replySourceFormat = "sona-agent-reply-v1"
    /// What the Mac's chat turn accepts.
    static let maxQuestionBytes = 8 * 1024
    static let maxRecentTurns = 8
    static let maxRecentTurnBytes = 32 * 1024

    /// Why the Mac's chat turn would refuse `question`, or nil when it would take it.
    /// Mirrors `is_safe_text` in `src-tauri/src/agent_panel/protocol.rs`.
    enum Refusal: Equatable { case empty, tooLong, address }

    static func refusal(_ question: String) -> Refusal? {
        if question.isEmpty { return .empty }
        if question.utf8.count > maxQuestionBytes { return .tooLong }
        if question.contains("\0") || question.hasPrefix("/") || question.hasPrefix("~/") || question.contains("://") {
            return .address
        }
        return nil
    }

    /// The Mac writes one reply object per ask, at an id both sides derive.
    static func replyObjectId(askObjectId: String) -> String {
        sha256Base64URL(Data("\(replySourceFormat):\(askObjectId)".utf8))
    }

    /// The newest turns that fit the Mac's chat turn.
    static func recent(_ turns: [AgentAskManifest.Turn]) -> [AgentAskManifest.Turn] {
        var kept: [AgentAskManifest.Turn] = []
        var bytes = 0
        for turn in turns.reversed() {
            bytes += turn.message.utf8.count
            guard kept.count < maxRecentTurns, bytes <= maxRecentTurnBytes else { break }
            kept.insert(turn, at: 0)
        }
        return kept
    }

    static func manifest(
        deviceId: String, askedAtUtcMs: Int64, conversationId: String, question: String,
        recentTurns: [AgentAskManifest.Turn]
    ) -> AgentAskManifest {
        AgentAskManifest(
            asked_at_utc_ms: askedAtUtcMs, conversation_id: conversationId, device_id: deviceId,
            format_version: 1, kind: kind, question: question, recent_turns: recent(recentTurns)
        )
    }

    static func encodeManifest(_ manifest: AgentAskManifest) throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        return try encoder.encode(manifest)
    }
}

import Foundation

struct WorkSession: Decodable, Identifiable, Equatable {
    var id: String
    var itemId: String
    var agent: String
    var state: WorkSessionState
    var unresponsive: Bool
    var createdAtMs: Int64
    var lastActivityAtMs: Int64?
    var heartbeatAtMs: Int64?
    var stopRequestedAtMs: Int64?
    var stopRequestedBy: String?
    var endedAtMs: Int64?
    var plan: [WorkPlanStep]
    var question: WorkSessionActivity?
    var unreadPrompts: [WorkSessionActivity]
    var activities: [WorkSessionActivity]

    var isOpen: Bool { endedAtMs == nil }
}

enum WorkSessionState: String, Decodable, Hashable {
    case pending, active, awaitingInput = "awaiting_input", stale, complete, error
    case unknown

    init(from decoder: Decoder) throws {
        self = Self(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
    }
}

struct WorkPlanStep: Decodable, Equatable {
    var content: String
    var status: WorkPlanStatus
}

enum WorkPlanStatus: String, Decodable, Hashable {
    case pending, inProgress = "in_progress", completed, canceled
    case unknown

    init(from decoder: Decoder) throws {
        self = Self(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
    }
}

struct WorkSessionActivity: Decodable, Identifiable, Equatable {
    var id: String
    var kind: WorkSessionActivityKind
    var atMs: Int64
    var author: WorkCommentAuthor
    var body: String?
    var action: String?
    var parameter: String?
    var result: String?
    var options: [WorkSelectOption]
    var value: String?
    var ephemeral: Bool
}

enum WorkSessionActivityKind: String, Decodable, Hashable {
    case thought, action, response, error, elicitation, prompt, stop
    case unknown

    init(from decoder: Decoder) throws {
        self = Self(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
    }
}

struct WorkSelectOption: Decodable, Hashable {
    var label: String
    var value: String
}

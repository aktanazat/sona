import Foundation

/// Where a task made from an answer line was said.
enum WorkCaptureSource: Codable, Equatable {
    case link(url: String, title: String?)
    case dictation(historyId: Int64, startMs: Int64, endMs: Int64, quote: String)
    case meeting(sessionId: String, atMs: Int64, quote: String)
    case loop(loopId: String)

    private enum Key: String, CodingKey { case kind, url, title, historyId, startMs, endMs, quote, sessionId, atMs, loopId }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        switch try container.decode(String.self, forKey: .kind) {
        case "link":
            self = .link(
                url: try container.decode(String.self, forKey: .url),
                title: try container.decodeIfPresent(String.self, forKey: .title))
        case "dictation":
            self = .dictation(
                historyId: try container.decode(Int64.self, forKey: .historyId),
                startMs: try container.decode(Int64.self, forKey: .startMs),
                endMs: try container.decode(Int64.self, forKey: .endMs),
                quote: try container.decode(String.self, forKey: .quote))
        case "meeting":
            self = .meeting(
                sessionId: try container.decode(String.self, forKey: .sessionId),
                atMs: try container.decode(Int64.self, forKey: .atMs),
                quote: try container.decode(String.self, forKey: .quote))
        case "loop":
            self = .loop(loopId: try container.decode(String.self, forKey: .loopId))
        case let other:
            throw DecodingError.dataCorruptedError(forKey: .kind, in: container, debugDescription: "Unknown source \(other)")
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        switch self {
        case let .link(url, title):
            try container.encode("link", forKey: .kind)
            try container.encode(url, forKey: .url)
            try container.encodeIfPresent(title, forKey: .title)
        case let .dictation(historyId, startMs, endMs, quote):
            try container.encode("dictation", forKey: .kind)
            try container.encode(historyId, forKey: .historyId)
            try container.encode(startMs, forKey: .startMs)
            try container.encode(endMs, forKey: .endMs)
            try container.encode(quote, forKey: .quote)
        case let .meeting(sessionId, atMs, quote):
            try container.encode("meeting", forKey: .kind)
            try container.encode(sessionId, forKey: .sessionId)
            try container.encode(atMs, forKey: .atMs)
            try container.encode(quote, forKey: .quote)
        case let .loop(loopId):
            try container.encode("loop", forKey: .kind)
            try container.encode(loopId, forKey: .loopId)
        }
    }
}

/// `work_ask`: the lines across meetings, open loops and dictations that
/// answer a question, best first.
struct WorkAnswer: Decodable, Equatable {
    var question: String
    /// What the question was read as: the words, the person, the stretch of
    /// time.
    var readAs: [String]
    var lines: [WorkAnswerLine]
}

struct WorkAnswerLine: Codable, Identifiable, Equatable, Hashable {
    var id: String
    var quote: String
    var title: String
    var speaker: String?
    var personId: String?
    var whenUtcMs: Int64
    var link: String
    var source: WorkCaptureSource
    var play: WorkAnswerPlay?

    static func == (lhs: Self, rhs: Self) -> Bool { lhs.id == rhs.id }
    func hash(into hasher: inout Hasher) { hasher.combine(id) }
}

struct WorkAnswerPlay: Codable, Equatable {
    var sessionId: String
    var segmentId: String
    var startOffsetNs: UInt64
}

struct WorkTakenLine: Encodable {
    var quote: String
    var source: WorkCaptureSource
    var personId: String?
}

/// `work_command`: a sentence read as one change to many tasks, with the
/// tasks it changes.
struct WorkCommand: Decodable, Equatable {
    var said: String
    /// What happens to each task: "Move to Monday, Nov 9".
    var summary: String
    /// Which tasks, a phrase each: "Yours", "Not urgent".
    var filters: [String]
    var change: WorkCommandChange
    var items: [WorkItemRow]
}

enum WorkCommandChange: Decodable, Equatable {
    case update(WorkPatch)
    case archive

    private enum Key: String, CodingKey { case kind, patch }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        switch try container.decode(String.self, forKey: .kind) {
        case "update": self = .update(try container.decode(WorkPatch.self, forKey: .patch))
        case "archive": self = .archive
        case let other:
            throw DecodingError.dataCorruptedError(forKey: .kind, in: container, debugDescription: "Unknown change \(other)")
        }
    }
}

extension WorkPatch: Decodable {
    private enum Field: String, CodingKey {
        case title, details, status, priority, projectId, milestoneId, owner, dueAtMs, labelIds, snoozedUntilMs
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Field.self)
        self.init(
            title: try container.decodeIfPresent(String.self, forKey: .title),
            details: try Self.value(.details, in: container),
            status: try container.decodeIfPresent(WorkStatus.self, forKey: .status),
            priority: try container.decodeIfPresent(Int.self, forKey: .priority),
            projectId: try Self.value(.projectId, in: container),
            milestoneId: try Self.value(.milestoneId, in: container),
            owner: try container.decodeIfPresent(WorkOwner.self, forKey: .owner),
            dueAtMs: try Self.value(.dueAtMs, in: container),
            labelIds: try container.decodeIfPresent([String].self, forKey: .labelIds),
            snoozedUntilMs: try Self.value(.snoozedUntilMs, in: container))
    }

    /// Absent stays as it is; null clears.
    private static func value<T: Decodable & Equatable>(
        _ key: Field, in container: KeyedDecodingContainer<Field>
    ) throws -> WorkPatchValue<T>? {
        guard container.contains(key) else { return nil }
        if try container.decodeNil(forKey: key) { return .clear }
        return .set(try container.decode(T.self, forKey: key))
    }
}

/// A word or phrase Sona listens for in processed meetings and dictations.
struct WorkWatch: Decodable, Identifiable, Equatable {
    var id: String
    var phrase: String
    /// Only lines this person says.
    var personId: String?
    var personName: String?
    var createdAtMs: Int64
    /// The latest times it came up, newest first.
    var hits: [WorkWatchHit]
}

struct WorkWatchHit: Decodable, Identifiable, Equatable {
    var id: String
    var watchId: String
    var phrase: String
    var heardAtMs: Int64
    /// The task kept from it.
    var taskId: String?
    var line: WorkAnswerLine
}

/// `work-watch-opened`: a watch notification was clicked, or its ▶ pressed.
struct WorkWatchOpenedEvent: Decodable {
    enum Gesture: String, Decodable { case open, play }
    var hit: WorkWatchHit
    var gesture: Gesture
}

/// `work-command-heard`: a dictation was a change to tasks, not tasks.
struct WorkCommandHeardEvent: Decodable {
    var said: String
    var historyId: Int64?
}

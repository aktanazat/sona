import Foundation

enum WorkFavoriteKind: String, Codable, Hashable {
    case task, project, view
    case unknown

    init(from decoder: Decoder) throws {
        self = Self(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
    }
}

/// A starred task, project or saved view, in the order it was starred.
struct WorkFavorite: Codable, Hashable {
    var kind: WorkFavoriteKind
    var targetId: String
    var createdAtMs: Int64
}

enum WorkSubscribeReason: String, Codable, Hashable {
    case subscribed, created, assigned, mentioned
    case unknown

    init(from decoder: Decoder) throws {
        self = Self(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
    }

    /// Why the inbox hears about the task.
    var label: String {
        switch self {
        case .subscribed: "You subscribed"
        case .created: "You created it"
        case .assigned: "It was assigned to you"
        case .mentioned: "You were mentioned"
        case .unknown: "Subscribed"
        }
    }
}

struct WorkSubscription: Codable, Equatable {
    var itemId: String
    var reason: WorkSubscribeReason
    var createdAtMs: Int64
}

enum WorkNotificationKind: String, Codable, Hashable {
    case status, comment, mention, assigned
    case unknown

    init(from decoder: Decoder) throws {
        self = Self(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
    }
}

/// Something Sona or an agent did to a task the user follows.
struct WorkNotification: Codable, Identifiable, Equatable {
    var id: String
    var itemId: String
    var kind: WorkNotificationKind
    var actor: WorkActor
    var agent: String?
    var commentId: String?
    var from: String?
    var to: String?
    var createdAtMs: Int64
    var readAtMs: Int64?
    var snoozedUntilMs: Int64?
    var archivedAtMs: Int64?

    /// "Claude Code", "Sona", or "An agent" when it gave no name.
    var who: String {
        if let agent, !agent.isEmpty { return agent }
        return actor == .sona ? "Sona" : "An agent"
    }

    /// What happened, after who did it.
    var what: String {
        switch kind {
        case .status:
            let status = to.flatMap(WorkStatus.init(rawValue:))
            return status.map { "moved it to \($0.label)" } ?? "changed its status"
        case .comment: return "commented"
        case .mention: return "mentioned you"
        case .assigned: return "assigned it to you"
        case .unknown: return "changed it"
        }
    }
}

struct WorkInbox: Codable, Equatable {
    var unread: Int
    var notifications: [WorkNotification]
}

/// What an inbox mark does: `untilMs` only for a snooze.
enum WorkInboxMark: String, Encodable {
    case read, unread, archive, unarchive, snooze
}

/// A task in Recently deleted, and when it goes for good.
struct WorkBinned: Codable, Identifiable, Equatable {
    var item: WorkItem
    var trashedAtMs: Int64
    var purgeAtMs: Int64

    var id: String { item.id }
}

struct WorkArchiveList: Codable, Equatable {
    var archived: [WorkItem]
    var deleted: [WorkBinned]
}

/// One saved text of a task's description, newest first in a list.
struct WorkDescriptionVersion: Codable, Identifiable, Equatable {
    var id: String
    var itemId: String
    var body: String?
    /// None for the text the task had before its first edit.
    var actor: WorkActor?
    var agent: String?
    var createdAtMs: Int64

    var author: String {
        switch actor {
        case .me?: "You"
        case .sona?: "Sona"
        case .agent?: agent ?? "An agent"
        case nil: "Original"
        }
    }
}

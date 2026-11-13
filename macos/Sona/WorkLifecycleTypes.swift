import Foundation

enum WorkFavoriteKind: String, Codable, Hashable {
    case task, project, view, person
    case unknown

    init(from decoder: Decoder) throws {
        self = Self(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
    }
}

/// A starred task, project, saved view or person, loose at the top or in a
/// folder, at its place there.
struct WorkFavorite: Codable, Hashable {
    var kind: WorkFavoriteKind
    var targetId: String
    var folderId: String?
    var position: Int
    var createdAtMs: Int64

    var entry: WorkFavoriteEntry { .favorite(kind: kind, id: targetId) }
}

/// A favorites folder. Folders sit at the top among the loose favorites and
/// hold favorites, never other folders.
struct WorkFavoriteFolder: Codable, Hashable, Identifiable {
    var id: String
    var name: String
    var position: Int
    var createdAtMs: Int64
}

/// Every folder and every favorite, as `work_favorites` answers.
struct WorkFavorites: Decodable {
    var folders: [WorkFavoriteFolder]
    var favorites: [WorkFavorite]
}

/// One place in the favorites' order: a favorite or a folder.
enum WorkFavoriteEntry: Codable, Hashable {
    case favorite(kind: WorkFavoriteKind, id: String)
    case folder(id: String)
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
    case status, comment, mention, assigned, changed, reminder, due, automation, digest
    case unknown

    init(from decoder: Decoder) throws {
        self = Self(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
    }
}

/// The automation run an automation notice is about.
struct WorkNoticeAutomation: Codable, Equatable {
    var id: String
    var runId: String
    var name: String
    var error: String?
}

/// The user's projects over one week, as the digest notice keeps them.
struct WorkProjectDigest: Decodable, Equatable {
    var fromMs: Int64
    var toMs: Int64
    var projects: [WorkDigestProject]
}

struct WorkDigestProject: Decodable, Equatable, Identifiable {
    var id: String
    var title: String
    var status: PlanningProjectStatus
    var statusName: String?
    var percent: Int
    var targetAtMs: Int64?
    var latestUpdate: WorkDigestUpdate?
    var done: [WorkDigestTask]
    var risks: [WorkDigestRisk]
}

struct WorkDigestUpdate: Decodable, Equatable {
    var health: PlanningProjectHealth
    var excerpt: String?
    var author: PlanningProjectUpdateAuthor
    var createdAtMs: Int64
}

struct WorkDigestTask: Decodable, Equatable, Identifiable {
    var id: String
    var number: Int64
    var title: String
}

/// One reason a project is at risk; `kind` says which fields it carries.
struct WorkDigestRisk: Decodable, Equatable {
    var kind: String
    var health: PlanningProjectHealth?
    var targetAtMs: Int64?
    var expectedAtMs: Int64?
    var tasks: [WorkDigestTask]?
    var title: String?
    var dueAtMs: Int64?
    var lastAtMs: Int64?
}

/// Something Sona or an agent did to a task the user follows, a task's
/// time coming, an automation run ending, or the weekly project digest.
struct WorkNotification: Decodable, Identifiable, Equatable {
    var id: String
    /// Nil only for an automation run about no task.
    var itemId: String?
    var kind: WorkNotificationKind
    var actor: WorkActor
    var agent: String?
    var commentId: String?
    var field: String?
    var automation: WorkNoticeAutomation?
    var from: String?
    var to: String?
    var createdAtMs: Int64
    var readAtMs: Int64?
    var snoozedUntilMs: Int64?
    var archivedAtMs: Int64?
    var digest: WorkProjectDigest?

    /// "Claude Code", "Sona", or "An agent" when it gave no name.
    var who: String {
        if kind == .reminder || kind == .due || kind == .digest { return "Sona" }
        if kind == .automation {
            guard itemId != nil, let automation else { return "Automation" }
            return "Automation “\(automation.name)”"
        }
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
        case .changed: return changed
        case .reminder: return "reminds you"
        case .due: return "says it's due today"
        case .digest: return digestSummary
        case .automation:
            guard to == "done" else {
                return automation?.error.map { "failed: \($0)" } ?? "failed"
            }
            return "finished"
        case .unknown: return "changed it"
        }
    }

    private var changed: String {
        switch field {
        case "priority": "changed the priority"
        case "owner": "reassigned it"
        case "due": to == nil ? "cleared the due date" : "changed the due date"
        case "project": "moved it to another project"
        case "title": "renamed it"
        case "details": "edited the description"
        case "labels": "changed the labels"
        case "archived": "archived it"
        case "restored": "restored it"
        case "snoozed": "snoozed it"
        case "relation", "block": "changed its links"
        case "recurrence": "changed how it repeats"
        case "parent": "changed its parent"
        case "attachment": "changed its attachments"
        case "reminder": to == nil ? "cleared the reminder" : "set a reminder"
        default: "changed it"
        }
    }

    var failed: Bool { kind == .automation && to != "done" }

    private var digestSummary: String {
        guard let projects = digest?.projects else { return "sent the weekly project digest" }
        let done = projects.reduce(0) { $0 + $1.done.count }
        let risky = projects.filter { !$0.risks.isEmpty }.count
        let count = projects.count == 1 ? "1 project" : "\(projects.count) projects"
        var parts = [done == 1 ? "1 task done" : "\(done) tasks done"]
        if risky > 0 { parts.append("\(risky) at risk") }
        return "sums up \(count): " + parts.joined(separator: ", ")
    }
}

struct WorkInbox: Decodable, Equatable {
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

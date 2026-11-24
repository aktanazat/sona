import Foundation

/// The tracker's shapes as the core sends them, from
/// `src-tauri/src/planning/work/types.rs`.

/// Where an item came from, which picks its triage split.
enum WorkSource: String, Codable, CaseIterable, Hashable {
    case meeting, dictation, agent, person
}

enum WorkOwner: Codable, Hashable {
    case myself
    case person(String)
    case nobody

    private enum Key: String, CodingKey { case kind, personId }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let kind = try container.decode(String.self, forKey: .kind)
        switch kind {
        case "self": self = .myself
        case "person": self = .person(try container.decode(String.self, forKey: .personId))
        case "nobody": self = .nobody
        default:
            throw DecodingError.dataCorruptedError(forKey: .kind, in: container, debugDescription: "Unknown owner \(kind)")
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        switch self {
        case .myself: try container.encode("self", forKey: .kind)
        case let .person(id):
            try container.encode("person", forKey: .kind)
            try container.encode(id, forKey: .personId)
        case .nobody: try container.encode("nobody", forKey: .kind)
        }
    }
}

/// A patch field that can be emptied: `.clear` sends `null`, while a field
/// left `nil` in `WorkPatch` is not sent and stays as it is.
enum WorkPatchValue<T: Equatable>: Equatable {
    case set(T)
    case clear
}

struct WorkPatch: Equatable {
    var title: String?
    var details: WorkPatchValue<String>?
    var status: WorkStatus?
    var priority: Int?
    var projectId: WorkPatchValue<String>?
    var milestoneId: WorkPatchValue<String>?
    var owner: WorkOwner?
    var dueAtMs: WorkPatchValue<Int64>?
    var labelIds: [String]?
    var snoozedUntilMs: WorkPatchValue<Int64>?
    var recurrence: WorkPatchValue<String>?
    var cycle: WorkPatchValue<Int64>?
    var parentId: WorkPatchValue<String>?
    var estimate: WorkPatchValue<Int>?
    var forPersonId: WorkPatchValue<String>?

    init(
        title: String? = nil, details: WorkPatchValue<String>? = nil, status: WorkStatus? = nil,
        priority: Int? = nil, projectId: WorkPatchValue<String>? = nil, milestoneId: WorkPatchValue<String>? = nil,
        owner: WorkOwner? = nil, dueAtMs: WorkPatchValue<Int64>? = nil, labelIds: [String]? = nil,
        snoozedUntilMs: WorkPatchValue<Int64>? = nil, recurrence: WorkPatchValue<String>? = nil,
        cycle: WorkPatchValue<Int64>? = nil, parentId: WorkPatchValue<String>? = nil,
        estimate: WorkPatchValue<Int>? = nil, forPersonId: WorkPatchValue<String>? = nil
    ) {
        self.title = title
        self.details = details
        self.status = status
        self.priority = priority
        self.projectId = projectId
        self.milestoneId = milestoneId
        self.owner = owner
        self.dueAtMs = dueAtMs
        self.labelIds = labelIds
        self.snoozedUntilMs = snoozedUntilMs
        self.recurrence = recurrence
        self.cycle = cycle
        self.parentId = parentId
        self.estimate = estimate
        self.forPersonId = forPersonId
    }
}

extension WorkPatch: Encodable {
    private enum Key: String, CodingKey {
        case title, details, status, priority, projectId, milestoneId, owner, dueAtMs, labelIds, snoozedUntilMs
        case recurrence, cycle, parentId, estimate, forPersonId
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        try container.encodeIfPresent(title, forKey: .title)
        try Self.encode(details, .details, in: &container)
        try container.encodeIfPresent(status, forKey: .status)
        try container.encodeIfPresent(priority, forKey: .priority)
        try Self.encode(projectId, .projectId, in: &container)
        try Self.encode(milestoneId, .milestoneId, in: &container)
        try container.encodeIfPresent(owner, forKey: .owner)
        try Self.encode(dueAtMs, .dueAtMs, in: &container)
        try container.encodeIfPresent(labelIds, forKey: .labelIds)
        try Self.encode(snoozedUntilMs, .snoozedUntilMs, in: &container)
        try Self.encode(recurrence, .recurrence, in: &container)
        try Self.encode(cycle, .cycle, in: &container)
        try Self.encode(parentId, .parentId, in: &container)
        try Self.encode(estimate, .estimate, in: &container)
        try Self.encode(forPersonId, .forPersonId, in: &container)
    }

    private static func encode<T: Encodable & Equatable>(
        _ value: WorkPatchValue<T>?, _ key: Key, in container: inout KeyedEncodingContainer<Key>
    ) throws {
        switch value {
        case nil: break
        case .clear?: try container.encodeNil(forKey: key)
        case let .set(inner)?: try container.encode(inner, forKey: key)
        }
    }
}

struct WorkItem: Codable, Identifiable, Equatable {
    var id: String
    var number: Int64
    var title: String
    var details: String?
    var status: WorkStatus
    var priority: Int
    var projectId: String?
    var milestoneId: String?
    var ownerPersonId: String?
    var ownerIsSelf: Bool
    var direction: WorkDirection
    var dueAtMs: Int64?
    var labelIds: [String]
    var sourceLoopId: String?
    var duplicateOfId: String?
    var snoozedUntilMs: Int64?
    var archivedAtMs: Int64?
    var statusChangedAtMs: Int64
    var createdAtMs: Int64
    var updatedAtMs: Int64
    var revision: Int64
    var sortOrder: Double
    /// The number of the cycle it is planned in.
    var cycle: Int64?
    var parentId: String?
    /// Points of effort, 0 to 64.
    var estimate: Int?
    /// When it was marked done; nil while it is not done.
    var completedAtMs: Int64?
    /// The person the work is for, who is owed it.
    var forPersonId: String?
}

/// An item with the names its row shows; the core sends the item's fields
/// at the top level beside them.
struct WorkItemRow: Codable, Identifiable, Equatable {
    var id: String
    var number: Int64
    var title: String
    var details: String?
    var status: WorkStatus
    var priority: Int
    var projectId: String?
    var milestoneId: String?
    var ownerPersonId: String?
    var ownerIsSelf: Bool
    var direction: WorkDirection
    var dueAtMs: Int64?
    var labelIds: [String]
    var sourceLoopId: String?
    var duplicateOfId: String?
    var snoozedUntilMs: Int64?
    var archivedAtMs: Int64?
    var statusChangedAtMs: Int64
    var createdAtMs: Int64
    var updatedAtMs: Int64
    var revision: Int64
    var sortOrder: Double
    var cycle: Int64?
    var parentId: String?
    var estimate: Int?
    var completedAtMs: Int64?
    var forPersonId: String?
    var subTasks: WorkProgress?
    var projectTitle: String?
    var ownerName: String?
    /// The agent working the item; none when the user is.
    var doer: WorkDoer?
    var sourceMeetingId: String?
    var sourceMeetingTitle: String?
    var sourceQuote: String?
    var sourceSpeaker: String?
    /// Offset into the source meeting where the commitment was said.
    var sourceAtMs: Int64?
    var blockedByOpenCount: Int
    /// The meetings that raised the item or a duplicate of it, counted once
    /// each.
    var raisedIn: Int
    var source: WorkSource
    /// When to ask about waiting-on work; nil for anything else.
    var followUpAtMs: Int64?
    /// Mine, open, due within two days, nothing booked for it, and no free
    /// hour left on its due day.
    var atRisk: Bool
    var timeBlock: WorkTimeBlock?
    var `repeat`: WorkRepeat?
    var attachmentCount: Int

    var item: WorkItem {
        WorkItem(
            id: id, number: number, title: title, details: details, status: status, priority: priority,
            projectId: projectId, milestoneId: milestoneId, ownerPersonId: ownerPersonId, ownerIsSelf: ownerIsSelf,
            direction: direction, dueAtMs: dueAtMs, labelIds: labelIds, sourceLoopId: sourceLoopId,
            duplicateOfId: duplicateOfId, snoozedUntilMs: snoozedUntilMs, archivedAtMs: archivedAtMs,
            statusChangedAtMs: statusChangedAtMs, createdAtMs: createdAtMs, updatedAtMs: updatedAtMs,
            revision: revision, sortOrder: sortOrder, cycle: cycle, parentId: parentId,
            estimate: estimate, completedAtMs: completedAtMs, forPersonId: forPersonId)
    }

    var owner: WorkOwner {
        if ownerIsSelf { return .myself }
        return ownerPersonId.map { .person($0) } ?? .nobody
    }
}

/// Estimate points offered, on Linear's exponential scale.
enum TaskEstimate {
    static let points = [1, 2, 4, 8, 16]

    static func title(_ points: Int) -> String { points == 1 ? "1 point" : "\(points) points" }
}

struct WorkProgress: Codable, Equatable {
    var done: Int
    var total: Int
}

struct WorkItemRef: Codable, Equatable {
    var id: String
    var number: Int64
    var title: String
    var status: WorkStatus
}

struct WorkCommentAuthor: Codable, Equatable {
    var kind: WorkActor
    var name: String?

    /// `me` is the profile name, when the user gave one.
    func display(me: String?) -> String {
        switch kind {
        case .me: me ?? "You"
        case .sona: "Sona"
        case .agent: name ?? "Agent"
        }
    }
}

struct WorkComment: Codable, Identifiable, Equatable {
    var id: String
    var itemId: String
    var parentId: String?
    var author: WorkCommentAuthor
    var body: String?
    var createdAtMs: Int64
    var editedAtMs: Int64?
    var resolvedAtMs: Int64?
    var deleted: Bool
    var replies: [WorkComment]
}

enum WorkAttachmentKind: String, Codable, Hashable { case file, image, link }

struct WorkAttachment: Codable, Identifiable, Equatable {
    var id: String
    var itemId: String
    var kind: WorkAttachmentKind
    var title: String
    var subtitle: String?
    var url: String?
    var path: String?
    var originalName: String?
    var mime: String?
    var size: Int64?
    var sha256: String?
    var createdAtMs: Int64
    var createdBy: WorkCommentAuthor
    /// Key-value facts an integration keeps on it.
    var metadata: [String: String]
    /// The comment it was added with; nil for one on the task itself.
    var commentId: String?

    var fileURL: URL? { path.map { URL(fileURLWithPath: $0) } }
    var linkURL: URL? { url.flatMap(URL.init(string:)) }
}

struct WorkTimeBlock: Codable, Equatable {
    var startsAtMs: Int64
    var endsAtMs: Int64
}

/// How a repeating item comes back; of the instances whose due day has
/// come, `kept` were done by the end of it out of `of`.
struct WorkRepeat: Codable, Equatable {
    var every: String
    var kept: Int
    var of: Int
}

/// How long cycles run, the weekday they start (1 for Monday through 7 for
/// Sunday), the weeks of rest between them, how many are kept ahead, and
/// whether started or done work with no cycle joins one by itself. The core
/// resets a field left out, so every save sends them all.
struct WorkCycleSettings: Codable, Equatable {
    var weeks: Int
    var startWeekday: Int
    var cooldownWeeks: Int
    var upcoming: Int
    var autoAddStarted: Bool
    var autoAddCompleted: Bool
}

enum WorkCycleState: String, Codable, Hashable { case past, current, upcoming }

/// One cycle; days are local `YYYY-MM-DD` and `endDay` is the first day
/// after it.
struct WorkCycle: Codable, Identifiable, Equatable {
    var number: Int64
    var name: String?
    var startDay: String
    var endDay: String
    var startsAtMs: Int64
    var endsAtMs: Int64
    var state: WorkCycleState
    var scope: Int
    var started: Int
    var done: Int
    /// Done at 100 and started at 25 over scope, set once the cycle ends.
    var success: Int?
    /// Tasks in it that joined after it began, by id; rolled-over work
    /// counts as planned. Nil from a core too old to send it.
    var addedAfterStart: [String]?

    var id: Int64 { number }
    var title: String { name ?? "Cycle \(number)" }
}

struct WorkCycles: Codable, Equatable {
    var settings: WorkCycleSettings
    var cycles: [WorkCycle]
    /// The tasks a cycle usually finishes: the mean done of the last three
    /// ended cycles. Nil until that mean reaches one task.
    var capacity: Int?
}

struct WorkCycleDay: Codable, Equatable, Identifiable {
    var day: String
    var scope: Int
    var started: Int
    var done: Int

    var id: String { day }
}

/// Where an even pace would have the done count by a day's end.
struct WorkCycleTarget: Codable, Equatable {
    var day: String
    var done: Double
}

struct WorkCycleProgress: Codable, Equatable {
    var cycle: WorkCycle
    var days: [WorkCycleDay]
    var carriedOver: Int
    /// Every day of the cycle, flat on weekends.
    var target: [WorkCycleTarget]
}

/// The agent working an item, by the name it goes by, since it took it.
struct WorkDoer: Codable, Equatable {
    var agent: String
    var sinceMs: Int64
}

enum WorkProposalField: String, Codable, Hashable {
    case title, details, status, priority, owner, due, project, milestone, labels, snoozed, recurrence, cycle, parent, estimate
    case forPerson = "for_person"
    case unknown

    init(from decoder: Decoder) throws {
        self = Self(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
    }
}

/// One field an agent would change, waiting for the user. `from` is the
/// value when it was proposed and `to` the value proposed, both in the form
/// a history entry keeps; details carry their whole text.
struct WorkProposal: Codable, Identifiable, Equatable {
    var id: String
    var itemId: String
    var agent: String
    var field: WorkProposalField
    var from: String?
    var to: String?
    var createdAtMs: Int64
}

/// What handing an item to an agent left: its doer, and the prompt to paste
/// into that agent, absent when the user took the item back.
struct WorkDelegation: Decodable {
    var doer: WorkDoer?
    var prompt: String?
}

/// A new item. Fields left `nil` start as not started, at no priority, with
/// nobody owning it.
struct WorkItemInput: Encodable, Equatable {
    var title: String
    var details: String?
    var status: WorkStatus?
    var priority: Int?
    var projectId: String?
    var milestoneId: String?
    var owner: WorkOwner?
    var dueAtMs: Int64?
    var labelIds: [String]?
    var recurrence: String?
}

struct WorkLabel: Codable, Identifiable, Equatable {
    var id: String
    var name: String
    /// Degrees on the color wheel, 0 to 360.
    var hue: Double
    var revision: Int64
    var group: String?
    var retiredAtMs: Int64?
    var scope: WorkLabelScope = .all

    var retired: Bool { retiredAtMs != nil }
}

extension WorkLabel {
    /// A core from before label scopes sends none; its labels go anywhere.
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        name = try container.decode(String.self, forKey: .name)
        hue = try container.decode(Double.self, forKey: .hue)
        revision = try container.decode(Int64.self, forKey: .revision)
        group = try container.decodeIfPresent(String.self, forKey: .group)
        retiredAtMs = try container.decodeIfPresent(Int64.self, forKey: .retiredAtMs)
        scope = try container.decodeIfPresent(WorkLabelScope.self, forKey: .scope) ?? .all
    }
}

/// Where a label can go: on tasks and projects, or on only one of them.
enum WorkLabelScope: String, Codable, CaseIterable, Identifiable {
    case all, tasks, projects

    var id: String { rawValue }

    var label: String {
        switch self {
        case .all: "Tasks and projects"
        case .tasks: "Tasks only"
        case .projects: "Projects only"
        }
    }
}

struct WorkCounts: Codable, Equatable {
    var triage: Int
    var mineOpen: Int
    var mineOverdue: Int
    var mineDueToday: Int
    var waitingOn: Int
    /// Waiting-on work whose follow-up moment has come.
    var waitingOnStale: Int
}

/// One local day's plan: the tasks picked to focus on, in the order they
/// were picked, and when; a day never planned has no `plannedAtMs`.
struct WorkDay: Codable, Equatable {
    var day: String
    var focusIds: [String]
    var plannedAtMs: Int64?
}

enum WorkActor: String, Codable, Hashable { case me, sona, agent }

enum WorkActivityKind: String, Codable, Hashable {
    case created, status, priority, owner, due, project, title, details, labels, archived, restored
    case duplicate, accepted, declined, snoozed, relation, kept, recurrence, cycle, block, parent, comment
    case attachment, estimate
    case forPerson = "for_person"
    case unknown

    init(from decoder: Decoder) throws {
        self = Self(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
    }
}

/// One change in an item's history. `from` and `to` hold raw values: a
/// status, a priority from "0" to "4", "self", "nobody" or a person id, a
/// due time in epoch ms, label ids joined by commas, or for relations
/// "blocks:", "blocked_by:" or "related:" and the other item's id.
struct WorkActivity: Codable, Identifiable, Equatable {
    var id: String
    var itemId: String
    var atMs: Int64
    var actor: WorkActor
    var kind: WorkActivityKind
    var from: String?
    var to: String?
    /// The agent behind an `agent` entry, when it gave its name.
    var agent: String?
    /// How that agent reached Sona: "mcp", "cli" or "chat", when recorded.
    var via: String?
    var reason: String?
}

enum WorkRelationKind: String, Codable, Hashable { case blocks, related }

struct WorkRelation: Codable, Identifiable, Equatable {
    var id: String
    var fromId: String
    var toId: String
    var kind: WorkRelationKind
}

enum WorkSuggestionField: String, Codable, Hashable { case owner, due, project, label, duplicate, related }

enum WorkSuggestionValue: Equatable {
    case owner(WorkOwner)
    case due(Int64)
    case project(String)
    case label(String)
    case item(String)
}

struct WorkFieldSuggestion: Equatable {
    var id: String
    var field: WorkSuggestionField
    var value: WorkSuggestionValue
    var reason: String
    var quote: String?
    /// Offset into the meeting the suggestion was read from.
    var atMs: Int64?
    var meetingId: String?
}

extension WorkFieldSuggestion: Decodable {
    private enum Key: String, CodingKey { case id, field, value, reason, quote, atMs, meetingId }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        id = try container.decode(String.self, forKey: .id)
        field = try container.decode(WorkSuggestionField.self, forKey: .field)
        switch field {
        case .owner: value = .owner(try container.decode(WorkOwner.self, forKey: .value))
        case .due: value = .due(try container.decode(Int64.self, forKey: .value))
        case .project: value = .project(try container.decode(String.self, forKey: .value))
        case .label: value = .label(try container.decode(String.self, forKey: .value))
        case .duplicate, .related: value = .item(try container.decode(String.self, forKey: .value))
        }
        reason = try container.decode(String.self, forKey: .reason)
        quote = try container.decodeIfPresent(String.self, forKey: .quote)
        atMs = try container.decodeIfPresent(Int64.self, forKey: .atMs)
        meetingId = try container.decodeIfPresent(String.self, forKey: .meetingId)
    }
}

struct WorkProjectChoice: Codable, Identifiable, Equatable {
    var id: String
    var title: String
    var status: PlanningProjectStatus
}

struct WorkPersonChoice: Codable, Identifiable, Equatable {
    var id: String
    var name: String
}

struct WorkList: Decodable, Equatable {
    var items: [WorkItemRow]
    var labels: [WorkLabel]
    var projects: [WorkProjectChoice]
    var people: [WorkPersonChoice]
    /// What Sona suggests for each listed item still in triage, by item id.
    var suggestions: [String: [WorkFieldSuggestion]]
    /// Changes agents proposed for listed items, by item id, oldest first.
    var proposals: [String: [WorkProposal]]
    /// False when the conversations could not be opened: every item is
    /// listed, but nothing read from its meeting, and no people.
    var conversationsAvailable: Bool
    /// Note, description and update edits automations proposed, oldest first.
    var documentProposals: [WorkDocProposal]
    /// Fields Sona filled by itself on listed items in Triage, by item id.
    var autoApplied: [String: [WorkAutoApplied]]
}

enum WorkDocProposalKind: String, Codable, Hashable {
    case note
    case projectDescription = "project_description"
    case projectUpdate = "project_update"
    case unknown

    init(from decoder: Decoder) throws {
        self = Self(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
    }
}

/// An edit an automation proposed to a note or a project, waiting for a yes
/// or no. `before` is the description it would replace.
struct WorkDocProposal: Decodable, Identifiable, Equatable {
    var id: String
    var kind: WorkDocProposalKind
    var targetId: String
    var targetTitle: String
    var agent: String
    var body: String
    var before: String?
    var health: PlanningProjectHealth?
    var createdAtMs: Int64
}

enum WorkAutoField: String, Codable, Hashable, CaseIterable, Identifiable {
    case project, labels, owner

    var id: String { rawValue }
}

/// A field Sona filled by itself on a task in Triage, until undone.
struct WorkAutoApplied: Decodable, Identifiable, Equatable {
    var id: String
    var taskId: String
    var field: WorkAutoField
    /// The project or label id, or "me" or a person id for an owner.
    var value: String
    var reason: String
    var appliedAtMs: Int64
}

struct WorkItemDetail: Decodable, Equatable {
    var item: WorkItemRow
    var recurrence: String?
    var evidence: [PlanningEvidenceLink]
    var relations: [WorkRelation]
    var activity: [WorkActivity]
    var suggestions: [WorkFieldSuggestion]
    /// False when the conversations could not be opened: the item's
    /// meeting, its meeting evidence and its people are not read.
    var conversationsAvailable: Bool
    var proposals: [WorkProposal]
    var parent: WorkItemRef?
    var subTasks: [WorkItem]
    var comments: [WorkComment]
    var attachments: [WorkAttachment]
}

/// The `work-changed` event: the items one committed change touched, or
/// none when only labels or views changed.
struct WorkChange: Decodable {
    var ids: [String]
}

/// `work_parse`: a typed title read for its fields. `title` is what is left
/// once the tokens are taken out, empty when nothing is.
struct WorkParse: Decodable, Equatable {
    var title: String
    var tokens: [WorkToken]
}

/// Words the core read as a field. `start` and `end` count UTF-16 code
/// units, `end` exclusive; tokens come in order and never overlap.
struct WorkToken: Decodable, Equatable, Identifiable {
    var text: String
    var start: Int
    var end: Int
    var field: WorkField

    var id: Int { start }
}

/// What a token sets. A label without an id does not exist yet. A repeat
/// carries the next time it falls due when no due date was said.
enum WorkField: Equatable {
    case due(at: Int64, timed: Bool)
    case priority(Int)
    case label(id: String?, name: String)
    case owner(WorkOwner, name: String)
    case project(id: String, title: String)
    case recurrence(String, nextAt: Int64?)
}

extension WorkField: Decodable {
    private enum Key: String, CodingKey { case kind, at, timed, level, id, name, owner, title, every, nextAt }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let kind = try container.decode(String.self, forKey: .kind)
        switch kind {
        case "due":
            self = .due(at: try container.decode(Int64.self, forKey: .at), timed: try container.decode(Bool.self, forKey: .timed))
        case "priority": self = .priority(try container.decode(Int.self, forKey: .level))
        case "label":
            self = .label(id: try container.decodeIfPresent(String.self, forKey: .id), name: try container.decode(String.self, forKey: .name))
        case "owner":
            self = .owner(try container.decode(WorkOwner.self, forKey: .owner), name: try container.decode(String.self, forKey: .name))
        case "project":
            self = .project(id: try container.decode(String.self, forKey: .id), title: try container.decode(String.self, forKey: .title))
        case "repeat":
            self = .recurrence(try container.decode(String.self, forKey: .every), nextAt: try container.decodeIfPresent(Int64.self, forKey: .nextAt))
        default:
            throw DecodingError.dataCorruptedError(forKey: .kind, in: container, debugDescription: "Unknown field \(kind)")
        }
    }
}

/// The page in front when the quick-entry shortcut was pressed. A task
/// made with it keeps the page as evidence.
struct WorkLink: Decodable, Equatable {
    var url: String
    var title: String?
}

/// `work-quick-entry`: the shortcut was pressed.
struct WorkQuickEntryEvent: Decodable {
    var link: WorkLink?
}

/// `work-ramble-filed`: what a spoken ramble put in Triage. No items and no
/// error means no task was heard.
struct WorkRambleFiledEvent: Decodable {
    var historyId: Int64?
    var items: [WorkItem]
    var error: String?
}

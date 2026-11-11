import Foundation

/// The tracker's shapes as the core sends them, from
/// `src-tauri/src/planning/work/types.rs`.

enum WorkStatus: String, Codable, CaseIterable, Identifiable, Hashable {
    case triage, backlog
    case notStarted = "not_started"
    case inProgress = "in_progress"
    case done, canceled, duplicate

    var id: String { rawValue }

    var label: String {
        switch self {
        case .triage: "Triage"
        case .backlog: "Backlog"
        case .notStarted: "Todo"
        case .inProgress: "In progress"
        case .done: "Done"
        case .canceled: "Canceled"
        case .duplicate: "Duplicate"
        }
    }

    var isOpen: Bool { self == .backlog || self == .notStarted || self == .inProgress }
}

enum WorkDirection: String, Codable, Hashable {
    case mine
    case waitingOn = "waiting_on"
    case unattributed
}

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

    init(
        title: String? = nil, details: WorkPatchValue<String>? = nil, status: WorkStatus? = nil,
        priority: Int? = nil, projectId: WorkPatchValue<String>? = nil, milestoneId: WorkPatchValue<String>? = nil,
        owner: WorkOwner? = nil, dueAtMs: WorkPatchValue<Int64>? = nil, labelIds: [String]? = nil,
        snoozedUntilMs: WorkPatchValue<Int64>? = nil, recurrence: WorkPatchValue<String>? = nil,
        cycle: WorkPatchValue<Int64>? = nil, parentId: WorkPatchValue<String>? = nil
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
    }
}

extension WorkPatch: Encodable {
    private enum Key: String, CodingKey {
        case title, details, status, priority, projectId, milestoneId, owner, dueAtMs, labelIds, snoozedUntilMs
        case recurrence, cycle, parentId
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
            revision: revision, sortOrder: sortOrder, cycle: cycle, parentId: parentId)
    }

    var owner: WorkOwner {
        if ownerIsSelf { return .myself }
        return ownerPersonId.map { .person($0) } ?? .nobody
    }
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

    var display: String {
        switch kind {
        case .me: "Aktan"
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

enum WorkCycleRef: Codable, Hashable {
    case current, next, noCycle
    case number(Int64)

    private enum Key: String, CodingKey { case kind, number }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        switch try container.decode(String.self, forKey: .kind) {
        case "current": self = .current
        case "next": self = .next
        case "none": self = .noCycle
        default: self = .number(try container.decode(Int64.self, forKey: .number))
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        switch self {
        case .current: try container.encode("current", forKey: .kind)
        case .next: try container.encode("next", forKey: .kind)
        case .noCycle: try container.encode("none", forKey: .kind)
        case let .number(number):
            try container.encode("number", forKey: .kind)
            try container.encode(number, forKey: .number)
        }
    }
}

/// How long cycles run, the weekday they start (1 for Monday through 7 for
/// Sunday), the weeks of rest between them and how many are kept ahead.
/// The core resets a field left out, so every save sends all four.
struct WorkCycleSettings: Codable, Equatable {
    var weeks: Int
    var startWeekday: Int
    var cooldownWeeks: Int
    var upcoming: Int
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

    var id: Int64 { number }
    var title: String { name ?? "Cycle \(number)" }
}

struct WorkCycles: Codable, Equatable {
    var settings: WorkCycleSettings
    var cycles: [WorkCycle]
}

struct WorkCycleDay: Codable, Equatable, Identifiable {
    var day: String
    var scope: Int
    var started: Int
    var done: Int

    var id: String { day }
}

struct WorkCycleProgress: Codable, Equatable {
    var cycle: WorkCycle
    var days: [WorkCycleDay]
    var carriedOver: Int
}

/// The agent working an item, by the name it goes by, since it took it.
struct WorkDoer: Codable, Equatable {
    var agent: String
    var sinceMs: Int64
}

enum WorkProposalField: String, Codable, Hashable {
    case title, details, status, priority, owner, due, project, milestone, labels, snoozed, recurrence, cycle, parent
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

/// A due window that moves with the calendar, on this Mac's clock. Weeks
/// start on Monday.
enum WorkDue: Codable, Hashable {
    /// Accepted, unfinished work due before today.
    case overdue
    case today
    /// Due from today through Sunday.
    case thisWeek
    /// Due from today through the given number of days after it.
    case nextDays(UInt16)
    case noDate

    private enum Key: String, CodingKey { case kind, days }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let kind = try container.decode(String.self, forKey: .kind)
        switch kind {
        case "overdue": self = .overdue
        case "today": self = .today
        case "this_week": self = .thisWeek
        case "next_days": self = .nextDays(try container.decode(UInt16.self, forKey: .days))
        case "no_date": self = .noDate
        default:
            throw DecodingError.dataCorruptedError(forKey: .kind, in: container, debugDescription: "Unknown due window \(kind)")
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        switch self {
        case .overdue: try container.encode("overdue", forKey: .kind)
        case .today: try container.encode("today", forKey: .kind)
        case .thisWeek: try container.encode("this_week", forKey: .kind)
        case let .nextDays(days):
            try container.encode("next_days", forKey: .kind)
            try container.encode(days, forKey: .days)
        case .noDate: try container.encode("no_date", forKey: .kind)
        }
    }
}

struct WorkQuery: Codable, Equatable, Hashable {
    var statuses: [WorkStatus]?
    var direction: WorkDirection?
    var ownerPersonIds: [String]?
    /// Project ids, where `"none"` stands for work in no project.
    var projectIds: [String]?
    var labelIds: [String]?
    var priorities: [Int]?
    var dueBeforeMs: Int64?
    /// Any of these windows; with `dueBeforeMs`, both must hold.
    var due: [WorkDue]?
    var sourceMeetingId: String?
    var search: String?
    var includeArchived = false
    var includeSnoozed = false
    var cycle: WorkCycleRef?
    /// Field conditions that must all hold, on top of the fields above.
    var filters: [WorkCondition]?

    init(
        statuses: [WorkStatus]? = nil, direction: WorkDirection? = nil, ownerPersonIds: [String]? = nil,
        projectIds: [String]? = nil, labelIds: [String]? = nil, priorities: [Int]? = nil,
        dueBeforeMs: Int64? = nil, due: [WorkDue]? = nil, sourceMeetingId: String? = nil, search: String? = nil,
        includeArchived: Bool = false, includeSnoozed: Bool = false, cycle: WorkCycleRef? = nil,
        filters: [WorkCondition]? = nil
    ) {
        self.statuses = statuses
        self.direction = direction
        self.ownerPersonIds = ownerPersonIds
        self.projectIds = projectIds
        self.labelIds = labelIds
        self.priorities = priorities
        self.dueBeforeMs = dueBeforeMs
        self.due = due
        self.sourceMeetingId = sourceMeetingId
        self.search = search
        self.includeArchived = includeArchived
        self.includeSnoozed = includeSnoozed
        self.cycle = cycle
        self.filters = filters
    }
}

enum WorkConditionField: String, Codable, CaseIterable, Hashable, Identifiable {
    case status, priority, owner, project, milestone, label, cycle, due, created, updated

    var id: Self { self }
    var label: String { rawValue.prefix(1).uppercased() + rawValue.dropFirst() }
    var dated: Bool { self == .due || self == .created || self == .updated }
    var operators: [WorkConditionOp] {
        dated ? [.before, .after, .is, .isNot]
            : self == .label ? [.includesAll, .includesAny, .noneOf]
            : [.is, .isNot, .anyOf, .noneOf]
    }
}

enum WorkConditionOp: String, Codable, CaseIterable, Hashable, Identifiable {
    case `is`, isNot = "is_not", anyOf = "any_of", noneOf = "none_of"
    case includesAll = "includes_all", includesAny = "includes_any", before, after

    var id: Self { self }
    var label: String {
        switch self {
        case .is: "is"
        case .isNot: "is not"
        case .anyOf: "is any of"
        case .noneOf: "is none of"
        case .includesAll: "includes all of"
        case .includesAny: "includes any of"
        case .before: "before"
        case .after: "after"
        }
    }
}

struct WorkCondition: Codable, Equatable, Hashable, Identifiable {
    var field: WorkConditionField
    var op: WorkConditionOp
    var values: [String]

    var id: String { "\(field.rawValue) \(op.rawValue) \(values.joined(separator: ","))" }
}

enum WorkLayout: String, Codable, Hashable { case list, board }
enum WorkGroupBy: String, Codable, CaseIterable, Hashable { case status, owner, project, meeting, due, priority, none }
enum WorkOrderBy: String, Codable, CaseIterable, Hashable { case priority, due, updated, created, manual }

struct WorkDisplay: Codable, Equatable, Hashable {
    var layout: WorkLayout
    var groupBy: WorkGroupBy
    var orderBy: WorkOrderBy
    var showEmptyGroups: Bool
    var properties: [String]

    init(
        layout: WorkLayout = .list, groupBy: WorkGroupBy = .status, orderBy: WorkOrderBy = .priority,
        showEmptyGroups: Bool = false, properties: [String] = []
    ) {
        self.layout = layout
        self.groupBy = groupBy
        self.orderBy = orderBy
        self.showEmptyGroups = showEmptyGroups
        self.properties = properties
    }
}

struct WorkView: Codable, Identifiable, Equatable {
    var id: String
    var name: String
    var query: WorkQuery
    var display: WorkDisplay
    var revision: Int64
}

struct WorkLabel: Codable, Identifiable, Equatable {
    var id: String
    var name: String
    /// Degrees on the color wheel, 0 to 360.
    var hue: Double
    var revision: Int64
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
    case attachment
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
}

enum WorkRelationKind: String, Codable, Hashable { case blocks, related }

struct WorkRelation: Codable, Identifiable, Equatable {
    var id: String
    var fromId: String
    var toId: String
    var kind: WorkRelationKind
}

enum WorkSuggestionField: String, Codable, Hashable { case owner, due, project, duplicate }

enum WorkSuggestionValue: Equatable {
    case owner(WorkOwner)
    case due(Int64)
    case project(String)
    case item(String)
}

struct WorkFieldSuggestion: Equatable {
    var field: WorkSuggestionField
    var value: WorkSuggestionValue
    var reason: String
    var quote: String?
    /// Offset into the meeting the suggestion was read from.
    var atMs: Int64?
    var meetingId: String?
}

extension WorkFieldSuggestion: Decodable {
    private enum Key: String, CodingKey { case field, value, reason, quote, atMs, meetingId }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        field = try container.decode(WorkSuggestionField.self, forKey: .field)
        switch field {
        case .owner: value = .owner(try container.decode(WorkOwner.self, forKey: .value))
        case .due: value = .due(try container.decode(Int64.self, forKey: .value))
        case .project: value = .project(try container.decode(String.self, forKey: .value))
        case .duplicate: value = .item(try container.decode(String.self, forKey: .value))
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

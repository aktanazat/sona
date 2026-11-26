import Foundation

/// What a list filters, groups and orders by, and the saved views that keep
/// them, as `src-tauri/src/planning/work/types.rs` sends them.

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
    /// Conditions, and groups of them where any or all must hold, that all
    /// must hold on top of the fields above.
    var filters: [WorkFilter]?

    init(
        statuses: [WorkStatus]? = nil, direction: WorkDirection? = nil, ownerPersonIds: [String]? = nil,
        projectIds: [String]? = nil, labelIds: [String]? = nil, priorities: [Int]? = nil,
        dueBeforeMs: Int64? = nil, due: [WorkDue]? = nil, sourceMeetingId: String? = nil, search: String? = nil,
        includeArchived: Bool = false, includeSnoozed: Bool = false, cycle: WorkCycleRef? = nil,
        filters: [WorkFilter]? = nil
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
    case status, priority, owner, delegate, project, milestone, label, parent, cycle
    case cycleScope = "cycle_scope"
    case projectLabel = "project_label"
    case due, created, updated, estimate
    case forPerson = "for"
    case completed, `repeat`

    var id: Self { self }
    var label: String {
        switch self {
        case .delegate: "Agent"
        case .cycleScope: "Cycle scope"
        case .forPerson: "For"
        case .repeat: "Repeats"
        case .projectLabel: "Project label"
        default: rawValue.prefix(1).uppercased() + rawValue.dropFirst()
        }
    }
    var dated: Bool { self == .due || self == .created || self == .updated || self == .completed }
    var operators: [WorkConditionOp] {
        dated ? [.before, .after, .is, .isNot]
            : self == .label || self == .projectLabel ? [.includesAll, .includesAny, .noneOf]
            : self == .cycleScope ? [.is, .isNot]
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

/// A condition, or a group of filters of which any or all must hold. The
/// core sends a group as `{"any": [...]}` or `{"all": [...]}`.
enum WorkFilter: Codable, Hashable, Sendable {
    case condition(WorkCondition)
    case any([WorkFilter])
    case all([WorkFilter])

    private enum Key: String, CodingKey { case any, all }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        if let any = try container.decodeIfPresent([WorkFilter].self, forKey: .any) {
            self = .any(any)
        } else if let all = try container.decodeIfPresent([WorkFilter].self, forKey: .all) {
            self = .all(all)
        } else {
            self = .condition(try WorkCondition(from: decoder))
        }
    }

    func encode(to encoder: Encoder) throws {
        switch self {
        case let .condition(condition):
            try condition.encode(to: encoder)
        case let .any(filters):
            var container = encoder.container(keyedBy: Key.self)
            try container.encode(filters, forKey: .any)
        case let .all(filters):
            var container = encoder.container(keyedBy: Key.self)
            try container.encode(filters, forKey: .all)
        }
    }

    var condition: WorkCondition? {
        if case let .condition(condition) = self { return condition }
        return nil
    }

    /// The filters a group holds; nil for a condition.
    var members: [WorkFilter]? {
        switch self {
        case .condition: nil
        case let .any(filters), let .all(filters): filters
        }
    }

    /// The same kind of group holding `filters`; a condition stays itself.
    func with(_ filters: [WorkFilter]) -> WorkFilter {
        switch self {
        case .condition: self
        case .any: .any(filters)
        case .all: .all(filters)
        }
    }
}

enum WorkLayout: String, Codable, Hashable { case list, board }
enum WorkGroupBy: String, Codable, CaseIterable, Hashable {
    case status, owner, project, meeting, due, priority, cycle, label, milestone, none
}
enum WorkOrderBy: String, Codable, CaseIterable, Hashable { case priority, due, updated, created, status, manual }
enum WorkOrderDirection: String, Codable, Hashable { case ascending, descending }

extension WorkOrderBy {
    /// The way an order runs unless a list says otherwise: urgent first,
    /// soonest due first, newest first, work under way first.
    var natural: WorkOrderDirection {
        switch self {
        case .priority, .updated, .created: .descending
        case .due, .status, .manual: .ascending
        }
    }
}

struct WorkDisplay: Codable, Equatable, Hashable {
    var layout: WorkLayout
    var groupBy: WorkGroupBy
    var orderBy: WorkOrderBy
    var showEmptyGroups: Bool
    var properties: [String]
    var showSubTasks: Bool
    /// The board's swimlanes, or the list's sub-groups; `.none` for neither.
    var subGroupBy: WorkGroupBy
    /// Board columns left out, by `TaskGroup.id`.
    var hiddenColumns: [String]
    /// Which way the order runs; nil for the order's natural way.
    var orderDirection: WorkOrderDirection?
    /// Ordered by priority, work with no priority comes first, not last.
    var noPriorityFirst: Bool
    /// Sub-tasks sit under their parent in a list, not in rows of their own.
    var nestSubTasks: Bool

    init(
        layout: WorkLayout = .list, groupBy: WorkGroupBy = .status, orderBy: WorkOrderBy = .priority,
        showEmptyGroups: Bool = false, properties: [String] = [], showSubTasks: Bool = true,
        subGroupBy: WorkGroupBy = .none, hiddenColumns: [String] = [], orderDirection: WorkOrderDirection? = nil,
        noPriorityFirst: Bool = false, nestSubTasks: Bool = false
    ) {
        self.layout = layout
        self.groupBy = groupBy
        self.orderBy = orderBy
        self.showEmptyGroups = showEmptyGroups
        self.properties = properties
        self.showSubTasks = showSubTasks
        self.subGroupBy = subGroupBy
        self.hiddenColumns = hiddenColumns
        self.orderDirection = orderDirection
        self.noPriorityFirst = noPriorityFirst
        self.nestSubTasks = nestSubTasks
    }

    /// Which way the order runs.
    var direction: WorkOrderDirection { orderDirection ?? orderBy.natural }
}

/// Where tasks go in the manual order.
enum WorkPlace: Encodable, Equatable {
    case top, bottom, before(String), after(String)

    private enum Key: String, CodingKey { case at, id }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        switch self {
        case .top: try container.encode("top", forKey: .at)
        case .bottom: try container.encode("bottom", forKey: .at)
        case let .before(id):
            try container.encode("before", forKey: .at)
            try container.encode(id, forKey: .id)
        case let .after(id):
            try container.encode("after", forKey: .at)
            try container.encode(id, forKey: .id)
        }
    }
}

struct WorkView: Codable, Identifiable, Equatable {
    var id: String
    var name: String
    var query: WorkQuery
    var display: WorkDisplay
    var revision: Int64
    /// An SF Symbol shown beside the view's name.
    var icon: String? = nil
    /// The project whose page shows the view; nil for the Tasks page.
    var projectId: String? = nil

    /// The link that opens a view, as `sona work views` prints it.
    static func link(_ id: String) -> String { "sona://tasks?view=\(id)" }
}

/// The project fields a Projects page view keeps; nil keeps them all.
struct ProjectViewFilter: Codable, Equatable, Hashable {
    var status: String?
    var overdueOnly = false
    var mineOnly = false
    var updateBefore: ProjectUpdateBefore?
    var link: String?
    /// An initiative's id, or `none` for projects in no initiative.
    var initiative: String?
    var health: ProjectListHealth?
    /// A label's id: only projects with this label.
    var label: String?
}

/// How long ago a project's latest update has to be, at least.
struct ProjectUpdateBefore: Codable, Equatable, Hashable {
    var kind: String
    var day: String?
}

enum ProjectViewLayout: String, Codable, Hashable { case list, board, timeline }
enum ProjectGroupBy: String, Codable, CaseIterable, Hashable {
    case status, lead, priority, quarter, health, initiative, label, none
}

/// A project's health as a list reads it: outdated when there is no latest
/// update or it is older than the project's update cadence.
enum ProjectListHealth: String, Codable, CaseIterable, Hashable {
    case onTrack = "on_track"
    case atRisk = "at_risk"
    case offTrack = "off_track"
    case outdated
}

/// The order a project list sorts by; manual is the order set by dragging.
enum ProjectOrderBy: String, Codable, CaseIterable, Hashable {
    case manual, status, priority, name, start, target, created, updated
}

struct ProjectViewDisplay: Codable, Equatable, Hashable {
    var layout: ProjectViewLayout = .list
    var groupBy: ProjectGroupBy = .status
    /// Nil keeps the list in the order it was loaded in.
    var orderBy: ProjectOrderBy?
    var reverse = false
}

/// A saved view of the Projects page.
struct ProjectView: Codable, Identifiable, Equatable {
    var id: String
    var name: String
    var icon: String?
    var filter: ProjectViewFilter
    var display: ProjectViewDisplay
    var revision: Int64

    /// The link that opens a Projects page view.
    static func link(_ id: String) -> String { "sona://projects?view=\(id)" }
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

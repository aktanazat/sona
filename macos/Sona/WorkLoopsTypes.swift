import Foundation

protocol WorkLoopValue: RawRepresentable, Codable, Hashable where RawValue == String {
    static var unknown: Self { get }
}

extension WorkLoopValue {
    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = Self(rawValue: raw) ?? .unknown
    }
}

enum WorkLoopTaskStatus: String, WorkLoopValue, CaseIterable, Identifiable {
    case triage, backlog
    case notStarted = "not_started"
    case inProgress = "in_progress"
    case done, canceled, duplicate
    case unknown

    static let choices: [Self] = allCases.filter { $0 != .unknown }

    var id: String { rawValue }

    var label: String { WorkStatus(rawValue: rawValue)?.label ?? "Unknown status" }
}

enum WorkLoopProjectStatus: String, WorkLoopValue, CaseIterable, Identifiable {
    case backlog, planned, active, paused, completed, canceled
    case unknown

    static let choices: [Self] = allCases.filter { $0 != .unknown }

    var id: String { rawValue }

    var label: String { PlanningProjectStatus(rawValue: rawValue)?.label ?? "Unknown status" }
}

enum WorkLoopWeekday: String, WorkLoopValue, CaseIterable, Identifiable {
    case mon, tue, wed, thu, fri, sat, sun
    case unknown

    static let week: [Self] = allCases.filter { $0 != .unknown }

    var id: String { rawValue }

    var short: String {
        switch self {
        case .mon: "Mon"
        case .tue: "Tue"
        case .wed: "Wed"
        case .thu: "Thu"
        case .fri: "Fri"
        case .sat: "Sat"
        case .sun: "Sun"
        case .unknown: "?"
        }
    }

    init(_ date: Date, in zone: TimeZone) {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = zone
        let index = (calendar.component(.weekday, from: date) + 5) % 7
        self = Self.week[index]
    }
}

enum WorkLoopEvery: String, WorkLoopValue, CaseIterable, Identifiable {
    case hourly, daily, weekly, monthly
    case unknown

    static let choices: [Self] = allCases.filter { $0 != .unknown }

    var id: String { rawValue }

    var label: String {
        switch self {
        case .hourly: "Hour"
        case .daily: "Day"
        case .weekly: "Week"
        case .monthly: "Month"
        case .unknown: "Unknown"
        }
    }
}

enum WorkLoopRunStatus: String, WorkLoopValue {
    case queued, running, done, failed, skipped
    case unknown

    var label: String {
        switch self {
        case .queued: "Queued"
        case .running: "Running"
        case .done: "Done"
        case .failed: "Failed"
        case .skipped: "Skipped"
        case .unknown: "Unknown"
        }
    }

    var active: Bool { self == .queued || self == .running }
}

struct WorkLoopSchedule: Hashable {
    var every: WorkLoopEvery
    var days: [WorkLoopWeekday]
    var timezone: String
    var firstRunAtMs: Int64
}

enum WorkLoopTrigger: Codable, Hashable {
    case schedule(WorkLoopSchedule)
    case taskCreated
    case statusChanged(to: WorkLoopTaskStatus?)
    case labelAdded(labelId: String?)
    case assigned
    case commentAdded
    case projectStatusChanged(to: WorkLoopProjectStatus?)
    case meetingEnded
    case unknown(String)

    private enum Key: String, CodingKey { case kind, every, days, timezone, firstRunAtMs, to, labelId }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let kind = try container.decode(String.self, forKey: .kind)
        switch kind {
        case "schedule":
            self = .schedule(WorkLoopSchedule(
                every: try container.decode(WorkLoopEvery.self, forKey: .every),
                days: try container.decodeIfPresent([WorkLoopWeekday].self, forKey: .days) ?? [],
                timezone: try container.decode(String.self, forKey: .timezone),
                firstRunAtMs: try container.decode(Int64.self, forKey: .firstRunAtMs)))
        case "task_created": self = .taskCreated
        case "status_changed": self = .statusChanged(to: try container.decodeIfPresent(WorkLoopTaskStatus.self, forKey: .to))
        case "label_added": self = .labelAdded(labelId: try container.decodeIfPresent(String.self, forKey: .labelId))
        case "assigned": self = .assigned
        case "comment_added": self = .commentAdded
        case "project_status_changed":
            self = .projectStatusChanged(to: try container.decodeIfPresent(WorkLoopProjectStatus.self, forKey: .to))
        case "meeting_ended": self = .meetingEnded
        default: self = .unknown(kind)
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        switch self {
        case let .schedule(schedule):
            try container.encode("schedule", forKey: .kind)
            try container.encode(schedule.every, forKey: .every)
            try container.encode(schedule.every == .weekly ? schedule.days : [], forKey: .days)
            try container.encode(schedule.timezone, forKey: .timezone)
            try container.encode(schedule.firstRunAtMs, forKey: .firstRunAtMs)
        case .taskCreated: try container.encode("task_created", forKey: .kind)
        case let .statusChanged(to):
            try container.encode("status_changed", forKey: .kind)
            try container.encode(to, forKey: .to)
        case let .labelAdded(labelId):
            try container.encode("label_added", forKey: .kind)
            try container.encode(labelId, forKey: .labelId)
        case .assigned: try container.encode("assigned", forKey: .kind)
        case .commentAdded: try container.encode("comment_added", forKey: .kind)
        case let .projectStatusChanged(to):
            try container.encode("project_status_changed", forKey: .kind)
            try container.encode(to, forKey: .to)
        case .meetingEnded: try container.encode("meeting_ended", forKey: .kind)
        case let .unknown(kind): try container.encode(kind, forKey: .kind)
        }
    }
}

struct WorkLoopConditions: Codable, Hashable {
    var labels: [String] = []
    var projects: [String] = []
    var statuses: [WorkLoopTaskStatus] = []
}

struct WorkLoopTool: Codable, Hashable, Identifiable {
    var name: String
    var title: String

    var id: String { name }
}

struct WorkLoopRun: Codable, Identifiable, Equatable {
    var id: String
    var automationId: String
    var version: Int
    var cause: String
    var taskId: String?
    var meetingId: String?
    var status: WorkLoopRunStatus
    var queuedAtMs: Int64
    var startedAtMs: Int64?
    var endedAtMs: Int64?
    var output: String?
    var error: String?
    var refusedTools: [String]
    var sessionId: String?
}

struct WorkLoop: Codable, Identifiable, Equatable {
    var id: String
    var name: String
    var prompt: String
    var enabled: Bool
    var trigger: WorkLoopTrigger
    var conditions: WorkLoopConditions
    var tools: [String]
    var version: Int
    var nextRunAtMs: Int64?
    var lastRun: WorkLoopRun?
    var createdAtMs: Int64
    var updatedAtMs: Int64
}

struct WorkLoopVersion: Codable, Identifiable, Equatable {
    var version: Int
    var name: String
    var prompt: String
    var trigger: WorkLoopTrigger
    var conditions: WorkLoopConditions
    var tools: [String]
    var savedAtMs: Int64
    var savedBy: String
    var restoredFrom: Int?

    var id: Int { version }
}

struct WorkLoopInput: Encodable, Equatable {
    var name: String
    var prompt: String
    var trigger: WorkLoopTrigger
    var conditions: WorkLoopConditions
    var tools: [String]
}

struct WorkLoopList: Decodable, Equatable {
    var loops: [WorkLoop]
    var tools: [WorkLoopTool]
}

struct WorkLoopDetail: Decodable, Equatable {
    var loop: WorkLoop
    var versions: [WorkLoopVersion]
    var runs: [WorkLoopRun]

    private enum CodingKeys: String, CodingKey {
        case loop = "automation", versions, runs
    }
}

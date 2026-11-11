import Foundation
import Observation

enum PlanningProjectStatus: String, Codable, CaseIterable, Identifiable {
    case backlog, planned, active, paused, completed, canceled
    var id: String { rawValue }

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = Self(rawValue: raw) ?? .backlog
    }
    var label: String {
        switch self {
        case .backlog: "Backlog"
        case .planned: "Planned"
        case .active: "Active"
        case .paused: "Paused"
        case .completed: "Completed"
        case .canceled: "Canceled"
        }
    }
}

enum PlanningDateResolution: String, Codable, CaseIterable, Identifiable {
    case day, month, quarter, halfYear, year
    var id: String { rawValue }
    var label: String {
        switch self {
        case .day: "Day"
        case .month: "Month"
        case .quarter: "Quarter"
        case .halfYear: "Half year"
        case .year: "Year"
        }
    }
}

enum PlanningProjectHealth: String, Codable, CaseIterable, Identifiable {
    case onTrack, atRisk, offTrack
    var id: String { rawValue }
    var label: String {
        switch self {
        case .onTrack: "On track"
        case .atRisk: "At risk"
        case .offTrack: "Off track"
        }
    }
}

struct PlanningProjectLink: Codable, Equatable, Identifiable {
    var id: String
    var title: String?
    var url: String
}

struct PlanningProjectUpdateAuthor: Decodable, Equatable {
    let kind: WorkActor
    let name: String?
}

struct PlanningProjectUpdate: Decodable, Identifiable, Equatable {
    let id: String
    let projectId: String
    let health: PlanningProjectHealth
    let body: String?
    let progress: Int
    let author: PlanningProjectUpdateAuthor
    let createdAtMs: Int64
    let editedAtMs: Int64?
    let deleted: Bool
}

struct PlanningProjectUpdatePage: Decodable {
    let updates: [PlanningProjectUpdate]
    let nextCursor: String?
}

struct PlanningProjectWeek: Decodable, Equatable {
    let weekStartMs: Int64
    let scope: Int
    let started: Int
    let done: Int
}

struct PlanningProjectPrediction: Decodable, Equatable {
    let expectedAtMs: Int64
    let optimisticAtMs: Int64
    let pessimisticAtMs: Int64
}

struct PlanningProjectProgress: Decodable, Equatable {
    let scope: Int
    let started: Int
    let done: Int
    let percent: Int
    let weeks: [PlanningProjectWeek]
    let prediction: PlanningProjectPrediction?
}

struct PlanningMilestoneProgress: Decodable, Equatable {
    let milestoneId: String
    let scope: Int
    let started: Int
    let done: Int
    let percent: Int
    let nextOpen: Bool
}

enum PlanningWorkStatus: String, Codable, CaseIterable, Identifiable {
    case notStarted = "not_started"
    case inProgress = "in_progress"
    case done, canceled
    var id: String { rawValue }
    var label: String {
        switch self {
        case .notStarted: "Not started"
        case .inProgress: "In progress"
        case .done: "Done"
        case .canceled: "Canceled"
        }
    }
    var isOpen: Bool { self == .notStarted || self == .inProgress }
}

struct PlanningProject: Decodable, Identifiable, Equatable {
    let id: String
    let title: String
    let description: String?
    var status: PlanningProjectStatus
    let revision: Int64
    let createdAtMs: Int64
    var updatedAtMs: Int64
    var leadPersonId: String? = nil
    var leadIsSelf: Bool? = nil
    var memberPersonIds: [String]? = nil
    var membersIncludeSelf: Bool? = nil
    var targetAtMs: Int64? = nil
    var hue: Int? = nil
    var archivedAtMs: Int64? = nil
    var priority: Int? = nil
    var startAtMs: Int64? = nil
    var startResolution: PlanningDateResolution? = nil
    var targetResolution: PlanningDateResolution? = nil
    var icon: String? = nil
    var labelIds: [String]? = nil
    var links: [PlanningProjectLink]? = nil
    var trashedAtMs: Int64? = nil
    var summary: String? = nil
    var statusNameId: String? = nil
}

/// The owner's own name for a status, kept inside one of the six categories.
struct PlanningProjectStatusName: Decodable, Identifiable, Equatable {
    let id: String
    let category: PlanningProjectStatus
    let name: String
    let position: Int64
}

/// A project kept to start new ones from: its milestones and tasks, by title.
struct PlanningProjectTemplate: Decodable, Identifiable, Equatable {
    let id: String
    let name: String
    let fields: Fields

    struct Fields: Decodable, Equatable {
        let milestones: [Part]
        let tasks: [Part]
    }

    struct Part: Decodable, Equatable {
        let title: String
    }
}

/// One project waiting on another to end before it starts. `broken` when
/// the blocker is open and ends after the blocked project starts.
struct PlanningProjectDependency: Decodable, Identifiable, Equatable {
    let id: String
    let blockerId: String
    let blockedId: String
    let createdAtMs: Int64
    let broken: Bool

    /// The core's rule, for dates still being dragged: an open blocker whose
    /// end falls on a later local day than the blocked project's start.
    static func breaks(blockerStatus: PlanningProjectStatus, blockerEnd: Date?, blockedStart: Date?) -> Bool {
        guard blockerStatus != .completed, blockerStatus != .canceled,
              let blockerEnd, let blockedStart else { return false }
        let calendar = Calendar.current
        return calendar.startOfDay(for: blockerEnd) > calendar.startOfDay(for: blockedStart)
    }
}

/// One change to a project's properties or milestones. Days are epoch
/// milliseconds and people are ids or `me`, written as text; `subject` is
/// the milestone's title for the milestone kinds.
struct PlanningProjectActivity: Decodable, Identifiable, Equatable {
    enum Kind: String, Decodable {
        case title, status, statusName, lead, members, startDate, targetDate, priority
        case milestoneAdded, milestoneRemoved, milestoneTarget
        case unknown

        init(from decoder: Decoder) throws {
            self = Self(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
        }
    }

    let id: String
    let projectId: String
    let kind: Kind
    let subject: String?
    let fromValue: String?
    let toValue: String?
    let atMs: Int64
}

struct PlanningMilestone: Decodable, Identifiable, Equatable {
    let id: String
    let projectId: String
    let title: String
    let dueAtMs: Int64?
    let status: PlanningWorkStatus
    let revision: Int64
    let createdAtMs: Int64
    let updatedAtMs: Int64
    var position: Int64? = nil
    var description: String? = nil
}

struct PlanningTask: Decodable, Identifiable, Equatable {
    let id: String
    let projectId: String
    let milestoneId: String?
    let title: String
    let details: String?
    let dueAtMs: Int64?
    let ownerPersonId: String?
    let ownerIsSelf: Bool
    let ownerAvailable: Bool
    let status: PlanningWorkStatus
    let revision: Int64
    let createdAtMs: Int64
    let updatedAtMs: Int64
}

enum PlanningTargetKind: String, Codable, Hashable { case project, milestone, task }
enum PlanningSourceKind: String, Codable, Hashable { case meeting, note, loop, dictation, link }

struct PlanningEvidenceLink: Decodable, Identifiable, Equatable {
    let id: String
    let targetKind: PlanningTargetKind
    let targetId: String
    let sourceKind: PlanningSourceKind
    let sourceId: String
    let atMs: Int64?
    let meetingNoteId: String?
    let revision: Int64
    let createdAtMs: Int64
    let available: Bool
    let title: String?
    /// A dictation's span ends here; `atMs` is where it starts.
    let endMs: Int64?
    /// The words said, for a dictation; the page's title, for a link.
    let caption: String?
}

struct PlanningProjectSummary: Decodable, Identifiable, Equatable {
    let project: PlanningProject
    let openTaskCount: Int64
    let completedTaskCount: Int64
    let nextDueAtMs: Int64?
    var latestUpdateAtMs: Int64? = nil
    var id: String { project.id }
}

struct PlanningOwnerCount: Decodable, Equatable {
    let personId: String
    let openTasks: Int
}

struct PlanningMilestonePulse: Decodable, Equatable {
    let milestoneId: String
    let meetings: Int
    let lastMeetingAtMs: Int64
}

/// A project at a glance as of the moment its page was read.
struct PlanningProjectPulse: Decodable, Equatable {
    let done: Int
    let open: Int
    let overdue: Int
    let nextDueAtMs: Int64?
    let meetingsThisWeek: Int
    let owners: [PlanningOwnerCount]
    let milestones: [PlanningMilestonePulse]
}

struct PlanningProjectDetail: Decodable, Equatable {
    var project: PlanningProject
    var milestones: [PlanningMilestone]
    var tasks: [PlanningTask]
    let evidence: [PlanningEvidenceLink]
    let pulse: PlanningProjectPulse
    var latestUpdate: PlanningProjectUpdate? = nil
    var updateMissing = false
    var progress: PlanningProjectProgress? = nil
    var milestoneProgress: [PlanningMilestoneProgress] = []
}

/// What the projects list is narrowed to.
struct PlanningListFilter: Equatable {
    var status: PlanningProjectStatus?
    var overdueOnly = false
    var mineOnly = false
    var updateBefore: PlanningUpdateBefore?
    var link: PlanningLinkFilter?
    var initiative: PlanningInitiativeFilter?

    var isActive: Bool {
        status != nil || overdueOnly || mineOnly || updateBefore != nil || link != nil || initiative != nil
    }
}

/// Which initiative a listed project has to sit in.
enum PlanningInitiativeFilter: Hashable {
    case outside
    case within(String)

    func keeps(_ initiativeId: String?) -> Bool {
        switch self {
        case .outside: initiativeId == nil
        case .within(let id): initiativeId == id
        }
    }
}

/// Which side of a dependency a listed project has to be on.
enum PlanningLinkFilter: String, CaseIterable, Identifiable {
    case blocked, blocking, broken

    var id: String { rawValue }

    var label: String {
        switch self {
        case .blocked: "Blocked"
        case .blocking: "Blocking"
        case .broken: "Broken dependency"
        }
    }

    func keeps(_ projectId: String, _ dependencies: [PlanningProjectDependency]) -> Bool {
        dependencies.contains { dependency in
            switch self {
            case .blocked: dependency.blockedId == projectId
            case .blocking: dependency.blockerId == projectId
            case .broken: dependency.broken && (dependency.blockedId == projectId || dependency.blockerId == projectId)
            }
        }
    }
}

/// How long ago a project's latest update was posted, for the list to keep
/// it: a span back from today or a picked day. A project with no update
/// always counts as before.
enum PlanningUpdateBefore: Hashable {
    case oneWeek, twoWeeks, oneMonth
    case day(Date)

    static let spans: [PlanningUpdateBefore] = [.oneWeek, .twoWeeks, .oneMonth]

    var label: String {
        switch self {
        case .oneWeek: "1 week ago"
        case .twoWeeks: "2 weeks ago"
        case .oneMonth: "1 month ago"
        case let .day(day): PlanningFormat.day(ProjectsStore.milliseconds(day))
        }
    }

    /// The start of the day an update must come before, the way `work
    /// projects --update-before` reads its day.
    func cutoffMs(now: Date) -> Int64 {
        let calendar = Calendar.current
        let today = calendar.startOfDay(for: now)
        let day: Date? = switch self {
        case .oneWeek: calendar.date(byAdding: .day, value: -7, to: today)
        case .twoWeeks: calendar.date(byAdding: .day, value: -14, to: today)
        case .oneMonth: calendar.date(byAdding: .month, value: -1, to: today)
        case let .day(day): calendar.startOfDay(for: day)
        }
        return ProjectsStore.milliseconds(day ?? today)
    }
}

extension PlanningProject {
    func with(status: PlanningProjectStatus, at now: Int64) -> PlanningProject {
        var changed = self
        changed.status = status
        changed.updatedAtMs = now
        return changed
    }
}

/// Work is overdue once its due day has ended, the rule the project pulse counts by.
enum PlanningDue {
    static func isPast(_ due: Int64?, at now: Int64) -> Bool {
        guard let due else { return false }
        let today = Calendar.current.startOfDay(for: Date(timeIntervalSince1970: Double(now) / 1000))
        return Double(due) / 1000 < today.timeIntervalSince1970
    }
}

extension PlanningMilestone {
    func isOverdue(at now: Int64) -> Bool { status.isOpen && PlanningDue.isPast(dueAtMs, at: now) }

    func with(status: PlanningWorkStatus, at now: Int64) -> PlanningMilestone {
        PlanningMilestone(id: id, projectId: projectId, title: title, dueAtMs: dueAtMs, status: status,
                          revision: revision, createdAtMs: createdAtMs, updatedAtMs: now, position: position,
                          description: description)
    }
}

extension PlanningTask {
    func isOverdue(at now: Int64) -> Bool { status.isOpen && PlanningDue.isPast(dueAtMs, at: now) }
}

extension PlanningProjectDetail {
    func replacing(project: PlanningProject) -> PlanningProjectDetail {
        var changed = self
        changed.project = project
        return changed
    }

    func replacing(milestone: PlanningMilestone) -> PlanningProjectDetail {
        var changed = self
        changed.milestones = milestones.map { $0.id == milestone.id ? milestone : $0 }
        return changed
    }

    /// Milestones in the order the project keeps them: position, then due day.
    var orderedMilestones: [PlanningMilestone] {
        milestones.sorted {
            ($0.position ?? .max, $0.dueAtMs ?? .max, $0.createdAtMs)
                < ($1.position ?? .max, $1.dueAtMs ?? .max, $1.createdAtMs)
        }
    }

    func progress(of milestone: PlanningMilestone) -> PlanningMilestoneProgress? {
        milestoneProgress.first { $0.milestoneId == milestone.id }
    }

    func overdueTaskCount(at now: Int64) -> Int { tasks.filter { $0.isOverdue(at: now) }.count }

    var hasMyOpenWork: Bool { tasks.contains { $0.ownerIsSelf && $0.status.isOpen } }
}

struct PlanningCalendarItem: Decodable, Identifiable, Equatable {
    enum Kind: String, Decodable { case task, milestone, meeting }
    let id: String
    let kind: Kind
    let title: String
    let projectId: String?
    let dueAtMs: Int64
    let meetingId: String?
    var key: String { "\(kind.rawValue):\(id)" }
    var date: Date { Date(timeIntervalSince1970: Double(dueAtMs) / 1000) }
}

private struct PlanningCalendarPage: Decodable {
    let items: [PlanningCalendarItem]
    let conversationsAvailable: Bool
}

struct PlanningPersonChoice: Identifiable, Hashable {
    let id: String
    let name: String
}

enum PlanningOwner: Hashable {
    case unassigned
    case myself
    case person(String)
}

enum PlanningSourceList: String, CaseIterable, Identifiable {
    case meeting, note, loop
    var id: String { rawValue }
    var label: String {
        switch self {
        case .meeting: "Meetings"
        case .note: "Notes"
        case .loop: "Commitments"
        }
    }
}

struct PlanningSourceChoice: Identifiable, Hashable {
    let kind: PlanningSourceKind
    let sourceId: String
    let title: String
    let updatedAtMs: Int64?
    var id: String { "\(kind.rawValue):\(sourceId)" }
}

struct PlanningSourcePage {
    let entries: [PlanningSourceChoice]
    let nextCursor: Int64?
}

struct PlanningSuggestionSource: Codable, Equatable {
    let kind: PlanningSourceKind
    let id: String
}

struct QuietReviewState: Decodable, Equatable, Identifiable {
    let projectId: String
    let enabled: Bool
    let selectedSources: [PlanningSuggestionSource]
    let lastCompletedAtMs: Int64?
    let lastOutcome: String?
    var id: String { projectId }
}

private struct QuietReviewSetParams: Encodable {
    let projectId: String
    let enabled: Bool
    let selectedSources: [PlanningSuggestionSource]
}

private struct QuietReviewProjectParams: Encodable {
    let projectId: String
}

enum PlanningSuggestionKind: String, Decodable {
    case possibleLink = "possible_link"
    case possibleDuplicate = "possible_duplicate"
    case possibleConflict = "possible_conflict"
    case possibleChange = "possible_change"

    var label: String {
        switch self {
        case .possibleLink: "Possible link"
        case .possibleDuplicate: "Possible duplicate"
        case .possibleConflict: "Possible conflict"
        case .possibleChange: "Possible change"
        }
    }
}

enum PlanningSuggestionState: String, Decodable {
    case pending, accepted, declined
}

enum PlanningSuggestionAvailability: String, Decodable {
    case available, changed, unavailable
}

struct PlanningSuggestionEvidence: Decodable, Equatable {
    let title: String
    let passageId: String
    let text: String
}

struct PlanningSuggestion: Decodable, Identifiable, Equatable {
    let id: String
    let kind: PlanningSuggestionKind
    let state: PlanningSuggestionState
    let revision: Int64
    let targetKind: PlanningTargetKind
    let targetId: String
    let projectId: String?
    let targetTitle: String?
    let source: PlanningSuggestionSource
    let atMs: Int64?
    let createdAtMs: Int64
    let availability: PlanningSuggestionAvailability
    let evidence: PlanningSuggestionEvidence?

    var canLink: Bool {
        availability == .available && targetTitle != nil && evidence != nil
    }

    var sourceLink: PlanningEvidenceLink? {
        guard canLink, let evidence else { return nil }
        let meetingNoteId = source.kind == .meeting
            ? evidence.passageId.hasPrefix("note:") ? String(evidence.passageId.dropFirst(5)) : nil
            : nil
        return PlanningEvidenceLink(
            id: id, targetKind: targetKind, targetId: targetId,
            sourceKind: source.kind, sourceId: source.id, atMs: atMs, meetingNoteId: meetingNoteId,
            revision: revision, createdAtMs: createdAtMs, available: true, title: evidence.title,
            endMs: nil, caption: nil
        )
    }
}

private struct PlanningSuggestionDecision: Decodable {
    let id: String
    let state: PlanningSuggestionState
    let revision: Int64
    let projectId: String?
    let evidenceLinkId: String?
}

private struct PlanningReviewLinksParams: Encodable {
    let projectId: String
    let selectedSources: [PlanningSuggestionSource]
}

private struct PlanningSuggestionDecisionParams: Encodable {
    let id: String
    let expectedRevision: Int64
    let operationId: String
}

private struct PlanningMeetingSources: Decodable {
    struct Row: Decodable {
        let sessionId: String
        let title: String
        let createdAtUtcMs: Int64
    }
    let entries: [Row]
    let hasMore: Bool
}

private struct PlanningNoteSource: Decodable {
    let id: String
    let title: String
    let updatedAtMs: Int64
}

private struct PlanningLoopSources: Decodable {
    struct Row: Decodable {
        let loopId: String
        let text: String
    }
    let rows: [Row]
}

private struct PlanningSourceQueryParams: Encodable { let query: String }

private struct PlanningIdParams: Encodable { let id: String }
private struct PlanningProjectParams: Encodable {
    let title: String
    let description: String?
    let operationId: String
}
private struct PlanningMilestoneParams: Encodable {
    let projectId: String
    let title: String
    let dueAtMs: Int64?
    let operationId: String
}
private struct PlanningTaskParams: Encodable {
    let projectId: String
    let milestoneId: String?
    let title: String
    let details: String?
    let dueAtMs: Int64?
    let ownerPersonId: String?
    let ownerIsSelf: Bool
}
private struct PlanningCreateTaskParams: Encodable {
    let input: PlanningTaskParams
    let operationId: String
}
private struct PlanningStatusParams<Status: Encodable>: Encodable {
    let id: String
    let expectedRevision: Int64
    let status: Status
    let operationId: String
}
private struct PlanningCalendarParams: Encodable {
    let startMs: Int64
    let endMs: Int64
}
private struct PlanningLinkParams: Encodable {
    let targetKind: PlanningTargetKind
    let targetId: String
    let sourceKind: PlanningSourceKind
    let sourceId: String
    let atMs: Int64?
    let operationId: String
}

enum PlanningFieldChange<Value: Encodable & Equatable>: Equatable {
    case set(Value)
    case clear
}

struct PlanningProjectLinkInput: Encodable, Equatable {
    var title: String?
    var url: String
}

/// The fields of a project to change: a field left `nil` keeps its value,
/// `.clear` empties it, the way `planning_project_save` reads a missing key
/// and a `null`.
struct PlanningProjectSave: Encodable, Equatable {
    var title: String?
    var summary: PlanningFieldChange<String>?
    var description: PlanningFieldChange<String>?
    var status: PlanningProjectStatus?
    var statusNameId: PlanningFieldChange<String>?
    var priority: Int?
    var leadPersonId: PlanningFieldChange<String>?
    var leadIsSelf: Bool?
    var memberPersonIds: [String]?
    var membersIncludeSelf: Bool?
    var startAtMs: PlanningFieldChange<Int64>?
    var startResolution: PlanningFieldChange<PlanningDateResolution>?
    var targetAtMs: PlanningFieldChange<Int64>?
    var targetResolution: PlanningFieldChange<PlanningDateResolution>?
    var hue: Int?
    var icon: PlanningFieldChange<String>?
    var labelIds: [String]?
    var links: [PlanningProjectLinkInput]?
    var trashed: Bool?

    private enum Key: String, CodingKey {
        case title, summary, description, status, statusNameId, priority, leadPersonId, leadIsSelf
        case memberPersonIds, membersIncludeSelf
        case startAtMs, startResolution
        case targetAtMs, targetResolution, hue, icon, labelIds, links, trashed
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        try container.encodeIfPresent(title, forKey: .title)
        try Self.put(summary, .summary, &container)
        try Self.put(description, .description, &container)
        try container.encodeIfPresent(status, forKey: .status)
        try Self.put(statusNameId, .statusNameId, &container)
        try container.encodeIfPresent(priority, forKey: .priority)
        try Self.put(leadPersonId, .leadPersonId, &container)
        try container.encodeIfPresent(leadIsSelf, forKey: .leadIsSelf)
        try container.encodeIfPresent(memberPersonIds, forKey: .memberPersonIds)
        try container.encodeIfPresent(membersIncludeSelf, forKey: .membersIncludeSelf)
        try Self.put(startAtMs, .startAtMs, &container)
        try Self.put(startResolution, .startResolution, &container)
        try Self.put(targetAtMs, .targetAtMs, &container)
        try Self.put(targetResolution, .targetResolution, &container)
        try container.encodeIfPresent(hue, forKey: .hue)
        try Self.put(icon, .icon, &container)
        try container.encodeIfPresent(labelIds, forKey: .labelIds)
        try container.encodeIfPresent(links, forKey: .links)
        try container.encodeIfPresent(trashed, forKey: .trashed)
    }

    private static func put<Value>(_ change: PlanningFieldChange<Value>?, _ key: Key,
                                   _ container: inout KeyedEncodingContainer<Key>) throws {
        switch change {
        case nil: break
        case let .set(value)?: try container.encode(value, forKey: key)
        case .clear?: try container.encodeNil(forKey: key)
        }
    }
}

private struct PlanningProjectSaveParams: Encodable {
    let operationId: String
    let id: String?
    let input: PlanningProjectSave
}

private struct PlanningUpdatesParams: Encodable {
    let projectId: String
    let limit: Int
    let cursor: String?
}

private struct PlanningUpdateAddParams: Encodable {
    let operationId: String
    let projectId: String
    let health: PlanningProjectHealth
    let body: String
}

private struct PlanningOperationParams: Encodable {
    let operationId: String
    let id: String
}

private struct PlanningDependencyAddParams: Encodable {
    let operationId: String
    let blockerId: String
    let blockedId: String
}

private struct PlanningTemplateSaveParams: Encodable {
    let operationId: String
    let name: String
    let projectId: String
}

private struct PlanningTemplateDeleteParams: Encodable {
    let operationId: String
    let id: String
}

private struct PlanningFromTemplateParams: Encodable {
    let operationId: String
    let templateId: String
    let title: String
    let startAtMs: Int64
}

private struct PlanningFromTemplate: Decodable {
    let project: PlanningProject
}

private struct PlanningStatusNameSaveParams: Encodable {
    let operationId: String
    let id: String?
    let category: PlanningProjectStatus
    let name: String
}

private struct PlanningUpdateEditParams: Encodable {
    let operationId: String
    let id: String
    let health: PlanningProjectHealth
    let body: String
}

private struct PlanningProjectIdParams: Encodable { let projectId: String }

private struct PlanningReorderParams: Encodable {
    let operationId: String
    let projectId: String
    let milestoneIds: [String]
}

/// The fields of a milestone to change: a field left `nil` keeps its value,
/// an empty description clears it and `.clear` takes the due day away, the
/// way `planning_milestone_save` reads them.
struct PlanningMilestoneSave: Encodable, Equatable {
    var title: String?
    var description: String?
    var dueAtMs: PlanningFieldChange<Int64>?

    private enum Key: String, CodingKey { case title, description, dueAtMs }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        try container.encodeIfPresent(title, forKey: .title)
        try container.encodeIfPresent(description, forKey: .description)
        switch dueAtMs {
        case nil: break
        case let .set(value)?: try container.encode(value, forKey: .dueAtMs)
        case .clear?: try container.encodeNil(forKey: .dueAtMs)
        }
    }
}

private struct PlanningMilestoneSaveParams: Encodable {
    let operationId: String
    let projectId: String
    let id: String
    let input: PlanningMilestoneSave
}

private struct PlanningChange: Decodable {
    let projectId: String
}

@MainActor
@Observable
final class ProjectsStore {
    private(set) var projects: [PlanningProjectSummary]?
    private(set) var listLoading = false
    private(set) var listError: String?
    private(set) var selectedProjectId: String?
    private(set) var details: [String: PlanningProjectDetail] = [:]
    private(set) var detailLoading = false
    private(set) var detailError: String?
    private(set) var calendarItems: [PlanningCalendarItem]?
    private(set) var calendarLoading = false
    private(set) var calendarError: String?
    private(set) var mutating = false
    private(set) var mutationError: String?
    private(set) var sourcesRevision = 0
    private(set) var suggestions: [PlanningSuggestion]?
    private(set) var suggestionsLoading = false
    private(set) var suggestionsGenerating = false
    private(set) var suggestionsError: String?
    private(set) var suggestionDecisionError: String?
    private(set) var suggestionActionId: String?
    private(set) var quietReviewStates: [QuietReviewState]?
    private(set) var quietReviewLoadError: String?
    private(set) var quietReviewSavingProjectId: String?
    private(set) var quietReviewRunningProjectId: String?
    private(set) var quietReviewStatus: [String: String] = [:]
    private(set) var quietReviewRevocationsPending: Set<String> = []
    private(set) var trashed: [PlanningProjectSummary]?
    private(set) var trashedError: String?
    /// The project just put in the bin, while its Undo is offered.
    private(set) var binned: PlanningProject?
    private(set) var updates: [String: [PlanningProjectUpdate]] = [:]
    private(set) var updatesCursor: [String: String] = [:]
    private(set) var updatesError: String?
    /// The owner's names for statuses, by category then place.
    private(set) var statusNames: [PlanningProjectStatusName] = []
    private(set) var statusNamesError: String?
    /// Projects waiting on other projects, oldest first.
    private(set) var dependencies: [PlanningProjectDependency] = []
    /// Initiatives, goals above projects, by name.
    private(set) var initiatives: [PlanningInitiative] = []
    /// The initiative whose page shows on Initiatives; `nil` shows the list.
    var selectedInitiativeId: String?
    /// Project templates, by name.
    private(set) var projectTemplates: [PlanningProjectTemplate] = []
    /// The property changes of each project whose updates were read, newest first.
    private(set) var history: [String: [PlanningProjectActivity]] = [:]
    private(set) var historyError: String?
    /// The comment threads on projects and milestones, among the others.
    let comments: CommentThreads
    /// What the list is narrowed to. Home sets it when it opens late work.
    var filter = PlanningListFilter()

    var detail: PlanningProjectDetail? { selectedProjectId.flatMap { details[$0] } }

    @ObservationIgnored private let core: Core
    @ObservationIgnored private var listSequence = 0
    @ObservationIgnored private var detailStamp = 0
    @ObservationIgnored private var detailStamps: [String: Int] = [:]
    @ObservationIgnored private var calendarSequence = 0
    @ObservationIgnored private var calendarRange: DateInterval?
    @ObservationIgnored private var statusOperations: [String: String] = [:]
    @ObservationIgnored private var suggestionSequence = 0
    @ObservationIgnored private var reviewingLinks = false
    @ObservationIgnored private var suggestionOperations: [String: String] = [:]
    @ObservationIgnored private var quietReviewSequence = 0
    @ObservationIgnored private var quietReviewRunRevision = 0

    init(core: Core, comments: CommentThreads) {
        self.core = core
        self.comments = comments
        let sourceEvents = [
            CoreEvent.meetingSessionChanged, CoreEvent.meetingTranscriptChanged, CoreEvent.meetingNoteChanged,
            CoreEvent.meetingArtifactChanged, CoreEvent.meetingRemoteJobChanged, CoreEvent.meetingRemoved,
            CoreEvent.meetingSourceHealthChanged, CoreEvent.scratchpadChanged
        ]
        for event in ["planning-changed-event"] + sourceEvents {
            core.observe(event) { [weak self] line in
                guard let self else { return }
                self.invalidateSuggestions()
                if event != "planning-changed-event" { self.sourcesRevision += 1 }
                let changed = event == "planning-changed-event"
                    ? (try? Core.payload(line) as PlanningChange)?.projectId : nil
                Task {
                    await self.refresh()
                    if let changed, !changed.isEmpty { await self.loadDetails([changed]) }
                }
            }
        }
    }

    /// Every project's page is read up front, so rows can show owners and
    /// late work, and an opened project draws at once.
    func start() async {
        await refresh()
        await loadDetails((projects ?? []).map(\.id))
    }

    func open(_ id: String) {
        guard selectedProjectId != id else { return }
        selectedProjectId = id
        detailLoading = false
        detailError = nil
        mutationError = nil
    }

    func closeProject() {
        selectedProjectId = nil
        detailLoading = false
        detailError = nil
        mutationError = nil
    }

    func refresh() async {
        await loadProjects()
        await loadStatusNames()
        await loadDependencies()
        await loadInitiatives()
        if let id = selectedProjectId {
            await loadDetail(id)
            if updates[id] != nil { await loadUpdates(id) }
            if history[id] != nil { await loadHistory(id) }
            await comments.reload([.project, .milestone])
        }
        if trashed != nil { await loadTrashed() }
        if let range = calendarRange { await loadCalendar(range) }
        if reviewingLinks { await loadSuggestions() }
        await loadQuietReviewGrants()
    }

    func saveProject(_ project: PlanningProject, _ change: PlanningProjectSave) async {
        do {
            try await write {
                let _: PlanningProject = try await self.core.request("planning_project_save", PlanningProjectSaveParams(
                    operationId: UUID().uuidString, id: project.id, input: change))
            }
        } catch {
            mutationError = error.localizedDescription
        }
        await refresh()
    }

    /// Changes a milestone's name, description or day, the save `work
    /// milestone-save` makes.
    func saveMilestone(_ milestone: PlanningMilestone, _ change: PlanningMilestoneSave) async {
        do {
            try await write {
                let _: PlanningMilestone = try await self.core.request("planning_milestone_save", PlanningMilestoneSaveParams(
                    operationId: UUID().uuidString, projectId: milestone.projectId, id: milestone.id, input: change))
            }
        } catch {
            mutationError = error.localizedDescription
        }
        await refresh()
        await loadDetails([milestone.projectId])
    }

    /// Puts a project in the 30-day bin and offers Undo.
    func binProject(_ project: PlanningProject) async {
        do {
            try await write {
                let _: PlanningProject = try await self.core.request("planning_project_save", PlanningProjectSaveParams(
                    operationId: UUID().uuidString, id: project.id, input: PlanningProjectSave(trashed: true)))
            }
            if selectedProjectId == project.id { closeProject() }
            binned = project
        } catch {
            mutationError = error.localizedDescription
        }
        await refresh()
    }

    func restoreProject(_ id: String) async {
        if binned?.id == id { binned = nil }
        do {
            try await write {
                let _: PlanningProject = try await self.core.request("planning_project_save", PlanningProjectSaveParams(
                    operationId: UUID().uuidString, id: id, input: PlanningProjectSave(trashed: false)))
            }
        } catch {
            mutationError = error.localizedDescription
        }
        await refresh()
    }

    func dismissBinned() { binned = nil }

    func loadTrashed() async {
        do {
            trashed = try await core.request("planning_projects_trashed")
            trashedError = nil
        } catch {
            trashedError = error.localizedDescription
        }
    }

    func loadUpdates(_ projectId: String, more: Bool = false) async {
        let cursor = more ? updatesCursor[projectId] : nil
        if more, cursor == nil { return }
        do {
            let page: PlanningProjectUpdatePage = try await core.request(
                "planning_project_updates", PlanningUpdatesParams(projectId: projectId, limit: 20, cursor: cursor))
            updates[projectId] = (more ? updates[projectId] ?? [] : []) + page.updates.filter { !$0.deleted }
            updatesCursor[projectId] = page.nextCursor
            updatesError = nil
        } catch {
            updatesError = error.localizedDescription
        }
    }

    func postUpdate(projectId: String, health: PlanningProjectHealth, body: String) async throws {
        try await write {
            let _: PlanningProjectUpdate = try await self.core.request("planning_project_update_add", PlanningUpdateAddParams(
                operationId: UUID().uuidString, projectId: projectId, health: health, body: body))
        }
        await loadUpdates(projectId)
        await loadDetail(projectId)
    }

    /// Rewrites one of my updates: its health and body, the change `work
    /// project-update-edit` makes.
    func editUpdate(_ update: PlanningProjectUpdate, health: PlanningProjectHealth, body: String) async throws {
        try await write {
            let _: PlanningProjectUpdate = try await self.core.request("planning_project_update_edit", PlanningUpdateEditParams(
                operationId: UUID().uuidString, id: update.id, health: health, body: body))
        }
        await loadUpdates(update.projectId)
        await loadDetail(update.projectId)
    }

    func deleteUpdate(_ update: PlanningProjectUpdate) async {
        do {
            try await write {
                let _: PlanningProjectUpdate = try await self.core.request("planning_project_update_delete",
                    PlanningOperationParams(operationId: UUID().uuidString, id: update.id))
            }
        } catch {
            mutationError = error.localizedDescription
        }
        await loadUpdates(update.projectId)
        await loadDetail(update.projectId)
    }

    /// What moved since the last update, as the core drafts it.
    func draftUpdate(projectId: String) async throws -> String {
        try await core.request("planning_project_update_draft", PlanningProjectIdParams(projectId: projectId))
    }

    func loadHistory(_ projectId: String) async {
        do {
            history[projectId] = try await core.request("planning_project_history",
                                                        PlanningProjectIdParams(projectId: projectId))
            historyError = nil
        } catch {
            historyError = error.localizedDescription
        }
    }

    func loadStatusNames() async {
        do {
            statusNames = try await core.request("planning_project_status_names")
            statusNamesError = nil
        } catch {
            statusNamesError = error.localizedDescription
        }
    }

    func loadDependencies() async {
        do {
            dependencies = try await core.request("planning_project_dependencies")
        } catch {
            mutationError = error.localizedDescription
        }
    }

    /// Makes `blockedId` wait on `blockerId`.
    func addDependency(blockerId: String, blockedId: String) async {
        do {
            let _: PlanningProjectDependency = try await write {
                try await self.core.request("planning_project_dependency_add",
                    PlanningDependencyAddParams(operationId: UUID().uuidString, blockerId: blockerId, blockedId: blockedId))
            }
        } catch {
            mutationError = error.localizedDescription
        }
        await loadDependencies()
    }

    func removeDependency(_ dependency: PlanningProjectDependency) async {
        do {
            let _: PlanningProjectDependency = try await write {
                try await self.core.request("planning_project_dependency_remove",
                    PlanningOperationParams(operationId: UUID().uuidString, id: dependency.id))
            }
        } catch {
            mutationError = error.localizedDescription
        }
        await loadDependencies()
    }

    /// The projects `projectId` is not linked to yet, either way.
    func dependencyChoices(for projectId: String) -> [PlanningProject] {
        let linked = Set(dependencies.filter { $0.blockerId == projectId || $0.blockedId == projectId }
            .flatMap { [$0.blockerId, $0.blockedId] })
        return (projects ?? []).map(\.project).filter { $0.id != projectId && !linked.contains($0.id) }
    }

    func projectTitle(_ id: String) -> String {
        projects?.first { $0.id == id }?.project.title ?? "A project"
    }

    /// The palette's dependency verbs for the open project: link it to each
    /// project it is not linked to yet, either way, or take a link off.
    var dependencyPaletteActions: [PaletteAction] {
        guard let id = selectedProjectId else { return [] }
        let choices = dependencyChoices(for: id)
        let blockedBy = choices.map { other in
            PaletteAction(id: "project-blocked-by-\(other.id)", group: .actions, title: "Blocked by \(other.title)",
                          run: { [weak self] in Task { await self?.addDependency(blockerId: other.id, blockedId: id) } },
                          icon: "arrow.right.to.line")
        }
        let blocking = choices.map { other in
            PaletteAction(id: "project-blocking-\(other.id)", group: .actions, title: "Blocking \(other.title)",
                          run: { [weak self] in Task { await self?.addDependency(blockerId: id, blockedId: other.id) } },
                          icon: "arrow.left.to.line")
        }
        let removals = dependencies.filter { $0.blockerId == id || $0.blockedId == id }.map { link in
            PaletteAction(id: "project-unlink-\(link.id)", group: .actions,
                          title: "Remove link: \(projectTitle(link.blockedId)) waits on \(projectTitle(link.blockerId))",
                          run: { [weak self] in Task { await self?.removeDependency(link) } },
                          icon: "xmark")
        }
        return blockedBy + blocking + removals
    }

    func loadInitiatives() async {
        do {
            initiatives = try await core.request("planning_initiatives")
        } catch {
            mutationError = error.localizedDescription
        }
    }

    /// The initiative a project sits in, if any.
    func initiative(of projectId: String) -> PlanningInitiative? {
        initiatives.first { $0.projectIds.contains(projectId) }
    }

    /// Makes an initiative, or changes `initiative`; returns the saved one.
    @discardableResult
    func saveInitiative(_ initiative: PlanningInitiative?, _ change: PlanningInitiativeSave) async -> PlanningInitiative? {
        var saved: PlanningInitiative?
        do {
            saved = try await write {
                try await self.core.request("planning_initiative_save", PlanningInitiativeSaveParams(
                    operationId: UUID().uuidString, id: initiative?.id, input: change))
            }
        } catch {
            mutationError = error.localizedDescription
        }
        await loadInitiatives()
        return saved
    }

    func deleteInitiative(_ initiative: PlanningInitiative) async {
        do {
            let _: PlanningInitiative = try await write {
                try await self.core.request("planning_initiative_delete",
                    PlanningOperationParams(operationId: UUID().uuidString, id: initiative.id))
            }
        } catch {
            mutationError = error.localizedDescription
        }
        await loadInitiatives()
    }

    /// Puts a project in an initiative, moving it out of any other.
    func addToInitiative(_ initiativeId: String, projectId: String) async {
        do {
            let _: PlanningInitiative = try await write {
                try await self.core.request("planning_initiative_add_project", PlanningInitiativeProjectParams(
                    operationId: UUID().uuidString, initiativeId: initiativeId, projectId: projectId))
            }
        } catch {
            mutationError = error.localizedDescription
        }
        await loadInitiatives()
    }

    func removeFromInitiative(projectId: String) async {
        do {
            let _: PlanningInitiative = try await write {
                try await self.core.request("planning_initiative_remove_project", PlanningInitiativeProjectParams(
                    operationId: UUID().uuidString, initiativeId: nil, projectId: projectId))
            }
        } catch {
            mutationError = error.localizedDescription
        }
        await loadInitiatives()
    }

    func loadProjectTemplates() async {
        do {
            projectTemplates = try await core.request("planning_project_templates")
        } catch {
            mutationError = error.localizedDescription
        }
    }

    /// Saves the project as the template called `name`, refilling the one
    /// of that name when there is one.
    func saveProjectTemplate(name: String, projectId: String, operationId: String) async throws {
        let _: PlanningProjectTemplate = try await write {
            try await self.core.request("planning_project_template_save",
                PlanningTemplateSaveParams(operationId: operationId, name: name, projectId: projectId))
        }
        await loadProjectTemplates()
    }

    /// Deletes a template. Projects made from it stay.
    func deleteProjectTemplate(id: String, operationId: String) async throws {
        let _: PlanningProjectTemplate = try await write {
            try await self.core.request("planning_project_template_delete",
                PlanningTemplateDeleteParams(operationId: operationId, id: id))
        }
        await loadProjectTemplates()
    }

    /// Makes a project from a template, its milestones dated from `start`;
    /// a summary or description given replaces the template's.
    func createProject(template: PlanningProjectTemplate, title: String, summary: String?, description: String?,
                       start: Date, operationId: String) async throws -> PlanningProject {
        let made: PlanningFromTemplate = try await write {
            try await self.core.request("planning_project_from_template", PlanningFromTemplateParams(
                operationId: operationId, templateId: template.id, title: title,
                startAtMs: Self.milliseconds(Calendar.current.startOfDay(for: start))))
        }
        if summary != nil || description != nil {
            try await write {
                let _: PlanningProject = try await self.core.request("planning_project_save", PlanningProjectSaveParams(
                    operationId: UUID().uuidString, id: made.project.id,
                    input: PlanningProjectSave(summary: summary.map { .set($0) },
                                               description: description.map { .set($0) })))
            }
        }
        open(made.project.id)
        await refresh()
        return made.project
    }

    /// The name the project's status goes by, when one is set in its category.
    func statusName(of project: PlanningProject) -> PlanningProjectStatusName? {
        guard let id = project.statusNameId else { return nil }
        return statusNames.first { $0.id == id && $0.category == project.status }
    }

    func statusTitle(of project: PlanningProject) -> String {
        statusName(of: project)?.name ?? project.status.label
    }

    /// Adds a status name, or renames one or moves it to another category;
    /// the projects under a moved name move with it.
    func saveStatusName(id: String?, category: PlanningProjectStatus, name: String) async throws {
        try await write {
            let _: PlanningProjectStatusName = try await self.core.request(
                "planning_project_status_name_save",
                PlanningStatusNameSaveParams(operationId: UUID().uuidString, id: id, category: category, name: name))
        }
        await refresh()
    }

    /// Removes a status name; the projects under it keep its category.
    func deleteStatusName(_ name: PlanningProjectStatusName) async throws {
        try await write { () async throws -> Void in
            try await self.core.request("planning_project_status_name_delete",
                                        PlanningOperationParams(operationId: UUID().uuidString, id: name.id))
        }
        await refresh()
    }

    /// Makes a project of a milestone, its tasks moving with it, and opens it.
    func convertMilestone(_ milestone: PlanningMilestone) async {
        do {
            let project: PlanningProject = try await write {
                let result: PlanningProject = try await self.core.request("planning_milestone_to_project",
                    PlanningOperationParams(operationId: UUID().uuidString, id: milestone.id))
                return result
            }
            open(project.id)
        } catch {
            mutationError = error.localizedDescription
        }
        await refresh()
        await loadDetails([milestone.projectId])
    }

    /// Deletes a milestone for good, the delete `work milestone-delete`
    /// makes. Its tasks stay in the project with no milestone.
    func deleteMilestone(_ milestone: PlanningMilestone) async {
        do {
            try await write { () async throws -> Void in
                try await self.core.request("planning_milestone_delete",
                                            PlanningOperationParams(operationId: UUID().uuidString, id: milestone.id))
            }
        } catch {
            mutationError = error.localizedDescription
        }
        await refresh()
        await loadDetails([milestone.projectId])
    }

    func reorderMilestones(projectId: String, ids: [String]) async {
        if let detail = details[projectId] {
            var changed = detail
            changed.milestones = detail.milestones.map { milestone in
                var moved = milestone
                moved.position = ids.firstIndex(of: milestone.id).map(Int64.init)
                return moved
            }
            details[projectId] = changed
        }
        do {
            try await write {
                let _: [PlanningMilestone] = try await self.core.request("planning_milestone_reorder", PlanningReorderParams(
                    operationId: UUID().uuidString, projectId: projectId, milestoneIds: ids))
            }
        } catch {
            mutationError = error.localizedDescription
        }
        await loadDetail(projectId)
    }

    func loadProjects() async {
        listSequence += 1
        let sequence = listSequence
        listLoading = true
        defer { if sequence == listSequence { listLoading = false } }
        do {
            let rows: [PlanningProjectSummary] = try await core.request("planning_projects")
            guard sequence == listSequence else { return }
            projects = rows
            listError = nil
        } catch {
            guard sequence == listSequence else { return }
            listError = error.localizedDescription
        }
    }

    func loadDetail(_ id: String) async {
        guard selectedProjectId == id else { return }
        let stamp = nextStamp(id)
        detailLoading = true
        defer { if detailStamps[id] == stamp, selectedProjectId == id { detailLoading = false } }
        do {
            let result: PlanningProjectDetail = try await core.request("planning_project", PlanningIdParams(id: id))
            guard detailStamps[id] == stamp else { return }
            details[id] = result
            if selectedProjectId == id { detailError = nil }
        } catch {
            guard detailStamps[id] == stamp, selectedProjectId == id else { return }
            details[id] = nil
            detailError = error.localizedDescription
        }
    }

    /// A project's milestones, from its page when that is read, else read now.
    func milestones(of projectId: String) async -> [PlanningMilestone] {
        if let known = details[projectId] { return known.milestones }
        let read: PlanningProjectDetail? = try? await core.request("planning_project", PlanningIdParams(id: projectId))
        return read?.milestones ?? []
    }

    /// Pages other than the open one, read one after another and shown in
    /// one change, so the list settles once.
    private func loadDetails(_ ids: [String]) async {
        let ids = ids.filter { $0 != selectedProjectId }
        guard !ids.isEmpty else { return }
        var stamps: [String: Int] = [:]
        for id in ids { stamps[id] = nextStamp(id) }
        var loaded: [String: PlanningProjectDetail] = [:]
        for id in ids {
            if let result: PlanningProjectDetail = try? await core.request("planning_project", PlanningIdParams(id: id)) {
                loaded[id] = result
            }
        }
        var merged = details
        for (id, result) in loaded where detailStamps[id] == stamps[id] { merged[id] = result }
        if let projects {
            let listed = Set(projects.map(\.id))
            merged = merged.filter { listed.contains($0.key) || $0.key == selectedProjectId }
        }
        details = merged
    }

    private func nextStamp(_ id: String) -> Int {
        detailStamp += 1
        detailStamps[id] = detailStamp
        return detailStamp
    }

    func stopCalendar() {
        calendarRange = nil
        calendarSequence += 1
        calendarLoading = false
    }

    func loadCalendar(_ range: DateInterval) async {
        if calendarRange != range {
            calendarItems = nil
            calendarError = nil
        }
        calendarRange = range
        calendarSequence += 1
        let sequence = calendarSequence
        calendarLoading = true
        defer { if sequence == calendarSequence { calendarLoading = false } }
        do {
            let page: PlanningCalendarPage = try await core.request("planning_calendar", PlanningCalendarParams(
                startMs: Self.milliseconds(range.start), endMs: Self.milliseconds(range.end)))
            guard sequence == calendarSequence else { return }
            calendarItems = page.items
            calendarError = page.conversationsAvailable ? nil : "Conversations are still unlocking. Showing planned work only."
        } catch {
            guard sequence == calendarSequence else { return }
            calendarItems = nil
            calendarError = error.localizedDescription
        }
    }

    func sourceCandidates(_ kind: PlanningSourceList, query: String, after cursor: Int64? = nil,
                          meetingId: String? = nil) async throws -> PlanningSourcePage {
        if kind == .loop, let meetingId {
            let page: PlanningLoopSources = try await core.request("meeting_loops", ["sessionId": meetingId])
            return PlanningSourcePage(entries: page.rows
                .filter { query.isEmpty || $0.text.localizedCaseInsensitiveContains(query) }
                .map { PlanningSourceChoice(kind: .loop, sourceId: $0.loopId, title: $0.text, updatedAtMs: nil) },
                nextCursor: nil)
        }
        switch kind {
        case .meeting, .loop:
            let page: PlanningMeetingSources = try await core.request(
                "meeting_list", MeetingRequest.list(cursorUtcMs: cursor, limit: 50, titleQuery: query,
                                                    status: .any, window: .any, folderId: nil))
            return PlanningSourcePage(entries: page.entries.map {
                PlanningSourceChoice(kind: .meeting, sourceId: $0.sessionId, title: $0.title, updatedAtMs: $0.createdAtUtcMs)
            }, nextCursor: page.hasMore ? page.entries.last?.createdAtUtcMs : nil)
        case .note:
            let notes: [PlanningNoteSource] = try await core.request(
                "scratchpad_list", PlanningSourceQueryParams(query: query))
            return PlanningSourcePage(entries: notes.map {
                PlanningSourceChoice(kind: .note, sourceId: $0.id, title: $0.title, updatedAtMs: $0.updatedAtMs)
            }, nextCursor: nil)
        }
    }

    func beginReview() async {
        reviewingLinks = true
        suggestionDecisionError = nil
        await loadSuggestions()
    }

    func endReview() {
        reviewingLinks = false
        invalidateSuggestions()
    }

    func invalidateSuggestions() {
        suggestionSequence += 1
        suggestions = nil
        suggestionsLoading = false
        suggestionsError = nil
    }

    func loadSuggestions() async {
        invalidateSuggestions()
        let sequence = suggestionSequence
        suggestionsLoading = true
        defer { if sequence == suggestionSequence { suggestionsLoading = false } }
        do {
            let rows: [PlanningSuggestion] = try await core.request("planning_suggestions")
            guard sequence == suggestionSequence, reviewingLinks else { return }
            suggestions = rows
        } catch {
            guard sequence == suggestionSequence, reviewingLinks else { return }
            suggestionsError = error.localizedDescription
        }
    }

    func generateSuggestions(projectId: String, sources: [PlanningSourceChoice]) async throws {
        guard !suggestionsGenerating else {
            throw CoreError.remote(.string("Wait for the current review to finish."))
        }
        guard (1...12).contains(sources.count) else {
            throw CoreError.remote(.string("Select 1–12 sources."))
        }
        suggestionsGenerating = true
        defer { suggestionsGenerating = false }
        do {
            try await write {
                let _: [PlanningSuggestion] = try await self.core.request(
                    "planning_generate_suggestions",
                    PlanningReviewLinksParams(projectId: projectId, selectedSources: sources.map {
                        PlanningSuggestionSource(kind: $0.kind, id: $0.sourceId)
                    })
                )
            }
            await loadSuggestions()
        } catch {
            await loadSuggestions()
            throw error
        }
    }

    func quietReview(for projectId: String) -> QuietReviewState? {
        quietReviewStates?.first { $0.projectId == projectId }
    }

    func loadQuietReviewGrants() async {
        quietReviewSequence += 1
        let sequence = quietReviewSequence
        do {
            let states: [QuietReviewState] = try await core.request("planning_quiet_review_grants")
            guard sequence == quietReviewSequence else { return }
            quietReviewStates = states
            quietReviewLoadError = nil
        } catch {
            guard sequence == quietReviewSequence else { return }
            quietReviewLoadError = "Could not load nighttime review."
        }
    }

    func setQuietReview(projectId: String, enabled: Bool, sources: [PlanningSourceChoice]) async throws {
        guard quietReviewSavingProjectId == nil else {
            throw CoreError.remote(.string("Wait for the review setting to save."))
        }
        guard !enabled || (1...12).contains(sources.count) else {
            throw CoreError.remote(.string("Select 1–12 sources."))
        }
        guard !enabled || quietReviewStates != nil else {
            throw CoreError.remote(.string("Reload nighttime review before enabling it."))
        }
        quietReviewSavingProjectId = projectId
        if !enabled { quietReviewRevocationsPending.insert(projectId) }
        if quietReviewRunningProjectId == projectId { quietReviewRunRevision += 1 }
        quietReviewSequence += 1
        quietReviewStatus[projectId] = nil
        defer { quietReviewSavingProjectId = nil }
        do {
            let state: QuietReviewState = try await core.request(
                "planning_quiet_review_set",
                QuietReviewSetParams(projectId: projectId, enabled: enabled, selectedSources: enabled ? sources.map {
                    PlanningSuggestionSource(kind: $0.kind, id: $0.sourceId)
                } : []))
            quietReviewSequence += 1
            mergeQuietReview(state)
            quietReviewRevocationsPending.remove(projectId)
        } catch {
            await loadQuietReviewGrants()
            let message = Self.quietReviewMessage(error)
            quietReviewStatus[projectId] = quietReviewRevocationsPending.contains(projectId)
                ? "Nighttime review is paused. \(message)" : message
            throw CoreError.remote(.string(quietReviewStatus[projectId] ?? message))
        }
    }

    func runQuietReview(projectId: String) async {
        guard !Task.isCancelled, quietReviewRunningProjectId == nil,
              quietReviewSavingProjectId != projectId, !quietReviewRevocationsPending.contains(projectId),
              quietReview(for: projectId)?.enabled == true else { return }
        let revision = quietReviewRunRevision
        quietReviewRunningProjectId = projectId
        quietReviewStatus[projectId] = nil
        defer { quietReviewRunningProjectId = nil }
        do {
            let state: QuietReviewState = try await core.request(
                "planning_quiet_review_run", QuietReviewProjectParams(projectId: projectId))
            guard !Task.isCancelled, revision == quietReviewRunRevision else { return }
            quietReviewSequence += 1
            mergeQuietReview(state)
            if reviewingLinks { await loadSuggestions() }
        } catch {
            guard !Task.isCancelled, revision == quietReviewRunRevision else { return }
            quietReviewStatus[projectId] = Self.quietReviewMessage(error)
        }
    }

    func cancelQuietReview(projectId: String) async {
        if quietReviewRunningProjectId == projectId { quietReviewRunRevision += 1 }
        do {
            try await core.request("planning_quiet_review_cancel", QuietReviewProjectParams(projectId: projectId))
        } catch {
            quietReviewStatus[projectId] = "Review could not be stopped. Turn off nighttime review or quit Sona."
        }
    }

    private func mergeQuietReview(_ state: QuietReviewState) {
        var states = quietReviewStates ?? []
        states.removeAll { $0.projectId == state.projectId }
        states.append(state)
        quietReviewStates = states
        quietReviewLoadError = nil
    }

    private static func quietReviewMessage(_ error: Error) -> String {
        switch error.localizedDescription {
        case "quiet_review:consent_required": "Nighttime review is off."
        case "quiet_review:invalid_request": "Select 1–12 available sources for an active project."
        case "quiet_review:busy": "Another review or recording is in progress."
        case "quiet_review:outside_hours": "Waiting for 22:00–06:00 local time."
        case "quiet_review:user_active": "Waiting for 10 minutes without keyboard or mouse input."
        case "quiet_review:sleeping": "Review paused while the Mac sleeps or another user is using it."
        case "quiet_review:apple_unavailable": "Apple Intelligence is not available for nighttime review."
        case "quiet_review:apple_required": "Nighttime review requires the on-device Apple Intelligence engine."
        case "quiet_review:cancelled": "Review paused."
        case "quiet_review:source_changed", "quiet_review:target_changed":
            "Sources or project changed. No new links were saved."
        case "quiet_review:unavailable": "Nighttime review is unavailable on this Mac."
        default: "Nighttime review could not finish."
        }
    }

    func decideSuggestion(_ suggestion: PlanningSuggestion, accept: Bool) async {
        guard !mutating, suggestionActionId == nil,
              let current = suggestions?.first(where: { $0.id == suggestion.id && $0.revision == suggestion.revision }) else {
            suggestionDecisionError = "Refresh links before trying again."
            return
        }
        guard !accept || current.canLink else {
            suggestionDecisionError = "Source unavailable."
            return
        }
        let command = accept ? "planning_accept_suggestion" : "planning_decline_suggestion"
        let key = "\(command):\(suggestion.id):\(suggestion.revision)"
        let operationId = suggestionOperations[key] ?? UUID().uuidString
        suggestionOperations[key] = operationId
        suggestionActionId = suggestion.id
        suggestionDecisionError = nil
        defer { suggestionActionId = nil }
        do {
            try await write {
                let _: PlanningSuggestionDecision = try await self.core.request(
                    command, PlanningSuggestionDecisionParams(
                        id: suggestion.id, expectedRevision: suggestion.revision, operationId: operationId
                    )
                )
            }
            suggestionOperations.removeValue(forKey: key)
            invalidateSuggestions()
            if accept { await refresh() }
            else { await loadSuggestions() }
        } catch {
            invalidateSuggestions()
            await loadSuggestions()
            suggestionDecisionError = error.localizedDescription
        }
    }

    func createProject(title: String, summary: String?, description: String?,
                       operationId: String) async throws -> PlanningProject {
        let project: PlanningProject = try await write {
            let result: PlanningProject = try await self.core.request("planning_create_project", PlanningProjectParams(
                title: title, description: description, operationId: operationId))
            return result
        }
        if let summary {
            try await write {
                let _: PlanningProject = try await self.core.request("planning_project_save", PlanningProjectSaveParams(
                    operationId: UUID().uuidString, id: project.id, input: PlanningProjectSave(summary: .set(summary))))
            }
        }
        open(project.id)
        await refresh()
        return project
    }

    func createMilestone(projectId: String, title: String, dueAt: Date?, operationId: String) async throws {
        try await write {
            let _: PlanningMilestone = try await self.core.request("planning_create_milestone", PlanningMilestoneParams(
                projectId: projectId, title: title, dueAtMs: dueAt.map(Self.milliseconds), operationId: operationId))
        }
        await refresh()
    }

    func createTask(projectId: String, milestoneId: String?, title: String, details: String?,
                    dueAt: Date?, owner: PlanningOwner, operationId: String) async throws {
        let personId: String?
        if case let .person(id) = owner { personId = id } else { personId = nil }
        try await write {
            let _: PlanningTask = try await self.core.request("planning_create_task", PlanningCreateTaskParams(
                input: PlanningTaskParams(projectId: projectId, milestoneId: milestoneId, title: title,
                                          details: details, dueAtMs: dueAt.map(Self.milliseconds),
                                          ownerPersonId: personId, ownerIsSelf: owner == .myself),
                operationId: operationId))
        }
        await refresh()
    }

    func setProjectStatus(_ project: PlanningProject, _ status: PlanningProjectStatus) async {
        guard !mutating else {
            mutationError = "Wait for the current change to finish."
            return
        }
        let changed = project.with(status: status, at: Self.milliseconds(.now))
        if let index = projects?.firstIndex(where: { $0.id == project.id }), let summary = projects?[index] {
            projects?[index] = PlanningProjectSummary(project: changed, openTaskCount: summary.openTaskCount,
                                                      completedTaskCount: summary.completedTaskCount,
                                                      nextDueAtMs: summary.nextDueAtMs,
                                                      latestUpdateAtMs: summary.latestUpdateAtMs)
        }
        if let detail = details[project.id] { details[project.id] = detail.replacing(project: changed) }
        await changeStatus(key: "project:\(project.id):\(project.revision):\(status.rawValue)") { operationId in
            let _: PlanningProject = try await self.core.request("planning_update_project", PlanningStatusParams(
                id: project.id, expectedRevision: project.revision, status: status, operationId: operationId))
        }
    }

    func setMilestoneStatus(_ milestone: PlanningMilestone, _ status: PlanningWorkStatus) async {
        guard !mutating else {
            mutationError = "Wait for the current change to finish."
            return
        }
        if let detail = details[milestone.projectId] {
            details[milestone.projectId] = detail.replacing(milestone: milestone.with(status: status, at: Self.milliseconds(.now)))
        }
        await changeStatus(key: "milestone:\(milestone.id):\(milestone.revision):\(status.rawValue)") { operationId in
            let _: PlanningMilestone = try await self.core.request("planning_update_milestone", PlanningStatusParams(
                id: milestone.id, expectedRevision: milestone.revision, status: status, operationId: operationId))
        }
    }

    func linkSource(targetKind: PlanningTargetKind, targetId: String, sourceKind: PlanningSourceKind,
                    sourceId: String, atMs: Int64?, operationId: String) async throws {
        try await write {
            let _: PlanningEvidenceLink = try await self.core.request("planning_link_source", PlanningLinkParams(
                targetKind: targetKind, targetId: targetId, sourceKind: sourceKind, sourceId: sourceId,
                atMs: atMs, operationId: operationId))
        }
        await refresh()
    }

    func clearMutationError() { mutationError = nil }

    var activeOpenTaskCount: Int {
        Int((projects ?? []).filter { $0.project.status == .active }.reduce(0) { $0 + $1.openTaskCount })
    }

    /// Open tasks past their date in active projects, from the pages read so far.
    func overdueTasks(at now: Int64) -> [PlanningTask] {
        let active = Set((projects ?? []).filter { $0.project.status == .active }.map(\.id))
        return details.values
            .filter { active.contains($0.project.id) }
            .flatMap { detail in detail.tasks.filter { $0.isOverdue(at: now) } }
            .sorted { ($0.dueAtMs ?? 0) < ($1.dueAtMs ?? 0) }
    }

    func showOverdue() {
        closeProject()
        filter = PlanningListFilter(overdueOnly: true)
    }

    private func changeStatus(key: String, _ change: @MainActor (String) async throws -> Void) async {
        let operationId = statusOperations[key] ?? UUID().uuidString
        statusOperations[key] = operationId
        do {
            try await write { try await change(operationId) }
            statusOperations.removeValue(forKey: key)
            await refresh()
        } catch {
            await refresh()
            mutationError = error.localizedDescription
        }
    }

    private func write<Response>(_ change: @MainActor () async throws -> Response) async throws -> Response {
        guard !mutating else { throw CoreError.remote(.string("Wait for the current change to finish.")) }
        mutating = true
        mutationError = nil
        defer { mutating = false }
        return try await change()
    }

    nonisolated static func milliseconds(_ date: Date) -> Int64 { Int64((date.timeIntervalSince1970 * 1000).rounded()) }
}

enum PlanningInitiativeStatus: String, Codable, CaseIterable, Identifiable {
    case planned, active, completed

    var id: String { rawValue }
    var title: String {
        switch self {
        case .planned: "Planned"
        case .active: "Active"
        case .completed: "Completed"
        }
    }
}

/// A goal one level above projects, holding projects; a project sits in one
/// at most. `progress` counts only live projects and their tasks.
struct PlanningInitiative: Decodable, Identifiable, Equatable {
    let id: String
    let name: String
    let description: String?
    let ownerPersonId: String?
    let ownerIsSelf: Bool
    let targetAtMs: Int64?
    let status: PlanningInitiativeStatus
    let projectIds: [String]
    let progress: Progress

    struct Progress: Decodable, Equatable {
        let projects: Int
        let completedProjects: Int
        let openTasks: Int
        let doneTasks: Int

        var fraction: Double {
            let tasks = openTasks + doneTasks
            return tasks == 0 ? 0 : Double(doneTasks) / Double(tasks)
        }
    }

    var owner: PlanningOwner {
        if ownerIsSelf { return .myself }
        return ownerPersonId.map(PlanningOwner.person) ?? .unassigned
    }
}

/// The fields of an initiative to change: `nil` keeps a field, `.clear`
/// empties it, as `planning_initiative_save` reads a missing key and a `null`.
struct PlanningInitiativeSave: Encodable, Equatable {
    var name: String?
    var description: PlanningFieldChange<String>?
    var owner: PlanningOwner?
    var targetAtMs: PlanningFieldChange<Int64>?
    var status: PlanningInitiativeStatus?

    private enum Key: String, CodingKey { case name, description, owner, targetAtMs, status }
    private enum OwnerKey: String, CodingKey { case kind, personId }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        try container.encodeIfPresent(name, forKey: .name)
        switch description {
        case nil: break
        case let .set(value)?: try container.encode(value, forKey: .description)
        case .clear?: try container.encodeNil(forKey: .description)
        }
        if let owner {
            var nested = container.nestedContainer(keyedBy: OwnerKey.self, forKey: .owner)
            switch owner {
            case .unassigned: try nested.encode("nobody", forKey: .kind)
            case .myself: try nested.encode("self", forKey: .kind)
            case let .person(id):
                try nested.encode("person", forKey: .kind)
                try nested.encode(id, forKey: .personId)
            }
        }
        switch targetAtMs {
        case nil: break
        case let .set(value)?: try container.encode(value, forKey: .targetAtMs)
        case .clear?: try container.encodeNil(forKey: .targetAtMs)
        }
        try container.encodeIfPresent(status, forKey: .status)
    }
}

private struct PlanningInitiativeSaveParams: Encodable {
    let operationId: String
    let id: String?
    let input: PlanningInitiativeSave
}

private struct PlanningInitiativeProjectParams: Encodable {
    let operationId: String
    let initiativeId: String?
    let projectId: String
}

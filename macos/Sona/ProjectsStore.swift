import Foundation
import Observation

enum PlanningProjectStatus: String, Codable, CaseIterable, Identifiable {
    case active, paused, completed
    var id: String { rawValue }
    var label: String {
        switch self {
        case .active: "Active"
        case .paused: "Paused"
        case .completed: "Completed"
        }
    }
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
    let status: PlanningProjectStatus
    let revision: Int64
    let createdAtMs: Int64
    let updatedAtMs: Int64
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
enum PlanningSourceKind: String, Codable, Hashable { case meeting, note, loop }

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
}

struct PlanningProjectSummary: Decodable, Identifiable, Equatable {
    let project: PlanningProject
    let openTaskCount: Int64
    let completedTaskCount: Int64
    let nextDueAtMs: Int64?
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
    let project: PlanningProject
    let milestones: [PlanningMilestone]
    let tasks: [PlanningTask]
    let evidence: [PlanningEvidenceLink]
    let pulse: PlanningProjectPulse
}

/// What the projects list is narrowed to.
struct PlanningListFilter: Equatable {
    var status: PlanningProjectStatus?
    var overdueOnly = false
    var mineOnly = false

    var isActive: Bool { status != nil || overdueOnly || mineOnly }
}

extension PlanningProject {
    func with(status: PlanningProjectStatus, at now: Int64) -> PlanningProject {
        PlanningProject(id: id, title: title, description: description, status: status, revision: revision,
                        createdAtMs: createdAtMs, updatedAtMs: now)
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
                          revision: revision, createdAtMs: createdAtMs, updatedAtMs: now)
    }
}

extension PlanningTask {
    func isOverdue(at now: Int64) -> Bool { status.isOpen && PlanningDue.isPast(dueAtMs, at: now) }

    func with(status: PlanningWorkStatus, at now: Int64) -> PlanningTask {
        PlanningTask(id: id, projectId: projectId, milestoneId: milestoneId, title: title, details: details,
                     dueAtMs: dueAtMs, ownerPersonId: ownerPersonId, ownerIsSelf: ownerIsSelf,
                     ownerAvailable: ownerAvailable, status: status, revision: revision,
                     createdAtMs: createdAtMs, updatedAtMs: now)
    }
}

extension PlanningProjectDetail {
    func replacing(project: PlanningProject) -> PlanningProjectDetail {
        PlanningProjectDetail(project: project, milestones: milestones, tasks: tasks, evidence: evidence, pulse: pulse)
    }

    func replacing(milestone: PlanningMilestone) -> PlanningProjectDetail {
        PlanningProjectDetail(project: project, milestones: milestones.map { $0.id == milestone.id ? milestone : $0 },
                              tasks: tasks, evidence: evidence, pulse: pulse)
    }

    func replacing(task: PlanningTask) -> PlanningProjectDetail {
        PlanningProjectDetail(project: project, milestones: milestones,
                              tasks: tasks.map { $0.id == task.id ? task : $0 }, evidence: evidence, pulse: pulse)
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
            revision: revision, createdAtMs: createdAtMs, available: true, title: evidence.title
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

    init(core: Core) {
        self.core = core
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
                let changed = event == "planning-changed-event" ? (try? Core.payload(line) as PlanningChange)?.projectId : nil
                Task {
                    await self.refresh()
                    if let changed { await self.loadDetails([changed]) }
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
        if let id = selectedProjectId { await loadDetail(id) }
        if let range = calendarRange { await loadCalendar(range) }
        if reviewingLinks { await loadSuggestions() }
        await loadQuietReviewGrants()
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

    func createProject(title: String, description: String?, operationId: String) async throws -> PlanningProject {
        let project: PlanningProject = try await write {
            let result: PlanningProject = try await self.core.request("planning_create_project", PlanningProjectParams(
                title: title, description: description, operationId: operationId))
            return result
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
                                                      nextDueAtMs: summary.nextDueAtMs)
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

    func setTaskStatus(_ task: PlanningTask, _ status: PlanningWorkStatus) async {
        guard !mutating else {
            mutationError = "Wait for the current change to finish."
            return
        }
        if let detail = details[task.projectId] {
            details[task.projectId] = detail.replacing(task: task.with(status: status, at: Self.milliseconds(.now)))
        }
        await changeStatus(key: "task:\(task.id):\(task.revision):\(status.rawValue)") { operationId in
            let _: PlanningTask = try await self.core.request("planning_update_task", PlanningStatusParams(
                id: task.id, expectedRevision: task.revision, status: status, operationId: operationId))
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

    static func milliseconds(_ date: Date) -> Int64 { Int64((date.timeIntervalSince1970 * 1000).rounded()) }
}

import Foundation
import Observation

/// The tracker. Every item, archived and snoozed ones too, is read in one
/// list, so the screens filter, group and count without asking the core
/// again. A change shows at once and goes to the core after every earlier
/// one; a change the core refuses is put back and named in `mutationError`.
@MainActor
@Observable
final class WorkStore {
    private(set) var items: [WorkItemRow]?
    private(set) var labels: [WorkLabel] = []
    private(set) var projects: [WorkProjectChoice] = []
    private(set) var people: [WorkPersonChoice] = []
    /// What Sona suggests for each item in triage, by item id.
    private(set) var suggestions: [String: [WorkFieldSuggestion]] = [:]
    private(set) var counts: WorkCounts?
    private(set) var views: [WorkView] = []
    private(set) var loading = false
    private(set) var listError: String?
    private(set) var mutationError: String?
    private(set) var detail: WorkItemDetail?
    private(set) var detailError: String?
    /// What `undo` would take back.
    private(set) var undoTitle: String?

    @ObservationIgnored private let core: Core
    @ObservationIgnored private var refreshing = false
    @ObservationIgnored private var refreshAgain = false
    @ObservationIgnored private var detailId: String?
    @ObservationIgnored private var queue: Task<Void, Never>?
    @ObservationIgnored private var pending = 0
    @ObservationIgnored private var reloadWhenIdle = false
    @ObservationIgnored private var undoStack: [Undo] = []
    @ObservationIgnored private var undoSerial = 0

    private static let changeEvents = ["work-changed", "planning-changed-event", CoreEvent.meetingRemoved]
    private static let everything = WorkQuery(includeArchived: true, includeSnoozed: true)

    init(core: Core) {
        self.core = core
        for event in Self.changeEvents {
            core.observe(event) { [weak self] _ in
                guard let self else { return }
                Task { await self.refresh() }
            }
        }
    }

    func start() async {
        await refresh()
    }

    /// Reads everything again. A call while a read runs asks that read to go
    /// once more instead of starting a second.
    func refresh() async {
        guard !refreshing else {
            refreshAgain = true
            return
        }
        refreshing = true
        loading = true
        defer {
            refreshing = false
            loading = false
        }
        repeat {
            refreshAgain = false
            await load()
        } while refreshAgain
    }

    func loadDetail(_ id: String) async {
        detailId = id
        do {
            let result: WorkItemDetail = try await core.request("work_item", IdParams(id: id))
            guard detailId == id else { return }
            if pending > 0 {
                reloadWhenIdle = true
                if detail?.item.id == id { return }
            }
            detail = result
            detailError = nil
        } catch {
            guard detailId == id else { return }
            detail = nil
            detailError = error.localizedDescription
        }
    }

    // MARK: Changes

    func update(_ ids: [String], _ patch: WorkPatch) {
        let before = rows(ids)
        guard !before.isEmpty else { return }
        let undoable = patch.status == nil || !before.contains { $0.status == .duplicate }
        let undo = undoable
            ? remember(Self.describe(patch, count: before.count), .patches(Self.reversal(of: patch, on: before)))
            : nil
        send(patch, to: before.map(\.id), undo: undo)
    }

    func archive(_ ids: [String]) {
        let ids = rows(ids).filter { $0.archivedAtMs == nil }.map(\.id)
        guard !ids.isEmpty else { return }
        setArchived(ids, true, undo: remember(Self.counted("Archived", ids.count), .restore(ids)))
    }

    func restore(_ ids: [String]) {
        let ids = rows(ids).filter { $0.archivedAtMs != nil }.map(\.id)
        guard !ids.isEmpty else { return }
        setArchived(ids, false, undo: remember(Self.counted("Restored", ids.count), .archive(ids)))
    }

    func accept(_ ids: [String], applySuggestions: Bool) {
        let before = rows(ids).filter { $0.status == .triage }
        guard !before.isEmpty else { return }
        let touched = applySuggestions
            ? WorkPatch(status: .notStarted, projectId: .clear, owner: .nobody, dueAtMs: .clear)
            : WorkPatch(status: .notStarted)
        let undo = remember(Self.counted("Accepted", before.count), .patches(Self.reversal(of: touched, on: before)))
        let suggested = suggestions
        let now = ProjectsStore.milliseconds(.now)
        change(before.map(\.id), undo: undo, { row in
            if applySuggestions {
                for suggestion in suggested[row.id] ?? [] { self.take(suggestion, into: &row) }
            }
            row.status = .notStarted
            row.statusChangedAtMs = now
        }) { operationId, rows in
            try await self.core.request("work_triage_accept", AcceptParams(
                operationId: operationId, ids: rows.map(\.id), applySuggestions: applySuggestions))
        }
    }

    func decline(_ ids: [String]) {
        let before = rows(ids).filter { $0.status == .triage }
        guard !before.isEmpty else { return }
        let undo = remember(
            Self.counted("Declined", before.count), .patches(Self.reversal(of: WorkPatch(status: .canceled), on: before)))
        let now = ProjectsStore.milliseconds(.now)
        change(before.map(\.id), undo: undo, { row in
            row.status = .canceled
            row.statusChangedAtMs = now
        }) { operationId, rows in
            try await self.core.request("work_triage_decline", IdsParams(operationId: operationId, ids: rows.map(\.id)))
        }
    }

    func markDuplicate(_ id: String, of other: String) {
        let before = rows([id]).filter { $0.status != .duplicate }
        guard id != other, !before.isEmpty else { return }
        let undo = remember(
            "Marked as duplicate", .patches(Self.reversal(of: WorkPatch(status: .canceled), on: before)))
        let now = ProjectsStore.milliseconds(.now)
        change([id], undo: undo, { row in
            row.status = .duplicate
            row.duplicateOfId = other
            row.statusChangedAtMs = now
        }) { operationId, _ in
            let item: WorkItem = try await self.core.request("work_mark_duplicate", DuplicateParams(
                operationId: operationId, id: id, ofId: other))
            return [item]
        }
    }

    func addRelation(from: String, to: String, kind: WorkRelationKind) {
        enqueue {
            do {
                let relation: WorkRelation = try await self.core.request("work_relation_add", RelationParams(
                    operationId: Self.newId(), fromId: from, toId: to, kind: kind))
                if var shown = self.detail, [relation.fromId, relation.toId].contains(shown.item.id),
                   !shown.relations.contains(where: { $0.id == relation.id }) {
                    shown.relations.append(relation)
                    self.detail = shown
                }
            } catch {
                self.mutationError = error.localizedDescription
            }
        }
    }

    func removeRelation(_ id: String) {
        let removed = detail?.relations.first { $0.id == id }
        if removed != nil { detail?.relations.removeAll { $0.id == id } }
        enqueue {
            do {
                try await self.core.request("work_relation_remove", OperationIdParams(operationId: Self.newId(), id: id))
            } catch {
                if let removed, var shown = self.detail, [removed.fromId, removed.toId].contains(shown.item.id) {
                    shown.relations.append(removed)
                    self.detail = shown
                }
                self.mutationError = error.localizedDescription
            }
        }
    }

    /// Creates or changes a label; returns its id, which is in `labels` at once.
    @discardableResult
    func saveLabel(id: String?, name: String, hue: Double) -> String {
        let id = id ?? Self.newId()
        let before = labels.first { $0.id == id }
        place(WorkLabel(id: id, name: name, hue: hue, revision: before?.revision ?? 0))
        enqueue {
            do {
                let saved: WorkLabel = try await self.core.request("work_label_save", LabelParams(
                    operationId: Self.newId(), id: id, name: name, hue: hue))
                self.place(saved)
            } catch {
                if let before { self.place(before) } else { self.labels.removeAll { $0.id == id } }
                self.mutationError = error.localizedDescription
            }
        }
        return id
    }

    func deleteLabel(_ id: String) {
        guard labels.contains(where: { $0.id == id }) else { return }
        labels.removeAll { $0.id == id }
        let wearing = (items ?? []).filter { $0.labelIds.contains(id) }.map(\.id)
        edit(Set(wearing)) { row in row.labelIds.removeAll { $0 == id } }
        enqueue {
            do {
                try await self.core.request("work_label_delete", OperationIdParams(operationId: Self.newId(), id: id))
            } catch {
                self.mutationError = error.localizedDescription
                self.reloadWhenIdle = true
            }
        }
    }

    /// Creates or changes a saved view; returns its id, which is in `views`
    /// at once.
    @discardableResult
    func saveView(id: String?, name: String, query: WorkQuery, display: WorkDisplay) -> String {
        let id = id ?? Self.newId()
        let index = views.firstIndex { $0.id == id }
        let before = index.map { views[$0] }
        let shown = WorkView(id: id, name: name, query: query, display: display, revision: before?.revision ?? 0)
        if let index { views[index] = shown } else { views.append(shown) }
        enqueue {
            do {
                let saved: WorkView = try await self.core.request("work_view_save", ViewParams(
                    operationId: Self.newId(), id: id, name: name, query: query, display: display))
                if let at = self.views.firstIndex(where: { $0.id == id }) { self.views[at] = saved }
            } catch {
                if let at = self.views.firstIndex(where: { $0.id == id }) {
                    if let before { self.views[at] = before } else { self.views.remove(at: at) }
                }
                self.mutationError = error.localizedDescription
            }
        }
        return id
    }

    func deleteView(_ id: String) {
        guard let index = views.firstIndex(where: { $0.id == id }) else { return }
        let removed = views.remove(at: index)
        enqueue {
            do {
                try await self.core.request("work_view_delete", OperationIdParams(operationId: Self.newId(), id: id))
            } catch {
                self.views.insert(removed, at: min(index, self.views.count))
                self.mutationError = error.localizedDescription
            }
        }
    }

    /// Waits for earlier changes, so a label made a moment ago exists when
    /// the new item names it.
    func create(
        title: String, details: String? = nil, status: WorkStatus? = nil, priority: Int? = nil,
        projectId: String? = nil, milestoneId: String? = nil, owner: WorkOwner? = nil, dueAtMs: Int64? = nil,
        labelIds: [String] = []
    ) async -> WorkItem? {
        await queue?.value
        pending += 1
        defer { settle() }
        mutationError = nil
        let input = WorkItemInput(
            title: title, details: details, status: status, priority: priority, projectId: projectId,
            milestoneId: milestoneId, owner: owner, dueAtMs: dueAtMs, labelIds: labelIds.isEmpty ? nil : labelIds)
        do {
            let item: WorkItem = try await core.request("work_create", CreateParams(operationId: Self.newId(), input: input))
            if var rows = items, !rows.contains(where: { $0.id == item.id }) {
                rows.append(row(for: item, beside: nil))
                items = rows
            }
            return item
        } catch {
            mutationError = error.localizedDescription
            return nil
        }
    }

    func undo() {
        guard let last = undoStack.popLast() else { return }
        undoTitle = undoStack.last?.title
        switch last.reversal {
        case let .patches(groups):
            for group in groups { send(group.patch, to: group.ids, undo: nil) }
        case let .archive(ids):
            setArchived(ids, true, undo: nil)
        case let .restore(ids):
            setArchived(ids, false, undo: nil)
        }
    }

    // MARK: Plumbing

    private struct Undo {
        let id: Int
        let title: String
        let reversal: Reversal
    }

    private enum Reversal {
        case patches([(ids: [String], patch: WorkPatch)])
        case archive([String])
        case restore([String])
    }

    private func load() async {
        let core = core
        let query = QueryParams(query: Self.everything)
        do {
            async let list: WorkList = core.request("work_list", query)
            async let tally: WorkCounts = core.request("work_counts")
            async let saved: [WorkView] = core.request("work_views")
            let (listed, counted, savedViews) = try await (list, tally, saved)
            if pending > 0 {
                reloadWhenIdle = true
            } else {
                items = listed.items
                labels = listed.labels
                projects = listed.projects
                people = listed.people
                suggestions = listed.suggestions
                counts = counted
                views = savedViews
                listError = nil
            }
        } catch {
            listError = error.localizedDescription
        }
        if let detailId { await loadDetail(detailId) }
    }

    private func send(_ patch: WorkPatch, to ids: [String], undo: Int?) {
        let now = ProjectsStore.milliseconds(.now)
        change(ids, undo: undo, { row in self.patched(&row, patch, at: now) }) { operationId, rows in
            if rows.count == 1, let row = rows.first {
                let item: WorkItem = try await self.core.request("work_update", UpdateParams(
                    operationId: operationId, id: row.id, expectedRevision: row.revision, patch: patch))
                return [item]
            }
            return try await self.core.request("work_bulk_update", PatchParams(
                operationId: operationId, ids: rows.map(\.id), patch: patch))
        }
    }

    private func setArchived(_ ids: [String], _ archived: Bool, undo: Int?) {
        let now = ProjectsStore.milliseconds(.now)
        change(ids, undo: undo, { row in row.archivedAtMs = archived ? now : nil }) { operationId, rows in
            try await self.core.request(
                archived ? "work_archive" : "work_restore", IdsParams(operationId: operationId, ids: rows.map(\.id)))
        }
    }

    /// Shows a change at once, then sends it in its turn with the rows as
    /// they are then, so it carries the revisions earlier changes left.
    private func change(
        _ ids: [String], undo: Int?, _ shown: (inout WorkItemRow) -> Void,
        _ request: @escaping @MainActor (String, [WorkItemRow]) async throws -> [WorkItem]
    ) {
        let before = rows(ids)
        edit(Set(ids), shown)
        enqueue {
            do {
                self.merge(try await request(Self.newId(), self.rows(ids)))
            } catch {
                self.putBack(before)
                self.forget(undo)
                self.mutationError = error.localizedDescription
                self.reloadWhenIdle = true
            }
        }
    }

    private func enqueue(_ work: @escaping @MainActor () async -> Void) {
        pending += 1
        mutationError = nil
        let previous = queue
        queue = Task {
            await previous?.value
            await work()
            self.settle()
        }
    }

    /// A read that lands while changes are in flight is dropped, so it never
    /// shows a row going back; the last change to finish reads again.
    private func settle() {
        pending -= 1
        guard pending == 0, reloadWhenIdle else { return }
        reloadWhenIdle = false
        Task { await refresh() }
    }

    private func rows(_ ids: [String]) -> [WorkItemRow] {
        let wanted = Set(ids)
        return (items ?? []).filter { wanted.contains($0.id) }
    }

    private func edit(_ ids: Set<String>, _ transform: (inout WorkItemRow) -> Void) {
        if var rows = items {
            for index in rows.indices where ids.contains(rows[index].id) { transform(&rows[index]) }
            items = rows
        }
        if var shown = detail, ids.contains(shown.item.id) {
            transform(&shown.item)
            detail = shown
        }
    }

    private func merge(_ saved: [WorkItem]) {
        let byId = Dictionary(saved.map { ($0.id, $0) }, uniquingKeysWith: { _, last in last })
        edit(Set(byId.keys)) { row in
            if let item = byId[row.id] { row = self.row(for: item, beside: row) }
        }
    }

    /// Puts rows back as they were before a refused change, keeping the
    /// revision the core holds.
    private func putBack(_ before: [WorkItemRow]) {
        let byId = Dictionary(before.map { ($0.id, $0) }, uniquingKeysWith: { _, last in last })
        edit(Set(byId.keys)) { row in
            guard var old = byId[row.id] else { return }
            old.revision = row.revision
            row = old
        }
    }

    private func remember(_ title: String, _ reversal: Reversal) -> Int {
        undoSerial += 1
        undoStack.append(Undo(id: undoSerial, title: title, reversal: reversal))
        undoTitle = title
        return undoSerial
    }

    private func forget(_ id: Int?) {
        guard let id else { return }
        undoStack.removeAll { $0.id == id }
        undoTitle = undoStack.last?.title
    }

    private func place(_ label: WorkLabel) {
        labels.removeAll { $0.id == label.id }
        labels.append(label)
        labels.sort { ($0.name.lowercased(), $0.id) < ($1.name.lowercased(), $1.id) }
    }

    private func row(for item: WorkItem, beside old: WorkItemRow?) -> WorkItemRow {
        WorkItemRow(
            id: item.id, number: item.number, title: item.title, details: item.details, status: item.status,
            priority: item.priority, projectId: item.projectId, milestoneId: item.milestoneId,
            ownerPersonId: item.ownerPersonId, ownerIsSelf: item.ownerIsSelf, direction: item.direction,
            dueAtMs: item.dueAtMs, labelIds: item.labelIds, sourceLoopId: item.sourceLoopId,
            duplicateOfId: item.duplicateOfId, snoozedUntilMs: item.snoozedUntilMs, archivedAtMs: item.archivedAtMs,
            statusChangedAtMs: item.statusChangedAtMs, createdAtMs: item.createdAtMs, updatedAtMs: item.updatedAtMs,
            revision: item.revision, sortOrder: item.sortOrder, projectTitle: projectTitle(item.projectId),
            ownerName: personName(item.ownerPersonId), sourceMeetingId: old?.sourceMeetingId,
            sourceMeetingTitle: old?.sourceMeetingTitle, sourceQuote: old?.sourceQuote,
            sourceSpeaker: old?.sourceSpeaker, sourceAtMs: old?.sourceAtMs,
            blockedByOpenCount: old?.blockedByOpenCount ?? 0)
    }

    private func patched(_ row: inout WorkItemRow, _ patch: WorkPatch, at now: Int64) {
        if let title = patch.title { row.title = title }
        if let details = patch.details { row.details = details.value }
        if let status = patch.status, status != row.status {
            if row.status == .duplicate { row.duplicateOfId = nil }
            row.status = status
            row.statusChangedAtMs = now
        }
        if let priority = patch.priority { row.priority = priority }
        if let project = patch.projectId {
            row.projectId = project.value
            row.projectTitle = projectTitle(row.projectId)
        }
        if let milestone = patch.milestoneId { row.milestoneId = milestone.value }
        if let owner = patch.owner { assign(&row, owner) }
        if let due = patch.dueAtMs { row.dueAtMs = due.value }
        if let labelIds = patch.labelIds { row.labelIds = labelIds }
        if let wake = patch.snoozedUntilMs { row.snoozedUntilMs = wake.value }
        row.updatedAtMs = now
    }

    private func take(_ suggestion: WorkFieldSuggestion, into row: inout WorkItemRow) {
        switch suggestion.value {
        case let .owner(owner): assign(&row, owner)
        case let .due(due): row.dueAtMs = due
        case let .project(id):
            row.projectId = id
            row.projectTitle = projectTitle(id)
        case .item: break
        }
    }

    private func assign(_ row: inout WorkItemRow, _ owner: WorkOwner) {
        switch owner {
        case .myself:
            row.ownerIsSelf = true
            row.ownerPersonId = nil
            row.direction = .mine
        case let .person(id):
            row.ownerIsSelf = false
            row.ownerPersonId = id
            row.direction = .waitingOn
        case .nobody:
            row.ownerIsSelf = false
            row.ownerPersonId = nil
            row.direction = .unattributed
        }
        row.ownerName = personName(row.ownerPersonId)
    }

    private func projectTitle(_ id: String?) -> String? {
        id.flatMap { id in projects.first { $0.id == id }?.title }
    }

    private func personName(_ id: String?) -> String? {
        id.flatMap { id in people.first { $0.id == id }?.name }
    }

    /// The patch that puts back every field `patch` sets, one per distinct
    /// set of old values.
    private static func reversal(of patch: WorkPatch, on rows: [WorkItemRow]) -> [(ids: [String], patch: WorkPatch)] {
        var groups: [(ids: [String], patch: WorkPatch)] = []
        for row in rows {
            let inverse = inverse(of: patch, on: row)
            if let index = groups.firstIndex(where: { $0.patch == inverse }) {
                groups[index].ids.append(row.id)
            } else {
                groups.append((ids: [row.id], patch: inverse))
            }
        }
        return groups
    }

    private static func inverse(of patch: WorkPatch, on row: WorkItemRow) -> WorkPatch {
        var inverse = WorkPatch()
        if patch.title != nil { inverse.title = row.title }
        if patch.details != nil { inverse.details = clearable(row.details) }
        if patch.status != nil { inverse.status = row.status }
        if patch.priority != nil { inverse.priority = row.priority }
        if patch.projectId != nil { inverse.projectId = clearable(row.projectId) }
        if patch.milestoneId != nil { inverse.milestoneId = clearable(row.milestoneId) }
        if patch.owner != nil { inverse.owner = row.owner }
        if patch.dueAtMs != nil { inverse.dueAtMs = clearable(row.dueAtMs) }
        if patch.labelIds != nil { inverse.labelIds = row.labelIds }
        if patch.snoozedUntilMs != nil { inverse.snoozedUntilMs = clearable(row.snoozedUntilMs) }
        return inverse
    }

    private static func clearable<T: Equatable>(_ value: T?) -> WorkPatchValue<T> {
        value.map { .set($0) } ?? .clear
    }

    private static func counted(_ verb: String, _ count: Int) -> String {
        count == 1 ? "\(verb) 1 task" : "\(verb) \(count) tasks"
    }

    private static func describe(_ patch: WorkPatch, count: Int) -> String {
        let noun = count == 1 ? "1 task" : "\(count) tasks"
        if let status = patch.status { return "Moved \(noun) to \(status.label)" }
        if patch.priority != nil { return "Changed priority of \(noun)" }
        if patch.owner != nil { return "Reassigned \(noun)" }
        if patch.dueAtMs != nil { return "Changed due date of \(noun)" }
        if patch.projectId != nil { return "Moved \(noun) to a project" }
        if patch.labelIds != nil { return "Changed labels of \(noun)" }
        if patch.snoozedUntilMs != nil { return "Snoozed \(noun)" }
        if patch.title != nil { return "Renamed \(noun)" }
        return "Changed \(noun)"
    }

    /// The core keeps ids in lowercase.
    private static func newId() -> String { UUID().uuidString.lowercased() }
}

private extension WorkPatchValue {
    var value: T? {
        if case let .set(inner) = self { return inner }
        return nil
    }
}

private struct QueryParams: Encodable { let query: WorkQuery }
private struct IdParams: Encodable { let id: String }
private struct CreateParams: Encodable {
    let operationId: String
    let input: WorkItemInput
}
private struct UpdateParams: Encodable {
    let operationId: String
    let id: String
    let expectedRevision: Int64
    let patch: WorkPatch
}
private struct PatchParams: Encodable {
    let operationId: String
    let ids: [String]
    let patch: WorkPatch
}
private struct IdsParams: Encodable {
    let operationId: String
    let ids: [String]
}
private struct AcceptParams: Encodable {
    let operationId: String
    let ids: [String]
    let applySuggestions: Bool
}
private struct DuplicateParams: Encodable {
    let operationId: String
    let id: String
    let ofId: String
}
private struct RelationParams: Encodable {
    let operationId: String
    let fromId: String
    let toId: String
    let kind: WorkRelationKind
}
private struct OperationIdParams: Encodable {
    let operationId: String
    let id: String
}
private struct LabelParams: Encodable {
    let operationId: String
    let id: String
    let name: String
    let hue: Double
}
private struct ViewParams: Encodable {
    let operationId: String
    let id: String
    let name: String
    let query: WorkQuery
    let display: WorkDisplay
}

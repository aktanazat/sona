import AppKit
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
    /// Changes agents proposed for each item, by item id, oldest first.
    private(set) var proposals: [String: [WorkProposal]] = [:]
    /// False while the conversations cannot be opened: the list holds every
    /// item, but no meeting details and no people to choose.
    private(set) var conversationsAvailable = true
    private(set) var counts: WorkCounts?
    private(set) var views: [WorkView] = []
    /// Every cycle, oldest first, and how long they run.
    private(set) var cycles: [WorkCycle] = []
    private(set) var cycleSettings: WorkCycleSettings?
    private(set) var loading = false
    private(set) var listError: String?
    private(set) var mutationError: String?
    private(set) var detail: WorkItemDetail?
    private(set) var detailError: String?
    /// The words Sona listens for, oldest first.
    private(set) var watches: [WorkWatch] = []
    /// What `undo` would take back.
    private(set) var undoTitle: String?
    /// The moment the lists are drawn at. It moves when work changes and on
    /// its own when a snooze ends, a wait comes due or the day turns, so
    /// work comes back on time with nobody touching the window.
    private(set) var now = TaskDates.now()

    @ObservationIgnored private let core: Core
    @ObservationIgnored private var refreshing = false
    @ObservationIgnored private var refreshAgain = false
    @ObservationIgnored private var detailId: String?
    @ObservationIgnored private var queue: Task<Void, Never>?
    @ObservationIgnored private var pending = 0
    @ObservationIgnored private var reloadWhenIdle = false
    @ObservationIgnored private var undoStack: [Undo] = []
    @ObservationIgnored private var undoSerial = 0
    @ObservationIgnored private var alarm: Task<Void, Never>?

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
        core.observe("work-watches-changed") { [weak self] _ in
            Task { await self?.loadWatches() }
        }
    }

    func start() async {
        await refresh()
        await loadWatches()
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
            fold(result.item)
        } catch {
            guard detailId == id else { return }
            detail = nil
            detailError = error.localizedDescription
        }
    }

    /// A change written elsewhere can reach the item's own read before the
    /// list's; the newer row wins so every view of the task agrees.
    private func fold(_ fresh: WorkItemRow) {
        guard var rows = items, let index = rows.firstIndex(where: { $0.id == fresh.id }),
              rows[index].revision < fresh.revision else { return }
        rows[index] = fresh
        items = rows
        keepTime()
    }

    func addComment(on itemId: String, body: String, replyTo: String? = nil) async -> Bool {
        await write(on: itemId, "work_comment_add", CommentAddParams(
            operationId: Self.newId(), itemId: itemId, body: body, replyTo: replyTo))
    }

    func editComment(_ id: String, on itemId: String, body: String) async -> Bool {
        await write(on: itemId, "work_comment_edit", CommentEditParams(operationId: Self.newId(), id: id, body: body))
    }

    func deleteComment(_ id: String, on itemId: String) async -> Bool {
        await write(on: itemId, "work_comment_delete", OperationIdParams(operationId: Self.newId(), id: id))
    }

    func resolveComment(_ id: String, on itemId: String, resolved: Bool) async -> Bool {
        await write(on: itemId, "work_comment_resolve", CommentResolveParams(
            operationId: Self.newId(), id: id, resolved: resolved))
    }

    func attach(_ sources: [WorkAttachmentSource], to itemId: String) async -> Int {
        let had = Set(detail.map { $0.item.id == itemId ? $0.attachments.map(\.id) : [] } ?? [])
        var failure: String?
        var repeats = 0
        for source in sources {
            do {
                let made: WorkAttachment = try await core.request(
                    source.method, source.params(itemId: itemId, operationId: Self.newId()))
                if had.contains(made.id) { repeats += 1 }
            } catch {
                failure = error.localizedDescription
            }
        }
        mutationError = failure
        await loadDetail(itemId)
        return repeats
    }

    func renameAttachment(_ attachment: WorkAttachment, to title: String) async -> Bool {
        await write(on: attachment.itemId, "work_attachment_rename", AttachmentRenameParams(
            operationId: Self.newId(), id: attachment.id, title: title))
    }

    func detach(_ attachment: WorkAttachment) async -> Bool {
        let kept: WorkAttachmentSource?
        if let url = attachment.linkURL, attachment.kind == .link {
            kept = .link(url.absoluteString, title: attachment.title)
        } else if let file = attachment.fileURL {
            let folder = FileManager.default.temporaryDirectory
                .appending(path: "sona-detached/\(Self.newId())", directoryHint: .isDirectory)
            let copy = folder.appending(path: attachment.originalName ?? file.lastPathComponent)
            do {
                try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
                try FileManager.default.copyItem(at: file, to: copy)
                kept = .file(copy, title: attachment.title)
            } catch {
                kept = nil
            }
        } else {
            kept = nil
        }
        let done = await write(on: attachment.itemId, "work_attachment_detach", OperationIdParams(
            operationId: Self.newId(), id: attachment.id))
        if done, let kept {
            _ = remember("Removed \(attachment.title)", .reattach(attachment.itemId, kept))
        }
        return done
    }

    private func write<P: Encodable>(on itemId: String, _ method: String, _ params: P) async -> Bool {
        do {
            try await core.request(method, params)
            mutationError = nil
            await loadDetail(itemId)
            return true
        } catch {
            mutationError = error.localizedDescription
            return false
        }
    }

    /// New tasks under `parentId`, one per title, in the parent's project.
    func createSubtasks(under parentId: String, titles: [String]) {
        let titles = titles.map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
        guard let parent = rows([parentId]).first, !titles.isEmpty else { return }
        let inputs = titles.map { WorkItemInput(title: $0, projectId: parent.projectId) }
        enqueue {
            do {
                let _: [WorkItem] = try await self.core.request("work_subtask_create", SubtaskParams(
                    operationId: Self.newId(), parentId: parentId, inputs: inputs))
            } catch {
                self.mutationError = error.localizedDescription
            }
            self.reloadWhenIdle = true
        }
    }

    func setParent(of id: String, to parentId: String?) {
        guard id != parentId else { return }
        update([id], WorkPatch(parentId: parentId.map { .set($0) } ?? .clear))
    }

    // MARK: Changes

    /// A wait was just asked about: it leaves Follow up at once, and the
    /// read that follows says when it comes back.
    func nudged(_ id: String) {
        guard let before = rows([id]).first else { return }
        let asked = TaskDates.now()
        edit([id]) { $0.followUpAtMs = asked + 7 * 86_400_000 }
        enqueue {
            do {
                try await self.core.request("work_nudged", IdParams(id: id))
            } catch {
                self.edit([id]) { $0.followUpAtMs = before.followUpAtMs }
                self.mutationError = error.localizedDescription
            }
        }
    }

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

    /// Says each task still stands, which changes nothing but its last
    /// touch, so there is nothing to take back.
    func keep(_ ids: [String]) {
        let ids = rows(ids).map(\.id)
        guard !ids.isEmpty else { return }
        let now = ProjectsStore.milliseconds(.now)
        change(ids, undo: nil, { row in row.updatedAtMs = now }) { operationId, rows in
            try await self.core.request("work_keep", IdsParams(operationId: operationId, ids: rows.map(\.id)))
        }
    }

    /// Books the next free hour before the task is due in the Sona
    /// calendar. The row stops reading at risk at once; the hour shows
    /// when the calendar answers.
    func book(_ id: String) {
        guard let before = rows([id]).first, before.status.isOpen, before.timeBlock == nil else { return }
        let undo = remember("Booked time for 1 task", .unbook(id))
        edit([id]) { $0.atRisk = false }
        enqueue {
            do {
                let block: WorkTimeBlock = try await self.core.request(
                    "work_time_block", OperationIdParams(operationId: Self.newId(), id: id))
                self.edit([id]) { $0.timeBlock = block }
            } catch {
                self.edit([id]) { $0.atRisk = before.atRisk }
                self.forget(undo)
                self.mutationError = error.localizedDescription
            }
        }
    }

    func saveCycleSettings(_ settings: WorkCycleSettings) {
        guard let before = cycleSettings, before != settings else { return }
        cycleSettings = settings
        enqueue {
            do {
                let saved: WorkCycles = try await self.core.request(
                    "work_cycle_settings_save", CycleSettingsParams(operationId: Self.newId(), settings: settings))
                self.cycleSettings = saved.settings
                self.cycles = saved.cycles
            } catch {
                self.cycleSettings = before
                self.mutationError = error.localizedDescription
            }
            self.reloadWhenIdle = true
        }
    }

    /// Scope and done as each day of a cycle ended, read from history.
    func progress(of number: Int64) async throws -> WorkCycleProgress {
        try await core.request("work_cycle", CycleParams(number: number))
    }

    var currentCycle: WorkCycle? { cycles.first { $0.state == .current } }

    /// The cycle after the current one.
    var nextCycle: WorkCycle? {
        guard let current = currentCycle else { return nil }
        return cycles.first { $0.startsAtMs == current.endsAtMs }
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

    /// An item's open proposals, oldest first: the open task's own read when
    /// it is that item, else the list's.
    func proposals(for id: String) -> [WorkProposal] {
        if let detail, detail.item.id == id { return detail.proposals }
        return proposals[id] ?? []
    }

    /// Accepts agents' proposals: the fields change at once, the core writes
    /// them in each agent's name, and undo sets them back.
    func acceptProposals(_ ids: [String]) {
        let taken = takeProposals(ids)
        guard !taken.isEmpty else { return }
        let patches = taken.reduce(into: [String: WorkPatch]()) { patches, proposal in
            Self.apply(proposal, to: &patches[proposal.itemId, default: WorkPatch()])
        }
        let before = rows(Array(patches.keys))
        let undoable = !before.contains { $0.status == .duplicate && patches[$0.id]?.status != nil }
        let reversal = before.flatMap { row in
            var touched = patches[row.id] ?? WorkPatch()
            if touched.projectId != nil { touched.milestoneId = .clear }
            return Self.reversal(of: touched, on: [row])
        }
        let undo = undoable ? remember(Self.accepted(taken), .patches(reversal)) : nil
        let now = ProjectsStore.milliseconds(.now)
        change(Array(patches.keys), undo: undo, { row in
            if let patch = patches[row.id] { self.patched(&row, patch, at: now) }
        }) { operationId, _ in
            do {
                return try await self.core.request(
                    "work_proposals_accept", IdsParams(operationId: operationId, ids: taken.map(\.id)))
            } catch {
                self.returnProposals(taken)
                throw error
            }
        }
    }

    /// Turns agents' proposals down; the tasks stay as they are.
    func rejectProposals(_ ids: [String]) {
        let taken = takeProposals(ids)
        guard !taken.isEmpty else { return }
        enqueue {
            do {
                try await self.core.request(
                    "work_proposals_reject", IdsParams(operationId: Self.newId(), ids: taken.map(\.id)))
            } catch {
                self.returnProposals(taken)
                self.mutationError = error.localizedDescription
            }
        }
    }

    /// Hands tasks to an agent, or takes them back with `nil`. The prompt the
    /// core writes for each lands on the clipboard, to paste into that agent.
    func delegate(_ ids: [String], to agent: String?) {
        let before = rows(ids)
        guard !before.isEmpty else { return }
        let noun = before.count == 1 ? "1 task" : "\(before.count) tasks"
        let title = agent.map { "Delegated \(noun) to \($0), prompt copied" } ?? "Took back \(noun)"
        let undo = remember(title, .delegate(before.map { (id: $0.id, agent: $0.doer?.agent) }))
        hand(before.map(\.id), to: agent, copying: true, undo: undo)
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
    /// the new item names it. A link is kept as the page the task came from.
    func create(
        title: String, details: String? = nil, status: WorkStatus? = nil, priority: Int? = nil,
        projectId: String? = nil, milestoneId: String? = nil, owner: WorkOwner? = nil, dueAtMs: Int64? = nil,
        labelIds: [String] = [], recurrence: String? = nil, link: WorkLink? = nil
    ) async -> WorkItem? {
        await queue?.value
        pending += 1
        defer { settle() }
        mutationError = nil
        let input = WorkItemInput(
            title: title, details: details, status: status, priority: priority, projectId: projectId,
            milestoneId: milestoneId, owner: owner, dueAtMs: dueAtMs, labelIds: labelIds.isEmpty ? nil : labelIds,
            recurrence: recurrence)
        do {
            let item: WorkItem
            if let link {
                let source = LinkSource(url: link.url, title: link.title)
                item = try await core.request("work_capture", CaptureParams(operationId: Self.newId(), input: input, source: source))
            } else {
                item = try await core.request("work_create", CreateParams(operationId: Self.newId(), input: input))
            }
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

    /// Reads a typed title for its fields, leaving the `plain` words as
    /// text. It changes nothing, so it does not wait in the queue.
    func parse(_ text: String, plain: [String]) async -> WorkParse? {
        try? await core.request("work_parse", ParseParams(text: text, plain: plain, spoken: false))
    }

    /// A new label in the next hue along; returns its id.
    func makeLabel(named name: String) -> String {
        let hues: [Double] = [25, 60, 145, 200, 250, 290, 330]
        return saveLabel(id: nil, name: name, hue: hues[labels.count % hues.count])
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
        case let .delegate(doers):
            for (agent, handed) in Dictionary(grouping: doers, by: { $0.agent }) {
                hand(handed.map { $0.id }, to: agent, copying: false, undo: nil)
            }
        case let .unbook(id):
            unbook(id)
        case let .reattach(itemId, source):
            Task {
                _ = await attach([source], to: itemId)
                if case let .file(copy, _) = source {
                    try? FileManager.default.removeItem(at: copy.deletingLastPathComponent())
                }
            }
        }
    }

    private func unbook(_ id: String) {
        guard let before = rows([id]).first else { return }
        edit([id]) { $0.timeBlock = nil }
        enqueue {
            do {
                try await self.core.request(
                    "work_time_block_remove", OperationIdParams(operationId: Self.newId(), id: id))
            } catch {
                self.edit([id]) { $0.timeBlock = before.timeBlock }
                self.mutationError = error.localizedDescription
            }
            self.reloadWhenIdle = true
        }
    }

    // MARK: Ask, say, watch

    /// The tasks a sentence would change and how, or nil when it is not a
    /// change to tasks. Reads only, so it does not wait in the queue.
    func command(_ text: String, selection: [String] = []) async -> WorkCommand? {
        do {
            let read: WorkCommand? = try await core.request(
                "work_command", CommandParams(text: text, selection: selection))
            return read
        } catch {
            return nil
        }
    }

    /// One change to every task the command named, undone as one.
    func apply(_ command: WorkCommand) {
        let ids = command.items.map(\.id)
        switch command.change {
        case let .update(patch): update(ids, patch)
        case .archive: archive(ids)
        }
    }

    func ask(_ question: String) async throws -> WorkAnswer {
        try await core.request("work_ask", QuestionParams(question: question))
    }

    /// Triage tasks from answer lines, each keeping its words and where they
    /// were said. Waits for earlier changes, like `create`.
    @discardableResult
    func take(_ lines: [WorkAnswerLine]) async -> [WorkItem] {
        guard !lines.isEmpty else { return [] }
        await queue?.value
        pending += 1
        defer { settle() }
        mutationError = nil
        let taken = lines.map { WorkTakenLine(quote: $0.quote, source: $0.source, personId: $0.personId) }
        do {
            let made: [WorkItem] = try await core.request(
                "work_take_lines", TakeParams(operationId: Self.newId(), lines: taken))
            if var rows = items {
                for item in made where !rows.contains(where: { $0.id == item.id }) {
                    rows.append(row(for: item, beside: nil))
                }
                items = rows
            }
            return made
        } catch {
            mutationError = error.localizedDescription
            return []
        }
    }

    func loadWatches() async {
        guard let read: [WorkWatch] = try? await core.request("work_watches") else { return }
        watches = read
    }

    /// Shows at once; the core's answer replaces it, or it goes again.
    func watch(_ phrase: String, personId: String? = nil, personName: String? = nil) {
        let phrase = phrase.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !phrase.isEmpty else { return }
        let already = watches.contains {
            $0.phrase.caseInsensitiveCompare(phrase) == .orderedSame && $0.personId == personId
        }
        guard !already else { return }
        let draft = WorkWatch(
            id: Self.newId(), phrase: phrase, personId: personId, personName: personName,
            createdAtMs: TaskDates.now(), hits: [])
        watches.append(draft)
        Task {
            do {
                let saved: WorkWatch = try await core.request(
                    "work_watch_save", WatchParams(phrase: phrase, personId: personId))
                if let index = watches.firstIndex(where: { $0.id == draft.id }) { watches[index] = saved }
            } catch {
                watches.removeAll { $0.id == draft.id }
                mutationError = error.localizedDescription
            }
        }
    }

    func unwatch(_ id: String) {
        guard let index = watches.firstIndex(where: { $0.id == id }) else { return }
        let gone = watches.remove(at: index)
        Task {
            do {
                try await core.request("work_watch_remove", IdParams(id: id))
            } catch {
                watches.insert(gone, at: min(index, watches.count))
                mutationError = error.localizedDescription
            }
        }
    }

    /// A Triage task from the line a watch heard; the hit keeps it.
    func task(from hit: WorkWatchHit) async -> WorkItem? {
        await queue?.value
        pending += 1
        defer { settle() }
        do {
            let item: WorkItem = try await core.request("work_watch_task", HitParams(hitId: hit.id))
            if var rows = items, !rows.contains(where: { $0.id == item.id }) {
                rows.append(row(for: item, beside: nil))
                items = rows
            }
            await loadWatches()
            return item
        } catch {
            mutationError = error.localizedDescription
            return nil
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
        case delegate([(id: String, agent: String?)])
        case unbook(String)
        case reattach(String, WorkAttachmentSource)
    }

    private func load() async {
        let core = core
        let query = QueryParams(query: Self.everything)
        do {
            async let list: WorkList = core.request("work_list", query)
            async let tally: WorkCounts = core.request("work_counts")
            async let saved: [WorkView] = core.request("work_views")
            async let cycled: WorkCycles = core.request("work_cycles")
            let (listed, counted, savedViews, cycleList) = try await (list, tally, saved, cycled)
            if pending > 0 {
                reloadWhenIdle = true
            } else {
                items = listed.items
                keepTime()
                labels = listed.labels
                projects = listed.projects
                people = listed.people
                suggestions = listed.suggestions
                proposals = listed.proposals
                conversationsAvailable = listed.conversationsAvailable
                counts = counted
                views = savedViews
                cycles = cycleList.cycles
                cycleSettings = cycleList.settings
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

    /// Sends each hand-over in its turn and copies the prompts the core
    /// wrote, a blank line apart.
    private func hand(_ ids: [String], to agent: String?, copying: Bool, undo: Int?) {
        let before = rows(ids)
        let now = ProjectsStore.milliseconds(.now)
        edit(Set(ids)) { row in row.doer = agent.map { WorkDoer(agent: $0, sinceMs: now) } }
        enqueue {
            var prompts: [String] = []
            for (index, row) in before.enumerated() {
                do {
                    let handed: WorkDelegation = try await self.core.request(
                        "work_delegate", DelegateParams(operationId: Self.newId(), id: row.id, agent: agent))
                    self.edit([row.id]) { $0.doer = handed.doer }
                    if let prompt = handed.prompt { prompts.append(prompt) }
                } catch {
                    self.putBack(Array(before[index...]))
                    self.forget(undo)
                    self.mutationError = error.localizedDescription
                    break
                }
            }
            if copying, !prompts.isEmpty {
                let board = NSPasteboard.general
                board.clearContents()
                if !board.setString(prompts.joined(separator: "\n\n"), forType: .string) {
                    self.mutationError = "The prompt could not be copied."
                }
            }
            self.reloadWhenIdle = true
        }
    }

    /// Takes proposals off the screen at once; returns them, oldest first.
    private func takeProposals(_ ids: [String]) -> [WorkProposal] {
        let wanted = Set(ids)
        var taken: [String: WorkProposal] = [:]
        for (item, open) in proposals where open.contains(where: { wanted.contains($0.id) }) {
            for proposal in open where wanted.contains(proposal.id) { taken[proposal.id] = proposal }
            let kept = open.filter { !wanted.contains($0.id) }
            proposals[item] = kept.isEmpty ? nil : kept
        }
        if var shown = detail, shown.proposals.contains(where: { wanted.contains($0.id) }) {
            for proposal in shown.proposals where wanted.contains(proposal.id) { taken[proposal.id] = proposal }
            shown.proposals.removeAll { wanted.contains($0.id) }
            detail = shown
        }
        return Self.oldestFirst(Array(taken.values))
    }

    /// Puts back proposals whose answer the core refused.
    private func returnProposals(_ taken: [WorkProposal]) {
        for (item, back) in Dictionary(grouping: taken, by: \.itemId) {
            proposals[item] = Self.oldestFirst((proposals[item] ?? []) + back)
            if var shown = detail, shown.item.id == item {
                shown.proposals = Self.oldestFirst(shown.proposals + back)
                detail = shown
            }
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
            keepTime()
        }
        if var shown = detail, ids.contains(shown.item.id) {
            transform(&shown.item)
            detail = shown
        }
    }

    /// Sets `now` and sleeps until the next moment a list changes by
    /// itself: the earliest snooze ending or wait coming due, or midnight.
    /// The counts are the core's, so they are read again then.
    private func keepTime() {
        let now = TaskDates.now()
        self.now = now
        let moments = (items ?? []).flatMap { [$0.snoozedUntilMs, $0.followUpAtMs] }.compactMap { $0 }
        let wake = min(moments.filter { $0 > now }.min() ?? .max, TaskDates.day(1, hour: 0))
        alarm?.cancel()
        alarm = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(wake - now))
            guard !Task.isCancelled, let self else { return }
            keepTime()
            Task { await self.load() }
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
            revision: item.revision, sortOrder: item.sortOrder, cycle: item.cycle, parentId: item.parentId,
            subTasks: old?.subTasks,
            projectTitle: projectTitle(item.projectId),
            ownerName: personName(item.ownerPersonId), doer: old?.doer, sourceMeetingId: old?.sourceMeetingId,
            sourceMeetingTitle: old?.sourceMeetingTitle, sourceQuote: old?.sourceQuote,
            sourceSpeaker: old?.sourceSpeaker, sourceAtMs: old?.sourceAtMs,
            blockedByOpenCount: old?.blockedByOpenCount ?? 0, raisedIn: old?.raisedIn ?? 0,
            source: old?.source ?? .person, followUpAtMs: old?.followUpAtMs,
            atRisk: item.status.isOpen && (old?.atRisk ?? false), timeBlock: old?.timeBlock, repeat: old?.repeat,
            attachmentCount: old?.attachmentCount ?? 0)
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
        if let cycle = patch.cycle { row.cycle = cycle.value }
        if let parent = patch.parentId { row.parentId = parent.value }
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
        if patch.cycle != nil { inverse.cycle = clearable(row.cycle) }
        if patch.parentId != nil { inverse.parentId = clearable(row.parentId) }
        return inverse
    }

    private static func clearable<T: Equatable>(_ value: T?) -> WorkPatchValue<T> {
        value.map { .set($0) } ?? .clear
    }

    private static func counted(_ verb: String, _ count: Int) -> String {
        count == 1 ? "\(verb) 1 task" : "\(verb) \(count) tasks"
    }

    /// "Accepted Claude Code's change", or how many when there were more.
    private static func accepted(_ taken: [WorkProposal]) -> String {
        let changes = taken.count == 1 ? "change" : "\(taken.count) changes"
        let agents = Set(taken.map(\.agent))
        guard agents.count == 1, let agent = agents.first else { return "Accepted \(changes)" }
        return "Accepted \(agent)'s \(changes)"
    }

    /// What accepting a proposal sets, read from the text history keeps.
    private static func apply(_ proposal: WorkProposal, to patch: inout WorkPatch) {
        let to = proposal.to.flatMap { $0.isEmpty ? nil : $0 }
        switch proposal.field {
        case .title: patch.title = to
        case .details: patch.details = clearable(to)
        case .status: patch.status = to.flatMap(WorkStatus.init(rawValue:))
        case .priority: patch.priority = to.flatMap { Int($0) }
        case .owner:
            patch.owner = switch to {
            case "self"?: .myself
            case "nobody"?, nil: .nobody
            case let id?: .person(id)
            }
        case .due: patch.dueAtMs = clearable(to.flatMap { Int64($0) })
        case .project: patch.projectId = clearable(to)
        case .milestone: patch.milestoneId = clearable(to)
        case .labels: patch.labelIds = to?.split(separator: ",").map(String.init) ?? []
        case .snoozed: patch.snoozedUntilMs = clearable(to.flatMap { Int64($0) })
        case .recurrence: patch.recurrence = clearable(to)
        case .cycle: patch.cycle = clearable(to.flatMap { Int64($0) })
        case .parent: patch.parentId = clearable(to)
        case .unknown: break
        }
    }

    private static func oldestFirst(_ proposals: [WorkProposal]) -> [WorkProposal] {
        proposals.sorted { ($0.createdAtMs, $0.id) < ($1.createdAtMs, $1.id) }
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
        if let cycle = patch.cycle {
            return cycle.value.map { "Moved \(noun) to cycle \($0)" } ?? "Took \(noun) out of its cycle"
        }
        if let parent = patch.parentId {
            return parent.value == nil ? "Took \(noun) out of its parent" : "Moved \(noun) under a parent"
        }
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
private struct CycleParams: Encodable { let number: Int64 }
private struct CycleSettingsParams: Encodable {
    let operationId: String
    let settings: WorkCycleSettings
}
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
private struct CommentAddParams: Encodable {
    let operationId: String
    let itemId: String
    let body: String
    let replyTo: String?
}
private struct CommentEditParams: Encodable {
    let operationId: String
    let id: String
    let body: String
}
private struct CommentResolveParams: Encodable {
    let operationId: String
    let id: String
    let resolved: Bool
}
private struct SubtaskParams: Encodable {
    let operationId: String
    let parentId: String
    let inputs: [WorkItemInput]
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
private struct DelegateParams: Encodable {
    let operationId: String
    let id: String
    let agent: String?
}
private struct ParseParams: Encodable {
    let text: String
    let plain: [String]
    let spoken: Bool
}
private struct CaptureParams: Encodable {
    let operationId: String
    let input: WorkItemInput
    let source: LinkSource
}
private struct LinkSource: Encodable {
    let kind = "link"
    let url: String
    let title: String?
}
private struct CommandParams: Encodable {
    let text: String
    let selection: [String]
}
private struct QuestionParams: Encodable { let question: String }
private struct TakeParams: Encodable {
    let operationId: String
    let lines: [WorkTakenLine]
}
private struct WatchParams: Encodable {
    let phrase: String
    let personId: String?
}
private struct HitParams: Encodable { let hitId: String }
private struct AttachmentRenameParams: Encodable {
    let operationId: String
    let id: String
    let title: String
}
private struct AttachParams: Encodable {
    let operationId: String
    let itemId: String
    var path: String?
    var data: String?
    var fileName: String?
    var url: String?
    var title: String?
}

enum WorkAttachmentSource {
    case file(URL, title: String? = nil)
    case data(Data, fileName: String)
    case link(String, title: String? = nil)

    static let linkSchemes: Set<String> = ["http", "https", "mailto", "sona"]

    fileprivate var method: String {
        switch self {
        case .file, .data: "work_attachment_attach_file"
        case .link: "work_attachment_attach_link"
        }
    }

    fileprivate func params(itemId: String, operationId: String) -> AttachParams {
        switch self {
        case let .file(url, title):
            AttachParams(operationId: operationId, itemId: itemId, path: url.path(percentEncoded: false), title: title)
        case let .data(data, fileName):
            AttachParams(operationId: operationId, itemId: itemId, data: data.base64EncodedString(), fileName: fileName)
        case let .link(url, title):
            AttachParams(operationId: operationId, itemId: itemId, url: url, title: title)
        }
    }
}

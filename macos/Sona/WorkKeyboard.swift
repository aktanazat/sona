import AppKit
import SwiftUI

/// A list of choices the palette panel shows: a status, an owner or a date
/// for the tasks it names, or, with no tasks, a recent task, a person or a
/// label to go to.
struct WorkPicker: Equatable {
    enum Kind: Equatable {
        case status, priority, owner, due, labels, project, milestone, duplicate, snooze, remind, decline
        case delegate, cycle, blockedBy, blocks, related, parent, estimate, forPerson
        case recent, person, label
    }

    let kind: Kind
    let ids: [String]
}

/// Everything a person can do to tasks from the keyboard, the palette, a
/// row's menu or the bulk bar, with the one key each answers to.
enum WorkAction: String, CaseIterable, Identifiable {
    case open, peek, status, priority, owner, assignToMe, due, labels, project, milestone, rename
    case comment, history
    case accept, decline, duplicate, snooze, remind, nudge, archive, restore, delete
    case delegate, acceptProposal, acceptAllProposals, rejectProposal
    case cycle, block, estimate, forPerson
    case blockedBy, blocks, related, parent, removeParent

    var id: String { rawValue }

    var title: String {
        switch self {
        case .open: "Open"
        case .peek: "Peek"
        case .status: "Change status…"
        case .priority: "Set priority…"
        case .owner: "Assign to…"
        case .assignToMe: "Assign to me"
        case .due: "Set due date…"
        case .labels: "Change labels…"
        case .project: "Move to project…"
        case .milestone: "Set milestone…"
        case .rename: "Rename"
        case .comment: "Comment…"
        case .history: "Description history"
        case .accept: "Accept"
        case .decline: "Decline…"
        case .duplicate: "Mark as duplicate…"
        case .snooze: "Snooze…"
        case .remind: "Remind me…"
        case .nudge: "Nudge…"
        case .archive: "Archive"
        case .restore: "Restore"
        case .delete: "Delete"
        case .delegate: "Delegate to…"
        case .acceptProposal: "Accept proposed change"
        case .acceptAllProposals: "Accept all proposed changes"
        case .rejectProposal: "Reject proposed change"
        case .cycle: "Move to cycle…"
        case .block: "Book time"
        case .estimate: "Set estimate…"
        case .forPerson: "Set who it’s for…"
        case .blockedBy: "Blocked by…"
        case .blocks: "Blocks…"
        case .related: "Related to…"
        case .parent: "Set parent…"
        case .removeParent: "Remove parent"
        }
    }

    /// In the form `Shortcut` draws.
    var keys: String {
        switch self {
        case .open: "return"
        case .peek: "space"
        case .status: "s"
        case .priority: "p"
        case .owner: "a"
        case .assignToMe: "i"
        case .due: "shift+d"
        case .labels: "l"
        case .project: "shift+p"
        case .milestone: "shift+m"
        case .rename: "e"
        case .comment, .history: ""
        case .accept: "1"
        case .decline: "2"
        case .duplicate: "3"
        case .snooze, .remind: "h"
        case .nudge: "r"
        case .archive, .restore: ""
        case .delete: "cmd+backspace"
        case .delegate: "shift+cmd+;"
        case .acceptProposal: "y"
        case .acceptAllProposals: "shift+y"
        case .rejectProposal: "n"
        case .cycle: "shift+c"
        case .block: "b"
        case .estimate: "shift+e"
        case .forPerson: ""
        case .blockedBy: "m b"
        case .blocks: "m x"
        case .related: "m r"
        case .parent: "m p"
        case .removeParent: ""
        }
    }

    /// The same key as a menu's equivalent, so a row's menu shows it. Keys
    /// pressed one after another have none, and neither has Snooze: H snoozes
    /// only Triage's tasks and reminds about the rest.
    var equivalent: KeyboardShortcut? {
        switch self {
        case .open: KeyboardShortcut(.return, modifiers: [])
        case .peek: KeyboardShortcut(.space, modifiers: [])
        case .status: KeyboardShortcut("s", modifiers: [])
        case .priority: KeyboardShortcut("p", modifiers: [])
        case .owner: KeyboardShortcut("a", modifiers: [])
        case .assignToMe: KeyboardShortcut("i", modifiers: [])
        case .due: KeyboardShortcut("d", modifiers: .shift)
        case .labels: KeyboardShortcut("l", modifiers: [])
        case .project: KeyboardShortcut("p", modifiers: .shift)
        case .milestone: KeyboardShortcut("m", modifiers: .shift)
        case .rename: KeyboardShortcut("e", modifiers: [])
        case .accept: KeyboardShortcut("1", modifiers: [])
        case .decline: KeyboardShortcut("2", modifiers: [])
        case .duplicate: KeyboardShortcut("3", modifiers: [])
        case .remind: KeyboardShortcut("h", modifiers: [])
        case .nudge: KeyboardShortcut("r", modifiers: [])
        case .delete: KeyboardShortcut(.delete, modifiers: .command)
        case .delegate: KeyboardShortcut(";", modifiers: [.command, .shift])
        case .acceptProposal: KeyboardShortcut("y", modifiers: [])
        case .acceptAllProposals: KeyboardShortcut("y", modifiers: .shift)
        case .rejectProposal: KeyboardShortcut("n", modifiers: [])
        case .cycle: KeyboardShortcut("c", modifiers: .shift)
        case .block: KeyboardShortcut("b", modifiers: [])
        case .estimate: KeyboardShortcut("e", modifiers: .shift)
        case .blockedBy, .blocks, .related, .parent, .removeParent, .archive, .restore, .snooze, .comment, .history,
             .forPerson: nil
        }
    }

    var icon: String {
        switch self {
        case .open: "arrow.up.right.square"
        case .peek: "sidebar.right"
        case .status: "circle.dashed"
        case .priority: "chart.bar"
        case .owner: "person.crop.circle"
        case .assignToMe: "person.crop.circle.badge.checkmark"
        case .due: "calendar"
        case .labels: "tag"
        case .project: "folder"
        case .milestone: "flag"
        case .rename: "pencil"
        case .comment: "text.bubble"
        case .history: "clock.arrow.circlepath"
        case .accept: "checkmark.circle"
        case .decline: "xmark.circle"
        case .duplicate: "square.on.square"
        case .snooze: "moon.zzz"
        case .remind: "bell.badge"
        case .nudge: "arrowshape.turn.up.right"
        case .archive: "archivebox"
        case .restore: "arrow.uturn.backward"
        case .delete: "trash"
        case .delegate: "sparkle"
        case .acceptProposal: "checkmark"
        case .acceptAllProposals: "checklist.checked"
        case .rejectProposal: "xmark"
        case .cycle: "arrow.triangle.2.circlepath"
        case .block: "calendar.badge.clock"
        case .estimate: "triangle"
        case .forPerson: "person.crop.circle.badge.checkmark"
        case .blockedBy: "exclamationmark.octagon"
        case .blocks: "hand.raised"
        case .related: "arrow.left.arrow.right"
        case .parent: "arrow.turn.left.up"
        case .removeParent: "arrow.uturn.down"
        }
    }

    var picker: WorkPicker.Kind? {
        switch self {
        case .status: .status
        case .priority: .priority
        case .owner: .owner
        case .due: .due
        case .labels: .labels
        case .project: .project
        case .milestone: .milestone
        case .duplicate: .duplicate
        case .snooze: .snooze
        case .remind: .remind
        case .decline: .decline
        case .open, .peek, .assignToMe, .rename, .comment, .history, .accept, .nudge, .archive, .restore, .delete, .block,
             .removeParent: nil
        case .delegate: .delegate
        case .cycle: .cycle
        case .blockedBy: .blockedBy
        case .blocks: .blocks
        case .related: .related
        case .parent: .parent
        case .estimate: .estimate
        case .forPerson: .forPerson
        case .acceptProposal, .acceptAllProposals, .rejectProposal: nil
        }
    }

    /// Whether the verb means anything for these tasks; the palette and the
    /// menus offer only the ones that do. `proposed` holds the tasks with a
    /// proposed change waiting.
    func applies(to rows: [WorkItemRow], proposed: Set<String> = []) -> Bool {
        guard let first = rows.first else { return false }
        return switch self {
        case .open, .peek, .rename, .comment, .history: rows.count == 1
        case .duplicate: rows.count == 1 && first.status != .duplicate
        case .assignToMe: rows.contains { !$0.ownerIsSelf }
        case .accept, .decline: rows.allSatisfy { $0.status == .triage }
        case .snooze: rows.allSatisfy { $0.status == .triage || $0.status.isOpen }
        case .remind: rows.allSatisfy { $0.status != .triage }
        case .nudge: rows.count == 1 && first.status.isOpen && first.followUpAtMs != nil
        case .archive: rows.contains { $0.archivedAtMs == nil }
        case .restore: rows.contains { $0.archivedAtMs != nil }
        case .delete: true
        case .status, .priority, .owner, .due, .labels, .project: true
        case .milestone: first.projectId != nil && rows.allSatisfy { $0.projectId == first.projectId }
        case .delegate, .estimate, .forPerson: true
        case .cycle: rows.allSatisfy { $0.status.isOpen }
        case .block: rows.count == 1 && first.status.isOpen && first.timeBlock == nil
        case .blockedBy, .blocks, .related: true
        case .parent: rows.count == 1
        case .removeParent: rows.contains { $0.parentId != nil }
        case .acceptProposal, .acceptAllProposals, .rejectProposal: rows.contains { proposed.contains($0.id) }
        }
    }
}

/// What a task copies as: the ID agents and the command line take, a Sona
/// link, Markdown to paste into a note or a message, or a link that opens a
/// new task filled in like this one.
@MainActor
enum TaskCopy: String, CaseIterable, Identifiable {
    case handle, link, markdown, createLink

    var id: String { rawValue }

    var title: String {
        switch self {
        case .handle: "Copy ID"
        case .link: "Copy link"
        case .markdown: "Copy as Markdown"
        case .createLink: "Copy create link"
        }
    }

    var icon: String {
        switch self {
        case .handle: "number"
        case .link: "link"
        case .markdown: "doc.plaintext"
        case .createLink: "plus.square.on.square"
        }
    }

    static func handle(_ row: WorkItemRow) -> String { "S-\(row.number)" }

    static func link(_ row: WorkItemRow) -> String { "sona://task/\(row.id)" }

    static func commentLink(task: String, comment: String) -> String { "sona://task/\(task)?comment=\(comment)" }

    /// The task a `sona://task/<id>` link names.
    static func task(in link: String) -> String? {
        guard let url = URL(string: link), url.scheme?.lowercased() == "sona", url.host() == "task" else { return nil }
        return url.pathComponents.dropFirst().first
    }

    /// The comment a `sona://task/<id>?comment=<id>` link names.
    static func comment(in link: String) -> String? {
        URLComponents(string: link)?.queryItems?.first { $0.name == "comment" }?.value
    }

    /// `sona://new?title=…&details=…&priority=2&project=…&labels=a,b&owner=me&due=friday`:
    /// a new task's fields. Project, labels and owner take an id or a name;
    /// due takes a day or words.
    static func createLink(_ row: WorkItemRow) -> String {
        var parts = URLComponents()
        parts.scheme = "sona"
        parts.host = "new"
        var items = [URLQueryItem(name: "title", value: row.title)]
        if let details = row.details, !details.isEmpty { items.append(URLQueryItem(name: "details", value: details)) }
        if row.priority != 0 { items.append(URLQueryItem(name: "priority", value: "\(row.priority)")) }
        if let project = row.projectId { items.append(URLQueryItem(name: "project", value: project)) }
        if !row.labelIds.isEmpty { items.append(URLQueryItem(name: "labels", value: row.labelIds.joined(separator: ","))) }
        let owner = switch row.owner {
        case .myself: "me"
        case let .person(id): id
        case .nobody: "none"
        }
        items.append(URLQueryItem(name: "owner", value: owner))
        if let due = row.dueAtMs { items.append(URLQueryItem(name: "due", value: RitualStore.day(PlanningFormat.moment(due)))) }
        parts.queryItems = items
        return parts.string ?? "sona://new"
    }

    /// The fields a `sona://new` link fills in; nil for any other link.
    static func draft(in link: String, work: WorkStore) -> WorkPatch? {
        guard let parts = URLComponents(string: link), parts.scheme?.lowercased() == "sona", parts.host == "new" else { return nil }
        var fields: [String: String] = [:]
        for item in parts.queryItems ?? [] {
            if let value = item.value?.trimmingCharacters(in: .whitespacesAndNewlines), !value.isEmpty {
                fields[item.name.lowercased()] = value
            }
        }
        var patch = WorkPatch(status: .notStarted, owner: .myself)
        patch.title = fields["title"]
        if let details = fields["details"] ?? fields["description"] { patch.details = .set(details) }
        if let priority = fields["priority"].flatMap(Int.init), (0...4).contains(priority) { patch.priority = priority }
        if let project = fields["project"],
           let found = work.projects.first(where: { $0.id == project || $0.title.localizedCaseInsensitiveCompare(project) == .orderedSame }) {
            patch.projectId = .set(found.id)
        }
        if let labels = fields["labels"] {
            let ids = labels.split(separator: ",").compactMap { name in
                let name = name.trimmingCharacters(in: .whitespaces)
                return work.labels.first { $0.id == name || $0.name.localizedCaseInsensitiveCompare(name) == .orderedSame }?.id
            }
            if !ids.isEmpty { patch.labelIds = ids }
        }
        if let owner = fields["owner"] ?? fields["assignee"] {
            switch owner.lowercased() {
            case "me": patch.owner = .myself
            case "none", "nobody": patch.owner = .nobody
            default:
                if let person = work.people.first(where: { $0.id == owner || $0.name.localizedCaseInsensitiveCompare(owner) == .orderedSame }) {
                    patch.owner = .person(person.id)
                }
            }
        }
        if let due = fields["due"] {
            let day = CycleFormat.day(due).map { TaskDates.day(0, from: $0) } ?? TaskDates.parse(due)
            if let day { patch.dueAtMs = .set(day) }
        }
        return patch
    }

    func text(_ rows: [WorkItemRow]) -> String {
        switch self {
        case .handle: rows.map(Self.handle).joined(separator: "\n")
        case .link: rows.map(Self.link).joined(separator: "\n")
        case .markdown: rows.map(Self.markdown).joined(separator: "\n\n")
        case .createLink: rows.map(Self.createLink).joined(separator: "\n")
        }
    }

    func done(_ rows: [WorkItemRow]) -> String {
        guard rows.count == 1, let row = rows.first else {
            return switch self {
            case .handle: "Copied \(rows.count) IDs"
            case .link: "Copied \(rows.count) links"
            case .markdown: "Copied \(rows.count) tasks as Markdown"
            case .createLink: "Copied \(rows.count) create links"
            }
        }
        return switch self {
        case .handle: "Copied \(Self.handle(row))"
        case .link: "Copied the link to \(Self.handle(row))"
        case .markdown: "Copied \(Self.handle(row)) as Markdown"
        case .createLink: "Copied a link that creates a task like \(Self.handle(row))"
        }
    }

    private static func markdown(_ row: WorkItemRow) -> String {
        var lines = ["## \(handle(row)) \(row.title)", "", "- Status: \(row.status.label)"]
        if let owner = row.ownerIsSelf ? "Me" : row.ownerName { lines.append("- Owner: \(owner)") }
        if let project = row.projectTitle { lines.append("- Project: \(project)") }
        if let due = row.dueAtMs { lines.append("- Due: \(WorkChoices.long(due))") }
        if let details = row.details?.trimmingCharacters(in: .whitespacesAndNewlines), !details.isEmpty {
            lines += ["", details]
        }
        if let quote = row.sourceQuote {
            lines.append("")
            lines += quote.split(separator: "\n", omittingEmptySubsequences: false).map { "> \($0)" }
            let heard = [row.sourceSpeaker, row.sourceMeetingTitle.map { "in \($0)" }].compactMap { $0 }
            if !heard.isEmpty { lines += [">", "> \(heard.joined(separator: " "))"] }
        }
        return lines.joined(separator: "\n")
    }
}

enum TasksTab: Hashable {
    case owe, waiting, all, cycle
    case view(String)
}

/// A part of an open task's page that a key or the palette asked for: the
/// page acts on it once, then clears it.
struct TaskPageAsk: Equatable {
    enum Part: Equatable {
        case composer, subtask, link, history
        case comment(String)
    }

    let id: String
    let part: Part
}

/// A saved view's filters and display as edited, before they are saved.
struct WorkViewDraft: Equatable {
    var query: WorkQuery
    var display: WorkDisplay
}

/// What the task screens point at: the one highlighted row that the pointer
/// and J/K share, the selection, the open task, and the panels over them.
@MainActor
@Observable
final class WorkFocus {
    var highlighted: String?
    /// The inbox notice J and K are on.
    var inboxHighlighted: String?
    /// The inbox shows snoozed notices instead of awake ones.
    var inboxSnoozed = false
    /// The archive row J/K or the pointer are on, for # to restore.
    var archiveHighlighted: String?
    private(set) var selection: Set<String> = []
    var peeking = false
    @ObservationIgnored var propertyFocused = false
    /// The task open on each place's page; Tasks and Triage keep their own.
    private(set) var openedIn: [Place: String] = [:]
    /// The person whose page shows their tasks now; while it does, the task
    /// keys work on People as they do on Tasks.
    var personList: String?
    /// The project whose Tasks tab shows; while it does, the task keys work
    /// on Projects as they do on Tasks.
    var projectList: String?
    var pageAsk: TaskPageAsk?
    /// ⌘I hid the property panel beside a task's or a project's page.
    var panelHidden = false
    /// Tasks opened lately, the latest first, for O I.
    private(set) var recent: [String] = []
    var picker: WorkPicker?
    var renaming: String?
    /// The new-task sheet, holding what the task starts with.
    var composing: WorkPatch?
    /// ⌥C: the new-task sheet opens on its template list.
    var composingFromTemplate = false
    /// The milestones of the project the milestone picker's tasks share.
    var milestones: [PlanningMilestone] = []
    var shortcutsShown = false
    var displayShown = false
    var collapsed: Set<String> = []
    var tab: TasksTab = .owe
    /// I owe narrowed to the current cycle.
    var oweThisCycle = false
    /// The cycle the Cycle tab shows; nil shows the one under way.
    var cycleOpened: Int64?
    /// The Cycle tab lists every cycle instead of one cycle's page.
    var cycleListShown = false
    /// The cycle row J and K are on in the cycle list.
    var cycleHighlighted: Int64?
    /// The person row J and K are on in the People list.
    var personHighlighted: String?
    var allQuery = WorkQuery()
    var allDisplay = WorkDisplay(properties: TaskProperty.standard)
    var filterShown = false
    var myProperties = TaskProperty.standard
    var viewDrafts: [String: WorkViewDraft] = [:]
    /// Each project's Tasks tab keeps its own filters and display.
    var projectDrafts: [String: WorkViewDraft] = [:]
    /// Which source Triage shows; nil shows all of it.
    var triageSplit: WorkSource?
    /// The change the undo toast offers to take back, and a count that
    /// restarts its timer when the same words come twice.
    var toast: String?
    /// Off for a toast that only says what happened, such as a copy.
    private(set) var toastUndoes = true
    private(set) var toastStamp = 0
    /// The row keys moved to, for the list to bring into view.
    private(set) var scrollTarget: String?
    /// The quote whose moment is playing, so only its button turns to Stop
    /// when several quotes come from one meeting.
    var playingMoment: String?
    /// Rows on screen, top to bottom, as the list last drew them.
    @ObservationIgnored private(set) var visible: [String] = []
    @ObservationIgnored private var anchor: String?
    /// Where the pointer was when keys last moved the highlight: rows that
    /// scroll under a still pointer must not take the highlight back.
    @ObservationIgnored private var pointerAtKeys: NSPoint?
    /// A task just made, which takes the highlight once the list shows it.
    @ObservationIgnored private var arriving: String?

    var selecting: Bool { !selection.isEmpty }

    func opened(in place: Place) -> String? { openedIn[place] }

    func open(_ id: String?, in place: Place) {
        openedIn[place] = id
        guard let id else { return }
        recent = [id] + recent.filter { $0 != id }.prefix(19)
    }

    /// What a verb acts on: the open task, else the selection, else the
    /// highlighted row.
    func targets(in place: Place) -> [String] {
        if let opened = openedIn[place] { return [opened] }
        if !selection.isEmpty { return visible.filter(selection.contains) }
        return highlighted.map { [$0] } ?? []
    }

    func announce(_ title: String?, undoes: Bool = true) {
        toast = title
        toastUndoes = undoes
        toastStamp += 1
    }

    func show(_ ids: [String]) {
        visible = ids
        selection.formIntersection(ids)
        if let anchor, !ids.contains(anchor) { self.anchor = nil }
        if let arriving, ids.contains(arriving) { highlighted = arriving }
        arriving = nil
        if let highlighted, !ids.contains(highlighted) { self.highlighted = nil }
    }

    func highlightWhenShown(_ id: String) {
        arriving = id
    }

    func move(_ delta: Int, extend: Bool) {
        guard let first = visible.first, let last = visible.last else { return }
        let next: String
        if let current = highlighted, let index = visible.firstIndex(of: current) {
            next = visible[min(max(index + delta, 0), visible.count - 1)]
        } else {
            next = delta > 0 ? first : last
        }
        if extend {
            let start = anchor ?? highlighted ?? next
            selectRange(from: start, to: next)
        }
        highlighted = next
        scrollTarget = next
        pointerAtKeys = NSEvent.mouseLocation
    }

    func hover(_ id: String) {
        guard pointerMoved() else { return }
        highlighted = id
    }

    func hoverPerson(_ id: String) {
        guard pointerMoved() else { return }
        personHighlighted = id
    }

    /// J or K on the People list, which the still pointer must not take back.
    func keyPerson(_ id: String) {
        personHighlighted = id
        pointerAtKeys = NSEvent.mouseLocation
    }

    private func pointerMoved() -> Bool {
        let pointer = NSEvent.mouseLocation
        if let pointerAtKeys, pointerAtKeys == pointer { return false }
        pointerAtKeys = nil
        return true
    }

    func toggle(_ id: String) {
        if selection.contains(id) { selection.remove(id) } else { selection.insert(id) }
        anchor = id
        highlighted = id
    }

    func extendSelection(to id: String) {
        selectRange(from: anchor ?? highlighted ?? id, to: id)
        highlighted = id
    }

    /// ⇧X: selects from the last row picked to the highlighted one.
    func extendSelectionToHighlight() -> Bool {
        guard let highlighted else { return false }
        if anchor == nil, selection.isEmpty {
            toggle(highlighted)
        } else {
            selectRange(from: anchor ?? highlighted, to: highlighted)
        }
        return true
    }

    func selectAll() {
        selection = Set(visible)
    }

    func clearSelection() {
        selection = []
        anchor = nil
    }

    func switchTab(_ tab: TasksTab) {
        guard self.tab != tab else { return }
        self.tab = tab
        clearSelection()
        peeking = false
        highlighted = nil
    }

    func split(_ source: WorkSource?) {
        guard triageSplit != source else { return }
        triageSplit = source
        clearSelection()
        peeking = false
        highlighted = nil
    }

    /// ⇥ and ⇧⇥ on Triage: the next or previous split, round the end.
    func cycleSplit(by delta: Int) {
        let splits: [WorkSource?] = [nil] + WorkSource.allCases
        let index = splits.firstIndex(of: triageSplit) ?? 0
        split(splits[(index + delta + splits.count) % splits.count])
    }

    /// Esc: the innermost thing open closes first.
    func escape(in place: Place) -> Bool {
        if displayShown || filterShown {
            displayShown = false
            filterShown = false
        } else if renaming != nil {
            renaming = nil
        } else if peeking {
            peeking = false
        } else if !selection.isEmpty {
            clearSelection()
        } else if openedIn[place] != nil {
            openedIn[place] = nil
        } else {
            return false
        }
        return true
    }

    /// The row the highlight lands on once `ids` leave the list.
    func successor(of ids: [String]) -> String? {
        guard let last = ids.compactMap({ visible.firstIndex(of: $0) }).max() else { return nil }
        let leaving = Set(ids)
        return visible[(last + 1)...].first { !leaving.contains($0) }
            ?? visible[..<last].last { !leaving.contains($0) }
    }

    func listing(_ views: [WorkView]) -> WorkViewDraft? {
        switch tab {
        case .owe, .waiting, .cycle:
            return nil
        case .all:
            return WorkViewDraft(query: allQuery, display: allDisplay)
        case let .view(id):
            if let draft = viewDrafts[id] { return draft }
            return views.first { $0.id == id }.map { WorkViewDraft(query: $0.query, display: $0.display) }
        }
    }

    func setListing(_ draft: WorkViewDraft, views: [WorkView]) {
        switch tab {
        case .owe, .waiting, .cycle:
            break
        case .all:
            allQuery = draft.query
            allDisplay = draft.display
        case let .view(id):
            let saved = views.first { $0.id == id }.map { WorkViewDraft(query: $0.query, display: $0.display) }
            viewDrafts[id] = draft == saved ? nil : draft
        }
    }

    func projectListing(_ project: String) -> WorkViewDraft {
        projectDrafts[project] ?? WorkViewDraft(
            query: WorkQuery(),
            display: WorkDisplay(properties: TaskProperty.standard.filter { $0 != TaskProperty.project.rawValue }))
    }

    private func selectRange(from start: String, to end: String) {
        guard let a = visible.firstIndex(of: start), let b = visible.firstIndex(of: end) else { return }
        selection.formUnion(visible[min(a, b)...max(a, b)])
        anchor = start
    }
}

/// The task screens' keys, read before any view sees them, so a key works
/// wherever focus sits in the window, and never while text is being typed.
@MainActor
final class WorkKeys {
    private var monitor: Any?
    private var goPressedAt: TimeInterval?
    private var openPressedAt: TimeInterval?
    private var relatePressedAt: TimeInterval?
    private static let destinations: [String: Place] = [
        "h": .capture, "d": .today, "m": .meetings, "t": .tasks, "i": .triage, "p": .people, "j": .projects,
    ]
    /// O then a letter: a recent task, a person or a label to go to.
    private static let openers: [String: WorkPicker.Kind] = ["i": .recent, "u": .person, "l": .label]
    private static let relations: [String: WorkAction] = [
        "b": .blockedBy, "x": .blocks, "r": .related, "m": .duplicate, "p": .parent,
    ]

    func install(_ model: AppModel) {
        guard monitor == nil else { return }
        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self, weak model] event in
            let handled = MainActor.assumeIsolated {
                guard let self, let model else { return false }
                return self.handle(event, model)
            }
            return handled ? nil : event
        }
    }

    private func handle(_ event: NSEvent, _ model: AppModel) -> Bool {
        guard let window = event.window, window.canBecomeMain, !window.isSheet,
              window.attachedSheet == nil, model.ready, model.onboarding.step == .done,
              model.sheet == nil, !model.paletteShown
        else { return false }
        if window.firstResponder is NSText || window.firstResponder is NSTextField { return false }
        let flags = event.modifierFlags.intersection([.command, .option, .control, .shift])
        let key = (event.charactersIgnoringModifiers ?? "").lowercased()
        if let pressed = goPressedAt {
            goPressedAt = nil
            if event.timestamp - pressed < 1.5, flags.isEmpty, let place = Self.destinations[key] {
                model.workFocus.peeking = false
                model.go(place)
                return true
            }
        }
        if let pressed = openPressedAt {
            openPressedAt = nil
            if event.timestamp - pressed < 1.5, flags.isEmpty, let kind = Self.openers[key] {
                model.workFocus.picker = WorkPicker(kind: kind, ids: [])
                model.paletteShown = true
                return true
            }
        }
        if flags.isEmpty, key == "g" || key == "o" {
            if key == "g" { goPressedAt = event.timestamp } else { openPressedAt = event.timestamp }
            return true
        }
        guard !model.showingSettings else { return false }
        if flags.isEmpty, key == "/" {
            model.toggleSearch()
            return true
        }
        if model.place == .inbox, model.workFocus.opened(in: .inbox) == nil {
            return InboxKeys.handle(event, key: key, flags: flags, model)
        }
        if model.place == .today || model.place == .capture {
            return RitualKeys.handle(event, key: key, flags: flags, model)
        }
        if model.place == .meetings {
            return LiveCardKeys.handle(event, key: key, flags: flags, model)
        }
        if model.place == .archive {
            return archive(key: key, flags: flags, model)
        }
        if model.place == .projects, model.workFocus.opened(in: .projects) == nil,
           let project = model.projects.selectedProjectId {
            switch (flags, key) {
            case (.command, "i"):
                model.workFocus.panelHidden.toggle()
                return true
            case (.option, "f") where model.workFocus.targets(in: .projects).isEmpty:
                let on = !model.work.isFavorite(.project, project)
                model.work.setFavorite(.project, project, on)
                model.workFocus.announce(on ? "Added to favorites" : "Removed from favorites", undoes: false)
                return true
            default:
                break
            }
        }
        if model.place == .tasks, model.workFocus.tab == .cycle, model.workFocus.opened(in: .tasks) == nil,
           CycleKeys.handle(event, key: key, flags: flags, model) {
            return true
        }
        if model.place == .people, model.people.route == .list,
           PeopleListKeys.handle(event, key: key, flags: flags, model) {
            return true
        }
        guard model.showsTasks else { return false }
        return work(event, key: key, flags: flags, model)
    }

    /// The archive page: J/K move along it, # restores the row under them,
    /// ⌘Z takes the last change back.
    private func archive(key: String, flags: NSEvent.ModifierFlags, _ model: AppModel) -> Bool {
        let focus = model.workFocus
        let shown = model.work.archive.map { $0.deleted.map(\.item.id) + $0.archived.map(\.id) } ?? []
        switch (flags, key) {
        case (.command, "z"):
            guard model.work.undoTitle != nil else { return false }
            model.undoWork()
            return true
        case ([], "j"), ([], "k"):
            guard let first = shown.first, let last = shown.last else { return false }
            let index = focus.archiveHighlighted.flatMap(shown.firstIndex(of:))
            focus.archiveHighlighted = index.map { shown[min(max($0 + (key == "j" ? 1 : -1), 0), shown.count - 1)] }
                ?? (key == "j" ? first : last)
            return true
        case (.shift, "#"):
            guard let id = focus.archiveHighlighted, let index = shown.firstIndex(of: id) else { return false }
            focus.archiveHighlighted = index + 1 < shown.count ? shown[index + 1] : index > 0 ? shown[index - 1] : nil
            model.changeWork { model.work.restoreFromArchive(id) }
            return true
        default:
            return false
        }
    }

    private func work(_ event: NSEvent, key: String, flags: NSEvent.ModifierFlags, _ model: AppModel) -> Bool {
        let focus = model.workFocus
        let plain = flags.isEmpty
        let shift = flags == .shift
        let command = flags == .command
        let place = model.place
        let opened = focus.opened(in: place)
        let targets = focus.targets(in: place)
        let rows = model.workRows(targets)
        func run(_ action: WorkAction) -> Bool {
            guard action.applies(to: rows, proposed: model.workProposed(rows)) else { return false }
            model.perform(action, on: targets)
            return true
        }
        func answer(_ action: WorkAction) -> Bool {
            guard place == .triage, opened == nil else { return run(action) }
            guard !focus.collapsed.contains(TriageProposals.group), let first = model.triageProposed.first else { return false }
            model.perform(action, on: [first.id])
            return true
        }
        if event.keyCode == 53 {
            relatePressedAt = nil
            return focus.escape(in: place)
        }
        if focus.propertyFocused, plain || shift,
           [.upArrow, .downArrow, .carriageReturn, .enter].contains(event.specialKey) || key == " " {
            return false
        }
        if let pressed = relatePressedAt {
            relatePressedAt = nil
            if event.timestamp - pressed < 1.5, plain, let action = Self.relations[key] {
                return run(action)
            }
        }
        switch event.specialKey {
        case .downArrow?, .upArrow?:
            if flags == [.option, .shift], opened == nil {
                return model.moveToEdge(targets, top: event.specialKey == .upArrow)
            }
            guard plain || shift else { return false }
            let delta = event.specialKey == .downArrow ? 1 : -1
            if let opened {
                return model.step(from: opened, by: delta)
            }
            focus.move(delta, extend: shift)
            return true
        case .carriageReturn?, .enter?:
            guard plain, opened == nil else { return false }
            return run(.open)
        case .delete?:
            guard command else { return false }
            return run(.delete)
        case .tab?, .backTab?:
            guard place == .triage, opened == nil, plain || shift else { return false }
            focus.cycleSplit(by: event.specialKey == .backTab || shift ? -1 : 1)
            return true
        default:
            break
        }
        if command {
            switch key {
            case "z":
                guard model.work.undoTitle != nil else { return false }
                model.undoWork()
                return true
            case "a":
                guard opened == nil else { return false }
                focus.selectAll()
                return true
            case "b":
                return model.toggleWorkLayout()
            case "v":
                guard let opened else { return false }
                return model.attachPasteboard(to: opened)
            case "f":
                guard place == .tasks || model.listedProject != nil, opened == nil, model.listing != nil else { return false }
                focus.filterShown = true
                return true
            case "i":
                guard opened != nil else { return false }
                focus.panelHidden.toggle()
                return true
            default:
                return false
            }
        }
        if flags == [.command, .shift], key == ";" || key == ":" {
            return run(.delegate)
        }
        if flags == [.command, .shift], key == "o", let opened {
            focus.pageAsk = TaskPageAsk(id: opened, part: .subtask)
            return true
        }
        if flags == .control, key == "l", let opened {
            focus.pageAsk = TaskPageAsk(id: opened, part: .link)
            return true
        }
        if flags == .option {
            switch key {
            case "f":
                return model.toggleFavorite(targets)
            case "c" where place != .people && place != .projects:
                focus.composing = model.workDraftDefaults
                focus.composingFromTemplate = true
                return true
            default:
                return false
            }
        }
        if shift {
            switch key {
            case "p": return run(.project)
            case "m": return run(.milestone)
            case "c": return run(.cycle)
            case "e": return run(.estimate)
            case "y": return answer(.acceptAllProposals)
            case "d": return run(.due)
            case "s": return model.toggleSubscription(targets)
            case "x":
                guard opened == nil else { return false }
                return focus.extendSelectionToHighlight()
            case "v":
                guard place == .tasks || model.listedProject != nil, opened == nil else { return false }
                focus.displayShown.toggle()
                return true
            case "?", "/":
                focus.shortcutsShown = true
                return true
            default:
                return false
            }
        }
        guard plain else { return false }
        switch key {
        case "j", "k":
            let delta = key == "j" ? 1 : -1
            if let opened {
                return model.step(from: opened, by: delta)
            }
            focus.move(delta, extend: false)
            return true
        case "f":
            guard place == .tasks || model.listedProject != nil, opened == nil, model.listing != nil else { return false }
            focus.filterShown = true
            return true
        case "x":
            guard opened == nil, let id = focus.highlighted else { return false }
            focus.toggle(id)
            return true
        case " ":
            guard opened == nil, focus.highlighted != nil else { return false }
            focus.peeking.toggle()
            return true
        case "c" where place != .people && place != .projects:
            focus.composing = place == .triage ? WorkPatch() : model.workDraftDefaults
            return true
        case "m":
            guard !targets.isEmpty else { return false }
            relatePressedAt = event.timestamp
            return true
        case "s": return run(.status)
        case "p": return run(.priority)
        case "a": return run(.owner)
        case "i": return run(.assignToMe)
        case "d": return run(.due)
        case "l": return run(.labels)
        case "e": return run(.rename)
        case "1" where place == .triage: return run(.accept)
        case "2" where place == .triage: return run(.decline)
        case "3" where place == .triage: return run(.duplicate)
        case "h": return run(place == .triage ? .snooze : .remind)
        case "b": return run(.block)
        case "r": return run(.nudge)
        case "y": return answer(.acceptProposal)
        case "n": return answer(.rejectProposal)
        case "?":
            focus.shortcutsShown = true
            return true
        default:
            return false
        }
    }
}

extension AppModel {
    /// Whether tasks are in front: Tasks, Triage, or a person's tasks and the
    /// task opened from them.
    var showsTasks: Bool {
        switch place {
        case .tasks, .triage: true
        case .people: workFocus.personList != nil || workFocus.opened(in: .people) != nil
        case .projects: workFocus.projectList != nil || workFocus.opened(in: .projects) != nil
        case .inbox: workFocus.opened(in: .inbox) != nil
        default: false
        }
    }

    /// The tasks a verb acts on now, when a task screen is showing.
    var workTargets: [String] {
        guard !showingSettings, showsTasks else { return [] }
        return workFocus.targets(in: place)
    }

    func workRows(_ ids: [String]) -> [WorkItemRow] {
        guard !ids.isEmpty, let rows = work.items else { return [] }
        let byId = Dictionary(rows.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        return ids.compactMap { byId[$0] }
    }

    /// The tasks among these with a proposed change waiting.
    func workProposed(_ rows: [WorkItemRow]) -> Set<String> {
        Set(rows.map(\.id).filter { !work.proposals(for: $0).isEmpty })
    }

    /// The tasks Triage lists under proposed changes, on its first split,
    /// the longest waiting first. With no task open there, Y, ⇧Y and N
    /// answer the first.
    var triageProposed: [WorkItemRow] {
        guard workFocus.triageSplit == nil else { return [] }
        let open = work.proposals
        return workRows(open.keys.sorted {
            (open[$0]?.first?.createdAtMs ?? 0, $0) < (open[$1]?.first?.createdAtMs ?? 0, $1)
        })
    }

    func perform(_ action: WorkAction, on ids: [String]) {
        guard let first = ids.first else { return }
        let focus = workFocus
        if action == .milestone, let project = workRows(ids).first?.projectId {
            Task {
                focus.milestones = await projects.milestones(of: project)
                focus.picker = WorkPicker(kind: .milestone, ids: ids)
                paletteShown = true
            }
            return
        }
        if let kind = action.picker {
            focus.picker = WorkPicker(kind: kind, ids: ids)
            paletteShown = true
            return
        }
        switch action {
        case .open:
            focus.peeking = false
            focus.highlighted = first
            focus.open(first, in: place)
        case .peek:
            focus.highlighted = first
            focus.peeking.toggle()
        case .assignToMe:
            changeWork { work.update(ids, WorkPatch(owner: .myself)) }
        case .rename:
            focus.renaming = first
        case .comment, .history:
            focus.peeking = false
            focus.highlighted = first
            focus.open(first, in: place)
            focus.pageAsk = TaskPageAsk(id: first, part: action == .comment ? .composer : .history)
        case .restore:
            changeWork { work.restore(ids) }
        case .accept:
            leave(ids) { work.accept(ids, applySuggestions: true) }
        case .archive:
            leave(ids) { work.archive(ids) }
        case .delete:
            leave(ids) { work.delete(ids) }
        case .nudge:
            sheet = .nudge(first)
        case .acceptProposal:
            changeWork { work.acceptProposals(ids.compactMap { work.proposals(for: $0).first?.id }) }
        case .acceptAllProposals:
            changeWork { work.acceptProposals(ids.flatMap { work.proposals(for: $0).map(\.id) }) }
        case .rejectProposal:
            work.rejectProposals(ids.compactMap { work.proposals(for: $0).first?.id })
        case .block:
            work.book(first)
        case .removeParent:
            let children = workRows(ids).filter { $0.parentId != nil }.map(\.id)
            changeWork { work.update(children, WorkPatch(parentId: .clear)) }
        case .status, .priority, .owner, .due, .labels, .project, .milestone, .duplicate, .snooze, .remind, .decline,
             .delegate, .cycle, .blockedBy, .blocks, .related, .parent, .estimate, .forPerson:
            break
        }
    }

    /// Every label by id, for rows that hold ids.
    var workLabels: [String: WorkLabel] {
        Dictionary(work.labels.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
    }

    /// A click on a row: ⌘ adds or removes it from the selection, ⇧ selects
    /// up to it, and a plain click opens it.
    func clickTask(_ id: String) {
        let flags = NSEvent.modifierFlags
        if flags.contains(.command) {
            workFocus.toggle(id)
        } else if flags.contains(.shift) {
            workFocus.extendSelection(to: id)
        } else {
            perform(.open, on: [id])
        }
    }

    /// A row's menu acts on the whole selection when the row is in it.
    func menuTargets(_ id: String) -> [String] {
        workFocus.selection.contains(id) ? workFocus.targets(in: place) : [id]
    }

    /// The end of an in-place rename: nil puts the title back.
    func renameTask(_ id: String, to title: String?) {
        guard workFocus.renaming == id else { return }
        workFocus.renaming = nil
        let trimmed = title?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        guard !trimmed.isEmpty, let row = workRows([id]).first, row.title != trimmed else { return }
        changeWork { work.update([id], WorkPatch(title: trimmed)) }
    }

    /// The project whose Tasks tab is in front.
    var listedProject: String? { place == .projects ? workFocus.projectList : nil }

    /// The filters and display of the list in front; a project's Tasks tab
    /// keeps its own.
    var listing: WorkViewDraft? {
        if let project = listedProject { return workFocus.projectListing(project) }
        return workFocus.listing(work.views)
    }

    /// Changes the filters or display of the list showing, as a draft until
    /// a saved view is saved.
    func editListing(_ change: (inout WorkViewDraft) -> Void) {
        guard var draft = listing else { return }
        change(&draft)
        setListing(draft)
    }

    private func setListing(_ draft: WorkViewDraft) {
        if let project = listedProject {
            workFocus.projectDrafts[project] = draft
        } else {
            workFocus.setListing(draft, views: work.views)
        }
    }

    /// Every change made from a task screen goes through here, so the toast
    /// can offer to take it back.
    func changeWork(_ change: () -> Void) {
        change()
        workFocus.announce(work.undoTitle)
    }

    func undoWork() {
        guard work.undoTitle != nil else { return }
        work.undo()
        workFocus.toast = nil
    }

    /// Runs a verb that takes `ids` out of the list; the highlight lands on
    /// the row after them, and on a triage page the next task opens.
    func leave(_ ids: [String], _ change: () -> Void) {
        let focus = workFocus
        let next = focus.successor(of: ids)
        changeWork(change)
        focus.clearSelection()
        focus.highlighted = next
        if let opened = focus.opened(in: place), ids.contains(opened) {
            focus.open(place == .triage ? next : nil, in: place)
        }
    }

    /// J/K on an open task: the next one in the list opens in its place.
    func step(from id: String, by delta: Int) -> Bool {
        let visible = workFocus.visible
        guard let index = visible.firstIndex(of: id) else { return false }
        let next = index + delta
        guard visible.indices.contains(next) else { return true }
        workFocus.highlighted = visible[next]
        workFocus.open(visible[next], in: place)
        return true
    }

    func toggleWorkLayout() -> Bool {
        guard place == .tasks || listedProject != nil, workFocus.opened(in: place) == nil,
              var draft = listing else { return false }
        draft.display.layout = draft.display.layout == .list ? .board : .list
        setListing(draft)
        return true
    }

    /// ⌥⇧↑ and ⌥⇧↓: the tasks go to the top or the bottom of the manual
    /// order, and the list showing orders by hand so the move shows.
    func moveToEdge(_ ids: [String], top: Bool) -> Bool {
        guard place == .tasks || listedProject != nil, !ids.isEmpty, var draft = listing else { return false }
        work.move(ids, to: top ? .top : .bottom)
        if draft.display.orderBy != .manual {
            draft.display.orderBy = .manual
            setListing(draft)
        }
        return true
    }

    /// What a new task starts with: on Waiting on, nobody yet; elsewhere, me.
    /// On a project's Tasks tab it starts in that project.
    var workDraftDefaults: WorkPatch {
        var start = WorkPatch(status: .notStarted,
                              owner: listedProject == nil && workFocus.tab == .waiting ? .nobody : .myself)
        if let project = listedProject { start.projectId = .set(project) }
        return start
    }

    /// The palette's verbs for the tasks in front: only those that apply, and
    /// a key only where that key works.
    var workPaletteActions: [PaletteAction] {
        if place == .archive, let id = workFocus.archiveHighlighted {
            return [PaletteAction(
                id: "work-restore", group: .work, title: WorkAction.restore.title,
                run: { [weak self] in self?.changeWork { self?.work.restoreFromArchive(id) } },
                icon: WorkAction.restore.icon, shortcut: "#")]
        }
        let ids = workTargets
        let rows = workRows(ids)
        let proposed = workProposed(rows)
        let verbs = WorkAction.allCases.filter { $0.applies(to: rows, proposed: proposed) }.map { action in
            PaletteAction(
                id: "work-\(action.rawValue)", group: .work, title: action.title,
                run: { [weak self] in self?.perform(action, on: ids) },
                icon: action.icon,
                shortcut: paletteKeys(action))
        }
        let copies = rows.isEmpty ? [] : TaskCopy.allCases.map { copy in
            PaletteAction(
                id: "work-copy-\(copy.rawValue)", group: .work, title: copy.title,
                run: { [weak self] in self?.copy(copy, ids: ids) },
                icon: copy.icon, shortcut: nil)
        }
        let toggles = rows.count == 1 ? watchToggles(ids) : []
        let overdue = overdueTargets
        guard !overdue.isEmpty else { return verbs + toggles + copies }
        return verbs + toggles + copies + [PaletteAction(
            id: "work-reschedule-overdue", group: .work, title: "Reschedule overdue",
            run: { [weak self] in self?.perform(.due, on: overdue) },
            icon: "calendar.badge.clock", shortcut: nil)]
    }

    /// Subscribe and favorite for the one task in front, each named for
    /// what it will do.
    private func watchToggles(_ ids: [String]) -> [PaletteAction] {
        guard let id = ids.first else { return [] }
        let watched = work.subscriptions[id] != nil
        let starred = work.isFavorite(.task, id)
        return [
            PaletteAction(
                id: "work-subscribe", group: .work, title: watched ? "Unsubscribe" : "Subscribe",
                run: { [weak self] in _ = self?.toggleSubscription(ids) },
                icon: watched ? "bell.slash" : "bell", shortcut: "shift+s"),
            PaletteAction(
                id: "work-favorite", group: .work, title: starred ? "Remove from favorites" : "Add to favorites",
                run: { [weak self] in _ = self?.toggleFavorite(ids) },
                icon: starred ? "star.slash" : "star", shortcut: "option+f"),
        ]
    }

    /// ⇧S: subscribes to every task here unless all of them already are.
    func toggleSubscription(_ ids: [String]) -> Bool {
        guard !ids.isEmpty else { return false }
        let on = ids.contains { work.subscriptions[$0] == nil }
        for id in ids { work.subscribe(id, on) }
        let noun = ids.count == 1 ? "this task" : "\(ids.count) tasks"
        workFocus.announce(on ? "Subscribed to \(noun)" : "Unsubscribed from \(noun)", undoes: false)
        return true
    }

    /// ⌥F: stars the one task here, or takes its star away.
    func toggleFavorite(_ ids: [String]) -> Bool {
        guard ids.count == 1, let id = ids.first else { return false }
        let on = !work.isFavorite(.task, id)
        work.setFavorite(.task, id, on)
        workFocus.announce(on ? "Added to favorites" : "Removed from favorites", undoes: false)
        return true
    }

    /// Sets, moves or, with nil, takes away the reminder on each task.
    func remind(_ ids: [String], at moment: Int64?) {
        let routines = self.routines
        let focus = workFocus
        Task {
            for id in ids { await routines.setReminder(on: id, at: moment) }
            guard routines.error == nil else { return focus.announce(routines.error, undoes: false) }
            let when = moment.map { "Reminder set for \(WorkChoices.long($0))" } ?? "Reminder removed"
            focus.announce(when, undoes: false)
        }
    }

    func copy(_ copy: TaskCopy, ids: [String]) {
        let rows = workRows(ids)
        guard !rows.isEmpty else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(copy.text(rows), forType: .string)
        workFocus.announce(copy.done(rows), undoes: false)
    }

    /// A `sona://task/<id>` link: the task's page on Tasks.
    func openTask(_ id: String) {
        reveal()
        go(.tasks)
        workFocus.peeking = false
        workFocus.highlighted = id
        workFocus.open(id, in: .tasks)
    }

    func paletteMatches(_ query: String) -> [PaletteMatch] {
        guard !query.isEmpty else { return [] }
        let number = Self.taskNumber(query)
        var found: [(row: WorkItemRow, rank: Int)] = []
        for row in work.items ?? [] where row.archivedAtMs == nil {
            if row.number == number {
                found.append((row, 0))
            } else if row.title.localizedCaseInsensitiveContains(query) {
                found.append((row, 1))
            } else if row.details?.localizedCaseInsensitiveContains(query) == true {
                found.append((row, 2))
            }
        }
        let tasks = found.sorted { a, b in
            (a.rank, a.row.status.isOpen ? 0 : 1, -a.row.updatedAtMs) < (b.rank, b.row.status.isOpen ? 0 : 1, -b.row.updatedAtMs)
        }
        .prefix(6)
        .map { row, rank in
            PaletteMatch(kind: .task(row.status), id: row.id, title: row.title,
                         meta: ["#\(row.number)", rank == 2 ? "in description" : nil, row.projectTitle]
                            .compactMap { $0 }.joined(separator: " · "))
        }
        let projects = work.projects.filter { $0.title.localizedCaseInsensitiveContains(query) }.prefix(4).map {
            PaletteMatch(kind: .project, id: $0.id, title: $0.title, meta: $0.status.label)
        }
        let people = work.people.filter { $0.name.localizedCaseInsensitiveContains(query) }.prefix(4).map {
            PaletteMatch(kind: .person, id: $0.id, title: $0.name)
        }
        let labels = work.labels.filter { $0.name.localizedCaseInsensitiveContains(query) }.prefix(4).map { label in
            let count = (work.items ?? []).filter { $0.archivedAtMs == nil && $0.labelIds.contains(label.id) }.count
            return PaletteMatch(kind: .label(label.hue), id: label.id, title: label.name,
                                meta: count == 1 ? "1 task" : "\(count) tasks")
        }
        return Array(tasks) + Array(projects) + Array(people) + Array(labels)
    }

    private static func taskNumber(_ query: String) -> Int64? {
        var digits = Substring(query.lowercased())
        if digits.hasPrefix("s-") {
            digits = digits.dropFirst(2)
        } else if digits.hasPrefix("#") || digits.hasPrefix("s") {
            digits = digits.dropFirst()
        }
        return Int64(digits)
    }

    func open(_ match: PaletteMatch) {
        switch match.kind {
        case .task: openTask(match.id)
        case .project: openProject(match.id)
        case .person: openPerson(match.id)
        case .label:
            reveal()
            go(.tasks)
            workFocus.open(nil, in: .tasks)
            editListing { $0.query.labelIds = [match.id] }
        }
    }

    private func paletteKeys(_ action: WorkAction) -> String? {
        if action == .duplicate, place != .triage { return "m m" }
        if action == (place == .triage ? .remind : .snooze) { return nil }
        let triageKeys: Set<WorkAction> = [.accept, .decline, .duplicate]
        let keys = place == .triage || !triageKeys.contains(action) ? action.keys : nil
        return keys?.isEmpty == true ? nil : keys
    }

    /// The tasks the palette acts on, for the chip over its field.
    var paletteContext: String? { workContext(workTargets) }

    func workContext(_ ids: [String]) -> String? {
        let rows = workRows(ids)
        guard let first = rows.first else { return nil }
        return rows.count == 1 ? "#\(first.number) \(first.title)" : "\(rows.count) tasks"
    }

    func newTask() {
        go(.tasks)
        workFocus.open(nil, in: .tasks)
        workFocus.composing = workDraftDefaults
    }
}

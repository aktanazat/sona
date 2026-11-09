import AppKit
import SwiftUI

/// A list of choices the palette panel shows for the tasks it names: a
/// status, an owner, a date.
struct WorkPicker: Equatable {
    enum Kind: Equatable {
        case status, priority, owner, due, labels, project, duplicate, snooze
    }

    let kind: Kind
    let ids: [String]
}

/// Everything a person can do to tasks from the keyboard, the palette, a
/// row's menu or the bulk bar, with the one key each answers to.
enum WorkAction: String, CaseIterable, Identifiable {
    case open, peek, status, priority, owner, assignToMe, due, labels, project, rename
    case accept, decline, duplicate, snooze, nudge, archive

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
        case .rename: "Rename"
        case .accept: "Accept"
        case .decline: "Decline"
        case .duplicate: "Mark as duplicate…"
        case .snooze: "Snooze…"
        case .nudge: "Nudge…"
        case .archive: "Archive"
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
        case .due: "d"
        case .labels: "l"
        case .project: "shift+p"
        case .rename: "e"
        case .accept: "1"
        case .decline: "2"
        case .duplicate: "3"
        case .snooze: "h"
        case .nudge: "r"
        case .archive: "cmd+backspace"
        }
    }

    /// The same key as a menu's equivalent, so a row's menu shows it.
    var equivalent: KeyboardShortcut {
        switch self {
        case .open: KeyboardShortcut(.return, modifiers: [])
        case .peek: KeyboardShortcut(.space, modifiers: [])
        case .status: KeyboardShortcut("s", modifiers: [])
        case .priority: KeyboardShortcut("p", modifiers: [])
        case .owner: KeyboardShortcut("a", modifiers: [])
        case .assignToMe: KeyboardShortcut("i", modifiers: [])
        case .due: KeyboardShortcut("d", modifiers: [])
        case .labels: KeyboardShortcut("l", modifiers: [])
        case .project: KeyboardShortcut("p", modifiers: .shift)
        case .rename: KeyboardShortcut("e", modifiers: [])
        case .accept: KeyboardShortcut("1", modifiers: [])
        case .decline: KeyboardShortcut("2", modifiers: [])
        case .duplicate: KeyboardShortcut("3", modifiers: [])
        case .snooze: KeyboardShortcut("h", modifiers: [])
        case .nudge: KeyboardShortcut("r", modifiers: [])
        case .archive: KeyboardShortcut(.delete, modifiers: .command)
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
        case .rename: "pencil"
        case .accept: "checkmark.circle"
        case .decline: "xmark.circle"
        case .duplicate: "square.on.square"
        case .snooze: "moon.zzz"
        case .nudge: "arrowshape.turn.up.right"
        case .archive: "archivebox"
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
        case .duplicate: .duplicate
        case .snooze: .snooze
        case .open, .peek, .assignToMe, .rename, .accept, .decline, .nudge, .archive: nil
        }
    }

    /// Whether the verb means anything for these tasks; the palette and the
    /// menus offer only the ones that do.
    func applies(to rows: [WorkItemRow]) -> Bool {
        guard let first = rows.first else { return false }
        return switch self {
        case .open, .peek, .rename: rows.count == 1
        case .duplicate: rows.count == 1 && first.status != .duplicate
        case .assignToMe: rows.contains { !$0.ownerIsSelf }
        case .accept, .decline: rows.allSatisfy { $0.status == .triage }
        case .snooze: rows.allSatisfy { $0.status == .triage || $0.status.isOpen }
        case .nudge: rows.count == 1 && first.status.isOpen && first.followUpAtMs != nil
        case .archive: rows.contains { $0.archivedAtMs == nil }
        case .status, .priority, .owner, .due, .labels, .project: true
        }
    }
}

enum TasksTab: Hashable {
    case owe, waiting, all
    case view(String)
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
    private(set) var selection: Set<String> = []
    var peeking = false
    /// The task open on each place's page; Tasks and Triage keep their own.
    private(set) var openedIn: [Place: String] = [:]
    var picker: WorkPicker?
    var renaming: String?
    /// The new-task sheet, holding what the task starts with.
    var composing: WorkPatch?
    var shortcutsShown = false
    var displayShown = false
    var collapsed: Set<String> = []
    var tab: TasksTab = .owe
    var allQuery = WorkQuery()
    var allDisplay = WorkDisplay(properties: TaskProperty.standard)
    var filterShown = false
    var myProperties = TaskProperty.standard
    var viewDrafts: [String: WorkViewDraft] = [:]
    /// Which source Triage shows; nil shows all of it.
    var triageSplit: WorkSource?
    /// The change the undo toast offers to take back, and a count that
    /// restarts its timer when the same words come twice.
    var toast: String?
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

    var selecting: Bool { !selection.isEmpty }

    func opened(in place: Place) -> String? { openedIn[place] }

    func open(_ id: String?, in place: Place) {
        openedIn[place] = id
    }

    /// What a verb acts on: the open task, else the selection, else the
    /// highlighted row.
    func targets(in place: Place) -> [String] {
        if let opened = openedIn[place] { return [opened] }
        if !selection.isEmpty { return visible.filter(selection.contains) }
        return highlighted.map { [$0] } ?? []
    }

    func announce(_ title: String?) {
        toast = title
        toastStamp += 1
    }

    func show(_ ids: [String]) {
        visible = ids
        selection.formIntersection(ids)
        if let anchor, !ids.contains(anchor) { self.anchor = nil }
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
        let pointer = NSEvent.mouseLocation
        if let pointerAtKeys, pointerAtKeys == pointer { return }
        pointerAtKeys = nil
        highlighted = id
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
        case .owe, .waiting:
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
        case .owe, .waiting:
            break
        case .all:
            allQuery = draft.query
            allDisplay = draft.display
        case let .view(id):
            let saved = views.first { $0.id == id }.map { WorkViewDraft(query: $0.query, display: $0.display) }
            viewDrafts[id] = draft == saved ? nil : draft
        }
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
    private static let destinations: [String: Place] = [
        "h": .capture, "d": .today, "m": .meetings, "t": .tasks, "i": .triage, "p": .people, "j": .projects,
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
        if flags.isEmpty, key == "g" {
            goPressedAt = event.timestamp
            return true
        }
        guard !model.showingSettings else { return false }
        if model.place == .today || model.place == .capture {
            return RitualKeys.handle(event, key: key, flags: flags, model)
        }
        guard model.place == .tasks || model.place == .triage else { return false }
        return work(event, key: key, flags: flags, model)
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
            guard action.applies(to: rows) else { return false }
            model.perform(action, on: targets)
            return true
        }
        if event.keyCode == 53 { return focus.escape(in: place) }
        switch event.specialKey {
        case .downArrow?, .upArrow?:
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
            return run(.archive)
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
            default:
                return false
            }
        }
        if shift {
            switch key {
            case "p": return run(.project)
            case "d":
                guard place == .tasks, opened == nil else { return false }
                return model.rescheduleOverdue()
            case "v":
                guard place == .tasks, opened == nil else { return false }
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
            guard place == .tasks, opened == nil, focus.listing(model.work.views) != nil else { return false }
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
        case "c":
            focus.composing = place == .triage ? WorkPatch() : model.workDraftDefaults
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
        case "h": return run(.snooze)
        case "r": return run(.nudge)
        case "?":
            focus.shortcutsShown = true
            return true
        default:
            return false
        }
    }
}

extension AppModel {
    /// The tasks a verb acts on now, when a task screen is showing.
    var workTargets: [String] {
        guard !showingSettings, place == .tasks || place == .triage else { return [] }
        return workFocus.targets(in: place)
    }

    func workRows(_ ids: [String]) -> [WorkItemRow] {
        guard !ids.isEmpty, let rows = work.items else { return [] }
        let byId = Dictionary(rows.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        return ids.compactMap { byId[$0] }
    }

    func perform(_ action: WorkAction, on ids: [String]) {
        guard let first = ids.first else { return }
        let focus = workFocus
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
        case .accept:
            leave(ids) { work.accept(ids, applySuggestions: true) }
        case .decline:
            leave(ids) { work.decline(ids) }
        case .archive:
            leave(ids) { work.archive(ids) }
        case .nudge:
            sheet = .nudge(first)
        case .status, .priority, .owner, .due, .labels, .project, .duplicate, .snooze:
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

    /// Changes the filters or display of the list showing, as a draft until
    /// a saved view is saved.
    func editListing(_ change: (inout WorkViewDraft) -> Void) {
        guard var draft = workFocus.listing(work.views) else { return }
        change(&draft)
        workFocus.setListing(draft, views: work.views)
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
        let focus = workFocus
        guard place == .tasks, focus.opened(in: place) == nil, var draft = focus.listing(work.views) else { return false }
        draft.display.layout = draft.display.layout == .list ? .board : .list
        focus.setListing(draft, views: work.views)
        return true
    }

    /// What a new task starts with on the tab showing: on Waiting on, nobody
    /// yet; elsewhere, me.
    var workDraftDefaults: WorkPatch {
        WorkPatch(status: .notStarted, owner: workFocus.tab == .waiting ? .nobody : .myself)
    }

    /// The palette's verbs for the tasks in front: only those that apply, and
    /// a key only where that key works.
    var workPaletteActions: [PaletteAction] {
        let ids = workTargets
        let rows = workRows(ids)
        let triageKeys: Set<WorkAction> = [.accept, .decline, .duplicate]
        let verbs = WorkAction.allCases.filter { $0.applies(to: rows) }.map { action in
            PaletteAction(
                id: "work-\(action.rawValue)", group: .work, title: action.title,
                run: { [weak self] in self?.perform(action, on: ids) },
                icon: action.icon,
                shortcut: place == .triage || !triageKeys.contains(action) ? action.keys : nil)
        }
        let overdue = overdueTargets
        guard !overdue.isEmpty else { return verbs }
        return verbs + [PaletteAction(
            id: "work-reschedule-overdue", group: .work, title: "Reschedule overdue",
            run: { [weak self] in self?.perform(.due, on: overdue) },
            icon: "calendar.badge.clock", shortcut: "shift+d")]
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

import SwiftUI

/// Tasks: what I owe, what others owe me, every task, and saved views.
struct TasksScreen: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let focus = model.workFocus
        ZStack(alignment: .bottomLeading) {
            if let id = focus.opened(in: .tasks) {
                TaskDetailView(id: id, place: .tasks)
            } else {
                TasksList()
            }
            UndoToast()
        }
        .modifier(WorkSheets())
        .onDisappear {
            focus.peeking = false
            focus.renaming = nil
            focus.clearSelection()
        }
    }
}

/// The new-task sheet and the shortcut sheet, for whichever task screen is up.
struct WorkSheets: ViewModifier {
    @Environment(AppModel.self) private var model

    func body(content: Content) -> some View {
        let focus = model.workFocus
        content
            .sheet(isPresented: Binding(get: { focus.composing != nil }, set: { if !$0 { focus.composing = nil } })) {
                TaskComposer(start: focus.composing ?? WorkPatch())
                    .environment(model)
            }
            .sheet(isPresented: Binding(get: { focus.shortcutsShown }, set: { focus.shortcutsShown = $0 })) {
                ShortcutSheet()
                    .environment(model)
            }
    }
}

extension AppModel {
    /// The groups of the task list showing; on I owe, today's focus first.
    func taskGroups(_ draft: WorkViewDraft?, now: Int64) -> [TaskGroup] {
        guard let items = work.items else { return [] }
        switch workFocus.tab {
        case .owe:
            return TaskListing.myWork(items, direction: .mine, now: now, focus: rituals.focus)
        case .waiting:
            return TaskListing.myWork(items, direction: .waitingOn, now: now)
        case .all, .view:
            guard let draft else { return [] }
            let triageAsked = draft.query.statuses?.contains(.triage) == true
            let filter = TaskListing.Filter(draft.query, now: now)
            let rows = items.filter { ($0.status != .triage || triageAsked) && filter.matches($0) }
            return TaskListing.groups(rows, display: draft.display, now: now,
                                      projects: work.projects, people: work.people)
        }
    }
}

private struct TasksList: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let focus = model.workFocus
        let store = model.work
        let now = store.now
        let draft = focus.listing(store.views)
        let groups = model.taskGroups(draft, now: now)
        VStack(spacing: 0) {
            WorkBar {
                TaskTabs()
            } trailing: {
                trailing(draft)
            }
            if let draft, focus.tab != .all || !TaskFilters.chips(draft.query, model: model).isEmpty {
                TaskFilterBar(draft: draft)
            }
            if let error = store.listError, store.items != nil {
                HStack(spacing: 10) {
                    Text(error).metaText(Theme.live).lineLimit(1)
                    Button("Retry") { Task { await store.refresh() } }
                        .buttonStyle(QuietButton(compact: true))
                    Spacer(minLength: 0)
                }
                .padding(.horizontal, 16)
                .frame(height: 32)
            }
            content(groups, draft: draft, now: now)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .overlay(alignment: .trailing) {
            if focus.peeking, let id = focus.highlighted, let row = model.workRows([id]).first {
                TaskPeek(row: row, place: .tasks)
                    .transition(reduceMotion ? .opacity : .move(edge: .trailing).combined(with: .opacity))
            }
        }
        .overlay(alignment: .bottom) {
            if focus.selecting {
                BulkActionBar(place: .tasks)
                    .padding(.bottom, 20)
                    .transition(reduceMotion ? .opacity : .move(edge: .bottom).combined(with: .opacity))
            }
        }
        .animation(Motion.regular, value: focus.peeking)
        .animation(Motion.regular, value: focus.selecting)
    }


    @ViewBuilder
    private func content(_ groups: [TaskGroup], draft: WorkViewDraft?, now: Int64) -> some View {
        let store = model.work
        let focus = model.workFocus
        let shows = Set(draft?.display.properties ?? focus.myProperties)
        if store.items == nil {
            if let error = store.listError {
                TaskFailure(message: error) { Task { await store.refresh() } }
            } else {
                TaskSkeleton()
            }
        } else if groups.allSatisfy(\.rows.isEmpty) {
            empty(draft)
                .onAppear { focus.show([]) }
        } else if draft?.display.layout == .board {
            TaskBoardBody(groups: groups, shows: shows, now: now)
        } else {
            let labels = model.workLabels
            TaskListBody(groups: groups, headers: draft?.display.groupBy != WorkGroupBy.none, add: add) { item in
                ItemRow(
                    row: item, shows: shows, labels: item.labelIds.compactMap { labels[$0] },
                    highlighted: focus.highlighted == item.id, selected: focus.selection.contains(item.id),
                    selecting: focus.selecting, renaming: focus.renaming == item.id, now: now,
                    revision: model.people.photoRevision, waiting: model.agents.waits(on: item)
                ) {
                    focus.toggle(item.id)
                } rename: { title in
                    model.renameTask(item.id, to: title)
                }
            }
        }
    }

    @ViewBuilder
    private func empty(_ draft: WorkViewDraft?) -> some View {
        switch model.workFocus.tab {
        case .owe:
            TaskEmpty(line: "Nothing you owe.") {
                TaskKeyButton(title: "New task", keys: "c") { model.newTask() }
            }
        case .waiting:
            TaskEmpty(line: "Nobody owes you anything.")
        case .all, .view:
            if let draft, !TaskFilters.chips(draft.query, model: model).isEmpty {
                TaskEmpty(line: "No tasks match these filters.") {
                    Button("Clear filters") { model.editListing { $0.query = WorkQuery() } }
                        .buttonStyle(.compact)
                }
            } else {
                TaskEmpty(line: "No tasks yet.") {
                    TaskKeyButton(title: "New task", keys: "c") { model.newTask() }
                }
            }
        }
    }

    @ViewBuilder
    private func trailing(_ draft: WorkViewDraft?) -> some View {
        let focus = model.workFocus
        if let draft {
            TaskFilterButton()
            SegmentedSwitch(
                items: [
                    SwitchItem(id: WorkLayout.list, title: "List", icon: "list.bullet"),
                    SwitchItem(id: WorkLayout.board, title: "Board", icon: "rectangle.split.3x1"),
                ],
                selection: draft.display.layout, iconOnly: true
            ) { layout in
                model.editListing { $0.display.layout = layout }
            }
            .help("List or board  ⌘B")
        }
        WorkBarButton(icon: "slider.horizontal.3", help: "Display options  ⇧V", on: focus.displayShown) {
            focus.displayShown.toggle()
        }
        .popover(isPresented: Binding(get: { focus.displayShown }, set: { focus.displayShown = $0 }), arrowEdge: .bottom) {
            DisplayOptionsView()
                .environment(model)
        }
        Button {
            model.newTask()
        } label: {
            HStack(spacing: 6) {
                Image(systemName: "plus").font(.system(size: 11, weight: .bold))
                Text("New task")
            }
        }
        .buttonStyle(.primary)
        .help("New task  C")
    }

    /// The header's plus: a new task that already sits in that group.
    private func add(_ group: TaskGroup) {
        guard let patch = group.patch else { return }
        var start = model.workDraftDefaults
        if let status = patch.status { start.status = status }
        if let priority = patch.priority { start.priority = priority }
        if let owner = patch.owner { start.owner = owner }
        if let project = patch.projectId { start.projectId = project }
        if let due = patch.dueAtMs { start.dueAtMs = due }
        model.workFocus.composing = start
    }
}

/// I owe, Waiting on, All, then each saved view.
private struct TaskTabs: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let counts = model.work.counts
        ScrollView(.horizontal) {
            HStack(spacing: 2) {
                tab(.owe, title: "I owe", count: counts?.mineOpen)
                tab(.waiting, title: "Waiting on", count: counts?.waitingOn)
                tab(.all, title: "All", count: nil)
                ForEach(model.work.views) { view in
                    tab(.view(view.id), title: view.name, count: nil, edited: model.workFocus.viewDrafts[view.id] != nil)
                        .contextMenu {
                            Button("Delete view", role: .destructive) { delete(view) }
                        }
                }
            }
        }
        .scrollIndicators(.never)
        .scrollBounceBehavior(.basedOnSize)
    }

    private func tab(_ tab: TasksTab, title: String, count: Int?, edited: Bool = false) -> some View {
        WorkTab(title: title, count: count, edited: edited, on: model.workFocus.tab == tab) {
            model.workFocus.switchTab(tab)
        }
    }

    private func delete(_ view: WorkView) {
        let focus = model.workFocus
        if focus.tab == .view(view.id) { focus.switchTab(.all) }
        focus.viewDrafts[view.id] = nil
        model.work.deleteView(view.id)
    }
}

/// A tab over a task list: its title, its count unless zero, and a dot
/// while its view holds unsaved edits.
struct WorkTab: View {
    let title: String
    var count: Int?
    var edited = false
    let on: Bool
    let select: () -> Void

    var body: some View {
        Button(action: select) {
            HStack(spacing: 6) {
                Text(title)
                if let count, count > 0 {
                    Text("\(count)").monospacedDigit().foregroundStyle(Theme.inkTertiary)
                }
                if edited {
                    Circle().fill(Theme.accent).frame(width: 5, height: 5)
                }
            }
            .font(on ? TypeScale.listTitle : TypeScale.list)
            .foregroundStyle(on ? Theme.ink : Theme.inkSecondary)
            .padding(.horizontal, 10)
            .frame(height: 28)
            .background(on ? Theme.control : .clear, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(edited ? "\(title), edited" : title)
        .accessibilityAddTraits(on ? .isSelected : [])
    }
}

/// Grouped rows under headers that stay pinned while their rows scroll,
/// after `lead`, a section the screen adds of its own.
struct TaskListBody<Row: View, Lead: View>: View {
    let groups: [TaskGroup]
    var headers = true
    var add: ((TaskGroup) -> Void)?
    @ViewBuilder let lead: () -> Lead
    @ViewBuilder let row: (WorkItemRow) -> Row
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let focus = model.workFocus
        let visible = groups.filter { !focus.collapsed.contains($0.id) }.flatMap(\.rows).map(\.id)
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0, pinnedViews: headers ? [.sectionHeaders] : []) {
                    lead()
                    ForEach(groups) { group in
                        Section {
                            if !focus.collapsed.contains(group.id) {
                                ForEach(group.rows.map { GroupedRow(group: group.id, item: $0) }) { entry in
                                    row(entry.item)
                                        .modifier(TaskRowInteractions(id: entry.item.id))
                                        .transition(WorkMotion.insert(reduced: reduceMotion))
                                }
                            }
                        } header: {
                            if headers {
                                TaskGroupHeader(
                                    group: group, collapsed: focus.collapsed.contains(group.id),
                                    revision: model.people.photoRevision,
                                    toggle: { toggle(group.id) },
                                    add: group.patch == nil ? nil : add.map { add in { add(group) } },
                                    reschedule: group.mark == .bucket(.overdue)
                                        ? { model.perform(.due, on: group.rows.map(\.id)) } : nil)
                            }
                        }
                    }
                }
                .padding(.top, headers ? 0 : 6)
                .padding(.bottom, 96)
                .animation(Motion.regular, value: model.work.items?.map(\.id))
            }
            .onChange(of: focus.scrollTarget) { _, id in
                guard let id, let group = groups.first(where: { $0.rows.contains { $0.id == id } }) else { return }
                proxy.scrollTo(GroupedRow.key(group: group.id, item: id))
            }
        }
        .onChange(of: visible, initial: true) { _, ids in focus.show(ids) }
    }

    private func toggle(_ group: String) {
        let focus = model.workFocus
        if focus.collapsed.contains(group) { focus.collapsed.remove(group) } else { focus.collapsed.insert(group) }
    }
}

extension TaskListBody where Lead == EmptyView {
    init(groups: [TaskGroup], headers: Bool = true, add: ((TaskGroup) -> Void)? = nil,
         @ViewBuilder row: @escaping (WorkItemRow) -> Row) {
        self.init(groups: groups, headers: headers, add: add, lead: { EmptyView() }, row: row)
    }
}

/// A task as one group holds it. With the task's id alone, LazyVStack keeps
/// showing the old, stale view of a task that moved to another group.
private struct GroupedRow: Identifiable {
    let group: String
    let item: WorkItemRow
    var id: String { Self.key(group: group, item: item.id) }

    static func key(group: String, item: String) -> String { "\(group)/\(item)" }
}

/// What every task row and card answers to: the pointer lights it, a click
/// opens it, ⌘-click and ⇧-click select, and its menu lists the verbs.
struct TaskRowInteractions: ViewModifier {
    let id: String
    @Environment(AppModel.self) private var model

    func body(content: Content) -> some View {
        content
            .contentShape(Rectangle())
            .onHover { inside in
                if inside { model.workFocus.hover(id) }
            }
            .onTapGesture { model.clickTask(id) }
            .contextMenu { WorkActionMenu(ids: model.menuTargets(id)) }
            .accessibilityAddTraits(.isButton)
            .accessibilityAction { model.perform(.open, on: [id]) }
            .accessibilityAction(named: "Select") { model.workFocus.toggle(id) }
    }
}

/// A row's menu: the verbs that apply, each with its key. A menu that stays
/// on screen leaves the keys off: a plain-key equivalent there would take
/// letters typed into the page's fields.
struct WorkActionMenu: View {
    let ids: [String]
    var omit: Set<WorkAction> = [.peek]
    var keys = true
    @Environment(AppModel.self) private var model

    var body: some View {
        let rows = model.workRows(ids)
        let proposed = model.workProposed(rows)
        ForEach(WorkAction.allCases.filter { !omit.contains($0) && $0.applies(to: rows, proposed: proposed) }) { action in
            Button {
                model.perform(action, on: ids)
            } label: {
                Label(action.title, systemImage: action.icon)
            }
            .keyboardShortcut(keys ? action.equivalent : nil)
        }
    }
}

/// The board: one column per group, cards dragged between them.
private struct TaskBoardBody: View {
    let groups: [TaskGroup]
    let shows: Set<String>
    let now: Int64
    @Environment(AppModel.self) private var model

    var body: some View {
        let visible = groups.flatMap(\.rows).map(\.id)
        ScrollView(.horizontal) {
            HStack(alignment: .top, spacing: 12) {
                ForEach(groups) { group in
                    TaskColumn(group: group, shows: shows, now: now)
                }
            }
            .padding(16)
            .frame(maxHeight: .infinity, alignment: .top)
        }
        .onChange(of: visible, initial: true) { _, ids in model.workFocus.show(ids) }
    }
}

private struct TaskColumn: View {
    let group: TaskGroup
    let shows: Set<String>
    let now: Int64
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var targeted = false

    var body: some View {
        let focus = model.workFocus
        let labels = model.workLabels
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                TaskGroupMarkView(mark: group.mark, revision: model.people.photoRevision)
                Text(group.title)
                    .font(TypeScale.listTitle)
                    .foregroundStyle(Theme.ink)
                    .lineLimit(1)
                Text("\(group.rows.count)")
                    .font(TypeScale.label(12))
                    .monospacedDigit()
                    .foregroundStyle(Theme.inkTertiary)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 12)
            .frame(height: Theme.listRow)
            .accessibilityElement(children: .combine)
            .accessibilityAddTraits(.isHeader)
            ScrollView {
                LazyVStack(spacing: 6) {
                    ForEach(group.rows) { item in
                        TaskCard(
                            row: item, shows: shows, labels: item.labelIds.compactMap { labels[$0] },
                            highlighted: focus.highlighted == item.id, selected: focus.selection.contains(item.id),
                            now: now, revision: model.people.photoRevision, waiting: model.agents.waits(on: item))
                            .modifier(TaskRowInteractions(id: item.id))
                            .draggable(model.menuTargets(item.id).joined(separator: "\n"))
                            .transition(WorkMotion.insert(reduced: reduceMotion))
                    }
                }
                .padding(.bottom, 24)
            }
            .scrollIndicators(.never)
        }
        .padding(.horizontal, 6)
        .frame(width: 280)
        .frame(maxHeight: .infinity, alignment: .top)
        .background(targeted ? Theme.selection : Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusCard, style: .continuous))
        .animation(Motion.regular, value: model.work.items?.map(\.id))
        .dropDestination(for: String.self) { payloads, _ in
            guard let patch = group.patch else { return false }
            let ids = payloads.flatMap { $0.split(separator: "\n").map(String.init) }
                .filter { id in !group.rows.contains { $0.id == id } }
            guard !ids.isEmpty else { return false }
            model.changeWork { model.work.update(ids, patch) }
            return true
        } isTargeted: { inside in
            targeted = inside && group.patch != nil
        }
    }
}

private struct TaskCard: View {
    let row: WorkItemRow
    let shows: Set<String>
    let labels: [WorkLabel]
    let highlighted: Bool
    let selected: Bool
    let now: Int64
    let revision: UInt64
    var waiting = false

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: Theme.radiusControl, style: .continuous)
        VStack(alignment: .leading, spacing: 8) {
            if hasTop {
                HStack(spacing: 6) {
                    if shows.contains(TaskProperty.number.rawValue) {
                        Text(verbatim: "#\(row.number)")
                            .font(TypeScale.mono(11))
                            .foregroundStyle(Theme.inkTertiary)
                    }
                    Spacer(minLength: 0)
                    if shows.contains(TaskProperty.updated.rawValue) {
                        Text(TaskDates.ago(row.updatedAtMs, now: now)).metaText()
                    }
                    if let doer = row.doer, row.status.isOpen {
                        TaskDoer(doer: doer, waiting: waiting)
                    }
                    if shows.contains(TaskProperty.owner.rawValue) {
                        Avatar(face: row.ownerFace, size: 18, revision: revision)
                    }
                }
            }
            HStack(alignment: .top, spacing: 8) {
                TaskStatusIcon(status: row.status).padding(.top, 1)
                Text(row.title)
                    .font(TypeScale.listTitle)
                    .foregroundStyle(row.status.isOpen || row.status == .triage ? Theme.ink : Theme.inkSecondary)
                    .lineLimit(3)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if hasMeta {
                HStack(spacing: 8) {
                    if shows.contains(TaskProperty.priority.rawValue), row.priority != 0 {
                        PriorityIcon(priority: TaskPriority(row.priority))
                    }
                    if shows.contains(TaskProperty.due.rawValue), let due = row.dueAtMs {
                        TaskDue(due: due, open: row.status.isOpen, now: now)
                    }
                    if shows.contains(TaskProperty.labels.rawValue) {
                        ForEach(labels.prefix(2)) { LabelPill(label: $0) }
                    }
                }
            }
            if shows.contains(TaskProperty.meeting.rawValue), let meeting = row.sourceMeetingTitle {
                TaskPlace(title: meeting, icon: "waveform")
            }
            if shows.contains(TaskProperty.project.rawValue), let project = row.projectTitle {
                TaskPlace(title: project, icon: "folder")
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(selected ? Theme.selection : Theme.surface, in: shape)
        .overlay {
            shape
                .fill(Theme.wash)
                .opacity(highlighted ? 1 : 0)
                .hoverFade(highlighted)
                .allowsHitTesting(false)
        }
        .overlay(shape.strokeBorder(selected ? Theme.accent : Theme.border, lineWidth: 1))
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    private var hasTop: Bool {
        [TaskProperty.number, .updated, .owner].contains { shows.contains($0.rawValue) }
            || (row.doer != nil && row.status.isOpen)
    }

    private var hasMeta: Bool {
        (shows.contains(TaskProperty.priority.rawValue) && row.priority != 0)
            || (shows.contains(TaskProperty.due.rawValue) && row.dueAtMs != nil)
            || (shows.contains(TaskProperty.labels.rawValue) && !labels.isEmpty)
    }
}

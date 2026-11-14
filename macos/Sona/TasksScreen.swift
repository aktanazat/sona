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
        case .cycle:
            return TaskListing.groups(cycleRows, display: WorkDisplay(groupBy: .status, orderBy: .priority), now: now,
                                      projects: work.projects, people: work.people)
        case .all, .view:
            guard let draft else { return [] }
            return listedGroups(items, draft: draft, now: now)
        }
    }

    /// `rows` through a listing's filters, grouped and ordered as its display
    /// asks; a board leaves its hidden columns out.
    func listedGroups(_ rows: [WorkItemRow], draft: WorkViewDraft, now: Int64) -> [TaskGroup] {
        let triageAsked = draft.query.statuses?.contains(.triage) == true
        let filter = TaskListing.Filter(draft.query, now: now, cycles: work.cycles)
        let shown = rows.filter { ($0.status != .triage || triageAsked) && filter.matches($0) }
        let groups = TaskListing.groups(shown, display: draft.display, now: now, projects: work.projects,
                                        people: work.people, cycles: work.cycles, labels: workLabels,
                                        milestones: milestones)
        guard draft.display.layout == .board, !draft.display.hiddenColumns.isEmpty else { return groups }
        return groups.filter { !draft.display.hiddenColumns.contains($0.id) }
    }

    /// A group's plus: a new task that already sits in that group.
    func composeTask(in group: TaskGroup) {
        guard let patch = group.patch else { return }
        var start = workDraftDefaults
        if let status = patch.status { start.status = status }
        if let priority = patch.priority { start.priority = priority }
        if let owner = patch.owner { start.owner = owner }
        if let project = patch.projectId { start.projectId = project }
        if let due = patch.dueAtMs { start.dueAtMs = due }
        if let labels = patch.labelIds { start.labelIds = labels }
        if let milestone = patch.milestoneId { start.milestoneId = milestone }
        if let cycle = patch.cycle { start.cycle = cycle }
        workFocus.composing = start
    }

    /// Every milestone of the projects read so far, for milestone groups.
    var milestones: [PlanningMilestone] { projects.details.values.flatMap(\.milestones) }

    /// The cycle the Cycle tab's page shows: the one opened from the list,
    /// else the one under way. Nil while the tab lists every cycle.
    var cycleShown: WorkCycle? {
        guard !workFocus.cycleListShown,
              let number = workFocus.cycleOpened ?? work.currentCycle?.number else { return nil }
        return work.cycles.first { $0.number == number }
    }

    /// The tasks planned in the cycle the tab shows, as its page lists them.
    var cycleRows: [WorkItemRow] {
        guard let number = cycleShown?.number else { return [] }
        return (work.items ?? []).filter {
            $0.cycle == number && $0.archivedAtMs == nil && $0.status != .triage && $0.status != .duplicate
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
            if let draft {
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
            if focus.peeking, focus.tab == .cycle, model.cycleShown == nil {
                if let number = focus.cycleHighlighted, let cycle = model.work.cycles.first(where: { $0.number == number }) {
                    CyclePeek(cycle: cycle)
                        .transition(reduceMotion ? .opacity : .move(edge: .trailing).combined(with: .opacity))
                }
            } else if focus.peeking, let id = focus.highlighted, let row = model.workRows([id]).first {
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
        } else if focus.tab == .cycle, let cycle = model.cycleShown {
            TaskListBody(groups: groups, lead: {
                CycleSummary(cycle: cycle, tally: CycleTally(model.cycleRows))
                if groups.allSatisfy(\.rows.isEmpty) {
                    Text("Nothing planned in this cycle. Press ⇧C on a task to add it.")
                        .bodyText(13, Theme.inkSecondary)
                        .frame(maxWidth: .infinity)
                        .padding(.top, 48)
                }
            }, row: { TaskListingRow(item: $0, shows: shows, now: now) })
        } else if focus.tab == .cycle, !store.cycles.isEmpty {
            CycleList()
        } else if groups.allSatisfy(\.rows.isEmpty) {
            empty(draft)
                .onAppear { focus.show([]) }
        } else {
            TaskListingBody(groups: groups, draft: draft, now: now)
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
        case .cycle:
            TaskEmpty(line: "No cycles yet.")
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
        if focus.tab == .cycle {
            CycleNavigation()
            CycleSettingsMenu()
        }
        TaskListingButtons(draft: draft)
        WorkChangesButton()
        WorkBarButton(icon: "archivebox", help: "Archive and recently deleted") {
            model.go(.archive)
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
}

/// A listing's filter button and its display options, which F and ⇧V open;
/// ⌘B switches between list and board.
struct TaskListingButtons: View {
    let draft: WorkViewDraft?
    @Environment(AppModel.self) private var model

    var body: some View {
        let focus = model.workFocus
        if draft != nil {
            TaskFilterButton()
        }
        WorkBarButton(icon: "slider.horizontal.3", help: "Display options  ⇧V", on: focus.displayShown) {
            focus.displayShown.toggle()
        }
        .popover(isPresented: Binding(get: { focus.displayShown }, set: { focus.displayShown = $0 }), arrowEdge: .bottom) {
            DisplayOptionsView()
                .environment(model)
        }
    }
}

/// A listing's tasks as its display lays them out: a board, or rows in
/// their groups. Outside a scrolling page of its own, the page scrolls them.
struct TaskListingBody: View {
    let groups: [TaskGroup]
    let draft: WorkViewDraft?
    let now: Int64
    var scrolls = true
    @Environment(AppModel.self) private var model

    var body: some View {
        let chosen = Set(draft?.display.properties ?? model.workFocus.myProperties)
        let shows = draft == nil && model.workFocus.tab == .owe ? chosen.subtracting([TaskProperty.owner.rawValue]) : chosen
        if let display = draft?.display, display.layout == .board {
            TaskBoardBody(groups: groups, display: display, shows: shows, now: now, scrolls: scrolls,
                          add: model.composeTask(in:))
        } else {
            TaskListBody(groups: groups, headers: draft?.display.groupBy != WorkGroupBy.none, scrolls: scrolls,
                         add: model.composeTask(in:)) { item in
                TaskListingRow(item: item, shows: shows, now: now)
            }
        }
    }
}

/// One task's row in a listing.
struct TaskListingRow: View {
    let item: WorkItemRow
    let shows: Set<String>
    let now: Int64
    @Environment(AppModel.self) private var model

    var body: some View {
        let focus = model.workFocus
        let labels = model.workLabels
        ItemRow(
            row: item, shows: shows, labels: item.labelIds.compactMap { labels[$0] },
            highlighted: focus.highlighted == item.id, selected: focus.selection.contains(item.id),
            selecting: focus.selecting, renaming: focus.renaming == item.id, now: now,
            revision: model.people.photoRevision, waiting: model.agents.waits(on: item),
            session: model.work.working[item.id]
        ) {
            focus.toggle(item.id)
        } rename: { title in
            model.renameTask(item.id, to: title)
        }
    }
}

/// I owe, Waiting on, the cycle, All, then one menu holding the saved views.
private struct TaskTabs: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let counts = model.work.counts
        ScrollView(.horizontal) {
            HStack(spacing: 2) {
                tab(.owe, title: "I owe", count: counts?.mineOpen)
                tab(.waiting, title: "Waiting on", count: counts?.waitingOn)
                if !model.work.cycles.isEmpty {
                    tab(.cycle, title: model.work.currentCycle.map { "Cycle \($0.number)" } ?? "Cycles", count: nil)
                }
                tab(.all, title: "All", count: nil)
                if !model.work.views.isEmpty {
                    views
                }
            }
        }
        .scrollIndicators(.never)
        .scrollBounceBehavior(.basedOnSize)
    }

    /// The saved views behind one tab that names the view showing.
    private var views: some View {
        let focus = model.workFocus
        let saved = model.work.views
        let current = saved.first { focus.tab == .view($0.id) }
        return Menu {
            ForEach(saved) { view in
                Toggle(isOn: Binding(get: { focus.tab == .view(view.id) },
                                     set: { _ in focus.switchTab(.view(view.id)) })) {
                    Text(focus.viewDrafts[view.id] != nil ? "\(view.name) (edited)" : view.name)
                }
            }
        } label: {
            HStack(spacing: 6) {
                Text(current?.name ?? "Views")
                if let current, focus.viewDrafts[current.id] != nil {
                    Circle().fill(Theme.accent).frame(width: 5, height: 5)
                }
                Image(systemName: "chevron.down").font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(Theme.inkTertiary)
            }
            .fixedSize()
            .font(current != nil ? TypeScale.listTitle : TypeScale.list)
            .foregroundStyle(current != nil ? Theme.ink : Theme.inkSecondary)
            .padding(.horizontal, 10)
            .frame(height: 28)
            .background(current != nil ? Theme.control : .clear, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .contentShape(Rectangle())
            .accessibilityElement(children: .combine)
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .menuIndicator(.hidden)
        .fixedSize()
        .accessibilityLabel(current.map { "Saved view, \($0.name)" } ?? "Saved views")
    }

    private func tab(_ tab: TasksTab, title: String, count: Int?) -> some View {
        WorkTab(title: title, count: count, on: model.workFocus.tab == tab) {
            model.workFocus.switchTab(tab)
        }
    }
}

/// A tab over a task list: its title and its count unless zero.
struct WorkTab: View {
    let title: String
    var count: Int?
    let on: Bool
    let select: () -> Void

    var body: some View {
        Button(action: select) {
            HStack(spacing: 6) {
                Text(title)
                if let count, count > 0 {
                    Text("\(count)").monospacedDigit().foregroundStyle(Theme.inkTertiary)
                }
            }
            .fixedSize()
            .font(on ? TypeScale.listTitle : TypeScale.list)
            .foregroundStyle(on ? Theme.ink : Theme.inkSecondary)
            .padding(.horizontal, 10)
            .frame(height: 28)
            .background(on ? Theme.control : .clear, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(title)
        .accessibilityAddTraits(on ? .isSelected : [])
    }
}

/// Grouped rows under headers that stay pinned while their rows scroll,
/// after `lead`, a section the screen adds of its own.
struct TaskListBody<Row: View, Lead: View>: View {
    let groups: [TaskGroup]
    var headers = true
    var scrolls = true
    var add: ((TaskGroup) -> Void)?
    @ViewBuilder let lead: () -> Lead
    @ViewBuilder let row: (WorkItemRow) -> Row
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let focus = model.workFocus
        let visible = groups.filter { !focus.collapsed.contains($0.id) }.flatMap(\.rows).map(\.id)
        ScrollViewReader { proxy in
            Group {
                if scrolls {
                    ScrollView { stack }
                } else {
                    stack
                }
            }
            .onChange(of: focus.scrollTarget) { _, id in
                guard let id, let group = groups.first(where: { $0.rows.contains { $0.id == id } }) else { return }
                proxy.scrollTo(GroupedRow.key(group: group.id, item: id))
            }
        }
        .onChange(of: visible, initial: true) { _, ids in focus.show(ids) }
    }

    private var stack: some View {
        let focus = model.workFocus
        return LazyVStack(alignment: .leading, spacing: 0, pinnedViews: headers && scrolls ? [.sectionHeaders] : []) {
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
        .padding(.bottom, scrolls ? 96 : 0)
        .animation(Motion.regular, value: model.work.items?.map(\.id))
    }

    private func toggle(_ group: String) {
        let focus = model.workFocus
        if focus.collapsed.contains(group) { focus.collapsed.remove(group) } else { focus.collapsed.insert(group) }
    }
}

extension TaskListBody where Lead == EmptyView {
    init(groups: [TaskGroup], headers: Bool = true, scrolls: Bool = true, add: ((TaskGroup) -> Void)? = nil,
         @ViewBuilder row: @escaping (WorkItemRow) -> Row) {
        self.init(groups: groups, headers: headers, scrolls: scrolls, add: add, lead: { EmptyView() }, row: row)
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
            .allowsWindowActivationEvents()
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
        if !rows.isEmpty {
            Divider()
            ForEach(TaskCopy.allCases) { copy in
                Button {
                    model.copy(copy, ids: ids)
                } label: {
                    Label(copy.title, systemImage: copy.icon)
                }
            }
            if rows.count == 1, let row = rows.first {
                Button {
                    model.sheet = .saveTemplate(row.id)
                } label: {
                    Label("Save as template…", systemImage: "doc.on.doc")
                }
            }
        }
    }
}

/// The board: one column per group, cards dragged between them; with
/// swimlanes, one row of those columns per group of the second grouping.
private struct TaskBoardBody: View {
    let groups: [TaskGroup]
    let display: WorkDisplay
    let shows: Set<String>
    let now: Int64
    var scrolls = true
    let add: (TaskGroup) -> Void
    @Environment(AppModel.self) private var model

    var body: some View {
        let lanes = lanes
        let visible = lanes.flatMap { $0.columns.flatMap(\.rows).map(\.id) }
        Group {
            if let only = lanes.first, lanes.count == 1, only.lane == nil, scrolls {
                ScrollView(.horizontal) {
                    columns(only.columns, scrolls: true)
                        .padding(16)
                        .frame(maxHeight: .infinity, alignment: .top)
                }
            } else {
                ScrollView(scrolls ? [.horizontal, .vertical] : .horizontal) {
                    VStack(alignment: .leading, spacing: 20) {
                        ForEach(lanes, id: \.id) { lane in
                            VStack(alignment: .leading, spacing: 8) {
                                if let header = lane.lane {
                                    HStack(spacing: 8) {
                                        TaskGroupMarkView(mark: header.mark, revision: model.people.photoRevision)
                                        Text(header.title).font(TypeScale.listTitle).foregroundStyle(Theme.ink)
                                        Text("\(header.rows.count)").font(TypeScale.label(12)).monospacedDigit()
                                            .foregroundStyle(Theme.inkTertiary)
                                    }
                                    .padding(.horizontal, 6)
                                    .accessibilityElement(children: .combine)
                                    .accessibilityAddTraits(.isHeader)
                                }
                                columns(lane.columns, scrolls: false)
                            }
                        }
                    }
                    .padding(16)
                }
            }
        }
        .onChange(of: visible, initial: true) { _, ids in model.workFocus.show(ids) }
    }

    private func columns(_ columns: [TaskGroup], scrolls: Bool) -> some View {
        HStack(alignment: .top, spacing: 12) {
            ForEach(columns) { group in
                TaskColumn(group: group, shows: shows, now: now, scrolls: scrolls, add: group.patch == nil ? nil : { add(group) })
            }
        }
    }

    private struct Lane {
        let id: String
        let lane: TaskGroup?
        let columns: [TaskGroup]
    }

    /// The columns once, or once per swimlane holding only that lane's
    /// cards; a card dropped in a lane's column takes both groups' fields.
    private var lanes: [Lane] {
        guard display.subGroupBy != .none, display.subGroupBy != display.groupBy else {
            return [Lane(id: "all", lane: nil, columns: groups)]
        }
        var byLane = display
        byLane.groupBy = display.subGroupBy
        let rows = Self.unique(groups.flatMap(\.rows))
        let work = model.work
        let laneGroups = TaskListing.grouped(
            rows, display: byLane, now: now, projects: work.projects, people: work.people, cycles: work.cycles,
            labels: model.workLabels, milestones: model.milestones)
        return laneGroups.map { lane in
            let inLane = Set(lane.rows.map(\.id))
            let columns = groups.map { column in
                TaskGroup(id: "\(lane.id)|\(column.id)", title: column.title, mark: column.mark,
                          rows: column.rows.filter { inLane.contains($0.id) },
                          patch: Self.both(column.patch, lane.patch), addsLabel: column.addsLabel ?? lane.addsLabel)
            }
            return Lane(id: lane.id, lane: lane, columns: columns)
        }
    }

    private static func unique(_ rows: [WorkItemRow]) -> [WorkItemRow] {
        var seen = Set<String>()
        return rows.filter { seen.insert($0.id).inserted }
    }

    /// A column's fields with its lane's beside them; nil when neither sets one.
    private static func both(_ column: WorkPatch?, _ lane: WorkPatch?) -> WorkPatch? {
        guard var patch = column else { return lane }
        guard let lane else { return patch }
        patch.status = patch.status ?? lane.status
        patch.priority = patch.priority ?? lane.priority
        patch.owner = patch.owner ?? lane.owner
        patch.projectId = patch.projectId ?? lane.projectId
        patch.milestoneId = patch.milestoneId ?? lane.milestoneId
        patch.dueAtMs = patch.dueAtMs ?? lane.dueAtMs
        patch.cycle = patch.cycle ?? lane.cycle
        patch.labelIds = patch.labelIds.map { Array(Set($0 + (lane.labelIds ?? []))) } ?? lane.labelIds
        return patch
    }
}

private struct TaskColumn: View {
    let group: TaskGroup
    let shows: Set<String>
    let now: Int64
    let scrolls: Bool
    let add: (() -> Void)?
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var targeted = false

    var body: some View {
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
                Menu {
                    Button("Hide column") { hide() }
                } label: {
                    Image(systemName: "ellipsis").font(.system(size: 11, weight: .semibold))
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .fixedSize()
                .foregroundStyle(Theme.inkTertiary)
                .help("Column options")
                if let add {
                    Button(action: add) {
                        Image(systemName: "plus").font(.system(size: 11, weight: .semibold))
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(Theme.inkTertiary)
                    .help("New task in \(group.title)")
                    .accessibilityLabel("New task in \(group.title)")
                }
            }
            .padding(.horizontal, 12)
            .frame(height: Theme.listRow)
            .accessibilityElement(children: .contain)
            .accessibilityAddTraits(.isHeader)
            if scrolls {
                ScrollView { cards.padding(.bottom, 24) }
                    .scrollIndicators(.never)
            } else {
                cards.padding(.bottom, 12)
            }
        }
        .padding(.horizontal, 6)
        .frame(width: 280)
        .frame(maxHeight: scrolls ? .infinity : nil, alignment: .top)
        .background(targeted ? Theme.selection : Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusCard, style: .continuous))
        .animation(Motion.regular, value: model.work.items?.map(\.id))
        .dropDestination(for: String.self) { payloads, _ in
            guard let patch = group.patch else { return false }
            let ids = payloads.flatMap { $0.split(separator: "\n").map(String.init) }
                .filter { id in !group.rows.contains { $0.id == id } }
            guard !ids.isEmpty else { return false }
            drop(ids, patch)
            return true
        } isTargeted: { inside in
            targeted = inside && group.patch != nil
        }
    }

    private var cards: some View {
        let focus = model.workFocus
        let labels = model.workLabels
        return LazyVStack(spacing: 6) {
            ForEach(group.rows) { item in
                TaskCard(
                    row: item, shows: shows, labels: item.labelIds.compactMap { labels[$0] },
                    highlighted: focus.highlighted == item.id, selected: focus.selection.contains(item.id),
                    now: now, revision: model.people.photoRevision, waiting: model.agents.waits(on: item),
                    session: model.work.working[item.id])
                    .modifier(TaskRowInteractions(id: item.id))
                    .draggable(model.menuTargets(item.id).joined(separator: "\n"))
                    .transition(WorkMotion.insert(reduced: reduceMotion))
            }
        }
    }

    /// A label column adds its label to the ones each card has; every other
    /// column sets its fields outright.
    private func drop(_ ids: [String], _ patch: WorkPatch) {
        guard let label = group.addsLabel else {
            model.changeWork { model.work.update(ids, patch) }
            return
        }
        for row in model.workRows(ids) {
            var each = patch
            each.labelIds = Array(Set(row.labelIds + (patch.labelIds ?? []) + [label]))
            model.changeWork { model.work.update([row.id], each) }
        }
    }

    private func hide() {
        let column = group.id.split(separator: "|").last.map(String.init) ?? group.id
        model.editListing { draft in
            if !draft.display.hiddenColumns.contains(column) { draft.display.hiddenColumns.append(column) }
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
    var session: WorkSession?

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
                    if let agent = session?.agent ?? (row.status.isOpen ? row.doer?.agent : nil) {
                        TaskDoer(agent: agent, waiting: waiting, session: session)
                    }
                    if shows.contains(TaskProperty.owner.rawValue), row.ownerIsSelf || row.ownerPersonId != nil {
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
                    .help(row.title)
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
                    if row.attachmentCount > 0 {
                        AttachmentCount(count: row.attachmentCount)
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
        [TaskProperty.number, .updated].contains { shows.contains($0.rawValue) }
            || (shows.contains(TaskProperty.owner.rawValue) && (row.ownerIsSelf || row.ownerPersonId != nil))
            || session != nil || (row.doer != nil && row.status.isOpen)
    }

    private var hasMeta: Bool {
        (shows.contains(TaskProperty.priority.rawValue) && row.priority != 0)
            || (shows.contains(TaskProperty.due.rawValue) && row.dueAtMs != nil)
            || (shows.contains(TaskProperty.labels.rawValue) && !labels.isEmpty)
            || row.attachmentCount > 0
    }
}

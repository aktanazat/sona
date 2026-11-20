import SwiftUI

/// One filter in force, as the chip bar names it.
struct TaskFilterChip: Identifiable {
    let id: String
    let text: String
    let clear: (inout WorkQuery) -> Void
}

@MainActor
enum TaskFilters {
    static func chips(_ query: WorkQuery, model: AppModel) -> [TaskFilterChip] {
        let store = model.work
        var chips: [TaskFilterChip] = []
        if let search = query.search?.trimmingCharacters(in: .whitespaces), !search.isEmpty {
            chips.append(TaskFilterChip(id: "search", text: "Contains “\(search)”") { $0.search = nil })
        }
        if let statuses = query.statuses {
            chips.append(TaskFilterChip(id: "status", text: "Status is \(list(statuses.map(\.label)))") { $0.statuses = nil })
        }
        if let priorities = query.priorities {
            let names = priorities.map { TaskPriority($0).label }
            chips.append(TaskFilterChip(id: "priority", text: "Priority is \(list(names))") { $0.priorities = nil })
        }
        if let direction = query.direction {
            let text = switch direction {
            case .mine: "Owner is me"
            case .waitingOn: "Owned by someone else"
            case .unattributed: "No owner"
            }
            chips.append(TaskFilterChip(id: "direction", text: text) { $0.direction = nil })
        }
        if let owners = query.ownerPersonIds {
            let names = owners.map { id in store.people.first { $0.id == id }?.name ?? "Someone" }
            chips.append(TaskFilterChip(id: "owner", text: "Owner is \(list(names))") { $0.ownerPersonIds = nil })
        }
        if let projects = query.projectIds {
            let names = projects.map { id in
                id == "none" ? "No project" : store.projects.first { $0.id == id }?.title ?? "A project"
            }
            chips.append(TaskFilterChip(id: "project", text: "Project is \(list(names))") { $0.projectIds = nil })
        }
        if let labels = query.labelIds {
            let names = labels.map { id in store.labels.first { $0.id == id }?.name ?? "A label" }
            chips.append(TaskFilterChip(id: "label", text: "Label is \(list(names))") { $0.labelIds = nil })
        }
        if let before = query.dueBeforeMs {
            chips.append(TaskFilterChip(id: "dueBefore", text: "Due before \(PlanningFormat.day(before))") { $0.dueBeforeMs = nil })
        }
        if let due = query.due {
            let names = due.enumerated().map { index, window in
                let name = title(window)
                return index == 0 ? name : name.prefix(1).lowercased() + name.dropFirst()
            }
            chips.append(TaskFilterChip(id: "due", text: names.formatted(.list(type: .or))) { $0.due = nil })
        }
        if let meeting = query.sourceMeetingId {
            let title = store.items?.first { $0.sourceMeetingId == meeting }?.sourceMeetingTitle ?? "a meeting"
            chips.append(TaskFilterChip(id: "meeting", text: "From \(title)") { $0.sourceMeetingId = nil })
        }
        if let cycle = query.cycle {
            chips.append(TaskFilterChip(id: "cycle", text: title(cycle)) { $0.cycle = nil })
        }
        for (index, filter) in (query.filters ?? []).enumerated() {
            chips.append(TaskFilterChip(id: "condition-\(index)", text: text(filter, model: model)) {
                $0.filters = TaskFilters.editing($0.filters, at: [index]) { $0 = nil }
            })
        }
        if query.includeSnoozed {
            chips.append(TaskFilterChip(id: "snoozed", text: "Snoozed shown") { $0.includeSnoozed = false })
        }
        return chips
    }

    /// The due windows the filter panel offers; each moves with the calendar.
    static let dueWindows: [WorkDue] = [.overdue, .today, .thisWeek, .nextDays(14), .noDate]

    static func title(_ window: WorkDue) -> String {
        switch window {
        case .overdue: "Overdue"
        case .today: "Due today"
        case .thisWeek: "Due this week"
        case .nextDays(14): "Due within two weeks"
        case .nextDays(1): "Due within a day"
        case let .nextDays(days): "Due within \(days) days"
        case .noDate: "No due date"
        }
    }

    /// The cycles the filter panel offers; current and next move as cycles
    /// turn over.
    static let cycleChoices: [WorkCycleRef] = [.current, .next, .noCycle]

    static func title(_ cycle: WorkCycleRef) -> String {
        switch cycle {
        case .current: "Current cycle"
        case .next: "Next cycle"
        case .noCycle: "No cycle"
        case let .number(number): "Cycle \(number)"
        }
    }

    /// "Todo, In progress" or "Todo and 3 more".
    private static func list(_ names: [String]) -> String {
        names.count <= 3 ? names.joined(separator: ", ") : "\(names[0]) and \(names.count - 1) more"
    }

    /// The list with `value` in it once, or without it; nil once empty, so an
    /// emptied filter stops filtering.
    static func toggled<T: Equatable>(_ list: [T]?, _ value: T) -> [T]? {
        var list = list ?? []
        if let index = list.firstIndex(of: value) { list.remove(at: index) } else { list.append(value) }
        return list.isEmpty ? nil : list
    }

    /// `filters` with the filter at `path` changed, a path leading through
    /// groups; one changed to nil goes, a group left empty goes with it,
    /// and nil once none are left.
    static func editing(_ filters: [WorkFilter]?, at path: [Int], _ change: (inout WorkFilter?) -> Void) -> [WorkFilter]? {
        guard var filters, let first = path.first, filters.indices.contains(first) else { return filters }
        var target: WorkFilter? = filters[first]
        if path.count == 1 {
            change(&target)
        } else if let members = target?.members {
            let edited = editing(members, at: Array(path.dropFirst()), change)
            target = edited.flatMap { target?.with($0) }
        }
        if let target { filters[first] = target } else { filters.remove(at: first) }
        return filters.isEmpty ? nil : filters
    }

    /// `filters` with `added` at the end of the group at `path`, or of the
    /// list itself for an empty path.
    static func adding(_ added: WorkFilter, to filters: [WorkFilter]?, at path: [Int]) -> [WorkFilter]? {
        guard !path.isEmpty else { return (filters ?? []) + [added] }
        return editing(filters, at: path) { group in
            group = group.map { $0.with(($0.members ?? []) + [added]) }
        }
    }

    /// The operator a new condition on `field` starts with: one that takes
    /// several values, so picking more widens it.
    static func startingOp(_ field: WorkConditionField) -> WorkConditionOp {
        field.dated ? .before : field == .label ? .includesAny : field == .cycleScope ? .is : .anyOf
    }

    /// Cycle scopes as the panel offers them, in the core's words.
    static let cycleScopes = [ConditionChoice(value: "planned", title: "Planned at start"),
                              ConditionChoice(value: "added", title: "Added after start")]

    /// Whether `filters` keep only the work of `scope`.
    static func scoped(_ filters: [WorkFilter]?, _ scope: String) -> Bool {
        filters?.contains { $0.condition.map { $0.field == .cycleScope && $0.op == .is && $0.values == [scope] } == true } == true
    }

    /// `filters` keeping only `scope`, or no scope when it was already kept.
    static func toggledScope(_ filters: [WorkFilter]?, _ scope: String) -> [WorkFilter]? {
        let others = (filters ?? []).filter { $0.condition?.field != .cycleScope }
        let kept = WorkFilter.condition(WorkCondition(field: .cycleScope, op: .is, values: [scope]))
        let next = scoped(filters, scope) ? others : others + [kept]
        return next.isEmpty ? nil : next
    }

    /// Operators that compare against one value only.
    static func takesOneValue(_ op: WorkConditionOp) -> Bool {
        op == .is || op == .isNot || op == .before || op == .after
    }

    /// The values a condition on `field` can name, in the core's words.
    static func choices(_ field: WorkConditionField, model: AppModel) -> [ConditionChoice] {
        let store = model.work
        switch field {
        case .status:
            return WorkStatus.allCases.map { ConditionChoice(value: $0.rawValue, title: $0.label) }
        case .priority:
            return [TaskPriority.urgent, .high, .medium, .low, .none].map {
                ConditionChoice(value: "\($0.rawValue)", title: $0.label)
            }
        case .owner:
            return [ConditionChoice(value: "me", title: "Me"), ConditionChoice(value: "none", title: "No owner")]
                + store.people.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
                .map { ConditionChoice(value: $0.id, title: $0.name) }
        case .forPerson:
            return store.people.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
                .map { ConditionChoice(value: $0.id, title: $0.name) }
                + [ConditionChoice(value: "none", title: "No one")]
        case .estimate:
            return TaskEstimate.points.map { ConditionChoice(value: "\($0)", title: TaskEstimate.title($0)) }
                + [ConditionChoice(value: "none", title: "No estimate")]
        case .completed:
            return [ConditionChoice(value: "today", title: "Today"),
                    ConditionChoice(value: "yesterday", title: "Yesterday"),
                    ConditionChoice(value: "this_week", title: "This week"),
                    ConditionChoice(value: "last_week", title: "Last week"),
                    ConditionChoice(value: "this_month", title: "This month"),
                    ConditionChoice(value: "last_month", title: "Last month")]
        case .delegate:
            var agents = WorkChoices.agents
            for agent in (store.items ?? []).compactMap(\.doer?.agent).sorted() where !agents.contains(agent) {
                agents.append(agent)
            }
            return agents.map { ConditionChoice(value: $0, title: $0) }
                + [ConditionChoice(value: "none", title: "No agent")]
        case .project:
            return store.projects.map { ConditionChoice(value: $0.id, title: $0.title) }
                + [ConditionChoice(value: "none", title: "No project")]
        case .milestone:
            let milestones = model.projects.details.values
                .flatMap(\.milestones)
                .sorted { $0.title.localizedStandardCompare($1.title) == .orderedAscending }
            return milestones.map { ConditionChoice(value: $0.id, title: $0.title) }
                + [ConditionChoice(value: "none", title: "No milestone")]
        case .label:
            return store.labels.map { ConditionChoice(value: $0.id, title: $0.name) }
        case .parent:
            return (store.items ?? []).filter { $0.subTasks != nil }
                .map { ConditionChoice(value: $0.id, title: "S-\($0.number) \($0.title)") }
                + [ConditionChoice(value: "none", title: "No parent")]
        case .cycleScope:
            return cycleScopes
        case .cycle:
            return [ConditionChoice(value: "current", title: "Current cycle"),
                    ConditionChoice(value: "next", title: "Next cycle")]
                + store.cycles.map { ConditionChoice(value: "\($0.number)", title: $0.title) }
                + [ConditionChoice(value: "none", title: "No cycle")]
        case .due, .created, .updated:
            let calendar = Calendar.current
            let today = calendar.startOfDay(for: .now)
            let shifted = [-30, -7, 7, 30].compactMap { offset -> ConditionChoice? in
                guard let date = calendar.date(byAdding: .day, value: offset, to: today) else { return nil }
                let title = offset < 0 ? "\(-offset) days ago" : "In \(offset) days"
                return ConditionChoice(value: dayString(date), title: title)
            }
            let relative = [ConditionChoice(value: "yesterday", title: "Yesterday"),
                            ConditionChoice(value: "today", title: "Today"),
                            ConditionChoice(value: "tomorrow", title: "Tomorrow")]
            if field == .due {
                return relative + shifted.suffix(2) + [ConditionChoice(value: "none", title: "No date")]
            }
            return Array(shifted.prefix(2)) + Array(relative.prefix(2))
        }
    }

    /// "Priority is any of Urgent, High".
    static func text(_ condition: WorkCondition, model: AppModel) -> String {
        let known = choices(condition.field, model: model)
        let names = condition.values.map { value in
            known.first { $0.value == value || $0.title.caseInsensitiveCompare(value) == .orderedSame }?.title
                ?? dayTitle(value) ?? value
        }
        return "\(condition.field.label) \(condition.op.label) \(list(names))"
    }

    /// "Priority is Urgent or Due before Today"; a group within a group is
    /// bracketed.
    static func text(_ filter: WorkFilter, model: AppModel) -> String {
        switch filter {
        case let .condition(condition): text(condition, model: model)
        case let .any(members): members.map { bracketed($0, model: model) }.joined(separator: " or ")
        case let .all(members): members.map { bracketed($0, model: model) }.joined(separator: " and ")
        }
    }

    private static func bracketed(_ member: WorkFilter, model: AppModel) -> String {
        member.condition == nil ? "(\(text(member, model: model)))" : text(member, model: model)
    }

    private static func dayString(_ date: Date) -> String {
        let parts = Calendar.current.dateComponents([.year, .month, .day], from: date)
        return String(format: "%04d-%02d-%02d", parts.year ?? 0, parts.month ?? 0, parts.day ?? 0)
    }

    private static func dayTitle(_ value: String) -> String? {
        let spans = ["last_week": "Last week", "this_week": "This week", "next_week": "Next week",
                     "last_month": "Last month", "this_month": "This month", "next_month": "Next month"]
        if let title = spans[value] { return title }
        let parts = value.split(separator: "-").compactMap { Int($0) }
        guard parts.count == 3,
              let date = Calendar.current.date(from: DateComponents(year: parts[0], month: parts[1], day: parts[2]))
        else { return nil }
        return date.formatted(.dateTime.month(.abbreviated).day())
    }
}

struct ConditionChoice: Identifiable, Hashable {
    let value: String
    let title: String
    var id: String { value }
}

/// The bar's filter button and the panel it opens; F opens it too.
struct TaskFilterButton: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let focus = model.workFocus
        WorkBarButton(icon: "line.3.horizontal.decrease", help: "Filter  F", on: focus.filterShown) {
            focus.filterShown.toggle()
        }
        .popover(isPresented: Binding(get: { focus.filterShown }, set: { focus.filterShown = $0 }), arrowEdge: .bottom) {
            TaskFilterPanel()
                .environment(model)
        }
    }
}

private struct TaskFilterPanel: View {
    @Environment(AppModel.self) private var model
    @FocusState private var searching: Bool

    var body: some View {
        let store = model.work
        let query = model.listing?.query ?? WorkQuery()
        let counts = FilterCounts(model: model, query: query)
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .font(.system(size: 12))
                    .foregroundStyle(Theme.inkTertiary)
                TextField("", text: Binding(
                    get: { query.search ?? "" },
                    set: { text in model.editListing { $0.query.search = text.isEmpty ? nil : text } }
                ), prompt: Text("Search tasks").foregroundStyle(Theme.inkTertiary))
                    .textFieldStyle(.plain)
                    .font(TypeScale.body(13))
                    .focused($searching)
            }
            .padding(.horizontal, 12)
            .frame(height: 38)
            Hairline()
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    conditions(query)
                    section("Status") {
                        ForEach([WorkStatus.triage, .backlog, .notStarted, .inProgress, .done, .canceled, .duplicate]) { status in
                            option(status.label, on: query.statuses?.contains(status) == true,
                                   count: counts.status[status, default: 0]) {
                                TaskStatusIcon(status: status)
                            } toggle: {
                                model.editListing { $0.query.statuses = TaskFilters.toggled($0.query.statuses, status) }
                            }
                        }
                    }
                    section("Priority") {
                        ForEach([TaskPriority.urgent, .high, .medium, .low, .none]) { priority in
                            option(priority.label, on: query.priorities?.contains(priority.rawValue) == true,
                                   count: counts.priority[priority.rawValue, default: 0]) {
                                PriorityIcon(priority: priority)
                            } toggle: {
                                model.editListing { $0.query.priorities = TaskFilters.toggled($0.query.priorities, priority.rawValue) }
                            }
                        }
                    }
                    section("Owner") {
                        option("Me", on: query.direction == .mine, count: counts.owner["me", default: 0]) {
                            Avatar(face: AvatarFace(kind: .me, name: "Me"), size: 16, revision: model.people.photoRevision)
                        } toggle: {
                            owner(direction: .mine, current: query)
                        }
                        option("No owner", on: query.direction == .unattributed, count: counts.owner["none", default: 0]) {
                            Avatar(face: AvatarFace(kind: .unassigned, name: "No owner"), size: 16, revision: model.people.photoRevision)
                        } toggle: {
                            owner(direction: .unattributed, current: query)
                        }
                        ForEach(store.people.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }) { person in
                            option(person.name, on: query.ownerPersonIds?.contains(person.id) == true,
                                   count: counts.owner[person.id, default: 0]) {
                                Avatar(face: AvatarFace(kind: .person(person.id), name: person.name), size: 16,
                                       revision: model.people.photoRevision)
                            } toggle: {
                                model.editListing {
                                    $0.query.direction = nil
                                    $0.query.ownerPersonIds = TaskFilters.toggled($0.query.ownerPersonIds, person.id)
                                }
                            }
                        }
                    }
                    if !store.projects.isEmpty {
                        section("Project") {
                            ForEach(store.projects.filter { $0.status != .completed }) { project in
                                option(project.title, on: query.projectIds?.contains(project.id) == true,
                                       count: counts.project[project.id, default: 0]) {
                                    symbol("folder")
                                } toggle: {
                                    model.editListing { $0.query.projectIds = TaskFilters.toggled($0.query.projectIds, project.id) }
                                }
                            }
                            option("No project", on: query.projectIds?.contains("none") == true,
                                   count: counts.project["none", default: 0]) {
                                symbol("folder.badge.minus")
                            } toggle: {
                                model.editListing { $0.query.projectIds = TaskFilters.toggled($0.query.projectIds, "none") }
                            }
                        }
                    }
                    if !store.labels.isEmpty {
                        section("Label") {
                            ForEach(store.labels) { label in
                                option(label.name, on: query.labelIds?.contains(label.id) == true,
                                       count: counts.label[label.id, default: 0]) {
                                    Circle().fill(Theme.label(hue: label.hue)).frame(width: 9, height: 9)
                                } toggle: {
                                    model.editListing { $0.query.labelIds = TaskFilters.toggled($0.query.labelIds, label.id) }
                                }
                            }
                        }
                    }
                    section("Due") {
                        ForEach(TaskFilters.dueWindows, id: \.self) { window in
                            option(TaskFilters.title(window), on: query.due?.contains(window) == true) {
                                symbol("calendar")
                            } toggle: {
                                model.editListing { $0.query.due = TaskFilters.toggled($0.query.due, window) }
                            }
                        }
                    }
                    section("Cycle") {
                        ForEach(TaskFilters.cycleChoices, id: \.self) { cycle in
                            option(TaskFilters.title(cycle), on: query.cycle == cycle) {
                                symbol(cycle == .noCycle ? "minus.circle" : "arrow.triangle.2.circlepath")
                            } toggle: {
                                model.editListing { $0.query.cycle = $0.query.cycle == cycle ? nil : cycle }
                            }
                        }
                        ForEach(TaskFilters.cycleScopes) { scope in
                            option(scope.title, on: TaskFilters.scoped(query.filters, scope.value)) {
                                symbol(scope.value == "added" ? "plus.circle" : "checklist")
                            } toggle: {
                                model.editListing { $0.query.filters = TaskFilters.toggledScope($0.query.filters, scope.value) }
                            }
                        }
                    }
                    option("Show snoozed", on: query.includeSnoozed) {
                        symbol("moon.zzz")
                    } toggle: {
                        model.editListing { $0.query.includeSnoozed.toggle() }
                    }
                }
                .padding(.vertical, 10)
            }
            .frame(maxHeight: 440)
            .fixedSize(horizontal: false, vertical: true)
        }
        .frame(width: 300)
        .onAppear { searching = true }
    }

    /// Me and No owner stand alone; people combine with each other.
    private func owner(direction: WorkDirection, current: WorkQuery) {
        model.editListing {
            $0.query.ownerPersonIds = nil
            $0.query.direction = current.direction == direction ? nil : direction
        }
    }

    /// Field, operator and values for each condition, groups where any or
    /// all of their conditions must hold, then a menu to add either.
    private func conditions(_ query: WorkQuery) -> some View {
        let filters = query.filters ?? []
        return section("Conditions") {
            ForEach(Array(filters.enumerated()), id: \.offset) { index, filter in
                filterRow(filter, path: [index])
            }
            addMenu("Add condition", depth: 0) { added in
                model.editListing { $0.query.filters = TaskFilters.adding(added, to: $0.query.filters, at: []) }
            }
        }
    }

    private func filterRow(_ filter: WorkFilter, path: [Int]) -> AnyView {
        switch filter {
        case let .condition(condition):
            AnyView(conditionRow(condition, path: path))
        case let .any(members), let .all(members):
            AnyView(groupBox(filter, members: members, path: path))
        }
    }

    /// A group: whether any or all of it must hold, its filters, and a menu
    /// adding one to it.
    private func groupBox(_ group: WorkFilter, members: [WorkFilter], path: [Int]) -> some View {
        let any = if case .any = group { true } else { false }
        let depth = path.count
        return VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 6) {
                Menu {
                    Button {
                        regroup(path, any: true)
                    } label: {
                        if any { Label("Any of these", systemImage: "checkmark") } else { Text("Any of these") }
                    }
                    Button {
                        regroup(path, any: false)
                    } label: {
                        if any { Text("All of these") } else { Label("All of these", systemImage: "checkmark") }
                    }
                } label: {
                    HStack(spacing: 4) {
                        Text(any ? "Any of these" : "All of these")
                        Image(systemName: "chevron.up.chevron.down").font(.system(size: 9))
                    }
                    .font(TypeScale.body(13).weight(.medium))
                    .foregroundStyle(Theme.inkSecondary)
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .fixedSize()
                .accessibilityLabel(any ? "Group: any of these" : "Group: all of these")
                Spacer(minLength: 0)
                removeButton(path, label: "Remove group")
            }
            .padding(.leading, 14 + CGFloat(depth - 1) * 12)
            .padding(.trailing, 14)
            .frame(height: 28)
            ForEach(Array(members.enumerated()), id: \.offset) { index, member in
                filterRow(member, path: path + [index])
            }
            addMenu(any ? "Or…" : "And…", depth: depth) { added in
                model.editListing { $0.query.filters = TaskFilters.adding(added, to: $0.query.filters, at: path) }
            }
        }
        .overlay(alignment: .leading) {
            Rectangle().fill(Theme.border).frame(width: 2)
                .padding(.leading, 6 + CGFloat(depth - 1) * 12)
                .padding(.vertical, 4)
        }
    }

    private func regroup(_ path: [Int], any: Bool) {
        model.editListing { draft in
            draft.query.filters = TaskFilters.editing(draft.query.filters, at: path) { group in
                guard let members = group?.members else { return }
                group = any ? .any(members) : .all(members)
            }
        }
    }

    /// Every field's choices as a condition, or as a group starting with one.
    private func addMenu(_ title: String, depth: Int, add: @escaping (WorkFilter) -> Void) -> some View {
        Menu {
            fieldMenus { add(.condition($0)) }
            Divider()
            Menu("Any of these…") { fieldMenus { add(.any([.condition($0)])) } }
            Menu("All of these…") { fieldMenus { add(.all([.condition($0)])) } }
        } label: {
            Label(title, systemImage: "plus")
                .font(TypeScale.body(13))
                .foregroundStyle(Theme.inkSecondary)
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .padding(.leading, 14 + CGFloat(depth) * 12)
        .padding(.trailing, 14)
        .frame(height: 28)
        .accessibilityLabel(title)
    }

    private func fieldMenus(_ add: @escaping (WorkCondition) -> Void) -> some View {
        ForEach(WorkConditionField.allCases) { field in
            Menu(field.label) {
                ForEach(TaskFilters.choices(field, model: model)) { choice in
                    Button(choice.title) {
                        add(WorkCondition(field: field, op: TaskFilters.startingOp(field), values: [choice.value]))
                    }
                }
            }
        }
    }

    private func removeButton(_ path: [Int], label: String) -> some View {
        Button {
            model.editListing { $0.query.filters = TaskFilters.editing($0.query.filters, at: path) { $0 = nil } }
        } label: {
            Image(systemName: "xmark").font(.system(size: 9, weight: .bold)).frame(width: 16, height: 16)
        }
        .buttonStyle(.plain)
        .foregroundStyle(Theme.inkTertiary)
        .accessibilityLabel(label)
    }

    private func conditionRow(_ condition: WorkCondition, path: [Int]) -> some View {
        let choices = TaskFilters.choices(condition.field, model: model)
        let names = condition.values.map { value in choices.first { $0.value == value }?.title ?? value }
        return HStack(spacing: 6) {
            Text(condition.field.label)
                .font(TypeScale.body(13).weight(.medium))
                .foregroundStyle(Theme.ink)
            Menu {
                ForEach(condition.field.operators) { op in
                    Button {
                        change(path) { changed in
                            changed.op = op
                            if TaskFilters.takesOneValue(op) { changed.values = Array(changed.values.prefix(1)) }
                        }
                    } label: {
                        if op == condition.op { Label(op.label, systemImage: "checkmark") } else { Text(op.label) }
                    }
                }
            } label: {
                Text(condition.op.label).font(TypeScale.body(13)).foregroundStyle(Theme.inkSecondary)
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .accessibilityLabel("Operator: \(condition.op.label)")
            Menu {
                ForEach(choices) { choice in
                    let on = condition.values.contains(choice.value)
                    Button {
                        pick(choice.value, in: condition, at: path)
                    } label: {
                        if on { Label(choice.title, systemImage: "checkmark") } else { Text(choice.title) }
                    }
                }
            } label: {
                Text(names.isEmpty ? "Choose" : names.joined(separator: ", "))
                    .font(TypeScale.body(13))
                    .foregroundStyle(Theme.ink)
                    .lineLimit(1)
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .accessibilityLabel("Values: \(names.joined(separator: ", "))")
            Spacer(minLength: 0)
            removeButton(path, label: "Remove condition")
        }
        .padding(.leading, 14 + CGFloat(path.count - 1) * 12)
        .padding(.trailing, 14)
        .frame(height: 28)
    }

    /// One-value operators swap the value; the others add or drop it, and
    /// dropping the last removes the condition.
    private func pick(_ value: String, in condition: WorkCondition, at path: [Int]) {
        if TaskFilters.takesOneValue(condition.op) {
            change(path) { $0.values = [value] }
            return
        }
        let values = TaskFilters.toggled(condition.values, value)
        model.editListing { draft in
            draft.query.filters = TaskFilters.editing(draft.query.filters, at: path) { filter in
                guard case var .condition(changed)? = filter else { return }
                changed.values = values ?? []
                filter = values == nil ? nil : .condition(changed)
            }
        }
    }

    private func change(_ path: [Int], _ edit: (inout WorkCondition) -> Void) {
        model.editListing { draft in
            draft.query.filters = TaskFilters.editing(draft.query.filters, at: path) { filter in
                guard case var .condition(changed)? = filter else { return }
                edit(&changed)
                filter = .condition(changed)
            }
        }
    }

    private func section<Content: View>(_ title: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(title)
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(Theme.inkTertiary)
                .padding(.horizontal, 14)
                .padding(.bottom, 4)
                .accessibilityAddTraits(.isHeader)
            content()
        }
    }

    private func option<Mark: View>(
        _ title: String, on: Bool, count: Int? = nil, @ViewBuilder mark: () -> Mark, toggle: @escaping () -> Void
    ) -> some View {
        Button(action: toggle) {
            HStack(spacing: 10) {
                Image(systemName: on ? "checkmark.square.fill" : "square")
                    .font(.system(size: 13))
                    .foregroundStyle(on ? Theme.accent : Theme.inkTertiary)
                mark().frame(width: 16, height: 16)
                Text(title)
                    .font(TypeScale.body(13))
                    .foregroundStyle(Theme.ink)
                    .lineLimit(1)
                Spacer(minLength: 0)
                if let count {
                    Text("\(count)")
                        .font(TypeScale.label(12))
                        .monospacedDigit()
                        .foregroundStyle(Theme.inkTertiary)
                        .accessibilityLabel(count == 1 ? "1 task" : "\(count) tasks")
                }
            }
            .padding(.horizontal, 14)
            .frame(height: 28)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(on ? .isSelected : [])
    }

    private func symbol(_ name: String) -> some View {
        Image(systemName: name).font(.system(size: 12)).foregroundStyle(Theme.inkSecondary)
    }
}

/// How many tasks each choice in the filter panel would keep beside the
/// list's other filters, as Linear counts each value.
@MainActor
private struct FilterCounts {
    let status: [WorkStatus: Int]
    let priority: [Int: Int]
    /// By person id, with `me` and `none` for the two directions.
    let owner: [String: Int]
    /// By project id, with `none` for work in no project.
    let project: [String: Int]
    let label: [String: Int]

    init(model: AppModel, query: WorkQuery) {
        let now = model.work.now
        let cycles = model.work.cycles
        let project = model.listedProject
        let base = (model.work.items ?? []).filter { row in project.map { row.projectId == $0 } ?? true }
        func kept(_ clear: (inout WorkQuery) -> Void) -> [WorkItemRow] {
            var widened = query
            clear(&widened)
            let triage = widened.statuses?.contains(.triage) == true
            let filter = TaskListing.Filter(widened, now: now, cycles: cycles)
            return base.filter { ($0.status != .triage || triage) && filter.matches($0) }
        }
        func tally<Key: Hashable>(_ rows: [WorkItemRow], _ keys: (WorkItemRow) -> [Key]) -> [Key: Int] {
            rows.reduce(into: [:]) { counts, row in
                for key in keys(row) { counts[key, default: 0] += 1 }
            }
        }
        status = tally(kept { $0.statuses = nil }) { [$0.status] }
        priority = tally(kept { $0.priorities = nil }) { [$0.priority] }
        owner = tally(kept { $0.ownerPersonIds = nil; $0.direction = nil }) { row in
            let direction = row.direction == .mine ? ["me"] : row.direction == .unattributed ? ["none"] : []
            return direction + (row.ownerPersonId.map { [$0] } ?? [])
        }
        self.project = tally(kept { $0.projectIds = nil }) { [$0.projectId ?? "none"] }
        label = tally(kept { $0.labelIds = nil }) { $0.labelIds }
    }
}

private extension VerticalAlignment {
    enum FirstChipRow: AlignmentID {
        static func defaultValue(in context: ViewDimensions) -> CGFloat {
            context[VerticalAlignment.center]
        }
    }

    static let firstChipRow = VerticalAlignment(FirstChipRow.self)
}

/// The filters in force, each with its own ✕, and saving them as a view.
struct TaskFilterBar: View {
    let draft: WorkViewDraft
    @Environment(AppModel.self) private var model
    @State private var naming = false
    @State private var name = ""
    @State private var icon: String?
    @State private var unread: [WorkWhereWord] = []

    var body: some View {
        let focus = model.workFocus
        let chips = TaskFilters.chips(draft.query, model: model)
        HStack(alignment: .firstChipRow, spacing: 6) {
            TaskWordsField(unread: $unread)
            ChatFlowRow(spacing: 6) {
                ForEach(unread, id: \.self) { word in
                    TaskUnreadWordChip(word: word) { unread.removeAll { $0 == word } }
                }
                ForEach(chips) { chip in
                    HStack(spacing: 6) {
                        Text(chip.text).lineLimit(1)
                        Button {
                            model.editListing { chip.clear(&$0.query) }
                        } label: {
                            Image(systemName: "xmark")
                                .font(.system(size: 8, weight: .bold))
                                .frame(width: 14, height: 14)
                                .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Remove \(chip.text)")
                    }
                    .font(TypeScale.label(12))
                    .foregroundStyle(Theme.inkSecondary)
                    .padding(.leading, 8)
                    .padding(.trailing, 4)
                    .frame(height: Theme.compactHeight)
                    .background(Theme.control, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
                }
            }
            .padding(.trailing, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
            let opened = openedView
            if model.listedProject == nil, !Self.listsTasks(focus.tab) {
                EmptyView()
            } else if let opened {
                if edited(opened) {
                    Button("Revert") { revert(opened) }
                        .buttonStyle(QuietButton(compact: true))
                    Button("Save") { save(opened) }
                        .buttonStyle(.compact)
                }
                options(opened)
            } else if !chips.isEmpty {
                Button("Clear") { model.editListing { $0.query = WorkQuery() } }
                    .buttonStyle(QuietButton(compact: true))
                Button("Save view") {
                    name = ""
                    icon = nil
                    naming = true
                }
                .buttonStyle(.compact)
                .popover(isPresented: $naming, arrowEdge: .bottom) {
                    ViewNamer(name: $name, icon: $icon, save: create)
                }
            }
        }
        .padding(.leading, 16)
        .padding(.trailing, 24)
        .padding(.vertical, 6)
        .frame(minHeight: 40)
        .overlay(alignment: .bottom) { Hairline() }
    }

    /// Whether a Tasks page tab lists tasks by filters: All and the views.
    private static func listsTasks(_ tab: TasksTab) -> Bool {
        switch tab {
        case .all, .view: true
        case .owe, .waiting, .cycle: false
        }
    }

    /// The saved view the list in front shows: the Tasks page's view tab,
    /// or the view picked on a project's Tasks tab.
    private var openedView: WorkView? {
        let id: String? = if let project = model.listedProject {
            model.workFocus.projectViews[project]
        } else if case let .view(id) = model.workFocus.tab {
            id
        } else {
            nil
        }
        return id.flatMap { id in model.work.views.first { $0.id == id } }
    }

    private func edited(_ view: WorkView) -> Bool {
        draft != WorkViewDraft(query: view.query, display: view.display)
    }

    private func revert(_ view: WorkView) {
        if let project = model.listedProject {
            model.workFocus.projectDrafts[project] = WorkViewDraft(query: view.query, display: view.display)
        } else {
            model.workFocus.viewDrafts[view.id] = nil
        }
    }

    private func options(_ view: WorkView) -> some View {
        let favorite = model.work.isFavorite(.view, view.id)
        return Menu {
            Button(favorite ? "Remove from favorites" : "Add to favorites",
                   systemImage: favorite ? "star.slash" : "star") {
                model.work.setFavorite(.view, view.id, !favorite)
            }
            Menu("Icon") {
                ForEach(ViewNamer.icons, id: \.self) { symbol in
                    Button(symbol, systemImage: symbol) { change(view) { $0.icon = symbol } }
                }
                Button("No icon") { change(view) { $0.icon = nil } }
            }
            Button("Copy link", systemImage: "link") {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(WorkView.link(view.id), forType: .string)
            }
            Button("Delete view", systemImage: "trash", role: .destructive) { delete(view) }
        } label: {
            Image(systemName: favorite ? "star.fill" : "ellipsis")
        }
        .menuStyle(.button)
        .buttonStyle(QuietButton(compact: true))
        .menuIndicator(.hidden)
        .fixedSize()
        .accessibilityLabel("\(view.name) options")
    }

    private func create() {
        let trimmed = name.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return }
        let project = model.listedProject
        let id = model.work.saveView(id: nil, name: trimmed, query: draft.query, display: draft.display,
                                     icon: icon, projectId: project)
        if let project { model.workFocus.projectViews[project] = id }
        naming = false
    }

    private func save(_ view: WorkView) {
        model.work.saveView(id: view.id, name: view.name, query: draft.query, display: draft.display,
                            icon: view.icon, projectId: view.projectId)
        model.workFocus.viewDrafts[view.id] = nil
    }

    /// Saves what the view keeps beside its filters, its filters as saved.
    private func change(_ view: WorkView, _ edit: (inout WorkView) -> Void) {
        var changed = view
        edit(&changed)
        model.work.saveView(id: view.id, name: changed.name, query: view.query, display: view.display,
                            icon: changed.icon, projectId: changed.projectId)
    }

    private func delete(_ view: WorkView) {
        let focus = model.workFocus
        if focus.tab == .view(view.id) { focus.switchTab(.all) }
        if let project = view.projectId, focus.projectViews[project] == view.id {
            focus.projectViews[project] = nil
            focus.projectDrafts[project] = nil
        }
        focus.viewDrafts[view.id] = nil
        model.work.deleteView(view.id)
    }
}

/// A new view's name and the symbol shown beside it.
struct ViewNamer: View {
    @Binding var name: String
    @Binding var icon: String?
    let save: () -> Void

    /// The symbols a view can show; the core keeps any SF Symbol name.
    static let icons = [
        "line.3.horizontal.decrease.circle", "star", "flame", "bolt", "flag", "exclamationmark.triangle",
        "checkmark.circle", "clock", "calendar", "person", "folder", "tag", "ant", "sparkles", "target", "tray",
    ]

    var body: some View {
        let trimmed = name.trimmingCharacters(in: .whitespaces)
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                TextField("", text: $name, prompt: Text("View name").foregroundStyle(Theme.inkTertiary))
                    .textFieldStyle(.plain)
                    .font(TypeScale.body(13))
                    .frame(width: 180)
                    .onSubmit(save)
                Button("Save", action: save)
                    .buttonStyle(PrimaryButton(compact: true))
                    .disabled(trimmed.isEmpty)
            }
            LazyVGrid(columns: Array(repeating: GridItem(.fixed(26), spacing: 4), count: 8), spacing: 4) {
                ForEach(Self.icons, id: \.self) { symbol in
                    let on = icon == symbol
                    Button {
                        icon = on ? nil : symbol
                    } label: {
                        Image(systemName: symbol)
                            .font(.system(size: 12))
                            .foregroundStyle(on ? Theme.accent : Theme.inkSecondary)
                            .frame(width: 26, height: 26)
                            .background(on ? Theme.selection : .clear, in: RoundedRectangle(cornerRadius: Theme.radiusKey))
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .help(symbol)
                    .accessibilityLabel("Icon \(symbol)")
                    .accessibilityAddTraits(on ? .isSelected : [])
                }
            }
        }
        .padding(12)
    }
}

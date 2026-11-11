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
        for (index, condition) in (query.filters ?? []).enumerated() {
            chips.append(TaskFilterChip(id: "condition-\(index)", text: text(condition, model: model)) {
                $0.filters = TaskFilters.removing(index, from: $0.filters)
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

    /// The conditions with the one at `index` gone; nil once none are left.
    static func removing(_ index: Int, from filters: [WorkCondition]?) -> [WorkCondition]? {
        guard var filters, filters.indices.contains(index) else { return filters }
        filters.remove(at: index)
        return filters.isEmpty ? nil : filters
    }

    /// The operator a new condition on `field` starts with: one that takes
    /// several values, so picking more widens it.
    static func startingOp(_ field: WorkConditionField) -> WorkConditionOp {
        field.dated ? .before : field == .label ? .includesAny : .anyOf
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
        let query = model.workFocus.listing(store.views)?.query ?? WorkQuery()
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
                            option(status.label, on: query.statuses?.contains(status) == true) {
                                TaskStatusIcon(status: status)
                            } toggle: {
                                model.editListing { $0.query.statuses = TaskFilters.toggled($0.query.statuses, status) }
                            }
                        }
                    }
                    section("Priority") {
                        ForEach([TaskPriority.urgent, .high, .medium, .low, .none]) { priority in
                            option(priority.label, on: query.priorities?.contains(priority.rawValue) == true) {
                                PriorityIcon(priority: priority)
                            } toggle: {
                                model.editListing { $0.query.priorities = TaskFilters.toggled($0.query.priorities, priority.rawValue) }
                            }
                        }
                    }
                    section("Owner") {
                        option("Me", on: query.direction == .mine) {
                            Avatar(face: AvatarFace(kind: .me, name: "Me"), size: 16, revision: model.people.photoRevision)
                        } toggle: {
                            owner(direction: .mine, current: query)
                        }
                        option("No owner", on: query.direction == .unattributed) {
                            Avatar(face: AvatarFace(kind: .unassigned, name: "No owner"), size: 16, revision: model.people.photoRevision)
                        } toggle: {
                            owner(direction: .unattributed, current: query)
                        }
                        ForEach(store.people.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }) { person in
                            option(person.name, on: query.ownerPersonIds?.contains(person.id) == true) {
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
                                option(project.title, on: query.projectIds?.contains(project.id) == true) {
                                    symbol("folder")
                                } toggle: {
                                    model.editListing { $0.query.projectIds = TaskFilters.toggled($0.query.projectIds, project.id) }
                                }
                            }
                            option("No project", on: query.projectIds?.contains("none") == true) {
                                symbol("folder.badge.minus")
                            } toggle: {
                                model.editListing { $0.query.projectIds = TaskFilters.toggled($0.query.projectIds, "none") }
                            }
                        }
                    }
                    if !store.labels.isEmpty {
                        section("Label") {
                            ForEach(store.labels) { label in
                                option(label.name, on: query.labelIds?.contains(label.id) == true) {
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

    /// Field, operator and values for each condition, then a menu to add one.
    private func conditions(_ query: WorkQuery) -> some View {
        let filters = query.filters ?? []
        return section("Conditions") {
            ForEach(Array(filters.enumerated()), id: \.offset) { index, condition in
                conditionRow(condition, index: index)
            }
            Menu {
                ForEach(WorkConditionField.allCases) { field in
                    Menu(field.label) {
                        ForEach(TaskFilters.choices(field, model: model)) { choice in
                            Button(choice.title) {
                                let added = WorkCondition(field: field, op: TaskFilters.startingOp(field), values: [choice.value])
                                model.editListing { $0.query.filters = ($0.query.filters ?? []) + [added] }
                            }
                        }
                    }
                }
            } label: {
                Label("Add condition", systemImage: "plus")
                    .font(TypeScale.body(13))
                    .foregroundStyle(Theme.inkSecondary)
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .padding(.horizontal, 14)
            .frame(height: 28)
            .accessibilityLabel("Add condition")
        }
    }

    private func conditionRow(_ condition: WorkCondition, index: Int) -> some View {
        let choices = TaskFilters.choices(condition.field, model: model)
        let names = condition.values.map { value in choices.first { $0.value == value }?.title ?? value }
        return HStack(spacing: 6) {
            Text(condition.field.label)
                .font(TypeScale.body(13).weight(.medium))
                .foregroundStyle(Theme.ink)
            Menu {
                ForEach(condition.field.operators) { op in
                    Button {
                        change(index) { changed in
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
                        pick(choice.value, in: condition, at: index)
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
            Button {
                model.editListing { $0.query.filters = TaskFilters.removing(index, from: $0.query.filters) }
            } label: {
                Image(systemName: "xmark").font(.system(size: 9, weight: .bold)).frame(width: 16, height: 16)
            }
            .buttonStyle(.plain)
            .foregroundStyle(Theme.inkTertiary)
            .accessibilityLabel("Remove condition")
        }
        .padding(.horizontal, 14)
        .frame(height: 28)
    }

    /// One-value operators swap the value; the others add or drop it, and
    /// dropping the last removes the condition.
    private func pick(_ value: String, in condition: WorkCondition, at index: Int) {
        if TaskFilters.takesOneValue(condition.op) {
            change(index) { $0.values = [value] }
            return
        }
        let values = TaskFilters.toggled(condition.values, value)
        model.editListing { draft in
            if let values {
                draft.query.filters?[index].values = values
            } else {
                draft.query.filters = TaskFilters.removing(index, from: draft.query.filters)
            }
        }
    }

    private func change(_ index: Int, _ edit: (inout WorkCondition) -> Void) {
        model.editListing { draft in
            guard var filters = draft.query.filters, filters.indices.contains(index) else { return }
            edit(&filters[index])
            draft.query.filters = filters
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
        _ title: String, on: Bool, @ViewBuilder mark: () -> Mark, toggle: @escaping () -> Void
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
            .alignmentGuide(.firstChipRow) { _ in Theme.compactHeight / 2 }
            switch focus.tab {
            case .all:
                if !chips.isEmpty {
                    Button("Clear") { model.editListing { $0.query = WorkQuery() } }
                        .buttonStyle(QuietButton(compact: true))
                    Button("Save view") {
                        name = ""
                        naming = true
                    }
                    .buttonStyle(.compact)
                    .popover(isPresented: $naming, arrowEdge: .bottom) { namer }
                }
            case let .view(id):
                if focus.viewDrafts[id] != nil {
                    Button("Revert") { focus.viewDrafts[id] = nil }
                        .buttonStyle(QuietButton(compact: true))
                    Button("Save") { save(id) }
                        .buttonStyle(.compact)
                }
                if let view = model.work.views.first(where: { $0.id == id }) {
                    let favorite = model.work.isFavorite(.view, id)
                    Menu {
                        Button(favorite ? "Remove from favorites" : "Add to favorites",
                               systemImage: favorite ? "star.slash" : "star") {
                            model.work.setFavorite(.view, id, !favorite)
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
            case .owe, .waiting, .cycle:
                EmptyView()
            }
        }
        .padding(.leading, 16)
        .padding(.trailing, 24)
        .padding(.vertical, 6)
        .frame(minHeight: 40)
        .overlay(alignment: .bottom) { Hairline() }
    }

    private var namer: some View {
        let trimmed = name.trimmingCharacters(in: .whitespaces)
        return HStack(spacing: 8) {
            TextField("", text: $name, prompt: Text("View name").foregroundStyle(Theme.inkTertiary))
                .textFieldStyle(.plain)
                .font(TypeScale.body(13))
                .frame(width: 180)
                .onSubmit(create)
            Button("Save", action: create)
                .buttonStyle(PrimaryButton(compact: true))
                .disabled(trimmed.isEmpty)
        }
        .padding(12)
    }

    private func create() {
        let trimmed = name.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return }
        model.work.saveView(id: nil, name: trimmed, query: draft.query, display: draft.display)
        naming = false
    }

    private func save(_ id: String) {
        guard let view = model.work.views.first(where: { $0.id == id }) else { return }
        model.work.saveView(id: id, name: view.name, query: draft.query, display: draft.display)
        model.workFocus.viewDrafts[id] = nil
    }

    private func delete(_ view: WorkView) {
        let focus = model.workFocus
        if focus.tab == .view(view.id) { focus.switchTab(.all) }
        focus.viewDrafts[view.id] = nil
        model.work.deleteView(view.id)
    }
}

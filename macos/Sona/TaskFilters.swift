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
            chips.append(TaskFilterChip(id: "due", text: "Due before \(PlanningFormat.day(before))") { $0.dueBeforeMs = nil })
        }
        if let meeting = query.sourceMeetingId {
            let title = store.items?.first { $0.sourceMeetingId == meeting }?.sourceMeetingTitle ?? "a meeting"
            chips.append(TaskFilterChip(id: "meeting", text: "From \(title)") { $0.sourceMeetingId = nil })
        }
        if query.includeSnoozed {
            chips.append(TaskFilterChip(id: "snoozed", text: "Snoozed shown") { $0.includeSnoozed = false })
        }
        return chips
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
                        let deadlines = dues
                        ForEach(deadlines.indices, id: \.self) { index in
                            let (title, before) = deadlines[index]
                            option(title, on: query.dueBeforeMs == before) {
                                symbol("calendar")
                            } toggle: {
                                model.editListing { $0.query.dueBeforeMs = $0.query.dueBeforeMs == before ? nil : before }
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

    /// Dates move with the clock, so each is worked out when the panel draws.
    private var dues: [(title: String, before: Int64)] {
        [
            ("Overdue", TaskDates.day(0, hour: 0)),
            ("Due by today", TaskDates.day(1, hour: 0)),
            ("Due this week", TaskDates.next(weekday: 2, hour: 0)),
            ("Due within two weeks", TaskDates.day(15, hour: 0)),
        ]
    }

    /// Me and No owner stand alone; people combine with each other.
    private func owner(direction: WorkDirection, current: WorkQuery) {
        model.editListing {
            $0.query.ownerPersonIds = nil
            $0.query.direction = current.direction == direction ? nil : direction
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

/// The filters in force, each with its own ✕, and saving them as a view.
struct TaskFilterBar: View {
    let draft: WorkViewDraft
    @Environment(AppModel.self) private var model
    @State private var naming = false
    @State private var name = ""

    var body: some View {
        let focus = model.workFocus
        let chips = TaskFilters.chips(draft.query, model: model)
        HStack(spacing: 6) {
            ScrollView(.horizontal) {
                HStack(spacing: 6) {
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
                        .frame(height: 24)
                        .background(Theme.control, in: RoundedRectangle(cornerRadius: Theme.radiusKey))
                    }
                }
            }
            .scrollIndicators(.never)
            .scrollBounceBehavior(.basedOnSize)
            Spacer(minLength: 8)
            switch focus.tab {
            case .all:
                Button("Clear") { model.editListing { $0.query = WorkQuery() } }
                    .buttonStyle(QuietButton(compact: true))
                Button("Save view") {
                    name = ""
                    naming = true
                }
                .buttonStyle(.compact)
                .popover(isPresented: $naming, arrowEdge: .bottom) { namer }
            case let .view(id):
                if focus.viewDrafts[id] != nil {
                    Button("Revert") { focus.viewDrafts[id] = nil }
                        .buttonStyle(QuietButton(compact: true))
                    Button("Save") { save(id) }
                        .buttonStyle(.compact)
                }
            case .owe, .waiting:
                EmptyView()
            }
        }
        .padding(.horizontal, 16)
        .frame(height: 40)
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
}

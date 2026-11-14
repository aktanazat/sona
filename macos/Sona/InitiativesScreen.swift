import SwiftUI

/// Projects and the goals above them, as the start of either place's bar.
struct ProjectsTabs: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        SegmentedSwitch(items: [SwitchItem(id: Place.projects, title: "Projects", icon: "folder"),
                                SwitchItem(id: Place.initiatives, title: "Initiatives", icon: "scope")],
                        selection: model.place == .initiatives ? Place.initiatives : Place.projects) { model.go($0) }
    }
}

/// Initiatives: goals one level above projects, each with its projects'
/// progress, and one initiative's page.
struct InitiativesScreen: View {
    let store: ProjectsStore
    let people: [PlanningPersonChoice]
    let openProject: (String) -> Void
    @State private var newName: String?

    var body: some View {
        Group {
            if let initiative = store.initiatives.first(where: { $0.id == store.selectedInitiativeId }) {
                InitiativePage(initiative: initiative, store: store, people: people, openProject: openProject)
            } else {
                list
            }
        }
        .task {
            if store.projects == nil { await store.refresh() } else { await store.loadInitiatives() }
        }
    }

    private var list: some View {
        Page {
            ErrorNote(store.mutationError)
            if let name = newName {
                Card {
                    HStack(spacing: 8) {
                        TextField("Initiative name", text: Binding(get: { name }, set: { newName = $0 }))
                            .textFieldStyle(.roundedBorder)
                            .onSubmit { create(name) }
                        Button("Cancel") { newName = nil }
                            .buttonStyle(.secondary)
                            .keyboardShortcut(.cancelAction)
                        Button("Create") { create(name) }
                            .buttonStyle(.primary)
                            .disabled(name.trimmingCharacters(in: .whitespaces).isEmpty || store.mutating)
                    }
                    .padding(12)
                }
            }
            Card {
                if store.initiatives.isEmpty {
                    CardLine("No initiatives yet. Click New initiative to group projects under a goal.")
                }
                ForEach(store.initiatives) { initiative in
                    row(initiative)
                }
            }
        }
        .pageBar {
            ProjectsTabs()
        } trailing: {
            Button("New initiative") { newName = "" }
                .buttonStyle(.secondary)
                .disabled(newName != nil)
        }
    }

    private func create(_ name: String) {
        let trimmed = name.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return }
        Task {
            if let made = await store.saveInitiative(nil, PlanningInitiativeSave(name: trimmed)) {
                newName = nil
                store.selectedInitiativeId = made.id
            }
        }
    }

    private func row(_ initiative: PlanningInitiative) -> some View {
        CardRow(action: { store.selectedInitiativeId = initiative.id }) {
            VStack(alignment: .leading, spacing: 2) {
                Text(initiative.name).bodyText(13)
                Text(InitiativeText.summary(initiative, people: people)).metaText().lineLimit(1)
            }
        } trailing: {
            HStack(spacing: 8) {
                Text(InitiativeText.progress(initiative.progress)).metaText(Theme.inkSecondary)
                ProgressRing(fraction: initiative.progress.fraction)
                Text(ProgressRing.percent(initiative.progress.fraction))
                    .metaText(Theme.inkSecondary)
                    .monospacedDigit()
                    .frame(width: 36, alignment: .trailing)
            }
        }
        .accessibilityLabel("\(initiative.name), \(initiative.status.title), \(InitiativeText.progress(initiative.progress)), \(ProgressRing.percent(initiative.progress.fraction))")
    }
}

/// How an initiative reads in a line.
enum InitiativeText {
    static func owner(_ initiative: PlanningInitiative, people: [PlanningPersonChoice]) -> String? {
        switch initiative.owner {
        case .unassigned: nil
        case .myself: "Me"
        case let .person(id): people.first { $0.id == id }?.name ?? "Someone"
        }
    }

    static func summary(_ initiative: PlanningInitiative, people: [PlanningPersonChoice]) -> String {
        var parts = [initiative.status.title]
        if let owner = owner(initiative, people: people) { parts.append(owner) }
        if let target = initiative.targetAtMs { parts.append("Target \(ProjectDates.text(target, nil))") }
        return parts.joined(separator: " · ")
    }

    /// "2 projects · 4 of 10 tasks done": the tasks the percent beside it counts.
    static func progress(_ progress: PlanningInitiative.Progress) -> String {
        let projects = switch progress.projects {
        case 0: "No projects"
        case 1: "1 project"
        default: "\(progress.projects) projects"
        }
        let tasks = progress.openTasks + progress.doneTasks
        return tasks == 0 ? projects : "\(projects) · \(progress.doneTasks) of \(tasks) tasks done"
    }
}

private struct InitiativePage: View {
    let initiative: PlanningInitiative
    let store: ProjectsStore
    let people: [PlanningPersonChoice]
    let openProject: (String) -> Void
    @State private var nameDraft: String?
    @State private var pickingDay = false
    @State private var deleting = false

    var body: some View {
        Page(back: PageBack("Initiatives") { store.selectedInitiativeId = nil }, title: initiative.name) {
            VStack(alignment: .leading, spacing: 6) {
                heading
                Text(InitiativeText.summary(initiative, people: people)).metaText(Theme.inkSecondary)
            }
            .padding(.bottom, 20)
            ErrorNote(store.mutationError)
            Card {
                VStack(alignment: .leading, spacing: 0) {
                    field("Status") { statusMenu }
                    field("Owner") { ownerMenu }
                    field("Target") { targetMenu }
                    field("Progress") {
                        HStack(spacing: 6) {
                            ProgressRing(fraction: initiative.progress.fraction, size: 14)
                            Text("\(InitiativeText.progress(initiative.progress)), \(ProgressRing.percent(initiative.progress.fraction))")
                                .font(TypeScale.label(12.5))
                        }
                    }
                }
                .padding(.vertical, 6)
            }
            .padding(.bottom, 16)
            ProjectDescriptionEditor(text: initiative.description, busy: store.mutating) { text in
                save(PlanningInitiativeSave(description: text.isEmpty ? .clear : .set(text)))
            }
            .padding(.bottom, 20)
            HStack {
                Text("Projects").sectionLabel()
                Spacer()
                addProjectMenu
            }
            .padding(.bottom, 8)
            Card {
                if projects.isEmpty {
                    CardLine("No projects yet. Add one to count its progress here.")
                }
                ForEach(projects) { summary in
                    projectRow(summary)
                }
            }
        } actions: {
            Button("Delete") { deleting = true }
                .buttonStyle(.secondary)
                .confirmationDialog("Delete “\(initiative.name)”?", isPresented: $deleting) {
                    Button("Delete initiative", role: .destructive) {
                        Task {
                            await store.deleteInitiative(initiative)
                            store.selectedInitiativeId = nil
                        }
                    }
                } message: {
                    Text("Its projects stay, in no initiative.")
                }
        }
    }

    @ViewBuilder
    private var heading: some View {
        if let draft = nameDraft {
            TextField("Initiative name", text: Binding(get: { draft }, set: { nameDraft = $0 }))
                .font(TypeScale.label(22))
                .textFieldStyle(.plain)
                .onSubmit {
                    let trimmed = draft.trimmingCharacters(in: .whitespaces)
                    nameDraft = nil
                    if !trimmed.isEmpty, trimmed != initiative.name { save(PlanningInitiativeSave(name: trimmed)) }
                }
                .onExitCommand { nameDraft = nil }
        } else {
            Button { nameDraft = initiative.name } label: {
                Text(initiative.name).heroText().lineLimit(2)
            }
            .buttonStyle(.plain)
            .help("Rename")
            .accessibilityAddTraits(.isHeader)
            .pageHeadline()
        }
    }

    private var projects: [PlanningProjectSummary] {
        let summaries = store.projects ?? []
        return initiative.projectIds.compactMap { id in summaries.first { $0.id == id } }
    }

    private func projectRow(_ summary: PlanningProjectSummary) -> some View {
        let done = Int(summary.completedTaskCount)
        let total = done + Int(summary.openTaskCount)
        let fraction = total == 0 ? 0 : Double(done) / Double(total)
        return CardRow(action: { openProject(summary.id) }) {
            HStack(spacing: 8) {
                ProjectStatusIcon(status: summary.project.status, progress: fraction)
                VStack(alignment: .leading, spacing: 2) {
                    Text(summary.project.title).bodyText(13)
                    Text(store.statusTitle(of: summary.project)).metaText().lineLimit(1)
                }
            }
        } trailing: {
            HStack(spacing: 8) {
                Text(total == 0 ? "No tasks" : "\(done) of \(total) tasks").metaText(Theme.inkSecondary)
                Button { Task { await store.removeFromInitiative(projectId: summary.id) } } label: {
                    Image(systemName: "xmark").font(.system(size: 9, weight: .bold))
                }
                .buttonStyle(.plain)
                .foregroundStyle(Theme.inkTertiary)
                .help("Take out of this initiative")
                .accessibilityLabel("Take \(summary.project.title) out of \(initiative.name)")
            }
        }
    }

    private var addProjectMenu: some View {
        let inside = Set(initiative.projectIds)
        let choices = (store.projects ?? []).map(\.project).filter { !inside.contains($0.id) }
        return Menu {
            ForEach(choices) { project in
                let elsewhere = store.initiative(of: project.id)
                Button(elsewhere.map { "\(project.title) (in \($0.name))" } ?? project.title) {
                    Task { await store.addToInitiative(initiative.id, projectId: project.id) }
                }
            }
        } label: {
            Label("Add project", systemImage: "plus").font(TypeScale.label(12.5))
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
        .disabled(choices.isEmpty)
        .help("Put a project in this initiative")
    }

    private func field<Value: View>(_ title: String, @ViewBuilder value: () -> Value) -> some View {
        HStack(spacing: 8) {
            Text(title).metaText().frame(width: 64, alignment: .leading)
            value()
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 12)
        .frame(minHeight: 30)
    }

    private func menuLabel(_ text: String, muted: Bool = false) -> some View {
        Text(text)
            .font(TypeScale.label(12.5))
            .foregroundStyle(muted ? Theme.inkTertiary : Theme.ink)
            .lineLimit(1)
    }

    private var statusMenu: some View {
        Menu {
            ForEach(PlanningInitiativeStatus.allCases) { status in
                Button(status.title) { save(PlanningInitiativeSave(status: status)) }
                    .disabled(status == initiative.status)
            }
        } label: {
            menuLabel(initiative.status.title)
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .accessibilityLabel("Status: \(initiative.status.title)")
    }

    private var ownerMenu: some View {
        let owner = InitiativeText.owner(initiative, people: people)
        return Menu {
            Button("No owner") { save(PlanningInitiativeSave(owner: .unassigned)) }
                .disabled(initiative.owner == .unassigned)
            Divider()
            Button("Me") { save(PlanningInitiativeSave(owner: .myself)) }
                .disabled(initiative.owner == .myself)
            ForEach(people) { person in
                Button(person.name) { save(PlanningInitiativeSave(owner: .person(person.id))) }
                    .disabled(initiative.owner == .person(person.id))
            }
        } label: {
            menuLabel(owner ?? "No owner", muted: owner == nil)
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .accessibilityLabel("Owner: \(owner ?? "none")")
    }

    private var targetMenu: some View {
        Menu {
            Button("Pick a day…") { pickingDay = true }
            if initiative.targetAtMs != nil {
                Button("Clear target date") { save(PlanningInitiativeSave(targetAtMs: .clear)) }
            }
        } label: {
            menuLabel(initiative.targetAtMs.map { ProjectDates.text($0, nil) } ?? "None", muted: initiative.targetAtMs == nil)
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .popover(isPresented: $pickingDay, arrowEdge: .leading) {
            ProjectDayPicker(start: initiative.targetAtMs.map(PlanningFormat.moment) ?? .now) { day in
                pickingDay = false
                save(PlanningInitiativeSave(targetAtMs: .set(ProjectsStore.milliseconds(day))))
            }
        }
        .accessibilityLabel("Target date")
    }

    private func save(_ change: PlanningInitiativeSave) {
        Task { await store.saveInitiative(initiative, change) }
    }
}

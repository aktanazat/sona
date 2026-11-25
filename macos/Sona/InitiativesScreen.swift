import AppKit
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
    let labels: [WorkLabel]
    let openProject: (String) -> Void
    @State private var newName: String?

    var body: some View {
        Group {
            if let initiative = store.initiatives.first(where: { $0.id == store.selectedInitiativeId }) {
                InitiativePage(initiative: initiative, store: store, people: people, labels: labels, openProject: openProject)
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
        let priority = TaskPriority(initiative.priority)
        let parentName = initiative.parentId.flatMap { pid in store.initiatives.first { $0.id == pid }?.name }
        return CardRow(action: { store.selectedInitiativeId = initiative.id }) {
            HStack(spacing: 8) {
                if priority != .none { PriorityIcon(priority: priority) }
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        if let parentName {
                            Text("\(parentName) ›").metaText(Theme.inkTertiary).lineLimit(1)
                        }
                        Text(initiative.name).bodyText(13)
                    }
                    Text(InitiativeText.summary(initiative, people: people)).metaText().lineLimit(1)
                }
            }
        } trailing: {
            HStack(spacing: 8) {
                if let health = initiative.latestUpdate?.health {
                    ProjectHealthPill(health: health, compact: true)
                }
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
        let priority = TaskPriority(initiative.priority)
        if priority != .none { parts.append(priority.label) }
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

private enum InitiativeTab: String, CaseIterable, Hashable {
    case overview, updates

    var title: String {
        switch self {
        case .overview: "Overview"
        case .updates: "Updates"
        }
    }

    static var items: [SwitchItem<InitiativeTab>] { allCases.map { SwitchItem(id: $0, title: $0.title) } }
}

private struct InitiativePage: View {
    let initiative: PlanningInitiative
    let store: ProjectsStore
    let people: [PlanningPersonChoice]
    let labels: [WorkLabel]
    let openProject: (String) -> Void
    @State private var tab: InitiativeTab = .overview
    @State private var nameDraft: String?
    @State private var pickingDay = false
    @State private var deleting = false

    var body: some View {
        Page(back: PageBack("Initiatives") { store.selectedInitiativeId = nil }, title: initiative.name) {
            VStack(alignment: .leading, spacing: 10) {
                if let parent = parentInitiative {
                    Button { store.selectedInitiativeId = parent.id } label: {
                        Label(parent.name, systemImage: "scope")
                            .font(TypeScale.label(12))
                            .foregroundStyle(Theme.inkSecondary)
                    }
                    .buttonStyle(.plain)
                }
                heading
                HStack(spacing: 10) {
                    Text(InitiativeText.summary(initiative, people: people)).metaText(Theme.inkSecondary)
                    if let health = initiative.latestUpdate?.health {
                        ProjectHealthPill(health: health)
                    }
                }
                SegmentedSwitch(items: InitiativeTab.items, selection: tab) { tab = $0 }
                    .padding(.top, 4)
            }
            .padding(.bottom, 20)
            ErrorNote(store.mutationError)
            switch tab {
            case .overview:
                overviewContent
            case .updates:
                InitiativeUpdatesView(store: store, initiative: initiative)
            }
        } actions: {
            SubjectReminderMenu(subject: WorkSubjectRef(kind: .initiative, id: initiative.id))
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
        .onChange(of: initiative.id) { _, _ in
            nameDraft = nil
        }
    }

    @ViewBuilder
    private var overviewContent: some View {
        Card {
            VStack(alignment: .leading, spacing: 0) {
                field("Status") { statusMenu }
                field("Priority") { priorityMenu }
                field("Parent") { parentMenu }
                field("Owner") { ownerMenu }
                field("Target") { targetMenu }
                field("Labels") { labelsMenu }
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
        if !children.isEmpty {
            HStack {
                Text("Sub-initiatives").sectionLabel()
                Spacer()
            }
            .padding(.bottom, 8)
            Card {
                ForEach(children) { child in
                    CardRow(action: { store.selectedInitiativeId = child.id }) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(child.name).bodyText(13)
                            Text(InitiativeText.summary(child, people: people)).metaText().lineLimit(1)
                        }
                    } trailing: {
                        HStack(spacing: 8) {
                            ProgressRing(fraction: child.progress.fraction)
                            Text(ProgressRing.percent(child.progress.fraction))
                                .metaText(Theme.inkSecondary)
                                .monospacedDigit()
                        }
                    }
                }
            }
            .padding(.bottom, 20)
        }
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
        PlanningCommentsView(threads: store.comments,
                             subject: PlanningCommentSubject(kind: .initiative, id: initiative.id))
            .padding(.top, 24)
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

    private var parentInitiative: PlanningInitiative? {
        initiative.parentId.flatMap { id in store.initiatives.first { $0.id == id } }
    }

    private var children: [PlanningInitiative] {
        initiative.childIds.compactMap { id in store.initiatives.first { $0.id == id } }
    }

    private var descendantIds: Set<String> {
        var seen: Set<String> = [initiative.id]
        var queue = initiative.childIds
        while let next = queue.popLast() {
            if seen.insert(next).inserted, let found = store.initiatives.first(where: { $0.id == next }) {
                queue.append(contentsOf: found.childIds)
            }
        }
        return seen
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
                ProjectStatusIcon(status: summary.project.status, progress: fraction,
                                  hue: store.statusName(of: summary.project)?.hue)
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

    private var priorityMenu: some View {
        let current = TaskPriority(initiative.priority)
        return ChoicePicker(TaskPriority.menuOrder, current: current, title: \.label,
                            accessibility: "Priority: \(current.label)",
                            pick: { save(PlanningInitiativeSave(priority: $0.rawValue)) },
                            icon: { PriorityIcon(priority: $0) }) {
            HStack(spacing: 6) {
                PriorityIcon(priority: current)
                menuLabel(current.label, muted: current == .none)
            }
        }
    }

    private var parentMenu: some View {
        let blocked = descendantIds
        let choices = store.initiatives.filter { !blocked.contains($0.id) }
        return Menu {
            Button("No parent") { save(PlanningInitiativeSave(parentId: .clear)) }
                .disabled(initiative.parentId == nil)
            if !choices.isEmpty { Divider() }
            ForEach(choices) { candidate in
                Button(candidate.name) { save(PlanningInitiativeSave(parentId: .set(candidate.id))) }
                    .disabled(initiative.parentId == candidate.id)
            }
        } label: {
            menuLabel(parentInitiative?.name ?? "None", muted: parentInitiative == nil)
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .accessibilityLabel("Parent initiative: \(parentInitiative?.name ?? "none")")
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

    private var labelsMenu: some View {
        let chosen = initiative.labelIds
        let names = labels.filter { chosen.contains($0.id) }
        return HStack(spacing: 5) {
            ForEach(names) { label in
                Circle().fill(Theme.label(hue: label.hue)).frame(width: 7, height: 7)
            }
            Menu {
                let offered = labels.filter { $0.scope != .tasks || chosen.contains($0.id) }
                if offered.isEmpty { Text(labels.isEmpty ? "No labels yet" : "No project labels") }
                ForEach(offered) { label in
                    let on = chosen.contains(label.id)
                    Button {
                        save(PlanningInitiativeSave(labelIds: on ? chosen.filter { $0 != label.id } : chosen + [label.id]))
                    } label: {
                        if on { Label(label.name, systemImage: "checkmark") } else { Text(label.name) }
                    }
                }
            } label: {
                menuLabel(names.isEmpty ? "None" : names.map(\.name).joined(separator: ", "), muted: names.isEmpty)
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .accessibilityLabel("Labels")
        }
    }

    private func save(_ change: PlanningInitiativeSave) {
        Task { await store.saveInitiative(initiative, change) }
    }
}

/// An initiative's updates, newest first, with a composer, Copy as Markdown,
/// and per-update comment threads.
private struct InitiativeUpdatesView: View {
    let store: ProjectsStore
    let initiative: PlanningInitiative
    @State private var health: PlanningProjectHealth = .onTrack
    @State private var draft = ""
    @State private var error: String?
    @State private var editing: String?
    @State private var editHealth: PlanningProjectHealth = .onTrack
    @State private var editBody = ""
    @State private var editError: String?
    @State private var openComments: Set<String> = []

    var body: some View {
        let now = ProjectsStore.milliseconds(.now)
        VStack(alignment: .leading, spacing: 16) {
            composer
            if let message = store.initiativeUpdatesError {
                PlannerFailure(message: message) { Task { await store.loadInitiativeUpdates(initiative.id) } }
            }
            if let updates = store.initiativeUpdates[initiative.id] {
                VStack(alignment: .leading, spacing: 10) {
                    if updates.isEmpty { Card { CardLine("No updates yet.") } }
                    ForEach(updates) { update in
                        row(update, now: now)
                    }
                    if store.initiativeUpdatesCursor[initiative.id] != nil {
                        Button("Show older updates") { Task { await store.loadInitiativeUpdates(initiative.id, more: true) } }
                            .buttonStyle(.quiet)
                    }
                }
            } else {
                PlannerLoadingRows(label: "Loading updates")
            }
        }
        .task(id: initiative.id) {
            await store.loadInitiativeUpdates(initiative.id)
            if let latest = initiative.latestUpdate?.health { health = latest }
        }
    }

    private var composer: some View {
        VStack(alignment: .leading, spacing: 10) {
            healthChoices($health)
            TextEditor(text: $draft)
                .font(TypeScale.body(13.5))
                .scrollContentBackground(.hidden)
                .padding(8)
                .frame(minHeight: 90)
                .background(Theme.control, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
                .overlay(alignment: .topLeading) {
                    if draft.isEmpty {
                        Text("How this initiative is going")
                            .bodyText(13.5, Theme.inkTertiary)
                            .padding(.horizontal, 13)
                            .padding(.vertical, 8)
                            .allowsHitTesting(false)
                    }
                }
                .accessibilityLabel("Initiative update")
            HStack {
                if let error { Text(error).metaText(Theme.live).lineLimit(2) }
                Spacer()
                Button("Post update") {
                    let body = draft.trimmingCharacters(in: .whitespacesAndNewlines)
                    Task {
                        do {
                            try await store.postInitiativeUpdate(initiativeId: initiative.id, health: health, body: body)
                            draft = ""
                            error = nil
                        } catch {
                            self.error = error.localizedDescription
                        }
                    }
                }
                .buttonStyle(.primary)
                .keyboardShortcut(editing == nil ? KeyboardShortcut(.return, modifiers: .command) : nil)
                .disabled(store.mutating || draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(14)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusCard, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.radiusCard, style: .continuous).strokeBorder(Theme.border, lineWidth: 1))
    }

    private func healthChoices(_ selection: Binding<PlanningProjectHealth>) -> some View {
        HStack(spacing: 8) {
            ForEach(PlanningProjectHealth.allCases) { choice in
                Button { selection.wrappedValue = choice } label: {
                    HStack(spacing: 5) {
                        Circle().fill(choice.tint).frame(width: 7, height: 7)
                        Text(choice.label).font(TypeScale.label(12.5))
                    }
                    .padding(.horizontal, 10)
                    .frame(height: Theme.compactHeight)
                    .foregroundStyle(Theme.ink)
                    .background(selection.wrappedValue == choice ? Theme.selection : Theme.control,
                                in: RoundedRectangle(cornerRadius: Theme.radiusControl))
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(selection.wrappedValue == choice ? .isSelected : [])
            }
        }
    }

    @ViewBuilder
    private func row(_ update: PlanningInitiativeUpdate, now: Int64) -> some View {
        if editing == update.id {
            editor(update)
        } else {
            let author = switch update.author.kind {
            case .me: "You"
            case .sona: "Sona"
            case .agent: update.author.name ?? "An agent"
            }
            let showingComments = openComments.contains(update.id)
            let subject = PlanningCommentSubject(kind: .update, id: update.id)
            let commentCount = store.comments.threads[subject]?.count ?? 0
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 8) {
                    ProjectHealthPill(health: update.health)
                    Text(author).font(TypeScale.label(12.5).weight(.medium)).foregroundStyle(Theme.ink)
                    Text(ProjectDates.relative(update.createdAtMs, now: now))
                        .metaText()
                        .help(PlanningFormat.date(update.createdAtMs))
                    if update.editedAtMs != nil { Text("Edited").metaText() }
                    Spacer()
                    Text("\(update.progress)%").metaText().monospacedDigit()
                }
                if let body = update.body, !body.isEmpty {
                    MarkdownBody(source: body, toggle: nil)
                        .textSelection(.enabled)
                }
                HStack(spacing: 12) {
                    Button {
                        if showingComments { openComments.remove(update.id) } else { openComments.insert(update.id) }
                    } label: {
                        Label(commentCount > 0 ? "Comments (\(commentCount))" : "Comments", systemImage: "bubble.left")
                            .font(TypeScale.label(12))
                            .foregroundStyle(Theme.inkSecondary)
                    }
                    .buttonStyle(.plain)
                    Button {
                        copyUpdateMarkdown(title: initiative.name, health: update.health, progress: update.progress,
                                           createdAtMs: update.createdAtMs, body: update.body)
                    } label: {
                        Label("Copy as Markdown", systemImage: "doc.on.doc")
                            .font(TypeScale.label(12))
                            .foregroundStyle(Theme.inkSecondary)
                    }
                    .buttonStyle(.plain)
                    Spacer()
                }
                .padding(.top, 2)
                if showingComments {
                    Divider()
                    PlanningCommentsView(threads: store.comments, subject: subject)
                }
            }
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusCard, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: Theme.radiusCard, style: .continuous).strokeBorder(Theme.border, lineWidth: 1))
            .contextMenu {
                Button("Copy as Markdown") {
                    copyUpdateMarkdown(title: initiative.name, health: update.health, progress: update.progress,
                                       createdAtMs: update.createdAtMs, body: update.body)
                }
                if update.author.kind == .me {
                    Button("Edit update") {
                        editHealth = update.health
                        editBody = update.body ?? ""
                        editError = nil
                        editing = update.id
                    }
                    Button("Delete update", role: .destructive) { Task { await store.deleteInitiativeUpdate(update) } }
                }
            }
        }
    }

    private func editor(_ update: PlanningInitiativeUpdate) -> some View {
        let body = editBody.trimmingCharacters(in: .whitespacesAndNewlines)
        return VStack(alignment: .leading, spacing: 10) {
            healthChoices($editHealth)
            TextEditor(text: $editBody)
                .font(TypeScale.body(13.5))
                .scrollContentBackground(.hidden)
                .padding(8)
                .frame(minHeight: 90)
                .background(Theme.control, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
                .accessibilityLabel("Initiative update")
            HStack(spacing: 8) {
                if let editError { Text(editError).metaText(Theme.live).lineLimit(2) }
                Spacer()
                Button("Cancel") { editing = nil }
                    .buttonStyle(.secondary)
                    .keyboardShortcut(.cancelAction)
                Button("Save update") {
                    Task {
                        do {
                            try await store.editInitiativeUpdate(update, health: editHealth, body: body)
                            editing = nil
                        } catch {
                            editError = error.localizedDescription
                        }
                    }
                }
                .buttonStyle(.primary)
                .keyboardShortcut(.return, modifiers: .command)
                .disabled(store.mutating || body.isEmpty)
            }
        }
        .padding(14)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusCard, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.radiusCard, style: .continuous).strokeBorder(Theme.accent, lineWidth: 1))
    }
}

/// Copies a project or initiative update to the pasteboard as Markdown.
func copyUpdateMarkdown(title: String, health: PlanningProjectHealth, progress: Int, createdAtMs: Int64, body: String?) {
    let date = PlanningFormat.day(createdAtMs)
    let header = "## \(title) — \(health.label) (\(progress)%) · \(date)"
    let text = body.map { "\(header)\n\n\($0)" } ?? header
    NSPasteboard.general.clearContents()
    NSPasteboard.general.setString(text, forType: .string)
}

import SwiftUI

/// C: a new task, its properties set before it exists. Return in the title
/// or ⌘Return anywhere creates it; Esc closes it.
struct TaskComposer: View {
    let start: WorkPatch
    @Environment(AppModel.self) private var model
    @State private var title = ""
    @State private var details = ""
    @State private var status = WorkStatus.notStarted
    @State private var priority = 0
    @State private var owner = WorkOwner.myself
    @State private var due: Int64?
    @State private var project: String?
    @State private var labels: [String] = []
    @State private var creating = false
    @State private var failure: String?
    @FocusState private var titleFocused: Bool
    @State private var reading = TitleReading()
    @State private var picked: Set<Choice> = []

    /// Fields set in their pickers: the title's words no longer set them.
    private enum Choice { case priority, owner, due, project }

    var body: some View {
        let work = model.work
        let ready = !title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && !creating
        VStack(alignment: .leading, spacing: 0) {
            Text("New task")
                .font(TypeScale.label(13))
                .foregroundStyle(Theme.inkSecondary)
                .padding(.horizontal, 20)
                .padding(.top, 18)
            TextField("", text: $title, prompt: Text("Title").foregroundStyle(Theme.inkDisabled))
                .textFieldStyle(.plain)
                .font(.system(size: 18, weight: .semibold))
                .foregroundStyle(Theme.ink)
                .focused($titleFocused)
                .onSubmit(create)
                .chipKeys(reading, text: title, work: work)
                .task(id: title) { await reading.read(title, in: work) }
                .padding(.horizontal, 20)
                .padding(.top, 10)
            if !reading.tokens.isEmpty {
                ParsedChips(reading: reading, text: title)
                    .padding(.horizontal, 20)
                    .padding(.top, 8)
            }
            TextField("", text: $details, prompt: Text("Details").foregroundStyle(Theme.inkDisabled), axis: .vertical)
                .textFieldStyle(.plain)
                .font(TypeScale.body(14))
                .foregroundStyle(Theme.ink)
                .lineLimit(3...10)
                .padding(.horizontal, 20)
                .padding(.top, 10)
            HStack(spacing: 6) {
                ComposerChip(title: status.label, prompt: "Status…") {
                    TaskStatusIcon(status: status)
                } make: { text in
                    WorkChoices.filtered(
                        WorkChoices.statuses { $0 == status } pick: { status = $0 }, by: text, kind: .status)
                }
                ComposerChip(title: chosenPriority == 0 ? "Priority" : TaskPriority(chosenPriority).label, prompt: "Priority…") {
                    PriorityIcon(priority: TaskPriority(chosenPriority))
                } make: { text in
                    WorkChoices.filtered(
                        WorkChoices.priorities { $0 == chosenPriority } pick: { priority = $0; picked.insert(.priority) },
                        by: text, kind: .priority)
                }
                ComposerChip(title: ownerName(work.people), prompt: "Assign to…") {
                    Avatar(face: face(work.people), size: 16, revision: model.people.photoRevision)
                } make: { text in
                    WorkChoices.filtered(
                        WorkChoices.owners(work.people) { $0 == chosenOwner } pick: { owner = $0; picked.insert(.owner) },
                        by: text, kind: .owner)
                }
                ComposerChip(title: chosenDue.map { TaskDates.label($0, now: TaskDates.now()) } ?? "Due date", prompt: "Due date, or type one…") {
                    Image(systemName: "calendar").font(.system(size: 11)).foregroundStyle(Theme.inkSecondary)
                } make: { text in
                    WorkChoices.dues(text, clearable: chosenDue != nil) { candidate in
                        chosenDue.map { PlanningFormat.days(from: $0, to: candidate) == 0 } ?? false
                    } pick: { due = $0; picked.insert(.due) }
                }
                ComposerChip(title: work.projects.first { $0.id == chosenProject }?.title ?? "Project", prompt: "Project…") {
                    Image(systemName: "folder").font(.system(size: 11)).foregroundStyle(Theme.inkSecondary)
                } make: { text in
                    WorkChoices.filtered(
                        WorkChoices.projects(work.projects) { $0 == chosenProject } pick: { project = $0; picked.insert(.project) },
                        by: text, kind: .project)
                }
                ComposerChip(title: labelTitle(work.labels), prompt: "Labels…", keepsOpen: true) {
                    Image(systemName: "tag").font(.system(size: 11)).foregroundStyle(Theme.inkSecondary)
                } make: { text in
                    WorkChoices.filtered(
                        WorkChoices.labels(work.labels, text: text) { labelChecked($0) } toggle: { label in
                            toggleLabel(label)
                        } create: { name in
                            labels.append(work.makeLabel(named: name))
                        }, by: text, kind: .labels)
                }
            }
            .padding(.horizontal, 20)
            .padding(.top, 16)
            .padding(.bottom, 18)
            Hairline()
            HStack(spacing: 12) {
                if let failure {
                    Text(failure).metaText(Theme.live).lineLimit(2)
                }
                Spacer(minLength: 0)
                Button("Cancel") { model.workFocus.composing = nil }
                    .buttonStyle(.quiet)
                    .keyboardShortcut(reading.selected == nil ? KeyboardShortcut.cancelAction : nil)
                Button(action: create) {
                    HStack(spacing: 8) {
                        Text("Create task")
                        Text("⌘↩").foregroundStyle(Theme.onAccent.opacity(0.7))
                    }
                }
                .buttonStyle(.primary)
                .keyboardShortcut(.return, modifiers: .command)
                .disabled(!ready)
            }
            .padding(.horizontal, 20)
            .frame(height: 56)
        }
        .frame(width: 620)
        .background(Theme.elevated)
        .onAppear {
            take(start)
            DispatchQueue.main.async { titleFocused = true }
        }
    }

    private func take(_ start: WorkPatch) {
        if let value = start.title { title = value }
        if case let .set(value)? = start.details { details = value }
        if let value = start.status { status = value }
        if let value = start.priority { priority = value }
        if let value = start.owner { owner = value }
        if case let .set(value)? = start.dueAtMs { due = value }
        if case let .set(value)? = start.projectId { project = value }
        if let value = start.labelIds { labels = value }
    }

    private func ownerName(_ people: [WorkPersonChoice]) -> String {
        switch chosenOwner {
        case .myself: "Me"
        case let .person(id): people.first { $0.id == id }?.name ?? "Someone"
        case .nobody: "No owner"
        }
    }

    private func face(_ people: [WorkPersonChoice]) -> AvatarFace {
        switch chosenOwner {
        case .myself: AvatarFace(kind: .me, name: "Me")
        case let .person(id): AvatarFace(kind: .person(id), name: ownerName(people))
        case .nobody: AvatarFace(kind: .unassigned, name: "No owner")
        }
    }

    private func labelTitle(_ all: [WorkLabel]) -> String {
        let words = self.words
        let ids = labels + words.labelIds.filter { !labels.contains($0) }
        let names = ids.compactMap { id in all.first { $0.id == id }?.name } + words.newLabels
        return switch names.count {
        case 0: "Labels"
        case 1, 2: names.joined(separator: ", ")
        default: "\(names.count) labels"
        }
    }

    private var words: TitleFields { TitleFields(reading.parse) }
    private var chosenPriority: Int { picked.contains(.priority) ? priority : words.priority ?? priority }
    private var chosenOwner: WorkOwner { picked.contains(.owner) ? owner : words.owner ?? owner }
    private var chosenDue: Int64? { picked.contains(.due) ? due : words.due ?? due }
    private var chosenProject: String? { picked.contains(.project) ? project : words.project ?? project }

    private func labelChecked(_ id: String) -> Bool {
        labels.contains(id) || words.labelIds.contains(id)
    }

    /// A label the title named is unchecked by keeping its words as text.
    private func toggleLabel(_ id: String) {
        guard labelChecked(id) else {
            labels.append(id)
            return
        }
        labels.removeAll { $0 == id }
        if let token = reading.labelToken(id) {
            let text = title
            Task { await reading.keepAsText(token, in: text, work: model.work) }
        }
    }

    private func create() {
        let trimmed = title.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, !creating else { return }
        creating = true
        failure = nil
        let text = details.trimmingCharacters(in: .whitespacesAndNewlines)
        let typed = title
        Task {
            let work = model.work
            let words = TitleFields(await reading.settled(typed, in: work))
            let item = await work.create(
                title: words.title ?? trimmed, details: text.isEmpty ? nil : text, status: status,
                priority: chosenPriority, projectId: chosenProject, owner: chosenOwner, dueAtMs: chosenDue,
                labelIds: labels + words.labels(making: work).filter { !labels.contains($0) },
                recurrence: words.recurrence)
            creating = false
            guard let item else {
                failure = model.work.mutationError ?? "The task wasn't created."
                return
            }
            model.workFocus.highlightWhenShown(item.id)
            model.workFocus.composing = nil
        }
    }
}

/// One property of the new task: its value, and the list to change it.
private struct ComposerChip<Mark: View>: View {
    let title: String
    let prompt: String
    var keepsOpen = false
    @ViewBuilder let mark: () -> Mark
    let make: (String) -> [WorkChoice]
    @State private var shown = false
    @Environment(AppModel.self) private var model

    var body: some View {
        Button {
            shown = true
        } label: {
            HStack(spacing: 6) {
                mark().frame(width: 16, height: 16)
                Text(title).lineLimit(1)
            }
            .font(TypeScale.label(12))
            .foregroundStyle(Theme.ink)
            .padding(.leading, 6)
            .padding(.trailing, 9)
            .frame(height: 26)
            .background(Theme.control, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .popover(isPresented: $shown, arrowEdge: .bottom) {
            ChoiceList(prompt: prompt, keepsOpen: keepsOpen, compact: true, make: make) { shown = false }
                .padding(.vertical, 4)
                .environment(model)
        }
    }
}

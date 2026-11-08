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
                .padding(.horizontal, 20)
                .padding(.top, 10)
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
                ComposerChip(title: priority == 0 ? "Priority" : TaskPriority(priority).label, prompt: "Priority…") {
                    PriorityIcon(priority: TaskPriority(priority))
                } make: { text in
                    WorkChoices.filtered(
                        WorkChoices.priorities { $0 == priority } pick: { priority = $0 }, by: text, kind: .priority)
                }
                ComposerChip(title: ownerName(work.people), prompt: "Assign to…") {
                    Avatar(face: face(work.people), size: 16, revision: model.people.photoRevision)
                } make: { text in
                    WorkChoices.filtered(
                        WorkChoices.owners(work.people) { $0 == owner } pick: { owner = $0 }, by: text, kind: .owner)
                }
                ComposerChip(title: due.map { TaskDates.label($0, now: TaskDates.now()) } ?? "Due date", prompt: "Due date, or type one…") {
                    Image(systemName: "calendar").font(.system(size: 11)).foregroundStyle(Theme.inkSecondary)
                } make: { text in
                    WorkChoices.dues(text, clearable: due != nil) { candidate in
                        due.map { PlanningFormat.days(from: $0, to: candidate) == 0 } ?? false
                    } pick: { due = $0 }
                }
                ComposerChip(title: work.projects.first { $0.id == project }?.title ?? "Project", prompt: "Project…") {
                    Image(systemName: "folder").font(.system(size: 11)).foregroundStyle(Theme.inkSecondary)
                } make: { text in
                    WorkChoices.filtered(
                        WorkChoices.projects(work.projects) { $0 == project } pick: { project = $0 }, by: text, kind: .project)
                }
                ComposerChip(title: labelTitle(work.labels), prompt: "Labels…", keepsOpen: true) {
                    Image(systemName: "tag").font(.system(size: 11)).foregroundStyle(Theme.inkSecondary)
                } make: { text in
                    WorkChoices.filtered(
                        WorkChoices.labels(work.labels, text: text) { labels.contains($0) } toggle: { label in
                            if let index = labels.firstIndex(of: label) { labels.remove(at: index) } else { labels.append(label) }
                        } create: { name in
                            let hues: [Double] = [25, 60, 145, 200, 250, 290, 330]
                            work.saveLabel(id: nil, name: name, hue: hues[work.labels.count % hues.count])
                            if let made = work.labels.first(where: { $0.name == name }) { labels.append(made.id) }
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
                    .keyboardShortcut(.cancelAction)
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
        switch owner {
        case .myself: "Me"
        case let .person(id): people.first { $0.id == id }?.name ?? "Someone"
        case .nobody: "No owner"
        }
    }

    private func face(_ people: [WorkPersonChoice]) -> AvatarFace {
        switch owner {
        case .myself: AvatarFace(kind: .me, name: "Me")
        case let .person(id): AvatarFace(kind: .person(id), name: ownerName(people))
        case .nobody: AvatarFace(kind: .unassigned, name: "No owner")
        }
    }

    private func labelTitle(_ all: [WorkLabel]) -> String {
        let names = labels.compactMap { id in all.first { $0.id == id }?.name }
        return switch names.count {
        case 0: "Labels"
        case 1, 2: names.joined(separator: ", ")
        default: "\(names.count) labels"
        }
    }

    private func create() {
        let trimmed = title.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, !creating else { return }
        creating = true
        failure = nil
        let text = details.trimmingCharacters(in: .whitespacesAndNewlines)
        Task {
            let item = await model.work.create(
                title: trimmed, details: text.isEmpty ? nil : text, status: status, priority: priority,
                projectId: project, owner: owner, dueAtMs: due, labelIds: labels)
            creating = false
            guard let item else {
                failure = model.work.mutationError ?? "The task wasn't created."
                return
            }
            model.workFocus.highlighted = item.id
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

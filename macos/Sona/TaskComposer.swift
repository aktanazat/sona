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
    @State private var template: WorkTemplate?
    @State private var fromTemplate = false

    /// Fields set in their pickers: the title's words no longer set them.
    private enum Choice { case priority, owner, due, project }

    var body: some View {
        let work = model.work
        let ready = !title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && !creating
        let chips = wordChips
        VStack(alignment: .leading, spacing: 0) {
            Text("New task")
                .font(TypeScale.label(13))
                .foregroundStyle(Theme.inkSecondary)
                .padding(.horizontal, Space.l)
                .padding(.top, Space.l)
            TextField("", text: $title, prompt: Text("Title").foregroundStyle(Theme.inkDisabled))
                .textFieldStyle(.plain)
                .font(.system(size: 18, weight: .semibold))
                .foregroundStyle(Theme.ink)
                .focused($titleFocused)
                .onSubmit(create)
                .chipKeys(reading, order: chips.order, text: title, work: work)
                .task(id: title) { await reading.read(title, in: work) }
                .padding(.horizontal, Space.l)
                .padding(.top, Space.s)
            TextField("", text: $details, prompt: Text("Details").foregroundStyle(Theme.inkDisabled), axis: .vertical)
                .textFieldStyle(.plain)
                .font(TypeScale.body(14))
                .foregroundStyle(Theme.ink)
                .lineLimit(3...10)
                .padding(.horizontal, Space.l)
                .padding(.top, Space.s)
            ChatFlowRow(spacing: Space.s) {
                if !model.routines.templates.isEmpty {
                    ComposerChip(title: template?.name ?? "Template", prompt: "Template…", opensAtStart: fromTemplate) {
                        Image(systemName: "doc.on.doc").font(.system(size: 11)).foregroundStyle(Theme.inkSecondary)
                    } make: { text in
                        WorkChoices.filtered(templateChoices, by: text, kind: .project)
                    }
                }
                ComposerChip(title: status.label, prompt: "Status…") {
                    TaskStatusIcon(status: status)
                } make: { text in
                    WorkChoices.filtered(
                        WorkChoices.statuses { $0 == status } pick: { status = $0 }, by: text, kind: .status)
                }
                ComposerChip(
                    title: chosenPriority == 0 ? "Priority" : TaskPriority(chosenPriority).label, prompt: "Priority…",
                    words: chips.slots[.priority], selected: reading.selected, keep: keepAsText
                ) {
                    PriorityIcon(priority: TaskPriority(chosenPriority))
                } make: { text in
                    WorkChoices.filtered(
                        WorkChoices.priorities { $0 == chosenPriority } pick: { priority = $0; picked.insert(.priority) },
                        by: text, kind: .priority)
                }
                ComposerChip(
                    title: ownerName(work.people), prompt: "Assign to…",
                    words: chips.slots[.owner], selected: reading.selected, keep: keepAsText
                ) {
                    Avatar(face: face(work.people), size: 16, revision: model.people.photoRevision)
                } make: { text in
                    WorkChoices.filtered(
                        WorkChoices.owners(work.people) { $0 == chosenOwner } pick: { owner = $0; picked.insert(.owner) },
                        by: text, kind: .owner)
                }
                ComposerChip(
                    title: chosenDue.map { TaskDates.label($0, now: TaskDates.now()) } ?? "Due date", prompt: "Due date, or type one…",
                    words: chips.slots[.due], selected: reading.selected, keep: keepAsText
                ) {
                    Image(systemName: "calendar").font(.system(size: 11)).foregroundStyle(Theme.inkSecondary)
                } make: { text in
                    WorkChoices.dues(text, clearable: chosenDue != nil) { candidate in
                        chosenDue.map { PlanningFormat.days(from: $0, to: candidate) == 0 } ?? false
                    } pick: { due = $0; picked.insert(.due) }
                }
                ComposerChip(
                    title: work.projects.first { $0.id == chosenProject }?.title ?? "Project", prompt: "Project…",
                    words: chips.slots[.project], selected: reading.selected, keep: keepAsText
                ) {
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
                ForEach(chips.rest) { token in
                    WordChip(token: token, selected: reading.selected == token.id) { keepAsText(token) }
                }
            }
            .padding(Space.l)
            Hairline()
            HStack(spacing: Space.m) {
                if let failure {
                    Text(failure).metaText(Theme.live).lineLimit(2)
                }
                Spacer(minLength: 0)
                Button("Cancel") { model.workFocus.composing = nil }
                    .buttonStyle(.quiet)
                    .keyboardShortcut(reading.selected == nil ? KeyboardShortcut.cancelAction : nil)
                Button(action: create) {
                    HStack(spacing: Space.s) {
                        Text("Create task")
                        Text("⌘↩").foregroundStyle(Theme.onAccent.opacity(0.7))
                    }
                }
                .buttonStyle(.primary)
                .keyboardShortcut(.return, modifiers: .command)
                .disabled(!ready)
            }
            .padding(.horizontal, Space.l)
            .frame(height: 56)
        }
        .frame(width: 620)
        .background(Theme.elevated)
        .onAppear {
            take(start)
            fromTemplate = model.workFocus.composingFromTemplate
            model.workFocus.composingFromTemplate = false
            if !fromTemplate { DispatchQueue.main.async { titleFocused = true } }
        }
    }

    /// The title's words for the fields with their own chip, and the rest
    /// of its words, each in the order its chip sits.
    private struct WordChips {
        var slots: [Choice: WorkToken] = [:]
        var rest: [WorkToken] = []

        var order: [WorkToken] {
            [Choice.priority, .owner, .due, .project].compactMap { slots[$0] } + rest
        }
    }

    private var wordChips: WordChips {
        var chips = WordChips()
        for token in reading.tokens {
            let choice: Choice? = switch token.field {
            case .priority: .priority
            case .owner: .owner
            case .due: .due
            case .project: .project
            case .label, .recurrence: nil
            }
            if let choice, !picked.contains(choice) { chips.slots[choice] = token }
        }
        let slotted = Set(chips.slots.values.map(\.id))
        chips.rest = reading.tokens.filter { !slotted.contains($0.id) }.inChipOrder
        return chips
    }

    private func keepAsText(_ token: WorkToken) {
        reading.selected = nil
        let text = title
        Task { await reading.keepAsText(token, in: text, work: model.work) }
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
        let names = labels.compactMap { id in all.first { $0.id == id }?.name }
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
            labels = WorkChoices.adding(id, to: labels, among: model.work.labels)
            return
        }
        labels.removeAll { $0 == id }
        if let token = reading.labelToken(id) {
            let text = title
            Task { await reading.keepAsText(token, in: text, work: model.work) }
        }
    }

    private var templateChoices: [WorkChoice] {
        var choices = model.routines.templates.map { candidate in
            let count = candidate.fields.subtasks.count
            return WorkChoice(
                id: candidate.id, title: candidate.name, mark: .symbol("doc.on.doc"),
                detail: count == 0 ? nil : count == 1 ? "1 sub-task" : "\(count) sub-tasks",
                checked: template?.id == candidate.id
            ) { apply(candidate) }
        }
        if template != nil {
            choices.insert(WorkChoice(id: "no-template", title: "No template", mark: .symbol("xmark")) { template = nil }, at: 0)
        }
        return choices
    }

    private func apply(_ chosen: WorkTemplate) {
        template = chosen
        let fields = chosen.fields
        if title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, let value = fields.title { title = value }
        if let value = fields.details { details = value }
        if let value = fields.status { status = value }
        if let value = fields.priority {
            priority = value
            picked.insert(.priority)
        }
        if let value = fields.owner {
            owner = value
            picked.insert(.owner)
        }
        if let days = fields.dueInDays {
            due = TaskDates.day(days)
            picked.insert(.due)
        }
        if let value = fields.projectId {
            project = value
            picked.insert(.project)
        }
        labels += fields.labelIds.filter { !labels.contains($0) }
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
            let chosenLabels = labels + words.labels(making: work).filter { !labels.contains($0) }
            let item: WorkItem?
            if let template {
                item = await make(from: template, title: words.title ?? trimmed, details: text.isEmpty ? nil : text,
                                  labelIds: chosenLabels)
            } else {
                item = await work.create(
                    title: words.title ?? trimmed, details: text.isEmpty ? nil : text, status: status,
                    priority: chosenPriority, projectId: chosenProject, owner: chosenOwner, dueAtMs: chosenDue,
                    labelIds: chosenLabels, recurrence: words.recurrence)
            }
            creating = false
            guard let item else {
                failure = failure ?? model.work.mutationError ?? "The task wasn't created."
                return
            }
            var place = WorkPatch(cycle: start.cycle)
            if case .set? = start.milestoneId, start.projectId == item.projectId.map({ .set($0) }) {
                place.milestoneId = start.milestoneId
            }
            if place != WorkPatch() { work.update([item.id], place) }
            model.workFocus.highlightWhenShown(item.id)
            model.workFocus.composing = nil
        }
    }

    private func make(from template: WorkTemplate, title: String, details: String?, labelIds: [String]) async -> WorkItem? {
        let made: [WorkItem]
        do {
            made = try await model.routines.use(template, title: title, dueAtMs: chosenDue, status: status)
        } catch {
            failure = error.localizedDescription
            return nil
        }
        guard let item = made.first else { return nil }
        var patch = WorkPatch()
        if item.priority != chosenPriority { patch.priority = chosenPriority }
        if item.projectId != chosenProject { patch.projectId = chosenProject.map { .set($0) } ?? .clear }
        let madeOwner: WorkOwner = item.ownerIsSelf ? .myself : item.ownerPersonId.map { .person($0) } ?? .nobody
        if madeOwner != chosenOwner { patch.owner = chosenOwner }
        if Set(item.labelIds) != Set(labelIds) { patch.labelIds = labelIds }
        if item.details != details { patch.details = details.map { .set($0) } ?? .clear }
        await model.work.refresh()
        if patch != WorkPatch() { model.work.update([item.id], patch) }
        return item
    }
}

/// One property of the new task: its value, and the list to change it. A
/// value the title's words set wears their tint and an × that keeps the
/// words as text.
private struct ComposerChip<Mark: View>: View {
    let title: String
    let prompt: String
    var keepsOpen = false
    var opensAtStart = false
    var words: WorkToken?
    var selected: WorkToken.ID?
    var keep: (WorkToken) -> Void = { _ in }
    @ViewBuilder let mark: () -> Mark
    let make: (String) -> [WorkChoice]
    @State private var shown = false
    @State private var hovering = false
    @State private var crossHovering = false
    @Environment(AppModel.self) private var model

    var body: some View {
        HStack(spacing: 0) {
            Button {
                shown = true
            } label: {
                HStack(spacing: Space.xs) {
                    mark().frame(width: 16, height: 16)
                    Text(title).lineLimit(1)
                }
                .padding(.leading, Space.s)
                .padding(.trailing, words == nil ? Space.s : Space.xs)
                .frame(maxHeight: .infinity)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            if let words {
                Button { keep(words) } label: { ChipCross(hot: crossHovering) }
                    .buttonStyle(.plain)
                    .onHover { crossHovering = $0 }
                    .padding(.trailing, Space.xs)
                    .help("Keep “\(words.text)” as text")
                    .accessibilityLabel("Keep “\(words.text)” as text")
            }
        }
        .chipFace(words: words != nil, selected: words.map { $0.id == selected } ?? false, hovering: hovering)
        .onHover { hovering = $0 }
        .hoverFade(hovering)
        .popover(isPresented: $shown, arrowEdge: .bottom) {
            ChoiceList(prompt: prompt, keepsOpen: keepsOpen, compact: true, make: make) { shown = false }
                .padding(.vertical, Space.xs)
                .environment(model)
        }
        .onChange(of: opensAtStart, initial: true) {
            if opensAtStart { DispatchQueue.main.async { shown = true } }
        }
    }
}

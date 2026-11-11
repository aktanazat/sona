import SwiftUI

struct ReminderPicker: View {
    let itemId: String
    let reminder: WorkReminder?
    let close: () -> Void
    @Environment(AppModel.self) private var model
    @State private var moment = Date.now.addingTimeInterval(3600)

    static func label(_ reminder: WorkReminder) -> String {
        let when = PlanningFormat.date(reminder.remindAtMs)
        return reminder.notifiedAtMs == nil ? when : "\(when), sent"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s) {
            Text("Remind me")
                .font(TypeScale.label(13))
                .foregroundStyle(Theme.inkSecondary)
            preset("In 1 hour", TaskDates.now() + 3_600_000)
            preset("Tomorrow at 9:00", TaskDates.day(1, hour: 9))
            preset("Monday at 9:00", TaskDates.next(weekday: 2, hour: 9))
            DatePicker("At", selection: $moment, displayedComponents: [.date, .hourAndMinute])
                .datePickerStyle(.field)
                .labelsHidden()
            HStack(spacing: Space.s) {
                if reminder != nil {
                    Button("Remove") { set(nil) }
                        .buttonStyle(.quiet)
                }
                Spacer(minLength: 0)
                Button("Set reminder") { set(TaskDates.milliseconds(moment)) }
                    .buttonStyle(.primary)
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(14)
        .frame(width: 260)
        .onAppear {
            if let reminder { moment = PlanningFormat.moment(reminder.remindAtMs) }
        }
    }

    private func preset(_ title: String, _ at: Int64) -> some View {
        Button { set(at) } label: {
            HStack {
                Text(title).bodyText(13)
                Spacer(minLength: Space.s)
                Text(PlanningFormat.date(at)).metaText()
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    private func set(_ at: Int64?) {
        let id = itemId
        let routines = model.routines
        close()
        Task { await routines.setReminder(on: id, at: at) }
    }
}

struct SaveTemplateSheet: View {
    let id: String
    @Environment(AppModel.self) private var model
    @State private var name = ""
    @State private var fields: WorkTemplateFields?
    @State private var failure: String?
    @State private var saving = false

    var body: some View {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: Space.m) {
                Text("Save as template")
                    .titleText()
                    .accessibilityAddTraits(.isHeader)
                TextField("Name", text: $name)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit(save)
                if let fields {
                    Picker("Due", selection: Binding(get: { fields.dueInDays }, set: { self.fields?.dueInDays = $0 })) {
                        ForEach(TemplateDue.choices(with: fields.dueInDays), id: \.self) { days in
                            Text(TemplateDue.label(days)).tag(days)
                        }
                    }
                    .fixedSize()
                    VStack(alignment: .leading, spacing: Space.xs) {
                        ForEach(kept(fields), id: \.self) { line in
                            Text(line).metaText(Theme.inkSecondary).fixedSize(horizontal: false, vertical: true)
                        }
                    }
                } else if failure == nil {
                    Text("Reading the task…").metaText()
                }
            }
            .padding(24)
            Hairline()
            HStack(spacing: Space.m) {
                if let failure {
                    Text(failure).metaText(Theme.live).lineLimit(2)
                }
                Spacer(minLength: 0)
                Button("Cancel") { model.sheet = nil }
                    .buttonStyle(.quiet)
                    .keyboardShortcut(.cancelAction)
                Button("Save template", action: save)
                    .buttonStyle(.primary)
                    .keyboardShortcut(.defaultAction)
                    .disabled(trimmed.isEmpty || fields == nil || saving)
            }
            .padding(.horizontal, 24)
            .frame(height: 56)
        }
        .frame(width: 460)
        .background(Theme.page)
        .task { await read() }
    }

    private func read() async {
        guard let row = model.workRows([id]).first else {
            failure = "This task is gone."
            return
        }
        if name.isEmpty { name = row.title }
        let retired = Set(model.work.labels.filter(\.retired).map(\.id))
        let owner = WorkChoices.owner(of: row)
        do {
            let detail = try await model.routines.detail(id)
            fields = WorkTemplateFields(
                title: row.title, details: row.details, priority: row.priority == 0 ? nil : row.priority,
                owner: owner == .nobody ? nil : owner, projectId: row.projectId,
                labelIds: row.labelIds.filter { !retired.contains($0) }, recurrence: detail.recurrence,
                subtasks: detail.subTasks.map(\.title))
        } catch {
            failure = error.localizedDescription
        }
    }

    private func kept(_ fields: WorkTemplateFields) -> [String] {
        let work = model.work
        var lines: [String] = []
        if let priority = fields.priority { lines.append("Priority: \(TaskPriority(priority).label)") }
        switch fields.owner {
        case .myself?: lines.append("Owner: Me")
        case let .person(personId)?: lines.append("Owner: \(work.people.first { $0.id == personId }?.name ?? "Someone")")
        case .nobody?, nil: break
        }
        if let project = fields.projectId.flatMap({ id in work.projects.first { $0.id == id } }) {
            lines.append("Project: \(project.title)")
        }
        let names = fields.labelIds.compactMap { id in work.labels.first { $0.id == id }?.name }
        if !names.isEmpty { lines.append("Labels: \(names.joined(separator: ", "))") }
        if let recurrence = fields.recurrence { lines.append("Repeats: \(Recurrence.display(recurrence))") }
        if !fields.subtasks.isEmpty { lines.append("Sub-tasks: \(fields.subtasks.joined(separator: ", "))") }
        if fields.details != nil { lines.append("Details") }
        return lines
    }

    private func save() {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let fields, !trimmed.isEmpty, !saving else { return }
        saving = true
        failure = nil
        Task {
            let saved = await model.routines.saveTemplate(name: trimmed, fields: fields)
            saving = false
            if saved == nil {
                failure = model.routines.error ?? "The template wasn't saved."
            } else {
                model.sheet = nil
            }
        }
    }
}

struct TasksSettingsView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Page {
            PageTitle("Tasks", subtitle: "The weekly project digest, label groups, retired labels and task templates.")
            ErrorNote(model.routines.error ?? model.work.mutationError)
            ProjectDigestSettings()
            labels
            templates
        }
        .task { await model.routines.loadTemplates() }
    }

    @ViewBuilder private var labels: some View {
        let all = model.work.labels
        let groups = Set(all.compactMap(\.group)).sorted { $0.localizedStandardCompare($1) == .orderedAscending }
        PageSection("Labels") {
            if all.isEmpty {
                Card { CardLine("No labels yet.") }
            }
            ForEach(groups.map(Optional.some) + [nil], id: \.self) { group in
                let members = all.filter { $0.group == group }
                    .sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
                if !members.isEmpty {
                    Text(group ?? "No group").metaText(Theme.inkSecondary)
                        .padding(.top, Space.xs)
                    Card {
                        ForEach(members) { label in
                            LabelSettingsRow(label: label, groups: groups)
                        }
                    }
                }
            }
        }
    }

    @ViewBuilder private var templates: some View {
        PageSection("Templates") {
            Card {
                if model.routines.templates.isEmpty {
                    CardLine("No templates yet. Save one from a task’s menu.")
                }
                ForEach(model.routines.templates) { template in
                    TemplateSettingsRow(template: template)
                }
            }
        }
    }
}

private struct LabelSettingsRow: View {
    let label: WorkLabel
    let groups: [String]
    @Environment(AppModel.self) private var model
    @State private var naming = false
    @State private var newGroup = ""

    var body: some View {
        let work = model.work
        let count = (work.items ?? []).filter { $0.archivedAtMs == nil && $0.labelIds.contains(label.id) }.count
        CardRow {
            HStack(spacing: Space.s) {
                LabelPill(label: label)
                    .opacity(label.retired ? 0.55 : 1)
                Text(count == 1 ? "1 task" : "\(count) tasks").metaText()
                if label.retired {
                    Text("Retired").metaText(Theme.inkSecondary)
                }
            }
        } trailing: {
            HStack(spacing: Space.s) {
                Menu(label.group ?? "Group") {
                    ForEach(groups, id: \.self) { group in
                        Button {
                            work.groupLabel(label.id, into: group)
                        } label: {
                            if label.group == group { Label(group, systemImage: "checkmark") } else { Text(group) }
                        }
                    }
                    if !groups.isEmpty { Divider() }
                    Button("New group…") {
                        newGroup = ""
                        naming = true
                    }
                    if label.group != nil {
                        Button("No group") { work.groupLabel(label.id, into: nil) }
                    }
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
                Menu(label.scope.label) {
                    Picker("Use on", selection: Binding(get: { label.scope }, set: { work.scopeLabel(label.id, $0) })) {
                        ForEach(WorkLabelScope.allCases) { Text($0.label).tag($0) }
                    }
                    .pickerStyle(.inline)
                    .labelsHidden()
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
                .help("Where the label can go")
                .accessibilityLabel("Use on: \(label.scope.label)")
                Button(label.retired ? "Restore" : "Retire") {
                    work.retireLabel(label.id, !label.retired)
                }
                .buttonStyle(.compact)
                .help(label.retired ? "Show it in label lists again" : "Hide it from label lists. Tasks keep it.")
            }
        }
        .alert("New group", isPresented: $naming) {
            TextField("Name", text: $newGroup)
            Button("Move") { work.groupLabel(label.id, into: newGroup) }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Moves “\(label.name)” into a new group.")
        }
    }
}

private struct TemplateSettingsRow: View {
    let template: WorkTemplate
    @Environment(AppModel.self) private var model
    @State private var renaming = false
    @State private var deleting = false
    @State private var editing = false
    @State private var name = ""

    var body: some View {
        let routines = model.routines
        CardRow {
            VStack(alignment: .leading, spacing: 2) {
                Text(template.name).bodyText(13)
                if let summary {
                    Text(summary).metaText().lineLimit(1)
                }
            }
        } trailing: {
            HStack(spacing: Space.s) {
                Menu(TemplateDue.label(template.fields.dueInDays)) {
                    ForEach(TemplateDue.choices(with: template.fields.dueInDays), id: \.self) { days in
                        Button {
                            var fields = template.fields
                            fields.dueInDays = days
                            Task { await routines.saveTemplate(id: template.id, name: template.name, fields: fields) }
                        } label: {
                            if template.fields.dueInDays == days {
                                Label(TemplateDue.label(days), systemImage: "checkmark")
                            } else {
                                Text(TemplateDue.label(days))
                            }
                        }
                    }
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
                Button("Edit") { editing = true }
                    .buttonStyle(.compact)
                    .popover(isPresented: $editing, arrowEdge: .bottom) {
                        TemplateFieldsEditor(template: template) { editing = false }
                            .environment(model)
                    }
                Button("Rename") {
                    name = template.name
                    renaming = true
                }
                .buttonStyle(.compact)
                Button("Delete") { deleting = true }
                    .buttonStyle(.compact)
            }
        }
        .alert("Rename template", isPresented: $renaming) {
            TextField("Name", text: $name)
            Button("Rename") {
                let text = name
                Task { await routines.rename(template, to: text) }
            }
            Button("Cancel", role: .cancel) {}
        }
        .confirmationDialog("Delete “\(template.name)”?", isPresented: $deleting) {
            Button("Delete", role: .destructive) {
                Task { await routines.delete(template) }
            }
        } message: {
            Text("Tasks made from it stay.")
        }
    }

    private var summary: String? {
        let fields = template.fields
        var parts: [String] = []
        switch fields.subtasks.count {
        case 0: break
        case 1: parts.append("1 sub-task")
        case let count: parts.append("\(count) sub-tasks")
        }
        if let priority = fields.priority { parts.append(TaskPriority(priority).label) }
        if let project = fields.projectId.flatMap({ id in model.work.projects.first { $0.id == id } }) {
            parts.append(project.title)
        }
        if !fields.labelIds.isEmpty { parts.append(fields.labelIds.count == 1 ? "1 label" : "\(fields.labelIds.count) labels") }
        return parts.isEmpty ? nil : parts.joined(separator: ", ")
    }
}

/// A template's title, details, priority, owner, project and sub-tasks,
/// changed in place; due day and labels keep their own controls.
private struct TemplateFieldsEditor: View {
    let template: WorkTemplate
    let close: () -> Void
    @Environment(AppModel.self) private var model
    @State private var fields = WorkTemplateFields()
    @State private var subtasks = ""

    var body: some View {
        let work = model.work
        VStack(alignment: .leading, spacing: Space.m) {
            Text("Edit “\(template.name)”").font(TypeScale.label(13)).foregroundStyle(Theme.inkSecondary)
            TextField("Task title", text: text(\.title))
                .textFieldStyle(.roundedBorder)
            TextField("Details", text: text(\.details), axis: .vertical)
                .textFieldStyle(.roundedBorder)
                .lineLimit(2...6)
            Picker("Priority", selection: $fields.priority) {
                Text("No priority").tag(Int?.none)
                ForEach([TaskPriority.urgent, .high, .medium, .low], id: \.rawValue) { priority in
                    Text(priority.label).tag(Int?.some(priority.rawValue))
                }
            }
            Picker("Owner", selection: $fields.owner) {
                Text("No owner").tag(WorkOwner?.none)
                Text("Me").tag(WorkOwner?.some(.myself))
                ForEach(work.people.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }) { person in
                    Text(person.name).tag(WorkOwner?.some(.person(person.id)))
                }
            }
            Picker("Project", selection: $fields.projectId) {
                Text("No project").tag(String?.none)
                ForEach(work.projects.filter { $0.status != .completed || $0.id == fields.projectId }, id: \.id) { project in
                    Text(project.title).tag(String?.some(project.id))
                }
            }
            VStack(alignment: .leading, spacing: Space.xs) {
                Text("Sub-tasks, one a line").metaText()
                TextEditor(text: $subtasks)
                    .font(TypeScale.body(13))
                    .frame(height: 80)
                    .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl).strokeBorder(Theme.border, lineWidth: 1))
            }
            HStack {
                Spacer(minLength: 0)
                Button("Cancel", action: close)
                    .buttonStyle(.quiet)
                    .keyboardShortcut(.cancelAction)
                Button("Save", action: save)
                    .buttonStyle(.primary)
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(14)
        .frame(width: 340)
        .onAppear {
            fields = template.fields
            subtasks = template.fields.subtasks.joined(separator: "\n")
        }
    }

    private func text(_ path: WritableKeyPath<WorkTemplateFields, String?>) -> Binding<String> {
        Binding(get: { fields[keyPath: path] ?? "" }, set: { fields[keyPath: path] = $0.isEmpty ? nil : $0 })
    }

    private func save() {
        var saved = fields
        saved.title = saved.title?.trimmingCharacters(in: .whitespacesAndNewlines)
        if saved.title?.isEmpty == true { saved.title = nil }
        saved.subtasks = subtasks.split(separator: "\n")
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
        let routines = model.routines
        let template = template
        close()
        Task { await routines.saveTemplate(id: template.id, name: template.name, fields: saved) }
    }
}

enum TemplateDue {
    static func choices(with current: Int?) -> [Int?] {
        let presets: [Int?] = [nil, 0, 1, 2, 3, 7, 14, 30]
        return presets.contains(current) ? presets : presets + [current]
    }

    static func label(_ days: Int?) -> String {
        switch days {
        case nil: "No due date"
        case 0?: "Due the day it’s made"
        case 1?: "Due the next day"
        case let days?: "Due \(days) days after it’s made"
        }
    }
}

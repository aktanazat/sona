import SwiftUI

/// One choice in a picker: a status, a person, a date, a label.
struct WorkChoice: Identifiable {
    enum Mark {
        case status(WorkStatus)
        case priority(TaskPriority)
        case face(AvatarFace)
        case symbol(String)
        case label(Double)
    }

    let id: String
    let title: String
    let mark: Mark
    var detail: String?
    var checked = false
    /// The digit that picks it while the field is empty.
    var key: String?
    /// Leaves the picker open: the choice swaps the list for its next step.
    var staysOpen = false
    let run: @MainActor () -> Void
}

/// The choices each property offers. The same lists serve tasks that exist,
/// through `make`, and the new-task sheet, through its own `checked` and
/// `pick`.
@MainActor
enum WorkChoices {
    static func prompt(_ kind: WorkPicker.Kind) -> String {
        switch kind {
        case .status: "Change status…"
        case .priority: "Set priority…"
        case .owner: "Assign to…"
        case .due: "Due date, or type one…"
        case .labels: "Change labels…"
        case .project: "Move to project…"
        case .duplicate: "Duplicate of…"
        case .snooze: "Snooze until, or type a date…"
        case .delegate: "Delegate to…"
        }
    }

    /// Labels toggle one after another; every other picker closes on a pick.
    static func keepsOpen(_ kind: WorkPicker.Kind) -> Bool { kind == .labels }

    /// The choices for tasks that exist; picking one changes them all.
    /// `pickDay` swaps the snooze list for a calendar.
    static func make(_ picker: WorkPicker, query: String, model: AppModel, pickDay: @escaping @MainActor () -> Void) -> [WorkChoice] {
        let ids = picker.ids
        let rows = model.workRows(ids)
        let text = query.trimmingCharacters(in: .whitespaces)
        let work = model.work
        let choices: [WorkChoice] = switch picker.kind {
        case .status:
            statuses { status in rows.allSatisfy { $0.status == status } } pick: { status in
                model.changeWork { work.update(ids, WorkPatch(status: status)) }
            }
        case .priority:
            priorities { value in rows.allSatisfy { $0.priority == value } } pick: { value in
                model.changeWork { work.update(ids, WorkPatch(priority: value)) }
            }
        case .owner:
            owners(work.people) { owner in rows.allSatisfy { WorkChoices.owner(of: $0) == owner } } pick: { owner in
                model.changeWork { work.update(ids, WorkPatch(owner: owner)) }
            }
        case .due:
            dues(text, clearable: rows.contains { $0.dueAtMs != nil }) { due in
                rows.allSatisfy { $0.dueAtMs.map { PlanningFormat.days(from: $0, to: due) == 0 } ?? false }
            } pick: { due in
                let value: WorkPatchValue<Int64> = due.map { .set($0) } ?? .clear
                model.changeWork { work.update(ids, WorkPatch(dueAtMs: value)) }
            }
        case .labels:
            labels(work.labels, text: text) { label in
                !rows.isEmpty && rows.allSatisfy { $0.labelIds.contains(label) }
            } toggle: { label in
                toggle(label, model.workRows(ids), model)
            } create: { name in
                let hues: [Double] = [25, 60, 145, 200, 250, 290, 330]
                work.saveLabel(id: nil, name: name, hue: hues[work.labels.count % hues.count])
                if let made = work.labels.first(where: { $0.name == name }) {
                    toggle(made.id, model.workRows(ids), model)
                }
            }
        case .project:
            projects(work.projects) { project in rows.allSatisfy { $0.projectId == project } } pick: { project in
                let value: WorkPatchValue<String> = project.map { .set($0) } ?? .clear
                model.changeWork { work.update(ids, WorkPatch(projectId: value)) }
            }
        case .duplicate:
            duplicates(of: ids, text: text, model: model)
        case .snooze:
            snoozes(text, pickDay: pickDay) { moment in
                model.leave(ids) { work.update(ids, WorkPatch(snoozedUntilMs: .set(moment))) }
            }
        case .delegate:
            delegates(rows) { agent in
                model.changeWork { work.delegate(ids, to: agent) }
            }
        }
        return filtered(choices, by: text, kind: picker.kind)
    }

    /// Typed letters narrow a list of names; dates and tasks read the text
    /// themselves.
    static func filtered(_ choices: [WorkChoice], by text: String, kind: WorkPicker.Kind) -> [WorkChoice] {
        guard !text.isEmpty, kind != .due, kind != .snooze, kind != .duplicate else { return choices }
        return choices.filter { $0.id == newLabel || $0.title.localizedCaseInsensitiveContains(text) }
    }

    static let newLabel = "new-label"

    static func owner(of row: WorkItemRow) -> WorkOwner {
        if row.ownerIsSelf { return .myself }
        return row.ownerPersonId.map { .person($0) } ?? .nobody
    }

    static func statuses(
        checked: (WorkStatus) -> Bool, pick: @escaping @MainActor (WorkStatus) -> Void
    ) -> [WorkChoice] {
        let statuses: [WorkStatus] = [.backlog, .notStarted, .inProgress, .done, .canceled]
        return statuses.enumerated().map { index, status in
            WorkChoice(id: status.rawValue, title: status.label, mark: .status(status),
                       checked: checked(status), key: "\(index + 1)") { pick(status) }
        }
    }

    static func priorities(checked: (Int) -> Bool, pick: @escaping @MainActor (Int) -> Void) -> [WorkChoice] {
        TaskPriority.menuOrder.map { priority in
            WorkChoice(id: "\(priority.rawValue)", title: priority.label, mark: .priority(priority),
                       checked: checked(priority.rawValue), key: "\(priority.rawValue)") { pick(priority.rawValue) }
        }
    }

    static func owners(
        _ people: [WorkPersonChoice], checked: (WorkOwner) -> Bool, pick: @escaping @MainActor (WorkOwner) -> Void
    ) -> [WorkChoice] {
        var choices = [
            WorkChoice(id: "self", title: "Me", mark: .face(AvatarFace(kind: .me, name: "Me")),
                       checked: checked(.myself)) { pick(.myself) },
        ]
        for person in people.sorted(by: { $0.name.localizedStandardCompare($1.name) == .orderedAscending }) {
            choices.append(WorkChoice(
                id: person.id, title: person.name, mark: .face(AvatarFace(kind: .person(person.id), name: person.name)),
                checked: checked(.person(person.id))) { pick(.person(person.id)) })
        }
        choices.append(WorkChoice(id: "nobody", title: "No owner", mark: .face(AvatarFace(kind: .unassigned, name: "No owner")),
                                  checked: checked(.nobody)) { pick(.nobody) })
        return choices
    }

    static func dues(
        _ text: String, clearable: Bool, checked: (Int64) -> Bool, pick: @escaping @MainActor (Int64?) -> Void
    ) -> [WorkChoice] {
        var choices: [WorkChoice] = []
        if let typed = TaskDates.parse(text) {
            choices.append(WorkChoice(id: "typed", title: TaskDates.label(typed, now: TaskDates.now()),
                                      mark: .symbol("calendar"), detail: long(typed)) { pick(typed) })
        }
        let presets: [(String, Int64)] = [
            ("Today", TaskDates.day(0)), ("Tomorrow", TaskDates.day(1)),
            ("Next week", TaskDates.next(weekday: 2)), ("In two weeks", TaskDates.day(14)),
        ]
        for (title, due) in distinct(presets) where text.isEmpty || title.localizedCaseInsensitiveContains(text) {
            choices.append(WorkChoice(id: title, title: title, mark: .symbol("calendar"), detail: long(due),
                                      checked: checked(due)) { pick(due) })
        }
        if clearable, text.isEmpty || "no due date".localizedCaseInsensitiveContains(text) {
            choices.append(WorkChoice(id: "none", title: "No due date", mark: .symbol("calendar.badge.minus")) { pick(nil) })
        }
        return choices
    }

    static func labels(
        _ labels: [WorkLabel], text: String, checked: (String) -> Bool, toggle: @escaping @MainActor (String) -> Void,
        create: (@MainActor (String) -> Void)? = nil
    ) -> [WorkChoice] {
        var choices = labels.map { label in
            WorkChoice(id: label.id, title: label.name, mark: .label(label.hue), checked: checked(label.id)) {
                toggle(label.id)
            }
        }
        let exists = labels.contains { $0.name.localizedCaseInsensitiveCompare(text) == .orderedSame }
        if !text.isEmpty, !exists {
            choices.append(WorkChoice(id: newLabel, title: "New label “\(text)”", mark: .symbol("plus")) {
                if let create { create(text) }
            })
        }
        return choices
    }

    static func projects(
        _ projects: [WorkProjectChoice], checked: (String?) -> Bool, pick: @escaping @MainActor (String?) -> Void
    ) -> [WorkChoice] {
        var choices = projects
            .filter { $0.status != .completed }
            .sorted { $0.title.localizedStandardCompare($1.title) == .orderedAscending }
            .map { project in
                WorkChoice(id: project.id, title: project.title, mark: .symbol("folder"), checked: checked(project.id)) {
                    pick(project.id)
                }
            }
        choices.append(WorkChoice(id: "none", title: "No project", mark: .symbol("folder.badge.minus"),
                                  checked: checked(nil)) { pick(nil) })
        return choices
    }

    static func snoozes(
        _ text: String, pickDay: @escaping @MainActor () -> Void, pick: @escaping @MainActor (Int64) -> Void
    ) -> [WorkChoice] {
        var choices: [WorkChoice] = []
        if let typed = TaskDates.parse(text) {
            choices.append(WorkChoice(id: "typed", title: TaskDates.label(typed, now: TaskDates.now()),
                                      mark: .symbol("moon.zzz"), detail: long(typed)) { pick(typed) })
        }
        if let later = TaskDates.laterToday(), text.isEmpty || "later today".localizedCaseInsensitiveContains(text) {
            choices.append(WorkChoice(id: "later", title: "Later today", mark: .symbol("moon.zzz"),
                                      detail: PlanningFormat.moment(later).formatted(date: .omitted, time: .shortened)) {
                pick(later)
            })
        }
        let presets: [(String, Int64)] = [
            ("Tomorrow", TaskDates.day(1, hour: 9)), ("In 3 days", TaskDates.day(3, hour: 9)),
            ("Next week", TaskDates.next(weekday: 2, hour: 9)), ("In a month", TaskDates.day(30, hour: 9)),
        ]
        for (title, moment) in distinct(presets) where text.isEmpty || title.localizedCaseInsensitiveContains(text) {
            choices.append(WorkChoice(id: title, title: title, mark: .symbol("moon.zzz"), detail: long(moment)) {
                pick(moment)
            })
        }
        if text.isEmpty || "pick a day".localizedCaseInsensitiveContains(text) {
            choices.append(WorkChoice(id: "day", title: "Pick a day…", mark: .symbol("calendar"), staysOpen: true, run: pickDay))
        }
        return choices
    }

    /// The agents a task can go to, and taking it back once one has it.
    private static func delegates(_ rows: [WorkItemRow], pick: @escaping @MainActor (String?) -> Void) -> [WorkChoice] {
        let agents = ["Claude Code", "Codex", "OMP", "Grok"]
        var choices = agents.enumerated().map { index, agent in
            WorkChoice(id: agent, title: agent, mark: .symbol("sparkle"),
                       checked: !rows.isEmpty && rows.allSatisfy { $0.doer?.agent == agent }, key: "\(index + 1)") {
                pick(agent)
            }
        }
        if rows.contains(where: { $0.doer != nil }) {
            choices.append(WorkChoice(id: "back", title: "Take it back", mark: .face(AvatarFace(kind: .me, name: "Me"))) {
                pick(nil)
            })
        }
        return choices
    }

    /// A preset whose moment an earlier one already offers drops out: on a
    /// Sunday, "Next week" is tomorrow.
    private static func distinct(_ presets: [(String, Int64)]) -> [(String, Int64)] {
        var seen: Set<Int64> = []
        return presets.filter { seen.insert($0.1).inserted }
    }

    /// The task Sona suggested first, then the rest, newest first.
    private static func duplicates(of ids: [String], text: String, model: AppModel) -> [WorkChoice] {
        guard let id = ids.first else { return [] }
        let suggested = (model.work.suggestions[id] ?? []).compactMap { suggestion -> String? in
            if case let .item(other) = suggestion.value { other } else { nil }
        }
        let number = text.hasPrefix("#") ? String(text.dropFirst()) : text
        let pool = (model.work.items ?? []).filter { row in
            row.id != id && row.status != .duplicate && row.archivedAtMs == nil
                && (text.isEmpty || row.title.localizedCaseInsensitiveContains(text) || "\(row.number)" == number)
        }
        let ordered = pool.filter { suggested.contains($0.id) }
            + pool.filter { !suggested.contains($0.id) }.sorted { $0.updatedAtMs > $1.updatedAtMs }
        return ordered.prefix(30).map { row in
            WorkChoice(id: row.id, title: row.title, mark: .status(row.status),
                       detail: suggested.contains(row.id) ? "#\(row.number) · Suggested" : "#\(row.number)") {
                model.leave([id]) { model.work.markDuplicate(id, of: row.id) }
            }
        }
    }

    /// On for every task when any lacks it, off for all when all have it.
    /// A patch sets the whole list, so tasks whose lists differ change apart.
    private static func toggle(_ label: String, _ rows: [WorkItemRow], _ model: AppModel) {
        let on = rows.allSatisfy { $0.labelIds.contains(label) }
        let alike = Dictionary(grouping: rows, by: \.labelIds)
        model.changeWork {
            for (labels, members) in alike {
                let next = on ? labels.filter { $0 != label } : labels.contains(label) ? labels : labels + [label]
                if next != labels { model.work.update(members.map(\.id), WorkPatch(labelIds: next)) }
            }
        }
    }

    /// "Fri, Nov 14".
    static func long(_ milliseconds: Int64) -> String {
        PlanningFormat.moment(milliseconds).formatted(.dateTime.weekday(.abbreviated).month(.abbreviated).day())
    }
}

/// "#41 Send Maya the updated deck" or "3 tasks": what a picker or the
/// palette is about to act on.
struct WorkContextChip: View {
    let text: String

    var body: some View {
        Text(text)
            .font(TypeScale.label(12))
            .foregroundStyle(Theme.inkSecondary)
            .lineLimit(1)
            .padding(.horizontal, 8)
            .frame(height: 22)
            .background(Theme.control, in: RoundedRectangle(cornerRadius: Theme.radiusKey))
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// A picker for tasks that exist. In the palette it fills the palette's
/// panel; from a click in the inspector or the bulk bar it sits in a popover.
struct WorkPickerList: View {
    let picker: WorkPicker
    var compact = false
    let close: () -> Void
    @State private var pickingDay = false
    @Environment(AppModel.self) private var model

    var body: some View {
        if pickingDay {
            SnoozeDayPicker(ids: picker.ids, compact: compact, close: close)
        } else {
            ChoiceList(
                prompt: WorkChoices.prompt(picker.kind),
                context: compact ? nil : model.workContext(picker.ids),
                keepsOpen: WorkChoices.keepsOpen(picker.kind),
                compact: compact,
                empty: picker.kind == .duplicate ? "No task matches." : "Nothing matches.",
                make: { WorkChoices.make(picker, query: $0, model: model) { pickingDay = true } },
                close: close)
        }
    }
}

/// Pick a day: a month from tomorrow on. ↩ snoozes until 9:00 that morning.
struct SnoozeDayPicker: View {
    let ids: [String]
    var compact = false
    let close: () -> Void
    @Environment(AppModel.self) private var model
    @State private var day = PlanningFormat.moment(TaskDates.day(1, hour: 9))
    @FocusState private var focused: Bool

    var body: some View {
        let moment = TaskDates.day(0, hour: 9, from: day)
        VStack(spacing: 0) {
            DatePicker("Snooze until", selection: $day, in: PlanningFormat.moment(TaskDates.day(1, hour: 0))...,
                       displayedComponents: .date)
                .datePickerStyle(.graphical)
                .labelsHidden()
                .focused($focused)
                .frame(maxWidth: .infinity)
                .padding(compact ? 12 : 20)
            Hairline()
            HStack(spacing: 8) {
                Text("\(WorkChoices.long(moment)), \(PlanningFormat.moment(moment).formatted(date: .omitted, time: .shortened))")
                    .metaText(Theme.inkSecondary)
                Spacer(minLength: 8)
                TaskKeyButton(title: "Snooze", keys: "return") { snooze(until: moment) }
                    .keyboardShortcut(.defaultAction)
            }
            .padding(.horizontal, compact ? 12 : 16)
            .frame(height: 48)
        }
        .frame(width: compact ? 280 : nil)
        .onAppear { DispatchQueue.main.async { focused = true } }
    }

    private func snooze(until moment: Int64) {
        model.leave(ids) { model.work.update(ids, WorkPatch(snoozedUntilMs: .set(moment))) }
        close()
    }
}

/// A field over a list of choices: typing narrows it, ↑↓ move, ↩ picks, and
/// a digit picks the choice that shows it.
struct ChoiceList: View {
    let prompt: String
    var context: String?
    var keepsOpen = false
    var compact = false
    var empty = "Nothing matches."
    let make: (String) -> [WorkChoice]
    let close: () -> Void
    @State private var query = ""
    @State private var selection = 0
    @FocusState private var focused: Bool

    var body: some View {
        let choices = make(query)
        VStack(spacing: 0) {
            if let context {
                WorkContextChip(text: context)
                    .padding(.horizontal, 20)
                    .padding(.top, 14)
            }
            TextField("", text: $query, prompt: Text(prompt).foregroundStyle(Theme.inkTertiary))
                .textFieldStyle(.plain)
                .font(TypeScale.body(compact ? 13 : 17))
                .foregroundStyle(Theme.ink)
                .focused($focused)
                .onSubmit { pick(choices, at: selection) }
                .onKeyPress(.upArrow) {
                    move(-1, in: choices)
                    return .handled
                }
                .onKeyPress(.downArrow) {
                    move(1, in: choices)
                    return .handled
                }
                .onKeyPress(characters: .decimalDigits, phases: .down) { press in
                    guard query.isEmpty, let index = choices.firstIndex(where: { $0.key == press.characters }) else {
                        return .ignored
                    }
                    pick(choices, at: index)
                    return .handled
                }
                .padding(.horizontal, compact ? 12 : 20)
                .frame(height: compact ? 36 : 56)
            Hairline()
            ScrollViewReader { proxy in
                ScrollView {
                    VStack(spacing: 2) {
                        ForEach(Array(choices.enumerated()), id: \.element.id) { index, choice in
                            WorkChoiceRow(choice: choice, selected: index == selection, compact: compact) {
                                pick(choices, at: index)
                            }
                            .id(choice.id)
                            .onHover { inside in
                                if inside { selection = index }
                            }
                        }
                        if choices.isEmpty {
                            Text(empty)
                                .metaText()
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(.horizontal, 20)
                                .padding(.vertical, 10)
                        }
                    }
                    .padding(.vertical, 6)
                }
                .scrollIndicators(.never)
                .frame(maxHeight: compact ? 300 : 380)
                .fixedSize(horizontal: false, vertical: true)
                .onChange(of: selection) { _, index in
                    if choices.indices.contains(index) { proxy.scrollTo(choices[index].id) }
                }
            }
            if !compact {
                Hairline()
                footer
            }
        }
        .frame(width: compact ? 280 : nil)
        .onAppear {
            selection = choices.firstIndex(where: \.checked) ?? 0
            DispatchQueue.main.async { focused = true }
        }
        .onChange(of: query) { _, _ in selection = 0 }
    }

    private var footer: some View {
        HStack(spacing: 6) {
            Spacer(minLength: 0)
            KeyCap("↑")
            KeyCap("↓")
            Text("Select").metaText(Theme.inkSecondary).padding(.trailing, 10)
            KeyCap("↩")
            Text(keepsOpen ? "Toggle" : "Choose").metaText(Theme.inkSecondary).padding(.trailing, 10)
            KeyCap("esc")
            Text("Close").metaText(Theme.inkSecondary)
        }
        .padding(.horizontal, 16)
        .frame(height: 40)
        .background(Theme.inset)
        .accessibilityElement(children: .combine)
    }

    private func move(_ delta: Int, in choices: [WorkChoice]) {
        guard !choices.isEmpty else { return }
        selection = (selection + delta + choices.count) % choices.count
    }

    private func pick(_ choices: [WorkChoice], at index: Int) {
        guard choices.indices.contains(index) else { return }
        let choice = choices[index]
        if keepsOpen || !choice.checked { choice.run() }
        if choice.id == WorkChoices.newLabel { query = "" }
        if !keepsOpen && !choice.staysOpen { close() }
    }
}

struct WorkChoiceRow: View {
    let choice: WorkChoice
    let selected: Bool
    var compact = false
    let pick: () -> Void
    @Environment(AppModel.self) private var model

    var body: some View {
        Button(action: pick) {
            HStack(spacing: 10) {
                mark.frame(width: 16, height: 16)
                Text(choice.title)
                    .font(TypeScale.body(compact ? 13 : 14))
                    .foregroundStyle(Theme.ink)
                    .lineLimit(1)
                Spacer(minLength: 8)
                if let detail = choice.detail {
                    Text(detail).metaText().lineLimit(1)
                }
                if choice.checked {
                    Image(systemName: "checkmark")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(Theme.accent)
                        .accessibilityLabel("Current")
                }
                if let key = choice.key, !compact {
                    KeyCap(key)
                }
            }
            .padding(.horizontal, compact ? 10 : 12)
            .frame(height: compact ? 30 : 36)
            .background(selected ? Theme.selection : .clear, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .padding(.horizontal, compact ? 6 : 8)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(choice.checked ? .isSelected : [])
    }

    @ViewBuilder
    private var mark: some View {
        switch choice.mark {
        case let .status(status): TaskStatusIcon(status: status)
        case let .priority(priority): PriorityIcon(priority: priority)
        case let .face(face): Avatar(face: face, size: 16, revision: model.people.photoRevision)
        case let .symbol(name):
            Image(systemName: name).font(.system(size: 12)).foregroundStyle(Theme.inkSecondary)
        case let .label(hue):
            Circle().fill(Theme.label(hue: hue)).frame(width: 9, height: 9)
        }
    }
}

/// A property's value that opens its picker in a popover: the inspector's
/// rows and the bulk bar's buttons.
struct WorkPickerButton<Label: View>: View {
    let kind: WorkPicker.Kind
    let ids: [String]
    @ViewBuilder let label: () -> Label
    @State private var shown = false
    @Environment(AppModel.self) private var model

    var body: some View {
        Button { shown = true } label: { label() }
            .buttonStyle(.plain)
            .popover(isPresented: $shown, arrowEdge: .bottom) {
                WorkPickerList(picker: WorkPicker(kind: kind, ids: ids), compact: true) { shown = false }
                    .padding(.vertical, 4)
                    .environment(model)
            }
    }
}

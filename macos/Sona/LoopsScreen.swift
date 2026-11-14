import SwiftUI

struct LoopsScreen: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let store = model.loops
        Group {
            if store.open {
                WorkLoopPage(store: store)
            } else {
                list(store)
            }
        }
        .task { await store.load() }
    }

    private func list(_ store: LoopsStore) -> some View {
        Page {
            PageTitle("Automations", subtitle: "Prompts the agent runs on a schedule or when something happens.") {
                Button("New automation") { store.startNew() }
                    .buttonStyle(.secondary)
                    .disabled(store.loops == nil)
            }
            ErrorNote(store.error)
            if let loops = store.loops {
                Card {
                    if loops.isEmpty {
                        CardLine("No automations yet. Click New automation to make one.")
                    }
                    ForEach(loops) { loop in
                        row(loop, store)
                    }
                }
            } else if store.error == nil {
                Card { CardLine("Reading your automations…") }
            }
        }
    }

    private func row(_ loop: WorkLoop, _ store: LoopsStore) -> some View {
        let failed = loop.lastRun.flatMap { loop.enabled && $0.status == .failed ? $0 : nil }
        return CardRow(action: { store.select(loop.id) }) {
            VStack(alignment: .leading, spacing: 2) {
                Text(loop.name).bodyText(13)
                Text(loop.trigger.summary(labels: model.work.labels)).metaText().lineLimit(1)
                if let failed {
                    Text(failed.error.map { "Failed \(FeedClock.ago(failed.queuedAtMs)): \($0)" }
                        ?? "Failed \(FeedClock.ago(failed.queuedAtMs))")
                        .metaText(Theme.live)
                        .lineLimit(2)
                        .help(loopDate(failed.queuedAtMs).formatted(date: .abbreviated, time: .shortened))
                }
            }
        } trailing: {
            VStack(alignment: .trailing, spacing: 2) {
                if !loop.enabled {
                    Text("Off").metaText(Theme.inkSecondary)
                } else if let next = loop.nextRunText {
                    Text(next).metaText(Theme.inkSecondary)
                }
                if loop.enabled, failed == nil, let run = loop.lastRun {
                    Text("\(run.status.label) \(FeedClock.ago(run.queuedAtMs))")
                        .metaText()
                        .help(loopDate(run.queuedAtMs).formatted(date: .abbreviated, time: .shortened))
                }
            }
        }
    }
}

private struct WorkLoopPage: View {
    let store: LoopsStore
    @Environment(AppModel.self) private var model
    @State private var deleting = false

    var body: some View {
        let loop = store.detail?.loop
        Page(back: PageBack("Automations") { store.close() }, title: loop?.name ?? "New automation") {
            VStack(alignment: .leading, spacing: 6) {
                Text(loop?.name ?? "New automation")
                    .heroText()
                    .lineLimit(2)
                    .accessibilityAddTraits(.isHeader)
                    .pageHeadline()
                if let loop {
                    Text(loop.trigger.summary(labels: model.work.labels)).metaText(Theme.inkSecondary)
                }
            }
            .padding(.bottom, 20)
            ErrorNote(store.error)
            if store.creating {
                WorkLoopEditor(store: store, loop: nil)
            } else if let detail = store.detail {
                PageSection("Status") {
                    Card {
                        ToggleRow(title: "Enabled", detail: detail.loop.nextRunText.map { "Next run: \($0)" }, isOn: Binding(
                            get: { detail.loop.enabled },
                            set: { enabled in Task { await store.setEnabled(enabled) } }))
                    }
                }
                WorkLoopEditor(store: store, loop: detail.loop)
                    .id(detail.loop.id)
                runs(detail)
                versions(detail)
            } else if store.error == nil {
                Text("Reading the automation…").metaText()
            }
        } actions: {
            if let loop {
                HStack(spacing: 8) {
                    Button("Run now") { Task { await store.run() } }
                        .buttonStyle(.secondary)
                        .disabled(store.busy)
                    Button {
                        deleting = true
                    } label: {
                        Image(systemName: "trash")
                    }
                    .buttonStyle(.icon)
                    .help("Delete automation")
                    .accessibilityLabel("Delete automation")
                    .disabled(store.busy)
                    .confirmationDialog("Delete “\(loop.name)”?", isPresented: $deleting) {
                        Button("Delete", role: .destructive) { Task { await store.delete() } }
                    } message: {
                        Text("Its runs and versions go too. This can’t be undone.")
                    }
                }
            }
        }
    }

    private func runs(_ detail: WorkLoopDetail) -> some View {
        PageSection("Runs") {
            Card {
                if detail.runs.isEmpty {
                    CardLine("No runs yet. Click Run now to try it.")
                }
                ForEach(detail.runs) { run in
                    CardRow {
                        VStack(alignment: .leading, spacing: 4) {
                            HStack(spacing: 8) {
                                Text(run.status.label)
                                    .bodyText(13, run.status == .failed ? Theme.live : Theme.ink)
                                Text(run.cause).bodyText(13, Theme.inkSecondary)
                            }
                            Text(times(run)).metaText()
                            if let output = run.output, !output.isEmpty {
                                Text(output).bodyText(13).textSelection(.enabled)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                            if let error = run.error, !error.isEmpty {
                                Text(error).bodyText(13, Theme.live).textSelection(.enabled)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                            if !run.refusedTools.isEmpty {
                                Text("Refused tools: \(run.refusedTools.map(toolTitle).joined(separator: ", "))")
                                    .metaText(Theme.live)
                            }
                        }
                    } trailing: {
                        Text("Version \(run.version)").metaText()
                    }
                }
            }
        }
    }

    private func versions(_ detail: WorkLoopDetail) -> some View {
        PageSection("Versions") {
            Card {
                ForEach(detail.versions) { version in
                    CardRow {
                        VStack(alignment: .leading, spacing: 2) {
                            Text("Version \(version.version)").bodyText(13)
                            Text("\(loopDate(version.savedAtMs).formatted(date: .abbreviated, time: .shortened)), by \(version.savedBy)")
                                .metaText()
                            if let from = version.restoredFrom {
                                Text("Restored from version \(from)").metaText()
                            }
                        }
                    } trailing: {
                        if version.version == detail.loop.version {
                            Text("Current").metaText(Theme.inkSecondary)
                        } else {
                            Button("Restore") { Task { await store.restore(version.version) } }
                                .buttonStyle(.compact)
                                .disabled(store.busy)
                        }
                    }
                }
            }
        }
    }

    private func times(_ run: WorkLoopRun) -> String {
        var parts = ["Queued \(loopDate(run.queuedAtMs).formatted(date: .abbreviated, time: .shortened))"]
        if let started = run.startedAtMs {
            parts.append("started \(loopDate(started).formatted(date: .omitted, time: .shortened))")
        }
        if let ended = run.endedAtMs {
            parts.append("ended \(loopDate(ended).formatted(date: .omitted, time: .shortened))")
        }
        return parts.joined(separator: ", ")
    }

    private func toolTitle(_ name: String) -> String {
        store.tools.first { $0.name == name }?.title ?? name
    }
}

private struct WorkLoopEditor: View {
    let store: LoopsStore
    let loop: WorkLoop?
    @Environment(AppModel.self) private var model
    @State private var draft: WorkLoopDraft

    init(store: LoopsStore, loop: WorkLoop?) {
        self.store = store
        self.loop = loop
        _draft = State(initialValue: WorkLoopDraft(loop, tools: store.tools))
    }

    var body: some View {
        let input = draft.input(tools: store.tools)
        let unchanged = loop.map { draft == WorkLoopDraft($0, tools: store.tools) } ?? false
        VStack(alignment: .leading, spacing: 0) {
            PageSection("Automation") {
                VStack(alignment: .leading, spacing: 6) {
                    Text("Name").metaText()
                    InputField(prompt: "Name", text: $draft.name)
                }
                VStack(alignment: .leading, spacing: 6) {
                    Text("Prompt").metaText()
                    TextEditor(text: $draft.prompt)
                        .font(TypeScale.body(14))
                        .foregroundStyle(Theme.ink)
                        .scrollContentBackground(.hidden)
                        .padding(8)
                        .frame(height: 140)
                        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
                        .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl).strokeBorder(Theme.border, lineWidth: 1))
                        .accessibilityLabel("Prompt")
                }
            }
            PageSection("Trigger") {
                Card { trigger }
            }
            if draft.kind.scope != .unavailable {
                PageSection("Only when the task matches") {
                    Card { conditions }
                }
            }
            PageSection("Tools") {
                Card { tools }
            }
            HStack {
                Spacer(minLength: 0)
                Button(loop == nil ? "Create automation" : "Save") {
                    guard let input else { return }
                    Task {
                        guard await store.save(input), let saved = store.detail?.loop else { return }
                        draft = WorkLoopDraft(saved, tools: store.tools)
                    }
                }
                .buttonStyle(.primary)
                .disabled(input == nil || unchanged || store.busy)
            }
            .padding(.bottom, 28)
        }
        .onChange(of: loop) { old, new in
            guard let old, let new, draft == WorkLoopDraft(old, tools: store.tools) else { return }
            draft = WorkLoopDraft(new, tools: store.tools)
        }
    }

    @ViewBuilder private var trigger: some View {
        let kinds = WorkLoopTriggerKind.choices + (draft.kind == .unknown ? [.unknown] : [])
        ChoiceRow(title: "Runs", choices: kinds, label: \.label, selection: $draft.kind)
        switch draft.kind {
        case .schedule:
            ChoiceRow(title: "Every", choices: WorkLoopEvery.choices, label: \.label, selection: $draft.every)
            if draft.every == .weekly {
                CardRow {
                    Text("On").bodyText()
                } trailing: {
                    HStack(spacing: 4) {
                        ForEach(WorkLoopWeekday.week) { day in
                            Toggle(day.short, isOn: Binding(
                                get: { draft.days.contains(day) },
                                set: { on in
                                    if on { draft.days.insert(day) } else { draft.days.remove(day) }
                                }))
                            .toggleStyle(.button)
                        }
                    }
                }
            }
            CardRow {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Time zone").bodyText()
                    if draft.zone == nil {
                        Text("Not a time zone name.").metaText(Theme.live)
                    }
                }
            } trailing: {
                InputField(prompt: "Time zone", text: $draft.timezone)
                    .frame(width: 220)
            }
            CardRow {
                VStack(alignment: .leading, spacing: 4) {
                    Text("First run").bodyText()
                    Text("Sets the time of day for every run.").metaText()
                }
            } trailing: {
                DatePicker("First run", selection: Binding(
                    get: { loopDate(draft.firstRunAtMs) },
                    set: { draft.firstRunAtMs = Int64(($0.timeIntervalSince1970 * 1000).rounded()) }),
                    displayedComponents: [.date, .hourAndMinute])
                    .labelsHidden()
                    .environment(\.timeZone, draft.zone ?? .current)
            }
        case .statusChanged:
            ChoiceRow(title: "To", choices: [nil] + WorkLoopTaskStatus.choices.map(Optional.some),
                      label: { $0?.label ?? "Any status" }, selection: $draft.statusTo)
        case .labelAdded:
            ChoiceRow(title: "Label", choices: [nil] + labelChoices.map { Optional.some($0.id) },
                      label: labelName, selection: $draft.labelId)
        case .projectStatusChanged:
            ChoiceRow(title: "To", choices: [nil] + WorkLoopProjectStatus.choices.map(Optional.some),
                      label: { $0?.label ?? "Any status" }, selection: $draft.projectStatusTo)
        case .unknown:
            CardLine("This trigger is from a newer Sona. Pick one to save changes.")
        case .taskCreated, .assigned, .commentAdded, .meetingEnded:
            EmptyView()
        }
    }

    @ViewBuilder private var conditions: some View {
        let work = model.work
        if draft.kind.scope == .tasks {
            conditionRow("Labels", names: labelChoices.filter { draft.labels.contains($0.id) }.map(\.name)) {
                ForEach(labelChoices) { label in
                    Toggle(label.name, isOn: member(\.labels, label.id))
                }
            }
        }
        conditionRow("Projects", names: work.projects.filter { draft.projects.contains($0.id) }.map(\.title)) {
            ForEach(work.projects) { project in
                Toggle(project.title, isOn: member(\.projects, project.id))
            }
        }
        if draft.kind.scope == .tasks {
            conditionRow("Statuses", names: WorkLoopTaskStatus.choices.filter { draft.statuses.contains($0) }.map(\.label)) {
                ForEach(WorkLoopTaskStatus.choices) { status in
                    Toggle(status.label, isOn: member(\.statuses, status))
                }
            }
        }
    }

    @ViewBuilder private var tools: some View {
        if store.tools.isEmpty {
            CardLine("No tools are available.")
        }
        ForEach(store.tools) { tool in
            CardRow {
                VStack(alignment: .leading, spacing: 2) {
                    Text(tool.title).bodyText(13)
                    Text(tool.name).metaText()
                }
            } trailing: {
                Toggle(tool.title, isOn: member(\.tools, tool.name))
                    .labelsHidden()
                    .toggleStyle(.checkbox)
            }
        }
    }

    private func conditionRow<Choices: View>(_ title: String, names: [String], @ViewBuilder choices: () -> Choices) -> some View {
        let content = choices()
        return CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text(title).bodyText()
                Text(names.isEmpty ? "Any" : names.joined(separator: ", ")).metaText().lineLimit(2)
            }
        } trailing: {
            Menu("Choose") { content }
                .menuStyle(.borderlessButton)
                .fixedSize()
        }
    }

    private func member<Value: Hashable>(_ path: WritableKeyPath<WorkLoopDraft, Set<Value>>, _ value: Value) -> Binding<Bool> {
        Binding(
            get: { draft[keyPath: path].contains(value) },
            set: { on in
                if on { draft[keyPath: path].insert(value) } else { draft[keyPath: path].remove(value) }
            })
    }

    private var labelChoices: [WorkLabel] {
        model.work.labels
            .filter { !$0.retired || draft.labels.contains($0.id) || draft.labelId == $0.id }
            .sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    }

    private func labelName(_ id: String?) -> String {
        guard let id else { return "Any label" }
        return model.work.labels.first { $0.id == id }?.name ?? "Unknown label"
    }
}

enum WorkLoopConditionScope {
    case unavailable, projects, tasks
}

enum WorkLoopTriggerKind: String, CaseIterable, Identifiable {
    case schedule, taskCreated, statusChanged, labelAdded, assigned, commentAdded, projectStatusChanged, meetingEnded
    case unknown

    static let choices: [Self] = allCases.filter { $0 != .unknown }

    var id: String { rawValue }

    var label: String {
        switch self {
        case .schedule: "On a schedule"
        case .taskCreated: "When a task is created"
        case .statusChanged: "When a task’s status changes"
        case .labelAdded: "When a label is added"
        case .assigned: "When a task is assigned or delegated"
        case .commentAdded: "When a comment is added"
        case .projectStatusChanged: "When a project’s status changes"
        case .meetingEnded: "When a meeting ends"
        case .unknown: "Unknown trigger"
        }
    }

    var scope: WorkLoopConditionScope {
        switch self {
        case .taskCreated, .statusChanged, .labelAdded, .assigned, .commentAdded: .tasks
        case .projectStatusChanged: .projects
        case .schedule, .meetingEnded, .unknown: .unavailable
        }
    }
}

struct WorkLoopDraft: Equatable {
    var name = ""
    var prompt = ""
    var kind = WorkLoopTriggerKind.schedule
    var every = WorkLoopEvery.daily
    var days: Set<WorkLoopWeekday> = []
    var timezone = TimeZone.current.identifier
    var firstRunAtMs: Int64
    var statusTo: WorkLoopTaskStatus?
    var labelId: String?
    var projectStatusTo: WorkLoopProjectStatus?
    var labels: Set<String> = []
    var projects: Set<String> = []
    var statuses: Set<WorkLoopTaskStatus> = []
    var tools: Set<String> = []

    init(_ loop: WorkLoop?, tools available: [WorkLoopTool]) {
        let hour = Calendar.current.dateInterval(of: .hour, for: .now)?.end ?? .now.addingTimeInterval(3600)
        firstRunAtMs = Int64((hour.timeIntervalSince1970 * 1000).rounded())
        guard let loop else {
            days = [WorkLoopWeekday(hour, in: .current)]
            tools = Set(available.map(\.name))
            return
        }
        name = loop.name
        prompt = loop.prompt
        labels = Set(loop.conditions.labels)
        projects = Set(loop.conditions.projects)
        statuses = Set(loop.conditions.statuses.filter { $0 != .unknown })
        tools = Set(loop.tools)
        switch loop.trigger {
        case let .schedule(schedule):
            kind = .schedule
            every = schedule.every
            days = Set(schedule.days.filter { $0 != .unknown })
            timezone = schedule.timezone
            firstRunAtMs = schedule.firstRunAtMs
        case .taskCreated: kind = .taskCreated
        case let .statusChanged(to):
            kind = .statusChanged
            statusTo = to
        case let .labelAdded(id):
            kind = .labelAdded
            labelId = id
        case .assigned: kind = .assigned
        case .commentAdded: kind = .commentAdded
        case let .projectStatusChanged(to):
            kind = .projectStatusChanged
            projectStatusTo = to
        case .meetingEnded: kind = .meetingEnded
        case .unknown: kind = .unknown
        }
    }

    var zone: TimeZone? { TimeZone(identifier: timezone.trimmingCharacters(in: .whitespaces)) }

    func input(tools available: [WorkLoopTool]) -> WorkLoopInput? {
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let prompt = prompt.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty, !prompt.isEmpty, let trigger else { return nil }
        let scope = kind.scope
        return WorkLoopInput(
            name: name, prompt: prompt, trigger: trigger,
            conditions: WorkLoopConditions(
                labels: scope == .tasks ? labels.sorted() : [],
                projects: scope == .unavailable ? [] : projects.sorted(),
                statuses: scope == .tasks ? WorkLoopTaskStatus.choices.filter(statuses.contains) : []),
            tools: available.map(\.name).filter(tools.contains))
    }

    private var trigger: WorkLoopTrigger? {
        switch kind {
        case .schedule:
            guard every != .unknown, let zone, every != .weekly || !days.isEmpty else { return nil }
            return .schedule(WorkLoopSchedule(
                every: every, days: WorkLoopWeekday.week.filter(days.contains),
                timezone: zone.identifier, firstRunAtMs: firstRunAtMs))
        case .taskCreated: return .taskCreated
        case .statusChanged: return .statusChanged(to: statusTo)
        case .labelAdded: return .labelAdded(labelId: labelId)
        case .assigned: return .assigned
        case .commentAdded: return .commentAdded
        case .projectStatusChanged: return .projectStatusChanged(to: projectStatusTo)
        case .meetingEnded: return .meetingEnded
        case .unknown: return nil
        }
    }
}

extension WorkLoopSchedule {
    var summary: String {
        let zone = TimeZone(identifier: timezone) ?? .current
        let first = loopDate(firstRunAtMs)
        let time = first.formatted(Date.FormatStyle(date: .omitted, time: .shortened, timeZone: zone))
        let place = zone.identifier == TimeZone.current.identifier ? "" : " (\(timezone))"
        switch every {
        case .hourly:
            return "Every hour"
        case .daily:
            return "Every day at \(time)\(place)"
        case .weekly:
            let names = WorkLoopWeekday.week.filter(days.contains).map(\.short)
            return "Every \(names.joined(separator: ", ")) at \(time)\(place)"
        case .monthly:
            var calendar = Calendar(identifier: .gregorian)
            calendar.timeZone = zone
            return "Every month on day \(calendar.component(.day, from: first)) at \(time)\(place)"
        case .unknown:
            return "On a schedule"
        }
    }
}

extension WorkLoopTrigger {
    func summary(labels: [WorkLabel]) -> String {
        switch self {
        case let .schedule(schedule):
            schedule.summary
        case .taskCreated:
            "When a task is created"
        case let .statusChanged(to):
            to.map { "When a task moves to \($0.label)" } ?? "When a task’s status changes"
        case let .labelAdded(labelId):
            labelId.map { id in "When \(labels.first { $0.id == id }?.name ?? "a label") is added" } ?? "When a label is added"
        case .assigned:
            "When a task is assigned or delegated"
        case .commentAdded:
            "When a comment is added"
        case let .projectStatusChanged(to):
            to.map { "When a project moves to \($0.label)" } ?? "When a project’s status changes"
        case .meetingEnded:
            "When a meeting ends"
        case .unknown:
            "Unknown trigger"
        }
    }
}

extension WorkLoop {
    /// When a scheduled automation that is on runs next.
    var nextRunText: String? {
        guard enabled, let next = nextRunAtMs else { return nil }
        return loopDate(next).formatted(date: .abbreviated, time: .shortened)
    }
}

private func loopDate(_ ms: Int64) -> Date {
    Date(timeIntervalSince1970: TimeInterval(ms) / 1000)
}

import SwiftUI

struct PlannerAgentView: View {
    let chat: ChatStore
    let onClose: () -> Void
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @FocusState private var goalFocused: Bool
    @State private var choosingSources = false

    private var store: PlannerAgentStore { chat.planner }

    var body: some View {
        VStack(spacing: 0) {
            ScrollViewReader { proxy in
                ScrollView {
                    VStack(alignment: .leading, spacing: 24) {
                        if store.turns.isEmpty {
                            Text("What are you planning?").font(.system(size: 23, weight: .semibold))
                                .padding(.top, 12)
                        }
                        ForEach(store.turns) { turn in
                            conversationTurn(turn).id(turn.id)
                        }
                        if store.draft == nil {
                            sourceSelector.disabled(store.busy)
                        }
                        if store.phase == .preparing || store.phase == .revising {
                            HStack(spacing: 10) {
                                ProgressView().controlSize(.small)
                                Text(store.phase == .revising ? "Revising on this Mac…" : "Preparing on this Mac…")
                                    .metaText(Theme.inkSecondary)
                            }
                            .accessibilityElement(children: .combine)
                        }
                    }
                    .foregroundStyle(Theme.ink)
                    .padding(.horizontal, 24)
                    .padding(.top, 20)
                    .padding(.bottom, 24)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                .scrollIndicators(.automatic)
                .defaultScrollAnchor(.top, for: .alignment)
                .onChange(of: store.turns.count) {
                    if let id = store.turns.last?.id { proxy.scrollTo(id, anchor: .top) }
                }
            }
            Hairline()
            actionBar
        }
        .sheet(isPresented: $choosingSources) {
            PlannerAgentSourcePicker(projects: model.projects, selected: store.selectedSources) {
                store.selectSources($0)
            }
        }
        .task { goalFocused = store.draft == nil }
        .transaction {
            if reduceMotion { $0.animation = nil; $0.disablesAnimations = true }
        }
    }

    @ViewBuilder
    private func conversationTurn(_ turn: PlanningConversationTurn) -> some View {
        switch turn.content {
        case .user(let text):
            ChatQuestion(message: text)
        case .notice(let text):
            Text(text).bodyText(13, Theme.inkSecondary)
                .fixedSize(horizontal: false, vertical: true)
        case .plan(let snapshot):
            if turn.id == store.revisionTurnId || (store.proposedRevision == nil && turn.id == store.currentPlanTurnId),
               let draft = store.reviewDraft {
                VStack(alignment: .leading, spacing: 20) {
                    HStack(alignment: .firstTextBaseline) {
                        Text(store.proposedRevision == nil ? "Draft plan" : "Proposed revision")
                            .font(.system(size: 19, weight: .semibold))
                            .accessibilityAddTraits(.isHeader)
                        Spacer(minLength: 8)
                        Text("\(draft.tasks.count) new tasks").metaText(Theme.inkSecondary)
                    }
                    if store.proposedRevision != nil, let previous = store.draft {
                        Text("The agent keeps chosen owners and dates. Change them in the plan.")
                            .metaText(Theme.inkSecondary)
                        DisclosureGroup("Compare with previous draft") {
                            planSnapshot(previous).padding(.top, 12)
                        }
                        .font(TypeScale.body(13))
                    }
                    projectEditor(draft)
                    milestoneEditor(draft)
                    taskEditor(draft)
                    linkedCommitments(draft)
                    sourceManifest(draft)
                }
                .disabled(store.busy || store.savedProjectId != nil)
            } else {
                DisclosureGroup("Earlier draft · \(snapshot.project.title)") {
                    planSnapshot(snapshot).padding(.top, 12)
                }
                .font(TypeScale.body(13))
            }
        }
    }

    private func planSnapshot(_ draft: PlanningPreparedDraft) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(draft.project.title).headlineText()
            if let description = draft.project.description { Text(description).bodyText(13) }
            citations(draft.project.citations, in: draft)
            ForEach(draft.milestones, id: \.key) { milestone in
                VStack(alignment: .leading, spacing: 6) {
                    Text(milestone.title).headlineText()
                    snapshotDate(milestone.dueAtMs)
                    citations(milestone.citations, in: draft)
                }
            }
            ForEach(draft.tasks, id: \.key) { task in
                VStack(alignment: .leading, spacing: 6) {
                    Text(task.title).bodyText(14)
                    if let details = task.details { Text(details).bodyText(13) }
                    Text(snapshotMembership(task.milestoneKey, in: draft)).metaText(Theme.inkSecondary)
                    Text(task.ownerIsSelf ? "Me" : task.ownerPersonId.map { id in
                        model.plannerPeople.first { $0.id == id }?.name ?? "Person unavailable"
                    } ?? "Unassigned").metaText(Theme.inkSecondary)
                    snapshotDate(task.dueAtMs)
                    citations(task.citations, in: draft)
                }
            }
            ForEach(draft.linkedCommitments, id: \.loopId) { commitment in
                VStack(alignment: .leading, spacing: 6) {
                    Text(commitment.title).bodyText(14)
                    Text(snapshotMembership(commitment.milestoneKey, in: draft)).metaText(Theme.inkSecondary)
                    citations(commitment.citations, in: draft)
                }
            }
        }
        .textSelection(.enabled)
    }

    private func snapshotMembership(_ key: String?, in draft: PlanningPreparedDraft) -> String {
        key.flatMap { key in draft.milestones.first { $0.key == key }?.title } ?? "Project · no milestone"
    }

    @ViewBuilder
    private func snapshotDate(_ value: Int64?) -> some View {
        if let value {
            Text(Date(timeIntervalSince1970: Double(value) / 1000), format: .dateTime.year().month().day())
                .metaText(Theme.inkSecondary)
        } else {
            Text("No due date").metaText(Theme.inkSecondary)
        }
    }

    private var sourceSelector: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(alignment: .firstTextBaseline) {
                Text("Saved sources").sectionLabel()
                Spacer()
                Text("\(store.selectedSources.count) of 12").metaText(Theme.inkSecondary)
                Button("Choose…") { choosingSources = true }.buttonStyle(.quiet)
            }
            if store.selectedSources.isEmpty {
                Text("No sources selected").metaText(Theme.inkSecondary)
            } else {
                VStack(alignment: .leading, spacing: 10) {
                    ForEach(store.selectedSources) { source in
                        HStack(alignment: .firstTextBaseline, spacing: 10) {
                            Image(systemName: source.kind == .note ? "note.text" : source.kind == .loop ? "link" : "waveform")
                                .foregroundStyle(Theme.inkTertiary).accessibilityHidden(true)
                            Text(source.title).bodyText(13).fixedSize(horizontal: false, vertical: true)
                            Spacer(minLength: 0)
                            Button {
                                store.selectSources(store.selectedSources.filter { $0.id != source.id })
                            } label: { Image(systemName: "xmark") }
                                .buttonStyle(.quiet)
                                .accessibilityLabel("Remove source \(source.title)")
                        }
                    }
                }
            }
        }
    }

    private func projectEditor(_ draft: PlanningPreparedDraft) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Project").sectionLabel().accessibilityAddTraits(.isHeader)
            TextField("Project title", text: projectBinding(draft.project, \.title), axis: .vertical)
                .font(.system(size: 19, weight: .semibold))
                .textFieldStyle(.plain)
                .accessibilityLabel("Project title")
            TextField("Description", text: Binding(
                get: { store.reviewDraft?.project.description ?? "" },
                set: { text in store.edit { $0.project.description = Self.optional(text) } }), axis: .vertical)
                .font(TypeScale.body(14))
                .textFieldStyle(.plain)
                .lineLimit(2...8)
                .accessibilityLabel("Project description")
            citations(draft.project.citations, in: draft) { citation in
                store.edit { edited in edited.project.citations.removeAll { $0 == citation } }
            }
        }
        .padding(16)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusCard))
        .overlay(RoundedRectangle(cornerRadius: Theme.radiusCard).strokeBorder(Theme.border, lineWidth: 1))
    }

    private func milestoneEditor(_ draft: PlanningPreparedDraft) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            sectionHeader("Milestones", action: "Add milestone", disabled: draft.milestones.count >= 16, perform: store.addMilestone)
            if draft.milestones.isEmpty { Text("No milestones").metaText(Theme.inkSecondary) }
            ForEach(draft.milestones, id: \.key) { milestone in
                VStack(alignment: .leading, spacing: 10) {
                    HStack(alignment: .top, spacing: 10) {
                        TextField("Milestone title", text: milestoneBinding(milestone, \.title), axis: .vertical)
                            .textFieldStyle(.roundedBorder)
                            .accessibilityLabel("Milestone title")
                        removeButton("Remove milestone \(milestone.title)") { store.removeMilestone(milestone.key) }
                    }
                    PlannerAgentDateField(value: milestoneBinding(milestone, \.dueAtMs))
                    citations(milestone.citations, in: draft) { citation in
                        store.edit { edited in
                            guard let index = edited.milestones.firstIndex(where: { $0.key == milestone.key }) else { return }
                            edited.milestones[index].citations.removeAll { $0 == citation }
                        }
                    }
                    Hairline()
                }
            }
        }
    }

    private func taskEditor(_ draft: PlanningPreparedDraft) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            sectionHeader("New tasks", action: "Add task", disabled: draft.tasks.count >= 32, perform: store.addTask)
            if draft.tasks.isEmpty { Text("No new tasks").metaText(Theme.inkSecondary) }
            ForEach(draft.tasks, id: \.key) { task in
                VStack(alignment: .leading, spacing: 10) {
                    HStack(alignment: .top, spacing: 10) {
                        TextField("Task title", text: taskBinding(task, \.title), axis: .vertical)
                            .textFieldStyle(.roundedBorder)
                            .accessibilityLabel("Task title")
                        removeButton("Remove task \(task.title)") {
                            store.edit { $0.tasks.removeAll { $0.key == task.key } }
                        }
                    }
                    TextField("Details", text: Binding(
                        get: { store.reviewDraft?.tasks.first { $0.key == task.key }?.details ?? "" },
                        set: { text in store.edit { draft in
                            guard let index = draft.tasks.firstIndex(where: { $0.key == task.key }) else { return }
                            draft.tasks[index].details = Self.optional(text)
                        } }), axis: .vertical)
                        .textFieldStyle(.plain)
                        .font(TypeScale.body(13))
                        .accessibilityLabel("Task details")
                    membership(taskBinding(task, \.milestoneKey), draft: draft)
                    Picker("Owner", selection: ownerBinding(task.key)) {
                        Text("Unassigned").tag(PlanningOwner.unassigned)
                        Text("Me").tag(PlanningOwner.myself)
                        ForEach(model.plannerPeople) { person in
                            Text(person.name).tag(PlanningOwner.person(person.id))
                        }
                        if let id = task.ownerPersonId, !model.plannerPeople.contains(where: { $0.id == id }) {
                            Text("Person unavailable").tag(PlanningOwner.person(id))
                        }
                    }
                    .pickerStyle(.menu)
                    .controlSize(.large)
                    .font(TypeScale.body(13))
                    if model.people.entries == nil || model.people.listFailed {
                        Text("People unavailable").metaText(Theme.inkSecondary)
                    }
                    PlannerAgentDateField(value: taskBinding(task, \.dueAtMs))
                    citations(task.citations, in: draft) { citation in
                        store.edit { edited in
                            guard let index = edited.tasks.firstIndex(where: { $0.key == task.key }) else { return }
                            edited.tasks[index].citations.removeAll { $0 == citation }
                        }
                    }
                    Hairline()
                }
            }
        }
    }

    @ViewBuilder
    private func linkedCommitments(_ draft: PlanningPreparedDraft) -> some View {
        if !draft.linkedCommitments.isEmpty {
            VStack(alignment: .leading, spacing: 14) {
                Text("Linked existing commitments").sectionLabel().accessibilityAddTraits(.isHeader)
                ForEach(draft.linkedCommitments, id: \.loopId) { commitment in
                    VStack(alignment: .leading, spacing: 10) {
                        HStack(alignment: .top, spacing: 10) {
                            Image(systemName: "link").foregroundStyle(Theme.inkSecondary).accessibilityHidden(true)
                            Text(commitment.title).bodyText(14).fixedSize(horizontal: false, vertical: true)
                            Spacer(minLength: 0)
                            removeButton("Unlink commitment \(commitment.title)") {
                                store.edit { $0.linkedCommitments.removeAll { $0.loopId == commitment.loopId } }
                            }
                        }
                        membership(Binding(
                            get: { store.reviewDraft?.linkedCommitments.first { $0.loopId == commitment.loopId }?.milestoneKey },
                            set: { value in store.edit { draft in
                                guard let index = draft.linkedCommitments.firstIndex(where: { $0.loopId == commitment.loopId }) else { return }
                                draft.linkedCommitments[index].milestoneKey = value
                            } }), draft: draft)
                        citations(commitment.citations, in: draft)
                        Hairline()
                    }
                }
            }
        }
    }

    private func sourceManifest(_ draft: PlanningPreparedDraft) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Prepared from").sectionLabel().accessibilityAddTraits(.isHeader)
            if draft.sources.isEmpty { Text("Goal only").metaText(Theme.inkSecondary) }
            ForEach(Array(draft.sources.enumerated()), id: \.offset) { index, source in
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    Text("\(index + 1). \(source.title)").bodyText(13)
                        .fixedSize(horizontal: false, vertical: true)
                    Spacer(minLength: 0)
                    Button("Open") { openSource(source, passage: nil) }.buttonStyle(.quiet)
                        .accessibilityLabel("Open saved source \(source.title)")
                }
            }
        }
    }

    @ViewBuilder
    private func citations(_ citations: [PlanningCitation], in draft: PlanningPreparedDraft,
                           remove: ((PlanningCitation) -> Void)? = nil) -> some View {
        if citations.isEmpty, !draft.sources.isEmpty {
            Text("No source linked.").metaText(Theme.inkSecondary)
        }
        if !citations.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(Array(citations.enumerated()), id: \.offset) { _, citation in
                    if citation.source > 0, Int(citation.source) <= draft.sources.count {
                        let source = draft.sources[Int(citation.source) - 1]
                        let passage = source.passages.first { $0.id == citation.passageId }
                        HStack(alignment: .firstTextBaseline, spacing: 8) {
                            Button {
                                openSource(source, passage: passage)
                            } label: {
                                HStack(alignment: .firstTextBaseline, spacing: 5) {
                                    Image(systemName: "arrow.up.right").accessibilityHidden(true)
                                    Text(source.title)
                                    if let at = passage?.atMs { Text(Self.offset(at)).monospacedDigit() }
                                    else if citation.passageId == "body" { Text("Note") }
                                    else if citation.passageId.hasPrefix("note:") { Text("Meeting note") }
                                }
                                .font(TypeScale.body(12))
                                .fixedSize(horizontal: false, vertical: true)
                                .multilineTextAlignment(.leading)
                            }
                            .buttonStyle(.quiet)
                            .accessibilityLabel("Open citation in \(source.title)\(passage?.atMs.map { " at " + Self.offset($0) } ?? "")")
                            if let remove {
                                Button { remove(citation) } label: { Image(systemName: "xmark") }
                                    .buttonStyle(.quiet)
                                    .accessibilityLabel("Remove citation in \(source.title)")
                            }
                        }
                    }
                }
            }
        }
    }

    private var actionBar: some View {
        VStack(alignment: .leading, spacing: 12) {
            if let error = store.error {
                Text(error).bodyText(13, Theme.live)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityLabel("Plan failed: \(error)")
            }
            if store.phase == .accepting {
                HStack(spacing: 10) {
                    ProgressView().controlSize(.small)
                    Text("Creating project…").metaText(Theme.inkSecondary)
                }
            } else if let id = store.savedProjectId {
                HStack {
                    Text("Project created").metaText(Theme.inkSecondary)
                    Spacer()
                    Button("Open project") { model.openProject(id); onClose() }.buttonStyle(.primary)
                }
            } else if store.proposedRevision != nil {
                HStack(spacing: 12) {
                    Button("Keep previous", action: store.keepPreviousDraft).buttonStyle(.quiet)
                    Spacer()
                    Button("Use revision", action: store.useRevision).buttonStyle(.primary)
                        .keyboardShortcut(.return, modifiers: .command)
                }
            } else {
                if store.draft != nil {
                    HStack(spacing: 12) {
                        Button("Discard") { store.reset(); goalFocused = true }.buttonStyle(.quiet)
                            .disabled(store.busy)
                        Spacer()
                        Button("Create project", action: approve)
                            .buttonStyle(.primary)
                            .disabled(!store.canApprove || unavailableOwner)
                    }
                    Text("Change owners and dates in the plan.").metaText(Theme.inkSecondary)
                }
                composer
            }
            if unavailableOwner { Text("Choose an available task owner.").metaText(Theme.live) }
        }
        .padding(.horizontal, 24)
        .padding(.vertical, 16)
        .background(Theme.page)
    }

    private var composer: some View {
        VStack(alignment: .leading, spacing: 10) {
            TextField(store.draft == nil ? "Describe the project" : "Ask for a change",
                      text: Binding(get: { store.goal }, set: store.editGoal), axis: .vertical)
                .textFieldStyle(.plain)
                .font(TypeScale.body(14))
                .lineLimit(1...4)
                .focused($goalFocused)
                .disabled(store.busy)
                .accessibilityLabel(store.draft == nil ? "Project goal" : "Message planner")
            HStack(spacing: 10) {
                Button {
                    if store.hasVoiceToStop { store.stopVoice() }
                    else { store.startVoice() }
                } label: {
                    Label(store.hasVoiceToStop ? "Stop microphone" : "Speak message",
                          systemImage: store.hasVoiceToStop ? "stop.fill" : "mic")
                        .labelStyle(.iconOnly)
                }
                .buttonStyle(.quiet)
                .disabled(store.busy || store.stoppingVoice || chat.stoppingVoice)
                .accessibilityHint("Transcribes one message on this Mac without sending it.")
                if let status = store.voiceLabel { Text(status).metaText(Theme.inkSecondary) }
                Spacer(minLength: 0)
                Button("Send", systemImage: "arrow.up", action: prepare)
                    .buttonStyle(.secondary)
                    .disabled(!store.canPrepare)
                    .keyboardShortcut(.return, modifiers: .command)
            }
        }
        .padding(12)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
        .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl)
            .strokeBorder(goalFocused ? Theme.accent : Theme.border, lineWidth: 1))
    }

    private var unavailableOwner: Bool {
        store.reviewDraft?.tasks.contains { task in
            task.ownerPersonId.map { id in !model.plannerPeople.contains { $0.id == id } } ?? false
        } ?? false
    }

    private func prepare() {
        goalFocused = false
        Task { await store.prepare { await chat.stopVoiceAndWait() } }
    }

    private func approve() {
        goalFocused = false
        Task {
            let id = await store.approve()
            if let id {
                model.openProject(id)
                onClose()
            }
            await model.projects.refresh()
        }
    }

    private func openSource(_ source: PlanningReviewSource, passage: PlanningPassage?) {
        var link = URLComponents()
        link.scheme = "sona"
        link.host = source.source.kind.rawValue
        link.path = "/" + source.source.id
        if let passage {
            if source.source.kind != .note {
                if source.source.kind == .loop, let meeting = source.source.id.split(separator: ":").first {
                    link.host = "meeting"
                    link.path = "/" + String(meeting)
                }
                var query: [URLQueryItem] = []
                if let at = passage.atMs { query.append(URLQueryItem(name: "atMs", value: String(at))) }
                if UUID(uuidString: passage.id) != nil { query.append(URLQueryItem(name: "segmentId", value: passage.id)) }
                link.queryItems = query.isEmpty ? nil : query
            }
        }
        guard let address = link.string else { return }
        store.preserveReviewForSource()
        model.open(link: address)
        onClose()
    }

    private func sectionHeader(_ title: String, action: String, disabled: Bool, perform: @escaping () -> Void) -> some View {
        HStack {
            Text(title).sectionLabel().accessibilityAddTraits(.isHeader)
            Spacer()
            Button(action, systemImage: "plus", action: perform).buttonStyle(.quiet).disabled(disabled)
        }
    }

    private func removeButton(_ label: String, action: @escaping () -> Void) -> some View {
        Button(action: action) { Image(systemName: "minus.circle") }
            .buttonStyle(.quiet)
            .frame(minWidth: 24, minHeight: 24)
            .accessibilityLabel(label)
    }

    private func membership(_ value: Binding<String?>, draft: PlanningPreparedDraft) -> some View {
        Picker("Milestone", selection: value) {
            Text("Project · no milestone").tag(nil as String?)
            ForEach(draft.milestones, id: \.key) { milestone in
                Text(milestone.title.isEmpty ? "Untitled milestone" : milestone.title).tag(Optional(milestone.key))
            }
        }
        .pickerStyle(.menu)
        .controlSize(.large)
        .font(TypeScale.body(13))
    }

    private func projectBinding<Value>(_ project: PlanningProjectDraft, _ path: WritableKeyPath<PlanningProjectDraft, Value>) -> Binding<Value> {
        Binding(get: {
            guard let current = store.reviewDraft?.project else { return project[keyPath: path] }
            return current[keyPath: path]
        }, set: { value in
            store.edit { $0.project[keyPath: path] = value }
        })
    }

    private func milestoneBinding<Value>(_ milestone: PlanningMilestoneDraft, _ path: WritableKeyPath<PlanningMilestoneDraft, Value>) -> Binding<Value> {
        Binding(get: {
            guard let current = store.reviewDraft?.milestones.first(where: { $0.key == milestone.key }) else {
                return milestone[keyPath: path]
            }
            return current[keyPath: path]
        }, set: { value in
            store.edit { draft in
                guard let index = draft.milestones.firstIndex(where: { $0.key == milestone.key }) else { return }
                draft.milestones[index][keyPath: path] = value
            }
        })
    }

    private func taskBinding<Value>(_ task: PlanningTaskDraft, _ path: WritableKeyPath<PlanningTaskDraft, Value>) -> Binding<Value> {
        Binding(get: {
            guard let current = store.reviewDraft?.tasks.first(where: { $0.key == task.key }) else {
                return task[keyPath: path]
            }
            return current[keyPath: path]
        }, set: { value in
            store.edit { draft in
                guard let index = draft.tasks.firstIndex(where: { $0.key == task.key }) else { return }
                draft.tasks[index][keyPath: path] = value
            }
        })
    }

    private func ownerBinding(_ key: String) -> Binding<PlanningOwner> {
        Binding(get: {
            guard let task = store.reviewDraft?.tasks.first(where: { $0.key == key }) else { return .unassigned }
            if task.ownerIsSelf { return .myself }
            return task.ownerPersonId.map(PlanningOwner.person) ?? .unassigned
        }, set: { value in store.edit { draft in
            guard let index = draft.tasks.firstIndex(where: { $0.key == key }) else { return }
            draft.tasks[index].ownerIsSelf = value == .myself
            if case let .person(id) = value { draft.tasks[index].ownerPersonId = id }
            else { draft.tasks[index].ownerPersonId = nil }
        } })
    }

    private static func optional(_ text: String) -> String? {
        text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : text
    }

    private static func offset(_ milliseconds: Int64) -> String {
        let seconds = milliseconds / 1000
        return seconds >= 3600
            ? String(format: "%lld:%02lld:%02lld", seconds / 3600, (seconds / 60) % 60, seconds % 60)
            : String(format: "%lld:%02lld", seconds / 60, seconds % 60)
    }
}

private struct PlannerAgentDateField: View {
    @Binding var value: Int64?

    var body: some View {
        HStack(spacing: 12) {
            Toggle("Due date", isOn: Binding(
                get: { value != nil },
                set: { value = $0 ? ProjectsStore.milliseconds(Calendar.current.startOfDay(for: .now)) : nil }))
                .toggleStyle(.checkbox)
            if value != nil {
                DatePicker("Due date", selection: Binding(
                    get: { Date(timeIntervalSince1970: Double(value ?? 0) / 1000) },
                    set: { value = ProjectsStore.milliseconds($0) }), displayedComponents: .date)
                    .labelsHidden()
            }
            Spacer(minLength: 0)
        }
        .font(TypeScale.body(13))
    }
}

private struct PlannerAgentSourcePicker: View {
    let projects: ProjectsStore
    let onSelect: ([PlanningSourceChoice]) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var selected: [PlanningSourceChoice]
    @State private var kind: PlanningSourceList = .meeting
    @State private var query = ""
    @State private var meeting: PlanningSourceChoice?
    @State private var rows: [PlanningSourceChoice] = []
    @State private var cursor: Int64?
    @State private var loading = false
    @State private var error: String?
    @State private var sequence = 0

    init(projects: ProjectsStore, selected: [PlanningSourceChoice], onSelect: @escaping ([PlanningSourceChoice]) -> Void) {
        self.projects = projects
        self.onSelect = onSelect
        _selected = State(initialValue: selected)
    }

    private var requestKey: String {
        "\(kind.rawValue)\u{1f}\(meeting?.sourceId ?? "")\u{1f}\(query)\u{1f}\(projects.sourcesRevision)"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                Text("Saved sources").titleText()
                Spacer()
                Text("\(selected.count) of 12").metaText(Theme.inkSecondary)
                Button("Cancel") { dismiss() }.buttonStyle(.quiet)
            }
            Picker("Source type", selection: $kind) {
                ForEach(PlanningSourceList.allCases) { Text($0.label).tag($0) }
            }
            .pickerStyle(.segmented)
            .onChange(of: kind) { meeting = nil; query = "" }
            if let meeting {
                HStack(alignment: .firstTextBaseline) {
                    Button("Meetings", systemImage: "chevron.left") { self.meeting = nil; query = "" }
                        .buttonStyle(.quiet)
                    Text(meeting.title).bodyText(13).lineLimit(2)
                }
            }
            SearchField(prompt: kind == .loop && meeting != nil ? "Search commitments" : "Search saved sources", text: $query)
                .accessibilityLabel("Search saved sources")
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    if rows.isEmpty, !loading, error == nil {
                        Text(kind == .loop && meeting == nil ? "No matching meetings" : "No matching sources")
                            .bodyText(13, Theme.inkSecondary).padding(.vertical, 18)
                    }
                    ForEach(rows) { source in sourceRow(source) }
                    if let error {
                        Text(error).bodyText(13, Theme.live).padding(.vertical, 12)
                        Button("Retry") { Task { await load(replacing: true) } }.buttonStyle(.quiet)
                    }
                    if loading {
                        ProgressView().controlSize(.small).padding(.vertical, 16)
                            .accessibilityLabel("Loading saved sources")
                    } else if cursor != nil {
                        Button("Load more") { Task { await load(replacing: false) } }
                            .buttonStyle(.quiet).padding(.vertical, 12)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            Hairline()
            HStack {
                if !selected.isEmpty {
                    Button("Clear selection") { selected = [] }.buttonStyle(.quiet)
                }
                Spacer()
                Button("Use selected sources") { onSelect(selected); dismiss() }.buttonStyle(.primary)
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(24)
        .frame(width: 512, height: 548)
        .background(Theme.page)
        .task(id: requestKey) { await load(replacing: true) }
        .onExitCommand { dismiss() }
    }

    private func sourceRow(_ source: PlanningSourceChoice) -> some View {
        let entersMeeting = kind == .loop && meeting == nil
        let isSelected = selected.contains { $0.id == source.id }
        return Button {
            if entersMeeting { meeting = source; query = "" }
            else if isSelected { selected.removeAll { $0.id == source.id } }
            else if selected.count < 12 { selected.append(source) }
        } label: {
            HStack(alignment: .firstTextBaseline, spacing: 12) {
                if !entersMeeting {
                    Image(systemName: isSelected ? "checkmark.square.fill" : "square")
                        .foregroundStyle(isSelected ? Theme.accent : Theme.inkSecondary).accessibilityHidden(true)
                }
                Text(source.title).bodyText(14).multilineTextAlignment(.leading)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 0)
                if entersMeeting { Image(systemName: "chevron.right").foregroundStyle(Theme.inkSecondary) }
            }
            .padding(.vertical, 12)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!entersMeeting && !isSelected && selected.count >= 12)
        .accessibilityLabel(entersMeeting ? "Choose commitments from \(source.title)" : source.title)
        .accessibilityValue(entersMeeting ? "" : isSelected ? "Selected" : "Not selected")
        .overlay(alignment: .bottom) { Hairline() }
    }

    private func load(replacing: Bool) async {
        guard replacing || !loading else { return }
        sequence += 1
        let request = sequence
        loading = true
        error = nil
        if replacing { rows = []; cursor = nil }
        defer { if request == sequence { loading = false } }
        do {
            let page = try await projects.sourceCandidates(kind, query: query,
                after: replacing ? nil : cursor, meetingId: meeting?.sourceId)
            guard request == sequence, !Task.isCancelled else { return }
            if replacing { rows = page.entries }
            else {
                let existing = Set(rows.map(\.id))
                rows.append(contentsOf: page.entries.filter { !existing.contains($0.id) })
            }
            cursor = page.nextCursor
        } catch {
            guard request == sequence, !Task.isCancelled else { return }
            self.error = (error as? CoreError)?.remote(as: String.self) ?? error.localizedDescription
        }
    }
}

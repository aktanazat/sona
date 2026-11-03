import SwiftUI

struct PersonContinuityView<Profile: View>: View {
    let core: Core
    let personId: String
    let revision: UInt64
    let openMeeting: (String) -> Void
    let openContribution: ((String, UInt64) -> Void)?
    @ViewBuilder let profile: () -> Profile

    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var memory: PersonMemoryStore?
    @State private var work: PersonWorkStore?
    @State private var mode: Mode = .history
    @State private var comparisonDate = Calendar.current.date(byAdding: .day, value: -7, to: .now) ?? .now

    private enum Mode: String, CaseIterable, Identifiable {
        case history = "History"
        case catchUp = "Before we talk"
        case work = "Work"
        case changes = "What changed"
        case subjects = "Subjects"
        case profile = "Profile"
        var id: String { rawValue }
    }

    private struct LoadKey: Equatable {
        let personId: String
        let revision: UInt64
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            modeNavigation
                .accessibilityLabel("Person view")
                .help("Switch views with Control–Option–1 through Control–Option–6")
            if mode == .profile {
                profile()
            } else if mode == .work {
                workContent
            } else if let memory {
                if let error = memory.error {
                    HStack(alignment: .firstTextBaseline, spacing: 12) {
                        Text(error).bodyText(14, Theme.inkSecondary)
                        Button("Retry") { memory.refresh() }.buttonStyle(.compact)
                    }
                } else if let snapshot = memory.snapshot, snapshot.personId == personId {
                    content(snapshot, memory: memory)
                    DisclosureGroup("Evidence limits") {
                        VStack(alignment: .leading, spacing: 6) {
                            ForEach(snapshot.limitations, id: \.self) { Text($0).metaText(Theme.inkSecondary) }
                        }
                        .padding(.top, 6)
                    }
                    .font(TypeScale.body(12))
                    .foregroundStyle(Theme.inkSecondary)
                } else {
                    ProgressView("Reading history…").controlSize(.small)
                        .frame(minHeight: 100, alignment: .topLeading)
                }
            }
        }
        .background {
            Group {
                Button("History") { mode = .history }.keyboardShortcut("1", modifiers: [.control, .option])
                Button("Before we talk") { mode = .catchUp }.keyboardShortcut("2", modifiers: [.control, .option])
                Button("Work") { mode = .work }.keyboardShortcut("3", modifiers: [.control, .option])
                Button("What changed") { mode = .changes }.keyboardShortcut("4", modifiers: [.control, .option])
                Button("Subjects") { mode = .subjects }.keyboardShortcut("5", modifiers: [.control, .option])
                Button("Profile") { mode = .profile }.keyboardShortcut("6", modifiers: [.control, .option])
            }
            .hidden()
            .accessibilityHidden(true)
        }
        .task(id: LoadKey(personId: personId, revision: revision)) {
            let memory = self.memory ?? PersonMemoryStore(core: core)
            self.memory = memory
            await memory.load(personId)
        }
        .task(id: LoadKey(personId: personId, revision: revision)) {
            let work = self.work ?? PersonWorkStore(core: core)
            self.work = work
            await work.load(personId)
        }
        .onDisappear {
            memory?.stop()
            work?.stop()
        }
    }

    @ViewBuilder
    private var modeNavigation: some View {
        if model.about.material == .glass {
            if #available(macOS 26.0, *) {
                modeControls
                    .controlSize(.large)
                    .padding(8)
                    .glassEffect(.regular, in: RoundedRectangle(cornerRadius: Theme.radiusControl + 8, style: .continuous))
            } else {
                modeControls
                    .controlSize(.large)
                    .padding(8)
                    .background(.regularMaterial, in: RoundedRectangle(cornerRadius: Theme.radiusControl + 8, style: .continuous))
                    .overlay {
                        RoundedRectangle(cornerRadius: Theme.radiusControl + 8, style: .continuous)
                            .strokeBorder(Theme.border, lineWidth: 1)
                            .allowsHitTesting(false)
                    }
            }
        } else {
            modeControls
        }
    }

    private var modeControls: some View {
        ViewThatFits(in: .horizontal) {
            modePicker.pickerStyle(.segmented).fixedSize(horizontal: true, vertical: false)
            modePicker.pickerStyle(.menu)
        }
    }

    private var modePicker: some View {
        Picker("Person view", selection: $mode) {
            ForEach(Mode.allCases) { mode in Text(mode.rawValue).tag(mode) }
        }
        .labelsHidden()
        .animation(reduceMotion ? nil : .easeOut(duration: 0.16), value: mode)
    }

    @ViewBuilder
    private func content(_ snapshot: PersonMemorySnapshot, memory: PersonMemoryStore) -> some View {
        switch mode {
        case .history:
            history(snapshot, memory: memory)
        case .catchUp:
            catchUp(snapshot)
        case .work:
            workContent
        case .changes:
            changes(snapshot, memory: memory)
        case .subjects:
            subjects(snapshot)
        case .profile:
            profile()
        }
    }

    private func history(_ snapshot: PersonMemorySnapshot, memory: PersonMemoryStore) -> some View {
        @Bindable var memory = memory
        return VStack(alignment: .leading, spacing: 12) {
            HStack {
                Picker("History filter", selection: $memory.filter) {
                    ForEach(PersonHistoryFilter.allCases) { filter in Text(filter.rawValue).tag(filter) }
                }
                .pickerStyle(.menu)
                .labelsHidden()
                Spacer()
                Button { memory.refresh() } label: { Image(systemName: "arrow.clockwise") }
                    .buttonStyle(.quiet)
                    .help("Refresh history")
                    .accessibilityLabel("Refresh history")
                    .disabled(memory.loading)
            }
            if memory.filteredTimeline.isEmpty {
                Text(emptyHistory(memory.filter)).bodyText(14, Theme.inkSecondary)
                    .padding(.vertical, 12)
            } else {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(memory.filteredTimeline.prefix(memory.visibleCount)) { entry in
                        CardRow {
                            switch entry {
                            case let .conversation(row):
                                conversation(row)
                            case let .contribution(row):
                                PersonMemoryContributionRow(row: row, open: open)
                            case let .note(row):
                                PersonMemoryNoteRow(row: row, open: open)
                            case let .commitment(row):
                                PersonMemoryCommitmentRow(row: row, open: open)
                            }
                        }
                    }
                    if memory.visibleCount < memory.filteredTimeline.count {
                        Button("Show earlier") { memory.showMore() }
                            .buttonStyle(.quiet)
                            .padding(16)
                    }
                }
                .background(Theme.surface)
            }
            if memory.filter == .contributions && snapshot.contributions.isEmpty && !snapshot.conversations.isEmpty {
                Text("No voice is attributed to this person in these conversations.")
                    .metaText(Theme.inkSecondary)
            }
        }
    }

    private func conversation(_ row: PersonMemoryConversation) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            PersonMemorySourceButton(source: row.source, open: open)
            HStack(spacing: 10) {
                Text(row.confidence == .suggested ? "Suggested link" : "Conversation").metaText(Theme.inkSecondary)
                if row.transcriptAvailable && row.attributedContributions == 0 {
                    Text("No attributed words").metaText(Theme.inkSecondary)
                }
            }
        }
    }

    private func catchUp(_ snapshot: PersonMemorySnapshot) -> some View {
        let catchUp = snapshot.catchUp
        return VStack(alignment: .leading, spacing: 20) {
            if let last = catchUp.lastConversation {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Last conversation").sectionLabel()
                    PersonMemorySourceButton(source: last, open: open)
                }
            } else {
                Text("No confirmed conversation yet.").bodyText(14, Theme.inkSecondary)
            }
            if !catchUp.contributionIds.isEmpty {
                PageSection("Their words") {
                    ForEach(snapshot.contributions.filter { catchUp.contributionIds.contains($0.id) }) { row in
                        PersonMemoryContributionRow(row: row, open: open)
                            .padding(.vertical, 6)
                    }
                }
            }
            let commitments = snapshot.commitments.filter { catchUp.commitmentIds.contains($0.id) }
            PageSection("Recent open commitments") {
                if commitments.isEmpty {
                    Text("No open commitments with retained evidence.").bodyText(14, Theme.inkSecondary)
                } else {
                    ForEach(commitments) { row in PersonMemoryCommitmentRow(row: row, open: open) }
                }
            }
            let notes = snapshot.notes.filter { catchUp.noteIds.contains($0.id) }
            if !notes.isEmpty {
                PageSection("Related notes") {
                    ForEach(notes) { row in PersonMemoryNoteRow(row: row, open: open) }
                }
            }
            let changes = snapshot.changes.filter { catchUp.changeIds.contains($0.id) }
            if !changes.isEmpty {
                PageSection("Source differences") {
                    ForEach(changes) { row in PersonMemoryChangeRow(row: row, open: open) }
                }
            }
        }
    }

    private func changes(_ snapshot: PersonMemorySnapshot, memory: PersonMemoryStore) -> some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(spacing: 10) {
                DatePicker("Since", selection: $comparisonDate, in: ...Date.now, displayedComponents: [.date])
                    .datePickerStyle(.field)
                    .fixedSize()
                Button("Compare") { memory.compare(since: comparisonDate) }
                    .buttonStyle(.compact)
                    .disabled(memory.loading)
            }
            if let since = snapshot.sinceUtcMs {
                Text("Since \(PeopleFormat.moment(PeopleFormat.date(since)))").metaText(Theme.inkSecondary)
            }
            if snapshot.changes.isEmpty {
                Text("No retained source differences for this period.").bodyText(14, Theme.inkSecondary)
            } else {
                LazyVStack(alignment: .leading, spacing: 18) {
                    ForEach(snapshot.changes) { row in PersonMemoryChangeRow(row: row, open: open) }
                }
            }
        }
    }

    private func subjects(_ snapshot: PersonMemorySnapshot) -> some View {
        LazyVStack(alignment: .leading, spacing: 18) {
            if snapshot.threads.isEmpty {
                Text("No linked subjects yet.").bodyText(14, Theme.inkSecondary)
            }
            ForEach(snapshot.threads) { thread in
                VStack(alignment: .leading, spacing: 8) {
                    HStack(alignment: .firstTextBaseline, spacing: 12) {
                        Text(thread.title).headlineText()
                        Spacer(minLength: 0)
                        if thread.origin == .explicitSubject {
                            Button("Open subject") {
                                if let url = URL(string: "sona://subject/\(thread.id.replacingOccurrences(of: "subject:", with: ""))") {
                                    model.open(url: url)
                                }
                            }
                            .buttonStyle(.quiet)
                        }
                    }
                    Text(thread.origin == .explicitSubject ? "Linked subject" : "AI topic · matching labels")
                        .metaText(Theme.inkSecondary)
                    if thread.origin == .explicitSubject {
                        ForEach(thread.sources) { source in PersonMemorySourceButton(source: source, open: open) }
                    } else {
                        DisclosureGroup("Source words") {
                            ForEach(thread.evidence, id: \.source.id) { passage in
                                PersonMemoryPassageView(passage: passage, open: open)
                                    .padding(.vertical, 6)
                            }
                        }
                        .font(TypeScale.body(12))
                        .foregroundStyle(Theme.inkSecondary)
                    }
                }
                Hairline()
            }
        }
    }

    @ViewBuilder
    private var workContent: some View {
        if let work {
            VStack(alignment: .leading, spacing: 16) {
                HStack {
                    Spacer()
                    Button { work.refresh() } label: { Image(systemName: "arrow.clockwise") }
                        .buttonStyle(.quiet)
                        .help("Refresh work")
                        .accessibilityLabel("Refresh work")
                        .disabled(work.loading)
                }
                if let error = work.error {
                    HStack(alignment: .firstTextBaseline, spacing: 12) {
                        Text(error).bodyText(14, Theme.inkSecondary)
                        Button("Retry") { work.refresh() }.buttonStyle(.compact)
                    }
                } else if let snapshot = work.snapshot, snapshot.personId == personId {
                    switch snapshot.identityStatus {
                    case .unavailable:
                        Text("This person is no longer available.").bodyText(14, Theme.inkSecondary)
                    case .merged:
                        HStack(alignment: .firstTextBaseline, spacing: 12) {
                            Text("This person was merged.").bodyText(14, Theme.inkSecondary)
                            if let id = snapshot.canonicalPersonId {
                                Button("Open current person") { model.openPerson(id) }.buttonStyle(.compact)
                            }
                        }
                    case .available:
                        selectedTasks(snapshot.tasks)
                        linkedWorkConversations(snapshot.conversations)
                    }
                } else {
                    ProgressView("Reading work…").controlSize(.small)
                        .frame(minHeight: 100, alignment: .topLeading)
                }
            }
        } else {
            ProgressView("Reading work…").controlSize(.small)
                .frame(minHeight: 100, alignment: .topLeading)
        }
    }

    private func selectedTasks(_ rows: [PersonWorkTask]) -> some View {
        PageSection("Tasks you selected") {
            if rows.isEmpty {
                Text("No tasks selected for this person.").bodyText(14, Theme.inkSecondary)
            } else {
                Card {
                    ForEach(rows) { row in
                        CardRow(action: { model.openProject(row.project.id) }) {
                            VStack(alignment: .leading, spacing: 6) {
                                Text(row.task.title).bodyText(14)
                                    .fixedSize(horizontal: false, vertical: true)
                                Text("\(row.project.title) · \(row.project.status.label)")
                                    .metaText(Theme.inkSecondary)
                                    .fixedSize(horizontal: false, vertical: true)
                                if let milestone = row.milestone {
                                    Text("\(milestone.title) · \(milestone.status.label)")
                                        .metaText(Theme.inkSecondary)
                                        .fixedSize(horizontal: false, vertical: true)
                                }
                                Text("Private task · \(row.task.status.label)").metaText(Theme.inkSecondary)
                                if let due = row.task.dueAtMs {
                                    Text(PlanningFormat.date(due)).metaText(Theme.inkSecondary)
                                }
                            }
                        } trailing: {
                            Image(systemName: "chevron.right")
                                .font(.system(size: 11, weight: .semibold))
                                .foregroundStyle(Theme.inkTertiary)
                                .accessibilityHidden(true)
                        }
                        .accessibilityLabel("\(row.task.title), \(row.task.status.label), private task selected by you. Open project \(row.project.title)")
                    }
                }
            }
        }
    }

    private func linkedWorkConversations(_ rows: [PersonWorkConversation]) -> some View {
        PageSection("Confirmed conversation context") {
            if rows.isEmpty {
                Text("No confirmed conversations linked to projects.").bodyText(14, Theme.inkSecondary)
            } else {
                Card {
                    ForEach(rows) { row in
                        CardRow {
                            VStack(alignment: .leading, spacing: 8) {
                                Button { openWorkConversation(row) } label: {
                                    VStack(alignment: .leading, spacing: 4) {
                                        Text(row.meetingTitle).bodyText(14, Theme.accent)
                                            .fixedSize(horizontal: false, vertical: true)
                                        let moment = row.meetingStartedAtMs.map {
                                            PeopleFormat.moment(PeopleFormat.date($0))
                                        }
                                        let detail = [moment, row.timestamp].compactMap { $0 }.joined(separator: " · ")
                                        if !detail.isEmpty {
                                            Text(detail).metaText(Theme.inkSecondary).monospacedDigit()
                                        }
                                    }
                                }
                                .buttonStyle(.quiet)
                                .accessibilityLabel("Open conversation \(row.meetingTitle)\(row.timestamp.map { " at \($0)" } ?? "")")
                                if row.targetKind != .project {
                                    Text(row.targetTitle).metaText(Theme.inkSecondary)
                                        .fixedSize(horizontal: false, vertical: true)
                                }
                                Button { model.openProject(row.project.id) } label: {
                                    Text("\(row.project.title) · \(row.project.status.label)")
                                        .bodyText(13, Theme.accent)
                                        .fixedSize(horizontal: false, vertical: true)
                                }
                                .buttonStyle(.quiet)
                                .accessibilityLabel("Open project \(row.project.title)")
                            }
                        }
                    }
                }
            }
        }
    }

    private func openWorkConversation(_ row: PersonWorkConversation) {
        if let atMs = row.atMs, let offset = UInt64(exactly: atMs), let openContribution {
            openContribution(row.meetingId, offset)
        } else {
            openMeeting(row.meetingId)
        }
    }

    private func open(_ source: PersonMemorySource) {
        if let meetingId = source.meetingId {
            if let offset = source.startOffsetNs, let openContribution {
                openContribution(meetingId, offset / 1_000_000)
            } else {
                openMeeting(meetingId)
            }
        } else if let url = URL(string: source.link) {
            model.open(url: url)
        }
    }

    private func emptyHistory(_ filter: PersonHistoryFilter) -> String {
        switch filter {
        case .all: "No linked history yet."
        case .conversations: "No linked conversations."
        case .contributions: "No attributed words."
        case .notes: "No related notes."
        case .commitments: "No sourced commitments."
        }
    }
}

struct PersonMemorySourceButton: View {
    let source: PersonMemorySource
    let open: (PersonMemorySource) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Button { open(source) } label: {
                VStack(alignment: .leading, spacing: 3) {
                    Text(source.title).font(TypeScale.body(13)).foregroundStyle(Theme.accent)
                        .lineLimit(2)
                    Text([PeopleFormat.moment(source.at), source.timestamp].compactMap { $0 }.joined(separator: " · "))
                        .metaText(Theme.inkSecondary).monospacedDigit()
                }
            }
            .buttonStyle(.quiet)
            .accessibilityLabel("Open \(source.title)\(source.timestamp.map { " at \($0)" } ?? "")")
            if source.transcriptAvailable == false {
                Text("Transcript unavailable").metaText(Theme.inkSecondary)
            } else if source.audioRetained == false {
                Text("Recording unavailable · transcript retained").metaText(Theme.inkSecondary)
            }
        }
    }
}

struct PersonMemoryPassageView: View {
    let passage: PersonMemoryPassage
    let open: (PersonMemorySource) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(passage.speakerLabel).font(TypeScale.body(12)).foregroundStyle(Theme.inkSecondary)
                Text(passage.correctedText == nil ? "Transcript" : "Corrected transcript").metaText(Theme.inkSecondary)
            }
            Text(passage.correctedText ?? passage.rawText).bodyText(14).textSelection(.enabled)
            if passage.correctedText != nil {
                DisclosureGroup("Original transcript") {
                    Text(passage.rawText).bodyText(13, Theme.inkSecondary).textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .font(TypeScale.body(12))
                .foregroundStyle(Theme.inkSecondary)
            }
            PersonMemorySourceButton(source: passage.source, open: open)
        }
    }
}

struct PersonMemoryContributionRow: View {
    let row: PersonMemoryContribution
    let open: (PersonMemorySource) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 9) {
            if row.attribution == .recognized {
                Text("Recognized voice").metaText(Theme.inkSecondary)
            }
            PersonMemoryPassageView(passage: row.passage, open: open)
            if row.before != nil || row.after != nil {
                DisclosureGroup("Surrounding dialogue") {
                    VStack(alignment: .leading, spacing: 14) {
                        if let before = row.before { PersonMemoryPassageView(passage: before, open: open) }
                        PersonMemoryPassageView(passage: row.passage, open: open)
                        if let after = row.after { PersonMemoryPassageView(passage: after, open: open) }
                    }
                    .padding(.top, 8)
                }
                .font(TypeScale.body(12))
                .foregroundStyle(Theme.inkSecondary)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

struct PersonMemoryNoteRow: View {
    let row: PersonMemoryNote
    let open: (PersonMemorySource) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(row.includesAiSynthesis ? "Note · includes AI synthesis" : "Saved note")
                .metaText(Theme.inkSecondary)
            Text(row.body).bodyText(14).lineLimit(5).textSelection(.enabled)
            PersonMemorySourceButton(source: row.source, open: open)
            if !row.directlyLinked, !row.relatedSources.isEmpty {
                DisclosureGroup("From a linked conversation") {
                    ForEach(row.relatedSources) { source in PersonMemorySourceButton(source: source, open: open) }
                }
                .font(TypeScale.body(12))
                .foregroundStyle(Theme.inkSecondary)
            }
        }
        .padding(.vertical, 6)
    }
}

struct PersonMemoryCommitmentRow: View {
    let row: PersonMemoryCommitment
    let open: (PersonMemorySource) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("AI interpretation · \(status)").metaText(Theme.inkSecondary)
            Text(row.interpretation).bodyText(14).textSelection(.enabled)
            Text(relation).metaText(Theme.inkSecondary)
            DisclosureGroup("Evidence") {
                ForEach(row.evidence, id: \.source.id) { passage in
                    PersonMemoryPassageView(passage: passage, open: open).padding(.vertical, 6)
                }
            }
            .font(TypeScale.body(12))
            .foregroundStyle(Theme.inkSecondary)
        }
        .padding(.vertical, 6)
    }

    private var relation: String {
        switch row.relation {
        case .explicitOwner: "Assigned to this person"
        case .attributedSpeech: "From this person's words"
        case .sharedConversation: "From a shared conversation"
        }
    }

    private var status: String {
        switch row.status {
        case .open: "Open"
        case .done: "Done"
        case .dropped: "Dropped"
        case .carried: "Carried forward"
        }
    }
}

struct PersonMemoryChangeRow: View {
    let row: PersonMemoryChange
    let open: (PersonMemorySource) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(row.title).headlineText()
            Text(row.kind == .noteEdited ? "Note edited" : "Different wording · AI-matched topic, not a confirmed change")
                .metaText(Theme.inkSecondary)
            side(row.earlier, label: "Earlier")
            side(row.later, label: "Later")
            Hairline()
        }
    }

    private func side(_ side: PersonMemoryComparisonSide, label: String) -> some View {
        VStack(alignment: .leading, spacing: 7) {
            Text(side.includesAiInterpretation
                 ? "\(label) · includes AI interpretation"
                 : side.source.kind == .transcript ? "\(label) · original transcript" : label)
                .metaText(Theme.inkSecondary)
            Text(side.text).bodyText(14).textSelection(.enabled)
            PersonMemorySourceButton(source: side.source, open: open)
        }
    }
}

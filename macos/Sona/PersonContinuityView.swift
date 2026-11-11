import SwiftUI

/// The parts of a person's page below the numbers, one at a time.
enum PersonPageTab: String, CaseIterable, Identifiable {
    case history = "History"
    case catchUp = "Before we talk"
    case work = "Work"
    case changes = "What changed"
    case subjects = "Subjects"
    case profile = "Profile"

    var id: String { rawValue }
}

/// A person's page below the numbers: their history, what to know before you
/// next talk, the work you share, what changed, your subjects, and the
/// profile. The page owns the reads behind each tab, so moving between tabs
/// never reads anything again.
struct PersonContinuityView<Profile: View>: View {
    let personId: String
    let memory: PersonMemoryStore?
    let work: PersonWorkStore?
    /// What is still open between you, from the person's ledger.
    let openLoops: [PersonOpenLoop]
    /// Plans that named this person whose time passed with no recording.
    let plansWithoutRecord: [HeardPlan]
    @Binding var tab: PersonPageTab
    let openMeeting: (String) -> Void
    let openContribution: ((String, UInt64) -> Void)?
    @ViewBuilder let profile: () -> Profile

    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var comparisonDate = Calendar.current.date(byAdding: .day, value: -7, to: .now) ?? .now

    private static var tabs: [SwitchItem<PersonPageTab>] {
        PersonPageTab.allCases.map { SwitchItem(id: $0, title: $0.rawValue) }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(spacing: Space.s) {
                SegmentedSwitch(items: Self.tabs, selection: tab, select: show)
                    .accessibilityElement(children: .contain)
                    .accessibilityLabel("Person view")
                    .help("Switch views with Control–Option–1 through Control–Option–6")
                Spacer(minLength: 0)
                let starred = model.work.isFavorite(.person, personId)
                Button {
                    model.work.setFavorite(.person, personId, !starred)
                } label: { Image(systemName: starred ? "star.fill" : "star") }
                    .buttonStyle(.icon)
                    .accessibilityLabel(starred ? "Remove from favorites" : "Add to favorites")
                    .help(starred ? "Remove from favorites" : "Add to favorites")
            }
            VStack(alignment: .leading, spacing: 16) {
                switch tab {
                case .profile:
                    PersonEmails(personId: personId)
                    profile()
                case .work: workContent
                default: memoryContent
                }
            }
            .id(tab)
            .transition(.asymmetric(insertion: .opacity, removal: .identity))
        }
        .background {
            Group {
                Button("History") { show(.history) }.keyboardShortcut("1", modifiers: [.control, .option])
                Button("Before we talk") { show(.catchUp) }.keyboardShortcut("2", modifiers: [.control, .option])
                Button("Work") { show(.work) }.keyboardShortcut("3", modifiers: [.control, .option])
                Button("What changed") { show(.changes) }.keyboardShortcut("4", modifiers: [.control, .option])
                Button("Subjects") { show(.subjects) }.keyboardShortcut("5", modifiers: [.control, .option])
                Button("Profile") { show(.profile) }.keyboardShortcut("6", modifiers: [.control, .option])
            }
            .hidden()
            .accessibilityHidden(true)
        }
    }

    private func show(_ next: PersonPageTab) {
        guard next != tab else { return }
        withAnimation(reduceMotion ? nil : .easeOut(duration: 0.14)) { tab = next }
    }

    @ViewBuilder
    private var memoryContent: some View {
        if let memory, let error = memory.error {
            Card {
                CardRow {
                    Text(error).bodyText(14, Theme.inkSecondary)
                } trailing: {
                    Button("Retry") { memory.refresh() }.buttonStyle(.compact)
                }
            }
        } else if let memory, let snapshot = memory.snapshot, snapshot.personId == personId {
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
            PersonRowsPlaceholder()
        }
    }

    @ViewBuilder
    private func content(_ snapshot: PersonMemorySnapshot, memory: PersonMemoryStore) -> some View {
        switch tab {
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
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 6) {
                ForEach(PersonHistoryFilter.allCases) { filter in
                    PersonFilterChip(
                        title: filter.rawValue,
                        count: count(filter, in: snapshot, memory: memory),
                        on: memory.filter == filter
                    ) { memory.filter = filter }
                }
                Spacer(minLength: 8)
                Button { memory.refresh() } label: { Image(systemName: "arrow.clockwise") }
                    .buttonStyle(.quiet)
                    .help("Refresh history")
                    .accessibilityLabel("Refresh history")
                    .disabled(memory.loading)
            }
            if memory.filteredTimeline.isEmpty {
                Card { CardLine(emptyHistory(memory.filter)) }
            } else {
                Card {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(memory.filteredTimeline.prefix(memory.visibleCount)) { entry in
                            PersonHistoryRow(entry: entry, open: open)
                        }
                        if memory.visibleCount < memory.filteredTimeline.count {
                            Button("Show earlier") { memory.showMore() }
                                .buttonStyle(.quiet)
                                .padding(16)
                        }
                    }
                }
            }
            if memory.filter == .contributions && snapshot.contributions.isEmpty && !snapshot.conversations.isEmpty {
                Text("No voice is attributed to this person in these conversations.")
                    .metaText(Theme.inkSecondary)
            }
        }
    }

    private func count(_ filter: PersonHistoryFilter, in snapshot: PersonMemorySnapshot, memory: PersonMemoryStore) -> Int {
        switch filter {
        case .all: memory.timeline.count
        case .conversations: snapshot.conversations.count
        case .contributions: snapshot.contributions.count
        case .notes: snapshot.notes.count
        case .commitments: snapshot.commitments.count
        }
    }

    private func catchUp(_ snapshot: PersonMemorySnapshot) -> some View {
        let catchUp = snapshot.catchUp
        let stillOpen = openLoops.filter(\.status.outstanding)
        let yours = stillOpen.filter { $0.direction == .mine }
        let theirs = stillOpen.filter { $0.direction != .mine }
        return VStack(alignment: .leading, spacing: 0) {
            if stillOpen.isEmpty {
                PageSection("Still open") {
                    Card { CardLine("Nothing is still open.") }
                }
            }
            if !yours.isEmpty {
                PageSection("You owe") {
                    Card {
                        ForEach(yours) { loop in
                            PersonLoopRow(loop: loop, openMeeting: openMeeting)
                        }
                    }
                }
            }
            if !theirs.isEmpty {
                PageSection("They owe you") {
                    Card {
                        ForEach(theirs) { loop in
                            PersonLoopRow(loop: loop, openMeeting: openMeeting)
                        }
                    }
                }
            }
            if !plansWithoutRecord.isEmpty {
                PageSection("Planned, not yet") {
                    Card {
                        ForEach(plansWithoutRecord) { plan in
                            PersonPlanRow(plan: plan, openMeeting: openMeeting)
                        }
                    }
                }
            }
            PageSection("Last conversation") {
                Card {
                    if let last = catchUp.lastConversation {
                        CardRow { PersonMemorySourceButton(source: last, open: self.open) }
                    } else {
                        CardLine("No confirmed conversation yet.")
                    }
                }
            }
            if !catchUp.contributionIds.isEmpty {
                PageSection("Their words") {
                    Card {
                        ForEach(snapshot.contributions.filter { catchUp.contributionIds.contains($0.id) }) { row in
                            CardRow { PersonMemoryContributionRow(row: row, open: self.open) }
                        }
                    }
                }
            }
            let commitments = snapshot.commitments.filter { catchUp.commitmentIds.contains($0.id) }
            PageSection("Recent open commitments") {
                Card {
                    if commitments.isEmpty {
                        CardLine("No open commitments with retained evidence.")
                    } else {
                        ForEach(commitments) { row in
                            CardRow { PersonMemoryCommitmentRow(row: row, open: self.open) }
                        }
                    }
                }
            }
            let notes = snapshot.notes.filter { catchUp.noteIds.contains($0.id) }
            if !notes.isEmpty {
                PageSection("Related notes") {
                    Card {
                        ForEach(notes) { row in
                            CardRow { PersonMemoryNoteRow(row: row, open: self.open) }
                        }
                    }
                }
            }
            let changes = snapshot.changes.filter { catchUp.changeIds.contains($0.id) }
            if !changes.isEmpty {
                PageSection("Source differences") {
                    ForEach(changes) { row in PersonMemoryChangeRow(row: row, open: self.open) }
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
                Card { CardLine("No retained source differences for this period.") }
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
                Card { CardLine("No linked subjects yet.") }
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
        if let work, let error = work.error {
            Card {
                CardRow {
                    Text(error).bodyText(14, Theme.inkSecondary)
                } trailing: {
                    Button("Retry") { work.refresh() }.buttonStyle(.compact)
                }
            }
        } else if let work, let snapshot = work.snapshot, snapshot.personId == personId {
            VStack(alignment: .leading, spacing: 0) {
                switch snapshot.identityStatus {
                case .unavailable:
                    Card { CardLine("This person is no longer available.") }
                case .merged:
                    Card {
                        CardRow {
                            Text("This person was merged.").bodyText(14, Theme.inkSecondary)
                        } trailing: {
                            if let id = snapshot.canonicalPersonId {
                                Button("Open current person") { model.openPerson(id) }.buttonStyle(.compact)
                            }
                        }
                    }
                case .available:
                    PersonTasks(personId: personId, owned: work.tasks, owed: work.owedTasks)
                    PersonProjects(personId: personId)
                    linkedWorkConversations(snapshot.conversations)
                }
            }
        } else {
            PersonRowsPlaceholder(rows: 2)
        }
    }

    private func linkedWorkConversations(_ rows: [PersonWorkConversation]) -> some View {
        PageSection("Confirmed conversation context") {
            Card {
                if rows.isEmpty {
                    CardLine("No confirmed conversations linked to projects.")
                } else {
                    ForEach(rows) { row in
                        CardRow {
                            VStack(alignment: .leading, spacing: 6) {
                                Button { openWorkConversation(row) } label: {
                                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                                        Text(row.meetingTitle).font(TypeScale.body(14)).foregroundStyle(Theme.accent)
                                            .lineLimit(1)
                                        let moment = row.meetingStartedAtMs.map {
                                            PeopleFormat.moment(PeopleFormat.date($0))
                                        }
                                        let detail = [moment, row.timestamp].compactMap { $0 }.joined(separator: " · ")
                                        if !detail.isEmpty {
                                            Text(detail).metaText().monospacedDigit().lineLimit(1).layoutPriority(1)
                                        }
                                    }
                                }
                                .buttonStyle(.quiet)
                                .accessibilityLabel("Open conversation \(row.meetingTitle)\(row.timestamp.map { " at \($0)" } ?? "")")
                                HStack(spacing: 6) {
                                    if row.targetKind != .project {
                                        Text(row.targetTitle).metaText(Theme.inkSecondary).lineLimit(1)
                                        Text("·").metaText()
                                    }
                                    Button { model.openProject(row.project.id) } label: {
                                        Text("\(row.project.title) · \(row.project.status.label)")
                                            .font(TypeScale.body(12))
                                            .lineLimit(1)
                                    }
                                    .buttonStyle(QuietButton(color: Theme.accent))
                                    .accessibilityLabel("Open project \(row.project.title)")
                                }
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

/// The emails calendar invites match this person by. Each one edits in
/// place and saves when it is left; an emptied one is taken off.
private struct PersonEmails: View {
    let personId: String
    @Environment(AppModel.self) private var model
    @State private var adding = ""

    var body: some View {
        let store = model.people
        let saved = store.detail.flatMap { $0.person.id == personId ? $0.person.calendarEmails : nil } ?? []
        PageSection("Emails") {
            Card {
                ForEach(Array(saved.enumerated()), id: \.element) { index, email in
                    CardRow {
                        EmailField(prompt: "Email", saved: email) { edited in
                            var next = saved
                            if edited.isEmpty { next.remove(at: index) } else { next[index] = edited }
                            store.setEmails(next)
                        }
                    } trailing: {
                        Button {
                            store.setEmails(saved.filter { $0 != email })
                        } label: { Image(systemName: "minus.circle") }
                            .buttonStyle(.icon)
                            .disabled(store.busy)
                            .accessibilityLabel("Remove \(email)")
                            .help("Remove this email")
                    }
                }
                CardRow {
                    EmailField(prompt: "Add an email", saved: "") { added in
                        guard !added.isEmpty else { return }
                        store.setEmails(saved + [added])
                    }
                } trailing: { EmptyView() }
            }
        }
        .disabled(store.busy)
    }
}

private struct EmailField: View {
    let prompt: String
    let saved: String
    let commit: (String) -> Void
    @State private var draft = ""
    @FocusState private var focused: Bool

    var body: some View {
        TextField(prompt, text: $draft)
            .textFieldStyle(.plain)
            .font(TypeScale.body(14))
            .focused($focused)
            .accessibilityLabel(saved.isEmpty ? prompt : "Email \(saved)")
            .onAppear { draft = saved }
            .onChange(of: saved) { _, saved in draft = saved }
            .onSubmit { focused = false }
            .onExitCommand {
                draft = saved
                focused = false
            }
            .onChange(of: focused) { _, focused in
                guard !focused else { return }
                let value = draft.trimmingCharacters(in: .whitespacesAndNewlines)
                guard value != saved else { return }
                commit(value)
                if saved.isEmpty { draft = "" }
            }
    }
}

/// The projects this person leads or is a member of, lead first.
private struct PersonProjects: View {
    let personId: String
    @Environment(AppModel.self) private var model

    var body: some View {
        let rows = (model.projects.projects ?? [])
            .map(\.project)
            .filter { $0.trashedAtMs == nil }
            .compactMap { project -> (project: PlanningProject, lead: Bool)? in
                let lead = project.leadPersonId == personId
                return lead || (project.memberPersonIds ?? []).contains(personId) ? (project, lead) : nil
            }
            .sorted { ($0.lead ? 0 : 1, $0.project.title.lowercased()) < ($1.lead ? 0 : 1, $1.project.title.lowercased()) }
        PageSection("Projects") {
            Card {
                if rows.isEmpty {
                    CardLine(model.projects.projects == nil ? "Reading projects…" : "Not on any project.")
                } else {
                    ForEach(rows, id: \.project.id) { row in
                        CardRow(action: { model.openProject(row.project.id) }) {
                            HStack(alignment: .firstTextBaseline, spacing: 6) {
                                Text(row.project.title).font(TypeScale.body(14)).foregroundStyle(Theme.ink).lineLimit(1)
                                Text(row.lead ? "Lead" : "Member").metaText(Theme.inkSecondary)
                            }
                        } trailing: {
                            Text(row.project.archivedAtMs == nil ? row.project.status.label : "Archived").metaText()
                        }
                        .accessibilityLabel("Open project \(row.project.title), \(row.lead ? "lead" : "member")")
                    }
                }
            }
        }
        .task { if model.projects.projects == nil { await model.projects.loadProjects() } }
    }
}

/// Everything a person owns, triage and work in no project included, then
/// the work owed to them, in the rows and groups of the task lists. The task
/// keys work here as on Tasks: J and K move, Return opens the task's page
/// and Space peeks at it.
private struct PersonTasks: View {
    let personId: String
    let owned: [WorkItemRow]
    let owed: [WorkItemRow]
    @Environment(AppModel.self) private var model
    @State private var collapsed: Set<String> = []

    private static let ownedShows = Set(TaskProperty.standard).subtracting([TaskProperty.owner.rawValue])
    private static let owedShows = Set(TaskProperty.standard)

    var body: some View {
        let focus = model.workFocus
        let now = TaskDates.now()
        let ownedGroups = groups(owned, now: now)
        let owedGroups = groups(owed, now: now)
        let visible = (shown(ownedGroups, in: "owned") + shown(owedGroups, in: "owed")).map(\.id)
        ScrollViewReader { proxy in
            VStack(alignment: .leading, spacing: 0) {
                section("Tasks", key: "owned", groups: ownedGroups, shows: Self.ownedShows, now: now)
                if !owedGroups.isEmpty {
                    section("Tasks for them", key: "owed", groups: owedGroups, shows: Self.owedShows, now: now)
                }
            }
            .onChange(of: focus.scrollTarget) { _, id in
                guard let id, visible.contains(id) else { return }
                proxy.scrollTo(id)
            }
        }
        .onChange(of: visible, initial: true) { _, ids in focus.show(ids) }
        .onAppear { focus.personList = personId }
        .onDisappear {
            if focus.personList == personId { focus.personList = nil }
            focus.peeking = false
            focus.renaming = nil
            focus.clearSelection()
        }
    }

    private func section(_ title: String, key: String, groups: [TaskGroup], shows: Set<String>, now: Int64) -> some View {
        let focus = model.workFocus
        let labels = model.workLabels
        return PageSection(title) {
            Card {
                if groups.isEmpty {
                    CardLine("No tasks.")
                } else {
                    ForEach(groups) { group in
                        let id = "\(key)/\(group.id)"
                        TaskGroupHeader(
                            group: group, collapsed: collapsed.contains(id),
                            revision: model.people.photoRevision, toggle: { toggle(id) })
                        if !collapsed.contains(id) {
                            ForEach(group.rows) { row in
                                ItemRow(
                                    row: row, shows: shows, labels: row.labelIds.compactMap { labels[$0] },
                                    highlighted: focus.highlighted == row.id, selected: focus.selection.contains(row.id),
                                    selecting: focus.selecting, renaming: focus.renaming == row.id, now: now,
                                    revision: model.people.photoRevision
                                ) {
                                    focus.toggle(row.id)
                                } rename: { title in
                                    model.renameTask(row.id, to: title)
                                }
                                .modifier(TaskRowInteractions(id: row.id))
                            }
                        }
                    }
                }
            }
        }
    }

    private func groups(_ rows: [WorkItemRow], now: Int64) -> [TaskGroup] {
        TaskListing.groups(live(rows, now: now), display: WorkDisplay(), now: now,
                           projects: model.work.projects, people: model.work.people)
    }

    private func shown(_ groups: [TaskGroup], in key: String) -> [WorkItemRow] {
        groups.filter { !collapsed.contains("\(key)/\($0.id)") }.flatMap(\.rows)
    }

    /// The tasks as the tracker holds them this moment, so a change made
    /// here shows at once, less the archived and snoozed ones lists hide.
    private func live(_ rows: [WorkItemRow], now: Int64) -> [WorkItemRow] {
        let live = Dictionary((model.work.items ?? []).map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        let shown = TaskListing.Filter(WorkQuery(), now: now)
        return rows.map { live[$0.id] ?? $0 }.filter(shown.matches)
    }

    private func toggle(_ group: String) {
        if collapsed.contains(group) { collapsed.remove(group) } else { collapsed.insert(group) }
    }
}

/// One way to narrow the history, with how many rows it keeps.
private struct PersonFilterChip: View {
    let title: String
    let count: Int
    let on: Bool
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 5) {
                Text(title).foregroundStyle(on ? Theme.ink : Theme.inkSecondary)
                Text("\(count)").foregroundStyle(Theme.inkTertiary).monospacedDigit()
            }
            .font(.system(size: 12, weight: .medium))
            .padding(.horizontal, 10)
            .frame(height: Theme.controlHeight)
            .background(on ? Theme.selection : hovering ? Theme.wash : .clear, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .contentShape(RoundedRectangle(cornerRadius: Theme.radiusControl))
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .hoverFade(hovering)
        .accessibilityLabel("\(title), \(count)")
        .accessibilityAddTraits(on ? .isSelected : [])
    }
}

/// A plan that named this person, its time gone with no recording of it.
struct PersonPlanRow: View {
    let plan: HeardPlan
    let openMeeting: (String) -> Void
    @Environment(AppModel.self) private var model

    var body: some View {
        CardRow(action: { openMeeting(plan.sessionId) }) {
            VStack(alignment: .leading, spacing: 4) {
                Text("“\(plan.quote ?? plan.title)”").bodyText(14).lineLimit(3)
                Text("\(plan.details) · No record of it since.").metaText()
            }
        } trailing: {
            if let segmentId = plan.citation?.segmentId {
                Button {
                    model.meetings.playContribution(sessionId: plan.sessionId, segmentId: segmentId)
                } label: {
                    Image(systemName: "play.fill")
                }
                .buttonStyle(QuietButton(color: Theme.accent, compact: true))
                .accessibilityLabel("Play where this was said")
            }
        }
    }
}

/// One line of a person's history: a mark for what kind it is, then the
/// words and where they were said.
struct PersonHistoryRow: View {
    let entry: PersonHistoryEntry
    let open: (PersonMemorySource) -> Void

    var body: some View {
        CardRow {
            HStack(alignment: .firstTextBaseline, spacing: 12) {
                Image(systemName: icon)
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(Theme.inkTertiary)
                    .frame(width: 16)
                    .accessibilityHidden(true)
                Group {
                    switch entry {
                    case let .conversation(row):
                        PersonMemoryConversationRow(row: row, open: open)
                    case let .contribution(row):
                        PersonMemoryContributionRow(row: row, open: open)
                    case let .note(row):
                        PersonMemoryNoteRow(row: row, open: open)
                    case let .commitment(row):
                        PersonMemoryCommitmentRow(row: row, open: open)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    private var icon: String {
        switch entry {
        case .conversation: "bubble.left.and.bubble.right"
        case .contribution: "quote.opening"
        case .note: "note.text"
        case .commitment: "checkmark.circle"
        }
    }
}

/// Where something was said, on one line: the meeting, then the day and the
/// moment in it. The line opens the meeting there.
struct PersonMemorySourceButton: View {
    let source: PersonMemorySource
    let open: (PersonMemorySource) -> Void

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Button { open(source) } label: {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(source.title).foregroundStyle(Theme.accent).lineLimit(1)
                    Text([PeopleFormat.moment(source.at), source.timestamp].compactMap { $0 }.joined(separator: " · "))
                        .foregroundStyle(Theme.inkTertiary)
                        .monospacedDigit()
                        .lineLimit(1)
                        .layoutPriority(1)
                }
                .font(TypeScale.body(12))
            }
            .buttonStyle(.quiet)
            .accessibilityLabel("Open \(source.title)\(source.timestamp.map { " at \($0)" } ?? "")")
            if source.transcriptAvailable == false {
                Text("· Transcript unavailable").metaText().lineLimit(1)
            } else if source.audioRetained == false {
                Text("· Recording unavailable").metaText().lineLimit(1)
            }
        }
    }
}

/// A conversation with this person: the meeting, and how sure the link is.
struct PersonMemoryConversationRow: View {
    let row: PersonMemoryConversation
    let open: (PersonMemorySource) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            PersonMemorySourceButton(source: row.source, open: open)
            if row.confidence == .suggested || (row.transcriptAvailable && row.attributedContributions == 0) {
                HStack(spacing: 6) {
                    if row.confidence == .suggested { Chip("Suggested link") }
                    if row.transcriptAvailable && row.attributedContributions == 0 {
                        Text("No attributed words").metaText()
                    }
                }
            }
        }
    }
}

struct PersonMemoryPassageView: View {
    let passage: PersonMemoryPassage
    let open: (PersonMemorySource) -> Void
    /// Off where every passage around it is this person's own.
    var showsSpeaker = true

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if showsSpeaker {
                Text(passage.speakerLabel).font(TypeScale.body(12)).foregroundStyle(Theme.inkSecondary)
            }
            Text(passage.correctedText ?? passage.rawText).bodyText(14).textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
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
        VStack(alignment: .leading, spacing: 8) {
            if row.attribution == .recognized {
                Chip("Recognized voice")
            }
            PersonMemoryPassageView(passage: row.passage, open: open, showsSpeaker: false)
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
        VStack(alignment: .leading, spacing: 6) {
            if row.includesAiSynthesis {
                Chip("Includes AI synthesis")
            }
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
    }
}

struct PersonMemoryCommitmentRow: View {
    let row: PersonMemoryCommitment
    let open: (PersonMemorySource) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(row.interpretation).bodyText(14).textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
            Text("\(status) · \(relation) · AI interpretation").metaText().lineLimit(1)
            DisclosureGroup("Evidence") {
                ForEach(row.evidence, id: \.source.id) { passage in
                    PersonMemoryPassageView(passage: passage, open: open).padding(.vertical, 6)
                }
            }
            .font(TypeScale.body(12))
            .foregroundStyle(Theme.inkSecondary)
        }
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

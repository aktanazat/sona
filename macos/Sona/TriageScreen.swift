import SwiftUI

/// Triage: what Sona heard promised in meetings, waiting for a yes or no
/// before it becomes a task.
struct TriageScreen: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let focus = model.workFocus
        ZStack(alignment: .bottomLeading) {
            if let id = focus.opened(in: .triage) {
                TaskDetailView(id: id, place: .triage)
            } else {
                TriageList()
            }
            UndoToast()
        }
        .modifier(WorkSheets())
        .onDisappear {
            focus.peeking = false
            focus.renaming = nil
            focus.clearSelection()
            model.voiceTriage.stop()
        }
    }
}

/// Triage by voice: Sona listens for a decision on the highlighted task,
/// applies it, moves on and listens again, until told to stop, Triage is
/// clear, or nothing is said for a while. Each decision is one change, so
/// "undo" or ⌘Z takes it back whole.
@MainActor @Observable
final class VoiceTriage {
    static let heardEvent = "work-triage-heard"

    enum Phase: Equatable { case off, listening, paused }

    private(set) var phase = Phase.off
    /// What the last words did, or why they did nothing.
    private(set) var last: String?

    private unowned let model: AppModel

    init(model: AppModel) {
        self.model = model
    }

    func start() {
        last = nil
        guard settle() != nil else {
            model.workFocus.announce("Triage is clear.", undoes: false)
            return
        }
        listen()
    }

    func resume() {
        last = nil
        guard settle() != nil else {
            stop()
            return
        }
        listen()
    }

    func stop() {
        guard phase != .off else { return }
        let listening = phase == .listening
        phase = .off
        last = nil
        let model = model
        Task {
            try? await model.core.request("work_triage_listen", ListenParams(listen: false))
            if listening, model.capture != .idle {
                try? await model.core.request("cancel_operation")
            }
        }
    }

    /// A recording error while listening: the reason shows, and Resume
    /// tries again.
    func interrupted(_ reason: String) {
        guard phase == .listening else { return }
        pause(reason)
    }

    func heard(_ line: Data) {
        guard phase == .listening, let event = try? Core.payload(line) as WorkTriageHeardEvent else { return }
        let said = event.said.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !said.isEmpty else {
            pause("Nothing was said for a while.")
            return
        }
        guard let decision = event.decision else {
            last = "Didn't catch “\(said)”."
            listen()
            return
        }
        guard let id = settle() else {
            stop()
            return
        }
        apply(decision, to: id)
        guard phase == .listening else { return }
        if settle() == nil {
            stop()
        } else {
            listen()
        }
    }

    private func listen() {
        phase = .listening
        let model = model
        Task {
            do {
                try await model.core.request("work_triage_listen", ListenParams(listen: true))
            } catch {
                self.interrupted(error.localizedDescription)
            }
        }
    }

    private func pause(_ reason: String) {
        phase = .paused
        last = reason
        let model = model
        Task { try? await model.core.request("work_triage_listen", ListenParams(listen: false)) }
    }

    /// The triage tasks in the list, in its order.
    private var waiting: [String] {
        model.workRows(model.workFocus.visible).filter { $0.status == .triage }.map(\.id)
    }

    /// The task the next words are about: the highlighted one when it is
    /// still waiting, else the first that is.
    @discardableResult
    private func settle() -> String? {
        let focus = model.workFocus
        let waiting = self.waiting
        let current = focus.highlighted.flatMap { waiting.contains($0) ? $0 : nil } ?? waiting.first
        focus.highlighted = current
        return current
    }

    private func apply(_ decision: TriageSaid, to id: String) {
        let focus = model.workFocus
        let work = model.work
        switch decision {
        case .next:
            focus.highlighted = focus.successor(of: [id]) ?? id
            last = "Next"
        case .back:
            let waiting = self.waiting
            if let index = waiting.firstIndex(of: id), index > 0 { focus.highlighted = waiting[index - 1] }
            last = "Back"
        case .undo:
            let title = work.undoTitle
            model.undoWork()
            last = title.map { "Undid: \($0)" } ?? "Nothing to undo."
        case .stop:
            stop()
        case let .decide(verdict, patch, summary, next):
            var original: String?
            if case let .duplicate(number) = verdict {
                original = work.items?.first { $0.number == number && $0.id != id }?.id
                guard original != nil else {
                    last = "There's no task #\(number)."
                    return
                }
            }
            let after = next ? focus.successor(of: [id]) : id
            let mark = work.undoMark
            model.changeWork {
                switch verdict {
                case .accept: work.accept([id], applySuggestions: true)
                case let .decline(reason): work.decline([id], reason: reason)
                case .duplicate: if let original { work.markDuplicate(id, of: original) }
                case nil: break
                }
                if patch != WorkPatch() { work.update([id], patch) }
                work.joinUndo(since: mark, as: summary)
            }
            focus.highlighted = after
            last = summary
        }
    }

    private struct ListenParams: Encodable {
        let listen: Bool
    }
}

/// The strip over Triage while Sona listens for decisions: what it is
/// doing, what the last words did, and Stop.
private struct VoiceTriageBar: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let voice = model.voiceTriage
        let listening = voice.phase == .listening
        HStack(spacing: 10) {
            Image(systemName: listening ? "waveform" : "pause.circle")
                .foregroundStyle(listening ? Theme.accent : Theme.inkSecondary)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(listening ? "Listening. Say accept, decline, or next." : "Paused")
                    .bodyText(13)
                    .lineLimit(1)
                if let last = voice.last {
                    Text(last).metaText().lineLimit(1)
                }
            }
            .accessibilityElement(children: .combine)
            Spacer(minLength: 0)
            if !listening {
                Button("Resume") { voice.resume() }
                    .buttonStyle(QuietButton(compact: true))
            }
            Button("Stop") { voice.stop() }
                .buttonStyle(QuietButton(compact: true))
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
    }
}

private struct TriageList: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let focus = model.workFocus
        let store = model.work
        let now = store.now
        let waiting = Self.waiting(store.items ?? [], now: now)
        let split = focus.triageSplit
        let found = Self.searched(waiting, focus.inboxSearch, projects: store.projects)
        let groups = Self.groups(found.filter { row in split.map { $0 == row.source } ?? true },
                                 byMeeting: split.map { $0 == .meeting } ?? true)
        VStack(spacing: 0) {
            WorkBar {
                InboxTabs()
            } trailing: {
                let voice = model.voiceTriage
                Button {
                    if voice.phase == .off { voice.start() } else { voice.stop() }
                } label: {
                    Image(systemName: "mic")
                }
                .buttonStyle(IconButton(on: voice.phase != .off))
                .help(voice.phase == .off ? "Triage by voice" : "Stop triage by voice")
                .accessibilityLabel(voice.phase == .off ? "Triage by voice" : "Stop triage by voice")
                Button {
                    focus.inboxSearching.toggle()
                    if !focus.inboxSearching { focus.inboxSearch = "" }
                } label: {
                    Image(systemName: "magnifyingglass")
                }
                .buttonStyle(IconButton(on: focus.inboxSearching))
                .keyboardShortcut("f", modifiers: .command)
                .help("Search  ⌘F")
                .accessibilityLabel("Search triage")
                let targets = model.workTargets
                if !targets.isEmpty {
                    TriageBarActions(ids: targets)
                }
            }
            if focus.inboxSearching {
                InboxSearchField(prompt: "Search triage")
            }
            if model.voiceTriage.phase != .off {
                VoiceTriageBar()
            }
            TriageSplits(rows: waiting)
            if let error = store.listError, store.items != nil {
                HStack(spacing: 10) {
                    Text(error).metaText(Theme.live).lineLimit(1)
                    Button("Retry") { Task { await store.refresh() } }
                        .buttonStyle(QuietButton(compact: true))
                    Spacer(minLength: 0)
                }
                .padding(.horizontal, 16)
                .frame(height: 32)
            }
            content(groups, split: focus.triageSplit, now: now)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .overlay(alignment: .trailing) {
            if focus.peeking, let id = focus.highlighted, let row = model.workRows([id]).first {
                TaskPeek(row: row, place: .triage)
                    .transition(reduceMotion ? .opacity : .move(edge: .trailing).combined(with: .opacity))
            }
        }
        .overlay(alignment: .bottom) {
            if focus.selecting {
                BulkActionBar(place: .triage)
                    .padding(.bottom, 20)
                    .transition(reduceMotion ? .opacity : .move(edge: .bottom).combined(with: .opacity))
            }
        }
        .animation(Motion.regular, value: focus.peeking)
        .animation(Motion.regular, value: focus.selecting)
    }

    @ViewBuilder
    private func content(_ groups: [TaskGroup], split: WorkSource?, now: Int64) -> some View {
        let store = model.work
        let focus = model.workFocus
        let proposed = model.triageProposed
        let documents = split == nil ? store.documentProposals : []
        if store.items == nil {
            if let error = store.listError {
                TaskFailure(message: error) { Task { await store.refresh() } }
            } else {
                TaskSkeleton(rows: 6, twoLine: true)
            }
        } else if groups.isEmpty, proposed.isEmpty, documents.isEmpty {
            TaskEmpty(line: !focus.inboxSearch.trimmingCharacters(in: .whitespaces).isEmpty ? "Nothing matches." : split.map { "Nothing \($0.split.lowercased()) to triage." } ?? "Triage is clear.") {
                TaskKeyButton(title: "Tasks", keys: "g t") { model.go(.tasks) }
            }
            .onAppear { focus.show([]) }
        } else {
            TaskListBody(groups: groups, headers: split.map { $0 == .meeting } ?? true, lead: {
                if !documents.isEmpty { DocumentProposals(proposals: documents) }
                if !proposed.isEmpty { TriageProposals(rows: proposed) }
            }) { item in
                TriageRow(
                    row: item, suggestions: store.suggestions[item.id] ?? [],
                    applied: store.autoApplied[item.id] ?? [],
                    highlighted: focus.highlighted == item.id, selected: focus.selection.contains(item.id),
                    selecting: focus.selecting, now: now
                ) {
                    focus.toggle(item.id)
                }
            }
        }
    }

    static func waiting(_ items: [WorkItemRow], now: Int64) -> [WorkItemRow] {
        let shown = TaskListing.Filter(WorkQuery(), now: now)
        return items.filter { $0.status == .triage && shown.matches($0) }
    }

    /// Matches the inbox search: title, project, priority and number.
    static func searched(_ rows: [WorkItemRow], _ text: String, projects: [WorkProjectChoice]) -> [WorkItemRow] {
        let query = text.trimmingCharacters(in: .whitespaces).lowercased()
        guard !query.isEmpty else { return rows }
        let titles = Dictionary(projects.map { ($0.id, $0.title) }, uniquingKeysWith: { first, _ in first })
        return rows.filter { row in
            let texts = [row.title, row.projectId.flatMap { titles[$0] }, TaskPriority(row.priority).label, "#\(row.number)"]
            return texts.contains { $0?.lowercased().contains(query) == true }
        }
    }

    /// One group per meeting, the latest meeting first; inside a meeting,
    /// the order things were said. A split from anywhere else is one list.
    static func groups(_ waiting: [WorkItemRow], byMeeting: Bool) -> [TaskGroup] {
        let meetings = Dictionary(grouping: waiting) { byMeeting ? $0.sourceMeetingId ?? "" : "" }
        return meetings.map { meeting, rows in
            let at = { (row: WorkItemRow) in meeting.isEmpty ? row.createdAtMs : row.sourceAtMs ?? row.createdAtMs }
            let ordered = rows.sorted { (at($0), $0.number) < (at($1), $1.number) }
            let title = meeting.isEmpty ? "Not from a meeting" : rows.first?.sourceMeetingTitle ?? "A meeting"
            return TaskGroup(id: "triage:\(meeting)", title: title, mark: .meeting, rows: ordered)
        }
        .sorted { left, right in
            let (a, b) = (left.rows.map(\.createdAtMs).max() ?? 0, right.rows.map(\.createdAtMs).max() ?? 0)
            return a != b ? a > b : left.id < right.id
        }
    }
}

/// Triage, then each place its suggestions come from that has any; ⇥ and
/// ⇧⇥ step through them. Nothing to choose between, no row.
private struct TriageSplits: View {
    let rows: [WorkItemRow]
    @Environment(AppModel.self) private var model

    var body: some View {
        let focus = model.workFocus
        let counts = Dictionary(grouping: rows, by: \.source).mapValues(\.count)
        let sources = WorkSource.allCases.filter { counts[$0] != nil || focus.triageSplit == $0 }
        if sources.count > 1 || focus.triageSplit != nil {
            ScrollView(.horizontal) {
                HStack(spacing: 2) {
                    WorkTab(title: "All", count: rows.count, on: focus.triageSplit == nil) { focus.split(nil) }
                    ForEach(sources, id: \.self) { source in
                        WorkTab(title: source.tab, count: counts[source], on: focus.triageSplit == source) {
                            focus.split(source)
                        }
                    }
                }
                .padding(.horizontal, 16)
            }
            .scrollIndicators(.never)
            .scrollBounceBehavior(.basedOnSize)
            .frame(height: 40)
        }
    }
}

extension WorkSource {
    var split: String {
        switch self {
        case .meeting: "From meetings"
        case .dictation: "From dictation"
        case .agent: "From agents"
        case .person: "From people"
        }
    }

    /// The split's tab: short enough that every tab fits beside the bar's
    /// actions in the narrowest window.
    var tab: String {
        switch self {
        case .meeting: "Meetings"
        case .dictation: "Dictation"
        case .agent: "Agents"
        case .person: "People"
        }
    }
}

/// Accept, decline, mark as a duplicate, or snooze: the keys 1, 2, 3 and H,
/// for the highlighted task or every selected one.
struct TriageButtons: View {
    let ids: [String]
    @Environment(AppModel.self) private var model

    var body: some View {
        let rows = model.workRows(ids)
        HStack(spacing: 6) {
            ForEach([WorkAction.accept, .decline, .duplicate, .snooze].filter { $0.applies(to: rows) }) { action in
                TaskKeyButton(title: action.title, keys: action.keys) {
                    model.perform(action, on: ids)
                }
            }
        }
    }
}

/// The bar's verbs: the first that applies as the one button, the rest in
/// a menu. The menu leaves its keys off for the reason `WorkActionMenu`
/// gives; 2, 3 and H still work from the list.
private struct TriageBarActions: View {
    let ids: [String]
    @Environment(AppModel.self) private var model

    var body: some View {
        let rows = model.workRows(ids)
        let actions = [WorkAction.accept, .decline, .duplicate, .snooze].filter { $0.applies(to: rows) }
        if let primary = actions.first {
            Button(primary.title) {
                model.perform(primary, on: ids)
            }
            .buttonStyle(.primary)
            .fixedSize()
            .help("\(primary.title)  \(Shortcut(primary.keys).keys.joined(separator: " "))")
        }
        if actions.count > 1 {
            Menu {
                ForEach(actions.dropFirst()) { action in
                    Button {
                        model.perform(action, on: ids)
                    } label: {
                        Label(action.title, systemImage: action.icon)
                    }
                }
            } label: {
                Image(systemName: "ellipsis")
            }
            .menuStyle(.button)
            .buttonStyle(.icon)
            .menuIndicator(.hidden)
            .fixedSize()
            .help(actions.dropFirst().map { "\($0.title)  \(Shortcut($0.keys).keys.joined(separator: " "))" }.joined(separator: ", "))
            .accessibilityLabel("More triage actions")
        }
    }
}

/// Edits automations proposed to notes and projects, each with what it
/// would add or replace, and Accept and Decline.
private struct DocumentProposals: View {
    let proposals: [WorkDocProposal]

    var body: some View {
        Section {
            ForEach(proposals) { proposal in
                DocumentProposalRow(proposal: proposal)
            }
        } header: {
            HStack(spacing: 10) {
                Color.clear
                    .frame(width: 22)
                    .padding(.trailing, -4)
                Image(systemName: "doc.text")
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(Theme.inkTertiary)
                    .frame(width: 16, height: 16)
                HStack(spacing: 6) {
                    Text("Proposed edits")
                        .font(TypeScale.listTitle)
                        .foregroundStyle(Theme.ink)
                    Text("\(proposals.count)")
                        .font(TypeScale.label(12))
                        .monospacedDigit()
                        .foregroundStyle(Theme.inkTertiary)
                }
                Spacer(minLength: 0)
            }
            .padding(.leading, 14)
            .padding(.trailing, 24)
            .frame(height: Theme.listRow)
            .background(Theme.inset)
            .accessibilityElement(children: .combine)
            .accessibilityAddTraits(.isHeader)
        }
    }
}

private struct DocumentProposalRow: View {
    let proposal: WorkDocProposal
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 10) {
                Color.clear.frame(width: 22)
                Image(systemName: proposal.kind == .note ? "note.text" : "folder")
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(Theme.inkTertiary)
                    .frame(width: 14)
                Text(proposal.targetTitle)
                    .font(TypeScale.listTitle)
                    .foregroundStyle(Theme.ink)
                    .lineLimit(1)
                Text(kind).metaText()
                AgentName(name: proposal.agent)
                if let health = proposal.health {
                    Text(health.label).metaText()
                }
                Spacer(minLength: Space.s)
                Button("Decline") {
                    Task { await model.work.decideDocProposal(proposal, accept: false) }
                }
                .buttonStyle(.quiet)
                Button("Accept") {
                    Task { await model.work.decideDocProposal(proposal, accept: true) }
                }
                .buttonStyle(QuietButton(compact: true))
            }
            .frame(height: Theme.listRow)
            Text(proposal.body)
                .bodyText(12, Theme.inkSecondary)
                .lineLimit(4)
                .textSelection(.enabled)
                .help(proposal.body)
                .padding(.leading, 22 + 10 + 14 + 10)
        }
        .padding(.leading, 6)
        .padding(.trailing, 16)
        .padding(.bottom, 8)
        .padding(.horizontal, 8)
    }

    private var kind: String {
        switch proposal.kind {
        case .note: "Addition"
        case .projectDescription: "Description"
        case .projectUpdate: "Project update"
        case .unknown: "Edit"
        }
    }
}

/// A suggestion waiting in triage: two lines, the title over what was said
/// and by whom, with Sona's guesses for its owner, date and project.
private struct TriageRow: View {
    let row: WorkItemRow
    let suggestions: [WorkFieldSuggestion]
    let applied: [WorkAutoApplied]
    let highlighted: Bool
    let selected: Bool
    let selecting: Bool
    let now: Int64
    let toggle: () -> Void

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            TaskCheckbox(on: selected, toggle: toggle)
                .opacity(selecting || selected || highlighted ? 1 : 0)
            TaskStatusIcon(status: row.status)
                .frame(height: 18)
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 8) {
                    Text(row.title)
                        .font(TypeScale.listTitle)
                        .foregroundStyle(Theme.ink)
                        .lineLimit(1)
                        .help(row.title)
                    Spacer(minLength: 8)
                    ForEach(Self.onePerField(suggestions), id: \.id) { suggestion in
                        SuggestionChip(itemId: row.id, suggestion: suggestion, now: now)
                    }
                    Text(TaskDates.ago(row.createdAtMs, now: now))
                        .metaText()
                        .frame(minWidth: 32, alignment: .trailing)
                }
                .frame(height: 18)
                if let quote = row.sourceQuote {
                    HStack(spacing: 6) {
                        Text("“\(quote)”")
                            .font(TypeScale.body(12))
                            .foregroundStyle(Theme.inkSecondary)
                            .lineLimit(1)
                        if let speaker = row.sourceSpeaker {
                            Text(speaker).metaText().lineLimit(1).fixedSize()
                        }
                    }
                }
                ForEach(applied) { change in
                    AutoAppliedLine(change: change)
                }
            }
        }
        .padding(.leading, 6)
        .padding(.trailing, 16)
        .padding(.vertical, 9)
        .frame(minHeight: 56, alignment: .top)
        .taskHighlight(highlighted, selected: selected)
        .padding(.horizontal, 8)
        .accessibilityElement(children: .combine)
    }

    /// Sona's first guess for each field; the rest wait in the task.
    static func onePerField(_ suggestions: [WorkFieldSuggestion]) -> [WorkFieldSuggestion] {
        suggestions.reduce(into: []) { kept, suggestion in
            if !kept.contains(where: { $0.field == suggestion.field }) { kept.append(suggestion) }
        }
    }
}

/// A field Sona filled by itself, why, and Undo.
private struct AutoAppliedLine: View {
    let change: WorkAutoApplied
    @Environment(AppModel.self) private var model

    var body: some View {
        HStack(spacing: 6) {
            Text(line)
                .metaText()
                .lineLimit(1)
                .help(change.reason)
            Button("Undo") {
                Task { await model.work.undoAutoApplied(change) }
            }
            .buttonStyle(.quiet)
        }
    }

    private var line: String {
        let work = model.work
        switch change.field {
        case .project:
            return "Sona set the project to \(work.projects.first { $0.id == change.value }?.title ?? "a project")"
        case .labels:
            return "Sona added the label \(work.labels.first { $0.id == change.value }?.name ?? "a label")"
        case .owner:
            if change.value == "me" { return "Sona made you the owner" }
            return "Sona set the owner to \(work.people.first { $0.id == change.value }?.name ?? "someone")"
        }
    }
}

/// One of Sona's guesses: what it is, on hover why it thinks so, and on a
/// click the reason with Use and Dismiss.
struct SuggestionChip: View {
    let itemId: String
    let suggestion: WorkFieldSuggestion
    let now: Int64
    @Environment(AppModel.self) private var model
    @State private var open = false

    var body: some View {
        let value = WorkSuggestionText.value(suggestion, model: model, now: now)
        Button { open = true } label: {
            HStack(spacing: 4) {
                Image(systemName: WorkSuggestionText.icon(suggestion.field))
                    .font(.system(size: 10, weight: .medium))
                    .foregroundStyle(Theme.accent)
                Text(WorkSuggestionText.chip(suggestion, model: model, now: now)).lineLimit(1)
            }
            .font(TypeScale.label(11))
            .foregroundStyle(Theme.ink)
            .padding(.horizontal, 6)
            .frame(height: 20)
            .background(Theme.accentSoft, in: RoundedRectangle(cornerRadius: Theme.radiusKey))
        }
        .buttonStyle(.plain)
        .help(WorkSuggestionText.why(suggestion))
        .popover(isPresented: $open, arrowEdge: .bottom) {
            SuggestionCard(itemId: itemId, suggestion: suggestion, now: now)
        }
        .accessibilityLabel("Suggested \(WorkSuggestionText.title(suggestion.field).lowercased()): \(value). \(WorkSuggestionText.why(suggestion))")
    }
}

/// How a suggestion reads, wherever it shows.
@MainActor
enum WorkSuggestionText {
    static func value(_ suggestion: WorkFieldSuggestion, model: AppModel, now: Int64) -> String {
        let work = model.work
        return switch suggestion.value {
        case .owner(.myself): "Me"
        case .owner(.nobody): "No owner"
        case let .owner(.person(id)): work.people.first { $0.id == id }?.name ?? "Someone"
        case let .due(moment): TaskDates.label(moment, now: now)
        case let .project(id): work.projects.first { $0.id == id }?.title ?? "A project"
        case let .label(id): work.labels.first { $0.id == id }?.name ?? "A label"
        case let .item(id): work.items?.first { $0.id == id }?.title ?? "Another task"
        }
    }

    /// A chip's words: a task Sona thinks this one repeats or touches is
    /// said as that, its title left to the card.
    static func chip(_ suggestion: WorkFieldSuggestion, model: AppModel, now: Int64) -> String {
        switch suggestion.field {
        case .duplicate: "Possible duplicate"
        case .related: "Related task"
        case .owner, .due, .project, .label: value(suggestion, model: model, now: now)
        }
    }

    static func icon(_ field: WorkSuggestionField) -> String {
        switch field {
        case .owner: "person.crop.circle"
        case .due: "calendar"
        case .project: "folder"
        case .label: "tag"
        case .duplicate: "square.on.square"
        case .related: "link"
        }
    }

    static func title(_ field: WorkSuggestionField) -> String {
        switch field {
        case .owner: "Owner"
        case .due: "Due date"
        case .project: "Project"
        case .label: "Label"
        case .duplicate: "Duplicate of"
        case .related: "Related to"
        }
    }

    /// The reason, then the words it rests on.
    static func why(_ suggestion: WorkFieldSuggestion) -> String {
        guard let quote = suggestion.quote else { return suggestion.reason }
        return "\(suggestion.reason)\n“\(quote)”"
    }
}

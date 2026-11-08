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
        }
    }
}

private struct TriageList: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let focus = model.workFocus
        let store = model.work
        let now = TaskDates.now()
        let groups = Self.groups(store.items ?? [], now: now)
        let count = groups.reduce(0) { $0 + $1.rows.count }
        VStack(spacing: 0) {
            WorkBar {
                HStack(spacing: 8) {
                    Text("Triage")
                        .font(TypeScale.listTitle)
                        .foregroundStyle(Theme.ink)
                    if count > 0 {
                        Text("\(count)").font(TypeScale.label(12)).monospacedDigit().foregroundStyle(Theme.inkTertiary)
                    }
                }
                .accessibilityElement(children: .combine)
            } trailing: {
                let targets = model.workTargets
                if !targets.isEmpty {
                    TriageButtons(ids: targets)
                }
            }
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
            content(groups, now: now)
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
    private func content(_ groups: [TaskGroup], now: Int64) -> some View {
        let store = model.work
        let focus = model.workFocus
        if store.items == nil {
            if let error = store.listError {
                TaskFailure(message: error) { Task { await store.refresh() } }
            } else {
                TaskSkeleton(rows: 6, twoLine: true)
            }
        } else if groups.isEmpty {
            TaskEmpty(line: "Triage is clear.") {
                TaskKeyButton(title: "Tasks", keys: "g t") { model.go(.tasks) }
            }
        } else {
            TaskListBody(groups: groups) { item in
                TriageRow(
                    row: item, suggestions: store.suggestions[item.id] ?? [],
                    highlighted: focus.highlighted == item.id, selected: focus.selection.contains(item.id),
                    selecting: focus.selecting, now: now
                ) {
                    focus.toggle(item.id)
                }
            }
        }
    }

    /// One group per meeting, the latest meeting first; inside a meeting,
    /// the order things were said.
    static func groups(_ items: [WorkItemRow], now: Int64) -> [TaskGroup] {
        let waiting = items.filter { $0.status == .triage && TaskListing.matches($0, WorkQuery(), now: now) }
        let meetings = Dictionary(grouping: waiting) { $0.sourceMeetingId ?? "" }
        return meetings.map { meeting, rows in
            let ordered = rows.sorted { ($0.sourceAtMs ?? $0.createdAtMs, $0.number) < ($1.sourceAtMs ?? $1.createdAtMs, $1.number) }
            let title = meeting.isEmpty ? "Not from a meeting" : rows.first?.sourceMeetingTitle ?? "A meeting"
            return TaskGroup(id: "triage:\(meeting)", title: title, mark: .meeting, rows: ordered)
        }
        .sorted { ($0.rows.map(\.createdAtMs).max() ?? 0) > ($1.rows.map(\.createdAtMs).max() ?? 0) }
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

/// A suggestion waiting in triage: two lines, the title over what was said
/// and by whom, with Sona's guesses for its owner, date and project.
private struct TriageRow: View {
    let row: WorkItemRow
    let suggestions: [WorkFieldSuggestion]
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
                    Spacer(minLength: 8)
                    ForEach(Array(suggestions.enumerated()), id: \.offset) { _, suggestion in
                        SuggestionChip(suggestion: suggestion, now: now)
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
}

/// One of Sona's guesses: what it is, and on hover why it thinks so and
/// the words it heard.
struct SuggestionChip: View {
    let suggestion: WorkFieldSuggestion
    let now: Int64
    @Environment(AppModel.self) private var model

    var body: some View {
        let value = WorkSuggestionText.value(suggestion, model: model, now: now)
        HStack(spacing: 4) {
            Image(systemName: WorkSuggestionText.icon(suggestion.field))
                .font(.system(size: 10, weight: .medium))
                .foregroundStyle(Theme.accent)
            Text(value).lineLimit(1)
        }
        .font(TypeScale.label(11))
        .foregroundStyle(Theme.ink)
        .padding(.horizontal, 6)
        .frame(height: 20)
        .background(Theme.accentSoft, in: RoundedRectangle(cornerRadius: Theme.radiusKey))
        .help(WorkSuggestionText.why(suggestion))
        .accessibilityElement(children: .ignore)
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
        case let .item(id): work.items?.first { $0.id == id }.map { "#\($0.number)" } ?? "Another task"
        }
    }

    static func icon(_ field: WorkSuggestionField) -> String {
        switch field {
        case .owner: "person.crop.circle"
        case .due: "calendar"
        case .project: "folder"
        case .duplicate: "square.on.square"
        }
    }

    static func title(_ field: WorkSuggestionField) -> String {
        switch field {
        case .owner: "Owner"
        case .due: "Due date"
        case .project: "Project"
        case .duplicate: "Duplicate of"
        }
    }

    /// The reason, then the words it rests on.
    static func why(_ suggestion: WorkFieldSuggestion) -> String {
        guard let quote = suggestion.quote else { return suggestion.reason }
        return "\(suggestion.reason)\n“\(quote)”"
    }
}

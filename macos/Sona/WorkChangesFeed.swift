import SwiftUI

struct RecentChangesParams: Encodable, Sendable {
    var limit: Int
    var latest: Bool
}

/// `work_changes`: one page of the change stream, oldest first.
struct WorkChangePage: Decodable, Sendable {
    var changes: [WorkChangeEntry]
    var nextCursor: Int64
    var hasMore: Bool
}

/// One change: what it was on, what happened, and under `updatedFrom` the
/// changed fields' earlier values. Keys inside `data` and `updatedFrom` stay
/// as the core wrote them, snake-cased.
struct WorkChangeEntry: Decodable, Identifiable, Sendable {
    var seq: Int64
    var atMs: Int64
    var entity: String
    var entityId: String
    var itemId: String?
    var action: String
    var updatedFrom: [String: JSONValue]?
    var data: [String: JSONValue]?

    var id: Int64 { seq }

    private func text(_ key: String) -> String? {
        if case let .string(value)? = data?[key] { return value }
        return nil
    }

    var name: String? { text("title") ?? text("name") }

    var kind: String {
        switch entity {
        case "task": "Task"
        case "project": "Project"
        case "milestone": "Milestone"
        case "cycle": "Cycle"
        case "view": "View"
        case "label": "Label"
        case "project_update": "Project update"
        case "note": "Note"
        case "meeting": "Meeting"
        case "session": "Agent session"
        case "comment":
            if let subject = text("subject_kind") { "Comment on a \(subject)" } else { "Comment on a task" }
        default: entity.capitalized
        }
    }

    var verb: String {
        switch (entity, action) {
        case ("meeting", "create"): "work taken in"
        case ("meeting", _): "work taken in again"
        case (_, "create"): "added"
        case (_, "remove"): "deleted"
        default: "changed"
        }
    }

    var icon: String {
        switch entity {
        case "task": "checklist"
        case "project", "project_update": "square.stack.3d.up"
        case "milestone": "flag"
        case "initiative": "scope"
        case "cycle": "arrow.triangle.2.circlepath"
        case "view": "line.3.horizontal.decrease"
        case "label": "tag"
        case "note": "note.text"
        case "meeting": "waveform"
        case "comment": "text.bubble"
        default: "circle"
        }
    }

    /// The changed fields in words, in a stable order.
    var fields: String {
        (updatedFrom ?? [:]).keys.sorted().map(Self.word).joined(separator: ", ")
    }

    private static func word(_ key: String) -> String {
        switch key {
        case "due_at_ms": "due date"
        case "target_at_ms": "target date"
        case "archived_at_ms": "archived"
        case "trashed_at_ms", "deleted_at_ms": "deleted"
        case "snoozed_until_ms": "snooze"
        case "retired_at_ms": "retired"
        case "resolved_at_ms": "resolved"
        case "project_id": "project"
        case "milestone_id": "milestone"
        case "parent_id": "parent"
        case "for_person_id": "for"
        case "start_day": "start"
        case "end_day": "end"
        case "duplicate_of": "duplicate"
        default: key.replacingOccurrences(of: "_", with: " ")
        }
    }
}

/// The Tasks bar's clock: the recent changes feed in a popover.
struct WorkChangesButton: View {
    @Environment(AppModel.self) private var model
    @State private var shown = false

    var body: some View {
        WorkBarButton(icon: "clock.arrow.circlepath", help: "Recent changes", on: shown) {
            shown.toggle()
        }
        .popover(isPresented: $shown, arrowEdge: .bottom) {
            WorkChangesFeed().environment(model)
        }
    }
}

/// The newest changes across everything Sona tracks, newest at the top.
/// A change on a task opens it.
struct WorkChangesFeed: View {
    @Environment(AppModel.self) private var model
    @State private var changes: [WorkChangeEntry]?
    @State private var failure: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text("Recent changes").sectionLabel()
                Spacer()
                WorkBarButton(icon: "arrow.clockwise", help: "Refresh") { Task { await load() } }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            Divider()
            content
        }
        .frame(width: 400, height: 460)
        .task { await load() }
    }

    @ViewBuilder
    private var content: some View {
        if let failure {
            TaskEmpty(line: failure) {
                Button("Try again") { Task { await load() } }.buttonStyle(.compact)
            }
        } else if let changes, changes.isEmpty {
            TaskEmpty(line: "Nothing has changed in the last 30 days.") {}
        } else if let changes {
            let now = Int64(Date().timeIntervalSince1970 * 1000)
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(changes) { change in
                        WorkChangeRow(change: change, now: now) {
                            if change.entity != "meeting", let task = change.itemId {
                                model.openTask(task)
                            }
                        }
                    }
                }
                .padding(.vertical, 4)
            }
        } else {
            ProgressView().controlSize(.small).frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private func load() async {
        do {
            changes = try await model.work.recentChanges().changes.reversed()
            failure = nil
        } catch {
            failure = error.localizedDescription
        }
    }
}

private struct WorkChangeRow: View {
    let change: WorkChangeEntry
    let now: Int64
    let open: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: open) {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Image(systemName: change.icon)
                    .font(.system(size: 11))
                    .foregroundStyle(Theme.inkTertiary)
                    .frame(width: 16)
                VStack(alignment: .leading, spacing: 2) {
                    Text(change.name ?? change.kind)
                        .bodyText(13)
                        .lineLimit(1)
                    Text(summary).metaText()
                        .lineLimit(1)
                }
                Spacer(minLength: 8)
                Text(ProjectDates.relative(change.atMs, now: now)).metaText()
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 6)
            .contentShape(Rectangle())
            .background(hovering && change.itemId != nil ? Theme.ink.opacity(0.05) : .clear)
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
    }

    private var summary: String {
        let what = change.name == nil ? change.verb.capitalizedFirst : "\(change.kind) \(change.verb)"
        let fields = change.fields
        return fields.isEmpty ? what : "\(what): \(fields)"
    }
}

private extension String {
    var capitalizedFirst: String { prefix(1).uppercased() + dropFirst() }
}

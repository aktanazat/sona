import SwiftUI

/// What a comment thread off the task list is on.
struct PlanningCommentSubject: Hashable {
    enum Kind: String, Encodable { case project, milestone, meeting, note }

    let kind: Kind
    let id: String
}

/// The comment threads on the projects, milestones, meetings and notes the
/// app shows, by subject. A meeting kept on this Mac comes back without
/// what agents wrote on it, and says so.
@MainActor
@Observable
final class CommentThreads {
    private(set) var threads: [PlanningCommentSubject: [WorkComment]] = [:]
    private(set) var keptLocal: Set<PlanningCommentSubject> = []
    private(set) var failures: [PlanningCommentSubject: String] = [:]
    private(set) var mutating = false

    @ObservationIgnored private let core: Core

    init(core: Core) {
        self.core = core
    }

    /// Reads again every subject of `kinds` whose threads were read.
    func reload(_ kinds: Set<PlanningCommentSubject.Kind>) async {
        for subject in threads.keys where kinds.contains(subject.kind) { await load(subject) }
    }

    func load(_ subject: PlanningCommentSubject) async {
        var read: [WorkComment] = []
        var cursor: String?
        var kept = false
        do {
            repeat {
                let page: CommentPage = try await core.request("work_comments", CommentsParams(
                    itemId: subject.id, subject: subject.kind, limit: 100, cursor: cursor))
                read += page.threads
                kept = kept || page.keptLocal
                cursor = page.nextCursor
            } while cursor != nil
            threads[subject] = read
            if kept { keptLocal.insert(subject) } else { keptLocal.remove(subject) }
            failures[subject] = nil
        } catch {
            failures[subject] = error.localizedDescription
        }
    }

    func add(on subject: PlanningCommentSubject, body: String, replyTo: String? = nil) async throws {
        try await write(subject) {
            let _: WorkComment = try await self.core.request("work_comment_add", CommentAddParams(
                operationId: UUID().uuidString, itemId: subject.id, subject: subject.kind, body: body, replyTo: replyTo))
        }
    }

    func edit(_ comment: WorkComment, on subject: PlanningCommentSubject, body: String) async throws {
        try await write(subject) {
            let _: WorkComment = try await self.core.request("work_comment_edit", CommentParams(
                operationId: UUID().uuidString, id: comment.id, body: body))
        }
    }

    func delete(_ comment: WorkComment, on subject: PlanningCommentSubject) async throws {
        try await write(subject) {
            let _: WorkComment = try await self.core.request("work_comment_delete", CommentParams(
                operationId: UUID().uuidString, id: comment.id))
        }
    }

    func resolve(_ thread: WorkComment, on subject: PlanningCommentSubject, resolved: Bool) async throws {
        try await write(subject) {
            let _: WorkComment = try await self.core.request("work_comment_resolve", CommentParams(
                operationId: UUID().uuidString, id: thread.id, resolved: resolved))
        }
    }

    private func write(_ subject: PlanningCommentSubject, _ change: @MainActor () async throws -> Void) async throws {
        guard !mutating else { throw CoreError.remote(.string("Wait for the current change to finish.")) }
        mutating = true
        defer { mutating = false }
        try await change()
        await load(subject)
    }
}

private struct CommentsParams: Encodable {
    let itemId: String
    let subject: PlanningCommentSubject.Kind
    let limit: Int
    let cursor: String?
}

private struct CommentAddParams: Encodable {
    let operationId: String
    let itemId: String
    let subject: PlanningCommentSubject.Kind
    let body: String
    let replyTo: String?
}

private struct CommentParams: Encodable {
    let operationId: String
    let id: String
    var body: String?
    var resolved: Bool?
}

private struct CommentPage: Decodable {
    let threads: [WorkComment]
    let nextCursor: String?
    let keptLocal: Bool
}

/// The comment threads on a project, a milestone, a meeting or a note,
/// oldest first, with a box to add one.
struct PlanningCommentsView: View {
    let threads: CommentThreads
    let subject: PlanningCommentSubject

    var body: some View {
        let shown = threads.threads[subject]
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text("Comments").sectionLabel().accessibilityAddTraits(.isHeader)
                if let shown, !shown.isEmpty { Text("\(shown.count)").metaText().monospacedDigit() }
            }
            if threads.keptLocal.contains(subject) {
                Text("Kept on this Mac. Agents can\u{2019}t read or comment on this meeting.").metaText()
            }
            if let shown {
                ForEach(shown) { thread in
                    PlanningCommentThread(threads: threads, subject: subject, thread: thread)
                }
            } else if let message = threads.failures[subject] {
                PlannerFailure(message: message) { Task { await threads.load(subject) } }
            } else {
                PlannerLoadingRows(label: "Loading comments")
            }
            PlanningCommentComposer(threads: threads, subject: subject, prompt: "Leave a comment")
        }
        .task(id: subject) { await threads.load(subject) }
    }
}

/// A milestone's comment threads, in a sheet from its row.
struct MilestoneCommentsSheet: View {
    let milestone: PlanningMilestone
    let threads: CommentThreads
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                Text(milestone.title).titleText().lineLimit(1).accessibilityAddTraits(.isHeader)
                Spacer()
                Button("Done") { dismiss() }
                    .buttonStyle(.quiet)
                    .keyboardShortcut(.cancelAction)
            }
            ScrollView {
                PlanningCommentsView(threads: threads, subject: PlanningCommentSubject(kind: .milestone, id: milestone.id))
                    .padding(.trailing, 4)
            }
        }
        .padding(24)
        .frame(width: 520, height: 560)
        .background(Theme.page)
    }
}

private struct PlanningCommentThread: View {
    let threads: CommentThreads
    let subject: PlanningCommentSubject
    let thread: WorkComment
    @State private var replying = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            PlanningCommentEntry(threads: threads, subject: subject, comment: thread, thread: thread) { replying = true }
            ForEach(thread.replies) { reply in
                PlanningCommentEntry(threads: threads, subject: subject, comment: reply, thread: thread) { replying = true }
                    .padding(.leading, 24)
            }
            if replying {
                PlanningCommentComposer(threads: threads, subject: subject, replyTo: thread.id, prompt: "Reply",
                                        autofocus: true) { replying = false }
                    .padding(.leading, 24)
                    .padding(.top, 6)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .overlay(RoundedRectangle(cornerRadius: Theme.radiusCard).strokeBorder(Theme.hairline))
        .opacity(thread.resolvedAtMs == nil ? 1 : 0.7)
    }
}

private struct PlanningCommentEntry: View {
    let threads: CommentThreads
    let subject: PlanningCommentSubject
    let comment: WorkComment
    let thread: WorkComment
    let reply: () -> Void
    @Environment(AppModel.self) private var model
    @State private var editing = false
    @State private var hovering = false
    @State private var failure: String?

    var body: some View {
        let root = comment.id == thread.id
        let resolved = thread.resolvedAtMs != nil
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Image(systemName: icon)
                    .font(.system(size: 11))
                    .foregroundStyle(Theme.inkTertiary)
                    .frame(width: 16)
                Text(comment.author.display(me: model.myName)).font(TypeScale.label(13)).foregroundStyle(Theme.ink)
                Text(TaskDates.ago(comment.createdAtMs, now: TaskDates.now()))
                    .metaText()
                    .help(PlanningFormat.date(comment.createdAtMs))
                if comment.editedAtMs != nil { Text("edited").metaText() }
                if root && resolved { Text("resolved").metaText() }
                Spacer(minLength: 8)
                if !comment.deleted && !editing {
                    HStack(spacing: 2) {
                        if !resolved { action("Reply", "arrowshape.turn.up.left", reply) }
                        if root {
                            action(resolved ? "Reopen" : "Resolve", resolved ? "arrow.uturn.backward" : "checkmark") {
                                run { try await threads.resolve(thread, on: subject, resolved: !resolved) }
                            }
                        }
                        if comment.author.kind == .me { action("Edit", "pencil") { editing = true } }
                        action("Delete", "trash") { run { try await threads.delete(comment, on: subject) } }
                    }
                    .opacity(hovering ? 1 : 0)
                }
            }
            .frame(height: 22)
            Group {
                if comment.deleted {
                    Text("Deleted").bodyText(13, Theme.inkTertiary).italic()
                } else if editing {
                    PlanningCommentComposer(threads: threads, subject: subject, editing: comment, prompt: "Edit comment",
                                            autofocus: true) { editing = false }
                } else if let body = comment.body {
                    MarkdownBody(source: body, toggle: nil).textSelection(.enabled)
                }
                if let failure { Text(failure).metaText(Theme.live).lineLimit(2) }
            }
            .padding(.leading, 24)
        }
        .padding(.vertical, 6)
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .accessibilityElement(children: .contain)
    }

    private var icon: String {
        switch comment.author.kind {
        case .me: "person.crop.circle"
        case .sona: "sparkles"
        case .agent: "cpu"
        }
    }

    private func action(_ title: String, _ icon: String, _ run: @escaping () -> Void) -> some View {
        Button(action: run) {
            Image(systemName: icon)
                .font(.system(size: 11))
                .frame(width: 24, height: 22)
                .contentShape(Rectangle())
        }
        .buttonStyle(QuietButton(compact: true))
        .help(title)
        .accessibilityLabel(title)
    }

    private func run(_ change: @escaping @MainActor () async throws -> Void) {
        Task {
            do {
                try await change()
                failure = nil
            } catch {
                failure = error.localizedDescription
            }
        }
    }
}

private struct PlanningCommentComposer: View {
    let threads: CommentThreads
    let subject: PlanningCommentSubject
    var replyTo: String?
    var editing: WorkComment?
    let prompt: String
    var autofocus = false
    var done: () -> Void = {}
    @State private var text = ""
    @State private var failure: String?
    @FocusState private var focused: Bool

    var body: some View {
        let empty = text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        let nested = editing != nil || replyTo != nil
        VStack(alignment: .trailing, spacing: 8) {
            ZStack(alignment: .topLeading) {
                Text(text.isEmpty ? " " : text + "\n")
                    .font(TypeScale.body(13))
                    .lineSpacing(2)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .hidden()
                if text.isEmpty {
                    Text(prompt).font(TypeScale.body(13)).foregroundStyle(Theme.inkDisabled)
                        .allowsHitTesting(false)
                }
                TextEditor(text: $text)
                    .font(TypeScale.body(13))
                    .lineSpacing(2)
                    .foregroundStyle(Theme.ink)
                    .textEditorStyle(.plain)
                    .scrollContentBackground(.hidden)
                    .scrollDisabled(true)
                    .padding(.horizontal, -5)
                    .focused($focused)
                    .onKeyPress(.return, phases: .down) { press in
                        guard press.modifiers.contains(.command) else { return .ignored }
                        post()
                        return .handled
                    }
                    .onExitCommand {
                        focused = false
                        if nested { done() }
                    }
                    .accessibilityLabel(prompt)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 9)
            .background(Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .overlay(
                RoundedRectangle(cornerRadius: Theme.radiusControl)
                    .strokeBorder(focused ? Theme.accent.opacity(0.6) : Theme.hairline, lineWidth: 1)
            )
            .contentShape(Rectangle())
            .onTapGesture { focused = true }
            if focused || !empty || nested {
                HStack(spacing: 12) {
                    if let failure { Text(failure).metaText(Theme.live).lineLimit(2) }
                    Spacer(minLength: 0)
                    Text("⌘↩").metaText()
                    if nested {
                        Button("Cancel") { done() }.buttonStyle(QuietButton(compact: true))
                    }
                    Button(editing != nil ? "Save" : replyTo != nil ? "Reply" : "Comment") { post() }
                        .buttonStyle(PrimaryButton(compact: true))
                        .disabled(empty || threads.mutating)
                }
            }
        }
        .onAppear {
            if let editing { text = editing.body ?? "" }
            if autofocus { focused = true }
        }
    }

    private func post() {
        let body = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !body.isEmpty, !threads.mutating else { return }
        Task {
            do {
                if let editing {
                    try await threads.edit(editing, on: subject, body: body)
                } else {
                    try await threads.add(on: subject, body: body, replyTo: replyTo)
                }
                failure = nil
                text = ""
                done()
            } catch {
                failure = error.localizedDescription
            }
        }
    }
}

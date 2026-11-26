import SwiftUI

/// The Board's way into Today: what needs you on the Mac, counted, one tap away.
struct TodayEntry: View {
    @ObservedObject var model: AppModel

    var body: some View {
        if let today = model.board?.workToday {
            Button { model.showsToday = true } label: {
                HStack(spacing: 12) {
                    VStack(alignment: .leading, spacing: 4) {
                        Label("today.title", systemImage: "sun.max")
                            .font(.body.weight(.semibold))
                            .foregroundStyle(Theme.textPrimary)
                        Text(TodayCounts(today: today).line)
                            .font(.subheadline)
                            .foregroundStyle(Theme.textSecondary)
                    }
                    Spacer()
                    Image(systemName: "chevron.right")
                        .font(.footnote.weight(.semibold))
                        .foregroundStyle(Theme.textTertiary)
                        .accessibilityHidden(true)
                }
                .padding(.vertical, 4)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("today-entry")
        }
    }
}

/// "2 questions · 3 to triage · 4 due", or that nothing waits.
struct TodayCounts {
    let today: WorkToday

    var line: String {
        let parts = [
            (today.questions.count, "today.count.questions"),
            (today.triage.count, "today.count.triage"),
            (today.due.count, "today.count.due"),
            (today.inbox.count, "today.count.inbox"),
        ].filter { $0.0 > 0 }.map { String(format: NSLocalizedString($0.1, comment: ""), $0.0) }
        return parts.isEmpty ? NSLocalizedString("today.clear", comment: "") : parts.joined(separator: " · ")
    }
}

/// The Mac's Today on the phone: agents waiting on an answer, tasks waiting in Triage,
/// my work due, and unread inbox rows. Every decision goes to the Mac that listed it.
struct TodayScreen: View {
    @ObservedObject var model: AppModel
    @ObservedObject var today: PhoneToday
    @State private var answering: WorkToday.Question?

    var body: some View {
        let snapshot = model.board?.workToday
        List {
            if let snapshot {
                if !today.refused.isEmpty {
                    Section {
                        ForEach(Array(today.refused.enumerated()), id: \.offset) { _, title in
                            Text("today.refused \(title)")
                                .font(.subheadline)
                                .foregroundStyle(Theme.textSecondary)
                        }
                        Button("today.dismiss") { today.refused = [] }
                    }
                }
                if !snapshot.questions.isEmpty {
                    Section("today.questions") {
                        ForEach(snapshot.questions) { question in
                            QuestionRow(model: model, today: today, question: question) { answering = question }
                        }
                    }
                }
                if !snapshot.triage.isEmpty {
                    Section("today.triage") {
                        ForEach(snapshot.triage) { item in
                            TriageRow(model: model, today: today, item: item)
                        }
                    }
                }
                if !snapshot.due.isEmpty {
                    Section("today.due") {
                        ForEach(snapshot.due) { item in
                            DueRow(model: model, today: today, item: item)
                        }
                    }
                }
                if !snapshot.inbox.isEmpty {
                    Section("today.inbox") {
                        ForEach(snapshot.inbox) { notice in
                            NoticeRow(model: model, today: today, notice: notice)
                        }
                    }
                }
                if snapshot.questions.isEmpty, snapshot.triage.isEmpty, snapshot.due.isEmpty, snapshot.inbox.isEmpty {
                    Text("today.clear")
                        .foregroundStyle(Theme.textSecondary)
                        .frame(maxWidth: .infinity, minHeight: 160)
                        .listRowBackground(Color.clear)
                }
            } else {
                Text(model.isPaired ? "today.none" : "board.notPaired")
                    .foregroundStyle(Theme.textSecondary)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: .infinity, minHeight: 160)
                    .listRowBackground(Color.clear)
            }
        }
        .listStyle(.insetGrouped)
        .readableColumn()
        .scrollContentBackground(.hidden)
        .background(Theme.background)
        .navigationTitle("today.title")
        .navigationBarTitleDisplayMode(.inline)
        .refreshable { await model.refreshBoard() }
        .safeAreaInset(edge: .bottom) {
            if let snapshot {
                Text("today.updated \(snapshot.producedAt.formatted(.relative(presentation: .named)))")
                    .font(.footnote)
                    .foregroundStyle(Theme.textSecondary)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 10)
                    .glassEffect(.regular, in: Capsule())
                    .padding(.bottom, 8)
            }
        }
        .task { await model.refreshBoard() }
        /* While a decision is on its way, look again every few seconds: the Mac answers
         * in its next snapshot, and the row stays marked until then. */
        .task(id: today.pending.map(\.objectId)) {
            while !today.pending.isEmpty, !Task.isCancelled {
                try? await Task.sleep(for: .seconds(5))
                guard !Task.isCancelled else { return }
                await model.refreshBoard()
            }
        }
        .sheet(item: $answering) { question in
            AnswerSheet(model: model, question: question)
        }
        .accessibilityIdentifier("today")
    }
}

/// What a row shows instead of its buttons while its decision is on its way.
private struct Sending: View {
    @ObservedObject var today: PhoneToday
    let decision: PhoneToday.Pending

    var body: some View {
        Text(today.isLate(decision) ? "today.waiting" : "today.sent")
            .font(.footnote)
            .foregroundStyle(Theme.textTertiary)
    }
}

private struct TaskTitle: View {
    let number: Int64
    let title: String

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Text(verbatim: "#\(number)")
                .font(.footnote.monospacedDigit())
                .foregroundStyle(Theme.textTertiary)
            Text(verbatim: title)
                .font(.body)
                .foregroundStyle(Theme.textPrimary)
                .lineLimit(3)
        }
    }
}

private struct QuestionRow: View {
    @ObservedObject var model: AppModel
    @ObservedObject var today: PhoneToday
    let question: WorkToday.Question
    let reply: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            TaskTitle(number: question.task_number, title: question.task_title)
            Text("today.asks \(question.agent) \(question.question)")
                .font(.subheadline)
                .foregroundStyle(Theme.textSecondary)
            if let decision = today.pending(for: question.session_id) {
                Sending(today: today, decision: decision)
            } else {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        ForEach(question.options, id: \.self) { option in
                            Button(option.label) {
                                Task {
                                    await model.decide(
                                        .answer, row: question.session_id, title: question.task_title,
                                        sessionId: question.session_id, value: option.value
                                    )
                                }
                            }
                            .buttonStyle(.bordered)
                            .buttonBorderShape(.capsule)
                        }
                        Button("today.reply", action: reply)
                            .buttonStyle(.bordered)
                            .buttonBorderShape(.capsule)
                    }
                }
            }
        }
        .padding(.vertical, 4)
    }
}

private struct TriageRow: View {
    @ObservedObject var model: AppModel
    @ObservedObject var today: PhoneToday
    let item: WorkToday.Item

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            TaskTitle(number: item.number, title: item.title)
            if let decision = today.pending(for: item.id) {
                Sending(today: today, decision: decision)
            } else {
                HStack(spacing: 8) {
                    Button("today.accept") { decide(.accept) }
                        .buttonStyle(.borderedProminent)
                    Button("today.decline") { decide(.decline) }
                        .buttonStyle(.bordered)
                }
                .buttonBorderShape(.capsule)
                .controlSize(.small)
            }
        }
        .padding(.vertical, 4)
    }

    private func decide(_ action: WorkActionManifest.Action) {
        Task { await model.decide(action, row: item.id, title: item.title, taskId: item.id) }
    }
}

private struct DueRow: View {
    @ObservedObject var model: AppModel
    @ObservedObject var today: PhoneToday
    let item: WorkToday.Item

    var body: some View {
        let decision = today.pending(for: item.id)
        HStack(alignment: .top, spacing: 12) {
            Button {
                Task {
                    await model.decide(
                        .done, row: item.id, title: item.title, taskId: item.id, expectedRevision: item.revision
                    )
                }
            } label: {
                Image(systemName: decision == nil ? "circle" : "checkmark.circle.fill")
                    .font(.title3)
                    .foregroundStyle(decision == nil ? Theme.textTertiary : Theme.accent)
            }
            .buttonStyle(.plain)
            .disabled(decision != nil)
            .accessibilityLabel(Text("today.markDone"))
            VStack(alignment: .leading, spacing: 4) {
                TaskTitle(number: item.number, title: item.title)
                HStack(spacing: 6) {
                    if let dueAt = item.dueAt {
                        Text(Calendar.current.isDateInToday(dueAt) || dueAt > .now ? "today.dueToday" : "today.overdue")
                            .foregroundStyle(Calendar.current.isDateInToday(dueAt) || dueAt > .now ? Theme.textSecondary : Theme.recording)
                    }
                    if let project = item.project {
                        Text(verbatim: project)
                            .foregroundStyle(Theme.textTertiary)
                    }
                }
                .font(.footnote)
                if let decision { Sending(today: today, decision: decision) }
            }
        }
        .padding(.vertical, 4)
    }
}

private struct NoticeRow: View {
    @ObservedObject var model: AppModel
    @ObservedObject var today: PhoneToday
    let notice: WorkToday.Notice

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let number = notice.task_number {
                TaskTitle(number: number, title: notice.title)
            } else {
                Text(verbatim: notice.title)
                    .font(.body)
                    .foregroundStyle(Theme.textPrimary)
            }
            Text(verbatim: notice.line)
                .font(.subheadline)
                .foregroundStyle(Theme.textSecondary)
                .lineLimit(3)
            HStack(spacing: 6) {
                if notice.needs_you {
                    Text("today.needsYou").foregroundStyle(Theme.accent)
                }
                Text(notice.at.formatted(.relative(presentation: .named)))
                    .foregroundStyle(Theme.textTertiary)
                if let decision = today.pending(for: notice.id) {
                    Sending(today: today, decision: decision)
                }
            }
            .font(.footnote)
        }
        .padding(.vertical, 4)
        .swipeActions(edge: .leading) {
            Button("today.read") { mark(.read) }
                .tint(Theme.accent)
        }
        .swipeActions(edge: .trailing) {
            Button("today.archive") { mark(.archive) }
        }
        .contextMenu {
            Button("today.read", systemImage: "envelope.open") { mark(.read) }
            Button("today.archive", systemImage: "archivebox") { mark(.archive) }
        }
    }

    private func mark(_ action: WorkActionManifest.Action) {
        Task { await model.decide(action, row: notice.id, title: notice.title, noticeIds: notice.notice_ids) }
    }
}

/// A written answer to an agent's question.
private struct AnswerSheet: View {
    @ObservedObject var model: AppModel
    let question: WorkToday.Question
    @Environment(\.dismiss) private var dismiss
    @State private var text = ""
    @FocusState private var focused: Bool

    private var trimmed: String { text.trimmingCharacters(in: .whitespacesAndNewlines) }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Text(verbatim: question.question)
                        .foregroundStyle(Theme.textSecondary)
                }
                Section {
                    TextField("today.answerPlaceholder", text: $text, axis: .vertical)
                        .lineLimit(3...8)
                        .focused($focused)
                }
            }
            .navigationTitle(Text(verbatim: question.agent))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("today.cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("today.send") {
                        let body = trimmed
                        Task {
                            await model.decide(
                                .answer, row: question.session_id, title: question.task_title,
                                sessionId: question.session_id, body: body
                            )
                        }
                        dismiss()
                    }
                    .disabled(trimmed.isEmpty || trimmed.utf8.count > WorkActionObject.maxTextBytes)
                }
            }
            .onAppear { focused = true }
        }
        .presentationDetents([.medium, .large])
    }
}

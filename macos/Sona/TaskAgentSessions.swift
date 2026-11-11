import SwiftUI

struct DetailAgentSessions: View {
    let row: WorkItemRow
    @Environment(AppModel.self) private var model

    var body: some View {
        let work = model.work
        let sessions = work.sessions[row.id] ?? []
        let open = sessions.filter(\.isOpen).map(\.id)
        VStack(alignment: .leading, spacing: 10) {
            if !sessions.isEmpty {
                DetailHeading(title: "Agent")
                if let error = work.sessionsError {
                    Text(error).metaText(Theme.live).lineLimit(2)
                }
                ForEach(sessions) { session in
                    SessionCard(session: session, newest: session.id == sessions.first?.id)
                }
            }
        }
        .padding(.top, sessions.isEmpty ? 0 : 32)
        .task(id: "\(row.id):\(open.joined(separator: ","))") {
            await work.loadSessions(row.id)
            guard !open.isEmpty else { return }
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(10))
                guard !Task.isCancelled else { return }
                await work.loadSessions(row.id)
            }
        }
    }
}

private struct SessionCard: View {
    let session: WorkSession
    let newest: Bool
    @Environment(AppModel.self) private var model
    @State private var expanded: Bool?
    @State private var stopping = false

    var body: some View {
        let shown = expanded ?? (session.isOpen || newest)
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Image(systemName: "sparkle")
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(Theme.inkSecondary)
                    .frame(width: 16, height: 16)
                Text(session.agent).font(TypeScale.label(13)).foregroundStyle(Theme.ink)
                SessionLiveness(session: session)
                Spacer(minLength: 8)
                Text(TaskDates.ago(session.lastActivityAtMs ?? session.createdAtMs, now: TaskDates.now()))
                    .metaText()
                    .help(PlanningFormat.date(session.lastActivityAtMs ?? session.createdAtMs))
                if session.isOpen && session.stopRequestedAtMs == nil {
                    Button {
                        stop()
                    } label: {
                        Label("Stop", systemImage: "stop.fill")
                    }
                    .buttonStyle(SecondaryButton(compact: true))
                    .disabled(stopping)
                    .help("Ask \(session.agent) to stop")
                }
                Button {
                    expanded = !shown
                } label: {
                    Image(systemName: shown ? "chevron.up" : "chevron.down")
                        .font(.system(size: 10))
                        .frame(width: 20, height: 20)
                        .contentShape(Rectangle())
                }
                .buttonStyle(QuietButton(compact: true))
                .help(shown ? "Hide the run" : "Show the run")
                .accessibilityLabel(shown ? "Hide the run" : "Show the run")
            }
            if shown {
                if !session.plan.isEmpty {
                    SessionPlan(plan: session.plan)
                }
                SessionTimeline(session: session)
                if session.isOpen {
                    SessionReply(session: session)
                }
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .overlay(RoundedRectangle(cornerRadius: Theme.radiusCard).strokeBorder(Theme.hairline))
        .accessibilityElement(children: .contain)
    }

    private func stop() {
        stopping = true
        let work = model.work
        let id = session.id
        Task {
            _ = await work.stopSession(id)
            stopping = false
        }
    }
}

struct SessionLiveness: View {
    let session: WorkSession

    var body: some View {
        let shown = Self.describe(session)
        HStack(spacing: 5) {
            Image(systemName: "circle.fill")
                .font(.system(size: 6))
                .symbolEffect(.pulse, options: .repeating, isActive: shown.moving)
            Text(shown.title).font(TypeScale.label(12))
        }
        .foregroundStyle(shown.tint)
        .help(shown.help)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(shown.title)
        .accessibilityHint(shown.help)
    }

    struct Shown {
        let title: String
        let help: String
        let tint: Color
        let moving: Bool
    }

    static func describe(_ session: WorkSession) -> Shown {
        let agent = session.agent
        if session.unresponsive {
            return Shown(title: "Not responding", help: "\(agent) has not answered since the run started.",
                         tint: Theme.live, moving: false)
        }
        switch session.state {
        case .stale:
            return Shown(title: "Gone quiet", help: "Nothing from \(agent) for 30 minutes.", tint: Theme.live, moving: false)
        case .awaitingInput:
            return Shown(title: "Waiting for your answer", help: "\(agent) asked you something.", tint: Theme.live, moving: false)
        case .complete:
            return Shown(title: "Finished", help: "\(agent) finished this run.", tint: Theme.inkTertiary, moving: false)
        case .error:
            return Shown(title: "Ended with an error", help: "\(agent) stopped on an error.", tint: Theme.live, moving: false)
        case .unknown:
            return Shown(title: "Unknown", help: "This version of Sona does not know this state.", tint: Theme.inkTertiary, moving: false)
        case .pending, .active:
            break
        }
        if session.stopRequestedAtMs != nil {
            return Shown(title: "Stopping", help: "\(agent) was asked to stop and has not finished yet.",
                         tint: Theme.inkSecondary, moving: true)
        }
        return session.state == .pending
            ? Shown(title: "Starting", help: "\(agent) has not started yet.", tint: Theme.inkSecondary, moving: true)
            : Shown(title: "Working", help: "\(agent) is working on it now.", tint: Theme.accent, moving: true)
    }
}

private struct SessionPlan: View {
    let plan: [WorkPlanStep]

    var body: some View {
        let done = plan.filter { $0.status == .completed }.count
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                Text("Plan").font(.system(size: 12, weight: .semibold)).foregroundStyle(Theme.inkTertiary)
                Text("\(done) of \(plan.count) done").metaText()
            }
            ForEach(Array(plan.enumerated()), id: \.offset) { _, step in
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Image(systemName: Self.icon(step.status))
                        .font(.system(size: 12))
                        .foregroundStyle(Self.tint(step.status))
                        .frame(width: 16)
                    Text(step.content)
                        .bodyText(13, step.status == .inProgress ? Theme.ink : Theme.inkSecondary)
                        .strikethrough(step.status == .canceled)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel("\(step.content), \(Self.spoken(step.status))")
            }
        }
    }

    private static func icon(_ status: WorkPlanStatus) -> String {
        switch status {
        case .completed: "checkmark.circle.fill"
        case .inProgress: "circle.lefthalf.filled"
        case .canceled: "xmark.circle"
        case .pending: "circle"
        case .unknown: "circle.dashed"
        }
    }

    private static func tint(_ status: WorkPlanStatus) -> Color {
        switch status {
        case .completed: Theme.accent
        case .inProgress: Theme.statusInProgress
        case .pending, .canceled, .unknown: Theme.inkTertiary
        }
    }

    private static func spoken(_ status: WorkPlanStatus) -> String {
        switch status {
        case .completed: "done"
        case .inProgress: "in progress"
        case .canceled: "dropped"
        case .pending: "not started"
        case .unknown: "unknown"
        }
    }
}

private struct SessionTimeline: View {
    let session: WorkSession

    var body: some View {
        let entries = Self.visible(session.activities)
        VStack(alignment: .leading, spacing: 0) {
            ForEach(entries) { activity in
                SessionActivityLine(
                    activity: activity,
                    agent: session.agent,
                    session: session,
                    standing: session.isOpen && session.question?.id == activity.id,
                    picked: activity.value.flatMap { Self.label(of: $0, before: activity, in: session.activities) })
            }
        }
    }

    private static func visible(_ activities: [WorkSessionActivity]) -> [WorkSessionActivity] {
        activities.enumerated()
            .filter { index, activity in !activity.ephemeral || index == activities.count - 1 }
            .map(\.element)
    }

    private static func label(of value: String, before prompt: WorkSessionActivity, in activities: [WorkSessionActivity]) -> String? {
        let earlier = activities.prefix { $0.id != prompt.id }
        return earlier.last { $0.kind == .elicitation }?.options.first { $0.value == value }?.label
    }
}

private struct SessionActivityLine: View {
    let activity: WorkSessionActivity
    let agent: String
    let session: WorkSession
    let standing: Bool
    let picked: String?
    @Environment(AppModel.self) private var model
    @State private var sending: String?

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            mark
            VStack(alignment: .leading, spacing: 4) {
                content
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            if activity.ephemeral {
                ProgressView().controlSize(.mini)
            }
            Text(TaskDates.ago(activity.atMs, now: TaskDates.now()))
                .metaText()
                .help(PlanningFormat.date(activity.atMs))
        }
        .padding(.vertical, 5)
        .opacity(activity.ephemeral ? 0.75 : 1)
    }

    @ViewBuilder
    private var mark: some View {
        if activity.author.kind == .me {
            Avatar(face: AvatarFace(kind: .me, name: "Me"), size: 16, revision: model.people.photoRevision)
        } else {
            Image(systemName: icon)
                .font(.system(size: 11, weight: .medium))
                .foregroundStyle(tint)
                .frame(width: 16, height: 16)
        }
    }

    private var icon: String {
        switch activity.kind {
        case .thought: "ellipsis.bubble"
        case .action: "terminal"
        case .elicitation: "questionmark.bubble"
        case .response: "checkmark.bubble"
        case .error: "exclamationmark.triangle"
        case .prompt: "arrowshape.turn.up.left"
        case .stop: "stop.circle"
        case .unknown: "circle"
        }
    }

    private var tint: Color {
        switch activity.kind {
        case .elicitation: standing ? Theme.live : Theme.inkSecondary
        case .response: Theme.accent
        case .error: Theme.live
        default: Theme.inkSecondary
        }
    }

    @ViewBuilder
    private var content: some View {
        switch activity.kind {
        case .thought:
            Text(activity.body ?? "Thinking")
                .bodyText(13, Theme.inkSecondary)
                .italic()
                .fixedSize(horizontal: false, vertical: true)
        case .action:
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(activity.action ?? "Action").font(TypeScale.label(13)).foregroundStyle(Theme.ink)
                if let parameter = activity.parameter {
                    Text(parameter)
                        .font(TypeScale.mono(12))
                        .foregroundStyle(Theme.inkSecondary)
                        .lineLimit(3)
                        .help(parameter)
                }
            }
            if let result = activity.result {
                Text(result)
                    .font(TypeScale.mono(12))
                    .foregroundStyle(Theme.inkTertiary)
                    .lineLimit(6)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
            }
            if let body = activity.body {
                Text(body).bodyText(13, Theme.inkSecondary).fixedSize(horizontal: false, vertical: true)
            }
        case .elicitation:
            Text(activity.body ?? "\(agent) asks you something")
                .bodyText(13)
                .fixedSize(horizontal: false, vertical: true)
            if standing && !activity.options.isEmpty {
                FlowOptions(options: activity.options, sending: sending) { pick($0) }
            }
        case .response:
            MarkdownBody(source: activity.body ?? "Done", toggle: nil)
                .textSelection(.enabled)
        case .error:
            Text(activity.body ?? "\(agent) hit an error")
                .bodyText(13, Theme.live)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
        case .prompt:
            Text(promptLine).bodyText(13).fixedSize(horizontal: false, vertical: true)
        case .stop:
            Text("\(activity.author.display) asked \(agent) to stop").bodyText(13, Theme.inkSecondary)
        case .unknown:
            Text(activity.body ?? "Something this version of Sona cannot show")
                .bodyText(13, Theme.inkTertiary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var promptLine: String {
        let who = activity.author.display
        switch (activity.body, picked ?? activity.value) {
        case let (body?, choice?): return "\(who) picked “\(choice)”: \(body)"
        case let (nil, choice?): return "\(who) picked “\(choice)”"
        case let (body?, nil): return "\(who): \(body)"
        case (nil, nil): return "\(who) answered"
        }
    }

    private func pick(_ option: WorkSelectOption) {
        guard sending == nil else { return }
        sending = option.value
        let work = model.work
        let id = session.id
        Task {
            _ = await work.replySession(id, body: nil, value: option.value)
            sending = nil
        }
    }
}

private struct FlowOptions: View {
    let options: [WorkSelectOption]
    let sending: String?
    let pick: (WorkSelectOption) -> Void

    var body: some View {
        ChatFlowRow(spacing: 6) {
            ForEach(options, id: \.value) { option in
                Button(option.label) { pick(option) }
                    .buttonStyle(SecondaryButton(compact: true))
                    .disabled(sending != nil)
                    .accessibilityHint("Answers with \(option.label)")
            }
        }
        .padding(.top, 4)
    }
}

private struct SessionReply: View {
    let session: WorkSession
    @Environment(AppModel.self) private var model
    @State private var text = ""
    @State private var sending = false
    @FocusState private var focused: Bool

    var body: some View {
        let empty = text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        HStack(alignment: .bottom, spacing: 8) {
            TextField("Reply to \(session.agent)", text: $text, axis: .vertical)
                .textFieldStyle(.plain)
                .font(TypeScale.body(13))
                .lineLimit(1...6)
                .focused($focused)
                .onSubmit { send() }
                .padding(.horizontal, 10)
                .padding(.vertical, 7)
                .background(Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
                .overlay(
                    RoundedRectangle(cornerRadius: Theme.radiusControl)
                        .strokeBorder(focused ? Theme.accent.opacity(0.6) : Theme.hairline, lineWidth: 1)
                )
                .accessibilityLabel("Reply to \(session.agent)")
            Button("Send") { send() }
                .buttonStyle(PrimaryButton(compact: true))
                .disabled(empty || sending)
        }
    }

    private func send() {
        let body = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !body.isEmpty, !sending else { return }
        sending = true
        let work = model.work
        let id = session.id
        Task {
            let sent = await work.replySession(id, body: body, value: nil)
            sending = false
            if sent { text = "" }
        }
    }
}

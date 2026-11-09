import SwiftUI

/// Today: the meetings ahead, the work due, and what I promised since
/// yesterday; up to three picks become the day's focus. Then, one at a
/// time, work nobody has touched in a month.
struct MorningPlanView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let plan = model.morningPlan(now: TaskDates.now())
        let rituals = model.rituals
        ZStack(alignment: .bottomLeading) {
            VStack(spacing: 0) {
                HStack(spacing: 10) {
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text("Today")
                            .font(TypeScale.listTitle)
                            .foregroundStyle(Theme.ink)
                        Text(Date.now.formatted(.dateTime.weekday(.wide).month(.wide).day()))
                            .font(TypeScale.label(12))
                            .foregroundStyle(Theme.inkTertiary)
                    }
                    Spacer(minLength: 12)
                    trailing
                }
                .frame(maxWidth: Theme.contentMax, alignment: .leading)
                .padding(.horizontal, Theme.margin)
                .frame(maxWidth: .infinity)
                .frame(height: Theme.headerHeight)
                .overlay(alignment: .bottom) { Hairline() }
                if let error = rituals.error {
                    ErrorNote(error)
                        .frame(maxWidth: Theme.contentMax, alignment: .leading)
                        .padding(.horizontal, Theme.margin)
                        .padding(.top, Space.m)
                        .frame(maxWidth: .infinity)
                }
                switch rituals.stage {
                case .pick: PlanPicks(plan: plan)
                case .stale: StaleQuestion()
                }
            }
            UndoToast()
        }
        .modifier(WorkSheets())
        .onAppear { model.beginMorning() }
    }

    @ViewBuilder
    private var trailing: some View {
        let rituals = model.rituals
        switch rituals.stage {
        case .pick:
            Text(verbatim: "\(rituals.picked.count) of \(RitualStore.focusLimit)")
                .font(TypeScale.label(12))
                .monospacedDigit()
                .foregroundStyle(Theme.inkTertiary)
                .accessibilityLabel("\(rituals.picked.count) of \(RitualStore.focusLimit) picked")
            Button("Start the day", action: model.startDay)
                .buttonStyle(.primary)
                .help("Start the day  ↩")
        case .stale:
            if let question = model.staleQuestion() {
                Text(verbatim: "\(question.number) of \(question.count)")
                    .font(TypeScale.label(12))
                    .monospacedDigit()
                    .foregroundStyle(Theme.inkTertiary)
            }
        }
    }
}

/// The first stage: what can be picked, in the order the keys walk it.
private struct PlanPicks: View {
    @Environment(AppModel.self) private var model
    let plan: MorningPlan

    var body: some View {
        let now = TaskDates.now()
        let rituals = model.rituals
        ScrollViewReader { proxy in
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    if let meetings = plan.meetings {
                        PageSection("Calendar") {
                            Card {
                                if meetings.isEmpty {
                                    CardLine("No meetings today.")
                                }
                                ForEach(meetings) { meeting in
                                    MeetingLine(meeting: meeting)
                                }
                            }
                        }
                    }
                    if !plan.focus.isEmpty {
                        PageSection("Focus") {
                            Card {
                                ForEach(plan.focus) { row in
                                    PickRow(row: row, meta: row.projectTitle.map { (text: $0, icon: "folder") },
                                            pick: rituals.picked.firstIndex(of: row.id),
                                            highlighted: rituals.cursor == row.id, now: now)
                                }
                            }
                        }
                    }
                    PageSection("Due") {
                        Card {
                            if plan.due.isEmpty {
                                CardLine("Nothing due today.")
                            }
                            ForEach(plan.due) { row in
                                PickRow(row: row, meta: row.projectTitle.map { (text: $0, icon: "folder") },
                                        pick: rituals.picked.firstIndex(of: row.id),
                                        highlighted: rituals.cursor == row.id, now: now)
                            }
                        }
                    }
                    if !plan.promised.isEmpty {
                        PageSection("Promised since yesterday") {
                            Card {
                                ForEach(plan.promised) { row in
                                    PickRow(row: row, meta: row.sourceMeetingTitle.map { (text: $0, icon: "waveform") },
                                            pick: rituals.picked.firstIndex(of: row.id),
                                            highlighted: rituals.cursor == row.id, now: now)
                                }
                            }
                        }
                    }
                }
                .frame(maxWidth: Theme.contentMax, alignment: .leading)
                .padding(.horizontal, Theme.margin)
                .padding(.top, Space.l)
                .padding(.bottom, 96)
                .frame(maxWidth: .infinity)
            }
            .onChange(of: rituals.cursor) { _, id in
                guard let id else { return }
                withAnimation(Motion.quick) { proxy.scrollTo(id) }
            }
        }
    }
}

/// A task that can be picked: its place among the picks, then the task.
private struct PickRow: View {
    @Environment(AppModel.self) private var model
    let row: WorkItemRow
    let meta: (text: String, icon: String)?
    let pick: Int?
    let highlighted: Bool
    let now: Int64

    var body: some View {
        HStack(spacing: 10) {
            PickMark(number: pick.map { $0 + 1 }, pointed: highlighted)
            TaskStatusIcon(status: row.status)
            Text(row.title)
                .font(TypeScale.listTitle)
                .foregroundStyle(Theme.ink)
                .lineLimit(1)
                .layoutPriority(1)
            Spacer(minLength: 12)
            if let meta {
                HStack(spacing: 4) {
                    Image(systemName: meta.icon).font(.system(size: 10))
                    Text(meta.text).lineLimit(1)
                }
                .font(TypeScale.label(12))
                .foregroundStyle(Theme.inkTertiary)
                .frame(maxWidth: 180, alignment: .trailing)
                .fixedSize(horizontal: true, vertical: false)
                .layoutPriority(-1)
            }
            if let due = row.dueAtMs {
                TaskDue(due: due, open: true, now: now)
                    .frame(width: 72, alignment: .trailing)
            }
        }
        .padding(.horizontal, 16)
        .frame(maxWidth: .infinity, minHeight: Theme.listRow, alignment: .leading)
        .background(highlighted ? Theme.wash : .clear)
        .hoverFade(highlighted)
        .overlay(alignment: .bottom) { Hairline().padding(.horizontal, 16) }
        .contentShape(Rectangle())
        .onTapGesture { model.togglePick(row.id) }
        .onHover { inside in
            if inside { model.rituals.cursor = row.id }
        }
        .id(row.id)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(pick != nil ? [.isButton, .isSelected] : .isButton)
        .accessibilityAction { model.togglePick(row.id) }
    }
}

/// The pick's number on the accent; unpicked, an empty box on the row the
/// keys point at and nothing elsewhere. A box, so it never reads as the
/// round status mark beside it.
private struct PickMark: View {
    let number: Int?
    let pointed: Bool

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: Theme.radiusKey)
        ZStack {
            shape
                .strokeBorder(Theme.inkTertiary, lineWidth: 1.5)
                .opacity(number == nil && pointed ? 1 : 0)
            shape
                .fill(Theme.accentFill)
                .opacity(number == nil ? 0 : 1)
            if let number {
                Text(verbatim: "\(number)")
                    .font(.system(size: 10, weight: .bold))
                    .monospacedDigit()
                    .foregroundStyle(Theme.onAccent)
            }
        }
        .frame(width: 16, height: 16)
        .animation(Motion.quick, value: number)
    }
}

/// A meeting today: when, what, and how many are in it.
private struct MeetingLine: View {
    let meeting: OverviewUpcomingRow

    var body: some View {
        let start = FeedClock.date(meeting.startUtcMs)
        HStack(spacing: 10) {
            Text(start.time)
                .font(TypeScale.label(12))
                .monospacedDigit()
                .foregroundStyle(start < .now ? Theme.inkTertiary : Theme.inkSecondary)
                .frame(width: 64, alignment: .leading)
            Text(meeting.title)
                .bodyText(14, start < .now ? Theme.inkSecondary : Theme.ink)
                .lineLimit(1)
            Spacer(minLength: 12)
            if meeting.attendeeCount > 1 {
                Text("\(meeting.attendeeCount) people").metaText()
            }
        }
        .padding(.horizontal, 16)
        .frame(maxWidth: .infinity, minHeight: Theme.listRow, alignment: .leading)
        .overlay(alignment: .bottom) { Hairline().padding(.horizontal, 16) }
        .accessibilityElement(children: .combine)
    }
}

/// The second stage: one month-old task at a time. K keeps it, X archives it.
private struct StaleQuestion: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let question = model.staleQuestion()
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                if let question {
                    PageSection("Still true?") {
                        Card {
                            VStack(alignment: .leading, spacing: 6) {
                                HStack(spacing: 10) {
                                    TaskStatusIcon(status: question.row.status)
                                    Text(question.row.title).headlineText().lineLimit(2)
                                }
                                Text(meta(question.row)).metaText()
                            }
                            .padding(16)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .overlay(alignment: .bottom) { Hairline().padding(.horizontal, 16) }
                            HStack(spacing: 12) {
                                answer("Keep", key: "k") { model.answerStale(keep: true) }
                                answer("Archive", key: "x") { model.answerStale(keep: false) }
                                Spacer(minLength: 12)
                                Button(action: model.finishMorning) {
                                    HStack(spacing: 6) {
                                        Text("Skip the rest")
                                        Shortcut("escape")
                                    }
                                }
                                .buttonStyle(QuietButton(compact: true))
                            }
                            .padding(.horizontal, 16)
                            .padding(.vertical, 12)
                        }
                    }
                    .id(question.row.id)
                    .transition(.opacity)
                }
            }
            .frame(maxWidth: Theme.contentMax, alignment: .leading)
            .padding(.horizontal, Theme.margin)
            .padding(.top, Space.l)
            .frame(maxWidth: .infinity)
            .animation(Motion.regular, value: question?.row.id)
        }
    }

    /// When it was last touched, and where it lives.
    private func meta(_ row: WorkItemRow) -> String {
        var parts = ["Untouched since \(PlanningFormat.day(row.updatedAtMs))"]
        if row.direction == .waitingOn, let owner = row.ownerName { parts.append("Waiting on \(owner)") }
        if let project = row.projectTitle { parts.append(project) }
        if let meeting = row.sourceMeetingTitle { parts.append(meeting) }
        return parts.joined(separator: " · ")
    }

    private func answer(_ title: String, key: String, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 6) {
                Text(title)
                Shortcut(key)
            }
        }
        .buttonStyle(.compact)
    }
}

import AppKit
import SwiftUI

/// Home under the dictation card, read top to bottom: today's counts, the
/// end-of-day note, everything that asks the reader a question, and what is
/// coming. The six widgets, the newest conversations, what Sona did without
/// being asked and the dictation numbers live one tab over, in Activity.
///
/// The card above it — the state word, the chord, the record control and the
/// mode — belongs to the capture screen.
struct OverviewView: View {
    /// Where the commitments widget scrolls to.
    static let needsYouAnchor = "overview-needs-you"
    /// Where the Today strip's meetings count scrolls to.
    static let upcomingAnchor = "overview-upcoming"

    let store: OverviewStore
    /// Opens a retained meeting named by an open loop or a receipt.
    var openMeeting: (String) -> Void = { _ in }
    /// The standing decisions the recurring upcoming rows offer. Meeting
    /// settings owns those three commands, so the integrator hands them in.
    var series: OverviewSeriesActions?
    /// Brings the needs-you list into view. The page owns the scroll.
    var showNeedsYou: () -> Void = {}
    /// Brings the calendar ahead into view.
    var showUpcoming: () -> Void = {}

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ErrorNote(store.error)
            if let today = store.today {
                OverviewTodayStrip(today: today, showUpcoming: showUpcoming, showNeedsYou: showNeedsYou)
            }
            EveningView()
            OverviewNeedsYou(store: store, openMeeting: openMeeting)
                .id(Self.needsYouAnchor)
            OverviewUpcomingSection(store: store, series: series)
                .id(Self.upcomingAnchor)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// Home's second tab: the six numbers, the newest conversations and notes,
/// what Sona did without being asked, and the dictation numbers in full.
struct ActivityScreen: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let store = model.overview
        Page {
            VStack(alignment: .leading, spacing: 0) {
                ErrorNote(store.error)
                OverviewWidgets(store: store) {
                    model.homeAnchor = OverviewView.needsYouAnchor
                    model.go(.capture)
                }
                OverviewConversations(store: store)
                FeedRecentView(store: store, openMeeting: model.openMeeting) { model.showSettings(.workflows) }
                if !OverviewStatsCard.nothingDictated(store.usage) {
                    OverviewStatsCard(store: store)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .pageBar {
            HomeTabs()
        } trailing: {
            EmptyView()
        }
    }
}

// MARK: - Today

/// One row across the top of Home: what today holds and what is waiting, each
/// a count that opens its list. A zero is left out, and so are today's
/// meetings when no calendar is connected; a count whose source could not be
/// read says so instead of reading as zero.
struct OverviewTodayStrip: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let today: HomeToday
    let showUpcoming: () -> Void
    let showNeedsYou: () -> Void

    var body: some View {
        TimelineView(.everyMinute) { context in
            let counters = counters(context.date)
            if !counters.isEmpty {
                HStack(spacing: 0) {
                    ForEach(Array(counters.enumerated()), id: \.element.id) { index, counter in
                        if index > 0 {
                            Rectangle().fill(Theme.border).frame(width: 1, height: 20)
                        }
                        TodayCounterView(counter: counter, revision: model.people.photoRevision)
                            .transition(.opacity)
                    }
                    Spacer(minLength: 0)
                }
                .padding(4)
                .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusCard))
                .overlay(RoundedRectangle(cornerRadius: Theme.radiusCard).strokeBorder(Theme.border, lineWidth: 1))
                .animation(Motion.settle(reduced: reduceMotion), value: counters.map(\.id))
                .padding(.bottom, 28)
            }
        }
    }

    private func counters(_ now: Date) -> [TodayCounter] {
        let speakers = today.unknownSpeakers.value
        let untracked = today.untracked.value
        let owed = today.owed.value
        let meetings = today.meetings.map { count in
            TodayCounter(
                id: "meetings", value: count.value?.count, label: count.value?.count == 1 ? "meeting" : "meetings",
                name: "Meetings today", unreadable: "Calendar unavailable",
                faces: (count.value?.people ?? []).map { AvatarFace(kind: .person($0.personId), name: $0.name) },
                action: showUpcoming)
        }
        let counters: [TodayCounter] = [
            TodayCounter(
                id: "processing", value: today.processing.value,
                label: today.processing.value == 1 ? "meeting processing" : "meetings processing",
                name: "Meetings processing",
                action: {
                    model.meetings.closeReview()
                    model.meetings.choose(status: .processing)
                    model.go(.meetings)
                }),
            TodayCounter(
                id: "speakers", value: speakers?.count,
                label: speakers?.count == 1 ? "speaker to name" : "speakers to name",
                name: "Speakers not named",
                action: speakers?.meetingId.map(review)),
            TodayCounter(
                id: "untracked", value: untracked?.count,
                label: untracked?.count == 1 ? "promise not in a project" : "promises not in a project",
                name: "Promises not in a project",
                action: untracked?.meetingId.map(review)),
            TodayCounter(
                id: "owed", value: owed?.count, label: "owed", name: "Promises you owe",
                detail: owed?.oldestAtUtcMs.map { Self.age(since: $0, now: now) },
                late: owed?.late ?? 0,
                action: showNeedsYou),
        ]
        return ([meetings].compactMap { $0 } + counters).filter { $0.value != 0 }
    }

    /// A meeting's review, where its speakers are named and its promises
    /// tracked.
    private func review(_ meetingId: MeetingSessionId) -> () -> Void {
        {
            model.openMeeting(meetingId)
            model.meetings.choose(tab: .review)
        }
    }

    private static func age(since milliseconds: Int64, now: Date) -> String {
        let calendar = Calendar.current
        let days = calendar.dateComponents(
            [.day], from: calendar.startOfDay(for: FeedClock.date(milliseconds)),
            to: calendar.startOfDay(for: now)).day ?? 0
        return days < 1 ? "oldest today" : days == 1 ? "oldest 1 day" : "oldest \(days) days"
    }
}

/// One count on the Today strip. `value` is absent when its source could not
/// be read.
private struct TodayCounter: Identifiable {
    let id: String
    let value: Int?
    let label: String
    /// What the count is, in words, for the tooltip and VoiceOver.
    let name: String
    var unreadable = "Can't be read right now"
    var faces: [AvatarFace] = []
    var detail: String?
    var late = 0
    let action: (() -> Void)?

    var help: String { value == nil ? "\(name): \(unreadable.lowercased())" : name }

    var spoken: String {
        guard let value else { return help }
        return ["\(name): \(value)", detail, late > 0 ? "\(late) late" : nil]
            .compactMap { $0 }.joined(separator: ", ")
    }
}

private struct TodayCounterView: View {
    let counter: TodayCounter
    let revision: UInt64
    @State private var hovering = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        if counter.value != nil, let action = counter.action {
            Button(action: action) { content }
                .buttonStyle(.plain)
                .onHover { hovering = $0 }
                .help(counter.help)
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(counter.spoken)
                .accessibilityAddTraits(.isButton)
                .accessibilityAction { action() }
        } else {
            content
                .help(counter.help)
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(counter.spoken)
        }
    }

    private var content: some View {
        HStack(spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 4) {
                Text(counter.value.map { "\($0)" } ?? "–")
                    .font(.system(size: 20, weight: .semibold))
                    .foregroundStyle(counter.value == nil ? Theme.inkTertiary : Theme.ink)
                    .monospacedDigit()
                    .contentTransition(reduceMotion ? .opacity : .numericText())
                Text(counter.label).font(TypeScale.body(12)).foregroundStyle(Theme.inkSecondary)
                if let detail = counter.detail {
                    Text(detail).metaText()
                }
                if counter.late > 0 {
                    Text("\(counter.late) late")
                        .font(TypeScale.body(12))
                        .foregroundStyle(Theme.live)
                        .monospacedDigit()
                        .contentTransition(reduceMotion ? .opacity : .numericText())
                }
            }
            .lineLimit(1)
            .animation(Motion.settle(reduced: reduceMotion), value: counter.value)
            .animation(Motion.settle(reduced: reduceMotion), value: counter.late)
            if !counter.faces.isEmpty {
                AvatarStack(faces: counter.faces, size: 18, limit: 4, revision: revision)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(Theme.wash.opacity(hovering ? 1 : 0), in: RoundedRectangle(cornerRadius: Theme.radiusControl))
        .contentShape(Rectangle())
        .hoverFade(hovering)
    }
}

// MARK: - Widgets

/// Home's six numbers, each a tile that opens the place it counts.
struct OverviewWidgets: View {
    @Environment(AppModel.self) private var model
    let store: OverviewStore
    let showNeedsYou: () -> Void

    private static let columns = Array(repeating: GridItem(.flexible(), spacing: 12, alignment: .top), count: 3)

    var body: some View {
        TimelineView(.everyMinute) { context in
            let now = context.date
            let week = OverviewWeek(ending: now)
            LazyVGrid(columns: Self.columns, alignment: .leading, spacing: 12) {
                today(now)
                recorded(week)
                if store.trend?.allTime.words != 0 {
                    dictated(week)
                }
                commitments()
                overdue(now)
                peopleMet(week)
            }
        }
        .padding(.bottom, 28)
    }

    /// Today's minutes and the next event. How many meetings today holds
    /// leads the Today strip, so it is not counted twice here.
    private func today(_ now: Date) -> some View {
        let calendar = Calendar.current
        let loaded: [MeetingHistorySummary]? = if case let .loaded(entries) = store.recentMeetings {
            entries.filter { calendar.isDate($0.date, inSameDayAs: now) }
        } else {
            nil
        }
        let recorded = loaded ?? []
        let ahead = (store.upcoming?.rows ?? []).filter {
            calendar.isDate(FeedClock.date($0.startUtcMs), inSameDayAs: now) && FeedClock.date($0.endUtcMs) > now
        }
        let blocks = recorded.map {
            DayStripBlock(id: "recorded:\($0.id)", start: $0.date,
                          end: $0.date.addingTimeInterval($0.recordedDuration ?? 0), recorded: true)
        } + ahead.map {
            DayStripBlock(id: "ahead:\($0.eventKey)", start: FeedClock.date($0.startUtcMs),
                          end: FeedClock.date($0.endUtcMs), recorded: false)
        }
        let seconds = recorded.compactMap(\.recordedDuration).reduce(0, +)
        let countOnly = seconds == 0 && !recorded.isEmpty
        let value = loaded == nil ? "–" : countOnly ? "\(recorded.count)" : "\(Int((seconds / 60).rounded(.up)))"
        let unit = countOnly ? "recorded" : "min recorded"
        let next = ahead.min(by: { $0.startUtcMs < $1.startUtcMs })
        return WidgetTile("Today", icon: "calendar", value: value, unit: unit,
                          caption: next.map { "next \(FeedClock.date($0.startUtcMs).formatted(date: .omitted, time: .shortened))" },
                          action: { model.go(.meetings) }) {
            DayStrip(blocks: blocks, now: now)
        }
    }

    private func recorded(_ week: OverviewWeek) -> some View {
        let points: [ActivityMeetingPoint]? = if case let .available(days) = store.meetings { days } else { nil }
        var minutes: [String: Double] = [:]
        var meetings = 0
        for point in points ?? [] where week.keys.contains(point.localDate) {
            minutes[point.localDate, default: 0] += Double(point.verifiedCapturedDurationMs) / 60_000
            meetings += point.meetings
        }
        let total = Int(minutes.values.reduce(0, +).rounded())
        return WidgetTile("Recorded this week", icon: "waveform", value: points == nil ? "–" : total.formatted(),
                          unit: "min", caption: points == nil ? nil : meetings == 1 ? "1 meeting" : "\(meetings) meetings",
                          action: { model.go(.meetings) }) {
            if points != nil {
                DayBars(days: week.chart { minutes[$0] ?? 0 } spoken: { "\(Int($0.rounded())) minutes" })
            }
        }
    }

    private func dictated(_ week: OverviewWeek) -> some View {
        var words: [String: Double] = [:]
        for point in store.trend?.points ?? [] where week.keys.contains(point.localDate) {
            words[point.localDate, default: 0] += Double(point.words)
        }
        let total = Int(words.values.reduce(0, +))
        return WidgetTile("Dictated this week", icon: "mic", value: store.trend == nil ? "–" : total.formatted(),
                          unit: "words", caption: store.trend.map { "\($0.allTime.words.formatted()) all time" },
                          action: { model.go(.library) }) {
            if store.trend != nil, total > 0 {
                Sparkline(days: week.chart { words[$0] ?? 0 } spoken: { "\(Int($0)) words" })
            }
        }
    }

    /// Every open promise, split by who owes it, and the oldest of yours. How
    /// many are yours and how long the oldest has waited lead the Today strip.
    private func commitments() -> some View {
        let loops: [FeedOpenLoop]? = if case let .loaded(entries) = store.openLoops { entries } else { nil }
        let count = loops?.count ?? 0
        let complete = count < OverviewStore.openLoopLimit
        let mine = complete ? (loops ?? []).filter { $0.direction == .mine } : []
        let theirs = complete ? (loops ?? []).filter { $0.direction == .waitingOn }.count : 0
        let oldest = mine.min { $0.raisedAtUtcMs < $1.raisedAtUtcMs }
        return WidgetTile("Commitments", icon: "checklist",
                          value: loops == nil ? "–" : complete ? "\(count)" : "\(count)+",
                          unit: "open",
                          action: count == 0 ? nil : showNeedsYou) {
            if !mine.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    PersonSplitBar(mine: mine.count, theirs: theirs)
                    if let oldest {
                        Text(oldest.text).font(TypeScale.body(11)).foregroundStyle(Theme.inkSecondary).lineLimit(1)
                    }
                }
            }
        }
    }

    private func overdue(_ now: Date) -> some View {
        let projects = model.projects
        let late = projects.overdueTasks(at: Int64(now.timeIntervalSince1970 * 1000))
            .sorted { ($0.dueAtMs ?? .max) < ($1.dueAtMs ?? .max) }
        let open = projects.activeOpenTaskCount
        let known = projects.projects != nil
        return WidgetTile("Overdue", icon: "exclamationmark.circle", value: known ? "\(late.count)" : "–",
                          unit: late.count == 1 ? "task" : "tasks", caption: known ? "of \(open) open" : nil,
                          tone: late.isEmpty ? Theme.ink : Theme.live,
                          action: {
                              if !late.isEmpty { projects.showOverdue() }
                              model.go(.projects)
                          }) {
            if known, open > 0, let oldest = late.first {
                VStack(alignment: .leading, spacing: 6) {
                    ShareBar(fraction: Double(late.count) / Double(open), tint: Theme.live)
                    Text(oldest.title).font(TypeScale.body(11)).foregroundStyle(Theme.inkSecondary).lineLimit(1)
                }
            }
        }
    }

    private func peopleMet(_ week: OverviewWeek) -> some View {
        let entries = model.people.entries
        let since = Int64(week.start.timeIntervalSince1970 * 1000)
        let met = (entries ?? [])
            .filter { ($0.lastMeetingAtUtcMs ?? .min) >= since }
            .sorted { ($0.lastMeetingAtUtcMs ?? 0) > ($1.lastMeetingAtUtcMs ?? 0) }
        return WidgetTile("People this week", icon: "person.2", value: entries == nil ? "–" : "\(met.count)",
                          unit: "met", caption: entries.map { $0.count == 1 ? "of 1 person" : "of \($0.count) people" },
                          action: { model.go(.people) }) {
            AvatarStack(faces: met.map { AvatarFace(kind: .person($0.person.id), name: $0.person.displayName) },
                        size: 22, limit: 6, revision: model.people.photoRevision)
        }
    }
}

/// The seven local days ending today: what "this week" means on Home, the
/// same span the numbers card counts.
private struct OverviewWeek {
    let days: [Date]
    let keys: [String]

    init(ending now: Date) {
        let calendar = Calendar.current
        let today = calendar.startOfDay(for: now)
        days = (0..<7).reversed().compactMap { calendar.date(byAdding: .day, value: -$0, to: today) }
        keys = days.map(ActivityDates.key)
    }

    var start: Date { days.first ?? .now }

    func chart(_ value: (String) -> Double, spoken: (Double) -> String) -> [ChartDay] {
        zip(days, keys).map { day, key in
            let amount = value(key)
            return ChartDay(date: day, value: amount, spoken: spoken(amount))
        }
    }
}

// MARK: - Capture mode

/// Which mode the next dictation will run in, changeable in place: the
/// trailing button of the dictation card's controls.
///
/// Modes left the sidebar: picking one is a capture decision and editing one
/// is a settings task, so this is the picking half. It reads and writes the
/// same active mode the switch chord and the overlay use, so there is one
/// owner of which mode is current.
struct CaptureModeChipView: View {
    let store: OverviewStore
    var openModes: () -> Void = {}

    var body: some View {
        // Nothing to say before the modes arrive, and a button naming a mode
        // this install does not have would be worse than none.
        if let snapshot = store.modes, let active = snapshot.active {
            Menu {
                Picker("Mode", selection: Binding(
                    get: { active },
                    set: { store.pick($0) }
                )) {
                    ForEach(snapshot.modes) { mode in
                        Text(mode.name).tag(mode)
                    }
                }
                .pickerStyle(.inline)
                .labelsHidden()
                Divider()
                Button("Edit modes in Settings", action: openModes)
            } label: {
                HStack(spacing: 6) {
                    Text("Mode").foregroundStyle(Theme.inkSecondary)
                    Text(active.name).lineLimit(1)
                    Image(systemName: "chevron.up.chevron.down")
                        .font(.system(size: 9, weight: .semibold))
                        .foregroundStyle(Theme.inkTertiary)
                }
            }
            .menuStyle(.button)
            .buttonStyle(.compact)
            .menuIndicator(.hidden)
            .fixedSize()
            .disabled(store.switchingMode || store.capturing)
            .help(store.capturing ? "Switching is off while a dictation is running" : "Mode for the next dictation")
            .accessibilityLabel("Mode, \(active.name)")
        }
    }
}

// MARK: - Needs you

/// Everything on this page that asks the reader for something, as one list: a
/// promise they made, a habit Sona wants permission to learn, a release they
/// are not on yet.
struct OverviewNeedsYou: View {
    let store: OverviewStore
    var openMeeting: (String) -> Void = { _ in }
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var showingAll = false
    /// How many promises the list draws before "Show all".
    private static let shown = 5

    var body: some View {
        // A list still being read is not an empty list, so the section draws
        // nothing until the open loops answer.
        if case .loading = store.openLoops {
            EmptyView()
        } else if nothingToShow {
            // The suggestions and the update check answer on their own clock,
            // so an empty page is not yet the claim this sentence makes.
            if store.suggestions != nil, store.updateChecked {
                Text("Nothing needs you.").metaText().padding(.bottom, 28)
            }
        } else {
            PageSection("Needs you") {
                Card {
                    if case .failed = store.openLoops {
                        FeedFailedRow(retry: store.refreshFeed)
                    }
                    ForEach(showingAll ? loops : Array(loops.prefix(Self.shown))) { loop in
                        OverviewLoopRow(loop: loop, openMeeting: openMeeting)
                    }
                    if loops.count > Self.shown {
                        CardRow(action: {
                            withAnimation(Motion.settle(reduced: reduceMotion)) { showingAll.toggle() }
                        }) {
                            Text(showingAll ? "Show fewer" : "Show all \(total)").metaText(Theme.inkSecondary)
                        }
                    }
                    ForEach(store.suggestions ?? []) { entry in
                        LearningRow(entry: entry, busy: store.answering.contains(entry.id)) { status in
                            store.answer(entry, status)
                        }
                    }
                    if let update = updateWaiting {
                        OverviewUpdateRow(update: update, dismiss: store.dismissUpdate)
                    }
                }
            }
        }
    }

    private var loops: [FeedOpenLoop] {
        if case let .loaded(entries) = store.openLoops { entries } else { [] }
    }

    private var total: String {
        loops.count >= OverviewStore.openLoopLimit ? "\(loops.count)+" : "\(loops.count)"
    }

    private var updateWaiting: OverviewUpdate? {
        guard !store.updateDismissed, let update = store.update, update.waiting else { return nil }
        return update
    }

    private var nothingToShow: Bool {
        guard case let .loaded(entries) = store.openLoops else { return false }
        return entries.isEmpty && (store.suggestions ?? []).isEmpty && updateWaiting == nil
    }
}

/// A promise whose meeting is gone still has to be read; it just has nothing
/// to open.
private struct OverviewLoopRow: View {
    let loop: FeedOpenLoop
    let openMeeting: (String) -> Void

    var body: some View {
        CardRow(action: loop.meetingId.isEmpty ? nil : { openMeeting(loop.meetingId) }) {
            VStack(alignment: .leading, spacing: 4) {
                Text(loop.text).bodyText(14)
                Text(meta).metaText().lineLimit(1)
            }
        }
    }

    private var meta: String {
        "\(need) · \(loop.title) · \(FeedClock.ago(loop.raisedAtUtcMs))"
    }

    /// What the reader is being asked for: these are the questions and
    /// topics a meeting left unsettled.
    private var need: String {
        switch loop.direction {
        case .mine: "Yours to settle"
        case .waitingOn: loop.waitingOnStale ? "Waiting on them over a week" : "Waiting on them"
        case .unattributed: "Left open, no owner"
        }
    }
}

/// One mined suggestion: the question, the evidence behind it, and the two
/// answers.
private struct LearningRow: View {
    let entry: LearningEntry
    let busy: Bool
    let answer: (DecisionStatus) -> Void

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text(entry.suggestion.headline).bodyText(14)
                Text(entry.evidence.sentence).metaText()
            }
        } trailing: {
            HStack(spacing: 12) {
                if entry.suggestion.acceptable {
                    Button("Yes") { answer(.accepted) }
                        .buttonStyle(.compact)
                        .disabled(busy)
                }
                Button("No thanks") { answer(.dismissed) }
                    .buttonStyle(.quiet)
                    .disabled(busy)
            }
        }
    }
}

/// The release the reader is not on yet. Settings > About owns the standing
/// answer and the button that asks again.
private struct OverviewUpdateRow: View {
    let update: OverviewUpdate
    let dismiss: () -> Void

    var body: some View {
        CardRow {
            Text(update.sentence).bodyText(14)
        } trailing: {
            HStack(spacing: 12) {
                if let address = update.url, let url = URL(string: address) {
                    Button("View") { NSWorkspace.shared.open(url) }.buttonStyle(.compact)
                }
                Button("Dismiss", action: dismiss).buttonStyle(.quiet)
            }
        }
    }
}

// MARK: - Upcoming

/// What the calendar holds in the next seven days. Drawn only under a
/// granted calendar: without one, Meetings is where the grant is offered.
struct OverviewUpcomingSection: View {
    let store: OverviewStore
    /// The standing decisions the recurring rows offer. Absent until the
    /// integrator wires Meeting settings in, and then the rows say only what
    /// is coming.
    var series: OverviewSeriesActions?

    var body: some View {
        if let upcoming = store.upcoming, upcoming.access == "authorized" {
            PageSection("Next seven days") {
                Card {
                    if upcoming.rows.isEmpty {
                        CardRow {
                            Text("Nothing in the next seven days.").metaText()
                        }
                    } else {
                        ForEach(upcoming.rows) { row in
                            OverviewUpcomingRowView(store: store, row: row, actions: series)
                        }
                    }
                }
            }
        }
    }
}

private struct OverviewUpcomingRowView: View {
    let store: OverviewStore
    let row: OverviewUpcomingRow
    var actions: OverviewSeriesActions?
    @State private var open = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            CardRow {
                VStack(alignment: .leading, spacing: 4) {
                    HStack(spacing: 8) {
                        Text(row.title).bodyText(14)
                        if row.series != nil {
                            Chip("Repeats")
                        }
                    }
                    Text(row.meta).metaText()
                    if let place = row.place {
                        Text("at \(place.name)").metaText().lineLimit(1)
                    } else if let location = row.location {
                        Text(location).metaText().lineLimit(1)
                    }
                }
            } trailing: {
                HStack(spacing: 12) {
                    if let address = row.joinUrl, let url = URL(string: address) {
                        Button("Join") { NSWorkspace.shared.open(url) }.buttonStyle(.compact)
                    }
                    // Only rows with a series have anything to disclose, and
                    // only a wired page can act on it.
                    if row.series != nil, actions != nil {
                        Button { open.toggle() } label: {
                            Image(systemName: "chevron.down")
                                .font(.system(size: 10, weight: .semibold))
                                .foregroundStyle(Theme.inkTertiary)
                                .rotationEffect(.degrees(open ? 180 : 0))
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Series options for \(row.title)")
                    }
                }
            }
            if open, let series = row.series, let actions {
                OverviewSeriesControls(store: store, series: series, actions: actions)
            }
        }
    }
}

/// The three decisions that belong to the series rather than to this
/// occurrence, behind the row's disclosure: a calendar row's job is to say
/// what is next, and three switches on every row would make the section a
/// settings page with dates on it.
private struct OverviewSeriesControls: View {
    let store: OverviewStore
    let series: OverviewSeries
    let actions: OverviewSeriesActions

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ToggleRow(
                title: "Always record this series",
                // The switch spends the microphone and this Mac's audio
                // output without asking again, so it says so where it is
                // thrown rather than in a settings page nobody opened.
                detail: "Sona captures your microphone and this Mac's audio output when a meeting in this series starts, without asking.",
                isOn: Binding(
                    get: { series.alwaysRecord },
                    set: { next in
                        store.writeSeries(series.seriesKey) { revision in
                            try await actions.setAlwaysRecord(series.seriesKey, next, revision)
                        }
                    }))
            MeetingTemplateChoiceRow(
                title: "Notes template",
                builtIns: OverviewTemplate.allCases,
                builtInLabel: { $0.label },
                templates: actions.templates,
                none: "App default",
                selection: Binding(
                    get: {
                        MeetingTemplateChoice(builtIn: series.template, customTemplateId: series.customTemplateId)
                    },
                    set: { next in
                        store.writeSeries(series.seriesKey) { revision in
                            try await actions.setTemplate(series.seriesKey, next, revision)
                        }
                    }))
            ToggleRow(
                title: "Include in the evening digest",
                isOn: Binding(
                    get: { series.digestIncluded },
                    set: { next in
                        store.writeSeries(series.seriesKey) { revision in
                            try await actions.setDigestIncluded(series.seriesKey, next, revision)
                        }
                    }))
        }
        .padding(.leading, 20)
        // A write is fenced by one number for the whole pane, so the row
        // being written stops offering more of them until it lands.
        .disabled(store.savingSeries != nil)
        .task { await actions.templates.loadIfNeeded() }
    }
}

// MARK: - Numbers

/// Which stretch the numbers card reads.
private enum OverviewPeriod: CaseIterable {
    case allTime
    case week

    var title: String {
        switch self {
        case .allTime: "All time"
        case .week: "This week"
        }
    }
}

/// How much has been said, as one card: the period, its numbers in a row,
/// and what they rest on behind the chevron.
///
/// This week's numbers are always this week's, never the paged week's: the
/// period says "This week", and numbers that changed as the charts paged
/// backwards would describe a week the period denies. The caption over the
/// charts says which week they draw.
struct OverviewStatsCard: View {
    @Environment(AppModel.self) private var model
    let store: OverviewStore
    @State private var period = OverviewPeriod.allTime
    @State private var page = 0
    @State private var open = false

    var body: some View {
        // The week is its own read and may not answer; all time is the one
        // period with numbers then.
        let week = store.trend.flatMap { period == .week ? $0 : nil }
        Card {
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 12) {
                    periodControl
                    Spacer(minLength: 12)
                    Button { open.toggle() } label: {
                        Image(systemName: "chevron.down")
                            .font(.system(size: 11, weight: .semibold))
                            .foregroundStyle(Theme.inkTertiary)
                            .rotationEffect(.degrees(open ? 180 : 0))
                            .frame(width: 24, height: 24)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .help(open ? "Hide details" : "Show details")
                    .accessibilityLabel(open ? "Hide details" : "Show details")
                }
                if let week {
                    weekFigures(week)
                } else {
                    allTimeFigures
                }
            }
            .padding(16)
            .overlay(alignment: .bottom) { Hairline().padding(.horizontal, 16) }
            if open {
                if let week {
                    ActivityBandBody(trend: week, meetings: store.meetings, page: $page)
                } else {
                    allTimeDetails
                }
            }
        }
        .padding(.bottom, 28)
    }

    /// A card of zeroes says nothing the reader needs, so Activity leaves it
    /// out until the first dictation.
    static func nothingDictated(_ usage: UsageState) -> Bool {
        if case let .loaded(stats, _) = usage { stats.dictations == 0 } else { false }
    }

    @ViewBuilder
    private var periodControl: some View {
        if store.trend == nil {
            Text(OverviewPeriod.allTime.title)
                .font(TypeScale.label(13))
                .foregroundStyle(Theme.inkSecondary)
        } else {
            Menu {
                Picker("Period", selection: $period) {
                    ForEach(OverviewPeriod.allCases, id: \.self) { choice in
                        Text(choice.title).tag(choice)
                    }
                }
                .pickerStyle(.inline)
                .labelsHidden()
            } label: {
                Text(period.title)
            }
            .menuStyle(.borderlessButton)
            .tint(Theme.inkSecondary)
            .fixedSize()
            .foregroundStyle(Theme.inkSecondary)
        }
    }

    @ViewBuilder
    private var allTimeFigures: some View {
        switch store.usage {
        case .loading:
            Text("Loading…").metaText()
        case .failed:
            HStack(alignment: .firstTextBaseline, spacing: 12) {
                Text("Couldn't read your usage.").metaText()
                Spacer(minLength: 12)
                Button("Retry", action: store.reloadUsage).buttonStyle(.quiet)
            }
        case let .loaded(stats, _):
            HStack(alignment: .top, spacing: 16) {
                Stat(label: "Words", value: stats.totalWords.formatted())
                Stat(label: "Words a minute", value: stats.wordsPerMinuteDictations > 0 ? "\(stats.wordsPerMinute)" : "–")
                Stat(label: "Time saved", value: stats.timeSaved)
                Stat(label: "Streak", value: UsageStats.days(stats.currentStreakDays))
            }
        }
    }

    /// Page 0 whatever the charts are drawing: the numbers belong to the
    /// period, not to the paged range.
    private func weekFigures(_ trend: ActivityTrend) -> some View {
        let days = Array(activityWindow(trend.points, page: 0).points)
        var dictations = 0
        var words = 0
        for day in days {
            dictations += day.recordings
            words += day.words
        }
        let meetings = ActivityBandBody.meetingCounts(days, store.meetings)?.reduce(0, +)
        return HStack(alignment: .top, spacing: 16) {
            Stat(label: "Dictations", value: dictations.formatted())
            Stat(label: "Words", value: words.formatted())
            Stat(label: "Streak", value: UsageStats.days(trend.currentStreakDays))
            if let meetings {
                Stat(label: "Meetings", value: meetings.formatted())
            }
        }
    }

    /// What the four numbers rest on, which apps the words went into, and the
    /// switch that decides whether an app is recorded with a dictation at
    /// all, beside the list it exists for rather than on a settings page.
    @ViewBuilder
    private var allTimeDetails: some View {
        if case let .loaded(stats, apps) = store.usage, stats.dictations > 0 {
            CardLine(stats.footing)
            UsageAppsList(apps: apps, other: stats.other, counting: model.settings.settings.countWordsPerApp)
        }
        ToggleRow(
            title: "Count words per app",
            detail: "Remembers which app each dictation went into. Stays on this Mac.",
            isOn: Binding(
                get: { model.settings.settings.countWordsPerApp },
                set: { value in Task { await model.settings.setCountWordsPerApp(value) } }
            )
        )
        .disabled(model.settings.isBusy("count_words_per_app"))
    }
}

// MARK: - Activity

private struct ActivityBandBody: View {
    let trend: ActivityTrend
    let meetings: ActivityMeetingTrend?
    @Binding var page: Int

    var body: some View {
        let window = activityWindow(trend.points, page: page)
        let points = Array(window.points)
        let labels = points.map { ActivityDates.narrowWeekday($0.localDate) }

        VStack(alignment: .leading, spacing: 16) {
            ActivityPager(
                caption: ActivityDates.range(points),
                canGoBack: window.start > 0,
                canGoForward: window.page > 0,
                back: { page += 1 },
                forward: { page = max(0, page - 1) }
            )
            HStack(alignment: .top, spacing: 16) {
                ActivityMeasure("Dictations") {
                    ActivityBars(values: points.map(\.recordings), labels: labels)
                }
                ActivityMeasure("Words") {
                    ActivitySparkline(values: points.map(\.words), labels: labels)
                }
                ActivityMeasure("Streak") {
                    ActivityWeekDots(points: points, labels: labels)
                }
                if let counts = Self.meetingCounts(points, meetings) {
                    ActivityMeasure("Meetings") {
                        ActivityBars(values: counts, labels: labels)
                    }
                }
            }
        }
        .padding(16)
    }

    /// The meeting trend joined onto the days the dictation charts are
    /// drawing. Storage that cannot answer gets no column and no number,
    /// rather than a week of zeroes it never measured.
    static func meetingCounts(_ points: [ActivityTrendPoint], _ meetings: ActivityMeetingTrend?) -> [Int]? {
        guard case let .available(days) = meetings else { return nil }
        var byDate: [String: Int] = [:]
        byDate.reserveCapacity(days.count)
        for day in days {
            byDate[day.localDate] = day.meetings
        }
        return points.map { byDate[$0.localDate] ?? 0 }
    }
}

/// Which week the charts are drawing, and the two steps that change it.
private struct ActivityPager: View {
    let caption: String
    let canGoBack: Bool
    let canGoForward: Bool
    let back: () -> Void
    let forward: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            Button(action: back) {
                Image(systemName: "chevron.left").font(.system(size: 11, weight: .semibold))
            }
            .buttonStyle(.quiet)
            .disabled(!canGoBack)
            .accessibilityLabel("Previous 7 days")
            Text(caption).metaText(Theme.inkSecondary).frame(minWidth: 110)
            Button(action: forward) {
                Image(systemName: "chevron.right").font(.system(size: 11, weight: .semibold))
            }
            .buttonStyle(.quiet)
            .disabled(!canGoForward)
            .accessibilityLabel("Next 7 days")
        }
    }
}

/// One chart inside the disclosure: what it counts, and the shape under it.
private struct ActivityMeasure<Content: View>: View {
    let label: String
    let content: Content

    init(_ label: String, @ViewBuilder content: () -> Content) {
        self.label = label
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(label).metaText(Theme.inkSecondary)
            content
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// The geometry every chart in the band shares.
enum ActivityChart {
    /// Seven days to a page, and seven slots whatever the page holds.
    static let slots = 7
    static let plotHeight: CGFloat = 52
    static let barWidth: CGFloat = 8
    /// A day with nothing on it still holds its place on the axis.
    static let stubHeight: CGFloat = 2
}

/// One bar per day, a stub where a day is empty, the weekday letter under it.
struct ActivityBars: View {
    let values: [Int]
    let labels: [String]

    var body: some View {
        let peak = max(values.max() ?? 0, 1)
        VStack(spacing: 0) {
            HStack(alignment: .bottom, spacing: 0) {
                ForEach(0..<ActivityChart.slots, id: \.self) { index in
                    let value = index < values.count ? max(0, values[index]) : 0
                    RoundedRectangle(cornerRadius: Theme.radiusMark, style: .continuous)
                        .fill(value == 0 ? Theme.track : Theme.accent)
                        .frame(width: ActivityChart.barWidth, height: ActivityBars.height(value, peak))
                        .frame(maxWidth: .infinity)
                }
            }
            .frame(height: ActivityChart.plotHeight, alignment: .bottom)
            Hairline()
            ActivityWeekdayRow(labels: labels)
        }
    }

    private static func height(_ value: Int, _ peak: Int) -> CGFloat {
        value == 0 ? ActivityChart.stubHeight : max(3, ActivityChart.plotHeight * CGFloat(value) / CGFloat(peak))
    }
}

/// The words per day as one line, with the last day marked.
struct ActivitySparkline: View {
    let values: [Int]
    let labels: [String]

    var body: some View {
        VStack(spacing: 0) {
            GeometryReader { proxy in
                let points = ActivityCurve.points(values, in: proxy.size)
                ZStack(alignment: .topLeading) {
                    if let area = ActivityCurve.area(points, bottom: proxy.size.height) {
                        area.fill(Theme.accentSoft)
                    }
                    if let line = ActivityCurve.line(points) {
                        line.stroke(Theme.accent, style: StrokeStyle(lineWidth: 1.5, lineCap: .round, lineJoin: .round))
                    }
                    if let last = points.last {
                        Circle().fill(Theme.accent).frame(width: 6, height: 6).position(last)
                    }
                }
            }
            .frame(height: ActivityChart.plotHeight)
            Hairline()
            ActivityWeekdayRow(labels: labels)
        }
    }
}

/// A dot per day: filled where something was dictated, ringed on today.
///
/// The trend has a streak total but no per-day streak payload, so the dots
/// derive from the same recordings the bars draw rather than inventing a
/// second definition of an active day.
struct ActivityWeekDots: View {
    let points: [ActivityTrendPoint]
    let labels: [String]

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 0) {
                ForEach(0..<ActivityChart.slots, id: \.self) { index in
                    let point = index < points.count ? points[index] : nil
                    Circle()
                        .fill((point?.recordings ?? 0) > 0 ? Theme.accent : Theme.track)
                        .frame(width: 10, height: 10)
                        .overlay {
                            if ActivityDates.isToday(point?.localDate) {
                                Circle().strokeBorder(Theme.accent.opacity(0.5), lineWidth: 1).padding(-3)
                            }
                        }
                        .frame(maxWidth: .infinity)
                }
            }
            .frame(height: ActivityChart.plotHeight, alignment: .bottom)
            Hairline()
            ActivityWeekdayRow(labels: labels)
        }
    }
}

private struct ActivityWeekdayRow: View {
    let labels: [String]

    var body: some View {
        HStack(spacing: 0) {
            ForEach(0..<ActivityChart.slots, id: \.self) { index in
                Text(index < labels.count ? labels[index] : "")
                    .font(TypeScale.body(9))
                    .foregroundStyle(Theme.inkTertiary)
                    .frame(maxWidth: .infinity)
            }
        }
        .padding(.top, 6)
    }
}

/// The sparkline's arithmetic: a padded domain so a flat week is still a
/// line, and a monotone cubic through the days so the curve never overshoots
/// a value that was never measured.
enum ActivityCurve {
    private static let inset: CGFloat = 4

    static func points(_ values: [Int], in size: CGSize) -> [CGPoint] {
        guard !values.isEmpty else { return [] }
        let samples = values.map { Double(max(0, $0)) }
        var minimum = samples[0]
        var maximum = samples[0]
        for value in samples {
            minimum = Swift.min(minimum, value)
            maximum = Swift.max(maximum, value)
        }
        let span = maximum - minimum
        let padding = span == 0 ? Swift.max(maximum * 0.12, 1) : Swift.max(span * 0.2, maximum * 0.12)
        let low = Swift.max(0, minimum - padding)
        let high = maximum + padding * 1.5
        let scale = Swift.max(high - low, 0.001)
        let width = Swift.max(size.width - inset * 2, 1)
        return samples.enumerated().map { index, value in
            let x = samples.count == 1
                ? size.width / 2
                : inset + width * CGFloat(index) / CGFloat(samples.count - 1)
            return CGPoint(x: x, y: size.height * CGFloat((high - value) / scale))
        }
    }

    static func line(_ points: [CGPoint]) -> Path? {
        guard let first = points.first, points.count >= 2 else { return nil }
        var path = Path()
        path.move(to: first)
        curve(&path, points)
        return path
    }

    static func area(_ points: [CGPoint], bottom: CGFloat) -> Path? {
        guard let first = points.first, let last = points.last, points.count >= 2 else { return nil }
        var path = Path()
        path.move(to: CGPoint(x: first.x, y: bottom))
        path.addLine(to: first)
        curve(&path, points)
        path.addLine(to: CGPoint(x: last.x, y: bottom))
        path.closeSubpath()
        return path
    }

    private static func curve(_ path: inout Path, _ points: [CGPoint]) {
        var slopes: [CGFloat] = []
        slopes.reserveCapacity(points.count - 1)
        for index in 0..<(points.count - 1) {
            let start = points[index]
            let end = points[index + 1]
            let run = end.x - start.x
            slopes.append(run == 0 ? 0 : (end.y - start.y) / run)
        }
        var tangents: [CGFloat] = []
        tangents.reserveCapacity(points.count)
        for index in 0..<points.count {
            if index == 0 {
                tangents.append(slopes[0])
            } else if index == points.count - 1 {
                tangents.append(slopes[slopes.count - 1])
            } else {
                let previous = slopes[index - 1]
                let next = slopes[index]
                tangents.append(previous * next <= 0 ? 0 : 2 * previous * next / (previous + next))
            }
        }
        for index in 0..<(points.count - 1) {
            let start = points[index]
            let end = points[index + 1]
            let run = end.x - start.x
            path.addCurve(
                to: end,
                control1: CGPoint(x: start.x + run / 3, y: start.y + tangents[index] * run / 3),
                control2: CGPoint(x: end.x - run / 3, y: end.y - tangents[index + 1] * run / 3))
        }
    }
}

/// The trend's dates are local calendar days as `2026-09-12`, with no zone to
/// convert: they are parsed in the reader's own calendar or not at all.
enum ActivityDates {
    private static let wire: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateFormat = "yyyy-MM-dd"
        return formatter
    }()

    private static let narrow: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateFormat = "EEEEE"
        return formatter
    }()

    static func parse(_ localDate: String) -> Date? {
        wire.date(from: localDate)
    }

    static func key(_ date: Date) -> String {
        wire.string(from: date)
    }

    static func narrowWeekday(_ localDate: String) -> String {
        guard let date = parse(localDate) else { return "" }
        return narrow.string(from: date)
    }

    static func isToday(_ localDate: String?) -> Bool {
        guard let localDate, let date = parse(localDate) else { return false }
        return Calendar.current.isDateInToday(date)
    }

    /// "6 Sep – 12 Sep".
    static func range(_ points: [ActivityTrendPoint]) -> String {
        guard let first = points.first.flatMap({ parse($0.localDate) }),
              let last = points.last.flatMap({ parse($0.localDate) })
        else { return "" }
        return "\(first.short) – \(last.short)"
    }
}

// MARK: - Usage

/// Where the words went, most first. Rows the core could not name are one
/// "Other" line, so the list adds up to the total above it. With counting
/// off the list says so instead of showing a stale one.
private struct UsageAppsList: View {
    let apps: [UsageAppRow]
    let other: UsageOther
    let counting: Bool

    var body: some View {
        if !counting {
            CardLine("Word counts per app are off.")
        } else if apps.isEmpty {
            CardLine("No app totals yet.")
        } else {
            ForEach(apps) { row in
                UsageAppRowView(row: row)
            }
            if other.dictations > 0 {
                UsageAppRowView(row: UsageAppRow(
                    id: "other", name: "Other", icon: nil, dictations: other.dictations, words: other.words))
            }
        }
    }
}

private struct UsageAppRowView: View {
    let row: UsageAppRow

    var body: some View {
        CardRow {
            HStack(spacing: 10) {
                if let icon = row.icon {
                    Image(nsImage: icon).resizable().frame(width: 18, height: 18)
                } else {
                    Image(systemName: "app.dashed")
                        .font(.system(size: 14))
                        .foregroundStyle(Theme.inkTertiary)
                        .frame(width: 18, height: 18)
                }
                Text(row.name).bodyText(14)
            }
        } trailing: {
            Text(row.counts).metaText()
        }
    }
}

// MARK: - Conversations and notes

/// The newest conversations and notes as one list, newest first, each
/// opening where it lives, beside the way to start a note.
struct OverviewConversations: View {
    @Environment(AppModel.self) private var model
    let store: OverviewStore
    private static let shown = 6

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline, spacing: 12) {
                Text("Conversations and notes").sectionLabel()
                    .accessibilityAddTraits(.isHeader)
                Spacer(minLength: 12)
                Button("New note") {
                    model.go(.scratchpad)
                    Task { await model.scratchpad.newNote() }
                }
                .buttonStyle(.compact)
            }
            Card {
                if failed {
                    FeedFailedRow { Task { await store.loadRecent() } }
                }
                if entries.isEmpty {
                    if loading {
                        CardLine("Loading…")
                    } else if !failed {
                        CardLine("No conversations or notes yet.")
                    }
                }
                ForEach(entries) { entry in
                    CardRow(action: { open(entry) }) {
                        HStack(alignment: .firstTextBaseline, spacing: 10) {
                            Image(systemName: entry.isNote ? "note.text" : "bubble.left.and.bubble.right")
                                .font(.system(size: 12))
                                .foregroundStyle(Theme.inkTertiary)
                                .frame(width: 16)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(entry.title).bodyText(14).lineLimit(1)
                                if !entry.detail.isEmpty {
                                    Text(entry.detail).metaText().lineLimit(1)
                                }
                            }
                        }
                    } trailing: {
                        Text(FeedClock.ago(entry.atMs)).metaText()
                    }
                }
            }
            HStack(spacing: 16) {
                Button("All conversations") { model.go(.meetings) }.buttonStyle(.quiet)
                Button("All notes") { model.go(.scratchpad) }.buttonStyle(.quiet)
            }
        }
        .padding(.bottom, 28)
    }

    private var entries: [OverviewRecentEntry] {
        var all: [OverviewRecentEntry] = []
        if case let .loaded(meetings) = store.recentMeetings {
            all += meetings.map {
                OverviewRecentEntry(
                    id: "meeting:\($0.id)", target: $0.id, isNote: false, title: $0.title,
                    detail: $0.headline?.text ?? Self.named($0.speakerLabels ?? []).joined(separator: ", "),
                    atMs: $0.createdAtUtcMs)
            }
        }
        if case let .loaded(notes) = store.recentNotes {
            all += notes.map {
                OverviewRecentEntry(
                    id: "note:\($0.id)", target: $0.id, isNote: true, title: $0.title, detail: $0.preview,
                    atMs: $0.updatedAtMs)
            }
        }
        all.sort { $0.atMs == $1.atMs ? $0.id < $1.id : $0.atMs > $1.atMs }
        return Array(all.prefix(Self.shown))
    }

    private var loading: Bool {
        if case .loading = store.recentMeetings { return true }
        if case .loading = store.recentNotes { return true }
        return false
    }

    private var failed: Bool {
        if case .failed = store.recentMeetings { return true }
        if case .failed = store.recentNotes { return true }
        return false
    }

    /// The labels the core writes itself, "Unknown speaker" and "Speaker 2",
    /// name nobody, so the row leaves them out.
    private static func named(_ labels: [String]) -> [String] {
        labels.filter { label in
            label != "Unknown speaker"
                && !(label.hasPrefix("Speaker ") && label.dropFirst("Speaker ".count).allSatisfy(\.isNumber))
        }
    }

    private func open(_ entry: OverviewRecentEntry) {
        if entry.isNote {
            model.open(link: "sona://note/\(entry.target)")
        } else {
            model.openMeeting(entry.target)
        }
    }
}

private struct OverviewRecentEntry: Identifiable {
    let id: String
    let target: String
    let isNote: Bool
    let title: String
    let detail: String
    let atMs: Int64
}

// MARK: - Recent

/// What Sona did without being asked: the latest runs that changed
/// something, each on its own card, and the way into the full run log.
struct FeedRecentView: View {
    let store: OverviewStore
    var openMeeting: (String) -> Void = { _ in }
    var seeAll: () -> Void = {}

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline, spacing: 12) {
                Text("Recent").sectionLabel()
                    .accessibilityAddTraits(.isHeader)
                Spacer(minLength: 12)
                Button("See all", action: seeAll).buttonStyle(.quiet)
            }
            switch store.receipts {
            case .loading:
                Card { CardLine("Loading…") }
            case .failed:
                Card { FeedFailedRow(retry: store.refreshFeed) }
            case .loaded:
                if entries.isEmpty {
                    Card { CardLine("No recent activity.") }
                } else {
                    VStack(spacing: 8) {
                        ForEach(entries) { receipt in
                            Card { FeedReceiptRow(receipt: receipt, openMeeting: openMeeting) }
                        }
                    }
                }
            }
        }
        .padding(.bottom, 28)
    }

    /// The list is effects, so a receipt with none does not reach it: the same
    /// rule the loader pages by, applied where the row is drawn.
    private var entries: [FeedReceipt] {
        guard case let .loaded(receipts) = store.receipts else { return [] }
        return Array(receipts.filter(\.hasEffect).prefix(3))
    }
}

/// A line with nothing to open is a line, not a dead button: skipping a
/// detected meeting leaves no session behind.
private struct FeedReceiptRow: View {
    let receipt: FeedReceipt
    let openMeeting: (String) -> Void

    var body: some View {
        let meetingId = receipt.jumpTarget?.meetingId
        CardRow(action: meetingId.map { id in { openMeeting(id) } }) {
            VStack(alignment: .leading, spacing: 4) {
                Text(receipt.sentence).bodyText(14).lineLimit(2)
                Text("\(receipt.source) · \(FeedClock.ago(receipt.finishedAtUtcMs))").metaText()
            }
        }
    }
}

/// A list that could not be read keeps its own row, and the way to ask again.
private struct FeedFailedRow: View {
    let retry: () -> Void

    var body: some View {
        CardRow {
            Text("Couldn't load this list.").metaText()
        } trailing: {
            Button("Retry", action: retry).buttonStyle(.quiet)
        }
    }
}

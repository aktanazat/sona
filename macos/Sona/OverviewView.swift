import AppKit
import SwiftUI

/// The capture page under the record button: the mode the next dictation runs
/// in, everything that asks the reader a question, what is coming, this
/// week's numbers, and what Sona did without being asked.
///
/// Read in that order, from the same origin every other page starts at. The
/// hero above it — the state word, the chord and the record control — belongs
/// to the shell.
struct OverviewView: View {
    let store: OverviewStore
    /// Opens a retained meeting named by an open loop or a receipt.
    var openMeeting: (String) -> Void = { _ in }
    /// Opens the Modes editor. Capture has no editor of its own.
    var openModes: () -> Void = {}
    /// The standing decisions the recurring upcoming rows offer. Meeting
    /// settings owns those three commands, so the integrator hands them in.
    var series: OverviewSeriesActions?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            CaptureModeChipView(store: store, openModes: openModes)
            ErrorNote(store.error)
            OverviewNeedsYou(store: store, openMeeting: openMeeting)
            OverviewUpcomingSection(store: store, series: series)
            ActivityBandView(store: store)
            FeedRecentView(store: store, openMeeting: openMeeting)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

// MARK: - Capture mode

/// Which mode the next dictation will run in, changeable in place.
///
/// Modes left the sidebar: picking one is a capture decision and editing one
/// is a settings task, so this is the picking half. It reads and writes the
/// same active mode the switch chord and the overlay use, so there is one
/// owner of which mode is current.
struct CaptureModeChipView: View {
    let store: OverviewStore
    var openModes: () -> Void = {}
    @State private var open = false

    var body: some View {
        // Nothing to say before the modes arrive, and a chip naming a mode
        // this install does not have would be worse than no chip.
        if let snapshot = store.modes, let active = snapshot.active {
            HStack(spacing: 8) {
                Text("Mode").metaText()
                Button { open = true } label: {
                    HStack(spacing: 5) {
                        Text(active.name).font(TypeScale.label(14)).foregroundStyle(Theme.ink)
                        Image(systemName: "chevron.down")
                            .font(.system(size: 9, weight: .semibold))
                            .foregroundStyle(Theme.inkTertiary)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .disabled(store.switchingMode || store.capturing)
                .popover(isPresented: $open, arrowEdge: .bottom) {
                    CaptureModePicker(store: store) {
                        open = false
                        openModes()
                    }
                }
                if store.capturing {
                    Text("switching is off while a dictation is running").metaText()
                }
            }
            .padding(.bottom, 24)
        }
    }
}

/// The list inside the chip's popover: every mode, the current one marked,
/// and one line out to the editor.
struct CaptureModePicker: View {
    let store: OverviewStore
    var openModes: () -> Void = {}

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Modes").sectionLabel().padding(.horizontal, 14).padding(.top, 12).padding(.bottom, 6)
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(store.modes?.modes ?? []) { mode in
                        CaptureModeRow(
                            mode: mode,
                            active: mode.id == store.modes?.active?.id,
                            busy: store.switchingMode
                        ) {
                            store.pick(mode)
                        }
                    }
                }
            }
            .frame(maxHeight: 260)
            Hairline()
            // Editing a mode is a different task in a different place; this
            // popover is for picking.
            Button("Edit modes in Settings", action: openModes)
                .buttonStyle(.quiet)
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
        }
        .frame(width: 260)
        .background(Theme.page)
    }
}

private struct CaptureModeRow: View {
    let mode: CaptureModeChoice
    let active: Bool
    let busy: Bool
    let pick: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: pick) {
            HStack(spacing: 8) {
                // The mark keeps its box when it is not the current mode, so
                // the names stay on one left edge down the list.
                Image(systemName: "checkmark")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(active ? Theme.accent : .clear)
                    .frame(width: 12)
                Text(mode.name)
                    .font(TypeScale.body(14))
                    .foregroundStyle(active ? Theme.ink : Theme.inkSecondary)
                    .lineLimit(1)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 14)
            .frame(height: 30)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(hovering && !busy ? Theme.wash : .clear)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(busy)
        .onHover { hovering = $0 }
    }
}

// MARK: - Needs you

/// Everything on this page that asks the reader for something, as one list: a
/// promise they made, a habit Sona wants permission to learn, a release they
/// are not on yet.
struct OverviewNeedsYou: View {
    let store: OverviewStore
    var openMeeting: (String) -> Void = { _ in }

    var body: some View {
        // A list still being read is not an empty list, so the section draws
        // nothing until the open loops answer.
        if case .loading = store.openLoops {
            EmptyView()
        } else if nothingToShow {
            // The suggestions and the update check answer on their own clock,
            // so an empty page is not yet the claim this sentence makes.
            if store.suggestions != nil, store.updateChecked {
                Text("Nothing needs you.").metaText().padding(.bottom, 32)
            }
        } else {
            PageSection("Needs you") {
                Card {
                    if case .failed = store.openLoops {
                        FeedFailedRow(retry: store.refreshFeed)
                    }
                    ForEach(loops) { loop in
                        OverviewLoopRow(loop: loop, openMeeting: openMeeting)
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
                Text("\(loop.title) · \(FeedClock.ago(loop.atUtcMs))").metaText().lineLimit(1)
            }
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

/// What the calendar holds in the next seven days. An empty week under a
/// granted calendar is a free week; an empty week under anything else is a
/// missing grant, and the section says something different for each.
struct OverviewUpcomingSection: View {
    let store: OverviewStore
    /// The standing decisions the recurring rows offer. Absent until the
    /// integrator wires Meeting settings in, and then the rows say only what
    /// is coming.
    var series: OverviewSeriesActions?

    var body: some View {
        if let upcoming = store.upcoming {
            PageSection("Next seven days") {
                Card {
                    if upcoming.rows.isEmpty {
                        CardRow {
                            Text(upcoming.emptyLine).metaText()
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
            ChoiceRow(
                title: "Notes template",
                choices: OverviewSeriesControls.templates,
                label: { $0?.label ?? "App default" },
                selection: Binding(
                    get: { series.template },
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
    }

    /// The app default first, then the core's own list.
    private static let templates: [OverviewTemplate?] = [nil] + OverviewTemplate.allCases.map { $0 }
}

// MARK: - Activity

/// This week, as one closed row until somebody wants the shape of it.
///
/// The fact is always this week's numbers, never the paged week's: the label
/// says "This week", and a summary that changed its numbers as you paged
/// backwards would be describing a week its own label denies. The caption
/// inside says which week the charts are drawing.
struct ActivityBandView: View {
    let store: OverviewStore
    @State private var page = 0

    var body: some View {
        if let trend = store.trend {
            OverviewDisclosure("This week", fact: ActivityBandView.fact(trend)) {
                ActivityBandBody(trend: trend, meetings: store.meetings, stats: store.stats, page: $page)
            }
            .padding(.bottom, 32)
        }
    }

    /// Page 0 whatever the charts are drawing: the fact belongs to the label,
    /// not to the paged range.
    private static func fact(_ trend: ActivityTrend) -> String {
        let week = activityWindow(trend.points, page: 0).points
        var dictations = 0
        var words = 0
        for point in week {
            dictations += point.recordings
            words += point.words
        }
        return [
            dictations == 1 ? "1 dictation" : "\(dictations) dictations",
            words == 1 ? "1 word" : "\(words) words",
            "\(trend.currentStreakDays)-day streak",
        ].joined(separator: " · ")
    }
}

private struct ActivityBandBody: View {
    let trend: ActivityTrend
    let meetings: ActivityMeetingTrend?
    let stats: HistoryStats?
    @Binding var page: Int

    var body: some View {
        let window = activityWindow(trend.points, page: page)
        let points = Array(window.points)
        let labels = points.map { ActivityDates.narrowWeekday($0.localDate) }

        VStack(alignment: .leading, spacing: 20) {
            ActivityPager(
                caption: ActivityDates.range(points),
                canGoBack: window.start > 0,
                canGoForward: window.page > 0,
                back: { page += 1 },
                forward: { page = max(0, page - 1) }
            )
            HStack(alignment: .top, spacing: 24) {
                ActivityMeasure("Dictations") {
                    ActivityBars(values: points.map(\.recordings), labels: labels)
                }
                ActivityMeasure("Words") {
                    ActivitySparkline(values: points.map(\.words), labels: labels)
                }
                ActivityMeasure("Streak") {
                    ActivityWeekDots(points: points, labels: labels)
                }
                if let counts = meetingCounts(points) {
                    ActivityMeasure("Meetings") {
                        ActivityBars(values: counts, labels: labels)
                    }
                }
            }
            if let stats {
                Text(ActivityBandBody.allTime(stats)).metaText()
            }
        }
        .padding(20)
    }

    /// The meeting trend joined onto the days the dictation charts are
    /// drawing. Storage that cannot answer draws no column at all, rather
    /// than a week of zeroes it never measured.
    private func meetingCounts(_ points: [ActivityTrendPoint]) -> [Int]? {
        guard case let .available(days) = meetings else { return nil }
        var byDate: [String: Int] = [:]
        byDate.reserveCapacity(days.count)
        for day in days {
            byDate[day.localDate] = day.meetings
        }
        return points.map { byDate[$0.localDate] ?? 0 }
    }

    private static func allTime(_ stats: HistoryStats) -> String {
        let dictations = stats.entries == 1 ? "1 dictation" : "\(stats.entries) dictations"
        let spoken = TimeInterval(stats.totalDurationMs) / 1000
        return "All time: \(dictations) · \(spoken.spoken) spoken · \(stats.totalWords) words"
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
                    RoundedRectangle(cornerRadius: 3, style: .continuous)
                        .fill(value == 0 ? Theme.selection : Theme.accent)
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
                        .fill((point?.recordings ?? 0) > 0 ? Theme.accent : Theme.selection)
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

// MARK: - Recent

/// What Sona did without being asked, closed by default.
///
/// The fact counts today's passes, so a reader knows whether opening this says
/// anything new: "0 today" over three older rows is the answer as often as
/// "3 today" is.
struct FeedRecentView: View {
    let store: OverviewStore
    var openMeeting: (String) -> Void = { _ in }

    var body: some View {
        OverviewDisclosure("Recent", fact: fact) {
            switch store.receipts {
            case .loading:
                CardRow { Text("Loading…").metaText() }
            case .failed:
                FeedFailedRow(retry: store.refreshFeed)
            case .loaded:
                if entries.isEmpty {
                    CardRow {
                        Text("People, words and follow-ups Sona files after a meeting show up here.").metaText()
                    }
                } else {
                    ForEach(entries) { receipt in
                        FeedReceiptRow(receipt: receipt, openMeeting: openMeeting)
                    }
                }
            }
        }
    }

    /// The list is effects, so a receipt with none does not reach it: the same
    /// rule the loader pages by, applied where the row is drawn.
    private var entries: [FeedReceipt] {
        guard case let .loaded(receipts) = store.receipts else { return [] }
        return Array(receipts.filter(\.hasEffect).prefix(3))
    }

    private var fact: String? {
        guard case .loaded = store.receipts else { return nil }
        if entries.isEmpty { return "Nothing yet" }
        return "\(entries.filter { FeedClock.isToday($0.finishedAtUtcMs) }.count) today"
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
                Text(receipt.sentence).bodyText(14)
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

// MARK: - Shared

/// A closed row that carries the measurement deciding whether to open it.
struct OverviewDisclosure<Content: View>: View {
    private let label: String
    private let fact: String?
    private let content: Content
    @State private var open = false

    init(_ label: String, fact: String? = nil, @ViewBuilder content: () -> Content) {
        self.label = label
        self.fact = fact
        self.content = content()
    }

    var body: some View {
        Card {
            CardRow(action: { open.toggle() }) {
                HStack(spacing: 10) {
                    Image(systemName: "chevron.right")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(Theme.inkTertiary)
                        .rotationEffect(.degrees(open ? 90 : 0))
                    Text(label).bodyText()
                }
            } trailing: {
                if let fact {
                    Text(fact).metaText()
                }
            }
            if open {
                content
            }
        }
    }
}

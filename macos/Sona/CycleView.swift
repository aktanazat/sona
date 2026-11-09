import Charts
import SwiftUI

enum CycleFormat {
    private static let parser: DateFormatter = {
        let formatter = DateFormatter()
        formatter.calendar = Calendar(identifier: .gregorian)
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = .current
        formatter.dateFormat = "yyyy-MM-dd"
        return formatter
    }()

    /// A local `YYYY-MM-DD` day at its midnight.
    static func day(_ text: String) -> Date? { parser.date(from: text) }

    /// The cycle's first and last day.
    static func span(_ cycle: WorkCycle) -> (first: Date, last: Date)? {
        guard let first = day(cycle.startDay), let end = day(cycle.endDay),
              let last = Calendar.current.date(byAdding: .day, value: -1, to: end) else { return nil }
        return (first, last)
    }

    /// "Nov 3 – 16", or "Nov 24 – Dec 7" across a month.
    static func title(_ cycle: WorkCycle) -> String {
        guard let (first, last) = span(cycle) else { return "" }
        return (first..<last).formatted(.interval.month(.abbreviated).day())
    }

    static func weekday(_ number: Int) -> String {
        let names = Calendar(identifier: .gregorian).weekdaySymbols
        return names[number % 7]
    }
}

/// What the tasks in a cycle count toward: the scope planned in it and not
/// dropped, and how much of it is done.
struct CycleTally: Equatable {
    var scope = 0
    var done = 0

    init(_ rows: [WorkItemRow]) {
        for row in rows where row.archivedAtMs == nil {
            switch row.status {
            case .canceled, .duplicate, .triage: break
            case .done:
                scope += 1
                done += 1
            case .backlog, .notStarted, .inProgress: scope += 1
            }
        }
    }
}

/// The head of the cycle tab: which cycle, how far it has come, and scope
/// against done for each day so far.
struct CycleSummary: View {
    let cycle: WorkCycle
    let tally: CycleTally
    @Environment(AppModel.self) private var model
    @State private var progress: WorkCycleProgress?
    @State private var failed = false

    var body: some View {
        let now = model.work.now
        VStack(alignment: .leading, spacing: 14) {
            HStack(alignment: .firstTextBaseline, spacing: 16) {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Cycle \(cycle.number)").headlineText()
                    Text(subtitle(now: now)).metaText()
                }
                Spacer(minLength: 0)
                VStack(alignment: .trailing, spacing: 4) {
                    Text(tally.scope == 0 ? "No work yet" : "\(tally.done * 100 / tally.scope)%")
                        .font(tally.scope == 0 ? TypeScale.list : TypeScale.stat)
                        .monospacedDigit()
                        .foregroundStyle(tally.scope == 0 ? Theme.inkSecondary : Theme.ink)
                    Text("\(tally.done) of \(tally.scope) done").metaText()
                }
            }
            if let (first, last) = CycleFormat.span(cycle) {
                CycleChart(first: first, last: last, points: points(now: now))
            }
            if failed {
                Text("Earlier days could not be read.").metaText(Theme.live)
            }
        }
        .padding(.horizontal, 24)
        .padding(.top, 18)
        .padding(.bottom, 16)
        .overlay(alignment: .bottom) { Hairline() }
        .task(id: Reload(number: cycle.number, tally: tally, day: now / 86_400_000)) {
            do {
                progress = try await model.work.progress(of: cycle.number)
                failed = false
            } catch {
                failed = true
            }
        }
    }

    private struct Reload: Equatable {
        let number: Int64
        let tally: CycleTally
        let day: Int64
    }

    private func subtitle(now: Int64) -> String {
        var parts = [CycleFormat.title(cycle)]
        if let end = CycleFormat.day(cycle.endDay) {
            let today = Calendar.current.startOfDay(for: PlanningFormat.moment(now))
            let left = Calendar.current.dateComponents([.day], from: today, to: end).day ?? 0
            parts.append(left == 1 ? "Last day" : "\(left) days left")
        }
        if let carried = progress?.carriedOver, carried > 0 {
            parts.append("\(carried) carried over")
        }
        return parts.joined(separator: " · ")
    }

    /// Where scope and done stood at each day's end, through today's from
    /// the rows on screen, so a change shows the moment it is made. Each
    /// count is drawn across the day it closes.
    private func points(now: Int64) -> [CycleChart.Point] {
        let calendar = Calendar.current
        let moment = PlanningFormat.moment(now)
        let today = calendar.startOfDay(for: moment)
        var points = (progress?.days ?? []).compactMap { day -> CycleChart.Point? in
            guard let date = CycleFormat.day(day.day), date < today,
                  let end = calendar.date(byAdding: .day, value: 1, to: date) else { return nil }
            return CycleChart.Point(at: end, scope: day.scope, done: day.done)
        }
        if cycle.state == .current {
            points.append(CycleChart.Point(at: moment, scope: tally.scope, done: tally.done))
        }
        if let first = points.first, let start = CycleFormat.day(cycle.startDay) {
            points.insert(CycleChart.Point(at: start, scope: first.scope, done: first.done), at: 0)
        }
        return points
    }
}

/// Scope as a dashed step, done as a filled step under it, over every day
/// of the cycle, so the days still to come read as room left.
struct CycleChart: View {
    struct Point: Identifiable {
        let at: Date
        let scope: Int
        let done: Int
        var id: Date { at }
    }

    let first: Date
    let last: Date
    let points: [Point]

    var body: some View {
        let peak = max(points.map(\.scope).max() ?? 0, 1)
        let end = Calendar.current.date(byAdding: .day, value: 1, to: last) ?? last
        VStack(spacing: 4) {
            Chart {
                ForEach(points) { point in
                    AreaMark(x: .value("Day", point.at), y: .value("Done", point.done))
                        .foregroundStyle(Theme.accentSoft)
                        .interpolationMethod(.stepStart)
                    LineMark(x: .value("Day", point.at), y: .value("Done", point.done),
                             series: .value("Line", "Done"))
                        .foregroundStyle(Theme.accent)
                        .lineStyle(StrokeStyle(lineWidth: 2))
                        .interpolationMethod(.stepStart)
                    LineMark(x: .value("Day", point.at), y: .value("Scope", point.scope),
                             series: .value("Line", "Scope"))
                        .foregroundStyle(Theme.inkTertiary)
                        .lineStyle(StrokeStyle(lineWidth: 1.5, dash: [3, 3]))
                        .interpolationMethod(.stepStart)
                }
            }
            .chartXScale(domain: first...end)
            .chartYScale(domain: 0...Double(peak))
            .chartXAxis(.hidden)
            .chartYAxis(.hidden)
            .chartLegend(.hidden)
            .frame(height: 88)
            .padding(.vertical, 4)
            .background(alignment: .bottom) { Hairline() }
            HStack {
                Text(first.formatted(.dateTime.month(.abbreviated).day()))
                Spacer(minLength: 8)
                Text(last.formatted(.dateTime.month(.abbreviated).day()))
            }
            .font(.system(size: 10, weight: .medium))
            .foregroundStyle(Theme.inkTertiary)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(points.last.map { "Scope \($0.scope), done \($0.done)" } ?? "No days counted yet")
    }
}

/// How long cycles run and the weekday they start; a change applies from
/// the next start day, and the current cycle ends then.
struct CycleSettingsMenu: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let store = model.work
        if let settings = store.cycleSettings {
            Menu {
                Picker("Length", selection: Binding(
                    get: { settings.weeks },
                    set: { store.saveCycleSettings(WorkCycleSettings(weeks: $0, startWeekday: settings.startWeekday)) }
                )) {
                    Text("1 week").tag(1)
                    Text("2 weeks").tag(2)
                }
                Picker("Starts on", selection: Binding(
                    get: { settings.startWeekday },
                    set: { store.saveCycleSettings(WorkCycleSettings(weeks: settings.weeks, startWeekday: $0)) }
                )) {
                    ForEach(1...7, id: \.self) { weekday in
                        Text(CycleFormat.weekday(weekday)).tag(weekday)
                    }
                }
            } label: {
                Text("\(settings.weeks == 1 ? "1 week" : "2 weeks") from \(CycleFormat.weekday(settings.startWeekday))")
            }
            .menuStyle(.button)
            .buttonStyle(.compact)
            .menuIndicator(.hidden)
            .fixedSize()
            .help("Cycle length and start day")
        }
    }
}

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

    static func weeks(_ count: Int) -> String {
        count == 1 ? "1 week" : "\(count) weeks"
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
    var started = 0
    var done = 0

    init(_ rows: [WorkItemRow]) {
        for row in rows where row.archivedAtMs == nil {
            switch row.status {
            case .canceled, .duplicate, .triage: break
            case .done:
                scope += 1
                done += 1
            case .inProgress:
                scope += 1
                started += 1
            case .backlog, .notStarted: scope += 1
            }
        }
    }

    /// Done counts 100 and started 25, over scope: the rule the core closes a cycle by.
    var success: Int? { scope == 0 ? nil : (done * 100 + started * 25) / scope }
}

/// The head of the cycle tab: which cycle, how far it has come, and scope
/// against done for each day so far.
struct CycleSummary: View {
    let cycle: WorkCycle
    let tally: CycleTally
    @Environment(AppModel.self) private var model
    @State private var progress: WorkCycleProgress?
    @State private var failed = false
    @State private var renaming: String?

    var body: some View {
        let now = model.work.now
        let success = cycle.success ?? tally.success
        VStack(alignment: .leading, spacing: 14) {
            HStack(alignment: .firstTextBaseline, spacing: 16) {
                VStack(alignment: .leading, spacing: 4) {
                    Button { renaming = cycle.name ?? "" } label: {
                        Text(cycle.title).headlineText()
                    }
                    .buttonStyle(.plain)
                    .help("Rename cycle")
                    .accessibilityLabel("\(cycle.title). Rename")
                    .popover(isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })) {
                        renameForm
                    }
                    Text(subtitle(now: now)).metaText()
                }
                Spacer(minLength: 0)
                actions
                VStack(alignment: .trailing, spacing: 4) {
                    Text(success.map { "\($0)%" } ?? "No work yet")
                        .font(success == nil ? TypeScale.list : TypeScale.stat)
                        .monospacedDigit()
                        .foregroundStyle(success == nil ? Theme.inkSecondary : Theme.ink)
                        .help("Done counts in full, started counts a quarter")
                    Text("\(tally.done) done, \(tally.started) started of \(tally.scope)").metaText()
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

    private var renameForm: some View {
        VStack(alignment: .leading, spacing: 8) {
            TextField("Cycle \(cycle.number)", text: Binding(get: { renaming ?? "" }, set: { renaming = $0 }))
                .textFieldStyle(.roundedBorder)
                .frame(width: 220)
                .onSubmit(rename)
            HStack {
                Text("Leave empty to use the number.").metaText()
                Spacer()
                Button("Save", action: rename).buttonStyle(.primary).keyboardShortcut(.defaultAction)
            }
        }
        .padding(12)
    }

    private func rename() {
        model.work.renameCycle(cycle.number, to: renaming ?? "")
        renaming = nil
    }

    @ViewBuilder
    private var actions: some View {
        if cycle.state == .current {
            Button("Roll over", systemImage: "arrow.uturn.right") { model.work.rolloverCycle(cycle.number) }
                .buttonStyle(.secondary)
                .help("Move unfinished work into the next cycle")
            if let next = model.work.nextCycle {
                Button("Start \(next.title) today") { model.work.startCycleToday(next.number) }
                    .buttonStyle(.secondary)
                    .help("End this cycle now and start the next one today")
            }
        } else if cycle.state == .upcoming {
            Button("Start today") { model.work.startCycleToday(cycle.number) }
                .buttonStyle(.secondary)
        }
    }

    /// Where scope, started and done stood at each day's end, from the
    /// frozen counts once the cycle closed, and through today's from the rows
    /// on screen while it runs. Each count is drawn across the day it closes.
    private func points(now: Int64) -> [CycleChart.Point] {
        let calendar = Calendar.current
        let moment = PlanningFormat.moment(now)
        let today = calendar.startOfDay(for: moment)
        var points = (progress?.days ?? []).compactMap { day -> CycleChart.Point? in
            guard let date = CycleFormat.day(day.day), cycle.state == .past || date < today,
                  let end = calendar.date(byAdding: .day, value: 1, to: date) else { return nil }
            return CycleChart.Point(at: end, scope: day.scope, started: day.started, done: day.done)
        }
        if cycle.state == .current {
            points.append(CycleChart.Point(at: moment, scope: tally.scope, started: tally.started, done: tally.done))
        }
        if let first = points.first, let start = CycleFormat.day(cycle.startDay) {
            points.insert(CycleChart.Point(at: start, scope: first.scope, started: first.started, done: first.done), at: 0)
        }
        return points
    }
}

/// Scope as a dashed step, started and done as steps under it, over every
/// day of the cycle, so the days still to come read as room left.
struct CycleChart: View {
    struct Point: Identifiable {
        let at: Date
        let scope: Int
        let started: Int
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
                    LineMark(x: .value("Day", point.at), y: .value("Started", point.started + point.done),
                             series: .value("Line", "Started"))
                        .foregroundStyle(Theme.statusInProgress)
                        .lineStyle(StrokeStyle(lineWidth: 1.5))
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
        .accessibilityLabel(points.last.map { "Scope \($0.scope), started \($0.started), done \($0.done)" }
            ?? "No days counted yet")
    }
}

/// Opens the cycle settings: length, cooldown, start day and how many
/// cycles are kept ahead.
struct CycleSettingsMenu: View {
    @Environment(AppModel.self) private var model
    @State private var editing: WorkCycleSettings?

    var body: some View {
        if let settings = model.work.cycleSettings {
            Button { editing = settings } label: {
                Text("\(CycleFormat.weeks(settings.weeks)) from \(CycleFormat.weekday(settings.startWeekday))")
            }
            .buttonStyle(.compact)
            .fixedSize()
            .help("Cycle settings")
            .sheet(item: Binding(get: { editing.map(CycleSettingsDraft.init) }, set: { editing = $0?.settings })) { draft in
                CycleSettingsSheet(settings: draft.settings) { saved in
                    model.work.saveCycleSettings(saved)
                    editing = nil
                } cancel: { editing = nil }
            }
        }
    }
}

private struct CycleSettingsDraft: Identifiable {
    let settings: WorkCycleSettings
    var id: Int { 0 }
}

private struct CycleSettingsSheet: View {
    @State var settings: WorkCycleSettings
    let save: (WorkCycleSettings) -> Void
    let cancel: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Cycles").headlineText()
            Form {
                Stepper(value: $settings.weeks, in: 1...8) {
                    LabeledContent("Length", value: CycleFormat.weeks(settings.weeks))
                }
                Stepper(value: $settings.cooldownWeeks, in: 0...4) {
                    LabeledContent("Cooldown",
                                   value: settings.cooldownWeeks == 0 ? "None" : CycleFormat.weeks(settings.cooldownWeeks))
                }
                Picker("Starts on", selection: $settings.startWeekday) {
                    ForEach(1...7, id: \.self) { weekday in
                        Text(CycleFormat.weekday(weekday)).tag(weekday)
                    }
                }
                Stepper(value: $settings.upcoming, in: 1...15) {
                    LabeledContent("Planned ahead", value: "\(settings.upcoming)")
                }
            }
            .formStyle(.grouped)
            Text("Changes apply from the next start day. The cycle under way keeps its dates.").metaText()
            HStack {
                Spacer()
                Button("Cancel", action: cancel).buttonStyle(.secondary).keyboardShortcut(.cancelAction)
                Button("Save") { save(settings) }.buttonStyle(.primary).keyboardShortcut(.defaultAction)
            }
        }
        .padding(20)
        .frame(width: 380)
    }
}

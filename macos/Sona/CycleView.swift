import Charts
import SwiftUI
import UniformTypeIdentifiers

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

    /// A day as the core spells it, `YYYY-MM-DD`.
    static func dayText(_ date: Date) -> String { parser.string(from: date) }

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

    /// How far off the cycle is: days left in it, days until it starts, or
    /// that it ended.
    static func when(_ cycle: WorkCycle, now: Int64) -> String {
        let today = Calendar.current.startOfDay(for: PlanningFormat.moment(now))
        func days(to text: String) -> Int {
            day(text).flatMap { Calendar.current.dateComponents([.day], from: today, to: $0).day } ?? 0
        }
        switch cycle.state {
        case .past: return "Ended"
        case .current:
            let left = days(to: cycle.endDay)
            return left == 1 ? "Last day" : "\(left) days left"
        case .upcoming:
            let away = days(to: cycle.startDay)
            return away == 1 ? "Starts tomorrow" : "Starts in \(away) days"
        }
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

    /// The counts the core sent with the cycle.
    init(_ cycle: WorkCycle) {
        scope = cycle.scope
        started = cycle.started
        done = cycle.done
    }

    /// Done over scope: the rule the core closes a cycle by.
    var success: Int? { scope == 0 ? nil : done * 100 / scope }
}

/// The head of a cycle's page: which cycle, how far it has come, and scope
/// against done for each day so far.
struct CycleSummary: View {
    let cycle: WorkCycle
    let tally: CycleTally
    @Environment(AppModel.self) private var model
    @State private var progress: WorkCycleProgress?
    @State private var failed = false
    @State private var renaming: String?
    @State private var dating: Dates?
    @State private var notes: [ScratchNote] = []

    private struct Dates {
        var first: Date
        var last: Date
    }

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
                if cycle.state == .upcoming, let capacity = model.work.cycleCapacity {
                    CycleCapacity(scope: tally.scope, capacity: capacity)
                }
                VStack(alignment: .trailing, spacing: 4) {
                    Text(success.map { "\($0)%" } ?? "No work yet")
                        .font(success == nil ? TypeScale.list : TypeScale.stat)
                        .monospacedDigit()
                        .foregroundStyle(success == nil ? Theme.inkSecondary : Theme.ink)
                    Text("\(tally.done) done, \(tally.started) started of \(tally.scope)").metaText()
                }
            }
            if let chart = CycleChart(cycle: cycle, progress: progress, live: tally, now: now) {
                chart
            }
            if failed {
                Text("Earlier days could not be read.").metaText(Theme.live)
            }
            if !notes.isEmpty {
                HStack(spacing: 8) {
                    Text("Notes").metaText()
                    ForEach(notes) { note in
                        Button(note.title) { model.open(link: "sona://note/\(note.id)") }
                            .buttonStyle(.secondary)
                    }
                }
            }
        }
        .task(id: cycle.number) {
            notes = (try? await model.scratchpad.cycleNotes(cycle.number)) ?? []
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
        var parts = [CycleFormat.title(cycle), CycleFormat.when(cycle, now: now)]
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

    private var actions: some View {
        Menu {
            if cycle.state != .past {
                Button("Change dates…") {
                    dating = CycleFormat.span(cycle).map { Dates(first: $0.first, last: $0.last) }
                }
            }
            if cycle.state == .current {
                Button("Move unfinished work to the next cycle") { model.work.rolloverCycle(cycle.number) }
                if let next = model.work.nextCycle {
                    Button("End this cycle and start \(next.title) today") { model.work.startCycleToday(next.number) }
                }
            } else if cycle.state == .upcoming {
                Button("Start today") { model.work.startCycleToday(cycle.number) }
            }
            Button("New note") {
                model.newNote(aboutCycle: cycle.number, titled: "Notes for \(cycle.title)")
            }
        } label: {
            Image(systemName: "ellipsis")
        }
        .menuStyle(.button)
        .buttonStyle(.icon)
        .menuIndicator(.hidden)
        .fixedSize()
        .help("More")
        .accessibilityLabel("More")
        .popover(isPresented: Binding(get: { dating != nil }, set: { if !$0 { dating = nil } })) {
            datesForm
        }
    }

    private var datesForm: some View {
        let today = Calendar.current.startOfDay(for: PlanningFormat.moment(model.work.now))
        let first = Binding(get: { dating?.first ?? today }, set: { dating?.first = $0 })
        let last = Binding(get: { dating?.last ?? today }, set: { dating?.last = $0 })
        return VStack(alignment: .leading, spacing: 10) {
            if cycle.state == .upcoming {
                DatePicker("First day", selection: first, in: today..., displayedComponents: .date)
            }
            DatePicker("Last day", selection: last, in: max(today, first.wrappedValue)..., displayedComponents: .date)
            Text(cycle.state == .current
                 ? "A cycle under way keeps its first day."
                 : "A cycle can't run into the ones beside it.").metaText()
            HStack {
                Spacer()
                Button("Save", action: moveDates).buttonStyle(.primary).keyboardShortcut(.defaultAction)
            }
        }
        .padding(12)
        .frame(width: 260)
    }

    private func moveDates() {
        guard let dating else { return }
        model.work.moveCycle(cycle.number, firstDay: cycle.state == .upcoming ? dating.first : nil, lastDay: dating.last)
        self.dating = nil
    }
}

/// Scope as a dashed step, started and done as steps under it, the tasks
/// finished each day as bars, and the pace that would finish the scope as
/// a dotted line that holds over weekends, across every day of the cycle,
/// so the days still to come read as room left.
struct CycleChart: View {
    struct Point: Identifiable {
        let at: Date
        let scope: Int
        let started: Int
        let done: Int
        var id: Date { at }
    }

    struct Bar: Identifiable {
        let day: Date
        let done: Int
        var id: Date { day }
    }

    struct Pace: Identifiable {
        let at: Date
        let done: Double
        var id: Date { at }
    }

    let first: Date
    let last: Date
    let points: [Point]
    let bars: [Bar]
    let pace: [Pace]

    /// Where scope, started and done stood at each day's end, from the
    /// frozen counts once the cycle closed, and through today's from `live`
    /// while it runs. Each count is drawn across the day it closes.
    init?(cycle: WorkCycle, progress: WorkCycleProgress?, live: CycleTally, now: Int64) {
        guard let (first, last) = CycleFormat.span(cycle) else { return nil }
        let calendar = Calendar.current
        let moment = PlanningFormat.moment(now)
        let today = calendar.startOfDay(for: moment)
        let closed = (progress?.days ?? []).compactMap { day -> (date: Date, counts: WorkCycleDay)? in
            guard let date = CycleFormat.day(day.day), cycle.state == .past || date < today else { return nil }
            return (date, day)
        }
        var points: [Point] = []
        var bars: [Bar] = []
        var before = 0
        for (date, counts) in closed {
            if let end = calendar.date(byAdding: .day, value: 1, to: date) {
                points.append(Point(at: end, scope: counts.scope, started: counts.started, done: counts.done))
            }
            bars.append(Bar(day: date, done: max(counts.done - before, 0)))
            before = counts.done
        }
        if cycle.state == .current {
            points.append(Point(at: moment, scope: live.scope, started: live.started, done: live.done))
            bars.append(Bar(day: today, done: max(live.done - before, 0)))
        }
        if let opening = points.first {
            points.insert(Point(at: first, scope: opening.scope, started: opening.started, done: opening.done), at: 0)
        }
        let pace = (progress?.target ?? []).compactMap { target -> Pace? in
            CycleFormat.day(target.day)
                .flatMap { calendar.date(byAdding: .day, value: 1, to: $0) }
                .map { Pace(at: $0, done: target.done) }
        }
        self.first = first
        self.last = last
        self.points = points
        self.bars = bars.filter { $0.done > 0 }
        self.pace = pace.isEmpty ? [] : [Pace(at: first, done: 0)] + pace
    }

    var body: some View {
        let peak = max(points.map(\.scope).max() ?? 0, Int((pace.last?.done ?? 0).rounded(.up)), 1)
        let end = Calendar.current.date(byAdding: .day, value: 1, to: last) ?? last
        VStack(spacing: 4) {
            Chart {
                ForEach(bars) { bar in
                    BarMark(x: .value("Day", bar.day, unit: .day), y: .value("Finished", bar.done), width: .ratio(0.5))
                        .foregroundStyle(Theme.accent.opacity(0.45))
                }
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
                ForEach(pace) { pace in
                    LineMark(x: .value("Day", pace.at), y: .value("Target", pace.done),
                             series: .value("Line", "Target"))
                        .foregroundStyle(Theme.inkSecondary)
                        .lineStyle(StrokeStyle(lineWidth: 1.5, lineCap: .round, dash: [0.5, 3.5]))
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
            HStack(spacing: 14) {
                Text(first.formatted(.dateTime.month(.abbreviated).day()))
                Spacer(minLength: 8)
                ChartKey(title: "Scope", color: Theme.inkTertiary, dash: [3, 2])
                ChartKey(title: "Started", color: Theme.statusInProgress)
                ChartKey(title: "Done", color: Theme.accent)
                if !pace.isEmpty { ChartKey(title: "Target", color: Theme.inkSecondary, dash: [1, 3]) }
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

/// Scope planned in a cycle against what a cycle usually finishes, the
/// mean done of the last three that ended; a full dial in red means more
/// is planned than usually gets done.
struct CycleCapacity: View {
    let scope: Int
    let capacity: Int

    var body: some View {
        let over = scope > capacity
        HStack(spacing: 6) {
            Gauge(value: Double(min(scope, capacity)), in: 0...Double(max(capacity, 1))) {
                Text("Capacity")
            } currentValueLabel: {
                Text("\(scope)").monospacedDigit()
            }
            .gaugeStyle(.accessoryCircularCapacity)
            .tint(over ? Theme.live : Theme.accent)
            .scaleEffect(0.55)
            .frame(width: 34, height: 34)
            Text("of \(capacity) usual").metaText(over ? Theme.live : Theme.inkSecondary)
        }
        .help("Planned \(scope); the last three cycles finished \(capacity) on average")
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(scope) planned, \(capacity) usually finished")
    }
}

/// Opens the cycle settings: length, cooldown, start day and how many
/// cycles are kept ahead.
struct CycleSettingsMenu: View {
    @Environment(AppModel.self) private var model
    @State private var editing: WorkCycleSettings?

    var body: some View {
        if let settings = model.work.cycleSettings {
            Button("Cycle settings") { editing = settings }
                .buttonStyle(.compact)
                .fixedSize()
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
                Toggle("Add started work", isOn: $settings.autoAddStarted)
                Toggle("Add finished work", isOn: $settings.autoAddCompleted)
            }
            .formStyle(.grouped)
            Text("Changes apply from the next start day. The cycle under way keeps its dates. Work with no cycle joins the one under way when it starts or finishes, as set above.").metaText()
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

/// The Cycle tab's way from one cycle's page to every cycle, and from the
/// list to a calendar file.
struct CycleNavigation: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        if model.cycleShown == nil {
            Button("Export calendar…", systemImage: "calendar.badge.plus", action: export)
                .buttonStyle(.compact)
                .fixedSize()
                .help("Save every cycle as a calendar file")
        } else {
            Button("All cycles", systemImage: "list.bullet") { CycleList.show(model) }
                .buttonStyle(.compact)
                .fixedSize()
                .help("Every cycle, past and ahead  Esc")
        }
    }

    private func export() {
        let panel = NSSavePanel()
        panel.nameFieldStringValue = "Sona cycles.ics"
        panel.allowedContentTypes = [.calendarEvent]
        guard panel.runModal() == .OK, let url = panel.url else { return }
        Task {
            do {
                try await model.work.cycleCalendar().write(to: url, atomically: true, encoding: .utf8)
                model.workFocus.announce("Saved \(model.work.cycles.count) cycles to \(url.lastPathComponent)", undoes: false)
            } catch {
                model.workFocus.announce("Couldn't save the calendar: \(error.localizedDescription)", undoes: false)
            }
        }
    }
}

/// Every cycle on one page: the one under way, the ones ahead, then the
/// ones behind, newest first. A click or Return opens a cycle's page;
/// Space peeks at the highlighted one.
struct CycleList: View {
    @Environment(AppModel.self) private var model

    /// The cycles in the order the list shows them, which J and K follow.
    static func ordered(_ cycles: [WorkCycle]) -> [WorkCycle] {
        cycles.filter { $0.state == .current }
            + cycles.filter { $0.state == .upcoming }.sorted { $0.startsAtMs < $1.startsAtMs }
            + cycles.filter { $0.state == .past }.sorted { $0.startsAtMs > $1.startsAtMs }
    }

    static func open(_ number: Int64, _ model: AppModel) {
        let focus = model.workFocus
        focus.peeking = false
        focus.cycleOpened = number
        focus.cycleHighlighted = number
        focus.cycleListShown = false
    }

    static func show(_ model: AppModel) {
        let focus = model.workFocus
        focus.peeking = false
        focus.clearSelection()
        focus.cycleHighlighted = model.cycleShown?.number ?? focus.cycleHighlighted
        focus.cycleListShown = true
    }

    var body: some View {
        let focus = model.workFocus
        let ordered = Self.ordered(model.work.cycles)
        ScrollViewReader { proxy in
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach([WorkCycleState.current, .upcoming, .past], id: \.self) { state in
                        let group = ordered.filter { $0.state == state }
                        if !group.isEmpty {
                            Text(Self.heading(state))
                                .metaText()
                                .padding(.horizontal, 24)
                                .padding(.top, 18)
                                .padding(.bottom, 6)
                            ForEach(group) { cycle in
                                CycleRow(cycle: cycle, highlighted: focus.cycleHighlighted == cycle.number)
                                    .id(cycle.number)
                            }
                        }
                    }
                }
                .padding(.bottom, 24)
            }
            .onChange(of: focus.cycleHighlighted) { _, number in
                if let number { proxy.scrollTo(number) }
            }
        }
        .onAppear { focus.show([]) }
    }

    private static func heading(_ state: WorkCycleState) -> String {
        switch state {
        case .current: "Under way"
        case .upcoming: "Ahead"
        case .past: "Ended"
        }
    }
}

private struct CycleRow: View {
    let cycle: WorkCycle
    let highlighted: Bool
    @Environment(AppModel.self) private var model
    @State private var hovering = false

    var body: some View {
        Button { CycleList.open(cycle.number, model) } label: {
            HStack(spacing: 14) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(cycle.title).font(TypeScale.list).foregroundStyle(Theme.ink)
                    Text("\(CycleFormat.title(cycle)) · \(CycleFormat.when(cycle, now: model.work.now))").metaText()
                }
                Spacer(minLength: 12)
                if cycle.state == .upcoming, let capacity = model.work.cycleCapacity {
                    CycleCapacity(scope: cycle.scope, capacity: capacity)
                }
                if let success = cycle.success {
                    Text("\(success)%").font(TypeScale.list).monospacedDigit().foregroundStyle(Theme.ink)
                }
                Text("\(cycle.done) done of \(cycle.scope)")
                    .metaText()
                    .monospacedDigit()
                    .frame(minWidth: 96, alignment: .trailing)
            }
            .padding(.horizontal, 24)
            .frame(maxWidth: .infinity, minHeight: 52, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: Theme.radiusRow)
                    .fill(highlighted ? Theme.washSelected : hovering ? Theme.wash : Color.clear)
                    .padding(.horizontal, 8))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .accessibilityLabel("\(cycle.title), \(CycleFormat.title(cycle)), \(cycle.done) done of \(cycle.scope)")
    }
}

/// Space on the cycle list: the highlighted cycle at a glance beside the
/// list. J and K carry the peek along; Space or Esc closes it and Return
/// opens the cycle's page.
struct CyclePeek: View {
    let cycle: WorkCycle
    @Environment(AppModel.self) private var model
    @State private var progress: WorkCycleProgress?
    @State private var failed = false

    var body: some View {
        let now = model.work.now
        let tally = CycleTally(cycle)
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                Text("Cycle \(cycle.number)")
                    .font(TypeScale.mono(12))
                    .foregroundStyle(Theme.inkTertiary)
                Spacer(minLength: 8)
                TaskKeyButton(title: "Open", keys: "return") { CycleList.open(cycle.number, model) }
                WorkBarButton(icon: "xmark", help: "Close  Space") { model.workFocus.peeking = false }
            }
            .padding(.leading, 20)
            .padding(.trailing, 12)
            .frame(height: Theme.headerHeight)
            Hairline()
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(cycle.title)
                            .font(.system(size: 17, weight: .semibold))
                            .foregroundStyle(Theme.ink)
                        Text("\(CycleFormat.title(cycle)) · \(CycleFormat.when(cycle, now: now))").metaText()
                    }
                    if let chart = CycleChart(cycle: cycle, progress: progress, live: tally, now: now) {
                        chart
                    }
                    if failed {
                        Text("Its days could not be read.").metaText(Theme.live)
                    }
                    VStack(alignment: .leading, spacing: 8) {
                        LabeledContent("Scope", value: "\(tally.scope)")
                        LabeledContent("Started", value: "\(tally.started)")
                        LabeledContent("Done", value: "\(tally.done)")
                        if let success = cycle.success ?? tally.success {
                            LabeledContent("Success", value: "\(success)%")
                        }
                        if let carried = progress?.carriedOver, carried > 0 {
                            LabeledContent("Carried over", value: "\(carried)")
                        }
                    }
                    .font(TypeScale.list)
                    if cycle.state == .upcoming, let capacity = model.work.cycleCapacity {
                        CycleCapacity(scope: cycle.scope, capacity: capacity)
                    }
                }
                .padding(20)
            }
        }
        .frame(width: 420)
        .frame(maxHeight: .infinity)
        .background {
            Rectangle()
                .fill(Theme.elevated)
                .overlay(alignment: .leading) { Rectangle().fill(Theme.border).frame(width: 1) }
                .elevation(.high)
        }
        .task(id: cycle.number) {
            do {
                progress = try await model.work.progress(of: cycle.number)
                failed = false
            } catch {
                progress = nil
                failed = true
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Peek at \(cycle.title)")
    }
}

/// Keys on the Cycle tab. In the list J, K and the arrows move, Space
/// peeks, Return opens and Esc closes the peek; on a cycle's page Esc goes
/// back to the list once nothing else on the page is open.
@MainActor
enum CycleKeys {
    static func handle(_ event: NSEvent, key: String, flags: NSEvent.ModifierFlags, _ model: AppModel) -> Bool {
        let focus = model.workFocus
        guard flags.isEmpty else { return false }
        if model.cycleShown != nil {
            guard event.keyCode == 53 else { return false }
            if !focus.escape(in: .tasks) { CycleList.show(model) }
            return true
        }
        let cycles = CycleList.ordered(model.work.cycles)
        let index = focus.cycleHighlighted.flatMap { number in cycles.firstIndex { $0.number == number } }
        func move(_ delta: Int) -> Bool {
            guard !cycles.isEmpty else { return true }
            let next = index.map { min(max($0 + delta, 0), cycles.count - 1) } ?? (delta > 0 ? 0 : cycles.count - 1)
            focus.cycleHighlighted = cycles[next].number
            return true
        }
        if event.keyCode == 53 {
            guard focus.peeking else { return false }
            focus.peeking = false
            return true
        }
        switch event.specialKey {
        case .downArrow?: return move(1)
        case .upArrow?: return move(-1)
        case .carriageReturn?, .enter?:
            guard let index else { return false }
            CycleList.open(cycles[index].number, model)
            return true
        default: break
        }
        switch key {
        case "j": return move(1)
        case "k": return move(-1)
        case " ":
            if focus.peeking {
                focus.peeking = false
                return true
            }
            if index == nil { _ = move(1) }
            focus.peeking = true
            return true
        default: return false
        }
    }
}

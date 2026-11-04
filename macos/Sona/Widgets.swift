import SwiftUI

/// How lists and charts settle when what they show changes.
enum Motion {
    static func settle(reduced: Bool) -> Animation? {
        reduced ? nil : .spring(duration: 0.35, bounce: 0.1)
    }
}

/// A number that leads, with one short label under it: "3 overdue".
struct NumberTile: View {
    let value: String
    let label: String
    var unit: String?
    var tone: Color
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    init(_ value: String, label: String, unit: String? = nil, tone: Color = Theme.ink) {
        self.value = value
        self.label = label
        self.unit = unit
        self.tone = tone
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(alignment: .firstTextBaseline, spacing: 3) {
                Text(value)
                    .font(.system(size: 22, weight: .semibold))
                    .foregroundStyle(tone)
                    .monospacedDigit()
                    .contentTransition(reduceMotion ? .opacity : .numericText())
                if let unit {
                    Text(unit).font(TypeScale.body(12)).foregroundStyle(Theme.inkTertiary)
                }
            }
            .lineLimit(1)
            .animation(Motion.settle(reduced: reduceMotion), value: value)
            Text(label).metaText(Theme.inkSecondary).lineLimit(1)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusCard))
        .overlay(RoundedRectangle(cornerRadius: Theme.radiusCard).strokeBorder(Theme.border, lineWidth: 1))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel([value, unit, label].compactMap { $0 }.joined(separator: " "))
    }
}

/// A Home widget: what it counts, the number, one line under it, a small
/// picture of it, and the whole tile opens the place it counts.
struct WidgetTile<Visual: View>: View {
    static var height: CGFloat { 150 }

    let title: String
    let icon: String
    let value: String
    var unit: String?
    var caption: String?
    var tone: Color
    let action: (() -> Void)?
    let visual: Visual
    @State private var hovering = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    init(_ title: String, icon: String, value: String, unit: String? = nil, caption: String? = nil,
         tone: Color = Theme.ink, action: (() -> Void)?, @ViewBuilder visual: () -> Visual) {
        self.title = title
        self.icon = icon
        self.value = value
        self.unit = unit
        self.caption = caption
        self.tone = tone
        self.action = action
        self.visual = visual()
    }

    var body: some View {
        if let action {
            Button(action: action) { tile }
                .buttonStyle(.plain)
                .onHover { hovering = $0 }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(spoken)
                .accessibilityAddTraits(.isButton)
                .accessibilityAction { action() }
        } else {
            tile
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(spoken)
        }
    }

    private var spoken: String {
        [title, [value, unit].compactMap { $0 }.joined(separator: " "), caption].compactMap { $0 }.joined(separator: ", ")
    }

    private var tile: some View {
        let shape = RoundedRectangle(cornerRadius: Theme.radiusCard)
        return VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 6) {
                Image(systemName: icon)
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(Theme.inkTertiary)
                    .frame(width: 14)
                Text(title).font(TypeScale.label(12)).foregroundStyle(Theme.inkSecondary).lineLimit(1)
                Spacer(minLength: 4)
                if action != nil {
                    Image(systemName: "arrow.up.right")
                        .font(.system(size: 9, weight: .bold))
                        .foregroundStyle(Theme.inkTertiary)
                        .opacity(hovering ? 1 : 0)
                }
            }
            HStack(alignment: .firstTextBaseline, spacing: 4) {
                Text(value)
                    .font(.system(size: 26, weight: .semibold))
                    .tracking(-0.3)
                    .foregroundStyle(tone)
                    .monospacedDigit()
                    .contentTransition(reduceMotion ? .opacity : .numericText())
                if let unit {
                    Text(unit).font(TypeScale.body(12)).foregroundStyle(Theme.inkTertiary)
                }
            }
            .lineLimit(1)
            .animation(Motion.settle(reduced: reduceMotion), value: value)
            .padding(.top, 8)
            Text(caption ?? " ")
                .metaText()
                .lineLimit(1)
                .opacity(caption == nil ? 0 : 1)
            Spacer(minLength: 6)
            visual.frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(14)
        .frame(maxWidth: .infinity, minHeight: Self.height, maxHeight: Self.height, alignment: .topLeading)
        .background(Theme.surface, in: shape)
        .overlay(shape.fill(Theme.wash).opacity(hovering ? 1 : 0))
        .overlay(shape.strokeBorder(Theme.border, lineWidth: 1))
        .contentShape(shape)
        .animation(.easeOut(duration: 0.12), value: hovering)
    }
}

// MARK: - Marks

/// A wedge of a circle from twelve o'clock, clockwise.
struct PieShape: Shape {
    var fraction: Double

    var animatableData: Double {
        get { fraction }
        set { fraction = newValue }
    }

    func path(in rect: CGRect) -> Path {
        var path = Path()
        let clamped = min(max(fraction, 0), 1)
        guard clamped > 0 else { return path }
        if clamped >= 1 {
            path.addEllipse(in: rect)
            return path
        }
        let center = CGPoint(x: rect.midX, y: rect.midY)
        path.move(to: center)
        path.addArc(center: center, radius: min(rect.width, rect.height) / 2,
                    startAngle: .degrees(-90), endAngle: .degrees(-90 + 360 * clamped), clockwise: false)
        path.closeSubpath()
        return path
    }
}

struct Diamond: Shape {
    func path(in rect: CGRect) -> Path {
        var path = Path()
        path.move(to: CGPoint(x: rect.midX, y: rect.minY))
        path.addLine(to: CGPoint(x: rect.maxX, y: rect.midY))
        path.addLine(to: CGPoint(x: rect.midX, y: rect.maxY))
        path.addLine(to: CGPoint(x: rect.minX, y: rect.midY))
        path.closeSubpath()
        return path
    }
}

/// A task's state at a glance: an empty ring, a half-filled ring, a check,
/// a cross. Open work past its date draws in red.
struct WorkStatusIcon: View {
    let status: PlanningWorkStatus
    var overdue = false
    var size: CGFloat = 14

    var body: some View {
        ZStack {
            switch status {
            case .notStarted:
                Circle().strokeBorder(overdue ? Theme.live : Theme.inkTertiary, lineWidth: 1.5)
            case .inProgress:
                Circle().strokeBorder(overdue ? Theme.live : Theme.accent, lineWidth: 1.5)
                PieShape(fraction: 0.5)
                    .fill(overdue ? Theme.live : Theme.accent)
                    .padding(size * 0.25)
            case .done:
                Circle().fill(Theme.accent)
                Image(systemName: "checkmark")
                    .font(.system(size: size * 0.5, weight: .heavy))
                    .foregroundStyle(Theme.onAccent)
            case .canceled:
                Circle().fill(Theme.inkDisabled)
                Image(systemName: "xmark")
                    .font(.system(size: size * 0.45, weight: .heavy))
                    .foregroundStyle(Theme.onAccent)
            }
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}

/// A project's state: a ring filling with its done work while active, a
/// pause mark while paused, a check once completed.
struct ProjectStatusIcon: View {
    let status: PlanningProjectStatus
    let progress: Double
    var size: CGFloat = 14

    var body: some View {
        ZStack {
            switch status {
            case .active:
                Circle().strokeBorder(Theme.accent, lineWidth: 1.5)
                PieShape(fraction: progress)
                    .fill(Theme.accent)
                    .padding(size * 0.25)
            case .paused:
                Circle().strokeBorder(Theme.inkSecondary, lineWidth: 1.5)
                Image(systemName: "pause.fill")
                    .font(.system(size: size * 0.4, weight: .bold))
                    .foregroundStyle(Theme.inkSecondary)
            case .completed:
                Circle().fill(Theme.accent)
                Image(systemName: "checkmark")
                    .font(.system(size: size * 0.5, weight: .heavy))
                    .foregroundStyle(Theme.onAccent)
            }
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}

/// A milestone: a diamond, outlined while open, solid once done.
struct MilestoneMark: View {
    let status: PlanningWorkStatus
    var overdue = false
    var size: CGFloat = 12

    var body: some View {
        let tint: Color = switch status {
        case _ where overdue && status.isOpen: Theme.live
        case .notStarted: Theme.inkTertiary
        case .inProgress, .done: Theme.accent
        case .canceled: Theme.inkDisabled
        }
        ZStack {
            if status == .done || status == .canceled {
                Diamond().fill(tint)
            } else {
                Diamond().fill(Theme.surface)
                Diamond().stroke(tint, lineWidth: 1.5).padding(0.75)
                if status == .inProgress {
                    Diamond().fill(tint).padding(size * 0.3)
                }
            }
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}

/// How much is done, as a ring that fills clockwise.
struct ProgressRing: View {
    let fraction: Double
    var size: CGFloat = 16
    var lineWidth: CGFloat = 2.5

    static func percent(_ fraction: Double) -> String {
        "\(Int((min(max(fraction, 0), 1) * 100).rounded()))%"
    }

    var body: some View {
        ZStack {
            Circle().inset(by: lineWidth / 2).stroke(Theme.inkDisabled.opacity(0.45), lineWidth: lineWidth)
            Circle().inset(by: lineWidth / 2)
                .trim(from: 0, to: min(max(fraction, 0), 1))
                .stroke(Theme.accent, style: StrokeStyle(lineWidth: lineWidth, lineCap: .round))
                .rotationEffect(.degrees(-90))
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}

// MARK: - Charts

/// One day of a week chart.
struct ChartDay: Identifiable, Equatable {
    let date: Date
    let value: Double
    /// What the day says aloud: "12 minutes".
    let spoken: String

    var id: Date { date }
    var letter: String { date.formatted(.dateTime.weekday(.narrow)) }
    var isToday: Bool { Calendar.current.isDateInToday(date) }
}

/// One bar per day, the tallest day full height. A day with nothing keeps a
/// flat mark so the week still reads as a week.
struct DayBars: View {
    let days: [ChartDay]
    var height: CGFloat = 30
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let peak = days.map(\.value).max() ?? 0
        VStack(spacing: 3) {
            HStack(alignment: .bottom, spacing: 5) {
                ForEach(days) { day in
                    RoundedRectangle(cornerRadius: 2, style: .continuous)
                        .fill(day.value > 0 ? Theme.accent : Theme.selection)
                        .frame(height: day.value > 0 && peak > 0 ? max(3, height * day.value / peak) : 2)
                        .frame(maxWidth: .infinity)
                }
            }
            .frame(height: height, alignment: .bottom)
            WeekLetters(days: days)
        }
        .animation(Motion.settle(reduced: reduceMotion), value: days)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(days.map { "\($0.date.formatted(.dateTime.weekday(.wide))), \($0.spoken)" }.joined(separator: "; "))
    }
}

/// The same week as a line, for counts that rise and fall rather than pile up.
struct Sparkline: View {
    let days: [ChartDay]
    var height: CGFloat = 30

    var body: some View {
        VStack(spacing: 3) {
            GeometryReader { proxy in
                let points = plot(in: proxy.size)
                ZStack {
                    if let first = points.first, let last = points.last {
                        Path { path in
                            path.move(to: CGPoint(x: first.x, y: proxy.size.height))
                            points.forEach { path.addLine(to: $0) }
                            path.addLine(to: CGPoint(x: last.x, y: proxy.size.height))
                            path.closeSubpath()
                        }
                        .fill(Theme.accentSoft)
                        Path { path in
                            path.move(to: first)
                            points.dropFirst().forEach { path.addLine(to: $0) }
                        }
                        .stroke(Theme.accent, style: StrokeStyle(lineWidth: 1.5, lineCap: .round, lineJoin: .round))
                    }
                }
            }
            .frame(height: height)
            WeekLetters(days: days)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(days.map { "\($0.date.formatted(.dateTime.weekday(.wide))), \($0.spoken)" }.joined(separator: "; "))
    }

    private func plot(in size: CGSize) -> [CGPoint] {
        let peak = max(days.map(\.value).max() ?? 0, 1)
        let step = days.count > 1 ? size.width / CGFloat(days.count) : 0
        return days.indices.map { index in
            CGPoint(x: step * (CGFloat(index) + 0.5),
                    y: size.height - 1.5 - CGFloat(days[index].value / peak) * (size.height - 3))
        }
    }
}

private struct WeekLetters: View {
    let days: [ChartDay]

    var body: some View {
        HStack(spacing: 5) {
            ForEach(days) { day in
                Text(day.letter)
                    .font(.system(size: 9, weight: day.isToday ? .semibold : .regular))
                    .foregroundStyle(day.isToday ? Theme.ink : Theme.inkTertiary)
                    .frame(maxWidth: .infinity)
            }
        }
        .accessibilityHidden(true)
    }
}

/// One stretch of a day: a recorded meeting, or one still to come.
struct DayStripBlock: Identifiable, Equatable {
    let id: String
    let start: Date
    let end: Date
    let recorded: Bool
}

/// Today from morning to evening, with its meetings on it and a mark at now.
struct DayStrip: View {
    let blocks: [DayStripBlock]
    let now: Date

    var body: some View {
        let bounds = range
        VStack(spacing: 3) {
            GeometryReader { proxy in
                let width = proxy.size.width
                let span = max(bounds.end.timeIntervalSince(bounds.start), 1)
                let x: (Date) -> CGFloat = { date in
                    CGFloat(min(max(date.timeIntervalSince(bounds.start) / span, 0), 1)) * width
                }
                ZStack(alignment: .leading) {
                    Capsule().fill(Theme.inset).frame(height: 10)
                    ForEach(blocks) { block in
                        RoundedRectangle(cornerRadius: 2.5, style: .continuous)
                            .fill(block.recorded ? Theme.accent : Theme.accentSoft)
                            .overlay(RoundedRectangle(cornerRadius: 2.5, style: .continuous)
                                .strokeBorder(Theme.accent, lineWidth: block.recorded ? 0 : 1))
                            .frame(width: max(4, x(block.end) - x(block.start)), height: 10)
                            .offset(x: min(x(block.start), width - 4))
                    }
                    Capsule().fill(Theme.ink).frame(width: 2, height: 16).offset(x: max(0, x(now) - 1))
                }
                .frame(width: width, height: 16)
            }
            .frame(height: 16)
            HStack {
                Text(bounds.start.formatted(.dateTime.hour()))
                Spacer()
                Text(bounds.end.formatted(.dateTime.hour()))
            }
            .font(.system(size: 9))
            .foregroundStyle(Theme.inkTertiary)
        }
        .accessibilityHidden(true)
    }

    /// Eight to eight, stretched to whatever falls outside it.
    private var range: DateInterval {
        let calendar = Calendar.current
        let day = calendar.startOfDay(for: now)
        let morning = calendar.date(byAdding: .hour, value: 8, to: day) ?? day
        let evening = calendar.date(byAdding: .hour, value: 20, to: day) ?? now
        let earliest = ([morning, now] + blocks.map(\.start)).min() ?? morning
        let latest = ([evening, now] + blocks.map(\.end)).max() ?? evening
        let start = calendar.dateInterval(of: .hour, for: max(earliest, day))?.start ?? earliest
        let nextDay = calendar.date(byAdding: .day, value: 1, to: day) ?? latest
        let end = calendar.dateInterval(of: .hour, for: min(latest, nextDay.addingTimeInterval(-1)))?.end ?? latest
        return DateInterval(start: start, end: max(end, start.addingTimeInterval(3600)))
    }
}

/// A share of a whole, as one bar.
struct ShareBar: View {
    let fraction: Double
    var tint: Color = Theme.accent

    var body: some View {
        GeometryReader { proxy in
            ZStack(alignment: .leading) {
                Capsule().fill(Theme.inset)
                Capsule().fill(tint).frame(width: proxy.size.width * min(max(fraction, 0), 1))
            }
        }
        .frame(height: 6)
        .accessibilityHidden(true)
    }
}

/// Where a meeting's threads landed: one bar in three parts, with each
/// count written beside it so the split never rests on color alone. Given
/// `select`, each count is a press that picks its part, and a second press
/// on the picked part lets it go.
struct OutcomeBar: View {
    let counts: LedgerOutcomeCounts
    var selected: LedgerOutcome?
    var select: ((LedgerOutcome?) -> Void)?

    private static let gap: CGFloat = 2

    var body: some View {
        HStack(spacing: 10) {
            GeometryReader { proxy in
                let room = proxy.size.width - Self.gap * CGFloat(max(present.count - 1, 0))
                HStack(spacing: Self.gap) {
                    ForEach(present, id: \.self) { outcome in
                        Capsule()
                            .fill(Self.tint(outcome))
                            .opacity(selected == nil || selected == outcome ? 1 : 0.35)
                            .frame(width: room * CGFloat(counts.count(outcome)) / CGFloat(max(counts.total, 1)))
                    }
                }
            }
            .frame(width: 96, height: 6)
            .accessibilityHidden(true)
            HStack(spacing: 10) {
                ForEach(present, id: \.self) { outcome in legend(outcome) }
            }
        }
    }

    private var present: [LedgerOutcome] {
        LedgerOutcome.allCases.filter { counts.count($0) > 0 }
    }

    @ViewBuilder
    private func legend(_ outcome: LedgerOutcome) -> some View {
        let text = Text("\(counts.count(outcome)) \(outcome.label)")
            .font(TypeScale.label(12))
            .monospacedDigit()
            .foregroundStyle(selected == outcome ? Theme.ink : Theme.inkSecondary)
        if let select {
            Button { select(selected == outcome ? nil : outcome) } label: { text }
                .buttonStyle(.plain)
                .accessibilityAddTraits(selected == outcome ? .isSelected : [])
                .accessibilityHint(selected == outcome ? "Show every thread" : "Show only these threads")
        } else {
            text
        }
    }

    private static func tint(_ outcome: LedgerOutcome) -> Color {
        switch outcome {
        case .landed: Theme.accent
        case .open: Theme.inkSecondary
        case .dropped: Theme.inkTertiary
        }
    }
}

/// Work done against work in scope, over the life of a project. Both lines
/// step: a task joins the scope when it is created, and counts as done from
/// its last change once it is done.
struct BurnUpSeries: Equatable {
    struct Point: Equatable {
        let at: Date
        let value: Int
    }

    let start: Date
    let end: Date
    let scope: [Point]
    let done: [Point]

    var total: Int { scope.last?.value ?? 0 }
    var completed: Int { done.last?.value ?? 0 }
}

struct BurnUpChart: View {
    let series: BurnUpSeries
    var height: CGFloat = 132

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 14) {
                legend(done: true, "Done", series.completed)
                legend(done: false, "Scope", series.total)
                Spacer()
            }
            GeometryReader { proxy in
                let top = max(series.scope.map(\.value).max() ?? 0, 1)
                ZStack(alignment: .topLeading) {
                    guide(top: top, size: proxy.size)
                    steps(series.done, top: top, size: proxy.size, closed: true)
                        .fill(Theme.accentSoft)
                    steps(series.done, top: top, size: proxy.size, closed: false)
                        .stroke(Theme.accent, style: StrokeStyle(lineWidth: 1.75, lineJoin: .round))
                    steps(series.scope, top: top, size: proxy.size, closed: false)
                        .stroke(Theme.inkTertiary, style: StrokeStyle(lineWidth: 1.25, lineJoin: .round, dash: [3, 3]))
                }
            }
            .frame(height: height)
            HStack {
                Text(series.start.formatted(.dateTime.month(.abbreviated).day()))
                Spacer()
                Text("Today")
            }
            .metaText()
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Done against scope")
        .accessibilityValue("\(series.completed) of \(series.total) tasks done since \(series.start.formatted(date: .abbreviated, time: .omitted))")
    }

    private func legend(done: Bool, _ title: String, _ value: Int) -> some View {
        HStack(spacing: 6) {
            if done {
                RoundedRectangle(cornerRadius: 1).fill(Theme.accent).frame(width: 10, height: 3)
            } else {
                Path { path in
                    path.move(to: CGPoint(x: 0, y: 1.5))
                    path.addLine(to: CGPoint(x: 10, y: 1.5))
                }
                .stroke(Theme.inkTertiary, style: StrokeStyle(lineWidth: 1.25, dash: [3, 2]))
                .frame(width: 10, height: 3)
            }
            Text(title).metaText(Theme.inkSecondary)
            Text("\(value)").font(TypeScale.label(12)).foregroundStyle(Theme.ink).monospacedDigit()
        }
    }

    private func guide(top: Int, size: CGSize) -> some View {
        ZStack(alignment: .topLeading) {
            Path { path in
                path.move(to: CGPoint(x: 0, y: y(top, top: top, height: size.height)))
                path.addLine(to: CGPoint(x: size.width, y: y(top, top: top, height: size.height)))
                path.move(to: CGPoint(x: 0, y: y(0, top: top, height: size.height)))
                path.addLine(to: CGPoint(x: size.width, y: y(0, top: top, height: size.height)))
            }
            .stroke(Theme.hairline, lineWidth: 1)
            Text("\(top)")
                .font(.system(size: 10))
                .foregroundStyle(Theme.inkTertiary)
                .monospacedDigit()
                .padding(.top, 2)
                .padding(.leading, 2)
        }
    }

    private func y(_ value: Int, top: Int, height: CGFloat) -> CGFloat {
        height - 1 - CGFloat(value) / CGFloat(top) * (height - 2)
    }

    private func steps(_ points: [BurnUpSeries.Point], top: Int, size: CGSize, closed: Bool) -> Path {
        let span = max(series.end.timeIntervalSince(series.start), 1)
        let x: (Date) -> CGFloat = { date in
            CGFloat(min(max(date.timeIntervalSince(series.start) / span, 0), 1)) * size.width
        }
        var path = Path()
        var current = 0
        path.move(to: CGPoint(x: 0, y: y(0, top: top, height: size.height)))
        for point in points {
            let at = x(point.at)
            path.addLine(to: CGPoint(x: at, y: y(current, top: top, height: size.height)))
            path.addLine(to: CGPoint(x: at, y: y(point.value, top: top, height: size.height)))
            current = point.value
        }
        path.addLine(to: CGPoint(x: size.width, y: y(current, top: top, height: size.height)))
        if closed {
            path.addLine(to: CGPoint(x: size.width, y: y(0, top: top, height: size.height)))
            path.closeSubpath()
        }
        return path
    }
}

/// A milestone placed on the project's line by its date.
struct TimelineMilestone: Identifiable, Equatable {
    let id: String
    let title: String
    let due: Date
    let status: PlanningWorkStatus
    let overdue: Bool
}

/// The project's dated milestones on one line, from its start to its last
/// date, with today marked.
struct MilestoneTimeline: View {
    let milestones: [TimelineMilestone]
    let start: Date
    let now: Date

    var body: some View {
        let first = min(([start, now] + milestones.map(\.due)).min() ?? start, now)
        let last = max(([now] + milestones.map(\.due)).max() ?? now, first.addingTimeInterval(86_400))
        VStack(spacing: 4) {
            GeometryReader { proxy in
                let inset: CGFloat = 8
                let width = max(proxy.size.width - inset * 2, 1)
                let span = max(last.timeIntervalSince(first), 1)
                let x: (Date) -> CGFloat = { date in
                    inset + CGFloat(min(max(date.timeIntervalSince(first) / span, 0), 1)) * width
                }
                ZStack(alignment: .topLeading) {
                    Capsule().fill(Theme.selection)
                        .frame(width: width, height: 2)
                        .position(x: inset + width / 2, y: 24)
                    Capsule().fill(Theme.ink)
                        .frame(width: 1.5, height: 14)
                        .position(x: x(now), y: 24)
                    Text("Today")
                        .font(.system(size: 10, weight: .medium))
                        .foregroundStyle(Theme.ink)
                        .fixedSize()
                        .position(x: min(max(x(now), 22), proxy.size.width - 22), y: 6)
                    ForEach(milestones) { milestone in
                        MilestoneMark(status: milestone.status, overdue: milestone.overdue, size: 12)
                            .position(x: x(milestone.due), y: 24)
                            .help("\(milestone.title), \(milestone.due.formatted(date: .abbreviated, time: .omitted))")
                    }
                }
            }
            .frame(height: 34)
            HStack {
                Text(first.formatted(.dateTime.month(.abbreviated).day()))
                Spacer()
                Text(last.formatted(.dateTime.month(.abbreviated).day()))
            }
            .metaText()
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Milestones by date")
        .accessibilityValue(milestones.map { "\($0.title), \($0.due.formatted(date: .abbreviated, time: .omitted)), \($0.status.label)" }.joined(separator: "; "))
    }
}

// MARK: - People

/// Whose work this is: someone Sona knows, me, or nobody yet.
struct AvatarFace: Identifiable, Hashable {
    enum Kind: Hashable {
        case me
        case person(String)
        case unassigned
    }

    let kind: Kind
    let name: String

    var id: String {
        switch kind {
        case .me: "me"
        case let .person(id): id
        case .unassigned: "unassigned"
        }
    }
}

struct Avatar: View {
    let face: AvatarFace
    var size: CGFloat = 20
    let revision: UInt64

    var body: some View {
        switch face.kind {
        case .me:
            Image(systemName: "person.fill")
                .font(.system(size: size * 0.5))
                .foregroundStyle(.white)
                .frame(width: size, height: size)
                .background { MonogramFill(color: Theme.accentFill) }
                .clipShape(Circle())
                .help(face.name)
                .accessibilityLabel(face.name)
        case let .person(id):
            PersonPortrait(personId: id, name: face.name, size: size, revision: revision)
        case .unassigned:
            Circle()
                .strokeBorder(Theme.inkTertiary, style: StrokeStyle(lineWidth: 1, dash: [2, 2]))
                .frame(width: size, height: size)
                .help(face.name)
                .accessibilityLabel(face.name)
        }
    }
}

/// A few faces, overlapping, and how many more.
struct AvatarStack: View {
    let faces: [AvatarFace]
    var size: CGFloat = 20
    var limit = 3
    let revision: UInt64
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        HStack(spacing: -size / 4) {
            ForEach(faces.prefix(limit)) { face in
                Avatar(face: face, size: size, revision: revision)
                    .padding(1.5)
                    .background(Theme.surface, in: Circle())
                    .transition(reduceMotion ? AnyTransition.opacity : AnyTransition.scale(scale: 0.6).combined(with: .opacity))
            }
            if faces.count > limit {
                Text("+\(faces.count - limit)")
                    .font(.system(size: max(9, size * 0.42), weight: .semibold))
                    .monospacedDigit()
                    .lineLimit(1)
                    .minimumScaleFactor(0.6)
                    .contentTransition(reduceMotion ? .opacity : .numericText())
                    .foregroundStyle(Theme.inkSecondary)
                    .frame(width: size, height: size)
                    .background(Theme.inset, in: Circle())
                    .padding(1.5)
                    .background(Theme.surface, in: Circle())
            }
        }
        .animation(Motion.settle(reduced: reduceMotion), value: faces)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(faces.map(\.name).joined(separator: ", "))
    }
}

// MARK: - Controls

struct SwitchItem<ID: Hashable>: Identifiable {
    let id: ID
    let title: String
    var icon: String?
}

/// A few choices where exactly one is on: the tabs of a place, or the ways to
/// lay out a list.
struct SegmentedSwitch<ID: Hashable>: View {
    let items: [SwitchItem<ID>]
    let selection: ID
    var iconOnly = false
    let select: (ID) -> Void
    @Namespace private var namespace
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        HStack(spacing: 2) {
            ForEach(items) { item in
                let on = item.id == selection
                Button { select(item.id) } label: {
                    HStack(spacing: 5) {
                        if let icon = item.icon {
                            Image(systemName: icon).font(.system(size: 11, weight: .semibold))
                        }
                        if !iconOnly { Text(item.title) }
                    }
                    .font(TypeScale.label(12.5))
                    .foregroundStyle(on ? Theme.ink : Theme.inkSecondary)
                    .padding(.horizontal, iconOnly ? 8 : 10)
                    .frame(height: 24)
                    .background {
                        if on {
                            RoundedRectangle(cornerRadius: 6)
                                .fill(Theme.surface)
                                .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(Theme.border, lineWidth: 1))
                                .matchedGeometryEffect(id: "on", in: namespace)
                        }
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help(item.title)
                .accessibilityLabel(item.title)
                .accessibilityAddTraits(on ? .isSelected : [])
            }
        }
        .padding(2)
        .background(Theme.control, in: RoundedRectangle(cornerRadius: 8))
        .animation(Motion.settle(reduced: reduceMotion), value: selection)
    }
}

/// A mark that opens a short list of states to choose from, each drawn with
/// its own mark.
struct ChoicePicker<Choice: Identifiable & Equatable, Label: View, Icon: View>: View {
    let choices: [Choice]
    let current: Choice
    let title: (Choice) -> String
    let accessibility: String
    let pick: (Choice) -> Void
    let icon: (Choice) -> Icon
    let label: Label
    @State private var open = false

    init(_ choices: [Choice], current: Choice, title: @escaping (Choice) -> String, accessibility: String,
         pick: @escaping (Choice) -> Void, @ViewBuilder icon: @escaping (Choice) -> Icon,
         @ViewBuilder label: () -> Label) {
        self.choices = choices
        self.current = current
        self.title = title
        self.accessibility = accessibility
        self.pick = pick
        self.icon = icon
        self.label = label()
    }

    var body: some View {
        Button { open.toggle() } label: { label.contentShape(Rectangle()) }
            .buttonStyle(.plain)
            .accessibilityLabel(accessibility)
            .popover(isPresented: $open, arrowEdge: .bottom) {
                VStack(alignment: .leading, spacing: 1) {
                    ForEach(choices) { choice in
                        ChoicePickerRow(title: title(choice), selected: choice == current) {
                            icon(choice)
                        } action: {
                            open = false
                            if choice != current { pick(choice) }
                        }
                    }
                }
                .padding(5)
                .frame(minWidth: 172)
            }
    }
}

private struct ChoicePickerRow<Icon: View>: View {
    let title: String
    let selected: Bool
    @ViewBuilder let icon: () -> Icon
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 8) {
                icon().frame(width: 16)
                Text(title).font(TypeScale.label(13)).foregroundStyle(Theme.ink)
                Spacer(minLength: 12)
                if selected {
                    Image(systemName: "checkmark")
                        .font(.system(size: 10, weight: .bold))
                        .foregroundStyle(Theme.inkSecondary)
                }
            }
            .padding(.horizontal, 8)
            .frame(height: 26)
            .background(hovering ? Theme.wash : .clear, in: RoundedRectangle(cornerRadius: 6))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

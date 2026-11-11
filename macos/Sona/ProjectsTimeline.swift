import SwiftUI

enum ProjectsTimelineZoom: String, CaseIterable {
    case week, month, quarter

    static var items: [SwitchItem<ProjectsTimelineZoom>] {
        allCases.map { SwitchItem(id: $0, title: $0.title) }
    }

    var title: String {
        switch self {
        case .week: "Week"
        case .month: "Month"
        case .quarter: "Quarter"
        }
    }

    var dayWidth: CGFloat {
        switch self {
        case .week: 28
        case .month: 9
        case .quarter: 3
        }
    }
}

private struct TimelineMove: Equatable {
    enum Part: Equatable {
        case start(String)
        case target(String)
        case milestone(String)

        var isStart: Bool {
            if case .start = self { return true }
            return false
        }
    }

    let part: Part
    let days: Int
    let from: Int64?
}

private struct TimelineRow: Identifiable {
    let project: PlanningProject
    let span: (start: Date, end: Date)?
    let order: Date
    let milestones: [(milestone: PlanningMilestone, due: Date)]
    var id: String { project.id }
}

private struct TimelineAxis {
    struct Group: Identifiable {
        let day: Int
        var span: Int
        let title: String
        var id: Int { day }
    }

    struct Tick: Identifiable {
        let day: Int
        let title: String
        var id: Int { day }
    }

    let first: Date
    let days: Int
    let today: Date
    let dayWidth: CGFloat
    let groups: [Group]
    let ticks: [Tick]
    let calendar: Calendar

    init(dates: [Date], zoom: ProjectsTimelineZoom, calendar: Calendar, now: Date) {
        let today = calendar.startOfDay(for: now)
        func monthStart(_ date: Date) -> Date {
            calendar.date(from: calendar.dateComponents([.year, .month], from: date)) ?? date
        }
        let lead = zoom == .quarter ? 3 : 1
        let first = calendar.date(byAdding: .month, value: -lead, to: monthStart(min(dates.min() ?? today, today))) ?? today
        let last = calendar.date(byAdding: .month, value: lead + 2, to: monthStart(max(dates.max() ?? today, today))) ?? today
        let days = max(calendar.dateComponents([.day], from: first, to: last).day ?? 1, 1)
        var groups: [Group] = []
        var ticks: [Tick] = []
        for day in 0..<days {
            guard let date = calendar.date(byAdding: .day, value: day, to: first) else { continue }
            let parts = calendar.dateComponents([.year, .month, .day, .weekday], from: date)
            let group = zoom == .quarter
                ? "Q\(((parts.month ?? 1) - 1) / 3 + 1) \(parts.year ?? 0)"
                : date.formatted(.dateTime.month(.wide).year())
            if let last = groups.indices.last, groups[last].title == group {
                groups[last].span += 1
            } else {
                groups.append(Group(day: day, span: 1, title: group))
            }
            switch zoom {
            case .week:
                ticks.append(Tick(day: day, title: "\(date.formatted(.dateTime.weekday(.narrow))) \(parts.day ?? 0)"))
            case .month where parts.weekday == calendar.firstWeekday:
                ticks.append(Tick(day: day, title: "\(parts.day ?? 0)"))
            case .quarter where parts.day == 1:
                ticks.append(Tick(day: day, title: date.formatted(.dateTime.month(.abbreviated))))
            default:
                break
            }
        }
        self.first = first
        self.days = days
        self.today = today
        self.dayWidth = zoom.dayWidth
        self.groups = groups
        self.ticks = ticks
        self.calendar = calendar
    }

    var width: CGFloat { CGFloat(days) * dayWidth }

    func x(_ date: Date) -> CGFloat {
        CGFloat(calendar.dateComponents([.day], from: first, to: calendar.startOfDay(for: date)).day ?? 0) * dayWidth
    }
}

/// Projects on one time axis: a bar from start to target day, milestones as
/// diamonds, today as a line. Dragging a bar's end or a diamond moves that
/// date through the saves the project page makes.
struct ProjectsTimeline: View {
    let store: ProjectsStore
    let projects: [PlanningProjectSummary]

    @AppStorage("projects.timeline.zoom") private var zoom = ProjectsTimelineZoom.month
    @State private var move: TimelineMove?
    @State private var position = ScrollPosition(edge: .leading)
    @State private var scrolled: CGFloat = 0

    private static let space = "projectsTimeline"
    private static let nameWidth: CGFloat = 196
    private static let headerHeight: CGFloat = 56
    private static let rowHeight: CGFloat = 52
    private static let barTop: CGFloat = 9
    private static let barHeight: CGFloat = 22
    private let calendar = Calendar.current

    var body: some View {
        let rows = rows()
        let axis = axis(rows)
        let height = Self.headerHeight + CGFloat(rows.count) * Self.rowHeight
        VStack(alignment: .leading, spacing: Space.m) {
            SegmentedSwitch(items: ProjectsTimelineZoom.items, selection: zoom) { zoom = $0 }
                .accessibilityElement(children: .contain)
                .accessibilityLabel("Zoom")
            HStack(alignment: .top, spacing: 0) {
                names(rows)
                Rectangle().fill(Theme.hairline).frame(width: 1)
                ScrollView(.horizontal) {
                    chart(rows, axis, height: height)
                }
                .scrollPosition($position)
                .onScrollGeometryChange(for: CGFloat.self) { $0.contentOffset.x } action: { _, x in scrolled = x }
                .onAppear { position.scrollTo(x: leading(axis)) }
                .onChange(of: zoom) { position.scrollTo(x: leading(self.axis(self.rows()))) }
            }
            .frame(height: height)
            .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusCard))
            .clipShape(RoundedRectangle(cornerRadius: Theme.radiusCard))
            .overlay { RoundedRectangle(cornerRadius: Theme.radiusCard).strokeBorder(Theme.hairline) }
        }
    }

    private func rows() -> [TimelineRow] {
        projects.map { summary in
            let project = summary.project
            let milestones = (store.details[summary.id]?.milestones ?? []).compactMap { milestone in
                due(of: milestone, moved: move).map { (milestone: milestone, due: $0) }
            }
            return TimelineRow(project: project, span: span(of: project, moved: move),
                               order: span(of: project, moved: nil)?.start ?? .distantFuture, milestones: milestones)
        }
        .sorted { ($0.order, $0.project.title) < ($1.order, $1.project.title) }
    }

    private func axis(_ rows: [TimelineRow]) -> TimelineAxis {
        let dates = rows.flatMap { row in
            [row.span?.start, row.span?.end].compactMap { $0 } + row.milestones.map(\.due)
        }
        return TimelineAxis(dates: dates, zoom: zoom, calendar: calendar, now: .now)
    }

    private func leading(_ axis: TimelineAxis) -> CGFloat {
        max(axis.x(axis.today) - 160, 0)
    }

    private func names(_ rows: [TimelineRow]) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            Color.clear.frame(height: Self.headerHeight)
            ForEach(rows) { row in
                Button { store.open(row.id) } label: {
                    HStack(spacing: 8) {
                        Circle().fill(tint(row.project)).frame(width: 8, height: 8)
                        Text(row.project.title).bodyText(13).lineLimit(1)
                        Spacer(minLength: 0)
                    }
                    .padding(.horizontal, 14)
                    .frame(height: Self.rowHeight)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .overlay(alignment: .top) { Rectangle().fill(Theme.hairline).frame(height: 1) }
            }
        }
        .frame(width: Self.nameWidth, alignment: .leading)
    }

    private func chart(_ rows: [TimelineRow], _ axis: TimelineAxis, height: CGFloat) -> some View {
        ZStack(alignment: .topLeading) {
            grid(axis, rows: rows.count)
            header(axis)
            todayMark(axis, height: height)
            ForEach(Array(rows.enumerated()), id: \.element.id) { index, row in
                lane(row, axis)
                    .offset(y: Self.headerHeight + CGFloat(index) * Self.rowHeight)
            }
        }
        .frame(width: axis.width, height: height, alignment: .topLeading)
        .coordinateSpace(.named(Self.space))
    }

    private func grid(_ axis: TimelineAxis, rows: Int) -> some View {
        Canvas { context, size in
            for group in axis.groups {
                let x = CGFloat(group.day) * axis.dayWidth
                context.fill(Path(CGRect(x: x, y: 0, width: 1, height: size.height)), with: .color(Theme.hairline))
            }
            for tick in axis.ticks {
                let x = CGFloat(tick.day) * axis.dayWidth
                context.fill(Path(CGRect(x: x, y: 24, width: 1, height: size.height - 24)),
                             with: .color(Theme.hairline.opacity(0.5)))
            }
            for row in 0...rows {
                let y = Self.headerHeight + CGFloat(row) * Self.rowHeight
                context.fill(Path(CGRect(x: 0, y: y, width: size.width, height: 1)), with: .color(Theme.hairline))
            }
        }
        .allowsHitTesting(false)
    }

    private func header(_ axis: TimelineAxis) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 0) {
                ForEach(axis.groups) { group in
                    let start = CGFloat(group.day) * axis.dayWidth
                    let width = CGFloat(group.span) * axis.dayWidth
                    let shift = min(max(scrolled - start, 0), width)
                    Text(group.title)
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(Theme.ink)
                        .lineLimit(1)
                        .frame(maxWidth: max(width - shift - 12, 0), alignment: .leading)
                        .padding(.leading, 6 + shift)
                        .frame(width: width, alignment: .leading)
                        .opacity(width - shift - 12 >= 70 ? 1 : 0)
                }
            }
            .frame(height: 24)
            ZStack(alignment: .topLeading) {
                ForEach(axis.ticks) { tick in
                    Text(tick.title)
                        .font(.system(size: 10.5))
                        .foregroundStyle(Theme.inkTertiary)
                        .fixedSize()
                        .offset(x: CGFloat(tick.day) * axis.dayWidth + 4)
                }
            }
            .frame(width: axis.width, height: Self.headerHeight - 24, alignment: .topLeading)
        }
        .allowsHitTesting(false)
    }

    private func todayMark(_ axis: TimelineAxis, height: CGFloat) -> some View {
        let x = axis.x(axis.today) + axis.dayWidth / 2
        return ZStack(alignment: .topLeading) {
            Rectangle()
                .fill(Theme.accent)
                .frame(width: 1.5, height: height - Self.headerHeight + 6)
                .position(x: x, y: (height + Self.headerHeight - 6) / 2)
            Text("Today")
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(Theme.onAccent)
                .padding(.horizontal, 6)
                .padding(.vertical, 2)
                .background(Theme.accent, in: Capsule())
                .fixedSize()
                .position(x: x, y: Self.headerHeight - 9)
        }
        .frame(width: axis.width, height: height)
        .allowsHitTesting(false)
    }

    private func lane(_ row: TimelineRow, _ axis: TimelineAxis) -> some View {
        let tint = tint(row.project)
        return ZStack(alignment: .topLeading) {
            if let span = row.span {
                bar(row.project, span, tint, axis, marks: row.milestones.map { axis.x($0.due) })
            } else {
                Text("No dates").metaText().offset(x: axis.x(axis.today) + 10, y: Self.barTop + 3)
            }
            ForEach(row.milestones, id: \.milestone.id) { item in
                let x = axis.x(item.due)
                milestoneMark(item.milestone, item.due, tint, axis,
                              room: row.milestones.map { axis.x($0.due) }.filter { $0 > x }.min().map { $0 - x - 8 })
            }
        }
        .frame(width: axis.width, height: Self.rowHeight, alignment: .topLeading)
    }

    private func bar(_ project: PlanningProject, _ span: (start: Date, end: Date), _ tint: Color, _ axis: TimelineAxis, marks: [CGFloat]) -> some View {
        let x = axis.x(span.start)
        let width = max(axis.x(span.end) + axis.dayWidth - x, axis.dayWidth)
        let shift = min(max(scrolled - x, 0), width)
        let room = min(marks.filter { $0 > x + shift }.min().map { $0 - x - shift - 24 } ?? .infinity, width - shift - 28)
        return RoundedRectangle(cornerRadius: Theme.radiusRow)
            .fill(tint.opacity(0.22))
            .overlay { RoundedRectangle(cornerRadius: Theme.radiusRow).strokeBorder(tint.opacity(0.8), lineWidth: 1) }
            .overlay(alignment: .leading) {
                if room >= 30 {
                    Text(project.title)
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(Theme.ink)
                        .lineLimit(1)
                        .frame(maxWidth: room, alignment: .leading)
                        .padding(.horizontal, 14)
                        .padding(.leading, shift)
                }
            }
            .overlay(alignment: .leading) { handle(.start(project.id), project, span.start, tint) }
            .overlay(alignment: .trailing) { handle(.target(project.id), project, span.end, tint) }
            .frame(width: width, height: Self.barHeight)
            .help("\(project.title): \(Self.day(span.start)) – \(Self.day(span.end))")
            .offset(x: x, y: Self.barTop)
    }

    private func handle(_ part: TimelineMove.Part, _ project: PlanningProject, _ date: Date, _ tint: Color) -> some View {
        RoundedRectangle(cornerRadius: Theme.radiusMark)
            .fill(tint)
            .frame(width: 4, height: 14)
            .frame(width: 12, height: Self.barHeight)
            .contentShape(Rectangle())
            .pointerStyle(.columnResize)
            .gesture(dragging(part, from: part.isStart ? project.startAtMs : project.targetAtMs))
            .accessibilityElement()
            .accessibilityLabel(part.isStart ? "\(project.title) start date" : "\(project.title) target date")
            .accessibilityValue(Self.day(date))
            .accessibilityAdjustableAction { nudge(part, from: part.isStart ? project.startAtMs : project.targetAtMs, $0) }
    }

    private func milestoneMark(_ milestone: PlanningMilestone, _ due: Date, _ tint: Color, _ axis: TimelineAxis, room: CGFloat?) -> some View {
        let x = axis.x(due) + axis.dayWidth / 2
        return ZStack(alignment: .topLeading) {
            if room.map({ $0 >= 24 }) ?? true {
                Text(milestone.title)
                    .font(.system(size: 10.5))
                    .foregroundStyle(Theme.inkSecondary)
                    .lineLimit(1)
                    .frame(maxWidth: room, alignment: .leading)
                    .offset(x: x - 4, y: Self.barTop + Self.barHeight + 3)
                    .allowsHitTesting(false)
            }
            Rectangle()
                .fill(milestone.status.isOpen ? Theme.surface : tint)
                .overlay { Rectangle().strokeBorder(tint, lineWidth: 1.5) }
                .frame(width: 9, height: 9)
                .rotationEffect(.degrees(45))
                .frame(width: 20, height: 20)
                .contentShape(Rectangle())
                .pointerStyle(.grabIdle)
                .gesture(dragging(.milestone(milestone.id), from: milestone.dueAtMs))
                .help("\(milestone.title): \(Self.day(due))")
                .accessibilityElement()
                .accessibilityLabel("\(milestone.title) milestone")
                .accessibilityValue(Self.day(due))
                .accessibilityAdjustableAction { nudge(.milestone(milestone.id), from: milestone.dueAtMs, $0) }
                .offset(x: x - 10, y: Self.barTop + Self.barHeight / 2 - 10)
        }
    }

    private func dragging(_ part: TimelineMove.Part, from: Int64?) -> some Gesture {
        DragGesture(minimumDistance: 2, coordinateSpace: .named(Self.space))
            .onChanged { value in
                guard !store.mutating else { return }
                move = TimelineMove(part: part, days: Int((value.translation.width / zoom.dayWidth).rounded()), from: from)
            }
            .onEnded { value in
                commit(part, days: Int((value.translation.width / zoom.dayWidth).rounded()), from: from)
            }
    }

    private func nudge(_ part: TimelineMove.Part, from: Int64?, _ direction: AccessibilityAdjustmentDirection) {
        switch direction {
        case .increment: commit(part, days: 1, from: from)
        case .decrement: commit(part, days: -1, from: from)
        @unknown default: break
        }
    }

    private func commit(_ part: TimelineMove.Part, days: Int, from: Int64?) {
        guard days != 0, !store.mutating else {
            move = nil
            return
        }
        let moved = TimelineMove(part: part, days: days, from: from)
        move = moved
        Task {
            switch part {
            case let .start(id), let .target(id):
                guard let project = projects.first(where: { $0.id == id })?.project,
                      let span = span(of: project, moved: moved) else { break }
                let change = part.isStart
                    ? PlanningProjectSave(startAtMs: .set(ProjectsStore.milliseconds(span.start)))
                    : PlanningProjectSave(targetAtMs: .set(ProjectsStore.milliseconds(span.end)))
                await store.saveProject(project, change)
            case let .milestone(id):
                guard let milestone = store.details.values.lazy.flatMap(\.milestones).first(where: { $0.id == id }),
                      let due = due(of: milestone, moved: moved) else { break }
                await store.moveMilestone(milestone, dueAtMs: ProjectsStore.milliseconds(due))
            }
            if move == moved { move = nil }
        }
    }

    private func span(of project: PlanningProject, moved: TimelineMove?) -> (start: Date, end: Date)? {
        guard let start = date(project.startAtMs ?? project.targetAtMs),
              let end = date(project.targetAtMs ?? project.startAtMs) else { return nil }
        guard let moved else { return (start, end) }
        switch moved.part {
        case .start(project.id) where moved.from == project.startAtMs:
            return (min(shift(start, moved.days), end), end)
        case .target(project.id) where moved.from == project.targetAtMs:
            return (start, max(shift(end, moved.days), start))
        default: return (start, end)
        }
    }

    private func due(of milestone: PlanningMilestone, moved: TimelineMove?) -> Date? {
        guard let due = date(milestone.dueAtMs) else { return nil }
        guard let moved, moved.part == .milestone(milestone.id), moved.from == milestone.dueAtMs else { return due }
        return shift(due, moved.days)
    }

    private func date(_ ms: Int64?) -> Date? {
        ms.map { Date(timeIntervalSince1970: Double($0) / 1000) }
    }

    private func shift(_ date: Date, _ days: Int) -> Date {
        calendar.date(byAdding: .day, value: days, to: date) ?? date
    }

    private func tint(_ project: PlanningProject) -> Color {
        Theme.label(hue: Double(project.hue ?? 210))
    }

    private static func day(_ date: Date) -> String {
        date.formatted(date: .abbreviated, time: .omitted)
    }
}

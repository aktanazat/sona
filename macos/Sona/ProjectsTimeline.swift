import SwiftUI

enum ProjectsTimelineZoom: String, CaseIterable {
    case week, month, quarter, year

    static var items: [SwitchItem<ProjectsTimelineZoom>] {
        allCases.map { SwitchItem(id: $0, title: $0.title) }
    }

    var title: String {
        switch self {
        case .week: "Week"
        case .month: "Month"
        case .quarter: "Quarter"
        case .year: "Year"
        }
    }

    var dayWidth: CGFloat {
        switch self {
        case .week: 28
        case .month: 9
        case .quarter: 3
        case .year: 1
        }
    }
}

private struct TimelineMove: Equatable {
    enum Part: Equatable {
        case start(String)
        case target(String)
        case span(String)
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

/// A curve from the end of the blocker's bar to the start of the bar it
/// blocks, with a dot at each end. With `hitWidth`, the band around the
/// curve a click lands on.
private struct DependencyLine: Shape {
    let from: CGPoint
    let to: CGPoint
    var hitWidth: CGFloat?

    func path(in rect: CGRect) -> Path {
        let reach = max(abs(to.x - from.x) / 2, 24)
        var curve = Path()
        curve.move(to: from)
        curve.addCurve(to: to, control1: CGPoint(x: from.x + reach, y: from.y),
                       control2: CGPoint(x: to.x - reach, y: to.y))
        if let hitWidth { return curve.strokedPath(StrokeStyle(lineWidth: hitWidth, lineCap: .round)) }
        var line = curve.strokedPath(StrokeStyle(lineWidth: 1.5, lineCap: .round))
        for point in [from, to] {
            line.addEllipse(in: CGRect(x: point.x - 3, y: point.y - 3, width: 6, height: 6))
        }
        return line
    }
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
        let lead = switch zoom {
        case .week, .month: 1
        case .quarter: 3
        case .year: 6
        }
        let first = calendar.date(byAdding: .month, value: -lead, to: monthStart(min(dates.min() ?? today, today))) ?? today
        let last = calendar.date(byAdding: .month, value: lead + 2, to: monthStart(max(dates.max() ?? today, today))) ?? today
        let days = max(calendar.dateComponents([.day], from: first, to: last).day ?? 1, 1)
        var groups: [Group] = []
        var ticks: [Tick] = []
        for day in 0..<days {
            guard let date = calendar.date(byAdding: .day, value: day, to: first) else { continue }
            let parts = calendar.dateComponents([.year, .month, .day, .weekday], from: date)
            let group = switch zoom {
            case .week, .month: date.formatted(.dateTime.month(.wide).year())
            case .quarter: "Q\(((parts.month ?? 1) - 1) / 3 + 1) \(parts.year ?? 0)"
            case .year: "\(parts.year ?? 0)"
            }
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
            case .quarter where parts.day == 1, .year where parts.day == 1:
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
/// diamonds, today as a line. Dragging a bar moves both its dates, dragging
/// one of its ends or a diamond moves that date, through the saves the
/// project page makes.
struct ProjectsTimeline: View {
    let store: ProjectsStore
    @Environment(AppModel.self) private var model
    let projects: [PlanningProjectSummary]

    @AppStorage("projects.timeline.zoom") private var zoom = ProjectsTimelineZoom.month
    @State private var move: TimelineMove?
    @State private var position = ScrollPosition(edge: .leading)
    @State private var scrolled: CGFloat = 0
    @State private var pickedLine: String?
    @State private var hoveredBar: String?
    @State private var linking: TimelineLinkDrag?

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
            cycleBands(axis, height: height)
            header(axis)
            todayMark(axis, height: height)
            dependencyLines(rows, axis)
            ForEach(Array(rows.enumerated()), id: \.element.id) { index, row in
                lane(row, axis)
                    .offset(y: Self.headerHeight + CGFloat(index) * Self.rowHeight)
            }
            if let linking {
                DependencyLine(from: linking.from, to: linking.to)
                    .fill(Self.kept.opacity(0.8))
                    .allowsHitTesting(false)
            }
        }
        .frame(width: axis.width, height: height, alignment: .topLeading)
        .coordinateSpace(.named(Self.space))
    }

    /// Each cycle as a faint band behind the lanes, named at its foot; the
    /// one under way in the accent.
    private func cycleBands(_ axis: TimelineAxis, height: CGFloat) -> some View {
        ZStack(alignment: .topLeading) {
            ForEach(model.work.cycles) { cycle in
                if let (first, last) = CycleFormat.span(cycle) {
                    let x = axis.x(first)
                    let width = axis.x(last) + axis.dayWidth - x
                    Rectangle()
                        .fill(cycle.state == .current ? Theme.accent.opacity(0.08) : Theme.wash)
                        .overlay(alignment: .bottomLeading) {
                            Text(cycle.title)
                                .font(.system(size: 10, weight: .medium))
                                .foregroundStyle(Theme.inkTertiary)
                                .lineLimit(1)
                                .frame(maxWidth: max(width - 8, 0), alignment: .leading)
                                .padding(.leading, 4)
                                .padding(.bottom, 3)
                                .opacity(width >= 48 ? 1 : 0)
                        }
                        .frame(width: width, height: height - Self.headerHeight)
                        .offset(x: x, y: Self.headerHeight)
                        .help("\(cycle.title): \(CycleFormat.title(cycle))")
                        .accessibilityLabel("\(cycle.title), \(CycleFormat.title(cycle))")
                }
            }
        }
        .frame(width: axis.width, height: height, alignment: .topLeading)
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

    /// Each dependency between two dated rows, blue while the blocker ends
    /// before the blocked project starts, red once it does not; the dates
    /// being dragged count. A click offers to open either project or remove it.
    private func dependencyLines(_ rows: [TimelineRow], _ axis: TimelineAxis) -> some View {
        let middle = Self.barTop + Self.barHeight / 2
        let placed = Dictionary(uniqueKeysWithValues: rows.enumerated().compactMap { index, row in
            row.span.map { (row.id, (row: row, span: $0, y: Self.headerHeight + CGFloat(index) * Self.rowHeight + middle)) }
        })
        return ZStack(alignment: .topLeading) {
            ForEach(store.dependencies) { dependency in
                if let blocker = placed[dependency.blockerId], let blocked = placed[dependency.blockedId] {
                    let broken = PlanningProjectDependency.breaks(blockerStatus: blocker.row.project.status,
                                                                  blockerEnd: blocker.span.end, blockedStart: blocked.span.start)
                    let from = CGPoint(x: axis.x(blocker.span.end) + axis.dayWidth, y: blocker.y)
                    let to = CGPoint(x: axis.x(blocked.span.start), y: blocked.y)
                    let title = "\(blocked.row.project.title) waits on \(blocker.row.project.title)"
                    DependencyLine(from: from, to: to)
                        .fill(broken ? Theme.live : Self.kept)
                        .contentShape(DependencyLine(from: from, to: to, hitWidth: 10))
                        .onTapGesture { pickedLine = dependency.id }
                        .popover(isPresented: Binding(get: { pickedLine == dependency.id },
                                                      set: { if !$0, pickedLine == dependency.id { pickedLine = nil } }),
                                 attachmentAnchor: .rect(.rect(CGRect(x: (from.x + to.x) / 2 - 4, y: (from.y + to.y) / 2 - 4,
                                                                      width: 8, height: 8)))) {
                            lineMenu(dependency, title: title, broken: broken,
                                     blocker: blocker.row.project, blocked: blocked.row.project)
                        }
                        .help(broken ? "\(title), but it ends after \(blocked.row.project.title) starts" : title)
                        .accessibilityElement()
                        .accessibilityLabel(broken ? "\(title), broken" : title)
                        .accessibilityAddTraits(.isButton)
                        .accessibilityAction { pickedLine = dependency.id }
                }
            }
        }
    }

    private func lineMenu(_ dependency: PlanningProjectDependency, title: String, broken: Bool,
                          blocker: PlanningProject, blocked: PlanningProject) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title).font(TypeScale.label(12.5)).foregroundStyle(Theme.ink)
            if broken {
                Text("\(blocker.title) ends after \(blocked.title) starts.").metaText(Theme.live)
            }
            Divider()
            Button("Open \(blocker.title)") { pickedLine = nil; store.open(blocker.id) }.buttonStyle(.plain)
            Button("Open \(blocked.title)") { pickedLine = nil; store.open(blocked.id) }.buttonStyle(.plain)
            Button("Remove dependency") {
                pickedLine = nil
                Task { await store.removeDependency(dependency) }
            }
            .buttonStyle(.plain)
            .foregroundStyle(Theme.live)
        }
        .font(TypeScale.label(12.5))
        .padding(12)
        .frame(minWidth: 220, alignment: .leading)
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
        let bar = RoundedRectangle(cornerRadius: Theme.radiusRow)
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
            .contentShape(RoundedRectangle(cornerRadius: Theme.radiusRow))
            .pointerStyle(.grabIdle)
            .gesture(dragging(.span(project.id), from: project.startAtMs ?? project.targetAtMs))
            .help("\(project.title): \(Self.day(span.start)) – \(Self.day(span.end)). Drag to move both dates.")
            .accessibilityElement(children: .contain)
            .accessibilityLabel(project.title)
            .accessibilityAction(named: "Move a day earlier") {
                commit(.span(project.id), days: -1, from: project.startAtMs ?? project.targetAtMs)
            }
            .accessibilityAction(named: "Move a day later") {
                commit(.span(project.id), days: 1, from: project.startAtMs ?? project.targetAtMs)
            }
        return HStack(spacing: 3) {
            bar
            connector(project, tint)
                .opacity(hoveredBar == project.id || linking?.blockerId == project.id ? 1 : 0)
        }
        .contentShape(Rectangle())
        .onHover { inside in
            if inside { hoveredBar = project.id } else if hoveredBar == project.id { hoveredBar = nil }
        }
        .offset(x: x, y: Self.barTop)
        .contextMenu { ProjectDependencyMenu(store: store, project: project) }
    }

    /// The circle past a bar's end: dragged onto another bar, it makes that
    /// project wait on this one.
    private func connector(_ project: PlanningProject, _ tint: Color) -> some View {
        Circle()
            .strokeBorder(tint, lineWidth: 1.5)
            .background(Circle().fill(Theme.surface))
            .frame(width: 9, height: 9)
            .frame(width: 14, height: Self.barHeight)
            .contentShape(Rectangle())
            .pointerStyle(.grabIdle)
            .gesture(DragGesture(minimumDistance: 2, coordinateSpace: .named(Self.space))
                .onChanged { value in
                    linking = TimelineLinkDrag(blockerId: project.id, from: value.startLocation, to: value.location)
                }
                .onEnded { value in
                    linking = nil
                    guard let target = dropTarget(at: value.location), target != project.id else { return }
                    Task { await store.addDependency(blockerId: project.id, blockedId: target) }
                })
            .help("Drag onto a project that waits on \(project.title)")
            .accessibilityHidden(true)
    }

    /// The dated project whose bar is under `point`, in the chart's space.
    private func dropTarget(at point: CGPoint) -> String? {
        let rows = rows()
        let axis = axis(rows)
        let index = Int(((point.y - Self.headerHeight) / Self.rowHeight).rounded(.down))
        guard rows.indices.contains(index), let span = rows[index].span,
              point.x >= axis.x(span.start) - 8, point.x <= axis.x(span.end) + axis.dayWidth + 8 else { return nil }
        return rows[index].id
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
            case let .span(id):
                guard let project = projects.first(where: { $0.id == id })?.project,
                      let span = span(of: project, moved: moved) else { break }
                await store.saveProject(project, PlanningProjectSave(
                    startAtMs: project.startAtMs.map { _ in .set(ProjectsStore.milliseconds(span.start)) },
                    targetAtMs: project.targetAtMs.map { _ in .set(ProjectsStore.milliseconds(span.end)) }))
            case let .milestone(id):
                guard let milestone = store.details.values.lazy.flatMap(\.milestones).first(where: { $0.id == id }),
                      let due = due(of: milestone, moved: moved) else { break }
                await store.saveMilestone(milestone, PlanningMilestoneSave(dueAtMs: .set(ProjectsStore.milliseconds(due))))
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
        case .span(project.id) where moved.from == (project.startAtMs ?? project.targetAtMs):
            return (shift(start, moved.days), shift(end, moved.days))
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

    private static var kept: Color { Theme.label(hue: 285) }

    private static func day(_ date: Date) -> String {
        date.formatted(date: .abbreviated, time: .omitted)
    }
}

/// A dependency being drawn from a bar's end, in the chart's space.
private struct TimelineLinkDrag: Equatable {
    let blockerId: String
    let from: CGPoint
    let to: CGPoint
}

/// Blocked by and Blocking submenus for one project, and one taking a link
/// off: for its timeline bar, its list row and its page's menu.
struct ProjectDependencyMenu: View {
    let store: ProjectsStore
    let project: PlanningProject

    var body: some View {
        let choices = store.dependencyChoices(for: project.id)
        let links = store.dependencies.filter { $0.blockerId == project.id || $0.blockedId == project.id }
        Menu("Blocked by") {
            ForEach(choices) { other in
                Button(other.title) { Task { await store.addDependency(blockerId: other.id, blockedId: project.id) } }
            }
        }
        .disabled(choices.isEmpty)
        Menu("Blocking") {
            ForEach(choices) { other in
                Button(other.title) { Task { await store.addDependency(blockerId: project.id, blockedId: other.id) } }
            }
        }
        .disabled(choices.isEmpty)
        Menu("Remove link") {
            ForEach(links) { link in
                Button(link.blockedId == project.id
                    ? "Waits on \(store.projectTitle(link.blockerId))"
                    : "\(store.projectTitle(link.blockedId)) waits on this") {
                    Task { await store.removeDependency(link) }
                }
            }
        }
        .disabled(links.isEmpty)
    }
}

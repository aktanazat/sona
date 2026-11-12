import SwiftUI

struct PlannerCalendarView: View {
    let store: ProjectsStore
    var material: AppearanceMaterial = .solid
    let openProject: (String) -> Void
    let openMeeting: (String) -> Void
    let showProjects: () -> Void

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @AppStorage("projects.layout") private var layout = ProjectsLayout.list
    @State private var selectedDate = Date()
    @State private var displayedMonth = Date()
    private let calendar = Calendar.current

    private var monthInterval: DateInterval? { calendar.dateInterval(of: .month, for: displayedMonth) }
    private var visibleInterval: DateInterval? {
        guard let month = monthInterval else { return nil }
        let weekday = calendar.component(.weekday, from: month.start)
        let leading = (weekday - calendar.firstWeekday + 7) % 7
        guard let start = calendar.date(byAdding: .day, value: -leading, to: month.start),
              let days = calendar.range(of: .day, in: .month, for: displayedMonth),
              let end = calendar.date(byAdding: .day, value: ((leading + days.count + 6) / 7) * 7, to: start)
        else { return nil }
        return DateInterval(start: start, end: end)
    }
    private var dates: [Date] {
        guard let range = visibleInterval else { return [] }
        let count = calendar.dateComponents([.day], from: range.start, to: range.end).day ?? 0
        return (0..<count).compactMap { calendar.date(byAdding: .day, value: $0, to: range.start) }
    }
    private var weekdaySymbols: [String] {
        let symbols = calendar.shortStandaloneWeekdaySymbols
        let start = calendar.firstWeekday - 1
        return Array(symbols[start...]) + Array(symbols[..<start])
    }
    private var dayItems: [PlanningCalendarItem] {
        (store.calendarItems ?? []).filter { calendar.isDate($0.date, inSameDayAs: selectedDate) }
            .sorted { $0.dueAtMs == $1.dueAtMs ? $0.key < $1.key : $0.dueAtMs < $1.dueAtMs }
    }
    private var itemsByDay: [Date: [PlanningCalendarItem]] {
        Dictionary(grouping: store.calendarItems ?? []) { calendar.startOfDay(for: $0.date) }
    }

    var body: some View {
        Page {
            if let error = store.calendarError {
                PlannerFailure(message: error) {
                    if let range = visibleInterval { Task { await store.loadCalendar(range) } }
                }
                .padding(.bottom, 20)
            }
            ViewThatFits(in: .horizontal) {
                HStack(alignment: .top, spacing: 28) {
                    monthPicker.frame(width: 300)
                    agenda.frame(minWidth: 260, maxWidth: .infinity, alignment: .leading)
                }
                VStack(alignment: .leading, spacing: 28) {
                    monthPicker
                    agenda
                }
            }
        }
        .pageBar {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                ProjectsTabs()
                Text("Calendar").metaText()
            }
        } trailing: {
            HStack(spacing: 10) { calendarControls }
        }
        .task(id: visibleInterval) {
            if let range = visibleInterval { await store.loadCalendar(range) }
        }
        .onDisappear { store.stopCalendar() }
        .transaction {
            if reduceMotion { $0.animation = nil; $0.disablesAnimations = true }
        }
    }

    @ViewBuilder
    private var calendarControls: some View {
        Button {
            if let range = visibleInterval { Task { await store.loadCalendar(range) } }
        } label: { Image(systemName: "arrow.clockwise") }
            .buttonStyle(.icon)
            .accessibilityLabel("Refresh calendar")
            .help("Refresh calendar")
            .disabled(store.calendarLoading)
        Button("Today") {
            let today = Date()
            selectedDate = today
            displayedMonth = today
        }
        .buttonStyle(.secondary)
        SegmentedSwitch(items: ProjectsViewChoice.items, selection: .calendar, iconOnly: true) { choice in
            switch choice {
            case .list:
                layout = .list
                showProjects()
            case .board:
                layout = .board
                showProjects()
            case .timeline:
                layout = .timeline
                showProjects()
            case .calendar:
                break
            }
        }
    }

    private var monthPicker: some View {
        VStack(alignment: .leading, spacing: 18) {
            PlannerNavigationSurface(material: material) {
                HStack(spacing: 12) {
                    Button { moveMonth(-1) } label: { Image(systemName: "chevron.left") }
                        .buttonStyle(.icon)
                        .accessibilityLabel("Previous month")
                    Spacer(minLength: 0)
                    Text(displayedMonth.formatted(.dateTime.month(.wide).year()))
                        .headlineText()
                        .accessibilityAddTraits(.isHeader)
                    Spacer(minLength: 0)
                    Button { moveMonth(1) } label: { Image(systemName: "chevron.right") }
                        .buttonStyle(.icon)
                        .accessibilityLabel("Next month")
                }
            }
            let grouped = itemsByDay
            LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 3), count: 7), spacing: 5) {
                ForEach(Array(weekdaySymbols.enumerated()), id: \.offset) { _, symbol in
                    Text(symbol).metaText(Theme.inkSecondary)
                        .frame(maxWidth: .infinity)
                        .padding(.bottom, 4)
                        .accessibilityHidden(true)
                }
                ForEach(dates, id: \.self) { date in
                    dateCell(date, count: grouped[calendar.startOfDay(for: date)]?.count ?? 0)
                }
            }
            DatePicker("Go to date", selection: $selectedDate, displayedComponents: .date)
                .datePickerStyle(.field)
                .onChange(of: selectedDate) { _, date in
                    if !calendar.isDate(date, equalTo: displayedMonth, toGranularity: .month) { displayedMonth = date }
                }
        }
    }

    private func dateCell(_ date: Date, count: Int) -> some View {
        let selected = calendar.isDate(date, inSameDayAs: selectedDate)
        let inMonth = calendar.isDate(date, equalTo: displayedMonth, toGranularity: .month)
        let today = calendar.isDateInToday(date)
        return Button {
            selectedDate = date
        } label: {
            VStack(spacing: 3) {
                Text(date.formatted(.dateTime.day()))
                    .font(.system(size: 13, weight: selected || today ? .semibold : .regular))
                    .foregroundStyle(inMonth ? Theme.ink : Theme.inkTertiary)
                Text(count > 0 ? String(count) : " ")
                    .font(.system(size: 10, weight: .medium))
                    .foregroundStyle(Theme.inkSecondary)
                    .monospacedDigit()
            }
            .frame(maxWidth: .infinity, minHeight: 42)
            .background(selected ? Theme.selection : .clear, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            .overlay {
                RoundedRectangle(cornerRadius: Theme.radiusControl)
                    .strokeBorder(today ? Theme.accent : .clear, lineWidth: 1)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(date.formatted(date: .complete, time: .omitted))
        .accessibilityValue("\(count) items\(today ? ", today" : "")")
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    private var agenda: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(selectedDate.formatted(.dateTime.weekday(.wide).month(.abbreviated).day()))
                .sectionLabel()
                .accessibilityAddTraits(.isHeader)
            if store.calendarItems == nil {
                if store.calendarLoading { PlannerLoadingRows(label: "Loading calendar") }
                else if store.calendarError == nil { Text("Calendar unavailable.").metaText() }
            } else {
                Card {
                    if dayItems.isEmpty {
                        CardLine(store.calendarError == nil ? "No due work or recorded meetings." : "No planned work due.")
                    }
                    ForEach(dayItems, id: \.key) { item in
                        calendarRow(item)
                    }
                }
            }
        }
    }

    private func calendarRow(_ item: PlanningCalendarItem) -> some View {
        let canOpen: Bool = item.kind == .meeting ? item.meetingId != nil : item.projectId != nil
        return CardRow(action: canOpen ? { open(item) } : nil) {
            VStack(alignment: .leading, spacing: 6) {
                Text(item.title).bodyText()
                    .fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 8) {
                    Text(kindLabel(item.kind))
                    Text(item.date.formatted(date: .omitted, time: .shortened)).monospacedDigit()
                }
                .metaText(Theme.inkSecondary)
                if let projectId = item.projectId,
                   let title = store.projects?.first(where: { $0.id == projectId })?.project.title {
                    Text(title).metaText()
                }
                if !canOpen { Text("Source unavailable").metaText() }
            }
        } trailing: {
            if canOpen {
                Image(systemName: "chevron.right")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(Theme.inkTertiary)
                    .accessibilityHidden(true)
            }
        }
    }

    private func kindLabel(_ kind: PlanningCalendarItem.Kind) -> String {
        switch kind {
        case .task: "Task due"
        case .milestone: "Milestone due"
        case .meeting: "Recorded meeting"
        }
    }

    private func moveMonth(_ offset: Int) {
        guard let month = monthInterval,
              let date = calendar.date(byAdding: .month, value: offset, to: month.start) else { return }
        displayedMonth = date
        selectedDate = date
    }

    private func open(_ item: PlanningCalendarItem) {
        if item.kind == .meeting, let id = item.meetingId { openMeeting(id) }
        else if let id = item.projectId { openProject(id) }
    }
}

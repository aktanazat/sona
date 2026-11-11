import Foundation

/// A task's priority as the screens name and order it. The wire value is the
/// raw number: 0 none, 1 urgent through 4 low.
enum TaskPriority: Int, CaseIterable, Identifiable {
    case none, urgent, high, medium, low

    init(_ value: Int) {
        self = TaskPriority(rawValue: value) ?? .none
    }

    var id: Int { rawValue }

    var label: String {
        switch self {
        case .none: "No priority"
        case .urgent: "Urgent"
        case .high: "High"
        case .medium: "Medium"
        case .low: "Low"
        }
    }

    /// Urgent first and no priority last, when a list orders by priority.
    var rank: Int { self == .none ? 5 : rawValue }

    static let menuOrder: [TaskPriority] = [.none, .urgent, .high, .medium, .low]
}

/// What a row may show besides its status and title. The raw values are what
/// a saved view stores.
enum TaskProperty: String, CaseIterable, Identifiable {
    case number = "id"
    case priority, labels, project, owner, due, meeting, updated

    var id: String { rawValue }

    var label: String {
        switch self {
        case .number: "ID"
        case .priority: "Priority"
        case .labels: "Labels"
        case .project: "Project"
        case .owner: "Owner"
        case .due: "Due date"
        case .meeting: "Meeting"
        case .updated: "Updated"
        }
    }

    static let standard: [String] = [number, priority, labels, project, owner, due].map(\.rawValue)
}

/// Where a task falls in My work, in the order the list shows them, after
/// Follow up: waits whose day to ask has come.
enum TaskBucket: Int, CaseIterable {
    case overdue, today, blocked, thisWeek, later, noDate, followUp

    var title: String {
        switch self {
        case .overdue: "Overdue"
        case .today: "Today"
        case .blocked: "Blocked"
        case .thisWeek: "This week"
        case .later: "Later"
        case .noDate: "No date"
        case .followUp: "Follow up"
        }
    }

    var symbol: String {
        switch self {
        case .blocked: "hand.raised"
        case .followUp: "arrowshape.turn.up.right"
        case .overdue, .today, .thisWeek, .later, .noDate: "calendar"
        }
    }

    /// `blocked` only in My work: a task waiting on another comes before
    /// the week ahead unless its own date already presses.
    static func of(_ row: WorkItemRow, now: Int64, blocking: Bool) -> TaskBucket {
        guard let due = row.dueAtMs else {
            return blocking && row.blockedByOpenCount > 0 ? .blocked : .noDate
        }
        let days = PlanningFormat.days(from: now, to: due)
        if days < 0 { return .overdue }
        if days == 0 { return .today }
        if blocking && row.blockedByOpenCount > 0 { return .blocked }
        return days < 7 ? .thisWeek : .later
    }
}

/// The mark a group's header draws before its title.
enum TaskGroupMark: Equatable {
    case status(WorkStatus)
    case priority(TaskPriority)
    case owner(AvatarFace)
    case project
    case meeting
    case bucket(TaskBucket)
    case focus
    case agent
    case none
}

struct TaskGroup: Identifiable, Equatable {
    let id: String
    let title: String
    let mark: TaskGroupMark
    var rows: [WorkItemRow]
    /// What a task dropped on this group, or added from its header, takes.
    var patch: WorkPatch?
}

/// Filtering, ordering and grouping, on the rows the store already holds, so
/// a filter or a regrouping lands in the same frame as the key that asked.
enum TaskListing {
    /// The statuses in the order a list groups them.
    static let statusOrder: [WorkStatus] = [.triage, .inProgress, .notStarted, .backlog, .done, .canceled, .duplicate]

    /// A query ready to test one list's rows: its due windows are worked out
    /// once, for the day `now` falls on, not once per row.
    struct Filter {
        let query: WorkQuery
        let now: Int64
        private let due: [DueSpan]?
        private let cycle: CycleWant?
        private let conditions: [Condition]

        private enum Value {
            case none, me, text(String), day(Int64, Int64), at(Int64)
        }

        /// One condition with its values read the way the core reads them.
        private struct Condition {
            let field: WorkConditionField
            let op: WorkConditionOp
            let values: [Value]

            init(_ condition: WorkCondition, now: Int64, cycles: [WorkCycle]) {
                field = condition.field
                op = condition.op
                values = condition.values.map { [field] raw in
                    let text = raw.trimmingCharacters(in: .whitespaces).lowercased()
                    if text == "none", field != .priority { return .none }
                    switch field {
                    case .owner where text == "me": return .me
                    case .cycle where text == "current" || text == "next":
                        let state: WorkCycleState = text == "current" ? .current : .upcoming
                        return cycles.first { $0.state == state }.map { .text(String($0.number)) } ?? .none
                    case .priority:
                        let names = ["none": "0", "no_priority": "0", "urgent": "1", "high": "2", "medium": "3", "low": "4"]
                        return .text(names[text] ?? text)
                    case .due, .created, .updated:
                        return Self.day(text, now: now)
                    case .status: return .text(text)
                    case .owner, .cycle, .project, .milestone, .label:
                        return .text(raw.trimmingCharacters(in: .whitespaces))
                    }
                }
            }

            private static func day(_ text: String, now: Int64) -> Value {
                if let ms = Int64(text) { return .at(ms) }
                let calendar = Calendar.current
                let today = calendar.startOfDay(for: PlanningFormat.moment(now))
                let parts = text.split(separator: "-").compactMap { Int($0) }
                let start: Date? = switch text {
                case "today": today
                case "tomorrow": calendar.date(byAdding: .day, value: 1, to: today)
                case "yesterday": calendar.date(byAdding: .day, value: -1, to: today)
                default: parts.count == 3
                    ? calendar.date(from: DateComponents(year: parts[0], month: parts[1], day: parts[2])) : nil
                }
                guard let start, let end = calendar.date(byAdding: .day, value: 1, to: start) else { return .day(0, 0) }
                return .day(TaskDates.milliseconds(start), TaskDates.milliseconds(end))
            }

            private func text(_ row: WorkItemRow) -> String? {
                switch field {
                case .status: row.status.rawValue
                case .priority: String(row.priority)
                case .owner: row.ownerPersonId
                case .project: row.projectId
                case .milestone: row.milestoneId
                case .cycle: row.cycle.map { String($0) }
                case .label, .due, .created, .updated: nil
                }
            }

            private func at(_ row: WorkItemRow) -> Int64? {
                switch field {
                case .due: row.dueAtMs
                case .created: row.createdAtMs
                case .updated: row.updatedAtMs
                default: nil
                }
            }

            private func hits(_ row: WorkItemRow, _ value: Value) -> Bool {
                switch (field, value) {
                case (.label, .none): row.labelIds.isEmpty
                case let (.label, .text(id)): row.labelIds.contains(id)
                case (.owner, .me): row.ownerIsSelf
                case (_, .me): false
                case (_, .none): field.dated ? at(row) == nil : text(row) == nil
                case let (_, .day(start, end)): at(row).map { start <= $0 && $0 < end } ?? false
                case let (_, .at(ms)): at(row) == ms
                case let (_, .text(value)): text(row) == value
                }
            }

            func holds(_ row: WorkItemRow) -> Bool {
                switch op {
                case .is where field == .label, .includesAll: return values.allSatisfy { hits(row, $0) }
                case .is, .anyOf, .includesAny: return values.contains { hits(row, $0) }
                case .isNot, .noneOf: return !values.contains { hits(row, $0) }
                case .before, .after:
                    guard let at = at(row) else { return false }
                    return values.contains { value in
                        switch (op, value) {
                        case let (.before, .day(start, _)), let (.before, .at(start)): at < start
                        case let (.after, .day(_, end)): at >= end
                        case let (.after, .at(ms)): at > ms
                        default: false
                        }
                    }
                }
            }
        }

        private enum CycleWant {
            case number(Int64), noCycle, nothing

            func holds(_ row: WorkItemRow) -> Bool {
                switch self {
                case let .number(number): row.cycle == number
                case .noCycle: row.cycle == nil
                case .nothing: false
                }
            }
        }

        init(_ query: WorkQuery, now: Int64, cycles: [WorkCycle] = []) {
            self.query = query
            self.now = now
            due = query.due.map { DueSpan.pin($0, now: now) }
            let current = cycles.first { $0.state == .current }
            let named: WorkCycle? = switch query.cycle {
            case .current?: current
            case .next?: cycles.first { $0.startsAtMs == current?.endsAtMs }
            case .noCycle?, .number?, nil: nil
            }
            cycle = switch query.cycle {
            case nil: nil
            case .noCycle?: .noCycle
            case let .number(number)?: .number(number)
            case .current?, .next?: named.map { .number($0.number) } ?? .nothing
            }
            conditions = (query.filters ?? []).map { Condition($0, now: now, cycles: cycles) }
        }

        func matches(_ row: WorkItemRow) -> Bool {
            if !query.includeArchived, row.archivedAtMs != nil { return false }
            if !query.includeSnoozed, let wake = row.snoozedUntilMs, wake > now { return false }
            if let statuses = query.statuses, !statuses.contains(row.status) { return false }
            if let direction = query.direction, row.direction != direction { return false }
            if let owners = query.ownerPersonIds, !owners.contains(row.ownerPersonId ?? "") { return false }
            if let projects = query.projectIds, !projects.contains(row.projectId ?? "none") { return false }
            if let labels = query.labelIds, !labels.contains(where: row.labelIds.contains) { return false }
            if let priorities = query.priorities, !priorities.contains(row.priority) { return false }
            if let before = query.dueBeforeMs {
                guard let due = row.dueAtMs, due < before else { return false }
            }
            if let due, !due.contains(where: { $0.holds(row) }) { return false }
            if let cycle, !cycle.holds(row) { return false }
            if !conditions.allSatisfy({ $0.holds(row) }) { return false }
            if let meeting = query.sourceMeetingId, row.sourceMeetingId != meeting { return false }
            if let search = query.search?.trimmingCharacters(in: .whitespaces), !search.isEmpty {
                let number = search.hasPrefix("#") ? String(search.dropFirst()) : search
                guard row.title.localizedCaseInsensitiveContains(search)
                    || (row.details ?? "").localizedCaseInsensitiveContains(search)
                    || "\(row.number)" == number
                else { return false }
            }
            return true
        }
    }

    /// A due window pinned to one day on this Mac's clock, as the core pins it.
    enum DueSpan {
        /// Accepted, unfinished work due before this time.
        case overdue(Int64)
        /// Due at or after the first time and before the second.
        case between(Int64, Int64)
        case noDate

        /// Each window as it stands on the day `now` falls on. Weeks start on
        /// Monday.
        static func pin(_ windows: [WorkDue], now: Int64) -> [DueSpan] {
            let calendar = Calendar.current
            let today = calendar.startOfDay(for: PlanningFormat.moment(now))
            func start(_ days: Int) -> Int64 {
                TaskDates.milliseconds(calendar.date(byAdding: .day, value: days, to: today) ?? today)
            }
            let first = start(0)
            let sinceMonday = (calendar.component(.weekday, from: today) + 5) % 7
            return windows.map { window in
                switch window {
                case .overdue: .overdue(first)
                case .today: .between(first, start(1))
                case .thisWeek: .between(first, start(7 - sinceMonday))
                case let .nextDays(days): .between(first, start(Int(days) + 1))
                case .noDate: .noDate
                }
            }
        }

        func holds(_ row: WorkItemRow) -> Bool {
            switch (self, row.dueAtMs) {
            case let (.overdue(before), due?): due < before && row.status.isOpen
            case let (.between(from, until), due?): from <= due && due < until
            case (.noDate, nil): true
            case (.overdue, nil), (.between, nil), (.noDate, _?): false
            }
        }
    }

    static func ordered(_ rows: [WorkItemRow], by order: WorkOrderBy) -> [WorkItemRow] {
        rows.sorted { left, right in
            switch order {
            case .priority:
                let (a, b) = (TaskPriority(left.priority).rank, TaskPriority(right.priority).rank)
                if a != b { return a < b }
                return earlierDue(left, right) ?? (left.number > right.number)
            case .due:
                if let due = earlierDue(left, right) { return due }
                let (a, b) = (TaskPriority(left.priority).rank, TaskPriority(right.priority).rank)
                return a != b ? a < b : left.number > right.number
            case .updated:
                return left.updatedAtMs != right.updatedAtMs ? left.updatedAtMs > right.updatedAtMs : left.number > right.number
            case .created:
                return left.createdAtMs != right.createdAtMs ? left.createdAtMs > right.createdAtMs : left.number > right.number
            case .manual:
                return left.sortOrder != right.sortOrder ? left.sortOrder < right.sortOrder : left.number < right.number
            }
        }
    }

    /// Dated work before undated, sooner first; nil when both dates tie.
    private static func earlierDue(_ left: WorkItemRow, _ right: WorkItemRow) -> Bool? {
        switch (left.dueAtMs, right.dueAtMs) {
        case let (a?, b?) where a != b: a < b
        case (.some, nil): true
        case (nil, .some): false
        default: nil
        }
    }

    static func groups(
        _ rows: [WorkItemRow], display: WorkDisplay, now: Int64,
        projects: [WorkProjectChoice], people: [WorkPersonChoice]
    ) -> [TaskGroup] {
        let rows = ordered(rows, by: display.orderBy)
        switch display.groupBy {
        case .none:
            return [TaskGroup(id: "all", title: "All", mark: .none, rows: rows)]
        case .status:
            let present = Set(rows.map(\.status))
            return statusOrder
                .filter { present.contains($0) || (display.showEmptyGroups && $0 != .triage && $0 != .duplicate) }
                .map { status in
                    TaskGroup(id: "status:\(status.rawValue)", title: status.label, mark: .status(status),
                              rows: rows.filter { $0.status == status }, patch: WorkPatch(status: status))
                }
        case .priority:
            let present = Set(rows.map(\.priority))
            return [TaskPriority.urgent, .high, .medium, .low, .none]
                .filter { present.contains($0.rawValue) || display.showEmptyGroups }
                .map { priority in
                    TaskGroup(id: "priority:\(priority.rawValue)", title: priority.label, mark: .priority(priority),
                              rows: rows.filter { $0.priority == priority.rawValue },
                              patch: WorkPatch(priority: priority.rawValue))
                }
        case .due:
            let buckets: [TaskBucket] = [.overdue, .today, .thisWeek, .later, .noDate]
            let keyed = Dictionary(grouping: rows) { TaskBucket.of($0, now: now, blocking: false) }
            return buckets
                .filter { keyed[$0] != nil || display.showEmptyGroups }
                .map { bucket in
                    TaskGroup(id: "due:\(bucket.rawValue)", title: bucket.title, mark: .bucket(bucket),
                              rows: keyed[bucket] ?? [], patch: bucket == .noDate ? WorkPatch(dueAtMs: .clear) : nil)
                }
        case .owner:
            var groups: [TaskGroup] = []
            let mine = rows.filter(\.ownerIsSelf)
            if !mine.isEmpty || display.showEmptyGroups {
                groups.append(TaskGroup(id: "owner:self", title: "Me", mark: .owner(AvatarFace(kind: .me, name: "Me")),
                                        rows: mine, patch: WorkPatch(owner: .myself)))
            }
            let named = Dictionary(grouping: rows.filter { !$0.ownerIsSelf && $0.ownerPersonId != nil }) { $0.ownerPersonId ?? "" }
            let ids = display.showEmptyGroups ? people.map(\.id) : Array(named.keys)
            let names = ids.map { id in (id: id, name: people.first { $0.id == id }?.name ?? named[id]?.first?.ownerName ?? "Someone") }
            for (id, name) in names.sorted(by: byName) {
                groups.append(TaskGroup(id: "owner:\(id)", title: name,
                                        mark: .owner(AvatarFace(kind: .person(id), name: name)),
                                        rows: named[id] ?? [], patch: WorkPatch(owner: .person(id))))
            }
            let nobody = rows.filter { !$0.ownerIsSelf && $0.ownerPersonId == nil }
            if !nobody.isEmpty || display.showEmptyGroups {
                groups.append(TaskGroup(id: "owner:nobody", title: "No owner",
                                        mark: .owner(AvatarFace(kind: .unassigned, name: "No owner")),
                                        rows: nobody, patch: WorkPatch(owner: .nobody)))
            }
            return groups
        case .project:
            let keyed = Dictionary(grouping: rows) { $0.projectId ?? "" }
            let listed = display.showEmptyGroups
                ? projects.filter { $0.status != .completed }.map(\.id)
                : keyed.keys.filter { !$0.isEmpty }
            var groups = listed
                .map { id in (id: id, name: projects.first { $0.id == id }?.title ?? keyed[id]?.first?.projectTitle ?? "Project") }
                .sorted(by: byName)
                .map { id, title in
                    TaskGroup(id: "project:\(id)", title: title, mark: .project, rows: keyed[id] ?? [],
                              patch: WorkPatch(projectId: .set(id)))
                }
            if keyed[""] != nil || display.showEmptyGroups {
                groups.append(TaskGroup(id: "project:none", title: "No project", mark: .project, rows: keyed[""] ?? [],
                                        patch: WorkPatch(projectId: .clear)))
            }
            return groups
        case .meeting:
            let keyed = Dictionary(grouping: rows) { $0.sourceMeetingId ?? "" }
            var groups = keyed.keys.filter { !$0.isEmpty }
                .sorted { left, right in
                    let (a, b) = (keyed[left]?.first?.createdAtMs ?? 0, keyed[right]?.first?.createdAtMs ?? 0)
                    return a != b ? a > b : left < right
                }
                .map { id in
                    TaskGroup(id: "meeting:\(id)", title: keyed[id]?.first?.sourceMeetingTitle ?? "Meeting",
                              mark: .meeting, rows: keyed[id] ?? [])
                }
            if let loose = keyed[""] {
                groups.append(TaskGroup(id: "meeting:none", title: "Not from a meeting", mark: .meeting, rows: loose))
            }
            return groups
        }
    }

    /// Names in Finder order; one name shared by two falls back to the id, so
    /// groups keep one order from launch to launch.
    private static func byName(_ left: (id: String, name: String), _ right: (id: String, name: String)) -> Bool {
        switch left.name.localizedStandardCompare(right.name) {
        case .orderedAscending: true
        case .orderedDescending: false
        case .orderedSame: left.id < right.id
        }
    }

    /// My work: open tasks one way, by what presses first, under the ones
    /// today's plan picked, in the order they were picked. Waits whose day
    /// to ask has come lead the rest, the longest waiting first.
    static func myWork(_ rows: [WorkItemRow], direction: WorkDirection, now: Int64, focus: [String] = []) -> [TaskGroup] {
        let shown = Filter(WorkQuery(), now: now)
        let open = rows.filter { $0.direction == direction && $0.status.isOpen && shown.matches($0) }
        let byId = Dictionary(open.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        let focused = focus.compactMap { byId[$0] }
        let picked = Set(focused.map(\.id))
        let rest = open.filter { !picked.contains($0.id) }
        let asks = { (row: WorkItemRow) in row.followUpAtMs.map { $0 <= now } == true }
        let asking = rest.filter(asks)
        let keyed = Dictionary(grouping: ordered(rest.filter { !asks($0) }, by: .due)) {
            TaskBucket.of($0, now: now, blocking: true)
        }
        let follow = asking.isEmpty ? [] : [TaskGroup(
            id: "bucket:\(TaskBucket.followUp.rawValue)", title: TaskBucket.followUp.title, mark: .bucket(.followUp),
            rows: asking.sorted { ($0.followUpAtMs ?? 0, $0.number) < ($1.followUpAtMs ?? 0, $1.number) })]
        let buckets = follow + TaskBucket.allCases.compactMap { bucket -> TaskGroup? in
            guard let rows = keyed[bucket] else { return nil }
            return TaskGroup(id: "bucket:\(bucket.rawValue)", title: bucket.title, mark: .bucket(bucket), rows: rows)
        }
        guard !focused.isEmpty else { return buckets }
        return [TaskGroup(id: "focus", title: "Focus", mark: .focus, rows: focused)] + buckets
    }
}

/// Dates a person can pick or type for a due date or a snooze.
enum TaskDates {
    static func now() -> Int64 { milliseconds(.now) }

    static func milliseconds(_ date: Date) -> Int64 { Int64((date.timeIntervalSince1970 * 1000).rounded()) }

    /// The start of the day `offset` days from today, plus `hour`.
    static func day(_ offset: Int, hour: Int = 17, from date: Date = .now) -> Int64 {
        let calendar = Calendar.current
        let start = calendar.startOfDay(for: date)
        let day = calendar.date(byAdding: .day, value: offset, to: start) ?? start
        let moment = calendar.date(bySettingHour: hour, minute: 0, second: 0, of: day) ?? day
        return milliseconds(moment)
    }

    /// The next given weekday (1 Sunday … 7 Saturday) after today.
    static func next(weekday: Int, hour: Int = 17) -> Int64 {
        let calendar = Calendar.current
        let today = calendar.component(.weekday, from: .now)
        let ahead = (weekday - today + 7) % 7
        return day(ahead == 0 ? 7 : ahead, hour: hour)
    }

    /// Three hours from now on the hour, while that is still today.
    static func laterToday() -> Int64? {
        let calendar = Calendar.current
        guard let hour = calendar.dateInterval(of: .hour, for: .now.addingTimeInterval(3 * 3600))?.end,
              calendar.isDateInToday(hour)
        else { return nil }
        return milliseconds(hour)
    }

    /// A date written in words, "fri", "nov 20", "in 3 days", when the system
    /// reads one; 5 pm that day, like the dates the pickers offer.
    static func parse(_ text: String) -> Int64? {
        let trimmed = text.trimmingCharacters(in: .whitespaces)
        guard trimmed.count >= 2,
              let detector = try? NSDataDetector(types: NSTextCheckingResult.CheckingType.date.rawValue),
              let match = detector.firstMatch(in: trimmed, range: NSRange(trimmed.startIndex..., in: trimmed)),
              let date = match.date
        else { return nil }
        return day(0, hour: 17, from: date)
    }

    static func label(_ milliseconds: Int64, now: Int64) -> String {
        let days = PlanningFormat.days(from: now, to: milliseconds)
        return switch days {
        case -1: "Yesterday"
        case 0: "Today"
        case 1: "Tomorrow"
        case 2...6: PlanningFormat.moment(milliseconds).formatted(.dateTime.weekday(.wide))
        default: PlanningFormat.day(milliseconds)
        }
    }

    /// "2h", "3d", "Nov 3": how long ago, as tight as a row's meta column.
    static func ago(_ milliseconds: Int64, now: Int64) -> String {
        let minutes = max(0, (now - milliseconds) / 60_000)
        if minutes < 1 { return "now" }
        if minutes < 60 { return "\(minutes)m" }
        if minutes < 24 * 60 { return "\(minutes / 60)h" }
        if minutes < 7 * 24 * 60 { return "\(minutes / (24 * 60))d" }
        return PlanningFormat.day(milliseconds)
    }
}

import AppKit
import Foundation
import Observation

/// Where the morning plan is: picking today's focus, then asking whether
/// month-old work still stands, one task at a time.
enum MorningStage: Equatable {
    case pick
    /// The tasks to ask about, fixed when the questions began.
    case stale([String])
}

/// The morning plan and the evening shutdown: today's focus as the core
/// holds it, and what each screen's keys point at.
@MainActor
@Observable
final class RitualStore {
    static let focusLimit = 3
    /// The local day the morning plan last opened by itself.
    static let shownKey = "tasks.morningShownDay"

    /// The plan last read or saved, for the day it names.
    private(set) var plan: WorkDay?
    private(set) var error: String?
    var stage: MorningStage = .pick
    /// The plan row the keys point at.
    var cursor: String?
    /// The tasks picked to focus on, in the order they were picked.
    var picked: [String] = []
    /// The slipped task the evening's keys point at.
    var slipped: String?

    @ObservationIgnored private let core: Core
    @ObservationIgnored private var saves = 0
    @ObservationIgnored private var queue: Task<Void, Never>?

    init(core: Core) {
        self.core = core
    }

    /// Today's focus, empty until today has a plan.
    var focus: [String] {
        guard let plan, plan.day == Self.day() else { return [] }
        return plan.focusIds
    }

    /// Today's plan has been read, so whether it was made can be told.
    var readToday: Bool { plan?.day == Self.day() }

    var plannedToday: Bool { readToday && plan?.plannedAtMs != nil }

    /// Reads today's plan. A save made while the read was out wins.
    func start() async {
        let serial = saves
        do {
            let read: WorkDay = try await core.request("work_day", DayParams(day: Self.day()))
            guard serial == saves else { return }
            plan = read
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }

    /// Saves today's plan after every earlier save. It shows at once; a
    /// refusal puts back the plan before it.
    func save(_ focus: [String]) {
        let day = Self.day()
        let before = plan
        plan = WorkDay(day: day, focusIds: focus, plannedAtMs: TaskDates.now())
        saves += 1
        let serial = saves
        let previous = queue
        queue = Task {
            await previous?.value
            do {
                let saved: WorkDay = try await core.request("work_day_plan", DayPlanParams(
                    operationId: UUID().uuidString.lowercased(), day: day, focusIds: focus))
                if serial == saves { plan = saved }
                error = nil
            } catch {
                if serial == saves { plan = before }
                self.error = error.localizedDescription
            }
        }
    }

    /// The local day holding `date`, as the core names days.
    static func day(_ date: Date = .now) -> String {
        let parts = Calendar.current.dateComponents([.year, .month, .day], from: date)
        return String(format: "%04d-%02d-%02d", parts.year ?? 0, parts.month ?? 0, parts.day ?? 0)
    }
}

/// What the morning plan shows, read off the rows the stores already hold.
struct MorningPlan {
    static let staleAfterMs: Int64 = 30 * 24 * 3_600_000
    static let staleAsked = 5

    /// Today's meetings; nil when no calendar can be read.
    let meetings: [OverviewUpcomingRow]?
    /// Today's focus, picked earlier, that no other section shows.
    let focus: [WorkItemRow]
    /// My open work due today or before, oldest date first.
    let due: [WorkItemRow]
    /// What I promised in a meeting since yesterday began, still in triage.
    let promised: [WorkItemRow]
    /// Open work nobody has touched in 30 days, longest untouched first.
    let stale: [WorkItemRow]

    /// The rows a focus is picked from, in the order the keys walk them.
    var choices: [WorkItemRow] { focus + due + promised }

    var isEmpty: Bool { (meetings ?? []).isEmpty && choices.isEmpty && stale.isEmpty }
}

/// The evening's account of my work today.
struct Shutdown {
    /// Finished today.
    let kept: [WorkItemRow]
    /// Due today or before and still open, with any focus left undone.
    let slipped: [WorkItemRow]
    /// What I said I'd do tomorrow.
    let tomorrow: [String]
}

extension AppModel {
    func morningPlan(now: Int64) -> MorningPlan {
        let live = (work.items ?? []).filter { TaskListing.matches($0, WorkQuery(), now: now) }
        let due = live.filter { row in
            row.direction == .mine && row.status.isOpen
                && [.overdue, .today].contains(TaskBucket.of(row, now: now, blocking: false))
        }
        let since = TaskDates.day(-1, hour: 0, from: PlanningFormat.moment(now))
        let promised = live.filter { row in
            row.status == .triage && row.sourceLoopId != nil && row.createdAtMs >= since && owedByMe(row)
        }
        let listed = Set((due + promised).map(\.id))
        let focus = rituals.focus.compactMap { id in
            live.first { $0.id == id && $0.status.isOpen && !listed.contains(id) }
        }
        return MorningPlan(
            meetings: meetingsToday(),
            focus: focus,
            due: TaskListing.ordered(due, by: .due),
            promised: promised.sorted { $0.createdAtMs > $1.createdAtMs },
            stale: live.filter { $0.status.isOpen && now - $0.updatedAtMs > MorningPlan.staleAfterMs }
                .sorted { $0.updatedAtMs < $1.updatedAtMs })
    }

    /// Tonight's account, once the evening digest's time has passed and
    /// there is work to account for.
    func shutdown(at date: Date) -> Shutdown? {
        guard eveningDue(date), let items = work.items else { return nil }
        let now = TaskDates.milliseconds(date)
        let morning = TaskDates.day(0, hour: 0, from: date)
        let focus = Set(rituals.focus)
        let mine = items.filter { $0.direction == .mine && $0.archivedAtMs == nil }
        let kept = mine.filter { $0.status == .done && $0.statusChangedAtMs >= morning }
        let slipped = mine.filter { row in
            guard row.status.isOpen, TaskListing.matches(row, WorkQuery(), now: now) else { return false }
            guard let due = row.dueAtMs else { return focus.contains(row.id) }
            return PlanningFormat.days(from: now, to: due) <= 0
        }
        guard !kept.isEmpty || !slipped.isEmpty else { return nil }
        let tomorrow = items.filter { row in
            guard row.archivedAtMs == nil, row.status.isOpen || row.status == .triage, owedByMe(row),
                  let due = row.dueAtMs ?? suggestedDue(row)
            else { return false }
            return PlanningFormat.days(from: now, to: due) == 1
        }
        return Shutdown(
            kept: kept.sorted { $0.statusChangedAtMs < $1.statusChangedAtMs },
            slipped: TaskListing.ordered(slipped, by: .due),
            tomorrow: TaskListing.ordered(tomorrow, by: .priority).map(\.title))
    }

    /// Past the evening digest's time on `date`'s day.
    func eveningDue(_ date: Date) -> Bool {
        let parts = Calendar.current.dateComponents([.hour, .minute], from: date)
        let minute = (parts.hour ?? 0) * 60 + (parts.minute ?? 0)
        return minute >= meetingSettings.settings.digestMinuteOfDay
    }

    /// Mine, or still in triage with Sona reading me as its owner.
    private func owedByMe(_ row: WorkItemRow) -> Bool {
        row.direction == .mine
            || row.status == .triage && (work.suggestions[row.id] ?? []).contains { $0.value == .owner(.myself) }
    }

    private func suggestedDue(_ row: WorkItemRow) -> Int64? {
        guard row.status == .triage else { return nil }
        for suggestion in work.suggestions[row.id] ?? [] {
            if case let .due(due) = suggestion.value { return due }
        }
        return nil
    }

    private func meetingsToday() -> [OverviewUpcomingRow]? {
        guard let upcoming = overview.upcoming, upcoming.access == "authorized" else { return nil }
        return upcoming.rows.filter { FeedClock.isToday($0.startUtcMs) }.sorted { $0.startUtcMs < $1.startUtcMs }
    }

    // MARK: Morning

    /// Opens the morning plan the first time the app comes forward after
    /// 6 a.m., once a day, when there is something to plan and nothing else
    /// is under way. It never brings the app forward itself.
    func openMorningPlanIfDue() {
        let today = RitualStore.day()
        guard ready, onboarding.step == .done, rituals.readToday, !rituals.plannedToday,
              work.items != nil, place != .today,
              UserDefaults.standard.string(forKey: RitualStore.shownKey) != today,
              Calendar.current.component(.hour, from: .now) >= 6,
              sheet == nil, !paletteShown, !showingSettings, capture == .idle,
              live.live == nil, live.active == nil, workFocus.composing == nil,
              !morningPlan(now: TaskDates.now()).isEmpty
        else { return }
        UserDefaults.standard.set(today, forKey: RitualStore.shownKey)
        workFocus.peeking = false
        go(.today)
    }

    /// The app came forward: a new day's plan is read before it is asked for.
    func morningCheck() async {
        if !rituals.readToday { await rituals.start() }
        openMorningPlanIfDue()
    }

    /// The plan opens on its first stage, with today's focus already picked.
    func beginMorning() {
        let plan = morningPlan(now: TaskDates.now())
        let open = Set(plan.choices.map(\.id))
        rituals.stage = .pick
        rituals.picked = rituals.focus.filter(open.contains)
        rituals.cursor = plan.choices.first?.id
    }

    func movePlanCursor(_ delta: Int) {
        let ids = morningPlan(now: TaskDates.now()).choices.map(\.id)
        guard let first = ids.first, let last = ids.last else { return }
        guard let current = rituals.cursor, let index = ids.firstIndex(of: current) else {
            rituals.cursor = delta > 0 ? first : last
            return
        }
        rituals.cursor = ids[min(max(index + delta, 0), ids.count - 1)]
    }

    /// Picks a task to focus on, or puts it back; three at most.
    func togglePick(_ id: String) {
        rituals.cursor = id
        if let index = rituals.picked.firstIndex(of: id) {
            rituals.picked.remove(at: index)
        } else if rituals.picked.count < RitualStore.focusLimit {
            rituals.picked.append(id)
        }
    }

    /// Saves the focus, takes the picked promises out of triage, then asks
    /// about month-old work, if there is any.
    func startDay() {
        let picked = rituals.picked
        let promises = workRows(picked).filter { $0.status == .triage }.map(\.id)
        if !promises.isEmpty {
            changeWork { work.accept(promises, applySuggestions: true) }
        }
        rituals.save(picked)
        let stale = morningPlan(now: TaskDates.now()).stale.map(\.id).filter { !picked.contains($0) }
        guard !stale.isEmpty else { return finishMorning() }
        rituals.stage = .stale(Array(stale.prefix(MorningPlan.staleAsked)))
    }

    /// The task the plan asks about now, and where it sits among them.
    func staleQuestion() -> (row: WorkItemRow, number: Int, count: Int)? {
        guard case let .stale(asked) = rituals.stage else { return nil }
        let stale = Set(morningPlan(now: TaskDates.now()).stale.map(\.id))
        guard let index = asked.firstIndex(where: stale.contains),
              let row = workRows([asked[index]]).first
        else { return nil }
        return (row, index + 1, asked.count)
    }

    /// K keeps the task as it is; X archives it. The day starts after the last.
    func answerStale(keep: Bool) {
        guard let question = staleQuestion() else { return finishMorning() }
        if keep {
            work.keep([question.row.id])
        } else {
            changeWork { work.archive([question.row.id]) }
        }
        if staleQuestion() == nil { finishMorning() }
    }

    /// My work, with today's focus on top.
    func finishMorning() {
        rituals.stage = .pick
        go(.tasks)
        workFocus.open(nil, in: .tasks)
        workFocus.switchTab(.owe)
        workFocus.highlighted = rituals.focus.first
    }

    // MARK: Evening

    func moveSlippedCursor(_ delta: Int) {
        let ids = shutdown(at: .now)?.slipped.map(\.id) ?? []
        guard let first = ids.first, let last = ids.last else { return }
        guard let current = rituals.slipped, let index = ids.firstIndex(of: current) else {
            rituals.slipped = delta > 0 ? first : last
            return
        }
        rituals.slipped = ids[min(max(index + delta, 0), ids.count - 1)]
    }

    /// Moves a slipped task on: → tomorrow, W this week, X drop. The keys
    /// stay on the row that takes its place.
    func moveSlipped(_ id: String, _ patch: WorkPatch) {
        let ids = shutdown(at: .now)?.slipped.map(\.id) ?? []
        if let index = ids.firstIndex(of: id) {
            let rest = ids.filter { $0 != id }
            rituals.slipped = rest.isEmpty ? nil : rest[min(index, rest.count - 1)]
        }
        changeWork { work.update([id], patch) }
    }

    // MARK: Overdue

    /// The Overdue group of the task list showing.
    var overdueTargets: [String] {
        guard !showingSettings, place == .tasks, workFocus.opened(in: .tasks) == nil else { return [] }
        let groups = taskGroups(workFocus.listing(work.views), now: TaskDates.now())
        return groups.first { $0.mark == .bucket(.overdue) }?.rows.map(\.id) ?? []
    }

    /// One date for every overdue task in the list, as one change to undo.
    @discardableResult
    func rescheduleOverdue() -> Bool {
        let ids = overdueTargets
        guard !ids.isEmpty else { return false }
        perform(.due, on: ids)
        return true
    }
}

/// The dates a slipped task moves to.
enum SlipMove {
    static var tomorrow: WorkPatch { WorkPatch(dueAtMs: .set(TaskDates.day(1))) }
    /// Friday, or the next one once this week's has come.
    static var thisWeek: WorkPatch { WorkPatch(dueAtMs: .set(TaskDates.next(weekday: 6))) }
    static var drop: WorkPatch { WorkPatch(status: .canceled) }
}

/// The morning plan's and the evening shutdown's keys. The task screens'
/// key monitor hands them over, so one monitor decides what a key is for.
@MainActor
enum RitualKeys {
    static func handle(_ event: NSEvent, key: String, flags: NSEvent.ModifierFlags, _ model: AppModel) -> Bool {
        switch model.place {
        case .today: morning(event, key: key, flags: flags, model)
        case .capture: evening(event, key: key, flags: flags, model)
        default: false
        }
    }

    private static func morning(_ event: NSEvent, key: String, flags: NSEvent.ModifierFlags, _ model: AppModel) -> Bool {
        let rituals = model.rituals
        if flags == .command, key == "z" {
            guard model.work.undoTitle != nil else { return false }
            model.undoWork()
            return true
        }
        if flags == .shift, key == "?" || key == "/" {
            model.workFocus.shortcutsShown = true
            return true
        }
        guard flags.isEmpty else { return false }
        if case .stale = rituals.stage {
            switch (event.keyCode, key) {
            case (53, _), (36, _), (76, _):
                model.finishMorning()
            case (_, "k"):
                model.answerStale(keep: true)
            case (_, "x"):
                model.answerStale(keep: false)
            default:
                return false
            }
            return true
        }
        switch event.specialKey {
        case .downArrow?:
            model.movePlanCursor(1)
            return true
        case .upArrow?:
            model.movePlanCursor(-1)
            return true
        case .carriageReturn?, .enter?:
            model.startDay()
            return true
        default:
            break
        }
        switch (event.keyCode, key) {
        case (53, _):
            model.finishMorning()
        case (_, "j"):
            model.movePlanCursor(1)
        case (_, "k"):
            model.movePlanCursor(-1)
        case (_, " "):
            guard let id = rituals.cursor else { return false }
            model.togglePick(id)
        case (_, "?"):
            model.workFocus.shortcutsShown = true
        default:
            return false
        }
        return true
    }

    private static func evening(_ event: NSEvent, key: String, flags: NSEvent.ModifierFlags, _ model: AppModel) -> Bool {
        guard let shutdown = model.shutdown(at: .now), !shutdown.slipped.isEmpty else { return false }
        let rituals = model.rituals
        if flags == .command, key == "z" {
            guard model.workFocus.toast != nil, model.work.undoTitle != nil else { return false }
            model.undoWork()
            return true
        }
        guard flags.isEmpty else { return false }
        let current = rituals.slipped.flatMap { id in shutdown.slipped.first { $0.id == id }?.id }
        if event.specialKey == .rightArrow {
            guard let current else { return false }
            model.moveSlipped(current, SlipMove.tomorrow)
            return true
        }
        switch (event.keyCode, key) {
        case (53, _):
            guard rituals.slipped != nil else { return false }
            rituals.slipped = nil
        case (_, "j"):
            model.moveSlippedCursor(1)
        case (_, "k"):
            model.moveSlippedCursor(-1)
        case (_, "w"):
            guard let current else { return false }
            model.moveSlipped(current, SlipMove.thisWeek)
        case (_, "x"):
            guard let current else { return false }
            model.moveSlipped(current, SlipMove.drop)
        default:
            return false
        }
        return true
    }
}

private struct DayParams: Encodable {
    let day: String
}

private struct DayPlanParams: Encodable {
    let operationId: String
    let day: String
    let focusIds: [String]
}

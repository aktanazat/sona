import AppKit
import SwiftUI

/// What agents and Sona did to the tasks the user follows, reminders and due
/// days, automation runs, and the weekly project digest. Return opens the
/// task here, or the digest beside the list; Esc comes back to the list.
struct InboxScreen: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let focus = model.workFocus
        ZStack(alignment: .bottomLeading) {
            if let id = focus.opened(in: .inbox) {
                TaskDetailView(id: id, place: .inbox)
            } else {
                InboxList()
            }
            UndoToast()
        }
        .modifier(WorkSheets())
        .onDisappear { focus.peeking = false }
    }
}

extension AppModel {
    /// The notices the inbox lists now, awake or snoozed, as its filters
    /// narrow them: the ones that need the user first, then the others.
    var inboxShown: [WorkNotification] {
        let focus = workFocus
        let filter = focus.inboxFilter
        let tasks = filter.readsTasks
            ? Dictionary((work.items ?? []).map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first }) : [:]
        let shown = (focus.inboxSnoozed ? work.snoozedNotices : work.inbox).filter { notice in
            (!focus.inboxPriorityOnly || notice.priority)
                && filter.matches(notice, task: notice.itemId.flatMap { tasks[$0] })
        }
        return shown.filter(\.priority) + shown.filter { !$0.priority }
    }

    func openNotice(_ notice: WorkNotification) {
        if notice.readAtMs == nil { work.mark([notice.id], .read) }
        workFocus.inboxHighlighted = notice.id
        workFocus.peeking = notice.digest != nil
        guard let itemId = notice.itemId else {
            if let subject = notice.subject {
                openSubject(subject)
            } else if let automation = notice.automation {
                go(.loops)
                loops.select(automation.id)
            }
            return
        }
        if work.binned[itemId] != nil {
            go(.archive)
            return
        }
        workFocus.highlighted = itemId
        workFocus.open(itemId, in: .inbox)
    }

    /// The task row behind the highlighted notice, for Space's peek.
    var inboxPeeked: WorkItemRow? {
        guard workFocus.peeking, let id = workFocus.inboxHighlighted,
              let itemId = inboxShown.first(where: { $0.id == id })?.itemId
        else { return nil }
        return workRows([itemId]).first
    }

    /// The digest behind the highlighted notice, shown beside the list.
    var inboxDigest: WorkProjectDigest? {
        guard workFocus.peeking, let id = workFocus.inboxHighlighted else { return nil }
        return inboxShown.first(where: { $0.id == id })?.digest
    }

    /// ⇧⌫: archives every read notice the inbox shows.
    func clearReadNotices() {
        work.mark(inboxShown.filter { $0.readAtMs != nil }.map(\.id), .archive)
    }
}

/// When a snoozed notice comes back.
enum InboxSnooze: CaseIterable {
    case laterToday, tomorrow, nextWeek

    var title: String {
        switch self {
        case .laterToday: "Later today"
        case .tomorrow: "Tomorrow"
        case .nextWeek: "Next week"
        }
    }

    var until: Int64? {
        switch self {
        case .laterToday: TaskDates.laterToday()
        case .tomorrow: TaskDates.day(1, hour: 9)
        case .nextWeek: TaskDates.next(weekday: 2, hour: 9)
        }
    }
}

/// Inbox's faces as the start of its bar, Triage among them: what
/// meetings promised waits there for a yes or no; Updates gathers the
/// projects' updates and digests.
struct InboxTabs: View {
    @Environment(AppModel.self) private var model

    private enum Face: Hashable { case inbox, triage, snoozed, updates }

    var body: some View {
        let focus = model.workFocus
        let place = model.place
        let waiting = place == .triage ? 0 : model.work.counts?.triage ?? 0
        let face: Face = switch place {
        case .triage: .triage
        case .updates: .updates
        default: focus.inboxSnoozed ? .snoozed : .inbox
        }
        SegmentedSwitch(
            items: [SwitchItem(id: Face.inbox, title: "Inbox", icon: "bell"),
                    SwitchItem(id: Face.triage, title: waiting > 0 ? "Triage \(waiting)" : "Triage", icon: "tray")]
                + (place == .inbox ? [SwitchItem(id: Face.snoozed, title: "Snoozed", icon: "moon.zzz")] : [])
                + [SwitchItem(id: Face.updates, title: "Updates", icon: Place.updates.icon)],
            selection: face
        ) { picked in
            switch picked {
            case .triage:
                model.go(.triage)
            case .updates:
                model.go(.updates)
            case .inbox, .snoozed:
                focus.inboxSnoozed = picked == .snoozed
                model.go(.inbox)
            }
        }
    }
}

/// What the inbox narrows its notices to; an empty set keeps every one.
/// A project keeps the notices about its tasks and about the project
/// itself; a task priority keeps only notices about tasks.
struct InboxFilter: Equatable {
    var kinds: Set<WorkNotificationKind> = []
    var projectId: String?
    var taskPriorities: Set<TaskPriority> = []
    var unreadOnly = false

    var isEmpty: Bool { self == InboxFilter() }

    /// Whether matching needs the tasks behind the notices.
    var readsTasks: Bool { projectId != nil || !taskPriorities.isEmpty }

    func matches(_ notice: WorkNotification, task: WorkItemRow?) -> Bool {
        if unreadOnly, notice.readAtMs != nil { return false }
        if !kinds.isEmpty, !kinds.contains(notice.kind) { return false }
        if let projectId {
            let about = notice.subject?.kind == .project ? notice.subject?.id : task?.projectId
            if about != projectId { return false }
        }
        if !taskPriorities.isEmpty {
            guard let task, taskPriorities.contains(TaskPriority(task.priority)) else { return false }
        }
        return true
    }
}

private struct InboxFilterMenu: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let focus = model.workFocus
        let filter = focus.inboxFilter
        Menu {
            Toggle("Unread only", isOn: Binding(get: { filter.unreadOnly }, set: { focus.inboxFilter.unreadOnly = $0 }))
            Menu("Kind") {
                ForEach(WorkNotificationKind.listed, id: \.self) { kind in
                    Toggle(kind.label, isOn: member(kind, of: \.kinds))
                }
            }
            Menu("Project") {
                Toggle("Any project", isOn: Binding(get: { filter.projectId == nil },
                                                    set: { _ in focus.inboxFilter.projectId = nil }))
                ForEach(model.work.projects) { project in
                    Toggle(project.title, isOn: Binding(get: { filter.projectId == project.id },
                                                        set: { on in focus.inboxFilter.projectId = on ? project.id : nil }))
                }
            }
            Menu("Task priority") {
                ForEach(TaskPriority.menuOrder) { priority in
                    Toggle(priority.label, isOn: member(priority, of: \.taskPriorities))
                }
            }
            Divider()
            Button("Clear filters") { focus.inboxFilter = InboxFilter() }
                .disabled(filter.isEmpty)
        } label: {
            Image(systemName: "line.3.horizontal.decrease")
        }
        .menuStyle(.button)
        .buttonStyle(IconButton(on: !filter.isEmpty))
        .menuIndicator(.hidden)
        .fixedSize()
        .help("Filter by kind, project, task priority or unread")
        .accessibilityLabel("Filter the inbox")
    }

    private func member<Value: Hashable>(
        _ value: Value, of set: WritableKeyPath<InboxFilter, Set<Value>>
    ) -> Binding<Bool> {
        let focus = model.workFocus
        return Binding(get: { focus.inboxFilter[keyPath: set].contains(value) }, set: { on in
            if on {
                focus.inboxFilter[keyPath: set].insert(value)
            } else {
                focus.inboxFilter[keyPath: set].remove(value)
            }
        })
    }
}

private struct InboxList: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let store = model.work
        let focus = model.workFocus
        let shown = model.inboxShown
        let urgent = shown.filter(\.priority)
        let other = shown.filter { !$0.priority }
        let narrowed = focus.inboxPriorityOnly || !focus.inboxFilter.isEmpty
        VStack(spacing: 0) {
            WorkBar {
                InboxTabs()
            } trailing: {
                WorkBarToggle(
                    title: "Priority", icon: "exclamationmark.circle",
                    help: "Show only what needs you: mentions, assignments, agents' questions, reminders and work due today",
                    isOn: Binding(get: { focus.inboxPriorityOnly }, set: { focus.inboxPriorityOnly = $0 }))
                InboxFilterMenu()
                Button("Clear read") { model.clearReadNotices() }
                    .buttonStyle(QuietButton(compact: true))
                    .disabled(!shown.contains { $0.readAtMs != nil })
                    .help("Archive every read notice  ⇧⌫")
                Button("Mark all read") { store.markAllRead() }
                    .buttonStyle(QuietButton(compact: true))
                    .disabled(store.unread == 0)
                    .help("Mark all read  ⌥U")
            }
            if shown.isEmpty, narrowed {
                TaskEmpty(line: "No notices match.") {
                    Button("Show all") {
                        focus.inboxPriorityOnly = false
                        focus.inboxFilter = InboxFilter()
                    }
                    .buttonStyle(.secondary)
                }
            } else if shown.isEmpty, !focus.inboxSnoozed, let waiting = store.counts?.triage, waiting > 0 {
                TaskEmpty(line: "Nothing new. \(waiting) waiting in Triage.") {
                    TaskKeyButton(title: "Triage", keys: "g i") { model.go(.triage) }
                }
            } else if shown.isEmpty {
                TaskEmpty(line: focus.inboxSnoozed
                    ? "Nothing snoozed."
                    : "Nothing new. Agents' changes, reminders, automation runs and the weekly project digest land here.")
            } else {
                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 0) {
                            if !urgent.isEmpty {
                                InboxSectionTitle(title: "Priority")
                                ForEach(urgent) { notice in
                                    InboxRow(notice: notice, highlighted: focus.inboxHighlighted == notice.id)
                                        .id(notice.id)
                                }
                            }
                            if !other.isEmpty {
                                InboxSectionTitle(title: "Other")
                                ForEach(other) { notice in
                                    InboxRow(notice: notice, highlighted: focus.inboxHighlighted == notice.id)
                                        .id(notice.id)
                                }
                            }
                        }
                        .padding(.vertical, 6)
                    }
                    .onChange(of: focus.inboxHighlighted) { _, id in
                        if let id { proxy.scrollTo(id) }
                    }
                }
            }
        }
        .overlay(alignment: .trailing) {
            if let row = model.inboxPeeked {
                TaskPeek(row: row, place: .inbox)
                    .transition(reduceMotion ? .opacity : .move(edge: .trailing).combined(with: .opacity))
            } else if let digest = model.inboxDigest {
                ProjectDigestPanel(digest: digest)
                    .transition(reduceMotion ? .opacity : .move(edge: .trailing).combined(with: .opacity))
            }
        }
        .animation(Motion.regular, value: focus.peeking)
        .onChange(of: shown.compactMap(\.itemId), initial: true) { _, ids in
            var seen = Set<String>()
            focus.show(ids.filter { seen.insert($0).inserted })
        }
    }
}

private struct InboxSectionTitle: View {
    let title: String

    var body: some View {
        Text(title)
            .sectionLabel()
            .padding(.horizontal, 24)
            .padding(.top, 10)
            .padding(.bottom, 4)
            .accessibilityAddTraits(.isHeader)
    }
}

private struct InboxRow: View {
    let notice: WorkNotification
    let highlighted: Bool
    @Environment(AppModel.self) private var model
    @State private var hovering = false

    var body: some View {
        let store = model.work
        let row = notice.itemId.flatMap { model.workRows([$0]).first }
        let deleted = row == nil ? notice.itemId.flatMap { store.binned[$0]?.item } : nil
        let unread = notice.readAtMs == nil
        HStack(spacing: 10) {
            Circle()
                .fill(unread ? Theme.accentFill : Color.clear)
                .frame(width: 7, height: 7)
            if let row {
                TaskStatusIcon(status: row.status)
            } else if deleted != nil {
                Image(systemName: "trash").font(.system(size: 12)).foregroundStyle(Theme.inkTertiary)
                    .frame(width: 14)
            } else if notice.itemId == nil, let about = notice.subject {
                Image(systemName: about.kind.icon).font(.system(size: 12))
                    .foregroundStyle(Theme.inkSecondary)
                    .frame(width: 14)
            } else if notice.kind == .automation {
                Image(systemName: "arrow.triangle.2.circlepath").font(.system(size: 12))
                    .foregroundStyle(notice.failed ? Theme.live : Theme.inkTertiary)
                    .frame(width: 14)
            } else if notice.kind == .digest {
                Image(systemName: "chart.bar.doc.horizontal").font(.system(size: 12))
                    .foregroundStyle(Theme.inkSecondary)
                    .frame(width: 14)
            }
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    if let number = row?.number ?? deleted?.number {
                        Text("#\(number)").font(TypeScale.mono(12)).foregroundStyle(Theme.inkTertiary)
                    }
                    Text(row?.title ?? deleted?.title ?? subject)
                        .bodyText(13.5, unread ? Theme.ink : Theme.inkSecondary)
                        .fontWeight(unread ? .semibold : .regular)
                        .lineLimit(1)
                    if deleted != nil {
                        Text("In Recently deleted").metaText()
                    }
                }
                HStack(spacing: 4) {
                    if let icon {
                        Image(systemName: icon).font(.system(size: 10))
                    }
                    Text("\(notice.who) \(notice.what)").lineLimit(1)
                }
                .metaText(notice.failed ? Theme.live : Theme.inkSecondary)
            }
            Spacer(minLength: 12)
            if hovering || highlighted {
                actions
            }
            Text(time).metaText().monospacedDigit()
        }
        .padding(.horizontal, 16)
        .frame(height: 52)
        .background(
            RoundedRectangle(cornerRadius: Theme.radiusRow)
                .fill(highlighted ? Theme.washSelected : hovering ? Theme.wash : Color.clear)
                .padding(.horizontal, 8))
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .onTapGesture { model.openNotice(notice) }
        .allowsWindowActivationEvents()
        .contextMenu { menu(store) }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
        .accessibilityValue(unread ? "Unread" : "")
    }

    /// The row's title when no task stands behind it.
    private var subject: String {
        if notice.itemId == nil, let about = notice.subject {
            return about.name ?? "A \(about.kind.noun) deleted"
        }
        if notice.itemId == nil, let automation = notice.automation { return automation.name }
        if notice.digest != nil { return "Weekly project digest" }
        return "A task deleted for good"
    }

    private var icon: String? {
        switch notice.kind {
        case .reminder: "alarm"
        case .question: "questionmark.bubble"
        case .due: "calendar"
        case .automation: nil
        default: notice.actor == .agent ? "sparkle" : nil
        }
    }

    private var time: String {
        if let until = notice.snoozedUntilMs, until > model.work.now {
            return TaskDates.label(until, now: model.work.now)
        }
        return TaskDates.ago(notice.createdAtMs, now: model.work.now)
    }

    private var actions: some View {
        HStack(spacing: 2) {
            Menu {
                snoozeChoices
            } label: {
                Image(systemName: "moon.zzz")
            }
            .menuStyle(.button)
            .buttonStyle(.icon)
            .menuIndicator(.hidden)
            .fixedSize()
            .help("Snooze  H")
            .accessibilityLabel("Snooze")
            WorkBarButton(icon: "checkmark", help: "Archive  ⌫") { model.work.mark([notice.id], .archive) }
                .accessibilityLabel("Archive")
        }
    }

    @ViewBuilder
    private var snoozeChoices: some View {
        if notice.snoozedUntilMs.map({ $0 > model.work.now }) ?? false {
            Button("Wake now") { model.work.mark([notice.id], .snooze, until: nil) }
            Divider()
        }
        ForEach(InboxSnooze.allCases, id: \.self) { choice in
            if let until = choice.until {
                Button(choice.title) { model.work.mark([notice.id], .snooze, until: until) }
            }
        }
    }

    @ViewBuilder
    private func menu(_ store: WorkStore) -> some View {
        if let itemId = notice.itemId {
            if store.binned[itemId] != nil {
                Button("Show in Recently deleted", systemImage: "trash") { model.openNotice(notice) }
            } else if model.workRows([itemId]).first != nil {
                Button("Open task", systemImage: "arrow.up.right.square") { model.openNotice(notice) }
            }
        } else if let about = notice.subject, about.kind != .unknown {
            Button("Open \(about.kind.noun)", systemImage: "arrow.up.right.square") { model.openNotice(notice) }
        } else if notice.automation != nil {
            Button("Open automation", systemImage: "arrow.up.right.square") { model.openNotice(notice) }
        } else if notice.digest != nil {
            Button("Open digest", systemImage: "arrow.up.right.square") { model.openNotice(notice) }
        }
        if notice.readAtMs == nil {
            Button("Mark read", systemImage: "envelope.open") { store.mark([notice.id], .read) }
        } else {
            Button("Mark unread", systemImage: "envelope.badge") { store.mark([notice.id], .unread) }
        }
        Menu("Snooze") { snoozeChoices }
        Button("Archive", systemImage: "checkmark") { store.mark([notice.id], .archive) }
    }
}

/// J and K move through the inbox, Space peeks at the task, Return opens
/// it, U flips read, ⌥U marks all read, H snoozes until tomorrow, Delete
/// archives and ⇧Delete clears every read notice.
@MainActor
enum InboxKeys {
    static func handle(_ event: NSEvent, key: String, flags: NSEvent.ModifierFlags, _ model: AppModel) -> Bool {
        let focus = model.workFocus
        let notices = model.inboxShown
        let index = focus.inboxHighlighted.flatMap { id in notices.firstIndex { $0.id == id } }
        let notice = index.map { notices[$0] }
        if flags == .option, key == "u" {
            model.work.markAllRead()
            return true
        }
        if flags == .shift, event.keyCode == 51 {
            if let notice, notice.readAtMs != nil {
                focus.inboxHighlighted = notices[(index ?? 0)...].first { $0.readAtMs == nil }?.id
                    ?? notices.last { $0.readAtMs == nil }?.id
            }
            model.clearReadNotices()
            return true
        }
        guard flags.isEmpty else { return false }
        func move(_ delta: Int) -> Bool {
            guard !notices.isEmpty else { return true }
            let next = index.map { min(max($0 + delta, 0), notices.count - 1) } ?? (delta > 0 ? 0 : notices.count - 1)
            focus.inboxHighlighted = notices[next].id
            return true
        }
        func leave(_ mark: WorkInboxMark, until: Int64? = nil) -> Bool {
            guard let notice, let index else { return false }
            let rest = notices.filter { $0.id != notice.id }
            focus.inboxHighlighted = rest.isEmpty ? nil : rest[min(index, rest.count - 1)].id
            model.work.mark([notice.id], mark, until: until)
            return true
        }
        if event.keyCode == 53 {
            guard focus.peeking else { return false }
            focus.peeking = false
            return true
        }
        if event.keyCode == 51 { return leave(.archive) }
        switch event.specialKey {
        case .downArrow?: return move(1)
        case .upArrow?: return move(-1)
        case .carriageReturn?, .enter?:
            guard let notice else { return false }
            model.openNotice(notice)
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
            if notice == nil { _ = move(1) }
            focus.peeking = true
            return true
        case "u":
            guard let notice else { return false }
            model.work.mark([notice.id], notice.readAtMs == nil ? .read : .unread)
            return true
        case "h": return leave(.snooze, until: InboxSnooze.tomorrow.until)
        default: return false
        }
    }
}

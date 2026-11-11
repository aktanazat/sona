import AppKit
import SwiftUI

/// What agents and Sona did to the tasks the user follows. Return opens the
/// task here; Esc comes back to the list.
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
    }
}

extension AppModel {
    /// The notices the inbox lists now, awake or snoozed.
    var inboxShown: [WorkNotification] {
        workFocus.inboxSnoozed ? work.snoozedNotices : work.inbox
    }

    func openNotice(_ notice: WorkNotification) {
        if notice.readAtMs == nil { work.mark([notice.id], .read) }
        workFocus.inboxHighlighted = notice.id
        if work.binned[notice.itemId] != nil {
            go(.archive)
            return
        }
        workFocus.highlighted = notice.itemId
        workFocus.open(notice.itemId, in: .inbox)
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

private struct InboxList: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let store = model.work
        let focus = model.workFocus
        let shown = model.inboxShown
        VStack(spacing: 0) {
            WorkBar {
                SegmentedSwitch(
                    items: [SwitchItem(id: false, title: "Inbox", icon: "bell"),
                            SwitchItem(id: true, title: "Snoozed", icon: "moon.zzz")],
                    selection: focus.inboxSnoozed
                ) { focus.inboxSnoozed = $0 }
            } trailing: {
                Button("Mark all read") { store.markAllRead() }
                    .buttonStyle(QuietButton(compact: true))
                    .disabled(store.unread == 0)
            }
            if shown.isEmpty {
                TaskEmpty(line: focus.inboxSnoozed
                    ? "Nothing snoozed."
                    : "Nothing new. Agents' changes to tasks you follow land here.")
            } else {
                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(spacing: 0) {
                            ForEach(shown) { notice in
                                InboxRow(notice: notice, highlighted: focus.inboxHighlighted == notice.id)
                                    .id(notice.id)
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
        .onChange(of: shown.map(\.itemId), initial: true) { _, ids in
            var seen = Set<String>()
            focus.show(ids.filter { seen.insert($0).inserted })
        }
    }
}

private struct InboxRow: View {
    let notice: WorkNotification
    let highlighted: Bool
    @Environment(AppModel.self) private var model
    @State private var hovering = false

    var body: some View {
        let store = model.work
        let row = model.workRows([notice.itemId]).first
        let deleted = row == nil ? store.binned[notice.itemId]?.item : nil
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
            }
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    if let number = row?.number ?? deleted?.number {
                        Text("#\(number)").font(TypeScale.mono(12)).foregroundStyle(Theme.inkTertiary)
                    }
                    Text(row?.title ?? deleted?.title ?? "A task deleted for good")
                        .bodyText(13.5, unread ? Theme.ink : Theme.inkSecondary)
                        .fontWeight(unread ? .semibold : .regular)
                        .lineLimit(1)
                    if deleted != nil {
                        Text("In Recently deleted").metaText()
                    }
                }
                HStack(spacing: 4) {
                    if notice.actor == .agent {
                        Image(systemName: "sparkle").font(.system(size: 10))
                    }
                    Text("\(notice.who) \(notice.what)").lineLimit(1)
                }
                .metaText(Theme.inkSecondary)
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
        .contextMenu { menu(store) }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
        .accessibilityValue(unread ? "Unread" : "")
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
        if store.binned[notice.itemId] != nil {
            Button("Show in Recently deleted", systemImage: "trash") { model.openNotice(notice) }
        } else if model.workRows([notice.itemId]).first != nil {
            Button("Open task", systemImage: "arrow.up.right.square") { model.openNotice(notice) }
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

/// J and K move through the inbox, Return opens the task, U flips read,
/// H snoozes until tomorrow and Delete archives.
@MainActor
enum InboxKeys {
    static func handle(_ event: NSEvent, key: String, flags: NSEvent.ModifierFlags, _ model: AppModel) -> Bool {
        guard flags.isEmpty else { return false }
        let focus = model.workFocus
        let notices = model.inboxShown
        let index = focus.inboxHighlighted.flatMap { id in notices.firstIndex { $0.id == id } }
        let notice = index.map { notices[$0] }
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
        case "u":
            guard let notice else { return false }
            model.work.mark([notice.id], notice.readAtMs == nil ? .read : .unread)
            return true
        case "h": return leave(.snooze, until: InboxSnooze.tomorrow.until)
        default: return false
        }
    }
}

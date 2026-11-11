import AppKit
import SwiftUI

/// The color each status draws in.
enum WorkTint {
    static func status(_ status: WorkStatus) -> Color {
        switch status {
        case .triage: Theme.statusTriage
        case .backlog: Theme.statusBacklog
        case .notStarted: Theme.statusTodo
        case .inProgress: Theme.statusInProgress
        case .done: Theme.statusDone
        case .canceled: Theme.statusCanceled
        case .duplicate: Theme.statusDuplicate
        }
    }
}

/// How rows arrive and leave.
enum WorkMotion {
    static func insert(reduced: Bool) -> AnyTransition {
        reduced
            ? .opacity
            : .asymmetric(insertion: .opacity.combined(with: .offset(y: -6)), removal: .opacity)
    }
}

extension View {
    /// The shared highlight's fill: on at once, off over 0.15 s. A selected
    /// row keeps the accent's tint under it.
    func taskHighlight(_ highlighted: Bool, selected: Bool, radius: CGFloat = Theme.radiusRow) -> some View {
        background {
            ZStack {
                RoundedRectangle(cornerRadius: radius, style: .continuous)
                    .fill(Theme.selection)
                    .opacity(selected ? 1 : 0)
                RoundedRectangle(cornerRadius: radius, style: .continuous)
                    .fill(Theme.wash)
                    .opacity(highlighted ? 1 : 0)
                    .hoverFade(highlighted)
            }
        }
    }
}

struct TaskStatusIcon: View {
    let status: WorkStatus
    var size: CGFloat = 14

    var body: some View {
        let tint = WorkTint.status(status)
        ZStack {
            switch status {
            case .triage:
                Circle().strokeBorder(tint, lineWidth: 1.5)
                Circle().fill(tint).padding(size * 0.32)
            case .backlog:
                Circle().strokeBorder(tint, style: StrokeStyle(lineWidth: 1.5, dash: [size * 0.13, size * 0.13]))
            case .notStarted:
                Circle().strokeBorder(tint, lineWidth: 1.5)
            case .inProgress:
                Circle().strokeBorder(tint, lineWidth: 1.5)
                PieShape(fraction: 0.5).fill(tint).padding(size * 0.25)
            case .done:
                Circle().fill(tint)
                Image(systemName: "checkmark")
                    .font(.system(size: size * 0.5, weight: .bold))
                    .foregroundStyle(Theme.onAccent)
            case .canceled, .duplicate:
                Circle().fill(tint)
                Image(systemName: "xmark")
                    .font(.system(size: size * 0.42, weight: .bold))
                    .foregroundStyle(Theme.onAccent)
            }
        }
        .frame(width: size, height: size)
        .accessibilityElement()
        .accessibilityLabel(status.label)
    }
}

struct PriorityIcon: View {
    let priority: TaskPriority

    var body: some View {
        Group {
            switch priority {
            case .urgent:
                RoundedRectangle(cornerRadius: Theme.radiusKey, style: .continuous)
                    .fill(Theme.priorityUrgent)
                    .frame(width: 14, height: 14)
                    .overlay {
                        Text(verbatim: "!")
                            .font(.system(size: 10, weight: .heavy))
                            .foregroundStyle(Theme.onAccent)
                    }
            case .none:
                HStack(spacing: 1.5) {
                    ForEach(0..<3, id: \.self) { _ in
                        Capsule().frame(width: 3, height: 1.5)
                    }
                }
                .foregroundStyle(Theme.inkDisabled)
            case .high, .medium, .low:
                HStack(alignment: .bottom, spacing: 1.5) {
                    ForEach(0..<3, id: \.self) { bar in
                        RoundedRectangle(cornerRadius: Theme.radiusMark / 2)
                            .fill(bar < filled ? Theme.inkSecondary : Theme.inkDisabled)
                            .frame(width: 3, height: [6, 9, 12][bar])
                    }
                }
            }
        }
        .frame(width: 16, height: 16)
        .accessibilityElement()
        .accessibilityLabel(priority.label)
    }

    private var filled: Int {
        switch priority {
        case .high: 3
        case .medium: 2
        case .low: 1
        case .urgent, .none: 0
        }
    }
}

struct LabelPill: View {
    let label: WorkLabel

    var body: some View {
        HStack(spacing: 5) {
            Circle().fill(Theme.label(hue: label.hue)).frame(width: 8, height: 8)
            Text(label.name)
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(Theme.inkSecondary)
                .lineLimit(1)
        }
        .padding(.leading, 7)
        .padding(.trailing, 8)
        .frame(height: 22)
        .overlay(Capsule().strokeBorder(Theme.border, lineWidth: 1))
    }
}

extension WorkItemRow {
    var ownerFace: AvatarFace {
        if ownerIsSelf { return AvatarFace(kind: .me, name: "Me") }
        if let ownerPersonId { return AvatarFace(kind: .person(ownerPersonId), name: ownerName ?? "Someone") }
        return AvatarFace(kind: .unassigned, name: "No owner")
    }
}

/// "Tomorrow", in red once it has come or gone and the task is still open.
struct TaskDue: View {
    let due: Int64
    let open: Bool
    let now: Int64

    var body: some View {
        let pressing = open && PlanningFormat.days(from: now, to: due) <= 0
        Text(TaskDates.label(due, now: now))
            .font(TypeScale.label(12))
            .monospacedDigit()
            .foregroundStyle(pressing ? Theme.live : Theme.inkTertiary)
            .lineLimit(1)
    }
}

struct TaskCheckbox: View {
    let on: Bool
    let toggle: () -> Void

    var body: some View {
        Button(action: toggle) {
            RoundedRectangle(cornerRadius: Theme.radiusKey, style: .continuous)
                .fill(on ? Theme.accentFill : Theme.surface)
                .overlay {
                    RoundedRectangle(cornerRadius: Theme.radiusKey, style: .continuous)
                        .strokeBorder(on ? Theme.accentFill : Theme.border, lineWidth: 1)
                }
                .overlay {
                    if on {
                        Image(systemName: "checkmark")
                            .font(.system(size: 8, weight: .bold))
                            .foregroundStyle(Theme.onAccent)
                    }
                }
                .frame(width: 14, height: 14)
                .frame(width: 22, height: Theme.listRow)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(on ? "Deselect" : "Select")
    }
}

/// One task in a list: 36 points, priority, number, status and title, then
/// whichever properties the view shows.
struct ItemRow: View {
    let row: WorkItemRow
    let shows: Set<String>
    let labels: [WorkLabel]
    let highlighted: Bool
    let selected: Bool
    let selecting: Bool
    let renaming: Bool
    let now: Int64
    let revision: UInt64
    var waiting = false
    let toggle: () -> Void
    let rename: (String?) -> Void

    var body: some View {
        HStack(spacing: 10) {
            TaskCheckbox(on: selected, toggle: toggle)
                .opacity(selecting || selected || highlighted ? 1 : 0)
                .padding(.trailing, -4)
            if shows.contains(TaskProperty.priority.rawValue) {
                PriorityIcon(priority: TaskPriority(row.priority))
            }
            if shows.contains(TaskProperty.number.rawValue) {
                Text(verbatim: "#\(row.number)")
                    .font(TypeScale.mono(12))
                    .foregroundStyle(Theme.inkTertiary)
                    .lineLimit(1)
                    .frame(width: 40, alignment: .leading)
            }
            TaskStatusIcon(status: row.status)
            if renaming {
                TaskTitleField(title: row.title, commit: rename)
            } else {
                Text(row.title)
                    .font(TypeScale.listTitle)
                    .foregroundStyle(row.status.isOpen || row.status == .triage ? Theme.ink : Theme.inkSecondary)
                    .lineLimit(1)
                    .layoutPriority(1)
            }
            if row.blockedByOpenCount > 0, row.status.isOpen {
                Image(systemName: "hand.raised.fill")
                    .font(.system(size: 10))
                    .foregroundStyle(Theme.live)
                    .help(row.blockedByOpenCount == 1 ? "Blocked by 1 task" : "Blocked by \(row.blockedByOpenCount) tasks")
            }
            if row.raisedIn > 1 {
                HStack(spacing: 3) {
                    Image(systemName: "waveform").font(.system(size: 9, weight: .medium))
                    Text(verbatim: "\(row.raisedIn)")
                }
                .font(TypeScale.label(11))
                .monospacedDigit()
                .foregroundStyle(Theme.inkTertiary)
                .help("Raised in \(row.raisedIn) meetings")
                .accessibilityElement(children: .ignore)
                .accessibilityLabel("Raised in \(row.raisedIn) meetings")
            }
            if let progress = row.subTasks, progress.total > 0 {
                HStack(spacing: 3) {
                    Image(systemName: "checklist").font(.system(size: 9, weight: .medium))
                    Text(verbatim: "\(progress.done)/\(progress.total)")
                }
                .font(TypeScale.label(11))
                .monospacedDigit()
                .foregroundStyle(progress.done == progress.total ? Theme.accent : Theme.inkTertiary)
                .help("\(progress.done) of \(progress.total) sub-tasks done")
                .accessibilityElement(children: .ignore)
                .accessibilityLabel("\(progress.done) of \(progress.total) sub-tasks done")
            }
            if row.attachmentCount > 0 {
                AttachmentCount(count: row.attachmentCount)
            }
            if let repeating = row.repeat {
                HStack(spacing: 3) {
                    Image(systemName: "repeat").font(.system(size: 9, weight: .medium))
                    if repeating.of > 0 {
                        Text(verbatim: "\(repeating.kept)/\(repeating.of)")
                    }
                }
                .font(TypeScale.label(11))
                .monospacedDigit()
                .foregroundStyle(Theme.inkTertiary)
                .help(TaskRepeatText.line(repeating))
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(TaskRepeatText.line(repeating))
            }
            if row.atRisk, row.status.isOpen {
                HStack(spacing: 3) {
                    Image(systemName: "exclamationmark.circle.fill").font(.system(size: 10))
                    Text("At risk")
                }
                .font(TypeScale.label(11))
                .foregroundStyle(Theme.live)
                .help("No free hour left on the day it is due. Press B to book time before then.")
                .accessibilityElement(children: .combine)
            } else if let block = row.timeBlock, row.status.isOpen, block.endsAtMs > now {
                Image(systemName: "calendar.badge.checkmark")
                    .font(.system(size: 10))
                    .foregroundStyle(Theme.inkTertiary)
                    .help("Time booked \(TaskBlockText.when(block))")
                    .accessibilityLabel("Time booked \(TaskBlockText.when(block))")
            }
            Spacer(minLength: 12)
            if shows.contains(TaskProperty.labels.rawValue), !labels.isEmpty {
                HStack(spacing: 4) {
                    ForEach(labels.prefix(3)) { LabelPill(label: $0) }
                }
                .layoutPriority(-1)
            }
            if shows.contains(TaskProperty.meeting.rawValue), let meeting = row.sourceMeetingTitle {
                meta(meeting, icon: "waveform")
            }
            if shows.contains(TaskProperty.project.rawValue), let project = row.projectTitle {
                meta(project, icon: "folder")
            }
            if shows.contains(TaskProperty.due.rawValue) {
                Group {
                    if let due = row.dueAtMs {
                        TaskDue(due: due, open: row.status.isOpen, now: now)
                    } else {
                        Color.clear
                    }
                }
                .frame(width: 72, alignment: .trailing)
            }
            if shows.contains(TaskProperty.updated.rawValue) {
                Text(TaskDates.ago(row.updatedAtMs, now: now))
                    .font(TypeScale.label(12))
                    .monospacedDigit()
                    .foregroundStyle(Theme.inkTertiary)
                    .frame(width: 40, alignment: .trailing)
            }
            if let doer = row.doer, row.status.isOpen {
                TaskDoer(doer: doer, waiting: waiting)
            }
            if shows.contains(TaskProperty.owner.rawValue) {
                Avatar(face: row.ownerFace, size: 20, revision: revision)
            }
        }
        .padding(.leading, 6)
        .padding(.trailing, 16)
        .frame(height: Theme.listRow)
        .taskHighlight(highlighted, selected: selected)
        .padding(.horizontal, 8)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    private func meta(_ text: String, icon: String) -> some View {
        TaskPlace(title: text, icon: icon)
            .frame(maxWidth: 150, alignment: .trailing)
            .fixedSize(horizontal: true, vertical: false)
            .layoutPriority(-1)
    }
}

/// Where a task belongs: its project, or the meeting it came from.
struct TaskPlace: View {
    let title: String
    let icon: String

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: icon).font(.system(size: 10))
            Text(title).lineLimit(1)
        }
        .font(TypeScale.label(12))
        .foregroundStyle(Theme.inkTertiary)
    }
}

/// An agent's name beside the mark every agent wears, in the quiet type of
/// a row's other details.
struct AgentName: View {
    let name: String
    var tint = Theme.inkTertiary

    var body: some View {
        HStack(spacing: Space.xs) {
            Image(systemName: "sparkle").font(.system(size: 11, weight: .medium))
            Text(name).lineLimit(1)
        }
        .font(TypeScale.label(12))
        .foregroundStyle(tint)
        .fixedSize()
    }
}

/// The agent working a task: its name, or, while its session waits on the
/// user's approval, "Waiting for you" in red.
struct TaskDoer: View {
    let doer: WorkDoer
    var waiting = false

    var body: some View {
        let help = waiting ? "\(doer.agent) is waiting for your approval" : "\(doer.agent) is working on it"
        AgentName(name: waiting ? "Waiting for you" : doer.agent, tint: waiting ? Theme.live : Theme.inkTertiary)
            .help(help)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(help)
    }
}

/// A title being renamed in place: ↩ keeps it, Esc puts it back.
struct TaskTitleField: View {
    let title: String
    let commit: (String?) -> Void
    @State private var text = ""
    @FocusState private var focused: Bool

    var body: some View {
        TextField("Title", text: $text)
            .textFieldStyle(.plain)
            .font(TypeScale.listTitle)
            .focused($focused)
            .onAppear {
                text = title
                focused = true
            }
            .onSubmit { commit(text) }
            .onExitCommand { commit(nil) }
            .onChange(of: focused) { _, now in
                if !now { commit(text) }
            }
    }
}

/// A group's band. Its chevron, mark and title stand in the columns of the
/// rows' checkbox, priority and number, so the list reads as one grid.
struct TaskGroupHeader: View {
    let group: TaskGroup
    let collapsed: Bool
    let revision: UInt64
    let toggle: () -> Void
    var add: (() -> Void)?
    /// Moves every task in the group to one date; the Overdue group has it.
    var reschedule: (() -> Void)?
    @State private var hovering = false

    var body: some View {
        HStack(spacing: 8) {
            Button(action: toggle) {
                HStack(spacing: 10) {
                    Image(systemName: "chevron.right")
                        .font(.system(size: 9, weight: .bold))
                        .foregroundStyle(Theme.inkTertiary)
                        .rotationEffect(.degrees(collapsed ? 0 : 90))
                        .frame(width: 22)
                        .padding(.trailing, -4)
                    TaskGroupMarkView(mark: group.mark, revision: revision)
                        .frame(width: 16)
                    HStack(spacing: 6) {
                        Text(group.title)
                            .font(TypeScale.listTitle)
                            .foregroundStyle(Theme.ink)
                            .lineLimit(1)
                        Text("\(group.rows.count)")
                            .font(TypeScale.label(12))
                            .monospacedDigit()
                            .foregroundStyle(Theme.inkTertiary)
                    }
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("\(group.title), \(group.rows.count)")
            .accessibilityHint(collapsed ? "Expand" : "Collapse")
            Spacer(minLength: 8)
            if let reschedule {
                Button(action: reschedule) {
                    HStack(spacing: 6) {
                        Text("Reschedule all")
                        Shortcut("shift+d")
                    }
                }
                .buttonStyle(QuietButton(compact: true))
                .accessibilityLabel("Reschedule all overdue")
            }
            if let add {
                Button(action: add) {
                    Image(systemName: "plus")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(Theme.inkSecondary)
                        .frame(width: 24, height: 24)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .opacity(hovering ? 1 : 0)
                .hoverFade(hovering)
                .help("New task in \(group.title)")
                .accessibilityLabel("New task in \(group.title)")
            }
        }
        .padding(.leading, 14)
        .padding(.trailing, 24)
        .frame(height: Theme.listRow)
        .background(Theme.inset)
        .onHover { hovering = $0 }
    }
}

struct TaskGroupMarkView: View {
    let mark: TaskGroupMark
    let revision: UInt64

    var body: some View {
        switch mark {
        case let .status(status): TaskStatusIcon(status: status)
        case let .priority(priority): PriorityIcon(priority: priority)
        case let .owner(face): Avatar(face: face, size: 16, revision: revision)
        case .project: symbol("folder")
        case .meeting: symbol("waveform")
        case .focus: symbol("scope")
        case let .bucket(bucket): symbol(bucket.symbol)
        case .agent: symbol("sparkle")
        case .none: EmptyView()
        }
    }

    private func symbol(_ name: String) -> some View {
        Image(systemName: name)
            .font(.system(size: 11, weight: .medium))
            .foregroundStyle(Theme.inkTertiary)
            .frame(width: 16, height: 16)
    }
}

/// Rows the size real ones will be, while the first list loads.
struct TaskSkeleton: View {
    var rows = 9
    /// Triage rows carry the quote under the title.
    var twoLine = false
    private static let widths: [CGFloat] = [260, 180, 320, 220, 150, 280, 200, 240, 170]

    var body: some View {
        VStack(spacing: 0) {
            ForEach(0..<rows, id: \.self) { index in
                let width = Self.widths[index % Self.widths.count]
                HStack(spacing: 10) {
                    Color.clear.frame(width: 18)
                    if !twoLine {
                        bar(width: 16, height: 12)
                        bar(width: 36, height: 10)
                    }
                    Circle().fill(Theme.track).frame(width: 14, height: 14)
                    VStack(alignment: .leading, spacing: 9) {
                        bar(width: width, height: 10)
                        if twoLine { bar(width: width * 1.4, height: 8) }
                    }
                    Spacer(minLength: 12)
                    bar(width: 52, height: 10)
                    if !twoLine { Circle().fill(Theme.track).frame(width: 20, height: 20) }
                }
                .padding(.leading, 6)
                .padding(.trailing, 16)
                .frame(height: twoLine ? 56 : Theme.listRow)
                .padding(.horizontal, 8)
            }
            Spacer(minLength: 0)
        }
        .padding(.top, 4)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .accessibilityElement()
        .accessibilityLabel("Loading tasks")
    }

    private func bar(width: CGFloat, height: CGFloat) -> some View {
        Capsule().fill(Theme.track).frame(width: width, height: height)
    }
}

/// A button that names its key: "New task C". Without one, the same button
/// for a choice no key makes. `prominent` marks the action Return commits,
/// drawn in the accent like any default button.
struct TaskKeyButton: View {
    let title: String
    let keys: String?
    /// False keeps the keys' room without showing them, so a column of
    /// buttons where one answers to the keys stays one width.
    var keysShown = true
    var prominent = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 8) {
                Text(title).font(TypeScale.label(13)).foregroundStyle(prominent ? Theme.onAccent : Theme.ink)
                if let keys {
                    Group {
                        if prominent {
                            Text(Shortcut(keys).keys.joined(separator: " "))
                                .font(.system(size: 12, weight: .semibold))
                                .foregroundStyle(Theme.onAccent.opacity(0.75))
                        } else {
                            Shortcut(keys)
                        }
                    }
                    .opacity(keysShown ? 1 : 0)
                    .accessibilityHidden(!keysShown)
                }
            }
            .padding(.leading, 10)
            .padding(.trailing, keys == nil || prominent ? 10 : 4)
        }
        .buttonStyle(KeyButtonStyle(prominent: prominent))
    }
}

private struct KeyButtonStyle: ButtonStyle {
    let prominent: Bool
    @Environment(\.isEnabled) private var isEnabled
    @State private var hovering = false

    func makeBody(configuration: Configuration) -> some View {
        let shape = RoundedRectangle(cornerRadius: Theme.radiusControl, style: .continuous)
        configuration.label
            .frame(height: Theme.controlHeight)
            .background(fill(pressed: configuration.isPressed), in: shape)
            .overlay(shape.strokeBorder(prominent ? Color.clear : Theme.border, lineWidth: 1))
            .contentShape(shape)
            .onHover { hovering = $0 }
            .opacity(isEnabled ? 1 : 0.45)
            .hoverFade(hovering)
            .animation(Motion.quick, value: configuration.isPressed)
    }

    private func fill(pressed: Bool) -> Color {
        if prominent { return Theme.accentFill.opacity(pressed ? 0.8 : 1) }
        return pressed ? Theme.controlPressed : hovering ? Theme.controlHover : Theme.surface
    }
}

/// An empty list: one line, and the control for what comes next.
struct TaskEmpty<Action: View>: View {
    let line: String
    @ViewBuilder let action: () -> Action

    var body: some View {
        VStack(spacing: 14) {
            Text(line).bodyText(13, Theme.inkSecondary)
            action()
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(.bottom, 60)
    }
}

extension TaskEmpty where Action == EmptyView {
    init(line: String) {
        self.init(line: line) { EmptyView() }
    }
}

/// A list that could not load, in the place the list would be.
struct TaskFailure: View {
    let message: String
    let retry: () -> Void

    var body: some View {
        PlannerFailure(message: message, retry: retry)
            .frame(maxWidth: 420, alignment: .leading)
            .padding(24)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }
}

/// The bar across the top of a task screen: what it shows on the left, its
/// actions on the right, in the window's toolbar across the whole pane. The
/// rule under it stays at the top of the list.
struct WorkBar<Leading: View, Trailing: View>: View {
    @ViewBuilder let leading: () -> Leading
    @ViewBuilder let trailing: () -> Trailing

    var body: some View {
        Hairline().pageBar(.full, leading: leading, trailing: trailing)
    }
}

/// A small icon button for a task screen's bar.
struct WorkBarButton: View {
    let icon: String
    let help: String
    var on = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: icon)
        }
        .buttonStyle(IconButton(on: on))
        .help(help)
        .accessibilityLabel(help)
    }
}

/// A titled on/off switch for a task screen's bar, as tall and as round as
/// the bar's other controls; tinted while on.
struct WorkBarToggle: View {
    let title: String
    let icon: String
    let help: String
    @Binding var isOn: Bool
    @State private var hovering = false

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: Theme.radiusControl)
        Button { isOn.toggle() } label: {
            Label(title, systemImage: icon)
                .labelStyle(.titleAndIcon)
                .font(TypeScale.label(13))
                .foregroundStyle(isOn ? Theme.accent : hovering ? Theme.ink : Theme.inkSecondary)
                .padding(.horizontal, 10)
                .frame(height: Theme.controlHeight)
                .background(isOn ? Theme.accentSoft : hovering ? Theme.controlHover : Theme.control, in: shape)
                .contentShape(shape)
                .fixedSize()
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .help(help)
        .accessibilityLabel(title)
        .accessibilityValue(isOn ? "On" : "Off")
        .accessibilityAddTraits(.isToggle)
    }
}

/// "Moved 1 task to Done  Undo ⌘Z": after each change, for five seconds.
struct UndoToast: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let focus = model.workFocus
        ZStack(alignment: .bottomLeading) {
            if let error = model.work.mutationError {
                toast {
                    Text(error).bodyText(13, Theme.live).lineLimit(2)
                }
            } else if let title = focus.toast {
                toast {
                    Text(title).bodyText(13).lineLimit(1)
                    if focus.toastUndoes {
                        Button {
                            model.undoWork()
                        } label: {
                            HStack(spacing: 6) {
                                Text("Undo").font(TypeScale.label(13)).foregroundStyle(Theme.accent)
                                Shortcut("cmd+z")
                            }
                        }
                        .buttonStyle(.plain)
                    }
                }
            }
        }
        .padding(16)
        .animation(Motion.regular, value: focus.toast)
        .animation(Motion.regular, value: model.work.mutationError)
        .task(id: focus.toastStamp) {
            guard focus.toast != nil else { return }
            try? await Task.sleep(for: .seconds(5))
            if !Task.isCancelled { focus.toast = nil }
        }
    }

    private func toast<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        HStack(spacing: 12) { content() }
            .padding(.leading, 14)
            .padding(.trailing, 8)
            .frame(height: Theme.listRow)
            .background {
                RoundedRectangle(cornerRadius: Theme.radiusCard, style: .continuous)
                    .fill(Theme.elevated)
                    .overlay(RoundedRectangle(cornerRadius: Theme.radiusCard, style: .continuous).strokeBorder(Theme.border, lineWidth: 1))
                    .elevation(.medium)
            }
            .transition(reduceMotion ? .opacity : .opacity.combined(with: .offset(y: 8)))
    }
}

struct AttachmentCount: View {
    let count: Int

    var body: some View {
        let line = count == 1 ? "1 attachment" : "\(count) attachments"
        HStack(spacing: 3) {
            Image(systemName: "paperclip").font(.system(size: 9, weight: .medium))
            Text(verbatim: "\(count)")
        }
        .font(TypeScale.label(11))
        .monospacedDigit()
        .foregroundStyle(Theme.inkTertiary)
        .help(line)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(line)
    }
}

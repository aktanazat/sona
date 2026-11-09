import AppKit
import SwiftUI

/// A new task typed from any app, in a panel the quick-entry shortcut
/// floats over it, and the card that says what a spoken ramble put in
/// Triage. Neither brings Sona to the front; a click on the card does.
@MainActor @Observable
final class QuickEntry {
    static let openEvent = "work-quick-entry"
    static let filedEvent = "work-ramble-filed"
    static let commandEvent = "work-command-heard"

    var title = ""
    private(set) var reading = TitleReading()
    private(set) var link: WorkLink?
    private(set) var failure: String?
    /// Counts the times the panel was asked for, so the field takes the
    /// keys each time.
    private(set) var summons = 0
    private(set) var filed: WorkRambleFiledEvent?
    /// The words read as a change to tasks; return applies it.
    private(set) var command: WorkCommand?

    private unowned let model: AppModel
    @ObservationIgnored private lazy var entryPanel: FloatingPanel = {
        let panel = FloatingPanel(QuickEntryPanel(entry: self).environment(model), keyed: true)
        panel.onResignKey = { [weak self] in self?.putAway() }
        return panel
    }()
    @ObservationIgnored private lazy var filedPanel = FloatingPanel(RambleCard(entry: self).environment(model))
    @ObservationIgnored private var shown = false
    /// Words put away by a click in another window, back with the shortcut.
    @ObservationIgnored private var kept = false
    @ObservationIgnored private var dismissal: Task<Void, Never>?

    init(model: AppModel) {
        self.model = model
    }

    /// The shortcut: a fresh panel, or the open or put-away one back in
    /// front with the page it was pressed over.
    func open(_ line: Data) {
        guard let event = try? Core.payload(line) as WorkQuickEntryEvent else { return }
        if shown || kept {
            if let page = event.link { link = page }
        } else {
            title = ""
            reading = TitleReading()
            link = event.link
            failure = nil
        }
        kept = false
        show()
    }

    /// Esc: the words go with the panel.
    func close() {
        shown = false
        kept = false
        entryPanel.hide()
    }

    /// A click in another window: the panel goes, and its words wait for
    /// the shortcut.
    private func putAway() {
        guard shown else { return }
        shown = false
        kept = !title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        entryPanel.hide()
    }

    func dropLink() {
        link = nil
    }

    /// The panel goes at once; the task is saved behind it, and the panel
    /// comes back with the words if the save fails. Words that read as a
    /// change to tasks apply it instead, undoable as one change.
    func submit() {
        if let command, command.said == title.trimmingCharacters(in: .whitespacesAndNewlines) {
            model.work.apply(command)
            close()
            return
        }
        submitAsTask()
    }

    /// The words as a new task, even when they read as a change to tasks.
    func submitAsTask() {
        let text = title
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        let reading = self.reading
        let link = self.link
        close()
        Task { await save(text, reading: reading, link: link) }
    }

    /// Reads the words for a change to tasks, a beat after the last key.
    func readCommand(_ text: String) async {
        let said = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard said.split(separator: " ").count >= 2 else {
            command = nil
            return
        }
        try? await Task.sleep(for: .milliseconds(120))
        guard !Task.isCancelled else { return }
        let read = await model.work.command(said)
        guard !Task.isCancelled else { return }
        command = read.flatMap { $0.items.isEmpty ? nil : $0 }
    }

    /// A dictation that was a change to tasks: the panel, with its words and
    /// what they would change, waiting for return.
    func heard(_ line: Data) {
        guard let event = try? Core.payload(line) as WorkCommandHeardEvent else { return }
        title = event.said
        reading = TitleReading()
        link = nil
        failure = nil
        command = nil
        kept = false
        show()
    }

    private func save(_ text: String, reading: TitleReading, link: WorkLink?) async {
        let work = model.work
        let typed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        let words = TitleFields(await reading.settled(text, in: work))
        let item = await work.create(
            title: words.title ?? typed, status: .notStarted, priority: words.priority, projectId: words.project,
            owner: words.owner ?? .myself, dueAtMs: words.due, labelIds: words.labels(making: work),
            recurrence: words.recurrence, link: link)
        guard item == nil else { return }
        let reason = work.mutationError ?? "The task wasn't added."
        if shown {
            failure = "“\(typed)” wasn't added. \(reason)"
            return
        }
        title = text
        self.reading = reading
        self.link = link
        failure = reason
        show()
    }

    private func show() {
        shown = true
        summons += 1
        entryPanel.show(at: .upperCenter)
    }

    func file(_ line: Data) {
        guard let event = try? Core.payload(line) as WorkRambleFiledEvent else { return }
        filed = event
        filedPanel.show(at: .topTrailing)
        dismissCard(after: event.error == nil ? 4 : 6)
    }

    /// The card stays while the pointer rests on it.
    func pointerOnCard(_ inside: Bool) {
        if inside {
            dismissal?.cancel()
        } else {
            dismissCard(after: 1.5)
        }
    }

    /// Triage with the first new task under the keys; with none, the
    /// dictation, to see what was heard.
    func openFiled() {
        guard let filed else { return }
        dismissal?.cancel()
        filedPanel.hide()
        model.reveal()
        if let first = filed.items.first {
            model.go(.triage)
            model.workFocus.highlighted = first.id
        } else if let id = filed.historyId {
            model.openDictation(id)
        } else {
            model.go(.triage)
        }
    }

    private func dismissCard(after seconds: Double) {
        dismissal?.cancel()
        dismissal = Task { [weak self] in
            try? await Task.sleep(for: .seconds(seconds))
            guard !Task.isCancelled else { return }
            self?.filedPanel.hide()
        }
    }
}

private struct QuickEntryPanel: View {
    let entry: QuickEntry
    @Environment(AppModel.self) private var model
    @FocusState private var focused: Bool

    var body: some View {
        @Bindable var entry = entry
        let reading = entry.reading
        let work = model.work
        let order = reading.tokens.inChipOrder
        let shape = RoundedRectangle(cornerRadius: Theme.radiusDialog)
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: Space.m) {
                HStack(spacing: Space.m) {
                    Group {
                        if entry.command == nil {
                            TaskStatusIcon(status: .notStarted, size: 16)
                        } else {
                            Image(systemName: "arrow.right.circle")
                                .font(.system(size: 15))
                                .foregroundStyle(Theme.accent)
                        }
                    }
                    .frame(width: 16, height: 16)
                    .accessibilityHidden(true)
                    TextField("", text: $entry.title, prompt: Text("New task").foregroundStyle(Theme.inkTertiary))
                        .textFieldStyle(.plain)
                        .font(TypeScale.body(17))
                        .foregroundStyle(Theme.ink)
                        .focused($focused)
                        .onSubmit { entry.submit() }
                        .chipKeys(reading, order: order, text: entry.title, work: work)
                        .task(id: entry.title) { await reading.read(entry.title, in: work) }
                        .task(id: entry.title) { await entry.readCommand(entry.title) }
                        .onKeyPress(.return, phases: .down) { press in
                            guard press.modifiers.contains(.option), entry.command != nil else { return .ignored }
                            entry.submitAsTask()
                            return .handled
                        }
                }
                if let command = entry.command {
                    PaletteCommandPreview(command: command, shown: 5)
                        .padding(.leading, 16 + Space.m)
                } else if !order.isEmpty || entry.link != nil || entry.failure != nil {
                    VStack(alignment: .leading, spacing: Space.s) {
                        if !order.isEmpty {
                            ParsedChips(reading: reading, text: entry.title)
                        }
                        if let link = entry.link {
                            LinkChip(link: link) { entry.dropLink() }
                        }
                        if let failure = entry.failure {
                            Text(failure).metaText(Theme.live).lineLimit(2)
                        }
                    }
                    .padding(.leading, 16 + Space.m)
                }
            }
            .padding(Space.l)
            Hairline()
            keys(reading)
        }
        .frame(width: 600)
        .background(Theme.elevated)
        .clipShape(shape)
        .overlay(shape.strokeBorder(Theme.border, lineWidth: 1))
        .elevation(.medium)
        .padding(Space.l)
        .onExitCommand {
            if reading.selected != nil {
                reading.selected = nil
            } else {
                entry.close()
            }
        }
        .onChange(of: entry.summons, initial: true) {
            DispatchQueue.main.async { focused = true }
        }
    }

    private func keys(_ reading: TitleReading) -> some View {
        HStack(spacing: Space.m) {
            Spacer(minLength: 0)
            if reading.selected == nil {
                if entry.command != nil {
                    hint("Apply", keys: "↩")
                    hint("Add as task", keys: "⌥", "↩")
                } else {
                    hint("Add task", keys: "↩")
                }
                if !reading.tokens.isEmpty {
                    hint("Select", keys: "↓")
                }
                if entry.link != nil {
                    hint("Remove link", keys: "⌘", "⌫")
                }
                hint("Close", keys: "esc")
            } else {
                hint("Move", keys: "←", "→")
                hint("Keep as text", keys: "⌫")
                hint("Back", keys: "↑")
            }
        }
        .padding(.horizontal, Space.l)
        .frame(height: 40)
        .background(Theme.inset)
        .accessibilityElement(children: .combine)
    }

    private func hint(_ action: String, keys: String...) -> some View {
        HStack(spacing: Space.xs) {
            ForEach(keys, id: \.self) { KeyCap($0) }
            Text(action).metaText(Theme.inkSecondary)
        }
    }
}

/// The page or file the task will keep, by its title when the app gave one.
private struct LinkChip: View {
    let link: WorkLink
    let remove: () -> Void
    @State private var crossHovering = false

    var body: some View {
        HStack(spacing: Space.xs) {
            Image(systemName: url?.isFileURL == true ? "doc" : "link")
                .font(.system(size: 11))
                .foregroundStyle(Theme.inkSecondary)
                .frame(width: 16, height: 16)
            Text(name).lineLimit(1)
            Button(action: remove) { ChipCross(hot: crossHovering) }
                .buttonStyle(.plain)
                .onHover { crossHovering = $0 }
                .keyboardShortcut(.delete, modifiers: .command)
                .help("Remove link")
                .accessibilityLabel("Remove link")
        }
        .padding(.leading, Space.s)
        .padding(.trailing, Space.xs)
        .chipFace()
        .help(address)
    }

    private var url: URL? { URL(string: link.url) }

    private var name: String {
        if let title = link.title?.trimmingCharacters(in: .whitespacesAndNewlines), !title.isEmpty { return title }
        guard let url else { return link.url }
        return url.isFileURL ? url.lastPathComponent : url.host() ?? link.url
    }

    private var address: String {
        guard let url, url.isFileURL else { return link.url }
        return url.path(percentEncoded: false)
    }
}

private struct RambleCard: View {
    let entry: QuickEntry
    @Environment(AppModel.self) private var model

    var body: some View {
        if let filed = entry.filed {
            let now = TaskDates.now()
            VStack(alignment: .leading, spacing: Space.s) {
                HStack(spacing: Space.s) {
                    Image(systemName: filed.items.isEmpty ? "mic.slash" : "tray.and.arrow.down")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(filed.error == nil ? Theme.accent : Theme.live)
                        .frame(width: 16, height: 16)
                    Text(headline(filed)).bodyText(13).lineLimit(1)
                    Spacer(minLength: Space.s)
                    Image(systemName: "arrow.up.right")
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.inkTertiary)
                        .frame(width: 16, height: 16)
                }
                ForEach(filed.items.prefix(4)) { item in
                    line(item, now: now)
                }
                if filed.items.count > 4 {
                    Text("\(filed.items.count - 4) more").metaText().padding(.leading, (16 + Space.s) * 2)
                }
                if let error = filed.error {
                    Text(error).metaText(Theme.live).lineLimit(3).fixedSize(horizontal: false, vertical: true)
                        .padding(.leading, 16 + Space.s)
                }
            }
            .padding(PanelSurface.inset)
            .frame(width: PanelSurface.width, alignment: .leading)
            .panelSurface(docked: false)
            .onPointer { inside in
                entry.pointerOnCard(inside)
            } press: {
                entry.openFiled()
            }
            .padding(Space.s)
            .accessibilityElement(children: .combine)
            .accessibilityAddTraits(.isButton)
            .accessibilityAction { entry.openFiled() }
        }
    }

    private func headline(_ filed: WorkRambleFiledEvent) -> String {
        switch (filed.items.count, filed.error) {
        case (0, nil): "No tasks heard"
        case (0, _): "Nothing was added"
        case (1, _): "1 task in Triage"
        case let (count, _): "\(count) tasks in Triage"
        }
    }

    /// The Tasks list's row in small: priority and status lead, the due
    /// date and owner close it, each in its own column.
    private func line(_ item: WorkItem, now: Int64) -> some View {
        HStack(spacing: Space.s) {
            PriorityIcon(priority: TaskPriority(item.priority))
            TaskStatusIcon(status: item.status, size: 12)
                .frame(width: 16, height: 16)
            Text(item.title).bodyText(13).lineLimit(1)
            Spacer(minLength: Space.s)
            if let due = item.dueAtMs {
                TaskDue(due: due, open: item.status.isOpen, now: now)
            }
            Avatar(face: face(item), size: 16, revision: model.people.photoRevision)
        }
    }

    private func face(_ item: WorkItem) -> AvatarFace {
        if item.ownerIsSelf { return AvatarFace(kind: .me, name: "Me") }
        guard let person = item.ownerPersonId else { return AvatarFace(kind: .unassigned, name: "No owner") }
        return AvatarFace(kind: .person(person), name: model.work.people.first { $0.id == person }?.name ?? "Someone")
    }
}

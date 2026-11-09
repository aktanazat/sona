import AppKit
import SwiftUI

/// A new task typed from any app, in a panel the quick-entry shortcut
/// floats over it, and the card that says what a spoken ramble put in
/// Triage. Neither brings Sona to the front; a click on the card does.
@MainActor @Observable
final class QuickEntry {
    static let openEvent = "work-quick-entry"
    static let filedEvent = "work-ramble-filed"

    var title = ""
    private(set) var reading = TitleReading()
    private(set) var link: WorkLink?
    private(set) var failure: String?
    /// Counts the times the panel was asked for, so the field takes the
    /// keys each time.
    private(set) var summons = 0
    private(set) var filed: WorkRambleFiledEvent?

    private unowned let model: AppModel
    @ObservationIgnored private lazy var entryPanel = FloatingPanel(
        QuickEntryPanel(entry: self).environment(model), keyed: true)
    @ObservationIgnored private lazy var filedPanel = FloatingPanel(RambleCard(entry: self).environment(model))
    @ObservationIgnored private var shown = false
    @ObservationIgnored private var dismissal: Task<Void, Never>?

    init(model: AppModel) {
        self.model = model
    }

    /// The shortcut: a fresh panel, or the open one back in front with the
    /// page it was pressed over.
    func open(_ line: Data) {
        guard let event = try? Core.payload(line) as WorkQuickEntryEvent else { return }
        if shown {
            if let page = event.link { link = page }
        } else {
            title = ""
            reading = TitleReading()
            link = event.link
            failure = nil
        }
        show()
    }

    func close() {
        shown = false
        entryPanel.hide()
    }

    func dropLink() {
        link = nil
    }

    /// The panel goes at once; the task is saved behind it, and the panel
    /// comes back with the words if the save fails.
    func submit() {
        let text = title
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        let reading = self.reading
        let link = self.link
        close()
        Task { await save(text, reading: reading, link: link) }
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
        entryPanel.show(at: .edge(.top))
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
        let shape = RoundedRectangle(cornerRadius: Theme.radiusDialog)
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: Space.m) {
                TaskStatusIcon(status: .notStarted, size: 16)
                TextField("", text: $entry.title, prompt: Text("New task").foregroundStyle(Theme.inkTertiary))
                    .textFieldStyle(.plain)
                    .font(TypeScale.body(17))
                    .foregroundStyle(Theme.ink)
                    .focused($focused)
                    .onSubmit { entry.submit() }
                    .chipKeys(reading, text: entry.title, work: work)
                    .task(id: entry.title) { await reading.read(entry.title, in: work) }
            }
            .padding(.horizontal, 20)
            .frame(height: 56)
            if !reading.tokens.isEmpty || entry.link != nil || entry.failure != nil {
                VStack(alignment: .leading, spacing: Space.s) {
                    if !reading.tokens.isEmpty {
                        ParsedChips(reading: reading, text: entry.title)
                    }
                    if let link = entry.link {
                        LinkChip(link: link) { entry.dropLink() }
                    }
                    if let failure = entry.failure {
                        Text(failure).metaText(Theme.live).lineLimit(2)
                    }
                }
                .padding(.leading, 48)
                .padding(.trailing, 20)
                .padding(.bottom, Space.m)
            }
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
        HStack(spacing: 6) {
            Spacer(minLength: 0)
            if reading.selected == nil {
                KeyCap("↩")
                Text("Add task").metaText(Theme.inkSecondary).padding(.trailing, 10)
                if !reading.tokens.isEmpty {
                    KeyCap("↓")
                    Text("Select").metaText(Theme.inkSecondary).padding(.trailing, 10)
                }
                if entry.link != nil {
                    KeyCap("⌘")
                    KeyCap("⌫")
                    Text("Remove link").metaText(Theme.inkSecondary).padding(.trailing, 10)
                }
                KeyCap("esc")
                Text("Close").metaText(Theme.inkSecondary)
            } else {
                KeyCap("←")
                KeyCap("→")
                Text("Move").metaText(Theme.inkSecondary).padding(.trailing, 10)
                KeyCap("⌫")
                Text("Keep as text").metaText(Theme.inkSecondary).padding(.trailing, 10)
                KeyCap("↑")
                Text("Back").metaText(Theme.inkSecondary)
            }
        }
        .padding(.horizontal, 16)
        .frame(height: 40)
        .background(Theme.inset)
        .accessibilityElement(children: .combine)
    }
}

/// The page or file the task will keep, by its title when the app gave one.
private struct LinkChip: View {
    let link: WorkLink
    let remove: () -> Void

    var body: some View {
        HStack(spacing: Space.xs) {
            Image(systemName: url?.isFileURL == true ? "doc" : "link")
                .font(.system(size: 11))
                .foregroundStyle(Theme.inkSecondary)
                .frame(width: 16, height: 16)
            Text(name)
                .font(TypeScale.label(12))
                .foregroundStyle(Theme.ink)
                .lineLimit(1)
            Button(action: remove) {
                Image(systemName: "xmark")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(Theme.inkTertiary)
                    .frame(width: 16, height: 16)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .keyboardShortcut(.delete, modifiers: .command)
            .help("Remove link")
            .accessibilityLabel("Remove link")
        }
        .padding(.leading, Space.s)
        .padding(.trailing, Space.xs)
        .frame(height: 22)
        .background(Theme.control, in: RoundedRectangle(cornerRadius: Theme.radiusKey))
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
                }
                ForEach(filed.items.prefix(4)) { item in
                    line(item, now: now)
                }
                if filed.items.count > 4 {
                    Text("\(filed.items.count - 4) more").metaText().padding(.leading, 24)
                }
                if let error = filed.error {
                    Text(error).metaText(Theme.live).lineLimit(3).fixedSize(horizontal: false, vertical: true)
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
            .padding(8)
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

    private func line(_ item: WorkItem, now: Int64) -> some View {
        HStack(spacing: Space.s) {
            TaskStatusIcon(status: item.status, size: 12)
                .frame(width: 16, height: 16)
            Text(item.title).bodyText(13).lineLimit(1)
            Spacer(minLength: Space.s)
            if item.priority > 0 {
                PriorityIcon(priority: TaskPriority(item.priority))
            }
            if let due = item.dueAtMs {
                Text(TaskDates.label(due, now: now)).metaText().lineLimit(1)
            }
            if let person = item.ownerPersonId {
                let name = model.work.people.first { $0.id == person }?.name ?? "Someone"
                Avatar(face: AvatarFace(kind: .person(person), name: name), size: 16, revision: model.people.photoRevision)
            }
        }
    }
}

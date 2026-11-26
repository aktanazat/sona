import SwiftUI

/// A meeting, a note or a task open in a window of its own, beside the main
/// window. The window follows the item and closes once it is gone.
enum ItemWindow: Codable, Hashable {
    case meeting(String)
    case note(String)
    case task(String)
}

struct ItemWindowView: View {
    let item: ItemWindow
    @Environment(AppModel.self) private var model
    @State private var paneWidth: CGFloat = 0

    var body: some View {
        Group {
            switch item {
            case let .meeting(id): MeetingWindow(id: id)
            case let .note(id): NoteWindow(id: id)
            case let .task(id): TaskWindow(id: id)
            }
        }
        .frame(minWidth: 640, minHeight: 480)
        .background(Theme.page.ignoresSafeArea())
        .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { paneWidth = $0 }
        .environment(\.paneWidth, max(0, paneWidth - Theme.windowControlsWidth))
        .onAppear { model.windowOpened() }
        .onDisappear { model.windowClosed() }
    }
}

private struct TaskWindow: View {
    let id: String
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        let row = model.workRows([id]).first
        TaskDetailView(id: id, place: .tasks, windowed: true)
            .navigationTitle(row.map { "#\($0.number) \($0.title)" } ?? "Task")
            .onChange(of: row != nil || model.work.items == nil, initial: true) { _, here in
                if !here { dismiss() }
            }
    }
}

/// The meeting has a store of its own, so the main window can go on to
/// another meeting. The main window lets go of this one when it opens here.
private struct MeetingWindow: View {
    let id: String
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var store: MeetingsStore?

    var body: some View {
        Group {
            if let store {
                MeetingReviewView(
                    store: store, settings: model.meetingSettings, windowed: true,
                    openPerson: { id in
                        model.reveal()
                        model.openPerson(id)
                    },
                    openMeetingSettings: { model.showSettings(.meetings) },
                    askAgent: { question in
                        model.reveal()
                        model.chat.editDraft(question)
                        model.sheet = .chat
                    })
                    .navigationTitle(store.snapshot?.session.title ?? "Meeting")
                    .onChange(of: store.openSessionId) { _, open in
                        if open == nil { dismiss() }
                    }
            }
        }
        .task {
            guard store == nil else { return }
            if model.meetings.openSessionId == id { model.meetings.closeReview() }
            let opened = MeetingsStore(core: model.core)
            opened.open(id)
            store = opened
        }
        .onDisappear { store?.closeReview() }
    }
}

/// The note has an editor of its own. The main window's editor saves and
/// lets go of it first; a note that cannot be saved there stays there. A
/// window whose note cannot be saved comes back with its words, as the main
/// editor keeps a note it could not save.
private struct NoteWindow: View {
    let id: String
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var store: ScratchpadStore?

    var body: some View {
        Group {
            if let store {
                ScratchpadView(store: store, windowed: true, openSource: { link in
                    model.reveal()
                    model.open(link: link)
                })
                .navigationTitle(store.saved?.title ?? "Note")
                .onChange(of: store.editing) { _, editing in
                    if !editing { dismiss() }
                }
            }
        }
        .task {
            guard store == nil else { return }
            if let unsaved = model.windowNotes.first(where: { $0.saved?.id == id }) {
                store = unsaved
                return
            }
            let main = model.scratchpad
            if main.editing, main.saved?.id == id {
                await main.close()
                guard !main.editing else {
                    dismiss()
                    return
                }
            }
            let opened = ScratchpadStore(core: model.core)
            await opened.open(id: id)
            guard opened.editing else {
                dismiss()
                return
            }
            model.windowNotes.append(opened)
            store = opened
        }
        .onDisappear {
            guard let store else { return }
            Task {
                if await store.flush() {
                    model.windowNotes.removeAll { $0 === store }
                } else {
                    model.openWindow(.note(id))
                }
            }
        }
    }
}

extension AppModel {
    func openWindow(_ item: ItemWindow) {
        presentItemWindow(item)
        NSApp.activate(ignoringOtherApps: true)
    }

    /// The meeting, note or task in front in the main window, or the one
    /// task picked in a list: what Open in New Window takes.
    var itemInFront: ItemWindow? {
        guard ready, onboarding.step == .done else { return nil }
        switch place {
        case .meetings:
            guard live.live == nil, live.gate == nil else { return nil }
            return meetings.openSessionId.map(ItemWindow.meeting)
        case .scratchpad:
            guard scratchpad.editing else { return nil }
            return scratchpad.saved.map { .note($0.id) }
        default:
            if let id = workFocus.opened(in: place) { return .task(id) }
            let targets = workFocus.targets(in: place)
            guard showsTasks, targets.count == 1 else { return nil }
            return .task(targets[0])
        }
    }

    /// Sona is in the Dock and the app switcher while any of its windows is open.
    func windowOpened() {
        openWindows += 1
        NSApp.setActivationPolicy(.regular)
        loadWindowStoresIfShown()
    }

    func windowClosed() {
        openWindows -= 1
        if openWindows <= 0 { NSApp.setActivationPolicy(.accessory) }
    }
}

struct OpenInWindowMenuItem: View {
    let model: AppModel

    var body: some View {
        Button("Open in New Window") {
            if let item = model.itemInFront { model.openWindow(item) }
        }
        .keyboardShortcut("o", modifiers: [.command, .option])
        .disabled(model.itemInFront == nil)
    }
}

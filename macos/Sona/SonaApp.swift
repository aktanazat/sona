import AppKit
import SwiftUI

@main
struct SonaApp: App {
    @NSApplicationDelegateAdaptor(SonaAppDelegate.self) private var appDelegate

    private var model: AppModel { appDelegate.model }

    var body: some Scene {
        Window("Sona", id: "main") {
            Shell().environment(model)
                // In the Dock and the app switcher only while this window is
                // open; the rest of the time Sona is its menu bar mark.
                .onAppear { NSApp.setActivationPolicy(.regular) }
                .onDisappear { NSApp.setActivationPolicy(.accessory) }
        }
        .windowStyle(.hiddenTitleBar)
        .windowToolbarStyle(.unifiedCompact(showsTitle: false))
        .defaultSize(width: 1240, height: 820)
        // Sona lives in the menu bar: the window opens when something asks for
        // it, never at launch, and never because it was open at the last quit.
        .defaultLaunchBehavior(.suppressed)
        .restorationBehavior(.disabled)
        .commands {
            CommandGroup(replacing: .newItem) {}
            CommandGroup(replacing: .appSettings) {
                Button("Settings…") { model.openSettings() }
                    .keyboardShortcut(",", modifiers: .command)
            }
            CommandMenu("Capture") {
                CaptureMenuItem(model: model)
                Button("Record a Meeting") { model.recordMeeting() }
                Button("Record the Screen") { model.recordScreen() }
                Divider()
                Button("Search Sona…") { model.toggleSearch() }
                    .keyboardShortcut("k", modifiers: .command)
            }
        }

        MenuBarExtra {
            MenuBarMenu().environment(model)
        } label: {
            MenuBarMark(model: model)
        }
    }
}

/// The app's single lifetime owner. Ordinary quit waits for editable notes;
/// the core keeps running until AppKit actually commits to termination.
@MainActor
final class SonaAppDelegate: NSObject, NSApplicationDelegate {
    let model = AppModel()
    private var terminationPending = false

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard !terminationPending else { return .terminateLater }
        terminationPending = true
        Task {
            repeat {
                guard await model.live.flushLiveNotes() else {
                    cancelTermination(
                        sender, place: .meetings,
                        message: "Your meeting notes could not be saved. Your words are still in the editor.",
                        detail: model.live.notesError)
                    return
                }
                guard await model.scratchpad.flush() else {
                    cancelTermination(
                        sender, place: .scratchpad,
                        message: "Your scratchpad note could not be saved. Your words are still in the editor.",
                        detail: model.scratchpad.error)
                    return
                }
                // A person may type in either editor while the other's
                // request is out. Both must be settled in this same turn.
            } while model.live.hasPendingLiveNotes || model.scratchpad.hasPendingSave
            sender.reply(toApplicationShouldTerminate: true)
        }
        return .terminateLater
    }

    private func cancelTermination(
        _ sender: NSApplication, place: Place, message: String, detail: String?
    ) {
        terminationPending = false
        sender.reply(toApplicationShouldTerminate: false)
        model.sheet = nil
        model.paletteShown = false
        model.go(place)
        if place == .scratchpad { model.scratchpad.focusDraft() }
        model.reveal()

        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "Sona stayed open"
        alert.informativeText = detail.map { "\(message)\n\n\($0)" } ?? message
        alert.addButton(withTitle: "Keep editing")
        alert.runModal()
    }

    func applicationWillTerminate(_ notification: Notification) {
        model.core.shutdown()
    }
}

extension CaptureState {
    /// The menu bar mark: the Sona mark, badged with what the core is doing.
    var mark: String {
        switch self {
        case .idle: "Mark"
        case .recording: "MarkRecording"
        case .working: "MarkWorking"
        }
    }
}

/// The menu bar mark. It is on screen from launch while the window is not,
/// so it is the view that hands the window's opener to the model.
struct MenuBarMark: View {
    let model: AppModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        HStack(spacing: 6) {
            Image(model.capture.mark)
            if model.prep.preferences?.menuBarEnabled == true && model.prep.upcoming?.access == .authorized {
                Text(model.prep.menuSummary.label)
                    .lineLimit(1)
            }
        }
        .onAppear { model.presentMainWindow = { openWindow(id: "main") } }
        .task(id: model.ready) {
            if model.ready { await model.prep.watchMenu() }
        }
    }
}

/// The start/stop item of a menu, with ⌘R. While the words are being worked
/// on it names the stage and is disabled, since there is nothing to stop.
struct CaptureMenuItem: View {
    let model: AppModel

    var body: some View {
        Button(title) { model.toggleCapture() }
            .keyboardShortcut("r", modifiers: .command)
            .disabled(model.captureActionTitle == nil)
    }

    private var title: String {
        switch model.capture {
        case .idle: "Start Recording"
        case .recording: "Stop Recording"
        case let .working(kind): "\(kind.capitalized)…"
        }
    }
}

/// The menu bar item: the state, the actions, the way in.
struct MenuBarMenu: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        if model.coreStopped {
            Text(model.coreRestarting ? "Restarting the engine…" : "Sona's engine stopped")
            Divider()
            Button("Restart the Engine") { model.restartCore() }
                .disabled(model.coreRestarting)
        } else {
            CaptureClock(state: model.capture, idleText: "Ready · \(model.pillMode ?? "Dictate")")
            if model.prep.preferences?.menuBarEnabled == true {
                let summary = model.prep.menuSummary
                Divider()
                if let event = summary.event {
                    Button(summary.label) { model.openBrief(event.eventKey) }
                } else {
                    Text(summary.label)
                    if model.prep.upcoming?.access != .authorized {
                        Button("Meeting settings…") {
                            model.reveal()
                            model.showSettings(.meetings)
                        }
                    }
                }
            }
            Divider()
            CaptureMenuItem(model: model)
            Button("Record a Meeting") { model.recordMeeting() }
            Button("Record the Screen") { model.recordScreen() }
        }
        Divider()
        Button("Open Sona") { model.reveal() }
        Button("Settings…") { model.openSettings() }
            .keyboardShortcut(",", modifiers: .command)
        Divider()
        Button("Quit Sona") { NSApplication.shared.terminate(nil) }
            .keyboardShortcut("q", modifiers: .command)
    }
}

import AppIntents
import AppKit
import SwiftUI

@main
struct SonaApp: App {
    @NSApplicationDelegateAdaptor(SonaAppDelegate.self) private var appDelegate
    @State private var fullScreen = false

    private var model: AppModel { appDelegate.model }

    var body: some Scene {
        Window("Sona", id: "main") {
            Shell().environment(model)
                .environment(\.fullScreen, fullScreen)
                .background(WindowTitleBar(fullScreen: $fullScreen))
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
            CommandGroup(replacing: .appTermination) {
                Button("Quit Sona") { model.quit() }
                    .keyboardShortcut("q", modifiers: .command)
            }
            CommandMenu("Capture") {
                CaptureMenuItem(model: model)
                Button("Record a Meeting") { model.recordMeeting() }
                ScreenRecordingMenuItem(model: model)
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

/// A hidden title bar has no toolbar, so the close, minimize and zoom
/// buttons sat 3 points from the sidebar's edge. An empty unified toolbar
/// gives them the inset of the system's own sidebar windows. The scene's
/// compact style still sets how far the content sits below them: without
/// it the content moved down 30 points instead of 20. Full screen keeps a
/// window's toolbar on screen, so the toolbar is hidden there and the bare
/// title bar slides in on hover as before.
private struct WindowTitleBar: NSViewRepresentable {
    @Binding var fullScreen: Bool

    func makeNSView(context: Context) -> NSView {
        let view = Attach()
        view.fullScreen = $fullScreen
        return view
    }
    func updateNSView(_ view: NSView, context: Context) {}

    private final class Attach: NSView {
        var fullScreen: Binding<Bool>?
        private var fullScreenObservers: [NSObjectProtocol] = []

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            fullScreenObservers.forEach(NotificationCenter.default.removeObserver)
            fullScreenObservers = []
            guard let window else { return }
            if window.toolbar == nil {
                window.toolbar = NSToolbar(identifier: "main")
                window.toolbarStyle = .unified
                window.titleVisibility = .hidden
            }
            fullScreenObservers = [
                (NSWindow.willEnterFullScreenNotification, false),
                (NSWindow.didExitFullScreenNotification, true),
            ].map { name, visible in
                NotificationCenter.default.addObserver(forName: name, object: window, queue: .main) {
                    [weak self, weak window] _ in
                    MainActor.assumeIsolated {
                        window?.toolbar?.isVisible = visible
                        self?.fullScreen?.wrappedValue = !visible
                    }
                }
            }
        }
    }
}

extension EnvironmentValues {
    /// The main window is in full screen. Its toolbar is hidden there, and
    /// the strip the toolbar held lies above the screen's top edge.
    @Entry var fullScreen = false
}

/// The app's single lifetime owner. Ordinary quit waits for a capture that
/// is starting or finishing, asks before it ends a dictation, a meeting or a
/// screen recording, waits for editable notes, and stops what the person
/// agreed to stop; the core keeps running until AppKit actually commits to
/// termination.
@MainActor
final class SonaAppDelegate: NSObject, NSApplicationDelegate {
    let model = AppModel()
    private var terminationPending = false

    /// AppKit answers a quit sent by AppleScript or the Dock by refusing it
    /// while a sheet is up, before Sona is asked. Sona takes that event and
    /// quits the way its own Quit does.
    func applicationWillFinishLaunching(_ notification: Notification) {
        NSAppleEventManager.shared().setEventHandler(
            self, andSelector: #selector(quitEvent(_:reply:)),
            forEventClass: AEEventClass(kCoreEventClass), andEventID: AEEventID(kAEQuitApplication))
        let model = model
        AppDependencyManager.shared.add(dependency: model)
    }

    @objc private func quitEvent(_ event: NSAppleEventDescriptor, reply: NSAppleEventDescriptor) {
        model.quit()
    }

    func application(_ application: NSApplication, open urls: [URL]) {
        for url in urls { model.open(url: url) }
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard !terminationPending else { return .terminateLater }
        terminationPending = true
        model.pauseQuietReview()
        Task {
            await model.waitForCapturesToSettle()
            let meeting = await stoppableMeeting() != nil
            let screen = model.recorder.phase.activity == .running
            let dictation = if case .recording = model.capture { true } else { false }
            if meeting || screen || dictation,
               !confirmStopping(meeting: meeting, screen: screen, dictation: dictation) {
                terminationPending = false
                model.resumeQuietReview()
                sender.reply(toApplicationShouldTerminate: false)
                model.restoreSheetAfterCancelledQuit()
                return
            }
            if dictation {
                model.stopCapture()
                await model.waitForDictationToFinish()
            }
            repeat {
                guard await model.live.flushLiveNotes() else {
                    cancelTermination(
                        sender, place: .meetings,
                        message: "Your meeting notes could not be saved. Your words are still in the editor.",
                        detail: model.live.notesError, button: "Keep editing")
                    return
                }
                guard await model.scratchpad.flush() else {
                    cancelTermination(
                        sender, place: .scratchpad,
                        message: "Your scratchpad note could not be saved. Your words are still in the editor.",
                        detail: model.scratchpad.error, button: "Keep editing")
                    return
                }
                // A person may type in either editor while the other's
                // request is out. Both must be settled in this same turn.
            } while model.live.hasPendingLiveNotes || model.scratchpad.hasPendingSave
            if meeting {
                guard await stopMeeting(sender) else { return }
            }
            if screen {
                await model.recorder.stopAndSave().value
            }
            sender.reply(toApplicationShouldTerminate: true)
        }
        return .terminateLater
    }

    /// True when the person chose to stop the capture and quit.
    private func confirmStopping(meeting: Bool, screen: Bool, dictation: Bool) -> Bool {
        let alert = NSAlert()
        alert.alertStyle = .warning
        let recording = [meeting ? "a meeting" : nil, screen ? "the screen" : nil, dictation ? "a dictation" : nil]
            .compactMap(\.self)
        let subject = ListFormatter.localizedString(byJoining: recording)
        alert.messageText = subject.prefix(1).uppercased() + subject.dropFirst()
            + (recording.count == 1 ? " is recording" : " are recording")
        var lines: [String] = []
        if meeting { lines.append("The meeting stops now and its audio is kept.") }
        if screen { lines.append("The screen recording stops and is saved.") }
        if dictation { lines.append("The dictation stops, and Sona quits once its words are saved.") }
        alert.informativeText = lines.joined(separator: "\n")
        alert.addButton(withTitle: "Stop and Quit")
        alert.addButton(withTitle: "Keep Recording").keyEquivalent = "\u{1b}"
        NSApp.activate()
        return alert.runModal() == .alertFirstButtonReturn
    }

    /// Quit goes on only once the meeting is no longer capturing: a refused
    /// stop keeps Sona open, since quitting would cut the capture off.
    private func stopMeeting(_ sender: NSApplication) async -> Bool {
        guard let session = await stoppableMeeting() else { return true }
        var detail: String?
        do {
            let result: MeetingMutationResult = try await model.core.request(
                "meeting_stop",
                MeetingLiveRequest.stop(session.sessionId, revision: session.revision, surface: .quit))
            if result.receipt.result == .committed { return true }
            detail = result.receipt.refusal
        } catch {
            detail = error.localizedDescription
        }
        guard await stoppableMeeting() != nil else { return true }
        cancelTermination(
            sender, place: .meetings,
            message: "The meeting could not be stopped, so it is still recording.",
            detail: detail, button: "Keep Recording")
        return false
    }

    /// The capture a quit would cut off. A meeting's start or stop holds the
    /// lock this read takes, so a meeting starting is reported once it
    /// records, and one stopping is not reported once it is sealed.
    private func stoppableMeeting() async -> MeetingSessionSnapshot? {
        do {
            let active: MeetingConsentPanelSessionState? =
                try await model.core.request("meeting_consent_panel_active_state")
            return active?.snapshot
        } catch {
            return nil
        }
    }

    private func cancelTermination(
        _ sender: NSApplication, place: Place, message: String, detail: String?, button: String
    ) {
        terminationPending = false
        model.resumeQuietReview()
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
        alert.addButton(withTitle: button)
        alert.runModal()
    }

    func applicationWillTerminate(_ notification: Notification) {
        model.pauseQuietReview()
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
            Image(model.recorder.phase.activity == .running ? "MarkRecording" : model.capture.mark)
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

/// The screen recorder's item of a menu: a way in while the screen is not
/// recording, a stop that saves while it is.
struct ScreenRecordingMenuItem: View {
    let model: AppModel

    var body: some View {
        switch model.recorder.phase.activity {
        case .none: Button("Record the Screen") { model.recordScreen() }
        case .running: Button("Stop Screen Recording") { model.stopScreenRecording() }
        case .settling: Button("Stop Screen Recording") {}.disabled(true)
        }
    }
}

/// The screen recording's line of the menu bar menu, while there is one.
struct ScreenRecordingStatus: View {
    let store: RecorderStore

    var body: some View {
        switch store.phase {
        case .recording: Text("Recording the screen")
        case .paused: Text("Screen recording paused")
        case .starting: Text("Starting the screen recording…")
        case .finalizing: Text("Saving the screen recording…")
        case .checking, .permission, .idle, .selectingSource, .previewing, .saved, .failed: EmptyView()
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
            let screen = model.recorder.phase.activity != .none
            if screen {
                ScreenRecordingStatus(store: model.recorder)
            }
            if !screen || model.capture != .idle {
                CaptureClock(state: model.capture, idleText: "Ready · \(model.pillMode ?? "Dictate")")
            }
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
            ScreenRecordingMenuItem(model: model)
        }
        Divider()
        Button("Open Sona") { model.reveal() }
        Button("Settings…") { model.openSettings() }
            .keyboardShortcut(",", modifiers: .command)
        Divider()
        Button("Quit Sona") { model.quit() }
            .keyboardShortcut("q", modifiers: .command)
    }
}

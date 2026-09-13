import SwiftUI

@main
struct SonaApp: App {
    @State private var model = AppModel()

    var body: some Scene {
        Window("Sona", id: "main") {
            Shell().environment(model)
        }
        .windowStyle(.hiddenTitleBar)
        .windowToolbarStyle(.unifiedCompact(showsTitle: false))
        .defaultSize(width: 1240, height: 820)
        .defaultLaunchBehavior(.presented)
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
            Image(model.capture.mark)
        }
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

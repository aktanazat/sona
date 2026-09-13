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

/// The menu bar item: the state, the actions, the way in.
struct MenuBarMenu: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        CaptureClock(state: model.capture, idleText: "Ready · \(model.pillMode ?? "Dictate")")
        Divider()
        Button(model.capture == .idle ? "Start recording" : "Stop recording") {
            model.toggleCapture()
        }
        Button("Record a meeting") {
            openWindow(id: "main")
            model.go(.meetings)
            model.live.startManual()
        }
        Button("Record the screen") {
            openWindow(id: "main")
            model.sheet = .recorder
        }
        Divider()
        Button("Open Sona") { openWindow(id: "main") }
        Button("Settings…") {
            openWindow(id: "main")
            model.showSettings(model.settingsPlace)
        }
        Divider()
        Button("Quit Sona") { NSApplication.shared.terminate(nil) }
    }
}

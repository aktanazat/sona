import SwiftUI

@main
struct SonaApp: App {
    @StateObject private var model = AppModel()
    @State private var page = Page.capture

    private enum Page { case board, capture, recording, dictation }

    var body: some Scene {
        WindowGroup {
            TabView(selection: $page) {
                BoardScreen(model: model)
                    .tabItem { Label("tab.board", systemImage: "square.grid.2x2") }
                    .tag(Page.board)
                CaptureScreen(model: model, dictation: model.dictation, recorder: model.recorder)
                    .tabItem { Label("tab.capture", systemImage: "plus.bubble") }
                    .tag(Page.capture)
                RecordingScreen(model: model, recorder: model.recorder)
                    .tabItem { Label("tab.recording", systemImage: "waveform") }
                    .tag(Page.recording)
                DictationScreen(model: model, dictation: model.dictation, recorder: model.recorder)
                    .tabItem { Label("tab.dictation", systemImage: "keyboard") }
                    .tag(Page.dictation)
            }
            .tint(Theme.accent)
            /* On the TabView, not on one screen: consent covers the microphone, and
             * dictation records too. A per-tab sheet left `sona://dictate` outside it. */
            .sheet(isPresented: .constant(!model.consentAccepted)) {
                ConsentScreen(model: model)
                    .interactiveDismissDisabled()
            }
            .onOpenURL { url in
                if DictationLink.opens(url) { page = .dictation }
            }
        }
    }
}

import SwiftUI

@main
struct SonaApp: App {
    @StateObject private var model = AppModel()
    @State private var page = Page.recording

    private enum Page { case recording, dictation }

    var body: some Scene {
        WindowGroup {
            TabView(selection: $page) {
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

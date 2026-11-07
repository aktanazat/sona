import AppIntents
import SwiftUI

@main
struct SonaApp: App {
    @StateObject private var model: AppModel

    init() {
        let model = AppModel()
        _model = StateObject(wrappedValue: model)
        AppDependencyManager.shared.add(dependency: model as any PhoneRecordingControls)
    }

    var body: some Scene {
        WindowGroup {
            TabView(selection: $model.page) {
                BoardScreen(model: model)
                    .tabItem { Label("tab.board", systemImage: "square.grid.2x2") }
                    .tag(AppPage.board)
                CaptureScreen(model: model, dictation: model.dictation, recorder: model.recorder)
                    .tabItem { Label("tab.capture", systemImage: "plus.bubble") }
                    .tag(AppPage.capture)
                RecordingScreen(model: model, recorder: model.recorder, calendar: model.calendar)
                    .tabItem { Label("tab.recording", systemImage: "waveform") }
                    .tag(AppPage.recording)
                DictationScreen(model: model, dictation: model.dictation, recorder: model.recorder)
                    .tabItem { Label("tab.dictation", systemImage: "keyboard") }
                    .tag(AppPage.dictation)
                PhoneLibraryScreen(model: model, library: model.library)
                    .tabItem { Label("tab.library", systemImage: "note.text") }
                    .tag(AppPage.library)
            }
            .tint(Theme.accent)
            /* On the TabView, not on one screen: consent covers the microphone, and
             * dictation records too. A per-tab sheet left `sona://dictate` outside it. */
            .sheet(isPresented: .constant(!model.consentAccepted)) {
                ConsentScreen(model: model)
                    .interactiveDismissDisabled()
            }
            .onOpenURL { model.handle($0) }
        }
    }
}

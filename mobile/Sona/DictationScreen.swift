import SwiftUI

struct DictationScreen: View {
    @ObservedObject var model: AppModel
    @ObservedObject var dictation: PhoneDictation
    /* Observed for the recording state that `model.canStartDictation` reads: AppModel
     * does not republish it. */
    @ObservedObject var recorder: PhoneRecorder
    @Environment(\.scenePhase) private var scenePhase
    @State private var saveError: String?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                Text("dictation.title")
                    .font(.title2.weight(.semibold))
                    .foregroundStyle(Theme.textPrimary)
                Text("dictation.privacy")
                    .font(.subheadline)
                    .foregroundStyle(Theme.textSecondary)
                TextEditor(text: $dictation.text)
                    .scrollContentBackground(.hidden)
                    .padding(8)
                    .frame(minHeight: 180)
                    .background(Theme.inset)
                    .clipShape(RoundedRectangle(cornerRadius: Theme.controlRadius))
                    .disabled(dictation.isBusy)
                    .accessibilityLabel(Text("dictation.transcript"))
                    .accessibilityIdentifier("dictation-transcript")
                controls
                if !model.canStartDictation {
                    Text("dictation.stopMeeting")
                        .font(.footnote)
                        .foregroundStyle(Theme.textSecondary)
                }
                if let notice = dictation.notice {
                    Text(notice)
                        .font(.footnote)
                        .foregroundStyle(Theme.recording)
                }
                Button(action: save) {
                    Text("dictation.useKeyboard")
                        .frame(maxWidth: .infinity, minHeight: 44)
                }
                .buttonStyle(.bordered)
                .disabled(dictation.isBusy || dictation.insertableText.isEmpty)
                .accessibilityIdentifier("dictation-use-keyboard")
                if model.keyboardDraft != nil {
                    Text("dictation.saved")
                        .font(.footnote)
                        .foregroundStyle(Theme.success)
                        .accessibilityIdentifier("dictation-saved")
                }
                if let saveError {
                    Text(saveError)
                        .font(.footnote)
                        .foregroundStyle(Theme.recording)
                }
                Divider()
                Text("dictation.setupTitle")
                    .font(.headline)
                Text("dictation.setup")
                    .font(.footnote)
                    .foregroundStyle(Theme.textSecondary)
                Text("dictation.fullAccess")
                    .font(.footnote)
                    .foregroundStyle(Theme.textSecondary)
            }
            .padding(24)
        }
        .background(Theme.background)
        .tint(Theme.accent)
        .onChange(of: dictation.text) { _, _ in
            /* Edited text is not the text the operator approved, so it stops being
             * insertable the moment it changes. */
            model.withdrawKeyboardDraft()
            saveError = nil
        }
        .onChange(of: scenePhase) { _, phase in
            if phase == .background { dictation.cancel() }
        }
        .onDisappear { dictation.cancel() }
    }

    private var controls: some View {
        HStack(spacing: 16) {
            switch dictation.phase {
            case .idle:
                Button(action: model.startDictation) {
                    Label("dictation.start", systemImage: "mic.fill")
                        .frame(minHeight: 44)
                }
                .buttonStyle(.borderedProminent)
                .disabled(!model.canStartDictation)
                .accessibilityIdentifier("dictation-start")
            case .listening:
                Button(action: dictation.finish) {
                    Label("dictation.stop", systemImage: "stop.fill")
                        .frame(minHeight: 44)
                }
                .buttonStyle(.borderedProminent)
                .tint(Theme.recording)
                .accessibilityIdentifier("dictation-stop")
            case .authorizing, .finishing:
                ProgressView()
                Text(dictation.phase == .authorizing ? "dictation.authorizing" : "dictation.finishing")
                    .font(.footnote)
            }
            if dictation.isBusy {
                Button("dictation.cancel", action: dictation.cancel)
                    .frame(minHeight: 44)
                    .accessibilityIdentifier("dictation-cancel")
            }
        }
    }

    private func save() {
        do {
            try model.approveForKeyboard(dictation.insertableText)
            saveError = nil
        } catch {
            saveError = error.localizedDescription
        }
    }
}

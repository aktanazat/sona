import SwiftUI
import Speech

struct DictationScreen: View {
    @ObservedObject var model: AppModel
    @ObservedObject var dictation: PhoneDictation
    /* Observed for the recording state that `model.canStartDictation` reads: AppModel
     * does not republish it. */
    @ObservedObject var recorder: PhoneRecorder
    @AppStorage(DictationPreferences.keyboardKey) private var keyboardEnabled = false
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
                if model.keyboardReturn {
                    Text("keyboard.swipeBack").font(.headline)
                        .accessibilityIdentifier("keyboard-return-instruction")
                }
                if let until = dictation.warmUntil {
                    HStack {
                        Text("keyboard.warmUntil \(until.formatted(date: .omitted, time: .shortened))").font(.footnote)
                        Spacer()
                        Button("keyboard.endSession", action: dictation.endWarmSession)
                    }
                }
                TextEditor(text: Binding(get: { dictation.text }, set: {
                    dictation.text = $0
                    model.withdrawKeyboardDraft()
                    saveError = nil
                }))
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
                Toggle("keyboard.enable", isOn: $keyboardEnabled)
                    .onChange(of: keyboardEnabled) { _, enabled in model.keyboardPermissionChanged(enabled) }
                Text("keyboard.permission").font(.footnote).foregroundStyle(Theme.textSecondary)
                PhoneDictationSettings(dictation: dictation)
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
                .disabled(!model.canStartDictation || (model.keyboardReturn && !keyboardEnabled))
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

private struct PhoneDictationSettings: View {
    @ObservedObject var dictation: PhoneDictation
    @AppStorage(DictationPreferences.minutesKey) private var minutes = 5
    @AppStorage(DictationPreferences.styleKey) private var style = ""
    @State private var languages = DictationPreferences.languages
    @State private var activeLanguage = DictationPreferences.activeLanguage
    @State private var choosesLanguages = false
    private static let supportedLanguages = SFSpeechRecognizer.supportedLocales()
        .map(\.identifier).sorted { languageName($0).localizedStandardCompare(languageName($1)) == .orderedAscending }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Picker("keyboard.sessionLength", selection: $minutes) {
                ForEach([1, 5, 15, 30], id: \.self) { Text("keyboard.minutes \($0)").tag($0) }
            }.disabled(dictation.isBusy)
            Text("keyboard.sessionPrivacy").font(.footnote).foregroundStyle(Theme.textSecondary)
            Picker("dictation.language", selection: $activeLanguage) {
                ForEach(languages, id: \.self) { Text(Self.languageName($0)).tag($0) }
            }
            .disabled(dictation.isBusy)
            .onChange(of: activeLanguage) { _, value in DictationPreferences.activeLanguage = value }
            Button("dictation.chooseLanguages") { choosesLanguages = true }.frame(minHeight: 44)
            Text("dictation.oneLanguage").font(.footnote).foregroundStyle(Theme.textSecondary)
            Picker("dictation.style", selection: $style) {
                Text("dictation.noStyle").tag("")
                ForEach(dictation.profile.styles) { Text($0.name).tag($0.id) }
            }.disabled(dictation.isBusy || !PhoneDictation.stylesAvailable)
            if !PhoneDictation.stylesAvailable {
                Text("dictation.styleUnavailable").font(.footnote).foregroundStyle(Theme.textSecondary)
            }
            if dictation.profile.vocabulary.isEmpty && dictation.profile.replacements.isEmpty && dictation.profile.snippets.isEmpty && dictation.profile.styles.isEmpty {
                Text("dictation.profileEmpty").font(.footnote).foregroundStyle(Theme.textSecondary)
            } else {
                Text("dictation.profileSynced").font(.footnote).foregroundStyle(Theme.textSecondary)
                DisclosureGroup("dictation.vocabulary") {
                    ForEach(dictation.profile.vocabulary, id: \.self) { word in
                        LabeledContent(word.spoken.isEmpty ? word.written : word.spoken, value: word.written)
                    }
                }
                DisclosureGroup("dictation.replacements") {
                    ForEach(dictation.profile.replacements, id: \.self) { word in
                        LabeledContent(word.spoken, value: word.written)
                    }
                }
                DisclosureGroup("dictation.snippets") {
                    ForEach(dictation.profile.snippets, id: \.self) { snippet in
                        LabeledContent(snippet.trigger, value: snippet.expansion)
                    }
                }
            }
        }
        .sheet(isPresented: $choosesLanguages) {
            NavigationStack {
                List(Self.supportedLanguages, id: \.self) { language in
                    Toggle(Self.languageName(language), isOn: Binding(get: { languages.contains(language) }, set: { enabled in
                        if enabled { languages.append(language) }
                        else if languages.count > 1 { languages.removeAll { $0 == language } }
                        DictationPreferences.languages = languages
                        activeLanguage = DictationPreferences.activeLanguage
                    }))
                    .disabled(dictation.isBusy || (languages.count == 1 && languages.contains(language)))
                }
                .navigationTitle("dictation.chooseLanguages")
                .toolbar { Button("library.done") { choosesLanguages = false } }
            }
        }
    }

    private static func languageName(_ id: String) -> String {
        Locale.current.localizedString(forIdentifier: id) ?? id
    }
}

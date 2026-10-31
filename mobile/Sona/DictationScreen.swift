import SwiftUI
import Speech

struct DictationScreen: View {
    @ObservedObject var model: AppModel
    @ObservedObject var dictation: PhoneDictation
    /* Observed for the recording state that `model.canStartDictation` reads: AppModel
     * does not republish it. */
    @ObservedObject var recorder: PhoneRecorder
    @AppStorage(DictationPreferences.keyboardKey) private var keyboardEnabled = false
    @AppStorage(DictationPreferences.styleKey) private var style = ""
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @ScaledMetric(relativeTo: .body) private var controlSize: CGFloat = 60
    @State private var saveError: String?
    @State private var showsSettings = false

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 12) {
                if model.keyboardReturn {
                    Text("keyboard.swipeBack")
                        .font(.subheadline)
                        .foregroundStyle(Theme.textPrimary)
                        .accessibilityIdentifier("keyboard-return-instruction")
                    if !keyboardEnabled {
                        Text("keyboard.permission")
                            .font(.footnote)
                            .foregroundStyle(Theme.textSecondary)
                        Button("dictation.settings") { showsSettings = true }
                            .font(.subheadline)
                    }
                }
                LevelWaveform(meter: dictation.meter, height: 48)
                    .padding(.vertical, 8)
                TextEditor(text: Binding(get: { dictation.text }, set: {
                    dictation.text = $0
                    model.withdrawKeyboardDraft()
                    saveError = nil
                }))
                    .font(.body)
                    .foregroundStyle(Theme.textPrimary)
                    .scrollContentBackground(.hidden)
                    .frame(maxHeight: .infinity)
                    .overlay(alignment: .topLeading) {
                        if dictation.text.isEmpty {
                            Text("dictation.transcript")
                                .foregroundStyle(Theme.textTertiary)
                                .padding(.top, 8)
                                .padding(.leading, 5)
                                .allowsHitTesting(false)
                                .accessibilityHidden(true)
                        }
                    }
                    .disabled(dictation.isBusy)
                    .accessibilityLabel(Text("dictation.transcript"))
                    .accessibilityIdentifier("dictation-transcript")
                messages
            }
            .padding(.horizontal, Theme.pageInset)
            .padding(.bottom, 8)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .background(Theme.background)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                if let until = dictation.warmUntil {
                    ToolbarItem(placement: .topBarLeading) { warmSession(until: until) }
                }
                ToolbarItem(placement: .principal) { title }
                ToolbarItem(placement: .topBarTrailing) {
                    Button { showsSettings = true } label: {
                        Label("dictation.settings", systemImage: "gearshape")
                    }
                    .accessibilityIdentifier("dictation-settings")
                }
            }
            .safeAreaBar(edge: .bottom) { controls }
            .sheet(isPresented: $showsSettings) {
                PhoneDictationSettings(model: model, dictation: dictation)
            }
        }
        .tint(Theme.accent)
    }

    @ViewBuilder
    private var messages: some View {
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
        if model.keyboardDraft != nil {
            Text("dictation.saved")
                .font(.footnote)
                .foregroundStyle(Theme.textSecondary)
                .accessibilityIdentifier("dictation-saved")
        }
        if let saveError {
            Text(saveError)
                .font(.footnote)
                .foregroundStyle(Theme.recording)
        }
    }

    @ViewBuilder
    private var title: some View {
        if PhoneDictation.stylesAvailable {
            Menu {
                Picker("dictation.style", selection: $style) {
                    Text("dictation.noStyle").tag("")
                    ForEach(dictation.profile.styles) { Text($0.name).tag($0.id) }
                }
            } label: {
                HStack(spacing: 4) {
                    styleName
                        .font(.headline)
                        .foregroundStyle(Theme.textPrimary)
                    Image(systemName: "chevron.down")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(Theme.textSecondary)
                }
            }
            .disabled(dictation.isBusy)
            .accessibilityLabel(Text("dictation.style"))
            .accessibilityValue(styleName)
        } else {
            Text("tab.dictation")
                .font(.headline)
                .foregroundStyle(Theme.textPrimary)
        }
    }

    private var styleName: Text {
        if let name = dictation.profile.styles.first(where: { $0.id == style })?.name {
            return Text(name)
        }
        return Text("dictation.noStyle")
    }

    private func warmSession(until: Date) -> some View {
        let label = Text("keyboard.warmUntil \(until.formatted(date: .omitted, time: .shortened))")
        return Menu {
            Section {
                Button("keyboard.endSession", action: dictation.endWarmSession)
            } header: {
                label
            }
        } label: {
            Image(systemName: "mic.fill")
                .foregroundStyle(Theme.recording)
        }
        .accessibilityLabel(label)
    }

    private var controls: some View {
        VStack(spacing: 12) {
            if !dictation.isBusy && !dictation.insertableText.isEmpty {
                Button(action: save) {
                    Text("dictation.useKeyboard")
                        .font(.body.weight(.semibold))
                        .frame(maxWidth: .infinity, minHeight: 36)
                }
                .buttonStyle(.borderedProminent)
                .buttonBorderShape(.capsule)
                .controlSize(.large)
                .accessibilityIdentifier("dictation-use-keyboard")
            }
            if dictation.phase == .authorizing || dictation.phase == .finishing {
                Text(dictation.phase == .authorizing ? "dictation.authorizing" : "dictation.finishing")
                    .font(.footnote)
                    .foregroundStyle(Theme.textSecondary)
            }
            pill
                .frame(maxWidth: .infinity)
                .overlay(alignment: .leading) {
                    if dictation.isBusy {
                        Button("dictation.cancel", action: dictation.cancel)
                            .foregroundStyle(Theme.textSecondary)
                            .frame(minHeight: 44)
                            .accessibilityIdentifier("dictation-cancel")
                    }
                }
        }
        .padding(.horizontal, Theme.pageInset)
        .padding(.vertical, 8)
    }

    @ViewBuilder
    private var pill: some View {
        switch dictation.phase {
        case .idle:
            Button(action: model.startDictation) {
                RecordPill(glyph: .record, height: controlSize)
            }
            .buttonStyle(PressScaleButtonStyle(reduceMotion: reduceMotion))
            .disabled(!model.canStartDictation || (model.keyboardReturn && !keyboardEnabled))
            .accessibilityLabel(Text("dictation.start"))
            .accessibilityIdentifier("dictation-start")
        case .listening:
            Button(action: dictation.finish) {
                RecordPill(glyph: .stop, height: controlSize)
            }
            .buttonStyle(PressScaleButtonStyle(reduceMotion: reduceMotion))
            .accessibilityLabel(Text("dictation.stop"))
            .accessibilityIdentifier("dictation-stop")
        case .authorizing, .finishing:
            RecordPill(glyph: .busy, height: controlSize)
                .disabled(true)
                .accessibilityHidden(true)
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
    @ObservedObject var model: AppModel
    @ObservedObject var dictation: PhoneDictation
    @Environment(\.dismiss) private var dismiss
    @AppStorage(DictationPreferences.keyboardKey) private var keyboardEnabled = false
    @AppStorage(DictationPreferences.minutesKey) private var minutes = 5
    @AppStorage(DictationPreferences.styleKey) private var style = ""
    @State private var languages = DictationPreferences.languages
    @State private var activeLanguage = DictationPreferences.activeLanguage
    @State private var choosesLanguages = false
    private static let supportedLanguages = SFSpeechRecognizer.supportedLocales()
        .map(\.identifier).sorted { languageName($0).localizedStandardCompare(languageName($1)) == .orderedAscending }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Toggle("keyboard.enable", isOn: $keyboardEnabled)
                        .onChange(of: keyboardEnabled) { _, enabled in model.keyboardPermissionChanged(enabled) }
                    Picker("keyboard.sessionLength", selection: $minutes) {
                        ForEach([1, 5, 15, 30], id: \.self) { Text("keyboard.minutes \($0)").tag($0) }
                    }
                    .disabled(dictation.isBusy)
                } footer: {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("keyboard.permission")
                        Text("keyboard.sessionPrivacy")
                    }
                    .foregroundStyle(Theme.textSecondary)
                }
                .listRowBackground(Theme.surface)
                Section {
                    Picker("dictation.language", selection: $activeLanguage) {
                        ForEach(languages, id: \.self) { Text(Self.languageName($0)).tag($0) }
                    }
                    .disabled(dictation.isBusy)
                    .onChange(of: activeLanguage) { _, value in DictationPreferences.activeLanguage = value }
                    Button("dictation.chooseLanguages") { choosesLanguages = true }
                } footer: {
                    Text("dictation.oneLanguage")
                        .foregroundStyle(Theme.textSecondary)
                }
                .listRowBackground(Theme.surface)
                Section {
                    Picker("dictation.style", selection: $style) {
                        Text("dictation.noStyle").tag("")
                        ForEach(dictation.profile.styles) { Text($0.name).tag($0.id) }
                    }
                    .disabled(dictation.isBusy || !PhoneDictation.stylesAvailable)
                } footer: {
                    VStack(alignment: .leading, spacing: 8) {
                        if !PhoneDictation.stylesAvailable {
                            Text("dictation.styleUnavailable")
                        }
                        if profileIsEmpty {
                            Text("dictation.profileEmpty")
                        }
                    }
                    .foregroundStyle(Theme.textSecondary)
                }
                .listRowBackground(Theme.surface)
                profile
                Section {
                    DisclosureGroup("dictation.setupTitle") {
                        Text("dictation.setup")
                        Text("dictation.fullAccess")
                        Text("dictation.privacy")
                    }
                    .font(.subheadline)
                }
                .foregroundStyle(Theme.textPrimary)
                .listRowBackground(Theme.surface)
            }
            .scrollContentBackground(.hidden)
            .background(Theme.background)
            .navigationTitle("dictation.settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("library.done") { dismiss() }
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
                        .listRowBackground(Theme.surface)
                    }
                    .listStyle(.insetGrouped)
                    .scrollContentBackground(.hidden)
                    .background(Theme.background)
                    .navigationTitle("dictation.chooseLanguages")
                    .navigationBarTitleDisplayMode(.inline)
                    .toolbar {
                        ToolbarItem(placement: .confirmationAction) {
                            Button("library.done") { choosesLanguages = false }
                        }
                    }
                }
            }
        }
        .tint(Theme.accent)
    }

    private var profileIsEmpty: Bool {
        dictation.profile.vocabulary.isEmpty && dictation.profile.replacements.isEmpty
            && dictation.profile.snippets.isEmpty && dictation.profile.styles.isEmpty
    }

    @ViewBuilder
    private var profile: some View {
        if !profileIsEmpty {
            Section {
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
            } footer: {
                Text("dictation.profileSynced")
                    .foregroundStyle(Theme.textSecondary)
            }
            .listRowBackground(Theme.surface)
        }
    }

    private static func languageName(_ id: String) -> String {
        Locale.current.localizedString(forIdentifier: id) ?? id
    }
}

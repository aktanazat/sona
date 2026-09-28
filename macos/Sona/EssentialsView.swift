import SwiftUI

/// The decisions a person makes once: the shortcut, the microphone, the
/// language, whether Sona makes a noise, and whether it starts with the Mac.
///
/// One surface, no headings — the tab above already names the page, and a
/// list this short reads at once. The meeting rows belong to another slice
/// and arrive through `meetingRows`, which is the last thing on the page.
struct EssentialsView<MeetingRows: View>: View {
    let store: SettingsStore
    @ViewBuilder var meetingRows: () -> MeetingRows

    var body: some View {
        Page {
            PageTitle("Essentials", subtitle: "The handful of settings worth deciding once.") {
                EmptyView()
            }
            ErrorNote(store.error)
            Card {
                ShortcutCaptureRow(store: store, id: "transcribe")
                ToggleRow(
                    title: "Push to talk",
                    detail: "Hold the shortcut to record, release to stop.",
                    isOn: Binding(
                        get: { store.settings.pushToTalk },
                        set: { value in Task { await store.setPushToTalk(value) } }
                    )
                )
                MicrophoneRow(store: store)
                /// The spoken language, beside the microphone that hears it.
                /// The list narrows to what the loaded model can recognize,
                /// so a one-language model offers one language rather than a
                /// hundred it would ignore.
                LanguageRow(store: store)
                SoundsRow(store: store)
                LoginItemRow(store: store)
                meetingRows()
            }
        }
    }
}

extension EssentialsView where MeetingRows == EmptyView {
    init(store: SettingsStore) {
        self.init(store: store) { EmptyView() }
    }
}

/// Whether Sona starts with the Mac. The switch is the setting; under it,
/// what macOS did with that, when it is not the same thing.
struct LoginItemRow: View {
    let store: SettingsStore

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text("Launch at login").bodyText()
                switch store.loginItem {
                case .matching:
                    EmptyView()
                case .needsApproval:
                    Text("Switched off in System Settings. Allow Sona under Login Items to start it with the Mac.")
                        .metaText()
                case let .failed(reason):
                    Text("macOS refused the change: \(reason)").metaText(Theme.live)
                }
            }
        } trailing: {
            HStack(spacing: 10) {
                if store.loginItem == .needsApproval {
                    Button("Open Login Items") { store.openLoginItems() }
                        .buttonStyle(.quiet)
                }
                Toggle("Launch at login", isOn: Binding(
                    get: { store.settings.autostartEnabled },
                    set: { value in Task { await store.setAutostart(value) } }
                ))
                .labelsHidden()
                .toggleStyle(.switch)
                .tint(Theme.accent)
                .disabled(store.isBusy("autostart_enabled"))
            }
        }
    }
}

/// The microphone, and a way back to the system default.
///
/// The row keeps naming the configured device even when the enumeration does
/// not contain it: a mic gets unplugged, a list has not resolved yet, and in
/// both states the honest answer is still the name that is stored.
struct MicrophoneRow: View {
    let store: SettingsStore

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text("Microphone").bodyText()
                if store.channelCount > 1 {
                    Text("\(store.channelCount) channels").metaText()
                }
            }
        } trailing: {
            HStack(spacing: 10) {
                if selected != "Default" {
                    Button("Reset") { Task { await store.resetMicrophone() } }
                        .buttonStyle(.quiet)
                        .disabled(store.isBusy("selected_microphone"))
                }
                Picker("Microphone", selection: Binding(
                    get: { selected },
                    set: { name in Task { await store.setMicrophone(name) } }
                )) {
                    ForEach(names, id: \.self) { name in
                        Text(name).tag(name)
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .fixedSize()
                .disabled(store.isBusy("selected_microphone"))
            }
        }
    }

    private var selected: String { SettingsStore.deviceLabel(store.settings.selectedMicrophone) }

    /// The enumeration, plus the stored device when this list does not have
    /// it: a picker with no item for its own value renders empty.
    private var names: [String] {
        let listed = store.microphones.map(\.name)
        return listed.contains(selected) ? listed : [selected] + listed
    }
}

/// One fixed language, a primary-first set, or unrestricted auto detection.
struct LanguageRow: View {
    let store: SettingsStore
    @State private var open = false
    @State private var query = ""

    private var selected: [String] { store.dictationLanguages }
    private var busy: Bool { !store.loaded || store.isBusy("dictation_languages") }

    private var detail: String {
        if !store.selectsLanguage, store.supportedLanguages.count > 1 {
            return "This model chooses its own language and may use languages outside your list."
        }
        if selected.count > 1 {
            return "Sona chooses one for each dictation. If unsure, it uses \(LanguageCatalog.name(selected[0]))."
        }
        if let language = selected.first, language != store.effectiveLanguage {
            return "\(LanguageCatalog.name(language)) is not in this model. It uses \(LanguageCatalog.name(store.effectiveLanguage))."
        }
        return selected.isEmpty
            ? "Detect the language for each dictation. A mode can choose its own language."
            : "Pick more languages to switch between them automatically."
    }

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text("Dictation languages").bodyText()
                Text(detail).metaText()
            }
        } trailing: {
            Button {
                query = ""
                open = true
            } label: {
                Text(selected.isEmpty ? "Auto detect" : selected.map(LanguageCatalog.name).joined(separator: ", "))
                    .lineLimit(2)
                    .frame(maxWidth: 240)
            }
            .buttonStyle(.secondary)
            .accessibilityLabel("Choose dictation languages")
            .disabled(busy)
            .popover(isPresented: $open, arrowEdge: .bottom) { picker }
        }
    }

    private var picker: some View {
        VStack(alignment: .leading, spacing: 0) {
            if store.detectsLanguage {
                Toggle("Auto detect any language", isOn: Binding(
                    get: { selected.isEmpty },
                    set: { enabled in
                        if enabled { Task { await store.setDictationLanguages([]) } }
                    }
                ))
                .toggleStyle(.checkbox)
                .padding(12)
                .disabled(busy || selected.isEmpty)
                Hairline()
            }
            SearchField(prompt: "Search languages", text: $query)
                .padding(12)
            Text("Select one or more. The first is your primary language. For cloud dictation, choose one to keep it fixed.")
                .metaText()
                .padding(.horizontal, 12)
                .padding(.bottom, 12)
            Hairline()
            if matches.isEmpty {
                Text("No languages found").metaText().padding(20)
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 0) {
                        ForEach(matches) { option in
                            Toggle(option.name, isOn: Binding(
                                get: { selected.contains(option.code) },
                                set: { _ in
                                    let next = LanguageCatalog.toggling(option.code, in: selected)
                                    Task { await store.setDictationLanguages(next) }
                                }
                            ))
                            .toggleStyle(.checkbox)
                            .bodyText(14)
                            .frame(maxWidth: .infinity, minHeight: 32, alignment: .leading)
                            .padding(.horizontal, 14)
                            .disabled(busy)
                        }
                    }
                    .padding(.vertical, 6)
                }
                .frame(maxHeight: 260)
            }
            Hairline()
            HStack {
                ErrorNote(store.error)
                Spacer()
                Button("Done") { open = false }.buttonStyle(.quiet)
            }
            .padding(12)
        }
        .frame(width: 320)
        .background(Theme.surface)
    }

    private var matches: [LanguageOption] {
        let needle = query.trimmingCharacters(in: .whitespaces).lowercased()
        var all = store.languageChoices.filter { $0.code != "auto" }
        for code in selected where !all.contains(where: { $0.code == code }) {
            all.append(LanguageOption(code: code, name: LanguageCatalog.name(code)))
        }
        guard !needle.isEmpty else { return all }
        return all.filter { $0.name.lowercased().contains(needle) || $0.code.hasPrefix(needle) }
    }
}

/// Feedback sounds as one row: a switch, and the loudness beside it.
///
/// Off, the slider is not there at all. A dimmed slider under a silent app is
/// a control that cannot do anything, and a reader has to work that out from
/// its grey before moving on.
struct SoundsRow: View {
    let store: SettingsStore

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text("Sounds").bodyText()
                if store.settings.audioFeedback {
                    Text("\(Int((store.settings.audioFeedbackVolume * 100).rounded()))%")
                        .font(TypeScale.mono(12))
                        .foregroundStyle(Theme.inkTertiary)
                }
            }
        } trailing: {
            HStack(spacing: 12) {
                if store.settings.audioFeedback {
                    Slider(
                        value: Binding(
                            get: { store.settings.audioFeedbackVolume },
                            set: { value in Task { await store.setAudioFeedbackVolume(value) } }
                        ),
                        in: 0 ... 1,
                        step: 0.01
                    )
                    .frame(width: 128)
                    .tint(Theme.accent)
                    .accessibilityLabel("Sound volume")
                }
                Toggle("Sounds", isOn: Binding(
                    get: { store.settings.audioFeedback },
                    set: { value in Task { await store.setAudioFeedback(value) } }
                ))
                .labelsHidden()
                .toggleStyle(.switch)
                .tint(Theme.accent)
                .disabled(store.isBusy("audio_feedback"))
            }
        }
    }
}

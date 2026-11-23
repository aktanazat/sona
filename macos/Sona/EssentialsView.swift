import SwiftUI

/// The decisions a person makes once: the shortcuts, the microphone, the
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
            PageTitle("Essentials", subtitle: "")
            ErrorNote(store.error)
            Card {
                ShortcutCaptureRow(store: store, id: "transcribe")
                ToggleRow(
                    title: "Push to talk",
                    detail: store.settings.pushToTalk
                        ? "Hold the shortcut to talk and let go to stop. For a long stretch, tap it once and talk hands-free, then tap it again to stop."
                        : "Tap the shortcut to start, and tap it again to stop.",
                    isOn: Binding(
                        get: { store.settings.pushToTalk },
                        set: { value in Task { await store.setPushToTalk(value) } }
                    )
                )
                ShortcutCaptureRow(store: store, id: "work_quick_entry")
                ShortcutCaptureRow(store: store, id: "work_ramble")
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

struct MicrophoneRow: View {
    let store: SettingsStore
    @State private var managing = false

    var body: some View {
        CardRow(action: { managing = true }) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Microphone").bodyText()
                if store.channelCount > 1 {
                    Text("\(store.channelCount) channels").metaText()
                }
            }
        } trailing: {
            HStack(spacing: 10) {
                MicrophoneChoiceLabel(choice: store.microphoneChoice)
                    .bodyText(14, Theme.inkSecondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .multilineTextAlignment(.trailing)
                Image(systemName: "chevron.right")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(Theme.inkTertiary)
                    .accessibilityHidden(true)
            }
            .frame(maxWidth: Theme.contentMax / 2, alignment: .trailing)
        }
        .sheet(isPresented: $managing) {
            MicrophonePreferencesSheet(store: store, open: $managing)
        }
    }
}

private struct MicrophoneChoiceLabel: View {
    let choice: MicrophoneChoice

    var body: some View {
        switch choice {
        case .systemDefault: Text("System default")
        case .preferred: Text("Preferred microphones")
        case let .device(name): Text(name)
        }
    }
}

private struct MicrophonePreferencesSheet: View {
    let store: SettingsStore
    @Binding var open: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Microphones").headlineText()
            ErrorNote(store.error)
            HStack(spacing: 16) {
                Text("Use").bodyText()
                Spacer()
                Picker("Microphone selection", selection: Binding(
                    get: { store.microphoneChoice },
                    set: { choice in Task { await store.selectMicrophone(choice) } }
                )) {
                    ForEach(store.microphoneChoices, id: \.self) { choice in
                        MicrophoneChoiceLabel(choice: choice)
                            .tag(choice)
                            .disabled(!store.microphoneChoiceAvailable(choice))
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
            }
            .disabled(store.isBusy("microphone_configuration"))
            MicrophonePreferencesList(store: store)
            HStack(spacing: 12) {
                if store.isBusy("microphone_configuration") {
                    ProgressView().controlSize(.small)
                        .accessibilityLabel("Saving microphone preferences")
                }
                Spacer()
                Button("Done") { open = false }
                    .buttonStyle(.primary)
                    .keyboardShortcut(.defaultAction)
                    .disabled(store.isBusy("microphone_configuration"))
            }
        }
        .padding(24)
        .frame(width: 600, height: 540)
        .background(Theme.page)
        .interactiveDismissDisabled(store.isBusy("microphone_configuration"))
        .onExitCommand {
            if !store.isBusy("microphone_configuration") { open = false }
        }
    }
}

private struct MicrophonePreferencesList: View {
    let store: SettingsStore

    var body: some View {
        let preferred = store.preferredMicrophones
        switch store.microphoneReadState {
        case .loading:
            ProgressView("Reading microphones…")
                .controlSize(.small)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        case let .failed(message):
            VStack(alignment: .leading, spacing: 12) {
                ErrorNote(message)
                Button("Retry") { Task { await store.refreshMicrophones() } }
                    .buttonStyle(.secondary)
                Spacer()
            }
        case .ready:
            if preferred.isEmpty && store.otherMicrophones.isEmpty && store.excludedMicrophones.isEmpty {
                Text("No microphones connected.").metaText()
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            } else {
                List {
                    if !preferred.isEmpty {
                        Section("Preferred") {
                            ForEach(preferred) { row in
                                MicrophonePreferenceRow(store: store, row: row)
                                    .contextMenu {
                                        Button("Move up") { Task { await store.moveMicrophone(row.name, by: -1) } }
                                            .disabled(store.microphoneChoice != .preferred || row.rank == 1 || store.isBusy("microphone_configuration"))
                                        Button("Move down") { Task { await store.moveMicrophone(row.name, by: 1) } }
                                            .disabled(store.microphoneChoice != .preferred || row.rank == preferred.count || store.isBusy("microphone_configuration"))
                                    }
                            }
                            .onMove { indices, destination in
                                var names = preferred.map(\.name)
                                names.move(fromOffsets: indices, toOffset: destination)
                                Task { await store.reorderMicrophones(names) }
                            }
                            .moveDisabled(store.microphoneChoice != .preferred || store.isBusy("microphone_configuration"))
                        }
                    }
                    if !store.otherMicrophones.isEmpty {
                        Section("Other microphones") {
                            ForEach(store.otherMicrophones) { row in
                                MicrophonePreferenceRow(store: store, row: row)
                            }
                        }
                    }
                    if !store.excludedMicrophones.isEmpty {
                        Section("Excluded") {
                            ForEach(store.excludedMicrophones) { row in
                                MicrophonePreferenceRow(store: store, row: row)
                            }
                        }
                    }
                }
                .listStyle(.plain)
                .scrollContentBackground(.hidden)
                .disabled(store.isBusy("microphone_configuration"))
            }
        }
    }
}

private struct MicrophonePreferenceRow: View {
    let store: SettingsStore
    let row: MicrophoneListItem

    var body: some View {
        CardRow {
            HStack(alignment: .firstTextBaseline, spacing: 12) {
                if let rank = row.rank {
                    Text(rank, format: .number).metaText()
                }
                VStack(alignment: .leading, spacing: 4) {
                    Text(row.name).bodyText(14, row.excluded ? Theme.inkSecondary : Theme.ink)
                        .fixedSize(horizontal: false, vertical: true)
                    if row.connected == false && row.excluded {
                        Text("Disconnected, excluded").metaText()
                    } else if row.excluded {
                        Text("Excluded").metaText()
                    } else if row.connected == false {
                        Text("Disconnected").metaText()
                    }
                }
            }
        } trailing: {
            HStack(spacing: 8) {
                Button {
                    Task { await store.preferMicrophone(row.name, preferred: row.rank == nil) }
                } label: {
                    if row.rank == nil {
                        Label("Prefer microphone", systemImage: "star")
                    } else {
                        Label("Remove from favourites", systemImage: "star.fill")
                    }
                }
                .labelStyle(.iconOnly)
                .buttonStyle(.quiet)
                Button {
                    Task { await store.excludeMicrophone(row.name, excluded: !row.excluded) }
                } label: {
                    if row.excluded {
                        Label("Allow microphone", systemImage: "checkmark.circle")
                    } else {
                        Label("Exclude microphone", systemImage: "minus.circle")
                    }
                }
                .labelStyle(.iconOnly)
                .buttonStyle(.quiet)
            }
        }
        .listRowSeparator(.hidden)
        .listRowInsets(EdgeInsets())
        .listRowBackground(Color.clear)
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
                HStack(spacing: 6) {
                    Text(selected.isEmpty ? "Auto detect" : selected.map(LanguageCatalog.name).joined(separator: ", "))
                        .lineLimit(1)
                    Image(systemName: "chevron.up.chevron.down")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(Theme.inkSecondary)
                }
                .frame(maxWidth: 240)
                .fixedSize(horizontal: true, vertical: false)
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

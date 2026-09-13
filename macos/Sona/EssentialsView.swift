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
                ToggleRow(
                    title: "Launch at login",
                    isOn: Binding(
                        get: { store.settings.autostartEnabled },
                        set: { value in Task { await store.setAutostart(value) } }
                    )
                )
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
                Picker("", selection: Binding(
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

/// The spoken language: a search field over the codes the loaded model can
/// recognize, and a reset back to letting Sona detect it.
struct LanguageRow: View {
    let store: SettingsStore
    @State private var open = false
    @State private var query = ""

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text("Language").bodyText()
                if store.settings.selectedLanguage != store.effectiveLanguage {
                    /// The stored intent is not what will be used, because
                    /// this model cannot recognize it. Say so rather than
                    /// show a language that will not happen.
                    Text("\(LanguageCatalog.name(store.settings.selectedLanguage)) is not in this model, so \(LanguageCatalog.name(store.effectiveLanguage)) is used")
                        .metaText()
                }
            }
        } trailing: {
            HStack(spacing: 10) {
                if store.settings.selectedLanguage != "auto" {
                    Button("Reset") { Task { await store.resetLanguage() } }
                        .buttonStyle(.quiet)
                        .disabled(store.isBusy("selected_language"))
                }
                Button {
                    query = ""
                    open = true
                } label: {
                    HStack(spacing: 6) {
                        Text(LanguageCatalog.name(store.effectiveLanguage))
                            .font(TypeScale.body(14))
                            .foregroundStyle(Theme.ink)
                        Image(systemName: "chevron.up.chevron.down")
                            .font(.system(size: 9, weight: .semibold))
                            .foregroundStyle(Theme.inkTertiary)
                    }
                    .padding(.horizontal, 10)
                    .frame(height: 28)
                    .background(Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
                    .overlay(
                        RoundedRectangle(cornerRadius: Theme.radiusControl)
                            .strokeBorder(Theme.border, lineWidth: 1)
                    )
                }
                .buttonStyle(.plain)
                .disabled(store.isBusy("selected_language"))
                .popover(isPresented: $open, arrowEdge: .bottom) { picker }
            }
        }
    }

    private var picker: some View {
        VStack(spacing: 0) {
            SearchField(prompt: "Search languages", text: $query)
                .padding(12)
            Hairline()
            if matches.isEmpty {
                Text("No languages found")
                    .metaText()
                    .padding(20)
            } else {
                ScrollView {
                    VStack(spacing: 0) {
                        ForEach(matches) { option in
                            Button {
                                open = false
                                Task { await store.setLanguage(option.code) }
                            } label: {
                                HStack {
                                    Text(option.name).bodyText(14)
                                    Spacer(minLength: 12)
                                    if option.code == store.effectiveLanguage {
                                        Image(systemName: "checkmark")
                                            .font(.system(size: 11, weight: .semibold))
                                            .foregroundStyle(Theme.accent)
                                    }
                                }
                                .padding(.horizontal, 14)
                                .frame(height: 30)
                                .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                        }
                    }
                    .padding(.vertical, 6)
                }
                .frame(maxHeight: 260)
            }
        }
        .frame(width: 260)
        .background(Theme.surface)
    }

    private var matches: [LanguageOption] {
        let needle = query.trimmingCharacters(in: .whitespaces).lowercased()
        let all = store.languageChoices
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
                Toggle("", isOn: Binding(
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

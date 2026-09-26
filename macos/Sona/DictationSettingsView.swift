import SwiftUI

/// What happens to a dictation between the microphone and the text.
///
/// The rows run in the order a dictation does: the chords that start and stop
/// one, how the words are written, what is on screen while it runs, and how
/// long the model stays in memory. The history rows belong to another slice
/// and arrive through `dataRows`, in the place they hold on the web page.
struct DictationSettingsView<DataRows: View>: View {
    let store: SettingsStore
    /// The door to the mode editor, which another slice owns.
    var onOpenModes: () -> Void = {}
    @ViewBuilder var dataRows: () -> DataRows

    var body: some View {
        Page {
            PageTitle("Dictation", subtitle: "What happens between the microphone and the text.") {
                EmptyView()
            }
            ErrorNote(store.error)
            if let notice = store.notice {
                CardRow {
                    Text(notice).bodyText(14, Theme.live)
                } trailing: {
                    Button("Dismiss") { store.clearNotice() }
                        .buttonStyle(.quiet)
                }
            }
            PageSection("Recording") {
                Card {
                    ToggleRow(
                        title: "Quiet speech",
                        detail: "Hears soft speech and whispers. Turns the microphone up and listens more closely.",
                        isOn: Binding(
                            get: { store.settings.quietSpeechEnabled },
                            set: { value in Task { await store.setQuietSpeechEnabled(value) } }
                        )
                    )
                    .disabled(store.isBusy("quiet_speech_enabled"))
                    CardLine("Dictations can last up to 20 minutes. Sona warns you with one minute left, then stops and transcribes what you said.")
                }
            }
            Card {
                /// The cancel chord exists only when a tap-to-start binding
                /// can leave a recording running with nothing holding it.
                if !store.settings.pushToTalk {
                    ShortcutCaptureRow(store: store, id: "cancel")
                }
                ToggleRow(
                    title: "Voice command mode",
                    detail: "Hold the command shortcut and say what to change about the text you have selected, or ask a question.",
                    isOn: Binding(
                        get: { store.settings.commandModeEnabled },
                        set: { value in Task { await store.setCommandMode(value) } }
                    )
                )
                if store.settings.commandModeEnabled {
                    ShortcutCaptureRow(store: store, id: "command")
                    ToggleRow(
                        title: "Show answers in chat",
                        detail: "A question opens the chat with its answer. Nothing is typed over your text. Off, every command edits the selection.",
                        isOn: Binding(
                            get: { store.settings.commandAnswersInChat },
                            set: { value in Task { await store.setCommandAnswersInChat(value) } }
                        )
                    )
                }
                ToggleRow(
                    title: "Learn from corrections in other apps",
                    detail: "Locally watches the last dictated passage for up to one minute in supported text fields. Repeated corrections become suggestions for you to review in Learning.",
                    isOn: Binding(
                        get: { store.settings.learnDestinationCorrections },
                        set: { value in Task { await store.setLearnDestinationCorrections(value) } }
                    )
                )
                projectFolderRow
                /// The microphone end of the same path, and only on a device
                /// with more than one channel — on most machines there is no
                /// row here at all.
                if store.channelCount > 1 {
                    AudioChannelRow(store: store)
                }
                ChoiceRow(
                    title: "English spelling",
                    choices: DictationSpelling.allCases,
                    label: \.label,
                    selection: Binding(
                        get: { store.settings.englishSpelling },
                        set: { value in Task { await store.setSpelling(value) } }
                    )
                )
                if store.supportsTranslation {
                    ToggleRow(
                        title: "Translate to English",
                        isOn: Binding(
                            get: { store.settings.translateToEnglish },
                            set: { value in Task { await store.setTranslateToEnglish(value) } }
                        )
                    )
                }
                /// Not a setting but a door: the editor for the styles a
                /// dictation is written through.
                ActionRow(title: "Dictation styles", button: "Edit", action: onOpenModes)
                ChoiceRow(
                    title: "Overlay",
                    detail: "None hides it, Minimal shows a compact pill, Live shows the transcription as it is written.",
                    choices: OverlayStyle.allCases,
                    label: \.label,
                    selection: Binding(
                        get: { store.settings.overlayStyle },
                        set: { value in Task { await store.setOverlayStyle(value) } }
                    )
                )
                /// Beside the recording overlay, because both are what Sona
                /// puts on screen outside its own window.
                HudPillRow(store: store)
                ToggleRow(
                    title: "Mute other audio while dictating",
                    detail: "Turns the Mac's sound off while you record, when something is playing, and back on when you stop. Sound you muted yourself stays muted.",
                    isOn: Binding(
                        get: { store.settings.muteWhileRecording },
                        set: { value in Task { await store.setMuteWhileRecording(value) } }
                    )
                )
                .disabled(store.isBusy("mute_while_recording"))
                ChoiceRow(
                    title: "Unload model",
                    choices: unloadChoices,
                    label: \.label,
                    selection: Binding(
                        get: { store.settings.modelUnloadTimeout },
                        set: { value in Task { await store.setUnloadTimeout(value) } }
                    )
                )
                dataRows()
                ToggleRow(
                    title: "Experimental features",
                    isOn: Binding(
                        get: { store.settings.experimentalEnabled },
                        set: { value in Task { await store.setExperimental(value) } }
                    )
                )
                if store.settings.experimentalEnabled {
                    ChoiceRow(
                        title: "Keyboard implementation",
                        detail: "Which layer listens for the shortcuts.",
                        choices: KeyboardImplementation.allCases,
                        label: \.label,
                        selection: Binding(
                            get: { store.settings.keyboardImplementation },
                            set: { value in Task { await store.setKeyboardImplementation(value) } }
                        )
                    )
                    AcceleratorRows(store: store)
                    ToggleRow(
                        title: "Keep the microphone open between dictations",
                        detail: "Keeps the microphone open for 30 seconds after you stop, so back-to-back dictations start faster. Bluetooth audio quality can drop while it is open.",
                        isOn: Binding(
                            get: { store.settings.lazyStreamClose },
                            set: { value in Task { await store.setLazyStreamClose(value) } }
                        )
                    )
                }
            }
        }
    }

    private var projectFolderRow: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text("Project vocabulary").font(TypeScale.label(14)).foregroundStyle(Theme.ink)
                Text("Filenames and identifiers can be sent to your cleanup model. Requires Full context in Privacy and your dictation style. Ignored and hidden files stay out.")
                    .bodyText(13, Theme.inkSecondary)
                if let root = store.settings.dictationProjectRoot {
                    Text(URL(fileURLWithPath: root).lastPathComponent)
                        .font(TypeScale.mono(12)).foregroundStyle(Theme.inkSecondary)
                        .help(root)
                } else {
                    Text("No folder selected").bodyText(13, Theme.inkTertiary)
                }
            }
        } trailing: {
            HStack(spacing: 12) {
                if store.settings.dictationProjectRoot != nil {
                    Button("Clear") { Task { await store.setDictationProjectRoot(nil) } }
                        .buttonStyle(.quiet)
                        .accessibilityLabel("Clear dictation project folder")
                }
                Button("Choose…", action: store.chooseDictationProject)
                    .buttonStyle(.compact)
                    .accessibilityLabel("Choose dictation project folder")
            }
        }
        .disabled(store.choosingProject || store.isBusy("dictation_project_root"))
    }

    /// Fifteen seconds is only useful for watching an unload happen, so it is
    /// offered only where that is the point.
    private var unloadChoices: [DictationUnloadTimeout] {
        DictationUnloadTimeout.allCases.filter { $0 != .sec15 || store.settings.debugMode }
    }
}

extension DictationSettingsView where DataRows == EmptyView {
    init(store: SettingsStore, onOpenModes: @escaping () -> Void = {}) {
        self.init(store: store, onOpenModes: onOpenModes) { EmptyView() }
    }
}

/// Which channel of a multi-channel input the recorder listens to. Averaging
/// is what it falls back to, and what a stored channel the device no longer
/// has resolves to.
struct AudioChannelRow: View {
    let store: SettingsStore

    var body: some View {
        ChoiceRow(
            title: "Input channel",
            choices: Array(-1 ..< store.channelCount),
            label: { $0 < 0 ? "Average all channels" : "Channel \($0 + 1)" },
            selection: Binding(
                get: {
                    guard let channel = store.settings.selectedChannel, channel < store.channelCount else { return -1 }
                    return channel
                },
                set: { value in Task { await store.setChannel(value < 0 ? nil : value) } }
            )
        )
    }
}

/// The idle pill, where it sits, and the hour it is away.
///
/// One row, two controls: where the pill sits is not a second setting, it is
/// the rest of this one, and it only exists once the pill does. An hour away,
/// asked for from the pill itself, shows here with the way to end it early.
struct HudPillRow: View {
    let store: SettingsStore

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text("Show the idle pill").bodyText()
                Text("Keep a small pill on screen between dictations. Click it to start, right-click it to switch modes, drag it to another edge of the screen.")
                    .metaText()
                if let back = hiddenUntil {
                    Text("Hidden until \(back.formatted(date: .omitted, time: .shortened)).")
                        .metaText()
                }
            }
        } trailing: {
            HStack(spacing: 12) {
                if hiddenUntil != nil {
                    Button("Show now") { Task { await store.showHudPillNow() } }
                        .buttonStyle(.quiet)
                        .disabled(store.isBusy("hud_pill_hidden_until_ms"))
                }
                if store.settings.hudPillEnabled {
                    Picker("Idle pill position", selection: Binding(
                        get: { store.settings.hudPillPosition },
                        set: { value in Task { await store.setHudPillPosition(value) } }
                    )) {
                        ForEach(HudPillEdge.allCases, id: \.self) { edge in
                            Text(edge.label).tag(edge)
                        }
                    }
                    .labelsHidden()
                    .pickerStyle(.menu)
                    .fixedSize()
                    .disabled(store.isBusy("hud_pill_position"))
                }
                Toggle("Show the idle pill", isOn: Binding(
                    get: { store.settings.hudPillEnabled },
                    set: { value in Task { await store.setHudPillEnabled(value) } }
                ))
                .labelsHidden()
                .toggleStyle(.switch)
                .tint(Theme.accent)
                .disabled(store.isBusy("hud_pill_enabled"))
            }
        }
    }

    /// When the pill comes back, while it is on and away. A time already
    /// passed is not an hour away, whatever the record still says.
    private var hiddenUntil: Date? {
        guard store.settings.hudPillEnabled, let ms = store.settings.hudPillHiddenUntilMs else { return nil }
        let back = Date(timeIntervalSince1970: Double(ms) / 1000)
        return back > .now ? back : nil
    }
}

/// Where transcription runs.
///
/// One row for transcribe.cpp, which names the GPU rather than making the
/// device a second question, and one for the ONNX models — the second only
/// where this build has providers to choose between.
struct AcceleratorRows: View {
    let store: SettingsStore

    var body: some View {
        let transcribe = store.transcribeChoices
        if transcribe.count > 1, let selected = store.transcribeChoice {
            ChoiceRow(
                title: "transcribe.cpp acceleration",
                choices: transcribe,
                label: \.label,
                selection: Binding(
                    get: { selected },
                    set: { choice in Task { await store.setTranscribeChoice(choice) } }
                )
            )
        }
        if store.ortChoices.count > 2 {
            ChoiceRow(
                title: "ONNX acceleration",
                detail: "Hardware acceleration for ONNX models (Parakeet, Canary, Moonshine). Models may fail to transcribe on an experimental provider.",
                choices: store.ortChoices,
                label: \.label,
                selection: Binding(
                    get: { store.settings.ortAccelerator },
                    set: { value in Task { await store.setOrtAccelerator(value) } }
                )
            )
        }
    }
}

/// The capture settings that live on the debug page: they change what the
/// microphone does, so they are the dictation slice's rows wherever the shell
/// decides to show them.
struct SettingsCaptureRows: View {
    let store: SettingsStore

    var body: some View {
        ToggleRow(
            title: "Always-on microphone",
            detail: "Hold the microphone open so a dictation starts without waiting for the device.",
            isOn: Binding(
                get: { store.settings.alwaysOnMicrophone },
                set: { value in Task { await store.setAlwaysOnMicrophone(value) } }
            )
        )
        /// A machine with no lid has no clamshell state to pick a microphone
        /// for.
        if store.laptop {
            MicrophoneClamshellRow(store: store)
        }
        SoundThemeRow(store: store)
    }
}

/// The microphone used while the lid is closed.
struct MicrophoneClamshellRow: View {
    let store: SettingsStore

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text("Clamshell microphone").bodyText()
                Text("The microphone used while the laptop lid is closed.").metaText()
            }
        } trailing: {
            HStack(spacing: 10) {
                if selected != "Default" {
                    Button("Reset") { Task { await store.resetClamshellMicrophone() } }
                        .buttonStyle(.quiet)
                        .disabled(store.isBusy("clamshell_microphone"))
                }
                Picker("Clamshell microphone", selection: Binding(
                    get: { selected },
                    set: { name in Task { await store.setClamshellMicrophone(name) } }
                )) {
                    ForEach(names, id: \.self) { name in
                        Text(name).tag(name)
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .fixedSize()
                .disabled(store.isBusy("clamshell_microphone"))
            }
        }
    }

    private var selected: String { SettingsStore.deviceLabel(store.settings.clamshellMicrophone) }

    private var names: [String] {
        let listed = store.microphones.map(\.name)
        return listed.contains(selected) ? listed : [selected] + listed
    }
}

/// Which sounds mark the start and the end of a recording, and a way to hear
/// them without recording anything.
struct SoundThemeRow: View {
    let store: SettingsStore

    var body: some View {
        CardRow {
            Text("Sound theme").bodyText()
        } trailing: {
            HStack(spacing: 10) {
                Picker("Sound theme", selection: Binding(
                    get: { store.settings.soundTheme },
                    set: { value in Task { await store.setSoundTheme(value) } }
                )) {
                    ForEach(themes, id: \.self) { theme in
                        Text(theme.label).tag(theme)
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .fixedSize()
                .disabled(store.isBusy("sound_theme"))
                Button {
                    Task {
                        await store.playTestSound("start")
                        await store.playTestSound("stop")
                    }
                } label: {
                    Image(systemName: "play.fill")
                        .font(.system(size: 11, weight: .semibold))
                }
                .buttonStyle(.quiet)
                .help("Preview the start and stop sounds")
                .accessibilityLabel("Preview sounds")
                .disabled(store.isBusy("test_sound"))
            }
        }
    }

    /// Custom is offered only once both files exist, and still shown when it
    /// is already the stored theme: a picker with no item for its own value
    /// renders empty.
    private var themes: [SoundTheme] {
        let custom = store.customSounds.start && store.customSounds.stop
        return SoundTheme.allCases.filter {
            $0 != .custom || custom || store.settings.soundTheme == .custom
        }
    }
}
